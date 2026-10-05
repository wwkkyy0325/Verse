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

/// Silero VAD, presented as a [`Segmenter`].
pub struct SileroVad {
    inner: VoiceActivityDetector,
    format: AudioFormat,
}

impl SileroVad {
    /// Load the Silero VAD model and prepare to segment `format` audio.
    pub fn load(model: &Path, format: AudioFormat) -> Result<Self> {
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
                threshold: 0.5,
                min_silence_duration: 0.25,
                min_speech_duration: 0.25,
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

        Ok(Self { inner, format })
    }

    /// Collect every span the detector currently holds.
    fn drain(&mut self) -> Vec<AudioChunk> {
        let mut spans = Vec::new();

        while !self.inner.is_empty() {
            if let Some(segment) = self.inner.front() {
                spans.push(AudioChunk {
                    samples: segment.samples().to_vec(),
                    format: self.format,
                    start: self.offset_of(segment.start()),
                });
            }
            self.inner.pop();
        }

        spans
    }

    /// Convert a sample index reported by the detector into a stream offset.
    fn offset_of(&self, sample_index: i32) -> Duration {
        let rate = self.format.sample_rate.max(1) as f64;
        Duration::from_secs_f64(sample_index.max(0) as f64 / rate)
    }
}

impl Segmenter for SileroVad {
    fn accept(&mut self, chunk: &AudioChunk) -> Result<()> {
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
    }
}
