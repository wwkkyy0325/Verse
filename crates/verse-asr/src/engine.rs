//! Offline (non-streaming) recognition engines.
//!
//! Both supported models buffer audio and emit a single segment at `finalize`.
//! Only recognizer construction differs between them, so buffering and result
//! plumbing are shared here.

use std::path::Path;
use std::time::Duration;

use sherpa_onnx::{
    OfflineModelConfig, OfflineParaformerModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineSenseVoiceModelConfig,
};
use verse_core::{
    AsrEngine, AudioChunk, AudioFormat, Error, ErrorKind, Result, Segment, SegmentId, Transcript,
    TranscriptDelta,
};

/// Language hint for models that accept one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Auto,
    Chinese,
    English,
    Cantonese,
    Japanese,
    Korean,
}

impl Language {
    fn as_code(self) -> &'static str {
        match self {
            Language::Auto => "auto",
            Language::Chinese => "zh",
            Language::English => "en",
            Language::Cantonese => "yue",
            Language::Japanese => "ja",
            Language::Korean => "ko",
        }
    }
}

/// A recognizer that consumes a whole span of audio and returns one segment.
///
/// This is the shape both Paraformer and SenseVoice have. Long recordings are
/// split into spans upstream — by VAD — and each span is fed to its own engine
/// call, which is what keeps memory bounded by span length rather than file
/// length.
pub struct OfflineEngine {
    recognizer: OfflineRecognizer,
    samples: Vec<f32>,
    /// Offset of the first accepted sample within the source stream.
    start: Duration,
}

impl OfflineEngine {
    /// Paraformer-large. Chinese and English, 10–20x realtime on CPU.
    ///
    /// Emits no punctuation — pair it with [`crate::Punctuator`].
    pub fn paraformer(model: &Path, tokens: &Path, threads: usize) -> Result<Self> {
        let config = OfflineRecognizerConfig {
            model_config: OfflineModelConfig {
                paraformer: OfflineParaformerModelConfig {
                    model: Some(path_string(model)?),
                },
                tokens: Some(path_string(tokens)?),
                num_threads: thread_count(threads),
                ..Default::default()
            },
            ..Default::default()
        };
        Self::create(config, model, tokens)
    }

    /// SenseVoice-Small. Chinese, English, Cantonese, Japanese and Korean.
    ///
    /// Punctuates and normalizes internally when `use_itn` is set, so it needs
    /// no separate punctuation stage — that is the whole reason it is the
    /// default despite its non-Apache license (see `design.md` §5.3).
    pub fn sensevoice(
        model: &Path,
        tokens: &Path,
        threads: usize,
        language: Language,
        use_itn: bool,
    ) -> Result<Self> {
        let config = OfflineRecognizerConfig {
            model_config: OfflineModelConfig {
                sense_voice: OfflineSenseVoiceModelConfig {
                    model: Some(path_string(model)?),
                    language: Some(language.as_code().to_string()),
                    use_itn,
                },
                tokens: Some(path_string(tokens)?),
                num_threads: thread_count(threads),
                ..Default::default()
            },
            ..Default::default()
        };
        Self::create(config, model, tokens)
    }

    fn create(config: OfflineRecognizerConfig, model: &Path, tokens: &Path) -> Result<Self> {
        for path in [model, tokens] {
            if !path.is_file() {
                return Err(Error::new(
                    ErrorKind::Model,
                    format!("model file not found: {}", path.display()),
                ));
            }
        }

        let recognizer = OfflineRecognizer::create(&config).ok_or_else(|| {
            Error::new(
                ErrorKind::Model,
                format!("failed to load model: {}", model.display()),
            )
        })?;

        Ok(Self {
            recognizer,
            samples: Vec::new(),
            start: Duration::ZERO,
        })
    }
}

impl AsrEngine for OfflineEngine {
    fn is_streaming(&self) -> bool {
        false
    }

    fn accept(&mut self, chunk: &AudioChunk) -> Result<()> {
        if self.samples.is_empty() {
            self.start = chunk.start;
        }
        self.samples.extend_from_slice(&chunk.samples);
        Ok(())
    }

    fn poll(&mut self) -> Result<Option<TranscriptDelta>> {
        // Offline models have nothing to say until finalize.
        Ok(None)
    }

    /// Recognize everything accepted so far.
    ///
    /// Does not clear the buffer, so it may be called more than once. Call
    /// [`reset`](AsrEngine::reset) to start a new span.
    fn finalize(&mut self) -> Result<Transcript> {
        if self.samples.is_empty() {
            return Ok(Transcript::default());
        }

        let stream = self.recognizer.create_stream();
        stream.accept_waveform(AudioFormat::TARGET.sample_rate as i32, &self.samples);
        self.recognizer.decode(&stream);

        let result = stream
            .get_result()
            .ok_or_else(|| Error::new(ErrorKind::Engine, "recognition produced no result"))?;

        let text = clean_markers(&result.text);
        if text.is_empty() {
            return Ok(Transcript::default());
        }

        let seconds = self.samples.len() as f64 / AudioFormat::TARGET.sample_rate as f64;

        Ok(Transcript {
            segments: vec![Segment {
                id: SegmentId(0),
                start: self.start,
                end: self.start + Duration::from_secs_f64(seconds),
                text,
            }],
            language: None,
        })
    }

    fn reset(&mut self) {
        self.samples.clear();
        self.start = Duration::ZERO;
    }
}

/// Strip the metadata markers SenseVoice puts in front of its output.
///
/// SenseVoice prefixes text with language, emotion, event and ITN flags —
/// `<|zh|><|NEUTRAL|><|Speech|><|woitn|>`. They describe the audio rather than
/// transcribe it, and letting them reach a subtitle file would be a bug.
///
/// Paraformer emits none, so this is a no-op for it.
fn clean_markers(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;

    while let Some(open) = rest.find("<|") {
        out.push_str(&rest[..open]);
        match rest[open..].find("|>") {
            // Skip past the closing `|>`.
            Some(close) => rest = &rest[open + close + 2..],
            // An unterminated marker means malformed output; drop the tail
            // rather than emitting it as text.
            None => return out.trim().to_string(),
        }
    }

    out.push_str(rest);
    out.trim().to_string()
}

fn thread_count(requested: usize) -> i32 {
    requested.max(1).min(i32::MAX as usize) as i32
}

fn path_string(path: &Path) -> Result<String> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        Error::new(
            ErrorKind::Io,
            format!("path is not valid UTF-8: {}", path.display()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::clean_markers;

    #[test]
    fn sensevoice_markers_are_stripped() {
        assert_eq!(
            clean_markers("<|zh|><|NEUTRAL|><|Speech|><|woitn|>你好世界"),
            "你好世界"
        );
    }

    #[test]
    fn text_without_markers_is_untouched() {
        assert_eq!(clean_markers("你好世界"), "你好世界");
    }

    #[test]
    fn markers_inside_text_are_removed_too() {
        assert_eq!(clean_markers("前<|en|>后"), "前后");
    }

    #[test]
    fn a_closed_marker_leaves_the_text_around_it() {
        assert_eq!(clean_markers("保留<|zh|>丢弃"), "保留丢弃");
    }

    #[test]
    fn an_unterminated_marker_drops_only_the_tail() {
        // No closing `|>`, so the marker is malformed. Everything from it
        // onward is discarded rather than emitted as if it were text.
        assert_eq!(clean_markers("保留<|zh丢弃"), "保留");
    }
}
