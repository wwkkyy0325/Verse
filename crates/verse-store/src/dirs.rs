//! Where Verse keeps things on this machine.
//!
//! Three locations, and they are not the same location:
//!
//! - **Output** is user-facing. A transcript is something a person will go
//!   looking for, so it goes where their documents are.
//! - **Data** is ours. The cache, the resume checkpoints and the output
//!   ownership record live beside the application's other local state, which is
//!   also where they belong when the user's Documents folder is backed up or
//!   synchronised.
//! - **Override** is either. Both have an environment variable, because a
//!   caller that cannot choose where output goes cannot be scripted.
//!
//! The resolution is a pure function of [`Roots`] rather than of the machine.
//! That is deliberate: every fallback in the chain is a branch that only runs
//! on somebody else's computer, and a branch nobody can run is a branch nobody
//! has tested.

use std::path::{Path, PathBuf};

/// The folder name, in both places.
pub const FOLDER: &str = "Verse";

/// The inputs the resolution is a function of.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roots {
    /// `VERSE_OUTPUT`, which wins over everything.
    pub output: Option<PathBuf>,
    /// `VERSE_CACHE`, which wins over everything.
    pub cache: Option<PathBuf>,
    /// The shell's Documents folder — the real Known Folder on Windows, not
    /// `%USERPROFILE%\Documents` guessed at.
    pub documents: Option<PathBuf>,
    pub home: Option<PathBuf>,
    /// `%LOCALAPPDATA%` on Windows, `$XDG_DATA_HOME` or `~/.local/share`
    /// elsewhere.
    pub local_app_data: Option<PathBuf>,
    /// `OneDrive`, when the variable is set. Only used to warn.
    pub onedrive: Option<PathBuf>,
}

impl Roots {
    /// Read the environment, once.
    pub fn from_env() -> Self {
        Self {
            output: env_path("VERSE_OUTPUT"),
            cache: env_path("VERSE_CACHE"),
            documents: dirs::document_dir(),
            home: dirs::home_dir(),
            local_app_data: dirs::data_local_dir(),
            onedrive: env_path("OneDrive"),
        }
    }
}

/// An environment variable as a path, treating empty as unset.
///
/// An empty `VERSE_OUTPUT` means "I did not set this", not "write to the
/// current directory" — the second reading would scatter transcripts into
/// whatever directory the process happened to start in.
fn env_path(name: &str) -> Option<PathBuf> {
    let value = std::env::var_os(name)?;
    if value.is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

/// Where finished transcripts go.
///
/// The fallback chain matters more than the first entry: a Documents folder can
/// be missing on a stripped-down profile, and local app data can be missing on
/// a roaming one. The last resort is a relative path, matching what `models`
/// does, because returning nothing would mean the caller has to invent a rule —
/// and two callers inventing it separately is how the CLI and the window end up
/// disagreeing about where a file went.
pub fn output_dir(roots: &Roots) -> PathBuf {
    if let Some(dir) = &roots.output {
        return dir.clone();
    }
    if let Some(documents) = &roots.documents {
        return documents.join(FOLDER);
    }
    if let Some(home) = &roots.home {
        return home.join("Documents").join(FOLDER);
    }
    if let Some(local) = &roots.local_app_data {
        return local.join(FOLDER).join("output");
    }
    PathBuf::from(FOLDER)
}

/// Where the cache, the resume checkpoints and the ownership record go.
pub fn data_dir(roots: &Roots) -> PathBuf {
    if let Some(dir) = &roots.cache {
        return dir.clone();
    }
    if let Some(local) = &roots.local_app_data {
        return local.join(FOLDER);
    }
    if let Some(home) = &roots.home {
        return home.join(".verse");
    }
    PathBuf::from("verse-data")
}

/// Anything the user should be told about where their files are going.
///
/// One case, and it is not an error. When Documents has been redirected into
/// OneDrive — "Known Folder Move", which Windows offers and most people accept
/// — then transcripts land in a folder that a background service uploads. Verse
/// itself still makes no network call, but something on the machine will, and
/// that is a fact about the user's own setup that they are entitled to hear
/// rather than deduce.
pub fn notices(roots: &Roots) -> Vec<String> {
    let mut notices = Vec::new();

    // An explicit override is an instruction, not an accident. Saying anything
    // about it would be second-guessing the caller.
    if roots.output.is_some() {
        return notices;
    }

    if let (Some(onedrive), Some(documents)) = (&roots.onedrive, &roots.documents) {
        if !onedrive.as_os_str().is_empty() && is_inside(documents, onedrive) {
            notices.push(format!(
                "转写结果会保存到 {}，这个文件夹在 OneDrive 里，会被自动上传到云端。\
                 程序本身不联网；如果不想同步，设置 VERSE_OUTPUT 指向别处。",
                output_dir(roots).display()
            ));
        }
    }

    notices
}

/// Whether `path` is `root` or lives under it.
///
/// Compares components rather than string prefixes, so `/a/bc` is not counted
/// as being inside `/a/b`. Neither path is canonicalised, because either may be
/// the one that does not exist yet.
fn is_inside(path: &Path, root: &Path) -> bool {
    let mut path = path.components();
    root.components().all(|part| path.next() == Some(part))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(documents: Option<&str>, home: Option<&str>, local: Option<&str>) -> Roots {
        Roots {
            documents: documents.map(PathBuf::from),
            home: home.map(PathBuf::from),
            local_app_data: local.map(PathBuf::from),
            ..Roots::default()
        }
    }

    #[test]
    fn the_default_output_is_documents_verse() {
        let r = roots(Some("/home/me/Documents"), Some("/home/me"), None);
        assert_eq!(output_dir(&r), PathBuf::from("/home/me/Documents/Verse"));
    }

    #[test]
    fn an_override_beats_documents() {
        let r = Roots {
            output: Some(PathBuf::from("/tmp/out")),
            ..roots(Some("/home/me/Documents"), Some("/home/me"), None)
        };
        assert_eq!(output_dir(&r), PathBuf::from("/tmp/out"));
    }

    #[test]
    fn a_missing_documents_folder_falls_back_to_home() {
        // A stripped-down or roaming profile: the shell has no Documents.
        let r = roots(None, Some("/home/me"), None);
        assert_eq!(output_dir(&r), PathBuf::from("/home/me/Documents/Verse"));
    }

    #[test]
    fn with_nothing_at_all_the_output_is_relative() {
        // Never ideal, but it fails visibly beside the binary rather than
        // silently somewhere unpredictable.
        let r = Roots::default();
        assert_eq!(output_dir(&r), PathBuf::from(FOLDER));
    }

    #[test]
    fn data_and_output_are_different_places() {
        // The cache must not land in a folder the user syncs or opens.
        let r = roots(Some("/home/me/Documents"), Some("/home/me"), Some("/home/me/.local/share"));
        assert_eq!(output_dir(&r), PathBuf::from("/home/me/Documents/Verse"));
        assert_eq!(data_dir(&r), PathBuf::from("/home/me/.local/share/Verse"));
        assert!(!is_inside(&data_dir(&r), Path::new("/home/me/Documents")));
    }

    #[test]
    fn a_redirected_documents_folder_is_reported_once() {
        let r = Roots {
            onedrive: Some(PathBuf::from("/home/me/OneDrive")),
            ..roots(Some("/home/me/OneDrive/Documents"), Some("/home/me"), None)
        };

        let notices = notices(&r);
        assert_eq!(notices.len(), 1, "one notice, not a stream of them");
        assert!(notices[0].contains("OneDrive"));
        assert!(
            notices[0].contains("VERSE_OUTPUT"),
            "a notice the user cannot act on is noise"
        );
    }

    #[test]
    fn a_documents_folder_outside_onedrive_says_nothing() {
        let r = Roots {
            onedrive: Some(PathBuf::from("/home/me/OneDrive")),
            ..roots(Some("/home/me/Documents"), Some("/home/me"), None)
        };
        assert!(notices(&r).is_empty());
    }

    #[test]
    fn an_explicit_override_is_not_second_guessed() {
        // Told where to write, we do not then remark on it.
        let r = Roots {
            output: Some(PathBuf::from("/home/me/OneDrive/Documents/Verse")),
            onedrive: Some(PathBuf::from("/home/me/OneDrive")),
            ..Roots::default()
        };
        assert!(notices(&r).is_empty());
    }

    #[test]
    fn being_inside_is_compared_component_wise() {
        // `/a/bc` is not inside `/a/b`, which a string prefix check would miss.
        assert!(is_inside(Path::new("/a/b/c"), Path::new("/a/b")));
        assert!(is_inside(Path::new("/a/b"), Path::new("/a/b")));
        assert!(!is_inside(Path::new("/a/bc"), Path::new("/a/b")));
        assert!(!is_inside(Path::new("/a"), Path::new("/a/b")));
    }
}
