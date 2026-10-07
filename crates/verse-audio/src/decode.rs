//! Decoding arbitrary audio and video files to PCM.
//!
//! ffmpeg writes raw samples to stdout and this reads them a chunk at a time,
//! so memory use is bounded by chunk size rather than file length — the same
//! property the pipeline relies on everywhere else.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
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
    /// stderr is drained on its own thread.
    ///
    /// Reading it only after stdout reaches EOF would deadlock: a chatty ffmpeg
    /// fills the stderr pipe, blocks on the write, and never closes stdout — so
    /// the EOF this side is waiting for can never arrive.
    stderr: Option<JoinHandle<String>>,
    /// The file's length in milliseconds, as ffmpeg reported it, or zero while
    /// it has not said.
    ///
    /// Shared with the drain thread, which is the only thing that reads
    /// ffmpeg's output. Written once, early — the banner comes before any
    /// audio — and read from [`FfmpegDecoder::total`] on the decoding thread
    /// while both are running.
    duration_millis: Arc<AtomicU64>,
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
            // `info`, not `error`, and this is the whole reason the decoder can
            // report progress at all: ffmpeg prints `Duration:` in its input
            // banner, which is emitted at `info` and was therefore being thrown
            // away. `-nostats` keeps the per-second counters off, so what
            // arrives is the banner and any complaint — nothing that grows with
            // the length of the file.
            //
            // Asking for a duration with a second ffmpeg pass was the
            // alternative, and it would have cost a second read of the file to
            // learn something this pass already knows.
            .arg("-nostats")
            .arg("-loglevel")
            .arg("info")
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
            // ffmpeg's own message. It is drained by a background thread —
            // see the `stderr` field.
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::new(ErrorKind::Io, format!("failed to start ffmpeg: {e}")))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::internal("ffmpeg stdout was not captured"))?;

        // Drain stderr from the moment the process starts, so a burst of
        // errors can never fill the pipe and stall ffmpeg.
        let duration_millis = Arc::new(AtomicU64::new(0));
        let observed = Arc::clone(&duration_millis);
        let stderr = child.stderr.take().map(|pipe| {
            std::thread::spawn(move || {
                let mut captured = String::new();

                // Line at a time rather than to EOF, because the length is
                // wanted *while* the file is being decoded and the banner
                // carrying it is the first thing ffmpeg says. Reading to the
                // end would deliver it only once the job was over.
                for line in BufReader::new(pipe).lines() {
                    let Ok(line) = line else { break };

                    if observed.load(Ordering::Relaxed) == 0 {
                        if let Some(found) = parse_duration_line(&line) {
                            observed.store(found.as_millis() as u64, Ordering::Relaxed);
                        }
                    }

                    captured.push_str(&line);
                    captured.push('\n');
                }

                captured
            })
        });

        Ok(Self {
            child,
            stdout,
            format,
            position: Duration::ZERO,
            finished: false,
            stderr,
            duration_millis,
        })
    }

    /// How long the file is, as ffmpeg reported it.
    ///
    /// `None` until the banner has been read, and `None` forever for a stream
    /// or container that declares no duration — including ffmpeg's own
    /// `Duration: N/A`. Callers must treat both the same way, because they
    /// cannot be told apart and a progress bar must not invent a denominator.
    ///
    /// Inherent rather than on [`AudioSource`]: this is a property of *this*
    /// decoder, and putting it on the trait would make every future source
    /// answer a question only ffmpeg can.
    pub fn total(&self) -> Option<Duration> {
        match self.duration_millis.load(Ordering::Relaxed) {
            0 => None,
            millis => Some(Duration::from_millis(millis)),
        }
    }

    /// Reap the child and surface its error output if it failed.
    fn finish(&mut self) -> Result<()> {
        let status = self
            .child
            .wait()
            .map_err(|e| Error::new(ErrorKind::Io, format!("failed to wait for ffmpeg: {e}")))?;

        // The child has exited, so its end of the stderr pipe is closed and the
        // drain thread has reached EOF.
        let stderr = self
            .stderr
            .take()
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default();

        if status.success() {
            Ok(())
        } else {
            let detail = complaint(&stderr);
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

/// The length ffmpeg announces, from a line like
/// `  Duration: 00:00:05.59, bitrate: 256 kb/s`.
///
/// `Duration: N/A` — a stream with no declared length — answers `None`, the
/// same as a banner that never arrived. That is deliberate: the two are
/// indistinguishable to a caller and must lead to the same behaviour, which is
/// a progress bar with no number in it.
fn parse_duration_line(line: &str) -> Option<Duration> {
    let rest = line.trim_start().strip_prefix("Duration:")?;
    let stamp = rest.trim_start().split(',').next()?.trim();

    let mut parts = stamp.split(':');
    let hours: f64 = parts.next()?.trim().parse().ok()?;
    let minutes: f64 = parts.next()?.trim().parse().ok()?;
    let seconds: f64 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() {
        return None;
    }

    // `from_secs_f64` panics on a negative or non-finite value, and this is
    // parsing text a program wrote, so it is checked rather than trusted.
    let total = hours * 3600.0 + minutes * 60.0 + seconds;
    (total.is_finite() && total >= 0.0).then(|| Duration::from_secs_f64(total))
}

/// What ffmpeg said that is worth repeating when it failed.
///
/// ffmpeg is asked for `info` so its banner is emitted — that is where the
/// duration comes from — and the banner would otherwise be the first thing a
/// person reads in an error message, pushing the actual complaint off the end.
/// The banner's shapes are few and stable; everything else is kept, because the
/// reason for a failure is usually a line nobody anticipated.
fn complaint(stderr: &str) -> String {
    let kept: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !is_banner(line))
        .collect();

    kept.join("\n")
}

/// Whether a line belongs to ffmpeg's description of the input rather than to
/// an account of what went wrong.
fn is_banner(line: &str) -> bool {
    const SHAPES: [&str; 6] = [
        "Input #",
        "Output #",
        "Duration:",
        "Stream #",
        "Stream mapping:",
        "Press [q]",
    ];

    SHAPES.iter().any(|shape| line.starts_with(shape)) || line.contains("Guessed Channel Layout")
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

        let chunk = AudioChunk {
            samples,
            format: self.format,
            start: self.position,
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    /// One line of ffmpeg's real banner, copied from this machine's ffmpeg
    /// 8.0.1 reading `models/sensevoice/zh.wav`. The leading spaces are
    /// ffmpeg's, and the shape is what the parser has to survive.
    const REAL_LINE: &str = "  Duration: 00:00:05.59, bitrate: 256 kb/s";

    #[test]
    fn the_banner_line_ffmpeg_actually_prints_is_understood() {
        assert_eq!(
            parse_duration_line(REAL_LINE),
            Some(Duration::from_secs_f64(5.59))
        );
    }

    #[test]
    fn hours_are_not_dropped() {
        // A two-hour recording is the case that would expose a parser counting
        // only minutes and seconds — and it is also the case where a wrong
        // denominator is least noticeable in a bar.
        assert_eq!(
            parse_duration_line("  Duration: 01:30:00.00, bitrate: 128 kb/s"),
            Some(Duration::from_secs(5400))
        );
    }

    #[test]
    fn a_stream_with_no_declared_length_answers_nothing() {
        // ffmpeg's own words for "no length", and the one input where a bar
        // must not invent a denominator.
        assert_eq!(parse_duration_line("  Duration: N/A, start: 0.000000"), None);
    }

    #[test]
    fn lines_that_are_not_the_banner_are_ignored() {
        // The parser sees every line ffmpeg writes, so anything that is not a
        // duration has to come back empty rather than nearly right.
        for line in [
            "Input #0, wav, from 'zh.wav':",
            "  Stream #0:0: Audio: pcm_s16le, 16000 Hz, mono, s16, 256 kb/s",
            "Stream mapping:",
            "",
            "  Duration:",
            "  Duration: nonsense, bitrate: 0 kb/s",
        ] {
            assert_eq!(parse_duration_line(line), None, "parsed {line:?}");
        }
    }

    #[test]
    fn a_failure_message_does_not_begin_with_the_banner() {
        // Raising the log level to `info` puts ffmpeg's description of the
        // input on the same stream as its complaints, and the whole point of
        // the error message is the complaint.
        let stderr = "\
Input #0, wav, from 'zh.wav':
  Duration: 00:00:05.59, bitrate: 256 kb/s
  Stream #0:0: Audio: pcm_s16le, 16000 Hz, mono, s16, 256 kb/s
Stream mapping:
  Stream #0:0 -> #0:0 (pcm_s16le (native) -> pcm_f32le (native))
zh.wav: Invalid data found when processing input
";

        let message = complaint(stderr);

        assert_eq!(message, "zh.wav: Invalid data found when processing input");
        assert!(
            !message.contains("Duration"),
            "the banner is not a complaint: {message}"
        );
    }

    #[test]
    fn a_failure_with_nothing_but_a_banner_says_nothing() {
        // Which leaves the exit status as the whole account, and that is better
        // than an error message made of the input description.
        assert_eq!(complaint("Input #0, wav, from 'x':\n  Duration: N/A\n"), "");
    }
}
