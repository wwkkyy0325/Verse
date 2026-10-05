use crate::domain::{AudioChunk, AudioFormat, Segment, Transcript, TranscriptDelta};
use crate::error::Result;

/// A source of PCM audio: a decoder reading a file, or a live capture device.
///
/// Pull-based. The caller drives the pace, which is what keeps memory bounded
/// by consumption rate rather than by file length — the reason a two-hour
/// recording does not need two hours of PCM resident.
pub trait AudioSource: Send {
    fn format(&self) -> AudioFormat;

    /// Pull the next chunk. `Ok(None)` means the stream ended normally.
    fn next_chunk(&mut self) -> Result<Option<AudioChunk>>;
}

/// A speech recognition backend.
///
/// Offline engines buffer everything passed to [`accept`](AsrEngine::accept)
/// and produce output only at [`finalize`](AsrEngine::finalize). Streaming
/// engines emit through [`poll`](AsrEngine::poll) as audio arrives; the
/// `stable` flag on a delta marks text the engine will not revise.
pub trait AsrEngine: Send {
    fn is_streaming(&self) -> bool;

    fn accept(&mut self, chunk: &AudioChunk) -> Result<()>;

    fn poll(&mut self) -> Result<Option<TranscriptDelta>>;

    fn finalize(&mut self) -> Result<Transcript>;

    /// Discard all buffered audio and start over.
    fn reset(&mut self);
}

/// A destination for recognized text: an SRT writer, a subtitle overlay, and
/// so on.
pub trait TextSink: Send {
    /// Called once per finalized segment, in order.
    fn emit(&mut self, segment: &Segment) -> Result<()>;

    /// Called once with the complete result. Defaults to a no-op so sinks that
    /// write as they go need not implement it.
    fn finish(&mut self, _transcript: &Transcript) -> Result<()> {
        Ok(())
    }
}
