//! The pipeline's side of the transcription cache.
//!
//! Two jobs, and they are the two things only this crate can do:
//!
//! - **Describe a run**, because the facts that decide what comes out are
//!   spread across `Request`, the detector's settings, the guard's, the
//!   engine's configuration and the ffmpeg build, and only here are all of
//!   them in one place.
//! - **Convert**, because `verse-store` deliberately does not depend on
//!   `verse-core` and so has its own replay types. The boundary between them is
//!   this file, the same arrangement `verse-cli/src/report.rs` has with the
//!   report and `verse-app/src/bridge.rs` has with the window.

use std::path::{Path, PathBuf};
use std::time::Duration;

use verse_core::{Segment, SegmentId, Transcript};

use crate::Request;

/// Whether a run consults the cache, and which one.
#[derive(Debug, Clone, Default)]
pub enum CachePolicy {
    /// Look nothing up and store nothing. What the benchmark uses: a cached
    /// benchmark measures the cache.
    ///
    /// The default, so a caller that has not thought about the cache gets the
    /// behaviour that cannot hand it the wrong transcript.
    #[default]
    Disabled,
    On {
        cache: verse_store::Cache,
        verify: verse_store::Verify,
    },
}

impl CachePolicy {
    /// Consult the cache under a data directory, with the verification mode the
    /// environment asks for.
    pub fn under(data_dir: &Path) -> Self {
        Self::On {
            cache: verse_store::Cache::under(data_dir),
            verify: verse_store::Verify::from_env(),
        }
    }

    /// The cache this run will store into, if it stores at all.
    pub(crate) fn open(&self) -> Option<(&verse_store::Cache, verse_store::Verify)> {
        match self {
            CachePolicy::Disabled => None,
            CachePolicy::On { cache, verify } => Some((cache, *verify)),
        }
    }
}

/// Everything about a run that decides what comes out of it.
///
/// `threads` is the **effective** count rather than what the caller asked for.
/// A caller passing `None` gets cores − 1, so two machines with different core
/// counts run with different thread counts while asking for the same thing —
/// and ONNX reduction order is not invariant under that. Recording `None` would
/// let a cache built on one machine answer for another.
///
/// Included although it costs hits between a run that named a thread count and
/// one that let the hardware choose. A false hit is the one mistake here with
/// no way back.
pub fn settings_digest(request: &Request, threads: usize, ffmpeg: Option<&str>) -> String {
    let mut fingerprint = verse_store::Fingerprint::new();

    fingerprint.add("engine", &request.engine);
    fingerprint.add("itn", request.inverse_text_normalization);
    fingerprint.add("threads", threads);
    fingerprint.add("max_output_tokens", said(request.max_output_tokens));
    fingerprint.add("hotwords", said_str(request.hotwords.as_deref()));

    fingerprint.add_f32("vad.threshold", request.vad.threshold);
    fingerprint.add_f32("vad.min_silence", request.vad.min_silence_seconds);
    fingerprint.add_f32("vad.min_speech", request.vad.min_speech_seconds);

    fingerprint.add_f64("guard.floor", request.guard.floor);
    fingerprint.add_f64("guard.min_energetic", request.guard.min_energetic_seconds);

    // The decoder, by build. Two ffmpegs can resample the same input to
    // different samples, and the recogniser sees the samples.
    fingerprint.add("ffmpeg", ffmpeg.unwrap_or("unknown"));

    // The model, file by file and by size and timestamp. Sorted, because the
    // order a directory happens to list in is not a fact about the run.
    let model_dir = request.models_dir.join(&request.engine);
    let files = model_files(&model_dir);
    fingerprint.add("model.count", files.len());
    for (name, path) in &files {
        fingerprint.add_file(&format!("model:{name}"), path);
    }

    fingerprint.add_file("vad_model", &request.vad_model);

    fingerprint.digest()
}

/// An optional number, present or absent being different facts.
///
/// `None` and `Some(0)` are not the same configuration, and a key that rendered
/// both as an empty string would say they were.
fn said(value: Option<i32>) -> String {
    match value {
        Some(n) => format!("some:{n}"),
        None => "none".to_string(),
    }
}

fn said_str(value: Option<&str>) -> String {
    match value {
        Some(text) => format!("some:{text}"),
        None => "none".to_string(),
    }
}

/// Every file under a model directory, by relative name and in a stable order.
///
/// A missing directory yields nothing rather than an error: a model that is not
/// installed is a run that will fail anyway, and the fingerprint's job is to
/// describe the run, not to validate it.
fn model_files(root: &Path) -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    collect(root, root, &mut found);
    found.sort();
    found
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, out);
        } else if path.is_file() {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            out.push((relative.to_string_lossy().into_owned(), path));
        }
    }
}

/// A finished run, in the form the cache stores.
pub fn to_entry(transcription: &crate::Transcription) -> verse_store::Entry {
    let segments = transcription
        .transcript
        .segments
        .iter()
        .map(|segment| verse_store::SegmentDto {
            id: segment.id.0,
            start: segment.start.into(),
            end: segment.end.into(),
            text: segment.text.clone(),
        })
        .collect();

    verse_store::Entry::new(
        verse_store::TranscriptDto {
            segments,
            language: transcription.transcript.language.clone(),
        },
        verse_store::CoverageDto {
            decoded_seconds: transcription.coverage.decoded_seconds,
            voiced_seconds: transcription.coverage.voiced_seconds,
            energetic_seconds: transcription.coverage.energetic_seconds,
        },
        transcription.recovered,
    )
}

/// A stored run, back in the form the pipeline uses.
pub fn from_entry(entry: &verse_store::Entry) -> crate::Transcription {
    let segments = entry
        .transcript
        .segments
        .iter()
        .map(|segment| Segment {
            id: SegmentId(segment.id),
            start: Duration::from(segment.start),
            end: Duration::from(segment.end),
            text: segment.text.clone(),
        })
        .collect();

    crate::Transcription {
        transcript: Transcript {
            segments,
            language: entry.transcript.language.clone(),
        },
        coverage: crate::Coverage {
            decoded_seconds: entry.coverage.decoded_seconds,
            voiced_seconds: entry.coverage.voiced_seconds,
            energetic_seconds: entry.coverage.energetic_seconds,
        },
        recovered: entry.recovered,
        cached: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use verse_audio::VadSettings;

    fn request(models_dir: &Path) -> Request {
        Request {
            input: models_dir.join("audio.wav"),
            models_dir: models_dir.to_path_buf(),
            engine: "sensevoice".to_string(),
            vad_model: models_dir.join("silero-vad").join("silero_vad.onnx"),
            inverse_text_normalization: true,
            vad: VadSettings::default(),
            max_output_tokens: None,
            guard: crate::GuardSettings::default(),
            hotwords: None,
            threads: None,
            cache: CachePolicy::Disabled,
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("verse-pipeline-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn the_same_request_gives_the_same_digest() {
        let dir = scratch("cache-digest-stable");
        let request = request(&dir);
        assert_eq!(
            settings_digest(&request, 7, Some("ffmpeg version 6.0")),
            settings_digest(&request, 7, Some("ffmpeg version 6.0"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_different_thread_count_is_a_different_run() {
        // ONNX reduction order is not thread-count invariant, so the same
        // request run with a different number of threads is not the same result.
        let dir = scratch("cache-digest-threads");
        let request = request(&dir);
        assert_ne!(
            settings_digest(&request, 7, None),
            settings_digest(&request, 3, None)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_different_ffmpeg_build_is_a_different_run() {
        let dir = scratch("cache-digest-ffmpeg");
        let request = request(&dir);
        assert_ne!(
            settings_digest(&request, 7, Some("ffmpeg version 6.0")),
            settings_digest(&request, 7, Some("ffmpeg version 7.1"))
        );
        // And ffmpeg being absent is its own answer rather than an empty one
        // that might match a build with an empty version line.
        assert_ne!(
            settings_digest(&request, 7, None),
            settings_digest(&request, 7, Some(""))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_model_file_is_a_different_run() {
        let dir = scratch("cache-digest-model");
        let model_dir = dir.join("sensevoice");
        std::fs::create_dir_all(&model_dir).expect("mkdir");
        std::fs::write(model_dir.join("model.int8.onnx"), b"first").expect("write");

        let request = request(&dir);
        let before = settings_digest(&request, 7, None);

        std::fs::write(model_dir.join("model.int8.onnx"), b"second, larger").expect("rewrite");
        let after = settings_digest(&request, 7, None);

        assert_ne!(before, after, "the model is part of what produced the text");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_added_model_file_is_a_different_run() {
        // A model directory that gained a file is not the model that ran.
        let dir = scratch("cache-digest-model-added");
        let model_dir = dir.join("sensevoice");
        std::fs::create_dir_all(&model_dir).expect("mkdir");
        std::fs::write(model_dir.join("tokens.txt"), b"tokens").expect("write");

        let request = request(&dir);
        let before = settings_digest(&request, 7, None);

        std::fs::write(model_dir.join("extra.onnx"), b"surprise").expect("write");
        let after = settings_digest(&request, 7, None);

        assert_ne!(before, after);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_order_a_directory_lists_in_does_not_change_the_digest() {
        // readdir order is not stable across filesystems or runs, and it is not
        // a fact about the run.
        let dir = scratch("cache-digest-order");
        let model_dir = dir.join("sensevoice");
        std::fs::create_dir_all(&model_dir).expect("mkdir");
        for name in ["a.onnx", "b.onnx", "c.onnx", "d.onnx"] {
            std::fs::write(model_dir.join(name), name.as_bytes()).expect("write");
        }

        let request = request(&dir);
        let digest = settings_digest(&request, 7, None);
        for _ in 0..5 {
            assert_eq!(digest, settings_digest(&request, 7, None));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn absent_and_zero_are_different_facts() {
        let dir = scratch("cache-digest-optional");
        let mut request = request(&dir);
        let absent = settings_digest(&request, 7, None);

        request.max_output_tokens = Some(0);
        let zero = settings_digest(&request, 7, None);

        assert_ne!(absent, zero, "an unset budget is not a budget of zero");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_detector_threshold_is_a_different_run() {
        let dir = scratch("cache-digest-vad");
        let mut request = request(&dir);
        let before = settings_digest(&request, 7, None);

        request.vad = VadSettings {
            threshold: request.vad.threshold + 0.01,
            ..request.vad
        };
        let after = settings_digest(&request, 7, None);

        assert_ne!(before, after);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_run_survives_the_round_trip_through_the_cache_format() {
        let transcription = crate::Transcription {
            transcript: Transcript {
                segments: vec![Segment {
                    id: SegmentId(3),
                    start: Duration::new(1, 500_000),
                    end: Duration::new(4, 250_000),
                    text: "开放时间".to_string(),
                }],
                language: Some("zh".to_string()),
            },
            coverage: crate::Coverage {
                decoded_seconds: 10.0,
                voiced_seconds: 9.0,
                energetic_seconds: 8.0,
            },
            recovered: true,
            cached: false,
        };

        let back = from_entry(&to_entry(&transcription));

        assert_eq!(back.transcript.segments, transcription.transcript.segments);
        assert_eq!(back.transcript.language, transcription.transcript.language);
        assert_eq!(
            back.coverage.energetic_seconds,
            transcription.coverage.energetic_seconds
        );
        assert!(back.recovered, "the guard's decision is part of the result");
        assert!(back.cached, "and it came from the cache");
    }
}
