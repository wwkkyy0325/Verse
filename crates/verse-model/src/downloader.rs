//! Fetching model files, with mirror failover and resume.
//!
//! Nothing here runs on its own. A download happens because someone called
//! [`Downloader::fetch`] — no startup fetch, no background refresh.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};
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

/// How many times one mirror is tried before the next is used.
///
/// One was not enough. `qwen3-asr` has a single mirror — a third-party upload
/// — so a connection that dropped once was the end of the download and the
/// person had to press the button again.
const ATTEMPTS_PER_MIRROR: usize = 3;

/// The wait before a second attempt at the same mirror, doubled each time.
///
/// Small on purpose. This is for a connection that blipped, not for a mirror
/// that is down: something genuinely unavailable should fall through to the
/// next mirror quickly rather than be waited on.
const RETRY_BACKOFF: Duration = Duration::from_millis(400);

/// How many files are fetched at once.
///
/// A model's files are independent of each other, so this is what turns a
/// 987 MB download into the time of its largest file rather than the sum of
/// all five. Three rather than "as many as there are": they come from the same
/// mirror, and a client that opens five connections to one host is a client
/// that will be rate-limited.
const MAX_PARALLEL_FILES: usize = 3;

/// The caller's progress callback, serialised while several threads report
/// through it.
type Reporting = std::sync::Mutex<Box<dyn FnMut(&DownloadState) + Send>>;

/// A failed attempt at one mirror, and whether another go could help.
struct AttemptFailed {
    error: Error,
    worth_retrying: bool,
}

impl From<Error> for AttemptFailed {
    fn from(error: Error) -> Self {
        // The default is by kind: a network error might be a blip, an I/O error
        // is the disk and will say the same thing however many times it is
        // asked. A status the server chose is overridden where it is read.
        Self {
            worth_retrying: error.kind() == ErrorKind::Network,
            error,
        }
    }
}

/// Sleep, unless the job is cancelled.
///
/// In slices, so somebody who pressed 取消 does not have to wait out a backoff
/// before the window notices.
fn pause(total: Duration, cancel: &CancelToken) -> Result<()> {
    const SLICE: Duration = Duration::from_millis(50);

    let mut left = total;
    while left > Duration::ZERO {
        cancel.check()?;
        let step = left.min(SLICE);
        std::thread::sleep(step);
        left -= step;
    }

    Ok(())
}

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
        on_state: impl FnMut(&DownloadState) + Send + 'static,
        cancel: &CancelToken,
    ) -> Result<DownloadState> {
        let dir = Self::directory_for(spec, root);

        // One place the callback is reached through, because it is now called
        // from several threads while the files are fetched together. Serialised
        // here rather than making every caller think about it.
        let reporting: Reporting = std::sync::Mutex::new(Box::new(on_state));
        let report = |state: &DownloadState| {
            let mut report = reporting.lock().expect("report mutex poisoned");
            report(state);
        };

        /// Report a failure and answer it, so no caller has to remember both.
        ///
        /// **This was a bug.** Every failure used to `return Ok(Failed { .. })`
        /// without telling the callback, so a download that failed never
        /// reached the window: it sat on "fetching" for ever, and the only way
        /// to learn otherwise was the return value — which the window does not
        /// read.
        fn give_up(report: &dyn Fn(&DownloadState), reason: String) -> DownloadState {
            let state = DownloadState::Failed { reason };
            report(&state);
            state
        }

        if let Err(e) = fs::create_dir_all(&dir) {
            let reason = format!("could not create {}: {e}", dir.display());
            return Ok(give_up(&report, reason));
        }

        report(&DownloadState::Idle);

        // Before the first byte, rather than an I/O error a gigabyte in. The
        // headroom is because a filesystem needs room for its own bookkeeping
        // and a disk filled to the last byte is a machine that has stopped.
        const HEADROOM: u64 = 64 * 1024 * 1024;

        if let (Some(needed), Some(free)) = (missing_bytes(spec, &dir), free_bytes(&dir)) {
            if needed + HEADROOM > free {
                let reason = format!(
                    "not enough room for {}: {needed} bytes needed, {free} available",
                    spec.id
                );
                return Ok(give_up(&report, reason));
            }
        }

        // The files that are not here yet, fetched together. A model's files do
        // not depend on each other: `qwen3-asr` is 987 MB across five, and its
        // largest is 756 of them, so fetching the other four alongside it
        // costs nothing and saves the rest.
        let missing: Vec<&ModelFile> = spec
            .files
            .iter()
            .filter(|file| !file_is_complete(file, &dir))
            .collect();

        if !missing.is_empty() {
            if let Err(failure) = self.fetch_together(spec, &missing, &dir, &reporting, cancel) {
                if failure.kind() == ErrorKind::Cancelled {
                    report(&DownloadState::Idle);
                    return Ok(DownloadState::Idle);
                }
                let reason = failure.message().to_string();
                return Ok(give_up(&report, reason));
            }
        }

        report(&DownloadState::Verifying);

        if let Some(problem) = verify(spec, &dir) {
            return Ok(give_up(&report, problem));
        }

        report(&DownloadState::Ready);
        Ok(DownloadState::Ready)
    }

    /// Fetch several files at once, stopping everyone on the first failure.
    ///
    /// The first error is the one reported: three workers failing against one
    /// unreachable mirror is one problem, and reporting the third interruption
    /// rather than the first would say less about it.
    fn fetch_together(
        &self,
        spec: &ModelSpec,
        missing: &[&ModelFile],
        dir: &Path,
        reporting: &Reporting,
        cancel: &CancelToken,
    ) -> Result<()> {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let outcome: std::sync::Mutex<Option<Error>> = std::sync::Mutex::new(None);

        let workers = missing.len().min(MAX_PARALLEL_FILES.max(1));

        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| loop {
                    if outcome.lock().expect("outcome mutex poisoned").is_some() {
                        return;
                    }

                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(file) = missing.get(index) else {
                        return;
                    };

                    let result = self.fetch_file(spec, file, dir, &mut |state| {
                        let mut report = reporting.lock().expect("report mutex poisoned");
                        report(state);
                    }, cancel);

                    if let Err(error) = result {
                        let mut slot = outcome.lock().expect("outcome mutex poisoned");
                        // The first failure is the one worth reporting; a
                        // cancellation outranks it, because it is the user.
                        if slot.is_none() || error.kind() == ErrorKind::Cancelled {
                            *slot = Some(error);
                        }
                        return;
                    }
                });
            }
        });

        match outcome.into_inner().expect("outcome mutex poisoned") {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Try every mirror in turn for one file, retrying each a few times.
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
            for attempt in 0..ATTEMPTS_PER_MIRROR {
                match self.try_mirror(mirror, file, dir, on_state, cancel) {
                    Ok(()) => return Ok(()),

                    Err(failed) => {
                        // Cancellation is the user's decision, not a mirror
                        // problem; retrying or moving on would override it.
                        if failed.error.kind() == ErrorKind::Cancelled {
                            return Err(failed.error);
                        }

                        let last = attempt + 1 == ATTEMPTS_PER_MIRROR;

                        if last || !failed.worth_retrying {
                            failures.push(format!(
                                "{} ({})",
                                mirror.name,
                                failed.error.message()
                            ));
                            break;
                        }

                        // Doubling: 400 ms, then 800 ms.
                        pause(RETRY_BACKOFF * (1 << attempt), cancel)?;
                    }
                }
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
    ) -> std::result::Result<(), AttemptFailed> {
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
            return Err(AttemptFailed {
                error: Error::new(ErrorKind::Network, format!("unexpected status {status}")),
                // A server that answered with a client error will answer the
                // same way however many times it is asked. A 5xx is usually a
                // moment, and worth another go.
                worth_retrying: status >= 500,
            });
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

        // Fed from the same buffer that goes to the disk, so verification costs
        // no second read. A resumed transfer cannot be hashed piecewise — the
        // digest covers the whole file — so it is computed only for a transfer
        // that started at zero, and a resumed one is checked by length alone.
        // Stated rather than hidden: `None` here means "this attempt cannot
        // say", and the caller treats it as such.
        let mut hasher = (!resumed && file.sha256.is_some()).then(Sha256::new);

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
            if let Some(hasher) = hasher.as_mut() {
                hasher.update(&buffer[..read]);
            }
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
                )
                .into());
            }
        }

        // Checked before the rename, so nothing that fails verification is ever
        // at the destination where `is_present` would trust it by length.
        if let (Some(hasher), Some(expected)) = (hasher, file.sha256.as_deref()) {
            let found = format!("{:x}", hasher.finalize());
            if !found.eq_ignore_ascii_case(expected) {
                // The partial goes: resuming would continue from bytes that are
                // already known to be wrong, and no later attempt would ever
                // agree with the digest.
                let _ = fs::remove_file(&partial);
                return Err(Error::new(
                    ErrorKind::Network,
                    format!("{found} does not match the recorded {expected}"),
                )
                .into());
            }
        }

        fs::rename(&partial, &dest).map_err(|e| Error::new(ErrorKind::Io, format!("{e}")))?;

        Ok(())
    }
}

/// The SHA-256 of a whole file, lowercase hex.
///
/// Public because it is also how the hashes in the catalogue were produced.
/// Sitting beside the check that consumes them means the two cannot be
/// different algorithms — which is the failure that would make every recorded
/// hash wrong at once and look like every mirror being corrupt.
///
/// `Ok(None)` for a file that cannot be read: the caller decides whether that
/// is a problem.
pub fn sha256_file(path: &Path) -> std::io::Result<Option<String>> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };

    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(Some(format!("{:x}", hasher.finalize())))
}

/// The bytes a fetch would still have to write, or `None` when a size is
/// unknown.
///
/// Only the files that are missing: a resumed download has paid for what is
/// already on disk, and counting it would refuse work there is room for.
fn missing_bytes(spec: &ModelSpec, dir: &Path) -> Option<u64> {
    let mut total = 0u64;

    for file in &spec.files {
        if file_is_complete(file, dir) {
            continue;
        }
        total += file.size?;
    }

    Some(total)
}

/// How much room is left on the filesystem holding `dir`, or `None` when it
/// will not say.
///
/// An unanswerable question must not refuse a download: the check is a courtesy
/// that turns a failure in the middle into a sentence before it.
fn free_bytes(dir: &Path) -> Option<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();

    // The longest mount point that is a prefix of the path — a filesystem
    // mounted at `/home` and one at `/` both match `/home/x`, and the deeper
    // one is the one that answers.
    disks
        .list()
        .iter()
        .filter(|disk| dir.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .map(|disk| disk.available_space())
}

/// Whether a file is present at the expected size.
///
/// Public because the window has to know the same thing to draw an honest
/// progress bar for a resumed download — the bytes already on disk count
/// towards it, and a second definition of "complete" is a second thing that
/// can disagree with this one.
pub fn file_is_complete(file: &ModelFile, dir: &Path) -> bool {
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
            description: None,
            display_name: "Test".to_string(),
            files: vec![
                ModelFile {
                    remote: "a.bin".to_string(),
                    local: "a.bin".to_string(),
                    size: Some(4),
                    sha256: None,
                },
                ModelFile {
                    remote: "b.onnx".to_string(),
                    local: "renamed.onnx".to_string(),
                    size: None,
                    sha256: None,
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
    fn a_failure_reaches_the_callback_and_not_only_the_return_value() {
        // This was a bug, and a quiet one. Every failure path returned
        // `Ok(Failed { .. })` without telling the callback, so a download that
        // failed never reached the window: it sat on "fetching" for ever, and
        // the return value — which the window does not read — was the only
        // place the failure existed.
        let spec = ModelSpec {
            id: "unreachable".to_string(),
            description: None,
            display_name: "Unreachable".to_string(),
            files: vec![ModelFile {
                remote: "a.bin".to_string(),
                local: "a.bin".to_string(),
                size: Some(4),
                sha256: None,
            }],
            // Fails at the request rather than after a network round trip.
            mirrors: vec![Mirror {
                name: "unreachable".to_string(),
                base_url: "not-a-url".to_string(),
                repo: "x".to_string(),
            }],
        };

        let root = temp_dir("failure-reported");
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let watching = std::sync::Arc::clone(&seen);

        let outcome = Downloader::new()
            .fetch(
                &spec,
                &root,
                move |state| {
                    watching
                        .lock()
                        .expect("seen mutex poisoned")
                        .push(format!("{state:?}"));
                },
                &CancelToken::new(),
            )
            .expect("a fetch answers with a state rather than an error");

        assert!(
            matches!(outcome, DownloadState::Failed { .. }),
            "got {outcome:?}"
        );

        let seen = seen.lock().expect("seen mutex poisoned").join(" | ");
        assert!(
            seen.contains("Failed"),
            "the callback must see the failure, not only the caller: {seen}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_digest_of_known_bytes_is_the_published_one() {
        // "abc" is the standard short vector. Checked against a value from
        // outside this program, because a hash function compared only against
        // itself would agree with any mistake it made.
        let dir = temp_dir("sha-vector");
        let path = dir.join("abc.txt");
        fs::create_dir_all(&dir).expect("dir");
        fs::write(&path, b"abc").expect("write");

        assert_eq!(
            sha256_file(&path).expect("hash").as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_is_not_there_has_no_digest_rather_than_an_error() {
        let dir = temp_dir("sha-missing");
        fs::create_dir_all(&dir).expect("dir");

        assert_eq!(sha256_file(&dir.join("nope")).expect("no error"), None);

        let _ = fs::remove_dir_all(&dir);
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
            description: None,
            display_name: "Nested".to_string(),
            files: vec![ModelFile {
                remote: "a.bin".to_string(),
                local: "sub/dir/a.bin".to_string(),
                size: Some(4),
                sha256: None,
            }],
            // Deliberately not a URL. The point is that the directory exists
            // by the time the transfer fails, and a malformed address fails at
            // the request rather than after a network round trip — a closed
            // port costs about two seconds here, which the retry then pays
            // three times, and the test has no reason to wait for any of it.
            mirrors: vec![Mirror {
                name: "unreachable".to_string(),
                base_url: "not-a-url".to_string(),
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
