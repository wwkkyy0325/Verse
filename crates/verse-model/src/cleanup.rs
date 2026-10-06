//! Removing what is no longer wanted, and saying how much it was.
//!
//! Nothing here runs on its own. A model directory is the largest thing this
//! program puts on a disk — 228 MB for the default, 942 MB for Qwen3 — and
//! deleting one because some heuristic decided it was unused would be a
//! destructive act taken on the user's behalf. Every function in this module
//! is reached only because someone asked.
//!
//! That includes the `.part` files. [`Downloader`](crate::Downloader) leaves
//! them behind deliberately when a transfer fails or is cancelled, because the
//! next attempt resumes from them; they are litter only once the download is
//! abandoned, and nothing here can tell when that has happened. So they are
//! reported, and removed when asked for.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// A half-finished download, sitting beside the file it is becoming.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Partial {
    /// The file it is becoming, by path on disk.
    pub path: PathBuf,
    pub bytes: u64,
}

/// What a directory occupies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub bytes: u64,
    pub files: usize,
}

/// How much a model's directory occupies, or nothing if it is not installed.
pub fn usage(root: &Path, id: &str) -> Usage {
    let dir = root.join(id);
    if !dir.is_dir() {
        return Usage::default();
    }
    measure(&dir)
}

/// Add up everything under a directory.
pub fn measure(dir: &Path) -> Usage {
    let mut usage = Usage::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return usage;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        // `symlink_metadata`, so a link is measured as a link rather than as
        // whatever it points at — which might be somewhere else entirely.
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };

        if metadata.is_dir() {
            let nested = measure(&path);
            usage.bytes += nested.bytes;
            usage.files += nested.files;
        } else {
            usage.bytes += metadata.len();
            usage.files += 1;
        }
    }

    usage
}

/// Every half-finished download under a models root, largest first.
///
/// Largest first because a caller listing them is usually looking at where the
/// disk went, and a 900 MB orphan matters more than a 12 KB one.
pub fn partials(root: &Path) -> Vec<Partial> {
    let mut found = Vec::new();
    collect_partials(root, &mut found);
    found.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    found
}

fn collect_partials(dir: &Path, out: &mut Vec<Partial>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };

        if metadata.is_dir() {
            collect_partials(&path, out);
        } else if is_partial(&path) {
            out.push(Partial {
                path,
                bytes: metadata.len(),
            });
        }
    }
}

/// Whether a file is a download in progress.
///
/// Recognised by the extension the downloader gives them, and nothing else: a
/// `.onnx` file is a model whatever else is true of it.
fn is_partial(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("part"))
}

/// Remove every half-finished download under a models root, and say how much
/// that freed.
pub fn remove_partials(root: &Path) -> std::io::Result<Usage> {
    let mut removed = Usage::default();
    for partial in partials(root) {
        if std::fs::remove_file(&partial.path).is_ok() {
            removed.bytes += partial.bytes;
            removed.files += 1;
        }
    }
    Ok(removed)
}

/// Remove an installed model, and say how much that freed.
///
/// The `id` is a directory name and not a path. Anything that could climb out
/// of the models root is refused before the filesystem is consulted, and the
/// resolved directory is then checked to be inside the root as well — the first
/// check covers what a caller passes, the second covers what the filesystem
/// does with it.
pub fn remove_model(root: &Path, id: &str) -> std::io::Result<u64> {
    if id.is_empty()
        || id == "."
        || id == ".."
        || id.contains('/')
        || id.contains('\\')
        || Path::new(id).components().count() != 1
    {
        return Err(invalid(format!("{id:?} is not a model id")));
    }

    let target = root.join(id);
    if !target.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{} is not installed", target.display()),
        ));
    }

    let canonical_root = std::fs::canonicalize(root)?;
    let canonical_target = std::fs::canonicalize(&target)?;

    // A model directory that is a link would otherwise take its target with it,
    // and the target is not ours to remove.
    if canonical_target == canonical_root || !canonical_target.starts_with(&canonical_root) {
        return Err(invalid(format!(
            "{} resolves outside the models directory",
            target.display()
        )));
    }

    let freed = measure(&target).bytes;
    std::fs::remove_dir_all(&canonical_target)?;
    Ok(freed)
}

fn invalid(message: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("verse-model-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn plant(path: &Path, bytes: usize) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, vec![0u8; bytes]).expect("write");
    }

    #[test]
    fn a_partial_is_found_and_a_completed_file_is_not() {
        let dir = scratch("cleanup-partials");
        plant(&dir.join("sensevoice/model.int8.onnx"), 100);
        plant(&dir.join("sensevoice/model.int8.onnx.part"), 40);
        plant(&dir.join("qwen3-asr/tokenizer/vocab.json.part"), 25);

        let found = partials(&dir);
        let names: Vec<String> = found
            .iter()
            .map(|p| p.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            names,
            ["model.int8.onnx.part", "vocab.json.part"],
            "largest first, and the finished model is not a partial"
        );
        assert_eq!(found[0].bytes, 40);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_model_named_with_part_in_it_is_not_a_partial() {
        // The check is the extension, not a substring. A model called
        // `part-of-speech.onnx` is a model.
        let dir = scratch("cleanup-part-substring");
        plant(&dir.join("model/part-of-speech.onnx"), 10);
        plant(&dir.join("model/data.part"), 10);

        let found = partials(&dir);
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("data.part"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_partials_leaves_the_models_alone() {
        let dir = scratch("cleanup-remove-partials");
        plant(&dir.join("sensevoice/model.int8.onnx"), 100);
        plant(&dir.join("sensevoice/model.int8.onnx.part"), 40);

        let removed = remove_partials(&dir).expect("remove");

        assert_eq!(removed.files, 1);
        assert_eq!(removed.bytes, 40);
        assert!(dir.join("sensevoice/model.int8.onnx").exists());
        assert!(!dir.join("sensevoice/model.int8.onnx.part").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn usage_adds_up_a_nested_directory() {
        let dir = scratch("cleanup-usage");
        plant(&dir.join("model/a.onnx"), 100);
        plant(&dir.join("model/tokenizer/vocab.json"), 25);

        let usage = usage(&dir, "model");
        assert_eq!(usage.bytes, 125);
        assert_eq!(usage.files, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_absent_model_occupies_nothing_rather_than_failing() {
        // Asking how big something that is not there is must not be an error,
        // or every caller needs a branch for the common case.
        let dir = scratch("cleanup-absent");
        assert_eq!(usage(&dir, "sensevoice"), Usage::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_a_model_reports_what_it_freed() {
        let dir = scratch("cleanup-remove");
        plant(&dir.join("sensevoice/model.int8.onnx"), 200);
        plant(&dir.join("sensevoice/tokens.txt"), 50);
        plant(&dir.join("qwen3-asr/encoder.onnx"), 10);

        let freed = remove_model(&dir, "sensevoice").expect("remove");

        assert_eq!(freed, 250);
        assert!(!dir.join("sensevoice").exists());
        assert!(dir.join("qwen3-asr").exists(), "only the one asked for");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_something_that_is_not_installed_says_so() {
        let dir = scratch("cleanup-remove-absent");
        let error = remove_model(&dir, "sensevoice").expect_err("should refuse");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_id_that_is_a_path_is_refused() {
        // The check that matters most: `../../something` must not reach the
        // filesystem at all.
        let dir = scratch("cleanup-escape");
        plant(&dir.join("outside/important.txt"), 10);
        plant(&dir.join("models/sensevoice/model.onnx"), 10);

        for id in [
            "..",
            ".",
            "",
            "../outside",
            "..\\outside",
            "models/../../outside",
            "models/nested",
        ] {
            let error = remove_model(&dir.join("models"), id)
                .expect_err(&format!("{id:?} must be refused"));
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{id:?}");
        }

        assert!(dir.join("outside/important.txt").exists(), "nothing was touched");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_models_root_itself_is_never_removed() {
        // The one deletion that would take everything with it.
        let dir = scratch("cleanup-root");
        let root = dir.join("models");
        plant(&root.join("sensevoice/model.onnx"), 10);

        // An empty id is refused above; `.` resolves to the root itself, which
        // the containment check then catches even though it "starts with" it.
        let error = remove_model(&root, ".").expect_err("should refuse");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(root.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
