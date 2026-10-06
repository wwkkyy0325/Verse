//! Fetching model files, with mirror failover and resume.
//!
//! Nothing here runs on its own. A download happens because someone called
//! [`Downloader::fetch`] — no startup fetch, no background refresh.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use verse_core::{CancelToken, Error, ErrorKind, Result};

use crate::catalog::{Mirror, ModelFile, ModelSpec};
use crate::state::DownloadState;

/// Wait this long to establish a connection before trying another mirror.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Wait this long for the response headers. A mirror that accepts a connection
/// and then says nothing is the failure this catches.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// Body read size. Large enough that a 240 MB model is a few thousand reads,
/// small enough that a cancelled download stops promptly.
const CHUNK: usize = 64 * 1024;

/// Smallest gap between progress reports.
///
/// Without this the callback fires for every 64 KB above, which is thousands
/// of calls for a model and a wall of output for any caller that prints them.
/// A percentage of the total is used when the size is known; this is the floor
/// for small files and the fallback when it is not.
const PROGRESS_STEP: u64 = 2 * 1024 * 1024;

/// Fetches model files.
pub struct Downloader {
    agent: ureq::Agent,
}

impl Default for Downloader {
    fn default() -> Self {
        Self::new()
    }
}

impl Downloader {
    pub fn new() -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_recv_response(Some(RESPONSE_TIMEOUT))
            .build()
            .into();

        Self { agent }
    }

    /// Where `spec`'s files live, given a models root.
    pub fn directory_for(spec: &ModelSpec, root: &Path) -> PathBuf {
        root.join(&spec.id)
    }

    /// Whether every file of `spec` is present and the expected size.
    ///
    /// Cheap enough to call before offering a download, so an already-complete
    /// model is never fetched twice.
    pub fn is_present(spec: &ModelSpec, root: &Path) -> bool {
        let dir = Self::directory_for(spec, root);
        spec.files.iter().all(|file| file_is_complete(file, &dir))
    }

    /// Fetch every missing file of `spec` under `root`.
    ///
    /// Files already present at the right size are skipped, so an interrupted
    /// run resumes rather than starting over. `on_state` is called on every
    /// transition and periodically during a transfer.
    ///
    /// Cancellation is not an error: it returns [`DownloadState::Idle`] and
    /// leaves the partial file in place for the next attempt.
    pub fn fetch(
        &self,
        spec: &ModelSpec,
        root: &Path,
        mut on_state: impl FnMut(&DownloadState),
        cancel: &CancelToken,
    ) -> Result<DownloadState> {
        let dir = Self::directory_for(spec, root);

        if let Err(e) = fs::create_dir_all(&dir) {
            return Ok(self.fail(format!("could not create {}: {e}", dir.display())));
        }

        on_state(&DownloadState::Idle);

        for file in &spec.files {
            if file_is_complete(file, &dir) {
                continue;
            }

            match self.fetch_file(spec, file, &dir, &mut on_state, cancel) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::Cancelled => {
                    on_state(&DownloadState::Idle);
                    return Ok(DownloadState::Idle);
                }
                Err(e) => return Ok(self.fail(e.message().to_string())),
            }
        }

        on_state(&DownloadState::Verifying);

        if let Some(problem) = verify(spec, &dir) {
            return Ok(self.fail(problem));
        }

        on_state(&DownloadState::Ready);
        Ok(DownloadState::Ready)
    }

    fn fail(&self, reason: String) -> DownloadState {
        DownloadState::Failed { reason }
    }

    /// Try every mirror in turn for one file.
    fn fetch_file(
        &self,
        spec: &ModelSpec,
        file: &ModelFile,
        dir: &Path,
        on_state: &mut impl FnMut(&DownloadState),
        cancel: &CancelToken,
    ) -> Result<()> {
        let mut failures = Vec::new();

        for mirror in &spec.mirrors {
            match self.try_mirror(mirror, file, dir, on_state, cancel) {
                Ok(()) => return Ok(()),
                // Cancellation is the user's decision, not a mirror problem;
                // trying the next one would override it.
                Err(e) if e.kind() == ErrorKind::Cancelled => return Err(e),
                Err(e) => failures.push(format!("{} ({})", mirror.name, e.message())),
            }
        }

        Err(Error::new(
            ErrorKind::Network,
            format!(
                "could not fetch {} from any mirror: {}",
                file.remote,
                failures.join("; ")
            ),
        ))
    }

    /// Fetch one file from one mirror, resuming if a partial copy exists.
    fn try_mirror(
        &self,
        mirror: &Mirror,
        file: &ModelFile,
        dir: &Path,
        on_state: &mut impl FnMut(&DownloadState),
        cancel: &CancelToken,
    ) -> Result<()> {
        let url = mirror.url_for(&file.remote);
        let dest = dir.join(&file.local);
        // Written under a temporary name so an interrupted transfer never
        // leaves something that looks like a complete model.
        let partial = dir.join(format!("{}.part", file.local));

        // `local` may name a subdirectory — Qwen3's tokenizer files do — and
        // the `.part` sibling has to sit beside the destination, so the
        // directory must exist before anything is written. Done here, before
        // the request, rather than alongside the write: a layout we cannot
        // create should fail immediately, not after the download that precedes
        // it. `create_dir_all` on a path that already exists is a no-op, so a
        // flat `local` costs nothing.
        if let Some(parent) = partial.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                Error::new(
                    ErrorKind::Io,
                    format!("could not create {}: {e}", parent.display()),
                )
            })?;
        }

        let resume_from = fs::metadata(&partial).map(|m| m.len()).unwrap_or(0);

        let mut request = self.agent.get(&url);
        if resume_from > 0 {
            request = request.header("Range", format!("bytes={resume_from}-"));
        }

        let report = |on_state: &mut dyn FnMut(&DownloadState), received: u64| {
            on_state(&DownloadState::Fetching {
                mirror: mirror.name.to_string(),
                file: file.local.to_string(),
                received,
                total: file.size,
            });
        };

        report(on_state, resume_from);

        let mut response = request
            .call()
            .map_err(|e| Error::new(ErrorKind::Network, format!("{e}")))?;

        let status = response.status().as_u16();
        // 206 means the range was honoured; 200 means the mirror ignored it and
        // is sending the whole file, so the partial copy must be discarded
        // rather than appended to.
        let resumed = status == 206 && resume_from > 0;
        if status != 200 && status != 206 {
            return Err(Error::new(
                ErrorKind::Network,
                format!("unexpected status {status}"),
            ));
        }

        let mut out = if resumed {
            OpenOptions::new()
                .append(true)
                .open(&partial)
                .map_err(|e| Error::new(ErrorKind::Io, format!("{e}")))?
        } else {
            fs::File::create(&partial).map_err(|e| Error::new(ErrorKind::Io, format!("{e}")))?
        };

        let mut received = if resumed { resume_from } else { 0 };
        let mut reader = response.body_mut().as_reader();
        let mut buffer = vec![0u8; CHUNK];

        // One percent of the total, but never finer than PROGRESS_STEP.
        let step = file
            .size
            .map(|total| (total / 100).max(PROGRESS_STEP))
            .unwrap_or(PROGRESS_STEP);
        let mut last_reported = received;

        loop {
            cancel.check()?;

            let read = reader
                .read(&mut buffer)
                .map_err(|e| Error::new(ErrorKind::Network, format!("{e}")))?;
            if read == 0 {
                break;
            }

            out.write_all(&buffer[..read])
                .map_err(|e| Error::new(ErrorKind::Io, format!("{e}")))?;
            received += read as u64;

            if received - last_reported >= step {
                report(on_state, received);
                last_reported = received;
            }
        }

        // Whatever the step was, the last figure the caller saw should be the
        // final one.
        report(on_state, received);

        out.sync_all()
            .map_err(|e| Error::new(ErrorKind::Io, format!("{e}")))?;
        drop(out);

        // Length is checked before the file is promoted, so a truncated
        // transfer is retried from another mirror rather than accepted.
        if let Some(expected) = file.size {
            if received != expected {
                return Err(Error::new(
                    ErrorKind::Network,
                    format!("got {received} bytes, expected {expected}"),
                ));
            }
        }

        fs::rename(&partial, &dest).map_err(|e| Error::new(ErrorKind::Io, format!("{e}")))?;

        Ok(())
    }
}

/// Whether a file is present at the expected size.
fn file_is_complete(file: &ModelFile, dir: &Path) -> bool {
    match (fs::metadata(dir.join(&file.local)), file.size) {
        (Ok(meta), Some(expected)) => meta.len() == expected,
        // Without a recorded size, presence is all that can be checked.
        (Ok(_), None) => true,
        (Err(_), _) => false,
    }
}

/// Check a finished download, returning the first problem found.
fn verify(spec: &ModelSpec, dir: &Path) -> Option<String> {
    for file in &spec.files {
        let path = dir.join(&file.local);
        match fs::metadata(&path) {
            Err(e) => {
                return Some(format!("{} is missing after download: {e}", file.local));
            }
            Ok(meta) => {
                if let Some(expected) = file.size {
                    if meta.len() != expected {
                        return Some(format!(
                            "{} is {} bytes, expected {expected}",
                            file.local,
                            meta.len()
                        ));
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ModelSpec;

    /// Two files: one with a known size, one without, and with a local name
    /// differing from the remote one.
    fn test_spec() -> ModelSpec {
        ModelSpec {
            id: "test".to_string(),
            display_name: "Test".to_string(),
            files: vec![
                ModelFile {
                    remote: "a.bin".to_string(),
                    local: "a.bin".to_string(),
                    size: Some(4),
                },
                ModelFile {
                    remote: "b.onnx".to_string(),
                    local: "renamed.onnx".to_string(),
                    size: None,
                },
            ],
            mirrors: Vec::new(),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("verse-model-test-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn directory_is_the_model_id_under_the_root() {
        let dir = Downloader::directory_for(&test_spec(), Path::new("/models"));
        assert!(dir.ends_with("test"));
    }

    #[test]
    fn presence_requires_the_expected_size() {
        // `is_present` takes the models root and looks inside the model's own
        // subdirectory, so the fixture has to live there too.
        let root = temp_dir("presence");
        let dir = Downloader::directory_for(&test_spec(), &root);
        fs::create_dir_all(&dir).unwrap();

        assert!(
            !Downloader::is_present(&test_spec(), &root),
            "nothing there yet"
        );

        fs::write(dir.join("a.bin"), b"abcd").unwrap();
        assert!(
            !Downloader::is_present(&test_spec(), &root),
            "second file still missing"
        );

        fs::write(dir.join("renamed.onnx"), b"x").unwrap();
        assert!(Downloader::is_present(&test_spec(), &root));

        // A file of the wrong size is not present, however plausible it looks.
        fs::write(dir.join("a.bin"), b"ab").unwrap();
        assert!(
            !Downloader::is_present(&test_spec(), &root),
            "wrong size is not present"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_names_the_file_that_is_wrong() {
        let dir = temp_dir("verify");
        fs::write(dir.join("a.bin"), b"ab").unwrap();
        fs::write(dir.join("renamed.onnx"), b"x").unwrap();

        let problem = verify(&test_spec(), &dir).expect("wrong size must be reported");
        assert!(
            problem.contains("a.bin"),
            "message should name the file: {problem}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_reports_a_missing_file() {
        let dir = temp_dir("verify-missing");
        let problem = verify(&test_spec(), &dir).expect("missing file must be reported");
        assert!(problem.contains("missing"), "got: {problem}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_spec_with_no_mirrors_fails_rather_than_hanging() {
        let root = temp_dir("no-mirrors");
        let cancel = CancelToken::new();
        let downloader = Downloader::new();

        let state = downloader
            .fetch(&test_spec(), &root, |_| {}, &cancel)
            .expect("fetch returns a state rather than erroring");

        assert!(state.is_settled());
        let reason = state.reason().expect("should have failed");
        assert!(reason.contains("mirror"), "got: {reason}");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_nested_local_path_has_its_directory_made() {
        // Qwen3's tokenizer files live in a subdirectory of the model
        // directory, and the `.part` file is written beside the destination.
        // Without the subdirectory the transfer fails on a path that was never
        // created — after downloading every file that came before it.
        let spec = ModelSpec {
            id: "nested".to_string(),
            display_name: "Nested".to_string(),
            files: vec![ModelFile {
                remote: "a.bin".to_string(),
                local: "sub/dir/a.bin".to_string(),
                size: Some(4),
            }],
            // A reserved port on the loopback interface, refused at once, so
            // the fetch gets past mirror selection and fails at the write.
            mirrors: vec![Mirror {
                name: "unreachable".to_string(),
                base_url: "http://127.0.0.1:1".to_string(),
                repo: "x".to_string(),
            }],
        };

        let root = temp_dir("nested-local");

        // The fetch is meant to fail. What is under test is that the directory
        // exists by the time it does.
        let _ = Downloader::new().fetch(&spec, &root, |_| {}, &CancelToken::new());

        assert!(
            Downloader::directory_for(&spec, &root).join("sub/dir").is_dir(),
            "the nested directory should have been created before the transfer"
        );

        let _ = fs::remove_dir_all(&root);
    }
}
