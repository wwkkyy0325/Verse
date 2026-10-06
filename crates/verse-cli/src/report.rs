//! The machine-readable report.
//!
//! `verse-core` has no dependencies and no serde derives, which is deliberate.
//! So the wire format lives here, where the wire is — the same arrangement
//! `verse-app/src/bridge.rs` uses for the window, and for the same reason: a
//! renamed field should fail a test rather than a consumer.
//!
//! **One shape for one file and for many.** A single file is a batch of one.
//! An agent should never have to branch on how many inputs it happened to
//! pass, and two shapes would mean two code paths here and two in every
//! caller.
//!
//! **Every field is always present**, `null` rather than absent when there is
//! nothing to say. Optional keys are pleasant to write and miserable to read:
//! the consumer needs `?.` on every access and can never tell "absent" from
//! "this build does not have that field".

use serde::Serialize;

use verse_core::ErrorKind;
use verse_pipeline::Transcription;

/// The report format's own version.
///
/// Separate from the tool's version, and bumped only when the shape of this
/// document changes in a way a reader would have to know about.
pub const VERSION: u32 = 1;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// The schema this document follows. See [`VERSION`].
    pub version: u32,
    /// What ran, so a result can be judged without asking again.
    pub engine: String,
    pub models_dir: String,
    /// The vocabulary bias that was applied, echoed back. `null` when none was
    /// given.
    pub hotwords: Option<String>,
    /// False if any file failed. The one field a caller has to check.
    pub ok: bool,
    pub succeeded: usize,
    pub failed: usize,
    pub elapsed_ms: u64,
    /// Run-level remarks. Never carries a failure — those are per file.
    pub warnings: Vec<String>,
    pub results: Vec<FileResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileResult {
    pub input: String,
    /// Where the transcript was written, or `null` for a failure or stdout.
    pub output: Option<String>,
    pub format: &'static str,
    pub ok: bool,
    pub elapsed_ms: u64,
    pub segment_count: usize,
    /// Fraction of the file's non-silent audio that reached the recogniser.
    ///
    /// `null` when there was too little sound to judge. This and `recovered`
    /// are the two fields that say whether the transcript can be trusted:
    /// a short transcript and a truncated one look identical without them.
    pub coverage: Option<f64>,
    /// The segmenter was distrusted and the file recognised whole.
    pub recovered: bool,
    /// The transcript came from the cache rather than from the recogniser.
    ///
    /// The field that explains why a file `elapsed_ms` says took no time at
    /// all. Without it, a caller timing a batch would conclude the second run
    /// was faster rather than that it did not happen.
    pub cached: bool,
    pub language: Option<String>,
    /// The whole transcript as one string, newline separated.
    pub text: Option<String>,
    /// The caller's own diagnostics for this file. Never carries a failure.
    pub warnings: Vec<String>,
    pub segments: Vec<SegmentDto>,
    /// Present only when this file failed.
    pub error: Option<ErrorDto>,
}

/// What models exist and which are installed.
///
/// Answering "is the model here" should not require reading a table, since it
/// is the question an agent asks before deciding whether to run a
/// transcription or a download.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelList {
    pub version: u32,
    pub models_dir: String,
    pub models: Vec<ModelEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    pub id: String,
    pub display_name: String,
    pub present: bool,
    /// Where it is, or would be.
    pub directory: String,
    pub mirrors: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentDto {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDto {
    /// Lower case, matching the exit-code vocabulary, so a caller can branch
    /// on either without a lookup table.
    pub kind: &'static str,
    pub message: String,
}

/// The name used for an error kind, in the report and on stderr alike.
///
/// Deliberately the same words the exit codes are documented with: an agent
/// that reads the code and an agent that reads the JSON should be learning the
/// same thing.
pub fn kind_name(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Io => "io",
        ErrorKind::Network => "network",
        ErrorKind::Decode => "decode",
        ErrorKind::Model => "model",
        ErrorKind::Engine => "engine",
        ErrorKind::Sink => "sink",
        ErrorKind::Registry => "registry",
        ErrorKind::Cancelled => "cancelled",
        ErrorKind::Internal => "internal",
    }
}

impl FileResult {
    /// The result of one file that was recognised.
    pub fn transcribed(
        input: &std::path::Path,
        output: Option<&std::path::Path>,
        format: &'static str,
        elapsed_ms: u64,
        transcription: &Transcription,
    ) -> Self {
        let transcript = &transcription.transcript;

        Self {
            input: input.display().to_string(),
            // `-` means stdout, which is not a file that was written.
            output: output
                .filter(|path| *path != std::path::Path::new("-"))
                .map(|path| path.display().to_string()),
            format,
            ok: true,
            elapsed_ms,
            segment_count: transcript.segments.len(),
            coverage: transcription.coverage.ratio(),
            recovered: transcription.recovered,
            cached: transcription.cached,
            language: transcript.language.clone(),
            text: Some(transcript.to_text()),
            warnings: Vec::new(),
            segments: transcript
                .segments
                .iter()
                .map(|segment| SegmentDto {
                    start_ms: segment.start.as_millis() as u64,
                    end_ms: segment.end.as_millis() as u64,
                    text: segment.text.clone(),
                })
                .collect(),
            error: None,
        }
    }

    /// The result of one file that was not.
    pub fn failed(
        input: &std::path::Path,
        format: &'static str,
        elapsed_ms: u64,
        kind: ErrorKind,
        message: String,
    ) -> Self {
        Self {
            input: input.display().to_string(),
            output: None,
            format,
            ok: false,
            elapsed_ms,
            segment_count: 0,
            coverage: None,
            recovered: false,
            cached: false,
            language: None,
            text: None,
            warnings: Vec::new(),
            segments: Vec::new(),
            error: Some(ErrorDto {
                kind: kind_name(kind),
                message,
            }),
        }
    }
}

impl Report {
    pub fn new(
        engine: String,
        models_dir: String,
        hotwords: Option<String>,
        warnings: Vec<String>,
        results: Vec<FileResult>,
        elapsed_ms: u64,
    ) -> Self {
        let succeeded = results.iter().filter(|r| r.ok).count();
        let failed = results.len() - succeeded;

        Self {
            version: VERSION,
            engine,
            models_dir,
            hotwords,
            ok: failed == 0,
            succeeded,
            failed,
            elapsed_ms,
            warnings,
            results,
        }
    }

    /// Write the report to stdout as a single JSON document.
    pub fn print(&self) {
        print_json(self);
    }
}

/// Write one JSON document to stdout, and nothing else to it.
///
/// Serialising a struct of strings and numbers cannot fail for any reason the
/// caller could act on. Printing nothing at all would be worse than printing a
/// document that says so, because the alternative to a broken document is a
/// consumer waiting for one that never arrives.
pub fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string(value) {
        Ok(json) => println!("{json}"),
        Err(e) => println!(
            r#"{{"version":{VERSION},"ok":false,"error":{{"kind":"internal","message":"report could not be serialised: {e}"}}}}"#
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    /// These pin the wire format, exactly as `verse-app/src/bridge.rs` pins
    /// the one the window reads. Nothing else connects the two sides: a
    /// renamed field would serialise happily and only fail in whoever was
    /// reading it.
    fn a_result() -> FileResult {
        FileResult {
            input: "会议.m4a".to_string(),
            output: Some("会议.srt".to_string()),
            format: "srt",
            ok: true,
            elapsed_ms: 4123,
            segment_count: 1,
            coverage: Some(0.998),
            recovered: false,
            cached: false,
            language: None,
            text: Some("开放时间".to_string()),
            warnings: Vec::new(),
            segments: vec![SegmentDto {
                start_ms: 0,
                end_ms: 1500,
                text: "开放时间".to_string(),
            }],
            error: None,
        }
    }

    #[test]
    fn the_report_names_its_own_format_version() {
        let value = serde_json::to_value(Report::new(
            "sensevoice".to_string(),
            "models".to_string(),
            None,
            Vec::new(),
            vec![a_result()],
            4200,
        ))
        .expect("serializes");

        assert_eq!(value["version"], json!(VERSION));
    }

    #[test]
    fn a_result_uses_camel_case_milliseconds() {
        let value = serde_json::to_value(a_result()).expect("serializes");

        assert_eq!(value["elapsedMs"], json!(4123));
        assert_eq!(value["segmentCount"], json!(1));
        assert_eq!(value["segments"][0]["startMs"], json!(0));
        assert_eq!(value["segments"][0]["endMs"], json!(1500));
    }

    #[test]
    fn every_field_is_present_even_when_it_has_nothing_to_say() {
        // Absent keys are pleasant to write and miserable to read: the
        // consumer needs a fallback on every access and cannot tell "absent"
        // from "this build does not have it".
        let value = serde_json::to_value(a_result()).expect("serializes");
        let object = value.as_object().expect("an object");

        for key in [
            "input",
            "output",
            "format",
            "ok",
            "elapsedMs",
            "segmentCount",
            "coverage",
            "recovered",
            "cached",
            "language",
            "text",
            "warnings",
            "segments",
            "error",
        ] {
            assert!(object.contains_key(key), "missing key: {key}");
        }

        assert_eq!(value["language"], json!(null));
        assert_eq!(value["error"], json!(null));
    }

    #[test]
    fn a_failure_carries_an_error_and_no_text() {
        let value = serde_json::to_value(FileResult::failed(
            Path::new("broken.m4a"),
            "srt",
            12,
            ErrorKind::Decode,
            "no audio stream".to_string(),
        ))
        .expect("serializes");

        assert_eq!(value["ok"], json!(false));
        assert_eq!(value["text"], json!(null));
        assert_eq!(value["output"], json!(null));
        assert_eq!(value["coverage"], json!(null));
        assert_eq!(value["error"]["kind"], json!("decode"));
        assert_eq!(value["error"]["message"], json!("no audio stream"));
    }

    #[test]
    fn the_error_kind_vocabulary_is_lower_case_and_stable() {
        // The same words the exit codes are documented with, so that reading
        // the code and reading the JSON teach the same thing.
        let names: Vec<&str> = [
            ErrorKind::Io,
            ErrorKind::Network,
            ErrorKind::Decode,
            ErrorKind::Model,
            ErrorKind::Engine,
            ErrorKind::Sink,
            ErrorKind::Registry,
            ErrorKind::Cancelled,
            ErrorKind::Internal,
        ]
        .iter()
        .map(|kind| kind_name(*kind))
        .collect();

        assert_eq!(
            names,
            vec![
                "io", "network", "decode", "model", "engine", "sink", "registry", "cancelled",
                "internal"
            ]
        );
    }

    #[test]
    fn the_summary_counts_what_the_results_say() {
        let report = Report::new(
            "sensevoice".to_string(),
            "models".to_string(),
            None,
            Vec::new(),
            vec![
                a_result(),
                FileResult::failed(
                    Path::new("broken.m4a"),
                    "srt",
                    1,
                    ErrorKind::Decode,
                    "no audio stream".to_string(),
                ),
            ],
            99,
        );

        assert_eq!(report.succeeded, 1);
        assert_eq!(report.failed, 1);
        assert!(!report.ok);
    }

    #[test]
    fn a_report_with_no_failures_says_so() {
        let report = Report::new(
            "sensevoice".to_string(),
            "models".to_string(),
            None,
            Vec::new(),
            vec![a_result()],
            10,
        );

        assert!(report.ok);
        assert_eq!(report.failed, 0);
    }

    #[test]
    fn stdout_is_not_reported_as_a_file_that_was_written() {
        let mut result = a_result();
        result.output = Some("-".to_string());

        let value = serde_json::to_value(FileResult {
            output: None,
            ..result
        })
        .expect("serializes");

        assert_eq!(value["output"], json!(null));
    }
}
