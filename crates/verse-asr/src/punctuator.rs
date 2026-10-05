//! Punctuation restoration.
//!
//! Recognition models emit bare text — `对我做了介绍啊那么我想说的是呢` — and
//! subtitles need sentences. Punctuation is a separate model because the two
//! tasks are trained separately; `design.md` §5.4 called this non-optional and
//! the first transcription confirmed it.

use std::path::Path;

use sherpa_onnx::{OfflinePunctuation, OfflinePunctuationConfig, OfflinePunctuationModelConfig};
use verse_core::{Error, ErrorKind, Result};

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
