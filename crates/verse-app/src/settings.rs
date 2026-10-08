//! The window's own preferences.
//!
//! One question so far: what the close button does. It is asked the first time
//! the button is pressed and the answer is remembered, because an application
//! that hides to the tray without being asked looks like an application that
//! failed to close — people press ✕ twice and then reach for the task manager.
//!
//! Deliberately *not* merged with the unfinished queue that lives beside it in
//! the same directory. Preferences change when somebody answers a question;
//! that file changes on every job. Sharing one file would mean rewriting the
//! preferences every time a file finished.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The version of the file's shape, so an older one is ignored rather than
/// misread. Same rule as `verse_store::History`.
const RECORD_VERSION: u32 = 1;

/// What pressing ✕ does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CloseAction {
    /// Hide the window and keep running, reachable from the tray.
    Hide,
    /// End the process.
    Quit,
}

/// Everything the window remembers between launches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    version: u32,
    /// `None` until the question has been asked and answered.
    #[serde(default)]
    pub close: Option<CloseAction>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: RECORD_VERSION,
            close: None,
        }
    }
}

impl Settings {
    /// The preferences with the close question answered.
    ///
    /// The only way to set it: the field is private, so nothing outside this
    /// module can produce a `Settings` whose `version` it chose itself.
    pub fn answered(action: CloseAction) -> Self {
        Self {
            version: RECORD_VERSION,
            close: Some(action),
        }
    }

    /// Read the preferences, or the defaults.
    ///
    /// Every failure is the default rather than an error. A preferences file
    /// that cannot be read is a reason to ask the question again, not a reason
    /// to refuse to start — the same reasoning `History::load` gives.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(settings) = serde_json::from_str::<Settings>(&text) else {
            return Self::default();
        };
        if settings.version != RECORD_VERSION {
            return Self::default();
        }
        settings
    }

    /// Write the preferences, atomically.
    ///
    /// Through a temporary name so a crash midway cannot leave a file that
    /// parses but says something the user never chose.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let text = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, path)
    }
}

/// The queue of work that was interrupted.
///
/// Written whenever the set changes rather than only on the way out, so a crash
/// or a task-manager kill is covered by the same code as a deliberate quit —
/// and so quitting needs no special case of its own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    version: u32,
    /// In the order they would have run.
    #[serde(default)]
    pub files: Vec<PathBuf>,
    /// The engine they were being run with.
    ///
    /// Not decoration: the resume log's key is a digest of the settings *and*
    /// the input, and the engine is part of the settings. Restore a queue under
    /// a different engine and every key misses — so "continue" would quietly
    /// redo the whole file, which is precisely what the confirmation promises
    /// it will not do.
    #[serde(default)]
    pub engine: String,
}

impl Pending {
    /// Read the queue, or an empty one.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(pending) = serde_json::from_str::<Pending>(&text) else {
            return Self::default();
        };
        if pending.version != RECORD_VERSION {
            return Self::default();
        }
        pending
    }

    pub fn of(engine: String, files: Vec<PathBuf>) -> Self {
        Self {
            version: RECORD_VERSION,
            files,
            engine,
        }
    }

    /// Write the queue, atomically, and at the same path as the preferences.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let text = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("verse-settings-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir.join(name)
    }

    #[test]
    fn an_unanswered_question_stays_unanswered() {
        // The whole point of `Option`: `None` means "do not assume", and a
        // default of `Quit` would silently throw away the first ✕ on a machine
        // whose user never got asked.
        assert_eq!(Settings::default().close, None);
        assert_eq!(Settings::load(&scratch("absent.json")).close, None);
    }

    #[test]
    fn an_answer_survives_a_restart() {
        let path = scratch("round-trip.json");
        let asked = Settings {
            close: Some(CloseAction::Hide),
            ..Settings::default()
        };
        asked.save(&path).expect("save");

        assert_eq!(Settings::load(&path).close, Some(CloseAction::Hide));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_file_that_makes_no_sense_asks_again_rather_than_guessing() {
        let path = scratch("corrupt.json");
        std::fs::write(&path, "{ this is not json").expect("write");
        assert_eq!(Settings::load(&path).close, None);

        // And a file from a future version, which parses but must not be read.
        std::fs::write(&path, r#"{"version":99,"close":"hide"}"#).expect("write");
        assert_eq!(Settings::load(&path).close, None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_queue_is_empty_when_there_was_no_queue() {
        assert!(Pending::load(&scratch("no-queue.json")).files.is_empty());
    }

    #[test]
    fn the_queue_comes_back_in_the_order_it_would_have_run() {
        let path = scratch("queue.json");
        let queue = Pending::of(
            "sensevoice".into(),
            vec![PathBuf::from("first.m4a"), PathBuf::from("second.m4a")],
        );
        queue.save(&path).expect("save");

        let read = Pending::load(&path);
        assert_eq!(read.files, queue.files);
        assert_eq!(read.engine, "sensevoice");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_older_queue_file_without_an_engine_still_reads() {
        // `#[serde(default)]` on the engine, so a file written by a build
        // before it existed is a queue of files rather than a parse failure.
        let path = scratch("older.json");
        std::fs::write(&path, r#"{"version":1,"files":["a.m4a"]}"#).expect("write");

        let read = Pending::load(&path);
        assert_eq!(read.files.len(), 1);
        assert_eq!(read.engine, "");
        let _ = std::fs::remove_file(&path);
    }
}
