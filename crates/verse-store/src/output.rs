//! Which file a transcript is written to.
//!
//! Output used to land beside its input, where the only way two files could
//! collide was `a.wav` and `a.mp3` in the same folder. Writing everything into
//! one directory turns that from an edge case into the ordinary case: two
//! recordings, each called `会议.m4a`, in different folders, both belong here.
//!
//! The rule:
//!
//! - A source that already has a file here **replaces it**. Running the same
//!   command twice leaves one file, not two.
//! - A different source gets ` (2)`, ` (3)`, … — the lowest number not taken
//!   by somebody else, preferring one already belonging to it, so the numbering
//!   is stable across runs.
//! - A file with **no record** is never replaced. It might be a transcript from
//!   before this record existed, or something the user put there. Replacing it
//!   would destroy work with no way to tell that it had.
//!
//! That third rule is why the record exists at all. Without it, "is this file
//! mine?" has no answer, and the safe answer — never overwrite anything — would
//! make every run produce a new numbered file until the folder was unusable.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::identity::FileId;

/// The version of the record's shape, so an older file is ignored rather than
/// misread.
const RECORD_VERSION: u32 = 1;

/// Which source each known output file came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ownership {
    version: u32,
    /// Source key ([`FileId::key`]) to the output path it was written to.
    ///
    /// Keyed by source rather than by output because the question asked most
    /// often is "where did this input go?", and the reverse — "who wrote this
    /// file?" — is a scan over a map that holds one entry per transcription the
    /// user has ever made in this folder.
    outputs: BTreeMap<String, PathBuf>,
}

impl Default for Ownership {
    fn default() -> Self {
        Self {
            version: RECORD_VERSION,
            outputs: BTreeMap::new(),
        }
    }
}

impl Ownership {
    /// Read the record, or start a new one.
    ///
    /// Every failure is an empty record rather than an error. This file is a
    /// convenience: losing it costs at most a needlessly numbered filename,
    /// and refusing to transcribe because it is unreadable would make a
    /// bookkeeping file more important than the product.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(record) = serde_json::from_str::<Ownership>(&text) else {
            return Self::default();
        };
        if record.version != RECORD_VERSION {
            return Self::default();
        }
        record
    }

    /// Write the record, atomically.
    ///
    /// Through a temporary name, so a crash midway cannot leave a file that
    /// parses as a valid but truncated record — the failure that would quietly
    /// orphan every entry after the cut.
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

    /// Where this source was written last time, if anywhere.
    pub fn destination_of(&self, source: &FileId) -> Option<&Path> {
        self.outputs.get(&source.key()).map(PathBuf::as_path)
    }

    /// Which source wrote this file, if it is one we know about.
    pub fn source_of(&self, output: &Path) -> Option<&str> {
        self.outputs
            .iter()
            .find(|(_, written)| written.as_path() == output)
            .map(|(source, _)| source.as_str())
    }

    pub fn record(&mut self, source: &FileId, output: &Path) {
        self.outputs.insert(source.key(), output.to_path_buf());
    }

    /// Drop entries whose file has gone.
    ///
    /// Called before a save, so a folder the user emptied does not leave the
    /// record growing for the life of the install.
    pub fn prune(&mut self) {
        self.outputs.retain(|_, path| path.exists());
    }

    pub fn len(&self) -> usize {
        self.outputs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty()
    }
}

/// Where a source's transcript should go, and whether that needed a number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub path: PathBuf,
    /// `Some(n)` when the plain name was taken by another source.
    pub serial: Option<u32>,
}

/// Choose the file for `source` inside `dir`.
///
/// `stem` and `extension` are passed separately rather than taken from a path,
/// because the two callers mean different things by them: the command line
/// derives the stem from the input and the extension from the format, and the
/// window does the same from a save-dialog suggestion.
pub fn destination(
    dir: &Path,
    stem: &std::ffi::OsStr,
    extension: &str,
    source: &FileId,
    ownership: &Ownership,
) -> Choice {
    // Already written here: replace it. Checking the directory too, because a
    // run with a different output directory has not "already" written anything
    // *here*, and reusing the old path would write outside the requested one.
    if let Some(previous) = ownership.destination_of(source) {
        if previous.parent() == Some(dir) {
            return Choice {
                path: previous.to_path_buf(),
                serial: serial_of(stem, previous),
            };
        }
    }

    let plain = dir.join(named(stem, extension, None));
    if is_free(&plain, source, ownership) {
        return Choice {
            path: plain,
            serial: None,
        };
    }

    // The name is taken by somebody else, or by a file we have no record of.
    // Numbers are not unbounded: past a few hundred we stop looking and say so
    // rather than spin.
    for n in 2..=MAX_SERIAL {
        let candidate = dir.join(named(stem, extension, Some(n)));
        if is_free(&candidate, source, ownership) {
            return Choice {
                path: candidate,
                serial: Some(n),
            };
        }
    }

    // Falling back to the plain name here would overwrite a foreign file, which
    // is the one thing this function exists to prevent. The caller is told
    // instead.
    Choice {
        path: plain,
        serial: Some(MAX_SERIAL + 1),
    }
}

/// How many numbered variations to try before giving up.
const MAX_SERIAL: u32 = 999;

/// Whether a path can be written without displacing anything but this source.
///
/// The record is consulted **before** the filesystem, and that order is the
/// whole point. A batch resolves every name before anything is written, so
/// asking the filesystem alone would say "nothing is there" for all of them and
/// hand two inputs — `2024/会议.m4a` and `2025/会议.m4a` — the same filename.
/// A name already claimed by another source is taken whether or not a file has
/// appeared at it yet; a name nobody has claimed is taken only if something is
/// actually there.
fn is_free(path: &Path, source: &FileId, ownership: &Ownership) -> bool {
    match ownership.source_of(path) {
        Some(owner) => owner == source.key(),
        None => !path.exists(),
    }
}

/// Build a filename, optionally numbered.
///
/// Assembled as an `OsString` rather than by formatting a `String`, because a
/// stem can be any sequence the filesystem accepts and round-tripping it
/// through UTF-8 would mangle a name that is not valid Unicode.
fn named(stem: &std::ffi::OsStr, extension: &str, serial: Option<u32>) -> OsString {
    let mut name = OsString::from(stem);
    if let Some(n) = serial {
        name.push(format!(" ({n})"));
    }
    name.push(".");
    name.push(extension);
    name
}

/// Which number a previously recorded path carries, if any.
///
/// Derived from the path rather than remembered, so the record stays a plain
/// map and cannot disagree with the filesystem about what a file is called.
fn serial_of(stem: &std::ffi::OsStr, path: &Path) -> Option<u32> {
    // The extension comes off first. Looking for a closing bracket on the whole
    // filename finds `.srt` instead, and the function then reports every
    // numbered file as unnumbered.
    let name = path.file_stem()?.to_string_lossy().into_owned();
    let stem = stem.to_string_lossy();

    let rest = name.strip_prefix(stem.as_ref())?.strip_prefix(" (")?;
    rest.strip_suffix(')')?.parse().ok()
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

    fn file(dir: &Path, name: &str) -> FileId {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&path, b"audio").expect("write input");
        FileId::of(&path).expect("stat")
    }

    #[test]
    fn the_first_write_takes_the_plain_name() {
        let dir = scratch("output-first");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("out dir");
        let source = file(&dir, "会议.m4a");

        let choice = destination(&out, "会议".as_ref(), "srt", &source, &Ownership::default());
        assert_eq!(choice.path, out.join("会议.srt"));
        assert_eq!(choice.serial, None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_same_source_overwrites_idempotently() {
        // The whole point: running twice leaves one file.
        let dir = scratch("output-idempotent");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("out dir");
        let source = file(&dir, "会议.m4a");

        let mut ownership = Ownership::default();
        let first = destination(&out, "会议".as_ref(), "srt", &source, &ownership);
        std::fs::write(&first.path, "first").expect("write transcript");
        ownership.record(&source, &first.path);

        let second = destination(&out, "会议".as_ref(), "srt", &source, &ownership);
        assert_eq!(second.path, first.path);
        assert_eq!(second.serial, None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_different_source_gets_a_serial_suffix() {
        let dir = scratch("output-serial");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("out dir");

        let first_source = file(&dir, "one/会议.m4a");
        let second_source = file(&dir, "two/会议.m4a");

        let mut ownership = Ownership::default();
        let first = destination(&out, "会议".as_ref(), "srt", &first_source, &ownership);
        std::fs::write(&first.path, "first").expect("write");
        ownership.record(&first_source, &first.path);

        let second = destination(&out, "会议".as_ref(), "srt", &second_source, &ownership);
        assert_eq!(second.path, out.join("会议 (2).srt"));
        assert_eq!(second.serial, Some(2));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_sources_are_numbered_even_before_anything_is_written() {
        // A batch resolves every name before it writes any of them, so a check
        // that only asks the filesystem says "free" to all of them and hands
        // two recordings the same filename. The record has to be what decides.
        let dir = scratch("output-planned");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("out dir");

        let first_source = file(&dir, "2024/会议.m4a");
        let second_source = file(&dir, "2025/会议.m4a");
        let mut ownership = Ownership::default();

        let first = destination(&out, "会议".as_ref(), "srt", &first_source, &ownership);
        ownership.record(&first_source, &first.path);
        let second = destination(&out, "会议".as_ref(), "srt", &second_source, &ownership);
        ownership.record(&second_source, &second.path);

        assert_eq!(first.path, out.join("会议.srt"));
        assert_eq!(
            second.path,
            out.join("会议 (2).srt"),
            "nothing has been written yet, and the second name is still taken"
        );
        assert_eq!(std::fs::read_dir(&out).expect("list").count(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_second_source_keeps_its_number_across_runs() {
        // Numbering that moved each run would fill the folder with copies.
        let dir = scratch("output-serial-stable");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("out dir");

        let first_source = file(&dir, "one/会议.m4a");
        let second_source = file(&dir, "two/会议.m4a");

        let mut ownership = Ownership::default();
        let first = destination(&out, "会议".as_ref(), "srt", &first_source, &ownership);
        std::fs::write(&first.path, "first").expect("write");
        ownership.record(&first_source, &first.path);

        let second = destination(&out, "会议".as_ref(), "srt", &second_source, &ownership);
        std::fs::write(&second.path, "second").expect("write");
        ownership.record(&second_source, &second.path);

        // Run again, in the order the command line would.
        let first_again = destination(&out, "会议".as_ref(), "srt", &first_source, &ownership);
        let second_again = destination(&out, "会议".as_ref(), "srt", &second_source, &ownership);

        assert_eq!(first_again.path, first.path);
        assert_eq!(second_again.path, second.path);
        assert_eq!(std::fs::read_dir(&out).expect("list").count(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_with_no_record_is_never_overwritten() {
        // Somebody else's transcript, or one from before the record existed.
        let dir = scratch("output-foreign");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("out dir");
        std::fs::write(out.join("会议.srt"), "somebody else's work").expect("plant file");

        let source = file(&dir, "会议.m4a");
        let choice = destination(&out, "会议".as_ref(), "srt", &source, &Ownership::default());

        assert_eq!(choice.path, out.join("会议 (2).srt"));
        assert_eq!(
            std::fs::read_to_string(out.join("会议.srt")).expect("read"),
            "somebody else's work",
            "the foreign file must be untouched"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_record_is_an_empty_record_not_an_error() {
        // Losing the record costs a numbered filename; it must not cost a
        // transcription.
        let dir = scratch("output-corrupt");
        let record = dir.join("outputs.json");
        std::fs::write(&record, "{ this is not json").expect("plant");

        assert!(Ownership::load(&record).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_record_from_a_future_version_is_ignored() {
        let dir = scratch("output-version");
        let record = dir.join("outputs.json");
        std::fs::write(
            &record,
            r#"{"version":99,"outputs":{"/a|1|2":"/out/a.srt"}}"#,
        )
        .expect("plant");

        assert!(Ownership::load(&record).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_record_survives_a_round_trip() {
        let dir = scratch("output-roundtrip");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("out dir");
        let source = file(&dir, "会议.m4a");

        let mut ownership = Ownership::default();
        let choice = destination(&out, "会议".as_ref(), "srt", &source, &ownership);
        std::fs::write(&choice.path, "text").expect("write");
        ownership.record(&source, &choice.path);
        ownership.save(&dir.join("outputs.json")).expect("save");

        let reloaded = Ownership::load(&dir.join("outputs.json"));
        assert_eq!(
            reloaded.destination_of(&source),
            Some(choice.path.as_path())
        );
        assert_eq!(
            reloaded.source_of(&choice.path),
            Some(source.key().as_str())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pruning_drops_only_files_that_have_gone() {
        let dir = scratch("output-prune");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("out dir");
        let kept = file(&dir, "kept.m4a");
        let dropped = file(&dir, "dropped.m4a");

        let mut ownership = Ownership::default();
        let kept_at = destination(&out, "kept".as_ref(), "srt", &kept, &ownership);
        let dropped_at = destination(&out, "dropped".as_ref(), "srt", &dropped, &ownership);
        std::fs::write(&kept_at.path, "x").expect("write");
        ownership.record(&kept, &kept_at.path);
        ownership.record(&dropped, &dropped_at.path);

        ownership.prune();

        assert_eq!(ownership.len(), 1);
        assert!(ownership.destination_of(&kept).is_some());
        assert!(ownership.destination_of(&dropped).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_output_directory_is_not_confused_with_the_old_one() {
        // Reusing the recorded path would write outside the directory the
        // caller asked for.
        let dir = scratch("output-moved");
        let first_out = dir.join("a");
        let second_out = dir.join("b");
        std::fs::create_dir_all(&first_out).expect("a");
        std::fs::create_dir_all(&second_out).expect("b");
        let source = file(&dir, "会议.m4a");

        let mut ownership = Ownership::default();
        let first = destination(&first_out, "会议".as_ref(), "srt", &source, &ownership);
        std::fs::write(&first.path, "x").expect("write");
        ownership.record(&source, &first.path);

        let moved = destination(&second_out, "会议".as_ref(), "srt", &source, &ownership);
        assert_eq!(moved.path, second_out.join("会议.srt"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_claim_survives_a_save_and_a_reload_without_the_file_existing() {
        // The order the command line needs: record, save, and only then write.
        // A caller that prunes between the record and the save erases the claim
        // it just made, because no transcript has been written yet — and then
        // every run looks like a first run and the output directory fills with
        // numbered copies. This pins the property, not the order, so the store
        // stays usable from a caller that writes later.
        let dir = scratch("output-claim-survives");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("out dir");
        let source = file(&dir, "会议.m4a");

        let mut ownership = Ownership::default();
        let choice = destination(&out, "会议".as_ref(), "srt", &source, &ownership);
        ownership.record(&source, &choice.path);
        ownership.save(&dir.join("outputs.json")).expect("save");

        // Nothing was written, and a reload still knows where it belongs.
        assert!(!choice.path.exists());
        let reloaded = Ownership::load(&dir.join("outputs.json"));
        let again = destination(&out, "会议".as_ref(), "srt", &source, &reloaded);

        assert_eq!(again.path, choice.path);
        assert_eq!(again.serial, None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reused_numbered_name_still_reports_its_number() {
        // The number is read back off the filename rather than remembered, so
        // the record cannot disagree with the filesystem about what a file is
        // called. Parsing it wrong is invisible except in what the caller
        // reports, which is exactly the kind of bug that survives.
        let dir = scratch("output-serial-readback");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("out dir");

        let first_source = file(&dir, "one/会议.m4a");
        let second_source = file(&dir, "two/会议.m4a");

        let mut ownership = Ownership::default();
        let first = destination(&out, "会议".as_ref(), "srt", &first_source, &ownership);
        std::fs::write(&first.path, "x").expect("write");
        ownership.record(&first_source, &first.path);

        let second = destination(&out, "会议".as_ref(), "srt", &second_source, &ownership);
        std::fs::write(&second.path, "y").expect("write");
        ownership.record(&second_source, &second.path);

        // Reused from the record, and still known to be number 2.
        let again = destination(&out, "会议".as_ref(), "srt", &second_source, &ownership);
        assert_eq!(again.path, out.join("会议 (2).srt"));
        assert_eq!(
            again.serial,
            Some(2),
            "the number must survive the round trip"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stem_that_is_not_unicode_still_produces_a_name() {
        let dir = scratch("output-nonunicode");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("out dir");
        let source = file(&dir, "会议.m4a");

        #[cfg(unix)]
        let stem = {
            use std::os::unix::ffi::OsStrExt;
            std::ffi::OsStr::from_bytes(b"\xff\xfe bad name")
        };
        #[cfg(windows)]
        let stem = std::ffi::OsStr::new("会议");

        let choice = destination(&out, stem, "srt", &source, &Ownership::default());
        assert!(choice.path.parent() == Some(out.as_path()));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
