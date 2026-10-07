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
/// 2. a copy shipped next to the Verse binary
/// 3. `ffmpeg` on `PATH`
///
/// **The bundled copy wins over `PATH`,** which is the opposite of what this
/// comment said while the step was unwritten. A copy that ships with the
/// program is the one the release was built and tested against; `PATH` holds
/// whatever the machine happens to have, and a user who wants theirs can say so
/// with `VERSE_FFMPEG`, which is still checked first.
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

    if let Some(shipped) = bundled(std::env::current_exe().ok().as_deref()) {
        return Ok(shipped);
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

/// A copy of ffmpeg that shipped with the program, wherever the installer put
/// it.
///
/// Three layouts, because a bundle is a different shape on each platform and
/// the program cannot ask the bundler where it put things:
///
/// - **beside the executable** — a flat install, which is what the Windows
///   installers produce and where the resources land;
/// - **`../lib/Verse`** — the `.deb` and AppImage layout, with the binary in
///   `bin/` and everything else under `lib/`.
///
/// **The Linux layout is reasoned from how the bundle is structured and has not
/// been checked**, because this project has only ever been built on Windows. A
/// copy that lands somewhere else is a copy that is not found, and the symptom
/// is the ordinary "ffmpeg not found" message — honest, but not useful. The
/// first release run is what will say.
///
/// macOS had a third entry here — `../Resources`, inside `Contents/` — and it
/// was removed with macOS itself rather than left as a path nothing produces.
fn bundled(exe: Option<&std::path::Path>) -> Option<PathBuf> {
    let dir = exe?.parent()?;

    let candidates = [
        // The plain sibling first, through the function the test exercises.
        beside(exe),
        Some(dir.join("..").join("lib").join("Verse").join(program_name())),
    ];

    candidates.into_iter().flatten().find(|path| path.is_file())
}

/// A copy of ffmpeg sitting next to `exe`, if there is one.
///
/// Takes the executable's path rather than reading `current_exe` so a test can
/// ask about a directory it made. Answers only a file that exists: a bundled
/// copy that is missing is not a bundled copy, and falling through to `PATH` is
/// better than returning a path to nothing.
fn beside(exe: Option<&std::path::Path>) -> Option<PathBuf> {
    let candidate = exe?.parent()?.join(program_name());
    candidate.is_file().then_some(candidate)
}

/// Whether ffmpeg is available, without the error text. Useful for capability
/// probing and for tests that must skip when it is absent.
pub fn is_available() -> bool {
    locate().is_ok()
}

/// The first line of `ffmpeg -version`, for the ffmpeg that would be used.
///
/// Decoding is reproducible only for a given binary. Two builds can resample
/// the same input to different samples, so a transcript is the same result only
/// under the same ffmpeg — which is why anything that caches recognition needs
/// to know which one produced it.
///
/// `None` when ffmpeg is absent or says nothing. A caller keying a cache should
/// read that as "unknown", and deciding what to do about unknown is the
/// caller's business rather than something to guess at here.
pub fn version() -> Option<String> {
    let ffmpeg = locate().ok()?;
    let output = Command::new(ffmpeg).arg("-version").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);

    text.lines()
        .next()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_beside_the_program_is_found() {
        let dir = std::env::temp_dir().join("verse-ffmpeg-beside");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");

        let program = dir.join(if cfg!(windows) { "verse.exe" } else { "verse" });
        std::fs::write(&program, b"not really a program").expect("write");

        assert_eq!(beside(Some(&program)), None, "nothing there yet");

        let bundled = dir.join(program_name());
        std::fs::write(&bundled, b"not really ffmpeg").expect("write");

        assert_eq!(
            beside(Some(&program)),
            Some(bundled),
            "the copy next to the program is the one that ships"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_program_with_no_directory_answers_nothing_rather_than_panicking() {
        // `current_exe` can fail, and a path with no parent exists.
        assert_eq!(beside(None), None);
        assert_eq!(beside(Some(std::path::Path::new(""))), None);
    }
}
