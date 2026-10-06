//! The transcription cache on disk.
//!
//! One file per transcription, named by its key. Everything here is written on
//! the assumption that the cache is a **convenience and not state**: any file
//! may be missing, truncated, corrupt, or from a different version of Verse,
//! and the worst consequence of any of those is that the work is done again.
//!
//! That assumption is what licenses the strictness. An entry that does not
//! parse is discarded rather than repaired; a version directory this build does
//! not recognise is ignored rather than migrated. A cache that tried to be
//! clever about recovering its own damage would be a second thing that can go
//! wrong, in service of making something faster that is already optional.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use crate::entry::Entry;

/// The directory name for this entry format. A future format gets `v2`, and
/// this build then ignores it rather than misreading it.
const DIR_VERSION: &str = "v1";

/// How old a half-written file must be before it is considered abandoned.
///
/// Generous, because the alternative — sweeping a temporary that another
/// process is still writing — is a race with no upside.
const STALE_TEMPORARY: Duration = Duration::from_secs(24 * 60 * 60);

/// The prefix marking a file that is not yet an entry.
const TEMPORARY: &str = ".tmp-";

/// Counts temporary names within a process, so two writes at the same
/// nanosecond in the same process cannot collide.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A transcription cache rooted at a directory.
#[derive(Debug, Clone)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    /// A cache under a data directory, which is what [`crate::data_dir`]
    /// returns.
    pub fn under(data_dir: &Path) -> Self {
        Self::new(data_dir.join("cache"))
    }

    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The per-user directory this cache sits under, which is where the other
    /// files of Verse's own go — the resume logs, for one.
    ///
    /// `None` for a cache constructed with [`Cache::new`] at an arbitrary path,
    /// which has no such directory. Callers that reached it through
    /// [`Cache::under`] always get one.
    pub fn data_dir(&self) -> Option<PathBuf> {
        self.root.parent().map(Path::to_path_buf)
    }

    fn entries_dir(&self) -> PathBuf {
        self.root.join(DIR_VERSION)
    }

    fn entry_path(&self, key: &str) -> PathBuf {
        self.entries_dir().join(format!("{key}.json"))
    }

    /// The entry for a key, if there is a readable one.
    ///
    /// Returns `None` for every kind of not-having-it: no file, unreadable
    /// file, malformed JSON, a version this build does not know. A caller
    /// cannot act differently on those, and making it try would only invite
    /// handling that differs between callers.
    pub fn get(&self, key: &str) -> Option<Entry> {
        // A key that is not a plain hex name would let a caller write outside
        // the cache directory. Keys are always produced by `key::key`, but this
        // is checked here rather than trusted from there.
        if !key.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }

        let path = self.entry_path(key);
        let text = std::fs::read_to_string(&path).ok()?;

        let entry = match serde_json::from_str::<Entry>(&text) {
            Ok(entry) if entry.is_readable() => entry,
            // Present but unusable. Removed so it is not re-read on every run
            // from now on; failing to remove it is fine and not worth a word.
            _ => {
                let _ = std::fs::remove_file(&path);
                return None;
            }
        };

        Some(entry)
    }

    /// Store an entry.
    ///
    /// Written to a temporary name and renamed into place, so a crash midway
    /// leaves a file that is obviously not an entry rather than one that parses
    /// as a truncated transcription — which would be handed back as a short
    /// result with nothing to distinguish it from a genuinely short recording.
    pub fn put(&self, key: &str, entry: &Entry) -> std::io::Result<()> {
        if !key.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cache keys are hexadecimal",
            ));
        }

        std::fs::create_dir_all(self.entries_dir())?;

        let text = serde_json::to_string(entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        let temporary = self.entries_dir().join(format!(
            "{TEMPORARY}{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&temporary, text)?;

        let destination = self.entry_path(key);
        match std::fs::rename(&temporary, &destination) {
            Ok(()) => Ok(()),
            Err(e) => {
                // Leaving a temporary behind would accumulate; the sweep would
                // get it eventually, but there is no reason to make it wait.
                let _ = std::fs::remove_file(&temporary);
                Err(e)
            }
        }
    }

    /// Forget one entry.
    pub fn forget(&self, key: &str) -> bool {
        std::fs::remove_file(self.entry_path(key)).is_ok()
    }

    /// How much is stored.
    pub fn usage(&self) -> Usage {
        let mut usage = Usage::default();
        let Ok(dir) = std::fs::read_dir(self.entries_dir()) else {
            return usage;
        };

        for file in dir.flatten() {
            let Ok(metadata) = file.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let name = file.file_name().to_string_lossy().into_owned();
            if name.starts_with(TEMPORARY) {
                usage.temporaries += 1;
                usage.temporary_bytes += metadata.len();
            } else if name.ends_with(".json") {
                usage.entries += 1;
                usage.bytes += metadata.len();
            }
        }
        usage
    }

    /// Remove everything, and report what went.
    pub fn clean(&self) -> std::io::Result<Usage> {
        let before = self.usage();
        let dir = self.entries_dir();
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(before)
    }

    /// Remove half-written files left by a process that died.
    ///
    /// Only ones older than a day, because a temporary belonging to a download
    /// still in flight is not litter — deleting it would be a race whose only
    /// possible outcome is losing somebody's work.
    pub fn sweep_temporaries(&self) -> usize {
        let Ok(dir) = std::fs::read_dir(self.entries_dir()) else {
            return 0;
        };

        let mut removed = 0;
        for file in dir.flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            if !name.starts_with(TEMPORARY) {
                continue;
            }
            let Ok(metadata) = file.metadata() else {
                continue;
            };
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            let too_old = SystemTime::now()
                .duration_since(modified)
                .is_ok_and(|age| age > STALE_TEMPORARY);
            if too_old && std::fs::remove_file(file.path()).is_ok() {
                removed += 1;
            }
        }
        removed
    }
}

/// What is stored, in files and in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub entries: usize,
    pub bytes: u64,
    /// Half-written files. Counted separately because they are not results and
    /// a caller reporting "N transcriptions cached" should not be counting
    /// them.
    pub temporaries: usize,
    pub temporary_bytes: u64,
}

impl Usage {
    pub fn total_bytes(&self) -> u64 {
        self.bytes + self.temporary_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{CoverageDto, SegmentDto, Span, TranscriptDto};
    use std::time::Duration as StdDuration;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("verse-store-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn an_entry(text: &str) -> Entry {
        Entry::new(
            TranscriptDto {
                segments: vec![SegmentDto {
                    id: 0,
                    start: Span::from(StdDuration::from_millis(0)),
                    end: Span::from(StdDuration::from_millis(1_000)),
                    text: text.to_string(),
                }],
                language: None,
            },
            CoverageDto {
                decoded_seconds: 1.0,
                voiced_seconds: 1.0,
                energetic_seconds: 1.0,
            },
            false,
        )
    }

    #[test]
    fn what_was_put_comes_back() {
        let dir = scratch("cache-roundtrip");
        let cache = Cache::under(&dir);
        let entry = an_entry("开放时间");

        cache.put("abc123", &entry).expect("put");
        assert_eq!(cache.get("abc123"), Some(entry));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_entry_is_a_miss_not_an_error() {
        let dir = scratch("cache-miss");
        let cache = Cache::under(&dir);
        assert_eq!(cache.get("deadbeef"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_entry_is_a_miss_and_is_removed() {
        // Left in place it would be re-read and re-parsed on every run.
        let dir = scratch("cache-corrupt");
        let cache = Cache::under(&dir);
        let path = cache.entry_path("abc123");
        std::fs::create_dir_all(path.parent().expect("dir")).expect("mkdir");
        std::fs::write(&path, "{ not json at all").expect("plant");

        assert_eq!(cache.get("abc123"), None);
        assert!(!path.exists(), "an unusable entry should not be kept");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_entry_from_another_version_is_a_miss_and_is_removed() {
        let dir = scratch("cache-version");
        let cache = Cache::under(&dir);
        let path = cache.entry_path("abc123");
        std::fs::create_dir_all(path.parent().expect("dir")).expect("mkdir");

        let mut entry = an_entry("open");
        entry.version = crate::entry::ENTRY_VERSION + 1;
        std::fs::write(&path, serde_json::to_string(&entry).expect("encode")).expect("plant");

        assert_eq!(cache.get("abc123"), None);
        assert!(!path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_key_that_is_not_hexadecimal_is_refused() {
        // A key becomes a filename. `../../something` would write outside the
        // cache directory, and the key is only hex because the code that makes
        // it says so — which is exactly the kind of assumption that stops being
        // true quietly.
        let dir = scratch("cache-traversal");
        let cache = Cache::under(&dir);

        assert_eq!(cache.get("../../escaped"), None);
        assert!(cache.put("../../escaped", &an_entry("x")).is_err());
        assert!(!dir.join("escaped.json").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_temporary_file_is_never_read_as_an_entry() {
        let dir = scratch("cache-temporary");
        let cache = Cache::under(&dir);
        let entries = cache.entries_dir();
        std::fs::create_dir_all(&entries).expect("mkdir");

        // Named as an entry, but with the temporary prefix.
        let temporary = entries.join(format!("{TEMPORARY}1234-0"));
        std::fs::write(
            &temporary,
            serde_json::to_string(&an_entry("x")).expect("encode"),
        )
        .expect("plant");

        // Even asked for the key it appears to hold, it is not served.
        assert_eq!(cache.get("1234-0"), None);
        assert_eq!(cache.usage().entries, 0);
        assert_eq!(cache.usage().temporaries, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_write_leaves_no_temporary_behind() {
        let dir = scratch("cache-no-litter");
        let cache = Cache::under(&dir);
        cache.put("abc123", &an_entry("开放时间")).expect("put");

        assert_eq!(cache.usage().temporaries, 0);
        assert_eq!(cache.usage().entries, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn usage_counts_what_is_there() {
        let dir = scratch("cache-usage");
        let cache = Cache::under(&dir);
        cache.put("aaa111", &an_entry("one")).expect("put");
        cache.put("bbb222", &an_entry("two")).expect("put");

        let usage = cache.usage();
        assert_eq!(usage.entries, 2);
        assert!(usage.bytes > 0);
        assert_eq!(usage.temporaries, 0);
        assert_eq!(usage.total_bytes(), usage.bytes);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleaning_removes_everything_and_says_what_went() {
        let dir = scratch("cache-clean");
        let cache = Cache::under(&dir);
        cache.put("aaa111", &an_entry("one")).expect("put");
        cache.put("bbb222", &an_entry("two")).expect("put");

        let removed = cache.clean().expect("clean");
        assert_eq!(removed.entries, 2);
        assert_eq!(cache.get("aaa111"), None);
        assert_eq!(cache.usage().entries, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forgetting_one_leaves_the_others() {
        let dir = scratch("cache-forget");
        let cache = Cache::under(&dir);
        cache.put("aaa111", &an_entry("one")).expect("put");
        cache.put("bbb222", &an_entry("two")).expect("put");

        assert!(cache.forget("aaa111"));
        assert_eq!(cache.get("aaa111"), None);
        assert!(cache.get("bbb222").is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fresh_temporary_is_left_alone() {
        // It may belong to a write still in progress.
        let dir = scratch("cache-sweep-fresh");
        let cache = Cache::under(&dir);
        let entries = cache.entries_dir();
        std::fs::create_dir_all(&entries).expect("mkdir");
        let fresh = entries.join(format!("{TEMPORARY}now-0"));
        std::fs::write(&fresh, b"half a file").expect("plant");

        assert_eq!(cache.sweep_temporaries(), 0);
        assert!(fresh.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_entry_is_not_mistaken_for_a_temporary() {
        let dir = scratch("cache-sweep-entries");
        let cache = Cache::under(&dir);
        cache.put("abc123", &an_entry("open")).expect("put");

        assert_eq!(cache.sweep_temporaries(), 0);
        assert!(cache.get("abc123").is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
