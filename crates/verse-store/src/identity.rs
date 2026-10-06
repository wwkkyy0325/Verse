//! What makes one input file the same as another.
//!
//! Two questions depend on this, and they want the same answer:
//!
//! - The result cache asks whether *this exact audio* has been transcribed
//!   before.
//! - The output record asks whether a file already sitting in the output
//!   directory was written from *this* input, in which case re-running should
//!   replace it rather than produce `name (2).srt`.
//!
//! The identity is metadata — canonical path, length, modification time — and
//! deliberately not the file's contents. Contents cost a full read, and a two
//! gigabyte video read on every run to decide whether to skip work is a way of
//! making the fast path slow. The cache adds a bounded digest on top of this,
//! because there the consequence of being wrong is worse; here it is one
//! needlessly serial-numbered filename.

use std::path::{Path, PathBuf};

/// A file, as it was when we looked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileId {
    /// Absolute where the filesystem could resolve it, as given otherwise.
    ///
    /// On Windows this keeps the `\\?\` prefix that `canonicalize` produces.
    /// That looks ugly in a cache filename and is the price of the path being
    /// unambiguous about separators and long names.
    pub path: PathBuf,
    pub len: u64,
    /// `None` when the platform or the filesystem will not say.
    pub modified_nanos: Option<u128>,
}

impl FileId {
    /// Look a file up. Fails only if it cannot be stat-ed at all.
    pub fn of(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::metadata(path)?;

        Ok(Self {
            path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
            len: metadata.len(),
            modified_nanos: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|since| since.as_nanos()),
        })
    }

    /// A stable string for this file, for use as a map key.
    ///
    /// Stable means: the same file gives the same string on the next run and
    /// on another machine with the same layout. Timestamps are nanoseconds
    /// since the epoch, which is what the filesystem actually stores, rather
    /// than a formatted date that would lose the distinction between two
    /// writes in the same second.
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}",
            self.path.display(),
            self.len,
            match self.modified_nanos {
                Some(nanos) => nanos.to_string(),
                None => "-".to_string(),
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("verse-store-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn the_same_file_gives_the_same_key_twice() {
        let dir = scratch("identity-stable");
        let file = dir.join("a.wav");
        std::fs::write(&file, b"hello").expect("write");

        let first = FileId::of(&file).expect("stat");
        let second = FileId::of(&file).expect("stat");
        assert_eq!(first.key(), second.key());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_length_changes_the_key() {
        let dir = scratch("identity-length");
        let file = dir.join("a.wav");
        std::fs::write(&file, b"hello").expect("write");
        let before = FileId::of(&file).expect("stat").key();

        std::fs::write(&file, b"hello, world").expect("rewrite");
        let after = FileId::of(&file).expect("stat").key();

        assert_ne!(before, after, "a longer file is not the same file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_key_survives_being_written_to_json() {
        // The key is a map key in the ownership record, so it has to round-trip
        // through JSON as a plain string with nothing to escape badly.
        let dir = scratch("identity-json");
        let file = dir.join("会议 记录.m4a");
        std::fs::write(&file, b"x").expect("write");

        let id = FileId::of(&file).expect("stat");
        let encoded = serde_json::to_string(&id.key()).expect("serialize");
        let decoded: String = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, id.key());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_is_an_error_rather_than_a_default() {
        // Silently inventing an identity for a file that is not there would
        // make two different missing paths look like the same input.
        let dir = scratch("identity-missing");
        assert!(FileId::of(&dir.join("nope.wav")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
