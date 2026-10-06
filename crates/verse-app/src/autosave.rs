//! Writing a finished transcript out without being asked.
//!
//! The window had no default output location at all: the user picked one every
//! time, and a transcript that was never exported lived only in memory and was
//! destroyed the moment the next file arrived. Dropping a two-hour recording,
//! waiting, and then dropping another one lost the first result completely.
//!
//! So the result is written as soon as it exists, and the save dialog becomes
//! *save as* rather than the only way to keep anything.
//!
//! The decision of **which file** is `verse_store`'s, shared with the command
//! line and tested there. What is here is the part that is the window's own:
//! reading the environment, rendering, and turning a failure into a sentence
//! the interface can show.

use std::path::{Path, PathBuf};

use verse_store::{FileId, Ownership};

/// Where a transcript was written, and what to tell the user if it was not.
#[derive(Debug)]
pub enum Saved {
    Written(PathBuf),
    /// Nothing was written, and this is why. Shown rather than swallowed: the
    /// transcript is still on screen and *save as* still works, so the failure
    /// costs a step rather than the work, but only if it is visible.
    Refused(String),
}

/// Write `rendered` for `input` into `dir`, remembering where it went.
///
/// Both directories are passed in rather than read from the environment, so a
/// test can describe a machine — including a Documents folder that cannot be
/// written to, which is the case that matters and the one that is impossible to
/// arrange on the machine running the tests.
pub fn save_into(dir: &Path, record: &Path, input: &Path, rendered: &str) -> Saved {
    let mut ownership = Ownership::load(record);
    ownership.prune();

    let stem = input.file_stem().unwrap_or_default().to_os_string();

    // Without an identity there is no way to know whether a file already here
    // is this recording's or somebody else's, and guessing wrong means
    // replacing work that cannot be recovered. A plain name is the safe answer.
    let destination = match FileId::of(input) {
        Ok(source) => {
            let choice = verse_store::destination(dir, &stem, "srt", &source, &ownership);
            ownership.record(&source, &choice.path);
            choice.path
        }
        Err(e) => {
            return Saved::Refused(format!("读不了 {}：{e}", input.display()));
        }
    };

    if let Some(parent) = destination.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Saved::Refused(format!("建不了 {}：{e}", parent.display()));
        }
    }

    if let Err(e) = std::fs::write(&destination, rendered) {
        return Saved::Refused(format!("写不进 {}：{e}", destination.display()));
    }

    // After the write, so a name is only remembered once a file is really
    // there. Failing to remember it costs a numbered filename next time, which
    // is not worth telling anyone about.
    let _ = ownership.save(record);

    Saved::Written(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("verse-app-test-autosave-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn audio(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&path, b"pretend audio").expect("write");
        path
    }

    #[test]
    fn a_finished_transcript_is_written_without_being_asked() {
        let dir = scratch("written");
        let out = dir.join("Documents/Verse");
        let record = dir.join("outputs.json");
        let input = audio(&dir, "会议.m4a");

        let saved = save_into(&out, &record, &input, "transcript text");

        match saved {
            Saved::Written(path) => {
                assert_eq!(path, out.join("会议.srt"));
                assert_eq!(std::fs::read_to_string(&path).expect("read"), "transcript text");
            }
            Saved::Refused(why) => panic!("should have been written: {why}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_output_directory_is_made_if_it_is_not_there() {
        // First run on a fresh machine: nothing has created it yet.
        let dir = scratch("mkdir");
        let out = dir.join("Documents/Verse");
        let input = audio(&dir, "a.m4a");

        assert!(!out.exists());
        let saved = save_into(&out, &dir.join("outputs.json"), &input, "text");

        assert!(matches!(saved, Saved::Written(_)));
        assert!(out.is_dir());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writing_the_same_recording_twice_replaces_rather_than_accumulates() {
        let dir = scratch("idempotent");
        let out = dir.join("Documents/Verse");
        let record = dir.join("outputs.json");
        let input = audio(&dir, "会议.m4a");

        save_into(&out, &record, &input, "first");
        save_into(&out, &record, &input, "second");

        assert_eq!(std::fs::read_to_string(out.join("会议.srt")).expect("read"), "second");
        assert_eq!(
            std::fs::read_dir(&out).expect("list").count(),
            1,
            "one recording, one file"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_different_recording_with_the_same_name_does_not_replace_it() {
        let dir = scratch("collision");
        let out = dir.join("Documents/Verse");
        let record = dir.join("outputs.json");
        let first = audio(&dir, "january/会议.m4a");
        let second = audio(&dir, "february/会议.m4a");

        save_into(&out, &record, &first, "january");
        let saved = save_into(&out, &record, &second, "february");

        assert!(matches!(saved, Saved::Written(_)));
        assert_eq!(std::fs::read_to_string(out.join("会议.srt")).expect("read"), "january");
        assert_eq!(
            std::fs::read_to_string(out.join("会议 (2).srt")).expect("read"),
            "february"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unwritable_directory_is_reported_rather_than_swallowed() {
        // The case that is impossible to arrange by accident on a machine you
        // are allowed to write to, and the one where staying quiet would lose
        // the result without saying so.
        let dir = scratch("unwritable");
        let input = audio(&dir, "a.m4a");

        // A *file* where the directory should be: creating the parent fails.
        let blocked = dir.join("Documents");
        std::fs::write(&blocked, b"not a directory").expect("write");

        let saved = save_into(&blocked.join("Verse"), &dir.join("outputs.json"), &input, "text");

        match saved {
            Saved::Refused(why) => assert!(why.contains("Documents"), "got: {why}"),
            Saved::Written(path) => panic!("should not have written {}", path.display()),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_input_that_cannot_be_read_is_reported() {
        let dir = scratch("missing-input");
        let out = dir.join("out");

        let saved = save_into(&out, &dir.join("outputs.json"), &dir.join("gone.m4a"), "text");

        match saved {
            Saved::Refused(why) => assert!(why.contains("gone.m4a"), "got: {why}"),
            Saved::Written(path) => panic!("should not have written {}", path.display()),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_record_file_that_cannot_be_written_still_leaves_the_transcript() {
        // Losing the record costs a numbered filename next time; losing the
        // transcript costs the work. The second must not follow from the first.
        let dir = scratch("unwritable-record");
        let out = dir.join("Documents/Verse");
        let input = audio(&dir, "a.m4a");

        let saved = save_into(&out, &dir.join("no-such-dir/nested/outputs.json"), &input, "text");

        assert!(
            matches!(saved, Saved::Written(_)),
            "the transcript is what matters"
        );
        assert!(out.join("a.srt").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
