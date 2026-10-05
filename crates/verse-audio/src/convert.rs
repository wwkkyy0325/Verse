//! Converting audio between formats.
//!
//! Decoding answers "get me PCM to recognize". This module answers "give me
//! this audio as that format" — any input ffmpeg can read to any output it can
//! write.

use std::path::PathBuf;
use std::process::Command;

use verse_core::{Error, ErrorKind, Result};

use crate::ffmpeg;

/// A transcoding request.
///
/// Only `input` and `output` are required. The named fields cover the common
/// knobs; `extra_args` is the escape hatch for everything else, so "any format
/// to any format" stays reachable even for codecs and flags not modelled here.
#[derive(Debug, Clone)]
pub struct TranscodeRequest {
    pub input: PathBuf,
    pub output: PathBuf,
    /// ffmpeg codec name, e.g. `libmp3lame`. When `None`, ffmpeg infers one
    /// from the output extension.
    pub codec: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    /// Audio bitrate such as `192k`. Ignored by lossless codecs.
    pub bitrate: Option<String>,
    /// Raw ffmpeg arguments, inserted immediately before the output path.
    pub extra_args: Vec<String>,
}

impl TranscodeRequest {
    pub fn new(input: impl Into<PathBuf>, output: impl Into<PathBuf>) -> Self {
        Self {
            input: input.into(),
            output: output.into(),
            codec: None,
            sample_rate: None,
            channels: None,
            bitrate: None,
            extra_args: Vec::new(),
        }
    }

    pub fn with_codec(mut self, codec: impl Into<String>) -> Self {
        self.codec = Some(codec.into());
        self
    }

    pub fn with_sample_rate(mut self, hz: u32) -> Self {
        self.sample_rate = Some(hz);
        self
    }

    pub fn with_channels(mut self, channels: u16) -> Self {
        self.channels = Some(channels);
        self
    }

    pub fn with_bitrate(mut self, bitrate: impl Into<String>) -> Self {
        self.bitrate = Some(bitrate.into());
        self
    }

    /// Append raw ffmpeg arguments. The escape hatch for anything not named
    /// above, e.g. `-ss` for trimming.
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.extra_args.extend(args.into_iter().map(Into::into));
        self
    }
}

/// Run a transcode, blocking until ffmpeg exits.
///
/// The output is overwritten. This is a conversion tool; refusing to replace a
/// previous run's output would surprise more often than it would protect.
pub fn transcode(request: &TranscodeRequest) -> Result<()> {
    if !request.input.is_file() {
        return Err(Error::new(
            ErrorKind::Io,
            format!("input file does not exist: {}", request.input.display()),
        ));
    }

    let ffmpeg = ffmpeg::locate()?;

    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-hide_banner").arg("-loglevel").arg("error").arg("-y");
    cmd.arg("-i").arg(&request.input);

    if let Some(codec) = &request.codec {
        cmd.arg("-c:a").arg(codec);
    }
    if let Some(rate) = request.sample_rate {
        cmd.arg("-ar").arg(rate.to_string());
    }
    if let Some(channels) = request.channels {
        cmd.arg("-ac").arg(channels.to_string());
    }
    if let Some(bitrate) = &request.bitrate {
        cmd.arg("-b:a").arg(bitrate);
    }

    cmd.args(&request.extra_args);
    cmd.arg(&request.output);

    let output = cmd
        .output()
        .map_err(|e| Error::new(ErrorKind::Io, format!("failed to run ffmpeg: {e}")))?;

    if output.status.success() {
        return Ok(());
    }

    let detail = String::from_utf8_lossy(&output.stderr);
    Err(Error::new(
        ErrorKind::Decode,
        format!(
            "could not convert {} to {}: {}",
            request.input.display(),
            request.output.display(),
            detail.trim()
        ),
    ))
}
