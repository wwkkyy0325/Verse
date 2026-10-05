//! Locating and invoking the ffmpeg sidecar.
//!
//! ffmpeg runs as a child process rather than a linked library. A crash, leak
//! or memory blow-up inside ffmpeg therefore cannot reach this process — which
//! is the entire reason for the sidecar design (see `design.md` §6.5 and the
//! no-leak constraint in §3).

use std::path::PathBuf;
use std::process::{Command, Stdio};
use verse_core::{Error, ErrorKind, Result};

/// Overrides ffmpeg discovery. Point it at a full path when ffmpeg is not on
/// `PATH` or when a specific build is needed.
pub const FFMPEG_ENV: &str = "VERSE_FFMPEG";

fn program_name() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

/// Locate the ffmpeg executable.
///
/// Search order:
/// 1. the `VERSE_FFMPEG` environment variable
/// 2. `ffmpeg` on `PATH`
/// 3. a copy shipped next to the Verse binary (P1b, not implemented yet)
///
/// Step 3 matters for the "install and run" goal — requiring a separate ffmpeg
/// install would break it — but during P1a the developer's own ffmpeg is fine.
pub fn locate() -> Result<PathBuf> {
    if let Some(configured) = std::env::var_os(FFMPEG_ENV) {
        let path = PathBuf::from(configured);
        if path.is_file() {
            return Ok(path);
        }
        return Err(Error::new(
            ErrorKind::Io,
            format!(
                "{FFMPEG_ENV} points at a file that does not exist: {}",
                path.display()
            ),
        ));
    }

    let name = program_name();
    if Command::new(name)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
    {
        return Ok(PathBuf::from(name));
    }

    Err(Error::new(
        ErrorKind::Io,
        format!(
            "ffmpeg not found. Install it, or point {FFMPEG_ENV} at the executable. \
             Verse uses it to read audio formats and to convert between them."
        ),
    ))
}

/// Whether ffmpeg is available, without the error text. Useful for capability
/// probing and for tests that must skip when it is absent.
pub fn is_available() -> bool {
    locate().is_ok()
}
