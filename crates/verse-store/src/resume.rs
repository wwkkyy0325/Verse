//! What a long transcription has got through so far.
//!
//! A two-hour recording is minutes of decoding and a great many more of
//! recognition. Losing the recognition to a crash, a cancelled window, or a
//! closed laptop lid is the difference between a nuisance and an afternoon.
//!
//! **Only the recognition is remembered.** On a second attempt the file is
//! decoded and segmented again from the beginning, because sherpa-onnx does not
//! expose the detector's internal state and a resumed run has to reach the same
//! spans by the same route. The decoding and the detecting are the cheap part;
//! recognising what comes out of them is not. Reusing a span therefore costs
//! nothing but the work it saves.
//!
//! **The failure mode is redoing work, never inventing it.** A span is
//! identified by its own samples, so a span that is not byte-identical to one
//! already done simply does not match and is recognised again. There is no
//! approximation and no interpolation, and "slightly different segmentation"
//! can only cost time.
//!
//! The file is a log rather than a document, appended to as each span lands. It
//! is written through on every record — buffering until the end would defeat
//! the entire purpose — and removed once the run finishes.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::entry::SegmentDto;

/// The directory name for this format, as in [`crate::cache`].
const DIR_VERSION: &str = "v1";

/// One line of the log.
#[derive(Debug, Serialize, Deserialize)]
struct Record {
    span: String,
    segments: Vec<SegmentDto>,
}

/// The spans a run has finished.
#[derive(Debug)]
pub struct Progress {
    path: PathBuf,
    /// Keyed by span identity. A `BTreeMap` rather than a `HashMap` so that a
    /// rewritten log is byte-stable, which makes the file diffable when
    /// something has gone wrong.
    done: BTreeMap<String, Vec<SegmentDto>>,
    handle: Option<std::fs::File>,
}

impl Progress {
    /// Open the log for a run, reading back whatever a previous attempt left.
    ///
    /// `run` identifies the whole job — the settings and the audio — and `pass`
    /// separates the detector's first attempt from the guard's whole-file
    /// second one. They must never share a log: a span from one pass has
    /// nothing to do with a span from the other, and reusing across them would
    /// splice two different segmentations into one transcript.
    pub fn open(data_dir: &Path, run: &str, pass: &str) -> Self {
        let path = data_dir
            .join("resume")
            .join(DIR_VERSION)
            .join(format!("{run}-{pass}.jsonl"));

        let mut done = BTreeMap::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                // A truncated final line is the expected shape of a crash, so
                // an unreadable line is skipped rather than treated as damage
                // to the whole file.
                if let Ok(record) = serde_json::from_str::<Record>(line) {
                    done.insert(record.span, record.segments);
                }
            }
        }

        Self {
            path,
            done,
            handle: None,
        }
    }

    /// What was recognised for this span, if a previous attempt got to it.
    pub fn remembered(&self, span: &str) -> Option<&[SegmentDto]> {
        self.done.get(span).map(Vec::as_slice)
    }

    /// How many spans are already in hand.
    pub fn len(&self) -> usize {
        self.done.len()
    }

    pub fn is_empty(&self) -> bool {
        self.done.is_empty()
    }

    /// Append a finished span, and make sure it is on disk before returning.
    ///
    /// The write is the point. A log that is only flushed at the end survives
    /// exactly the failures it exists to survive — none of them.
    pub fn record(&mut self, span: &str, segments: &[SegmentDto]) -> std::io::Result<()> {
        if self.done.contains_key(span) {
            return Ok(());
        }

        if self.handle.is_none() {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            self.handle = Some(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?,
            );
        }

        let line = serde_json::to_string(&Record {
            span: span.to_string(),
            segments: segments.to_vec(),
        })
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        if let Some(handle) = self.handle.as_mut() {
            handle.write_all(line.as_bytes())?;
            handle.write_all(b"\n")?;
            handle.flush()?;
        }

        self.done.insert(span.to_string(), segments.to_vec());
        Ok(())
    }

    /// Remove the log, because the run finished and it is no longer evidence of
    /// anything.
    pub fn forget(self) -> std::io::Result<()> {
        // Dropped before the removal so the handle is not open on Windows,
        // where deleting a file that is still open fails.
        drop(self.handle);
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// The file this log lives in, for tests and for saying so when it matters.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// The identity of one span: where it starts, and the samples it holds.
///
/// The samples themselves rather than a description of them, so that the
/// question "is this the same audio as last time" is answered by the audio and
/// not by a promise that the segmenter will produce it again. A segmenter that
/// drifted by a few milliseconds between runs would produce a different span
/// and lose the stored text — which costs time, and cannot cost correctness.
pub fn span_key(start: std::time::Duration, samples: &[f32]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(start.as_nanos().to_le_bytes());
    hasher.update((samples.len() as u64).to_le_bytes());
    for sample in samples {
        hasher.update(sample.to_le_bytes());
    }

    let mut out = String::with_capacity(64);
    for byte in hasher.finalize() {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::Span;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("verse-store-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn segments(text: &str) -> Vec<SegmentDto> {
        vec![SegmentDto {
            id: 0,
            start: Span::from(std::time::Duration::from_millis(0)),
            end: Span::from(std::time::Duration::from_millis(500)),
            text: text.to_string(),
        }]
    }

    #[test]
    fn what_was_recorded_is_remembered_by_the_next_attempt() {
        let dir = scratch("resume-basic");
        let mut progress = Progress::open(&dir, "run-one", "vad");
        progress
            .record("span-a", &segments("开放时间"))
            .expect("record");

        let reopened = Progress::open(&dir, "run-one", "vad");
        assert_eq!(
            reopened.remembered("span-a").map(<[SegmentDto]>::len),
            Some(1)
        );
        assert!(reopened.remembered("span-b").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_different_run_does_not_see_the_other_run_s_spans() {
        // Different settings or different audio means the spans are not the
        // same spans, whatever they are called.
        let dir = scratch("resume-run-key");
        let mut first = Progress::open(&dir, "run-one", "vad");
        first
            .record("span-a", &segments("开放时间"))
            .expect("record");

        let second = Progress::open(&dir, "run-two", "vad");
        assert!(second.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_two_passes_never_share_a_log() {
        // The guard's whole-file pass produces different spans from the
        // detector's. Reusing across them would splice two segmentations into
        // one transcript.
        let dir = scratch("resume-passes");
        let mut detected = Progress::open(&dir, "run-one", "vad");
        detected
            .record("span-a", &segments("from the detector"))
            .expect("record");

        let whole = Progress::open(&dir, "run-one", "whole");
        assert!(whole.is_empty(), "a different pass is a different log");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_record_survives_a_process_that_never_gets_to_finish() {
        // The property the whole module exists for. The file must be complete
        // on disk before `record` returns.
        let dir = scratch("resume-written-through");
        let mut progress = Progress::open(&dir, "run-one", "vad");
        progress
            .record("span-a", &segments("first"))
            .expect("record");

        // Read it back from the filesystem without going through the in-memory
        // map, as a fresh process would.
        let text = std::fs::read_to_string(progress.path()).expect("the log is on disk");
        assert!(text.contains("first"), "got: {text}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_truncated_last_line_costs_only_that_line() {
        // A crash mid-write leaves a partial line, which is expected rather
        // than damage.
        let dir = scratch("resume-truncated");
        let mut progress = Progress::open(&dir, "run-one", "vad");
        progress
            .record("span-a", &segments("complete"))
            .expect("record");
        drop(progress);

        let path = dir
            .join("resume")
            .join(DIR_VERSION)
            .join("run-one-vad.jsonl");
        let mut text = std::fs::read_to_string(&path).expect("read");
        text.push_str("{\"span\":\"span-b\",\"segmen");
        std::fs::write(&path, text).expect("truncate");

        let reopened = Progress::open(&dir, "run-one", "vad");
        assert_eq!(reopened.len(), 1, "the whole line before it survives");
        assert!(reopened.remembered("span-a").is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recording_the_same_span_twice_does_not_duplicate_it() {
        let dir = scratch("resume-idempotent");
        let mut progress = Progress::open(&dir, "run-one", "vad");
        progress
            .record("span-a", &segments("first"))
            .expect("record");
        progress
            .record("span-a", &segments("second"))
            .expect("record");

        assert_eq!(progress.len(), 1, "the first answer is the one kept");
        assert_eq!(
            progress.remembered("span-a").expect("remembered")[0].text,
            "first"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finishing_removes_the_log() {
        let dir = scratch("resume-forget");
        let mut progress = Progress::open(&dir, "run-one", "vad");
        progress
            .record("span-a", &segments("open"))
            .expect("record");
        let path = progress.path().to_path_buf();
        assert!(path.exists());

        progress.forget().expect("forget");
        assert!(!path.exists(), "a finished run leaves no checkpoint");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forgetting_something_that_was_never_written_is_not_an_error() {
        // A run with nothing to remember still finishes.
        let dir = scratch("resume-forget-empty");
        let progress = Progress::open(&dir, "run-one", "vad");
        assert!(progress.forget().is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_span_key_depends_on_the_samples() {
        let quiet = vec![0.0_f32; 16];
        let loud = vec![1.0_f32; 16];

        assert_ne!(
            span_key(std::time::Duration::from_millis(0), &quiet),
            span_key(std::time::Duration::from_millis(0), &loud)
        );
    }

    #[test]
    fn a_span_key_depends_on_where_the_span_starts() {
        // Same audio at a different place in the file is a different span.
        let samples = vec![0.5_f32; 16];
        assert_ne!(
            span_key(std::time::Duration::from_millis(0), &samples),
            span_key(std::time::Duration::from_millis(16_000), &samples)
        );
    }

    #[test]
    fn a_span_key_depends_on_the_length() {
        assert_ne!(
            span_key(std::time::Duration::from_millis(0), &[0.5_f32; 16]),
            span_key(std::time::Duration::from_millis(0), &[0.5_f32; 17])
        );
    }

    #[test]
    fn a_span_key_is_stable_and_a_plain_hex_name() {
        let samples = vec![0.25_f32; 8];
        let key = span_key(std::time::Duration::from_millis(1_600), &samples);

        assert_eq!(
            key,
            span_key(std::time::Duration::from_millis(1_600), &samples)
        );
        assert_eq!(key.len(), 64);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_one_sample_difference_changes_the_key() {
        // The property that makes reuse safe: a span that is not the same audio
        // cannot be mistaken for one that is.
        let mut samples = vec![0.5_f32; 512];
        let before = span_key(std::time::Duration::from_millis(0), &samples);

        samples[511] = 0.500_001;

        assert_ne!(
            before,
            span_key(std::time::Duration::from_millis(0), &samples)
        );
    }
}
