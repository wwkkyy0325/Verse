//! Speech detection.
//!
//! Cutting a stream into speech spans is what makes long files workable: each
//! span is recognized on its own, so memory follows span length rather than
//! file length. The boundaries also become subtitle timestamps.

use std::path::Path;
use std::time::Duration;

use sherpa_onnx::{SileroVadModelConfig, VadModelConfig, VoiceActivityDetector};
use verse_core::{AudioChunk, AudioFormat, Error, ErrorKind, Result, Segmenter};

/// Longest span the detector will emit, in seconds.
///
/// This is the hard ceiling on memory. A span is held resident while it is
/// being recognized, so without a cap a few minutes of unbroken speech would
/// defeat the point of segmenting at all.
const MAX_SPAN_SECONDS: f32 = 20.0;

/// Audio prepended to each span, in seconds.
///
/// The detector starts a span where it becomes confident of speech, which can
/// be well after the speech actually began. Measured on one clip, feeding the
/// same audio from different offsets:
///
/// ```text
/// from 0.25 s  ->  开放时间   (correct)
/// from 0.50 s  ->  开放时间   (correct)
/// from 0.75 s  ->  菜饭时间   (wrong, and this is where the detector chose)
/// whole file   ->  开饭时间   (also wrong)
/// ```
///
/// A short run of leading context buys more than trimming does: it gives the
/// model room to settle before the first real syllable. Note that the whole
/// file scored *worse* than the padded slice, so this is not simply "more
/// audio is better".
const PAD_SECONDS: f32 = 0.4;

/// Where the detector decides a frame is speech.
///
/// Far below the usual 0.5, and measured rather than reasoned. On 200
/// utterances of each of two datasets, character error rate:
///
/// ```text
/// threshold   read news   conversation
///   0.30         2.28%        8.58%
///   0.10         2.07%        6.75%
///   0.05         2.11%        6.00%
///   0.02         2.14%        5.76%
/// ```
///
/// On clean read speech the threshold hardly matters. On conversation it is
/// worth a third of the error rate, because speech that is quiet, overlapped
/// or competing with music never reaches 0.3 and the whole utterance is
/// discarded before the recogniser sees it — 17 of 898 utterances came back
/// empty at 0.3 against 1 at 0.05.
///
/// 0.05 rather than the 0.02 that scores marginally better, because
/// sherpa-onnx refuses anything at or below 0.01 and the margin is worth
/// more than two hundredths of a point.
pub const DEFAULT_THRESHOLD: f32 = 0.05;

/// Silence long enough to end a span.
const MIN_SILENCE_SECONDS: f32 = 0.25;

/// Speech shorter than this is discarded as a noise.
///
/// Worth knowing when short utterances come back empty: a clipped 好 or 对
/// can fall under it and disappear entirely.
const MIN_SPEECH_SECONDS: f32 = 0.25;

/// Silero VAD, presented as a [`Segmenter`].
pub struct SileroVad {
    inner: VoiceActivityDetector,
    format: AudioFormat,
    /// Audio fed since the last drain, kept so a span's leading context is
    /// still available when the detector reports it.
    ///
    /// Bounded: a span cannot run longer than [`MAX_SPAN_SECONDS`], so audio
    /// older than that can never be needed. This is a fixed ceiling, not
    /// accumulation.
    pending: Vec<f32>,
    /// Stream offset of `pending[0]`, in samples.
    pending_start: usize,
}

impl SileroVad {
    /// Load the Silero VAD model and prepare to segment `format` audio.
    pub fn load(model: &Path, format: AudioFormat) -> Result<Self> {
        Self::load_with(model, format, DEFAULT_THRESHOLD)
    }

    /// Load with an explicit speech threshold.
    ///
    /// The threshold is the knob that decides what counts as speech at all.
    /// Too high and quiet or unusual speech is discarded before the recogniser
    /// ever sees it — which shows up not as a wrong transcript but as no
    /// transcript, the hardest kind of failure to notice from a sample.
    pub fn load_with(model: &Path, format: AudioFormat, threshold: f32) -> Result<Self> {
        // Checked here rather than left to sherpa-onnx, which refuses the same
        // values but reports them as "failed to load VAD model" — sending
        // anyone who reads that message to inspect a model that is perfectly
        // fine. Its accepted floor is somewhere just above 0.01.
        if !(0.02..=1.0).contains(&threshold) {
            return Err(Error::new(
                ErrorKind::Model,
                format!("VAD threshold {threshold} is outside the usable range 0.02–1.0"),
            ));
        }

        if !model.is_file() {
            return Err(Error::new(
                ErrorKind::Model,
                format!("VAD model not found: {}", model.display()),
            ));
        }

        let model_path = model.to_str().ok_or_else(|| {
            Error::new(
                ErrorKind::Io,
                format!("path is not valid UTF-8: {}", model.display()),
            )
        })?;

        let config = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(model_path.to_string()),
                threshold,
                min_silence_duration: MIN_SILENCE_SECONDS,
                min_speech_duration: MIN_SPEECH_SECONDS,
                // Silero v5 works on 512-sample windows at 16 kHz.
                window_size: 512,
                max_speech_duration: MAX_SPAN_SECONDS,
            },
            sample_rate: format.sample_rate as i32,
            num_threads: 1,
            provider: Some("cpu".to_string()),
            debug: false,
            ..Default::default()
        };

        // The detector's own ring buffer has to comfortably exceed the longest
        // span it may emit, or it would discard audio before we collect it.
        let buffer_seconds = MAX_SPAN_SECONDS * 3.0;

        let inner = VoiceActivityDetector::create(&config, buffer_seconds).ok_or_else(|| {
            Error::new(
                ErrorKind::Model,
                format!("failed to load VAD model: {}", model.display()),
            )
        })?;

        Ok(Self {
            inner,
            format,
            pending: Vec::new(),
            pending_start: 0,
        })
    }

    fn rate(&self) -> usize {
        self.format.sample_rate.max(1) as usize
    }

    /// How much history must stay resident.
    fn pending_capacity(&self) -> usize {
        ((MAX_SPAN_SECONDS + PAD_SECONDS) * self.rate() as f32) as usize
    }

    /// Drop history no future span could reference.
    ///
    /// Trimmed in bulk rather than on every chunk: this is a memmove over the
    /// whole buffer, so doing it every 0.1 s for no reason would be wasteful.
    fn trim_pending(&mut self) {
        let capacity = self.pending_capacity();
        if self.pending.len() <= capacity * 2 {
            return;
        }

        let excess = self.pending.len() - capacity;
        self.pending.copy_within(excess.., 0);
        self.pending.truncate(capacity);
        self.pending_start += excess;
    }

    /// Assemble a span's audio, with leading context when we still have it.
    fn build_span(&self, start: usize, samples: &[f32]) -> AudioChunk {
        let pad_samples = (PAD_SECONDS * self.rate() as f32) as usize;
        let pad_from = start.saturating_sub(pad_samples);

        let mut audio = Vec::with_capacity(samples.len() + pad_samples);

        // History may already have been trimmed past this point; when it has,
        // the span simply goes without padding rather than failing.
        if pad_from >= self.pending_start {
            let from = pad_from - self.pending_start;
            let to = start
                .saturating_sub(self.pending_start)
                .min(self.pending.len());
            if from < to {
                audio.extend_from_slice(&self.pending[from..to]);
            }
        }

        audio.extend_from_slice(samples);

        AudioChunk {
            samples: audio,
            format: self.format,
            start: self.offset_of(pad_from),
        }
    }

    /// Collect every span the detector currently holds.
    fn drain(&mut self) -> Vec<AudioChunk> {
        let mut spans = Vec::new();

        while !self.inner.is_empty() {
            if let Some(segment) = self.inner.front() {
                let start = segment.start().max(0) as usize;
                spans.push(self.build_span(start, segment.samples()));
            }
            self.inner.pop();
        }

        spans
    }

    /// Convert a sample index reported by the detector into a stream offset.
    fn offset_of(&self, sample_index: usize) -> Duration {
        Duration::from_secs_f64(sample_index as f64 / self.rate() as f64)
    }
}

impl Segmenter for SileroVad {
    fn accept(&mut self, chunk: &AudioChunk) -> Result<()> {
        self.pending.extend_from_slice(&chunk.samples);
        self.trim_pending();

        self.inner.accept_waveform(&chunk.samples);
        Ok(())
    }

    fn take(&mut self) -> Vec<AudioChunk> {
        self.drain()
    }

    fn finish(&mut self) -> Vec<AudioChunk> {
        // Emit whatever speech is still buffered when the stream ends;
        // otherwise a recording ending mid-sentence would lose its tail.
        self.inner.flush();
        self.drain()
    }

    fn reset(&mut self) {
        self.inner.clear();
        self.inner.reset();
        self.pending.clear();
        self.pending_start = 0;
    }
}
