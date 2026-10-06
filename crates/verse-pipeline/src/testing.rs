//! Pieces that exist only so a test can describe a run without a model.
//!
//! Compiled for tests only. It lives in its own module rather than in each
//! test module because two of them need the same fake engine now, and a second
//! copy of a fake is a second thing that can drift from what it imitates.

use std::time::Duration;

use verse_core::{AsrEngine, AudioChunk, AudioFormat, Result, Transcript};

use crate::{effective_threads, settings_digest, Request, Transcriber};

/// An engine that says whatever it was told to say.
///
/// `finalize` yields the transcript once and then nothing, which is enough to
/// exercise everything the pipeline does with a recogniser's output.
pub struct Fixed(pub Option<Transcript>);

impl AsrEngine for Fixed {
    fn is_streaming(&self) -> bool {
        false
    }

    fn accept(&mut self, _chunk: &AudioChunk) -> Result<()> {
        Ok(())
    }

    fn poll(&mut self) -> Result<Option<verse_core::TranscriptDelta>> {
        Ok(None)
    }

    fn finalize(&mut self) -> Result<Transcript> {
        Ok(self.0.take().unwrap_or_default())
    }

    fn reset(&mut self) {}
}

/// A chunk of pretend audio at the pipeline's own format.
pub fn a_chunk(start_ms: u64, samples: usize) -> AudioChunk {
    AudioChunk {
        samples: vec![0.5; samples],
        format: AudioFormat::TARGET,
        start: Duration::from_millis(start_ms),
    }
}

impl Transcriber {
    /// A transcriber over a fake engine, carrying the **real** settings digest.
    ///
    /// Only `Registry::create_engine` is skipped. Everything that decides
    /// identity — the resolved thread count and the digest — is computed the
    /// same way `load` computes it, so a test that watches for reuse is
    /// watching the same decision the real thing makes rather than a stubbed
    /// string that would agree with anything.
    pub(crate) fn with_engine(request: Request, engine: Box<dyn AsrEngine>) -> Self {
        let settings = settings_digest(
            &request,
            effective_threads(&request),
            verse_audio::ffmpeg_version().as_deref(),
        );

        Self {
            request,
            engine,
            settings,
        }
    }
}
