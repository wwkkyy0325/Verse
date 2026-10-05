//! Integration tests for VAD segmentation.
//!
//! Needs ffmpeg, a Silero VAD model and a speech sample. Skips when any of
//! those is missing, so the suite still passes on a bare machine.

use std::path::{Path, PathBuf};
use std::process::Command;

use verse_audio::{ffmpeg_available, locate_ffmpeg, FfmpegDecoder, SileroVad};
use verse_core::{AudioFormat, AudioSource, Segmenter};

const RATE: u32 = 16_000;

/// Path relative to the workspace root. Tests run with the crate directory as
/// the working directory, so this climbs back out.
fn workspace_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn vad_model() -> PathBuf {
    workspace_path("models/vad/silero_vad.onnx")
}

fn speech_sample() -> PathBuf {
    workspace_path("models/sensevoice/zh.wav")
}

fn temp_file(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("verse-vad-test-{name}"))
}

/// `silence_secs` of silence, then the real speech sample.
///
/// The leading silence is the point: it makes the reported span offset
/// observable, which is what pins down what the detector means by a start
/// index.
///
/// `name` keeps tests from sharing a temp file. They run in parallel and each
/// removes its own input when done, so a shared path means one test deleting
/// another's file mid-run.
fn silence_then_speech(silence_secs: u32, name: &str) -> Option<PathBuf> {
    if !ffmpeg_available() || !speech_sample().is_file() {
        return None;
    }

    let out = temp_file(name);
    let status = Command::new(locate_ffmpeg().ok()?)
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg(format!("anullsrc=r={RATE}:cl=mono:d={silence_secs}"))
        .arg("-i")
        .arg(speech_sample())
        .arg("-filter_complex")
        .arg("[0][1]concat=n=2:v=0:a=1")
        .arg("-ar")
        .arg(RATE.to_string())
        .arg("-ac")
        .arg("1")
        .arg(&out)
        .status()
        .ok()?;

    status.success().then_some(out)
}

/// Segment a file and return its spans.
fn spans_of(audio: &Path) -> Vec<verse_core::AudioChunk> {
    let mut source = FfmpegDecoder::open(audio, AudioFormat::TARGET).expect("decode audio");
    let mut vad = SileroVad::load(&vad_model(), AudioFormat::TARGET).expect("load VAD");

    let mut spans = Vec::new();
    while let Some(chunk) = source.next_chunk().expect("chunk") {
        vad.accept(&chunk).expect("feed VAD");
        spans.extend(vad.take());
    }
    spans.extend(vad.finish());
    spans
}

#[test]
fn span_offsets_accumulate_from_the_stream_start() {
    if !vad_model().is_file() {
        eprintln!("skipping: VAD model not present");
        return;
    }
    let Some(padded) = silence_then_speech(2, "offset.wav") else {
        eprintln!("skipping: ffmpeg or speech sample unavailable");
        return;
    };

    let bare = spans_of(&speech_sample());
    assert!(
        !bare.is_empty(),
        "speech should be detected in the bare sample"
    );

    let padded_spans = spans_of(&padded);
    assert!(
        !padded_spans.is_empty(),
        "speech should be detected after padding"
    );

    // Differential rather than absolute: the sample is a real recording and
    // begins with some silence of its own, so the bare run establishes the
    // baseline and only the *difference* is meaningful. That difference is
    // what proves offsets are measured from the start of the stream rather
    // than from wherever the detector happened to begin.
    let shift = padded_spans[0].start.as_secs_f64() - bare[0].start.as_secs_f64();
    assert!(
        (1.7..=2.3).contains(&shift),
        "prepending 2s of silence should shift the first span by ~2s, got {shift:.2}s"
    );

    // Offsets must increase, or subtitle timestamps would be nonsense.
    for pair in padded_spans.windows(2) {
        assert!(
            pair[1].start >= pair[0].start,
            "span offsets must not go backwards"
        );
    }

    let _ = std::fs::remove_file(&padded);
}

#[test]
fn span_timestamps_stay_within_the_audio() {
    if !vad_model().is_file() {
        eprintln!("skipping: VAD model not present");
        return;
    }
    let Some(audio) = silence_then_speech(1, "carry.wav") else {
        eprintln!("skipping: ffmpeg or speech sample unavailable");
        return;
    };

    let mut source = FfmpegDecoder::open(&audio, AudioFormat::TARGET).expect("decode audio");
    let mut total_samples = 0usize;
    let mut vad = SileroVad::load(&vad_model(), AudioFormat::TARGET).expect("load VAD");

    let mut spans = Vec::new();
    while let Some(chunk) = source.next_chunk().expect("chunk") {
        total_samples += chunk.samples.len();
        vad.accept(&chunk).expect("feed VAD");
        spans.extend(vad.take());
    }
    spans.extend(vad.finish());

    let audio_secs = total_samples as f64 / RATE as f64;

    for span in &spans {
        let start = span.start.as_secs_f64();
        let end = start + span.samples.len() as f64 / RATE as f64;
        eprintln!(
            "span: start={start:.3}s end={end:.3}s len={:.3}s  (audio is {audio_secs:.3}s)",
            span.samples.len() as f64 / RATE as f64
        );
        assert!(
            end <= audio_secs + 0.05,
            "span ending at {end:.3}s exceeds the {audio_secs:.3}s of audio"
        );
    }

    let _ = std::fs::remove_file(&audio);
}

#[test]
fn spans_carry_recognizable_audio() {
    if !vad_model().is_file() {
        eprintln!("skipping: VAD model not present");
        return;
    }
    let Some(audio) = silence_then_speech(1, "timestamps.wav") else {
        eprintln!("skipping: ffmpeg or speech sample unavailable");
        return;
    };

    let mut source = FfmpegDecoder::open(&audio, AudioFormat::TARGET).expect("decode audio");
    let mut vad = SileroVad::load(&vad_model(), AudioFormat::TARGET).expect("load VAD");

    let mut spans = Vec::new();
    while let Some(chunk) = source.next_chunk().expect("chunk") {
        vad.accept(&chunk).expect("feed VAD");
        spans.extend(vad.take());
    }
    spans.extend(vad.finish());

    let total: usize = spans.iter().map(|s| s.samples.len()).sum();
    assert!(
        total > RATE as usize / 2,
        "spans should carry real audio, got {total} samples total"
    );

    // The detector never emits more audio than it was given.
    let source_len = {
        let mut s = FfmpegDecoder::open(&audio, AudioFormat::TARGET).expect("decode audio");
        let mut n = 0;
        while let Some(chunk) = s.next_chunk().expect("chunk") {
            n += chunk.samples.len();
        }
        n
    };
    assert!(
        total <= source_len,
        "spans ({total} samples) cannot exceed the input ({source_len} samples)"
    );

    let _ = std::fs::remove_file(&audio);
}
