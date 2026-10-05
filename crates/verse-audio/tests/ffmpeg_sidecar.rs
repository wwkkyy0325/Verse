//! Integration tests for the ffmpeg sidecar.
//!
//! A working ffmpeg is required. When none is present these skip rather than
//! fail, so the suite still passes on a machine without it.

use std::path::{Path, PathBuf};
use std::process::Command;

use verse_audio::{ffmpeg_available, locate_ffmpeg, transcode, FfmpegDecoder, TranscodeRequest};
use verse_core::{AudioFormat, AudioSource};

const SECONDS: u32 = 2;
const SOURCE_RATE: u32 = 44_100;

fn temp_file(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("verse-audio-test-{name}"))
}

/// A 2 s stereo 44.1 kHz tone.
///
/// Deliberately neither the target rate nor the target channel count, so that
/// decoding has to resample and downmix rather than pass samples straight
/// through.
fn make_source() -> Option<PathBuf> {
    if !ffmpeg_available() {
        return None;
    }
    let path = temp_file("source.wav");
    let status = Command::new(locate_ffmpeg().ok()?)
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg(format!("sine=frequency=440:duration={SECONDS}"))
        .arg("-ar")
        .arg(SOURCE_RATE.to_string())
        .arg("-ac")
        .arg("2")
        .arg(&path)
        .status()
        .ok()?;
    status.success().then_some(path)
}

fn count_frames<S: AudioSource>(mut source: S) -> usize {
    let mut frames = 0;
    while let Some(chunk) = source.next_chunk().expect("chunk") {
        frames += chunk.samples.len() / chunk.format.channels as usize;
    }
    frames
}

#[test]
fn missing_input_is_reported_before_ffmpeg_is_invoked() {
    let err = FfmpegDecoder::open(Path::new("definitely-not-here.wav"), AudioFormat::TARGET)
        .err()
        .expect("missing input should fail");

    assert!(
        err.message().contains("does not exist"),
        "error should name the problem, got: {}",
        err.message()
    );
}

#[test]
fn decodes_with_resampling_and_downmixing() {
    let Some(src) = make_source() else {
        eprintln!("skipping: ffmpeg not available");
        return;
    };

    let decoder = FfmpegDecoder::open(&src, AudioFormat::TARGET).expect("open decoder");
    assert_eq!(decoder.format(), AudioFormat::TARGET);

    let frames = count_frames(decoder);
    let expected = SECONDS as usize * AudioFormat::TARGET.sample_rate as usize;

    // ffmpeg's resampler has edge behaviour, so allow a tenth of a second.
    let tolerance = AudioFormat::TARGET.sample_rate as usize / 10;
    assert!(
        frames.abs_diff(expected) < tolerance,
        "expected about {expected} frames after resampling to 16 kHz, got {frames}"
    );

    let _ = std::fs::remove_file(&src);
}

#[test]
fn decodes_only_the_first_chunk_on_demand() {
    let Some(src) = make_source() else {
        eprintln!("skipping: ffmpeg not available");
        return;
    };

    let mut decoder = FfmpegDecoder::open(&src, AudioFormat::TARGET).expect("open decoder");
    let first = decoder.next_chunk().expect("first chunk");

    let chunk = first.expect("a two-second tone should yield at least one chunk");
    assert_eq!(chunk.start, std::time::Duration::ZERO, "first chunk starts at zero");
    assert!(
        chunk.samples.len() <= 1_600,
        "chunks should be bounded, got {} samples",
        chunk.samples.len()
    );

    let _ = std::fs::remove_file(&src);
}

#[test]
fn converts_between_formats() {
    let Some(src) = make_source() else {
        eprintln!("skipping: ffmpeg not available");
        return;
    };
    let mp3 = temp_file("converted.mp3");

    transcode(
        &TranscodeRequest::new(&src, &mp3)
            .with_codec("libmp3lame")
            .with_bitrate("128k"),
    )
    .expect("convert wav to mp3");

    let size = std::fs::metadata(&mp3).expect("output exists").len();
    assert!(size > 0, "converted file should not be empty");

    // Present is not enough — it has to be readable back.
    let decoder = FfmpegDecoder::open(&mp3, AudioFormat::TARGET).expect("decode the mp3");
    assert!(count_frames(decoder) > 0, "round-tripped audio should have frames");

    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&mp3);
}
