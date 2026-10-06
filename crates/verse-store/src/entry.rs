//! The shape of a cached transcription.
//!
//! These mirror `verse_core::Transcript` and friends without depending on them
//! — the boundary converts, as it does for the CLI's report and the window's
//! bridge. What makes this one different from those is what it is compared
//! against: a report is *read*, so milliseconds and tidy names are right, while
//! an entry is *replayed*. Every value here has to survive the round trip
//! exactly, because a cached result is handed out as though the work had just
//! been done, and "close enough" would mean a transcript that quietly differs
//! from the one a fresh run produces.

use serde::{Deserialize, Serialize};

/// The version of this format.
///
/// A file carrying anything else is ignored rather than guessed at. The cache
/// is not the only copy of anything, so discarding it costs a recomputation and
/// nothing more — which is exactly why it can afford to be strict here.
pub const ENTRY_VERSION: u32 = 1;

/// A moment on the recording's timeline, exactly.
///
/// Held as seconds and nanoseconds rather than as milliseconds. `Duration` is
/// what the pipeline works in, and the report's millisecond fields are a
/// deliberate lossy rendering for a reader — lossy is the one thing this must
/// not be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub secs: u64,
    pub nanos: u32,
}

impl From<std::time::Duration> for Span {
    fn from(duration: std::time::Duration) -> Self {
        Self {
            secs: duration.as_secs(),
            nanos: duration.subsec_nanos(),
        }
    }
}

impl From<Span> for std::time::Duration {
    fn from(span: Span) -> Self {
        std::time::Duration::new(span.secs, span.nanos)
    }
}

/// One recognised stretch of speech.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentDto {
    pub id: u64,
    pub start: Span,
    pub end: Span,
    pub text: String,
}

/// A whole transcript.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptDto {
    pub segments: Vec<SegmentDto>,
    /// `null` where the engine does not report one.
    ///
    /// Always present, for the reason the CLI's report gives: an absent key and
    /// a null one read the same to a person and differently to a program, and
    /// only one of them is a decision.
    pub language: Option<String>,
}

/// How much of the audio reached the recogniser.
///
/// Kept because it travels with every result for a reason — a short transcript
/// and a truncated one look identical without it — and a replayed result that
/// dropped it would be the one kind of result that cannot be judged.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CoverageDto {
    pub decoded_seconds: f64,
    pub voiced_seconds: f64,
    pub energetic_seconds: f64,
}

/// One cached transcription.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub version: u32,
    pub transcript: TranscriptDto,
    pub coverage: CoverageDto,
    /// Whether the guard discarded the first pass and recognised the file
    /// whole. Part of the result, not of the run: a caller measuring cost needs
    /// to know which it got, and a cache that dropped it would answer that
    /// question differently the second time.
    pub recovered: bool,
}

impl Entry {
    pub fn new(transcript: TranscriptDto, coverage: CoverageDto, recovered: bool) -> Self {
        Self {
            version: ENTRY_VERSION,
            transcript,
            coverage,
            recovered,
        }
    }

    /// Whether this entry is one this build can read.
    pub fn is_readable(&self) -> bool {
        self.version == ENTRY_VERSION
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_span_survives_the_round_trip_exactly() {
        // Sub-millisecond precision is the whole point: a transcript whose
        // timestamps were rounded would differ from a computed one.
        for duration in [
            Duration::new(0, 0),
            Duration::new(1, 500_000), // half a millisecond
            Duration::new(12, 345_678_901),
            Duration::new(9_999, 999_999_999),
        ] {
            let span = Span::from(duration);
            assert_eq!(
                Duration::from(span),
                duration,
                "{duration:?} did not survive"
            );
        }
    }

    #[test]
    fn a_sub_millisecond_offset_is_not_rounded_away() {
        let start = Span::from(Duration::from_micros(1_500));
        assert_eq!(start.secs, 0);
        assert_eq!(start.nanos, 1_500_000);
        assert_eq!(
            Duration::from(start),
            Duration::from_micros(1_500),
            "milliseconds would have made this 1ms or 2ms"
        );
    }

    #[test]
    fn an_entry_round_trips_through_json() {
        let entry = Entry::new(
            TranscriptDto {
                segments: vec![SegmentDto {
                    id: 0,
                    start: Span::from(Duration::from_millis(1_234)),
                    end: Span::from(Duration::from_millis(5_678)),
                    text: "开放时间".to_string(),
                }],
                language: Some("zh".to_string()),
            },
            CoverageDto {
                decoded_seconds: 12.5,
                voiced_seconds: 9.0,
                energetic_seconds: 8.5,
            },
            false,
        );

        let encoded = serde_json::to_string(&entry).expect("serialize");
        let decoded: Entry = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, entry);
    }

    #[test]
    fn every_field_is_present_even_when_it_has_nothing_to_say() {
        // The same rule the CLI's report holds itself to, and for the same
        // reason: a reader should not have to branch on a key existing.
        let entry = Entry::new(
            TranscriptDto::default(),
            CoverageDto {
                decoded_seconds: 0.0,
                voiced_seconds: 0.0,
                energetic_seconds: 0.0,
            },
            false,
        );

        let value = serde_json::to_value(&entry).expect("serialize");
        let keys: Vec<&str> = value
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["coverage", "recovered", "transcript", "version"]);
        assert!(
            value["transcript"]["language"].is_null(),
            "null, not absent"
        );
    }

    #[test]
    fn an_entry_from_another_version_says_so() {
        let mut entry = Entry::new(
            TranscriptDto::default(),
            CoverageDto {
                decoded_seconds: 0.0,
                voiced_seconds: 0.0,
                energetic_seconds: 0.0,
            },
            false,
        );
        assert!(entry.is_readable());

        entry.version = ENTRY_VERSION + 1;
        assert!(!entry.is_readable());
    }
}
