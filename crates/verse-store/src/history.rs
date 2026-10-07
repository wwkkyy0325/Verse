//! What has been transcribed, so it can be found again.
//!
//! The window's file list lived only in memory: closing it lost the list while
//! the `.srt` files it had written stayed in the output folder. The results
//! survived and the record of them did not, so somebody who transcribed a
//! meeting last week had no way to reach it from inside the program.
//!
//! **A pointer, not a copy.** An entry says which file, which engine, when, and
//! where the result went. It does not hold the transcript: the `.srt` on disk
//! *is* the result, and a second copy here would be a second thing that can
//! drift from the one a person can open in a player.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The version of the record's shape, so an older file is ignored rather than
/// misread.
const RECORD_VERSION: u32 = 1;

/// How many transcriptions are remembered.
///
/// A record that grows without limit is a leak with a friendlier name, and the
/// list is something a person scrolls. Two hundred is more than anybody scrolls
/// and small enough to read at startup.
const MOST_REMEMBERED: usize = 200;

/// One transcription that has been done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Past {
    /// The recording that was transcribed.
    pub input: PathBuf,
    /// Where the transcript was written.
    pub output: PathBuf,
    /// The engine that produced it. Recorded per entry because it can be
    /// changed between files, and a list of results with different engines
    /// behind them should say so.
    pub engine: String,
    /// When it finished, in milliseconds since the Unix epoch.
    pub finished_at_ms: u64,
}

/// Every transcription this machine remembers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct History {
    version: u32,
    /// Oldest first, so the newest is the one most likely to be looked at and
    /// the cap drops from the front.
    entries: Vec<Past>,
}

impl Default for History {
    fn default() -> Self {
        Self {
            version: RECORD_VERSION,
            entries: Vec::new(),
        }
    }
}

impl History {
    /// Read the record, or an empty one.
    ///
    /// Every failure is an empty record rather than an error, as
    /// [`crate::Ownership::load`] is: this is a convenience, and refusing to
    /// start because a bookkeeping file is unreadable would make the
    /// bookkeeping more important than the program.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(record) = serde_json::from_str::<History>(&text) else {
            return Self::default();
        };
        if record.version != RECORD_VERSION {
            return Self::default();
        }
        record
    }

    /// Write the record, atomically.
    ///
    /// Through a temporary name so a crash midway cannot leave a file that
    /// parses as valid but truncated — which would silently orphan everything
    /// after the cut.
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

    /// Note a finished transcription, newest last.
    ///
    /// An entry for the same input is replaced rather than added: transcribing
    /// a file again supersedes the last time, and two rows for one recording
    /// would be two rows a person has to choose between.
    pub fn record(&mut self, past: Past) {
        self.entries.retain(|entry| entry.input != past.input);
        self.entries.push(past);

        // Dropped from the front, which is the oldest.
        if self.entries.len() > MOST_REMEMBERED {
            let excess = self.entries.len() - MOST_REMEMBERED;
            self.entries.drain(..excess);
        }
    }

    /// Forget one recording, leaving its result where it is.
    ///
    /// The pair to [`History::prune`]: that one drops entries whose file has
    /// gone, this one drops an entry on purpose. Answers whether there was
    /// anything to forget, so a caller can tell "removed" from "was not there".
    pub fn forget(&mut self, input: &Path) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.input != input);
        self.entries.len() != before
    }

    /// Forget entries whose result is no longer on disk.
    ///
    /// An entry is a pointer, and a pointer to nothing is not history: showing
    /// a row that opens to an error would be worse than not showing it.
    pub fn prune(&mut self) {
        self.entries.retain(|entry| entry.output.is_file());
    }

    /// Oldest first.
    pub fn entries(&self) -> &[Past] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("verse-history-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn past(input: &str, output: &Path) -> Past {
        Past {
            input: PathBuf::from(input),
            output: output.to_path_buf(),
            engine: "sensevoice".to_string(),
            finished_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn what_is_written_can_be_read_back() {
        let dir = scratch("round-trip");
        let path = dir.join("history.json");
        let result = dir.join("a.srt");
        std::fs::write(&result, "subtitle").expect("write");

        let mut history = History::default();
        history.record(past("a.wav", &result));
        history.save(&path).expect("save");

        let read = History::load(&path);
        assert_eq!(read.entries().len(), 1);
        assert_eq!(read.entries()[0].input, PathBuf::from("a.wav"));
        assert_eq!(read.entries()[0].engine, "sensevoice");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_record_that_cannot_be_read_is_empty_rather_than_fatal() {
        let dir = scratch("broken");
        let path = dir.join("history.json");

        std::fs::write(&path, "{ this is not json").expect("write");
        assert!(History::load(&path).is_empty());

        std::fs::write(&path, r#"{"version": 99, "entries": []}"#).expect("write");
        assert!(
            History::load(&path).is_empty(),
            "a shape from the future is not half-read"
        );

        assert!(History::load(&dir.join("not-there.json")).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transcribing_a_file_again_replaces_its_row_rather_than_adding_one() {
        let dir = scratch("repeat");
        let first = dir.join("a.srt");
        let second = dir.join("a (2).srt");

        let mut history = History::default();
        history.record(past("a.wav", &first));
        history.record(past("b.wav", &first));
        history.record(past("a.wav", &second));

        assert_eq!(history.entries().len(), 2, "a.wav has one row");
        assert_eq!(
            history.entries().last().expect("newest").output,
            second,
            "and it is the newest one"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_row_whose_result_is_gone_is_forgotten() {
        let dir = scratch("prune");
        let here = dir.join("here.srt");
        std::fs::write(&here, "subtitle").expect("write");

        let mut history = History::default();
        history.record(past("here.wav", &here));
        history.record(past("gone.wav", &dir.join("gone.srt")));
        assert_eq!(history.entries().len(), 2);

        history.prune();

        assert_eq!(history.entries().len(), 1, "a pointer to nothing is not history");
        assert_eq!(history.entries()[0].input, PathBuf::from("here.wav"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forgetting_one_recording_leaves_the_others() {
        let dir = scratch("forget");
        let mut history = History::default();
        history.record(past("a.wav", &dir.join("out.srt")));
        history.record(past("b.wav", &dir.join("out.srt")));

        assert!(history.forget(Path::new("a.wav")), "there was one to forget");
        assert_eq!(history.entries().len(), 1);
        assert_eq!(history.entries()[0].input, PathBuf::from("b.wav"));

        assert!(
            !history.forget(Path::new("a.wav")),
            "forgetting it twice says so rather than pretending"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_record_does_not_grow_without_limit() {
        let dir = scratch("cap");
        let mut history = History::default();

        for index in 0..(MOST_REMEMBERED + 25) {
            history.record(past(&format!("{index}.wav"), &dir.join("out.srt")));
        }

        assert_eq!(history.entries().len(), MOST_REMEMBERED);
        assert_eq!(
            history.entries()[0].input,
            PathBuf::from(format!("{}.wav", 25)),
            "the oldest are the ones dropped"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
