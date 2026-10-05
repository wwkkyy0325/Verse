//! Decoding arbitrary audio and video files to PCM.
//!
//! ffmpeg writes raw samples to stdout and this reads them a chunk at a time,
//! so memory use is bounded by chunk size rather than file length — the same
//! property the pipeline relies on everywhere else.

use std::io::Read;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::Duration;

use verse_core::{AudioChunk, AudioFormat, AudioSource, Error, ErrorKind, Result};

use crate::ffmpeg;

/// Frames per chunk. 0.1 s at 16 kHz — small enough to keep latency low, large
/// enough not to flood the channel with tiny reads.
const CHUNK_FRAMES: usize = 1_600;

/// Decodes any ffmpeg-readable file into mono f32 PCM.
///
/// Pull-based via [`AudioSource`]. Dropping the decoder kills the child
/// process, so an abandoned job does not leave ffmpeg running.
pub struct FfmpegDecoder {
    child: Child,
    stdout: ChildStdout,
    format: AudioFormat,
    position: Duration,
    /// Set once stdout reports EOF, so we only reap the child once.
    finished: bool,
}

impl FfmpegDecoder {
    /// Start decoding `path`.
    ///
    /// `format` is the output layout to request from ffmpeg — normally
    /// [`AudioFormat::TARGET`], the layout recognition engines expect. Asking
    /// ffmpeg to resample here is deliberate: it uses a proper resampler, which
    /// a naive linear one does not.
    pub fn open(path: &Path, format: AudioFormat) -> Result<Self> {
        if !path.is_file() {
            return Err(Error::new(
                ErrorKind::Io,
                format!("input file does not exist: {}", path.display()),
            ));
        }

        let ffmpeg = ffmpeg::locate()?;

        let mut child = Command::new(ffmpeg)
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-i")
            .arg(path)
            // Ignore any video stream: this project only wants audio.
            .arg("-vn")
            .arg("-f")
            .arg("f32le")
            .arg("-c:a")
            .arg("pcm_f32le")
            .arg("-ac")
            .arg(format.channels.to_string())
            .arg("-ar")
            .arg(format.sample_rate.to_string())
            // Write the result to stdout.
            .arg("-")
            .stdout(Stdio::piped())
            // Piped, not null, so a decode failure can be reported with
            // ffmpeg's own message. Safe because `-loglevel error` keeps the
            // volume far below the pipe buffer.
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                Error::new(ErrorKind::Io, format!("failed to start ffmpeg: {e}"))
            })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::internal("ffmpeg stdout was not captured"))?;

        Ok(Self { child, stdout, format, position: Duration::ZERO, finished: false })
    }

    /// Reap the child and surface its error output if it failed.
    fn finish(&mut self) -> Result<()> {
        let mut stderr = String::new();
        if let Some(mut pipe) = self.child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }

        let status = self
            .child
            .wait()
            .map_err(|e| Error::new(ErrorKind::Io, format!("failed to wait for ffmpeg: {e}")))?;

        if status.success() {
            Ok(())
        } else {
            let detail = stderr.trim();
            Err(Error::new(
                ErrorKind::Decode,
                if detail.is_empty() {
                    format!("ffmpeg exited with {status}")
                } else {
                    format!("ffmpeg failed: {detail}")
                },
            ))
        }
    }
}

impl AudioSource for FfmpegDecoder {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn next_chunk(&mut self) -> Result<Option<AudioChunk>> {
        if self.finished {
            return Ok(None);
        }

        let bytes_wanted = CHUNK_FRAMES * self.format.channels as usize * size_of::<f32>();
        let mut raw = vec![0u8; bytes_wanted];
        let mut filled = 0;

        while filled < bytes_wanted {
            match self.stdout.read(&mut raw[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    return Err(Error::new(
                        ErrorKind::Decode,
                        format!("failed to read from ffmpeg: {e}"),
                    ))
                }
            }
        }

        if filled == 0 {
            self.finished = true;
            self.finish()?;
            return Ok(None);
        }

        // A short read can split a sample; drop the trailing partial one rather
        // than fabricating a value. At 0.1 s chunks this is at most 3 bytes.
        let samples: Vec<f32> = raw[..filled]
            .chunks_exact(size_of::<f32>())
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();

        let chunk = AudioChunk { samples, format: self.format, start: self.position };
        self.position += chunk.duration();
        Ok(Some(chunk))
    }
}

impl Drop for FfmpegDecoder {
    fn drop(&mut self) {
        // Best effort. If the stream was consumed to EOF the child is already
        // reaped and this is a no-op.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
