//! Audio decoding and conversion, backed by the ffmpeg sidecar.
//!
//! ffmpeg is invoked as a child process, never linked. That keeps the widest
//! possible format coverage (`design.md` §4.1) without adding FFI to a project
//! whose constraints rule it out (`design.md` §3, C2).
//!
//! Phase 1 uses [`decode`] and [`convert`]. Phase 2 adds a `capture` module
//! (cpal plus per-platform loopback) behind a feature flag, so Phase 1 builds
//! do not pull in a device layer.

pub mod convert;
pub mod decode;
pub mod ffmpeg;
pub mod vad;

pub use convert::{transcode, TranscodeRequest};
pub use decode::FfmpegDecoder;
pub use ffmpeg::{is_available as ffmpeg_available, locate as locate_ffmpeg, FFMPEG_ENV};
pub use vad::{SileroVad, DEFAULT_THRESHOLD};
