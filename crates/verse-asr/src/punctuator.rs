//! Punctuation restoration.
//!
//! Paraformer emits bare text — `对我做了介绍啊那么我想说的是呢` — which is
//! unusable as subtitles. This stage restores sentences.
//!
//! SenseVoice punctuates internally, so it needs no stage here. That is what
//! makes this optional rather than mandatory: `design.md` §5.4 called it
//! non-optional at a time when Paraformer was the only engine available.

use std::path::Path;

use sherpa_onnx::{OfflinePunctuation, OfflinePunctuationConfig, OfflinePunctuationModelConfig};
use verse_core::{Error, ErrorKind, Result, TextProcessor};

/// Restores punctuation to unpunctuated text.
pub struct Punctuator {
    inner: OfflinePunctuation,
}

impl Punctuator {
    /// Load a CT-Transformer punctuation model.
    ///
    /// Unlike the recognition models this needs no tokens file — the vocabulary
    /// is embedded in the ONNX graph.
    pub fn load(model: &Path) -> Result<Self> {
        if !model.is_file() {
            return Err(Error::new(
                ErrorKind::Model,
                format!("punctuation model not found: {}", model.display()),
            ));
        }

        let config = OfflinePunctuationConfig {
            model: OfflinePunctuationModelConfig {
                ct_transformer: Some(model.to_string_lossy().into_owned()),
                num_threads: 1,
                debug: false,
                provider: Some("cpu".to_string()),
            },
        };

        let inner = OfflinePunctuation::create(&config).ok_or_else(|| {
            Error::new(
                ErrorKind::Model,
                format!("failed to load punctuation model: {}", model.display()),
            )
        })?;

        Ok(Self { inner })
    }

    /// Add punctuation to a run of text.
    ///
    /// Falls back to the input when the model returns nothing, so a
    /// punctuation failure costs readability rather than the transcript.
    pub fn punctuate(&self, text: &str) -> String {
        if text.trim().is_empty() {
            return text.to_string();
        }

        self.inner
            .add_punctuation(text)
            .filter(|punctuated| !punctuated.trim().is_empty())
            .unwrap_or_else(|| text.to_string())
    }
}

impl TextProcessor for Punctuator {
    fn name(&self) -> &'static str {
        "punctuator"
    }

    /// Satisfies the [`TextProcessor`] contract by construction:
    /// [`punctuate`](Punctuator::punctuate) already returns the input
    /// unchanged when the model produces nothing.
    fn process(&self, input: &str) -> String {
        self.punctuate(input)
    }
}
