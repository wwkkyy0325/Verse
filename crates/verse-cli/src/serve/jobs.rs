//! The queue, the worker, and what a client can ask about a job.
//!
//! **One job at a time, deliberately.** A keeper is one model, and a model is
//! held for the whole of a transcription — `Transcriber` is `Send` and not
//! `Sync`, so exclusive access is forced rather than chosen. Requests beyond
//! the first wait in a queue rather than starting a second model: four
//! concurrent jobs would mean four resident copies of something that is 228 MB
//! or a gigabyte.
//!
//! **Nothing here grows with uptime.** The queue is bounded, the retained
//! results are bounded, and the bus is drained continuously by exactly one
//! thread that keeps a count rather than any text. `design.md` §3 makes this a
//! hard constraint for a long-running process, and a service is the first thing
//! in this project that is one.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use verse_core::{CancelToken, Error, ErrorKind, Event, EventBus, JobId, Subscription};
use verse_pipeline::{ModelKeeper, Request, Transcription};

use crate::report::{FileResult, Progress, VERSION};

/// How many jobs may wait before a submit is refused.
pub const MAX_QUEUED: usize = 32;

/// How many finished jobs are kept for a client to collect.
pub const MAX_RETAINED: usize = 32;

/// How long a client may take to be told anything, when the queue is full.
const RETRY_AFTER_SECONDS: u64 = 1;

/// Runs one job. A function so a test can drive the whole lifecycle without
/// weights — the same reason `keep::Loader` exists.
pub type Runner =
    dyn Fn(Request, JobId, &CancelToken) -> Result<Transcription, Error> + Send + Sync;

/// Where a job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Running => "running",
            State::Done => "done",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
        }
    }

    pub fn is_settled(self) -> bool {
        matches!(self, State::Done | State::Failed | State::Cancelled)
    }
}

/// What a client sent.
pub struct Submission {
    pub input: PathBuf,
    pub engine: String,
    /// The extension the result would be rendered as, for the `FileResult`.
    pub format: &'static str,
    pub request: Request,
}

/// One job, as the service remembers it.
struct JobRecord {
    id: u64,
    input: PathBuf,
    engine: String,
    format: &'static str,
    state: State,
    submitted: Instant,
    started: Option<Instant>,
    finished: Option<Instant>,
    /// Kept as a count, never as text: the full transcript arrives once, in the
    /// result, and holding every segment twice would double the only unbounded
    /// thing in this process.
    segments: usize,
    position_ms: u64,
    result: Option<FileResult>,
    cancel: CancelToken,
}

impl JobRecord {
    /// `origin` is when the service started, and every timestamp is measured
    /// from it — the same origin for all three, so they can be compared with
    /// each other and with `/health`. Measuring each from a different instant
    /// produces a set of numbers that look like timestamps and are not.
    fn view(&self, origin: Instant) -> JobView {
        let since = |at: Instant| at.saturating_duration_since(origin).as_millis() as u64;

        JobView {
            version: VERSION,
            id: self.id,
            state: self.state.name().to_string(),
            input: self.input.display().to_string(),
            engine: self.engine.clone(),
            submitted_at_ms: since(self.submitted),
            started_at_ms: self.started.map(since),
            finished_at_ms: self.finished.map(since),
            queued_ms: self.started.map(|started| {
                started.duration_since(self.submitted).as_millis() as u64
            }),
            elapsed_ms: match (self.started, self.finished) {
                (Some(started), Some(finished)) => {
                    Some(finished.duration_since(started).as_millis() as u64)
                }
                _ => None,
            },
            progress: if self.state == State::Running {
                Some(Progress {
                    segments: self.segments,
                    position_ms: self.position_ms,
                })
            } else {
                None
            },
            result: self.result.clone(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    pub version: u32,
    pub id: u64,
    pub state: String,
    pub input: String,
    pub engine: String,
    /// Milliseconds since the service started, for every timestamp. Relative
    /// rather than wall-clock because a client comparing two of these wants the
    /// interval, and the wall clock is available from `/health`.
    pub submitted_at_ms: u64,
    pub started_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    /// How long it waited before the worker picked it up.
    ///
    /// Separate from `elapsedMs` so the two can be told apart: a job that took
    /// a minute because it queued behind another is a different problem from
    /// one that took a minute because the recording is long.
    pub queued_ms: Option<u64>,
    /// How long the job took once the worker had it.
    ///
    /// **This is recognition time except on the job that had to load the
    /// model** — the first after the service starts, and the first after the
    /// model has been idle long enough to be released. Measured: a cold job at
    /// 272 ms against a warm one at 242 ms for the same clip, and 1558 ms for
    /// the job that paid the load. So it is comparable with the same field in
    /// `verse transcribe --json` for every job but that one, and a client
    /// timing the first job should expect it to be the slow one.
    pub elapsed_ms: Option<u64>,
    pub progress: Option<Progress>,
    pub result: Option<FileResult>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobList {
    pub version: u32,
    pub jobs: Vec<JobView>,
    pub queued: usize,
    pub running: bool,
}

/// Why a submission was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The queue is full.
    Full,
}

/// Everything the queue and the worker share.
struct Queue {
    records: BTreeMap<u64, JobRecord>,
    /// Ids waiting to run, oldest first.
    waiting: VecDeque<u64>,
    /// The request for each job that has not started. Held here rather than in
    /// the record, because a record does not otherwise need an engine
    /// configuration, and here rather than in a `static` because two of these
    /// may be alive at once — which in a test binary they are.
    pending: BTreeMap<u64, Request>,
    running: Option<u64>,
    next_id: u64,
}

struct Inner {
    /// When this service started. Every timestamp a client sees is measured
    /// from here.
    origin: Instant,
    state: Mutex<Queue>,
    signal: Condvar,
    runner: Box<Runner>,
}

/// The queue and the one worker that drains it.
pub struct Jobs {
    inner: Arc<Inner>,
}

impl Jobs {
    /// Start the worker.
    ///
    /// `bus` is the keeper's own bus; the worker subscribes to it separately so
    /// that progress reaches a job record.
    pub fn new(
        keeper: Arc<ModelKeeper>,
        bus: EventBus,
        runner: Option<Box<Runner>>,
    ) -> Self {
        let runner = runner.unwrap_or_else(|| {
            // The keeper is shared rather than moved: the service also needs to
            // ask it what it is doing, and a runner that owned it would leave
            // `/health` unable to answer.
            let keeper = Arc::clone(&keeper);
            Box::new(move |request: Request, job: JobId, cancel: &CancelToken| {
                keeper.transcribe(request, job, cancel)
            })
        });

        let inner = Arc::new(Inner {
            origin: Instant::now(),
            state: Mutex::new(Queue {
                records: BTreeMap::new(),
                waiting: VecDeque::new(),
                pending: BTreeMap::new(),
                running: None,
                next_id: 1,
            }),
            signal: Condvar::new(),
            runner,
        });

        spawn_worker(Arc::clone(&inner));
        spawn_drain(bus.subscribe_all(), Arc::clone(&inner));

        Self { inner }
    }

    /// Accept a job, or say the queue is full.
    pub fn submit(&self, submission: Submission) -> Result<u64, Refusal> {
        let mut state = self.inner.state.lock().expect("jobs mutex poisoned");

        if state.waiting.len() >= MAX_QUEUED {
            return Err(Refusal::Full);
        }

        let id = state.next_id;
        state.next_id += 1;
        state.waiting.push_back(id);
        state.records.insert(
            id,
            JobRecord {
                id,
                input: submission.input,
                engine: submission.engine,
                format: submission.format,
                state: State::Queued,
                submitted: Instant::now(),
                started: None,
                finished: None,
                segments: 0,
                position_ms: 0,
                result: None,
                cancel: CancelToken::new(),
            },
        );

        state.pending.insert(id, submission.request);

        self.inner.signal.notify_all();
        Ok(id)
    }

    pub fn get(&self, id: u64) -> Option<JobView> {
        let state = self.inner.state.lock().expect("jobs mutex poisoned");
        state
            .records
            .get(&id)
            .map(|record| record.view(self.inner.origin))
    }

    /// Every job still held, newest first.
    pub fn list(&self) -> JobList {
        let state = self.inner.state.lock().expect("jobs mutex poisoned");
        let mut jobs: Vec<JobView> = state
            .records
            .values()
            .map(|record| record.view(self.inner.origin))
            .collect();
        jobs.reverse();

        JobList {
            version: VERSION,
            jobs,
            queued: state.waiting.len(),
            running: state.running.is_some(),
        }
    }

    /// Ask a job to stop.
    ///
    /// A queued job leaves at once. A running one is cancelled through its
    /// token and settles when the pipeline returns, which is not the same
    /// instant — the response says so by still reporting `running`.
    pub fn cancel(&self, id: u64) -> Result<JobView, CancelOutcome> {
        let mut state = self.inner.state.lock().expect("jobs mutex poisoned");

        let Some(record) = state.records.get(&id) else {
            return Err(CancelOutcome::Unknown);
        };
        if record.state.is_settled() {
            return Err(CancelOutcome::Settled);
        }

        if record.state == State::Queued {
            state.waiting.retain(|queued| *queued != id);
            state.pending.remove(&id);

            let record = state.records.get_mut(&id).expect("just checked");
            record.state = State::Cancelled;
            record.finished = Some(Instant::now());
            let (input, format) = (record.input.clone(), record.format);
            record.result = Some(cancelled_result(&input, format));

            return Ok(record.view(self.inner.origin));
        }

        let record = state.records.get(&id).expect("just checked");
        record.cancel.cancel();
        Ok(record.view(self.inner.origin))
    }

    /// How many finished jobs are being kept.
    pub fn retained(&self) -> usize {
        let state = self.inner.state.lock().expect("jobs mutex poisoned");
        state.records.values().filter(|r| r.state.is_settled()).count()
    }

    pub fn queued(&self) -> usize {
        self.inner.state.lock().expect("jobs mutex poisoned").waiting.len()
    }

    pub fn running(&self) -> bool {
        self.inner
            .state
            .lock()
            .expect("jobs mutex poisoned")
            .running
            .is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    Unknown,
    Settled,
}

fn spawn_worker(inner: Arc<Inner>) {
    std::thread::spawn(move || {
        loop {
            let (id, request) = {
                let mut state = inner.state.lock().expect("jobs mutex poisoned");

                loop {
                    if let Some(id) = state.waiting.pop_front() {
                        let Some(request) = state.pending.remove(&id) else {
                            continue;
                        };

                        state.running = Some(id);
                        if let Some(record) = state.records.get_mut(&id) {
                            record.state = State::Running;
                            record.started = Some(Instant::now());
                        }
                        break (id, request);
                    }

                    state = inner
                        .signal
                        .wait(state)
                        .expect("jobs mutex poisoned");
                }
            };

            // The model mutex is taken inside the runner and held for the whole
            // job, so nothing else can be recognising while this runs.
            let (cancel, input, format) = {
                let state = inner.state.lock().expect("jobs mutex poisoned");
                let record = state.records.get(&id).expect("running job has a record");
                (record.cancel.clone(), record.input.clone(), record.format)
            };

            let outcome = (inner.runner)(request, JobId(id), &cancel);

            let mut state = inner.state.lock().expect("jobs mutex poisoned");
            state.running = None;

            if let Some(record) = state.records.get_mut(&id) {
                record.finished = Some(Instant::now());
                let elapsed = record
                    .started
                    .map(|started| started.elapsed().as_millis() as u64)
                    .unwrap_or(0);

                match outcome {
                    Ok(transcription) => {
                        record.state = State::Done;
                        // `elapsedMs` is recognition time only, so it is
                        // comparable with the same field in the CLI's report.
                        record.result = Some(FileResult::transcribed(
                            &input,
                            None,
                            format,
                            elapsed,
                            &transcription,
                        ));
                    }
                    Err(error) if error.kind() == ErrorKind::Cancelled => {
                        record.state = State::Cancelled;
                        record.result = Some(FileResult::failed(
                            &input,
                            format,
                            elapsed,
                            error.kind(),
                            error.message().to_string(),
                        ));
                    }
                    Err(error) => {
                        record.state = State::Failed;
                        record.result = Some(FileResult::failed(
                            &input,
                            format,
                            elapsed,
                            error.kind(),
                            error.message().to_string(),
                        ));
                    }
                }
            }

            inner.signal.notify_all();
        }
    });
}

/// The bus is unbounded, so it must be drained continuously and by exactly one
/// thread. A subscriber that stopped reading would accumulate every event of
/// every job for the life of the process.
fn spawn_drain(subscription: Subscription, inner: Arc<Inner>) {
    std::thread::spawn(move || loop {
        let event = match subscription.recv_timeout(Duration::from_millis(250)) {
            Some(event) => event,
            None => continue,
        };

        let Some(job) = event.job() else { continue };
        let mut state = inner.state.lock().expect("jobs mutex poisoned");
        let Some(record) = state.records.get_mut(&job.0) else {
            continue;
        };

        match &*event {
            // A count, never the text: the transcript arrives once, in the
            // result, and holding every segment twice would double the only
            // unbounded thing in this process.
            Event::TranscriptSegment { .. } => record.segments += 1,
            Event::JobProgress { position, .. } => {
                record.position_ms = position.as_millis() as u64;
            }
            _ => {}
        }
    });
}

fn cancelled_result(input: &std::path::Path, format: &'static str) -> FileResult {
    FileResult::failed(
        input,
        format,
        0,
        ErrorKind::Cancelled,
        "the job was cancelled before it started".to_string(),
    )
}

/// How long a refused client should wait before trying again.
pub fn retry_after() -> String {
    RETRY_AFTER_SECONDS.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use verse_core::{Segment, SegmentId, Transcript};
    use verse_pipeline::{GuardSettings, ModelKeeper};

    fn a_request() -> Request {
        let dir = std::env::temp_dir();
        Request {
            input: dir.join("audio.wav"),
            models_dir: dir.clone(),
            engine: "sensevoice".to_string(),
            vad_model: dir.join("vad.onnx"),
            inverse_text_normalization: true,
            vad: verse_audio::VadSettings::default(),
            max_output_tokens: None,
            guard: GuardSettings::default(),
            hotwords: None,
            threads: None,
            cache: verse_pipeline::CachePolicy::Disabled,
        }
    }

    fn a_submission() -> Submission {
        Submission {
            input: std::env::temp_dir().join("audio.wav"),
            engine: "sensevoice".to_string(),
            format: "srt",
            request: a_request(),
        }
    }

    /// A runner that says a fixed thing and returns at once.
    fn instant(text: &str) -> Box<Runner> {
        let text = text.to_string();
        Box::new(move |_request, _job, _cancel| {
            Ok(Transcription {
                transcript: Transcript {
                    segments: vec![Segment {
                        id: SegmentId(0),
                        start: Duration::from_millis(0),
                        end: Duration::from_millis(900),
                        text: text.clone(),
                    }],
                    language: None,
                },
                coverage: verse_pipeline::Coverage {
                    decoded_seconds: 1.0,
                    voiced_seconds: 1.0,
                    energetic_seconds: 1.0,
                },
                recovered: false,
                cached: false,
            })
        })
    }

    fn a_jobs(runner: Box<Runner>) -> Jobs {
        let bus = EventBus::new();
        let keeper = Arc::new(ModelKeeper::with_timeout(bus.clone(), None));
        Jobs::new(keeper, bus, Some(runner))
    }

    /// Wait for a job to settle, or give up.
    fn settled(jobs: &Jobs, id: u64) -> JobView {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let view = jobs.get(id).expect("the job exists");
            if matches!(view.state.as_str(), "done" | "failed" | "cancelled") {
                return view;
            }
            assert!(Instant::now() < deadline, "job {id} never settled");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_submitted_job_runs_and_carries_a_result() {
        let jobs = a_jobs(instant("开放时间"));
        let id = jobs.submit(a_submission()).expect("accepted");

        let view = settled(&jobs, id);
        assert_eq!(view.state, "done");
        assert_eq!(view.id, id);

        let result = view.result.expect("a result");
        assert!(result.ok);
        assert_eq!(result.text.as_deref(), Some("开放时间"));
        assert_eq!(result.segments.len(), 1);
    }

    #[test]
    fn the_result_is_the_same_shape_the_command_line_emits() {
        // The claim this whole arrangement rests on: a job's result is a
        // `FileResult`, so a client parses one thing whether it drove the
        // service or read `verse transcribe --json`.
        let jobs = a_jobs(instant("开放时间"));
        let id = jobs.submit(a_submission()).expect("accepted");
        let result = settled(&jobs, id).result.expect("a result");

        let value = serde_json::to_value(&result).expect("serialises");
        for key in [
            "input",
            "output",
            "format",
            "ok",
            "elapsedMs",
            "segmentCount",
            "coverage",
            "recovered",
            "cached",
            "language",
            "text",
            "warnings",
            "segments",
            "error",
        ] {
            assert!(value.get(key).is_some(), "missing key: {key}");
        }
    }

    #[test]
    fn a_failing_runner_lands_as_failed_with_the_error_named() {
        let jobs = a_jobs(Box::new(|_r, _j, _c| {
            Err(Error::new(ErrorKind::Model, "no usable model"))
        }));
        let id = jobs.submit(a_submission()).expect("accepted");

        let view = settled(&jobs, id);
        assert_eq!(view.state, "failed");

        let result = view.result.expect("a result");
        assert!(!result.ok);
        assert_eq!(result.error.as_ref().expect("an error").kind, "model");
    }

    #[test]
    fn cancelling_a_queued_job_settles_it_at_once() {
        // The first job holds the worker; the second is still in the queue.
        let jobs = a_jobs(Box::new(|_r, _j, cancel| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !cancel.is_cancelled() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(Error::cancelled())
        }));

        let first = jobs.submit(a_submission()).expect("accepted");

        // Wait for the first to take the worker, so that what is in the queue
        // is a fact rather than a matter of scheduling. Asserting on the queue
        // length before this is a race, and it was one.
        let deadline = Instant::now() + Duration::from_secs(5);
        while jobs.get(first).expect("exists").state != "running" && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(jobs.get(first).expect("exists").state, "running");

        let second = jobs.submit(a_submission()).expect("accepted");
        assert_eq!(jobs.queued(), 1, "the second is waiting its turn");

        let view = jobs.cancel(second).expect("cancellable");
        assert_eq!(view.state, "cancelled");
        assert!(view.result.is_some());
        assert_eq!(jobs.queued(), 0, "it left the queue");

        jobs.cancel(first).expect("cancellable");
        settled(&jobs, first);
    }

    #[test]
    fn cancelling_a_running_job_settles_it_when_the_runner_returns() {
        let jobs = a_jobs(Box::new(|_r, _j, cancel| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !cancel.is_cancelled() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(Error::cancelled())
        }));

        let id = jobs.submit(a_submission()).expect("accepted");
        // Wait for it to actually be running, or `cancel` would find it queued.
        let deadline = Instant::now() + Duration::from_secs(5);
        while jobs.get(id).expect("exists").state != "running" && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }

        let immediate = jobs.cancel(id).expect("cancellable");
        assert_eq!(immediate.state, "running", "not settled yet");

        let view = settled(&jobs, id);
        assert_eq!(view.state, "cancelled");
    }

    #[test]
    fn cancelling_something_settled_says_so() {
        let jobs = a_jobs(instant("x"));
        let id = jobs.submit(a_submission()).expect("accepted");
        settled(&jobs, id);

        assert!(matches!(jobs.cancel(id), Err(CancelOutcome::Settled)));
        assert!(matches!(jobs.cancel(9999), Err(CancelOutcome::Unknown)));
    }

    #[test]
    fn the_queue_is_bounded_and_says_so_rather_than_growing() {
        // A job that never returns, so the queue actually fills.
        let jobs = a_jobs(Box::new(|_r, _j, cancel| {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !cancel.is_cancelled() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(Error::cancelled())
        }));

        // One job takes the worker, and waiting for it to be running is what
        // makes the queue length a fact rather than a race: otherwise the
        // worker pops as fast as this pushes, and the count never reaches the
        // bound.
        let running = jobs.submit(a_submission()).expect("accepted");
        let deadline = Instant::now() + Duration::from_secs(5);
        while jobs.get(running).expect("exists").state != "running" && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(jobs.get(running).expect("exists").state, "running");

        let mut accepted = Vec::new();
        for index in 0..MAX_QUEUED {
            match jobs.submit(a_submission()) {
                Ok(id) => accepted.push(id),
                Err(Refusal::Full) => panic!("refused at {index}, before the bound"),
            }
        }
        assert_eq!(jobs.queued(), MAX_QUEUED, "the queue is exactly full");

        // And one more is refused rather than growing.
        assert!(matches!(jobs.submit(a_submission()), Err(Refusal::Full)));

        for id in accepted {
            let _ = jobs.cancel(id);
        }
        let _ = jobs.cancel(running);
    }

    #[test]
    fn the_listing_is_newest_first() {
        let jobs = a_jobs(instant("x"));
        let first = jobs.submit(a_submission()).expect("accepted");
        settled(&jobs, first);
        let second = jobs.submit(a_submission()).expect("accepted");
        settled(&jobs, second);

        let list = jobs.list();
        assert_eq!(list.version, VERSION);
        assert_eq!(list.jobs[0].id, second, "newest first");
        assert_eq!(list.jobs[1].id, first);
    }

    #[test]
    fn a_queued_job_reports_its_wait_separately_from_its_work() {
        // `elapsedMs` is recognition time only, so it means the same thing here
        // as it does in the CLI's report. The queue wait is its own field.
        let jobs = a_jobs(instant("x"));
        let id = jobs.submit(a_submission()).expect("accepted");
        let view = settled(&jobs, id);

        assert!(view.queued_ms.is_some());
        assert!(view.elapsed_ms.is_some());
        assert!(view.started_at_ms.is_some());
        assert!(view.finished_at_ms.is_some());
    }
}
