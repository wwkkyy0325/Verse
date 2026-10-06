//! Holding a model between jobs.
//!
//! A model is the largest thing in this program's memory — 228 MB for the
//! default, about a gigabyte for Qwen3 — and loading one takes longer than
//! recognising a short utterance. `Transcriber` was built to be loaded once and
//! used for many files; this is the thing that owns it across those uses,
//! because until now nothing did: the window loaded one inside every per-job
//! thread and dropped it when the thread ended, which measured at 89% of the
//! work done on a one-second clip.
//!
//! Three controls, which are the three things anyone asks of a long-lived
//! resource: [`ModelKeeper::preload`] starts it, [`ModelKeeper::release`] stops
//! it, and [`ModelKeeper::status`] says what it is doing. Between those, a model
//! releases itself once it has been idle long enough.
//!
//! **This is the component a local service would hold.** No transport is built
//! here; `status` and `with_timeout` are the two seams such a thing needs, and
//! they exist and are used by the window rather than being designed around.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use verse_core::{CancelToken, Error, Event, EventBus, JobId, JobKind};

use crate::{effective_threads, settings_digest, Request, Transcriber, Transcription};

/// How long a model may sit unused before it is released.
///
/// **Reasoned, not measured** — unlike the constants in `coverage.rs` and
/// `vad.rs`, which were chosen from data. What it is reasoned from: the window's
/// usage is several files in a row, which happens within minutes, so five
/// minutes keeps a batch warm; and a window left open all evening should not be
/// holding a gigabyte for nothing. Revisit with a number if there is ever
/// evidence about how long a real pause is.
pub const DEFAULT_IDLE: Duration = Duration::from_secs(300);

/// How often the sweeper wakes to look at the clock.
///
/// The same interval the window's event forwarder uses, for the same reason: a
/// shorter one burns wakeups to learn nothing.
const POLL: Duration = Duration::from_millis(250);

const UNLOADED: u8 = 0;
const LOADING: u8 = 1;
const STANDBY: u8 = 2;
const BUSY: u8 = 3;

/// What the keeper is doing with a model right now.
///
/// **An in-memory vocabulary, and deliberately not `verse_core::ModelState`.**
/// That one is documented as the lifecycle of a model *on disk* — absent,
/// downloading, present — and a `Ready` that meant both "the file is there" and
/// "the weights are resident" would be one word for two facts, which is the
/// collapse this project has already rejected once. Nothing here is an event:
/// see [`ModelKeeper::status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelStatus {
    /// No model in memory.
    Unloaded,
    /// Reading weights. No job can run yet.
    Loading,
    /// Held, and no job is running.
    Standby,
    /// Held, and a job is running.
    Busy,
}

/// Builds a transcriber for a request. A function so a test can describe a
/// machine that loads instantly, and so the transport a later round adds can
/// supply its own.
type Loader = dyn Fn(&Request) -> Result<Transcriber, Error> + Send + Sync;

struct Loaded {
    /// The settings digest this model was built for — the same string the cache
    /// keys on, and the only thing that decides whether a new request can reuse
    /// it.
    identity: String,
    transcriber: Transcriber,
}

struct Inner {
    bus: EventBus,
    /// Held for the **whole** job, not just the load.
    ///
    /// `Transcriber` is `Send` and not `Sync` — `AsrEngine: Send` and nothing
    /// more — so exclusive access is forced rather than chosen. It also gives
    /// the honest semantics: one model does one job at a time. A second caller
    /// waits, which is what it should do with a gigabyte of weights.
    loaded: Mutex<Option<Loaded>>,
    /// Read without the lock, so a status call cannot block behind a job that
    /// runs for minutes. The only reason the state is split in two.
    activity: AtomicU8,
    last_used: Mutex<Instant>,
    /// `ffmpeg -version`, which spawns a process, captured once. Everything
    /// else in the digest is a handful of `stat` calls, so this is what makes
    /// a per-job identity check cheap enough to do every time.
    ffmpeg: OnceLock<Option<String>>,
    notify: mpsc::Sender<()>,
    sweeper: Mutex<Option<std::thread::JoinHandle<()>>>,
    loader: Box<Loader>,
    loads: AtomicU64,
}

impl Inner {
    fn set(&self, activity: u8) {
        self.activity.store(activity, Ordering::Relaxed);
    }

    /// Let the model go if it has been unused for long enough.
    ///
    /// `try_lock` rather than `lock`: if a job holds the model then the model is
    /// not idle, and blocking here would only make a background thread wait for
    /// a job it has no business interrupting.
    fn sweep(&self, idle: Duration) {
        let Ok(mut loaded) = self.loaded.try_lock() else {
            return;
        };
        if loaded.is_none() {
            return;
        }

        let last = *self.last_used.lock().expect("keeper mutex poisoned");
        if last.elapsed() < idle {
            return;
        }

        loaded.take();
        self.set(UNLOADED);
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Dropping the sender disconnects the receiver, which is what tells the
        // sweeper to stop. Taking the handle first, so this cannot be reached
        // twice.
        let handle = self
            .sweeper
            .lock()
            .expect("keeper mutex poisoned")
            .take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

/// Owns at most one loaded model, and hands it to one job at a time.
pub struct ModelKeeper {
    inner: Arc<Inner>,
}

impl ModelKeeper {
    /// A keeper that releases its model after [`DEFAULT_IDLE`].
    pub fn new(bus: EventBus) -> Self {
        Self::with_timeout(bus, Some(DEFAULT_IDLE))
    }

    /// A keeper with a chosen idle timeout. `None` holds the model until it is
    /// released or the process ends.
    pub fn with_timeout(bus: EventBus, idle: Option<Duration>) -> Self {
        Self::build(bus, idle, Box::new(|request: &Request| Transcriber::load(request.clone())))
    }

    #[cfg(test)]
    fn with_loader(bus: EventBus, idle: Option<Duration>, loader: Box<Loader>) -> Self {
        Self::build(bus, idle, loader)
    }

    fn build(bus: EventBus, idle: Option<Duration>, loader: Box<Loader>) -> Self {
        let (notify, wakeups) = mpsc::channel();

        let inner = Arc::new(Inner {
            bus,
            loaded: Mutex::new(None),
            activity: AtomicU8::new(UNLOADED),
            last_used: Mutex::new(Instant::now()),
            ffmpeg: OnceLock::new(),
            notify,
            sweeper: Mutex::new(None),
            loader,
            loads: AtomicU64::new(0),
        });

        // Only when there is a deadline to enforce. A keeper told to hold its
        // model for good should not own a thread that wakes to decide nothing.
        if let Some(idle) = idle {
            let weak = Arc::downgrade(&inner);
            let handle = std::thread::spawn(move || {
                // A finished job wakes this early; otherwise it is the clock.
                // `Disconnected` — the keeper is gone — ends the loop.
                while let Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) =
                    wakeups.recv_timeout(POLL)
                {
                    match weak.upgrade() {
                        Some(inner) => inner.sweep(idle),
                        None => break,
                    }
                }
            });

            *inner.sweeper.lock().expect("keeper mutex poisoned") = Some(handle);
        }

        Self { inner }
    }

    /// What the keeper is doing. Never blocks, so it is safe to ask while a job
    /// runs — which is the only time anyone asks.
    ///
    /// A query rather than an event, deliberately. `Event::ModelStateChanged`
    /// has been defined and unused since the first phase of this project; adding
    /// a second event that nothing subscribes to would repeat that, and wrapping
    /// an in-memory lifetime in an on-disk vocabulary would say one word for two
    /// facts. When something needs to be *pushed* rather than asked, that is the
    /// moment to decide what it should say.
    pub fn status(&self) -> ModelStatus {
        match self.inner.activity.load(Ordering::Relaxed) {
            LOADING => ModelStatus::Loading,
            STANDBY => ModelStatus::Standby,
            BUSY => ModelStatus::Busy,
            _ => ModelStatus::Unloaded,
        }
    }

    /// How many times a model has been read from disk.
    ///
    /// The number this type exists to keep small, and the instrument the reuse
    /// measurement is read with. Exposed rather than kept for tests because
    /// "how often did it reload" is the first question anyone asks of a
    /// long-lived resource, and a service's own statistics would want it.
    pub fn loads(&self) -> u64 {
        self.inner.loads.load(Ordering::Relaxed)
    }

    /// Read the weights now, so the first job does not pay for it.
    ///
    /// Returns the error rather than publishing one: a preload is not a job, so
    /// there is no job id for an event to belong to.
    pub fn preload(&self, request: &Request) -> Result<(), Error> {
        let identity = self.identity(request);
        let mut loaded = self.inner.loaded.lock().expect("keeper mutex poisoned");

        if loaded.as_ref().is_some_and(|l| l.identity == identity) {
            return Ok(());
        }

        self.inner.set(LOADING);
        match (self.inner.loader)(request) {
            Ok(transcriber) => {
                *loaded = Some(Loaded {
                    identity,
                    transcriber,
                });
                self.inner.loads.fetch_add(1, Ordering::Relaxed);
                self.inner.set(STANDBY);
                Ok(())
            }
            Err(error) => {
                self.inner.set(UNLOADED);
                Err(error)
            }
        }
    }

    /// Let the model go.
    ///
    /// **After the current job, not during it.** A release blocks until a
    /// running job returns, because the alternative — taking the engine out
    /// from under a recognition in progress — would corrupt the transcript and
    /// the resume checkpoint it is writing. A caller who wants to stop *now*
    /// wants the job's `CancelToken`, which is a different thing.
    ///
    /// Returns whether there was anything to release.
    pub fn release(&self) -> bool {
        let mut loaded = self.inner.loaded.lock().expect("keeper mutex poisoned");
        let had = loaded.take().is_some();
        self.inner.set(UNLOADED);
        had
    }

    /// Transcribe, reusing a loaded model when it is the right one.
    ///
    /// The returned transcript is the caller's; the events go out on the bus
    /// the keeper was built with, exactly as [`Transcriber::transcribe`]
    /// publishes them.
    pub fn transcribe(
        &self,
        request: Request,
        job: JobId,
        cancel: &CancelToken,
    ) -> Result<Transcription, Error> {
        let identity = self.identity(&request);
        let bus = self.inner.bus.clone();

        let mut loaded = self.inner.loaded.lock().expect("keeper mutex poisoned");

        // Reuse only when the settings digest matches. Not a narrower "same
        // engine" test: `Transcriber::run` reads the VAD and guard settings and
        // the VAD model path out of the request it stored when it was loaded, so
        // reusing across a change in any of those would run the *old* settings
        // and produce a transcript for a configuration nobody asked for. That is
        // a wrong answer, not a slow one.
        if !loaded.as_ref().is_some_and(|l| l.identity == identity) {
            *loaded = None;
            self.inner.set(LOADING);

            match (self.inner.loader)(&request) {
                Ok(transcriber) => {
                    *loaded = Some(Loaded {
                        identity,
                        transcriber,
                    });
                    self.inner.loads.fetch_add(1, Ordering::Relaxed);
                }
                Err(error) => {
                    // Announced, and announced in this order. `JobStarted`
                    // first because it is what claims the job id: without it the
                    // window's ownership check discards the failure that follows
                    // and leaves the screen on "正在准备…" forever. That was a
                    // real dead end, and it is a load failure — the one error
                    // that happens before the pipeline can publish anything —
                    // that reached it.
                    self.inner.set(UNLOADED);
                    bus.publish(Event::JobStarted {
                        id: job,
                        kind: JobKind::FileTranscribe,
                    });
                    bus.publish(Event::JobFailed {
                        id: job,
                        error: (&error).into(),
                    });
                    return Err(error);
                }
            }
        }

        let entry = loaded.as_mut().expect("a model was just ensured");
        entry.transcriber.set_input(request.input.clone());
        entry
            .transcriber
            .set_cache_policy(request.cache.clone());

        self.inner.set(BUSY);
        let outcome = entry.transcriber.transcribe(job, &bus, cancel);
        self.inner.set(STANDBY);

        *self.inner.last_used.lock().expect("keeper mutex poisoned") = Instant::now();
        // Waking the sweeper now rather than letting it notice on its own
        // shortens the window in which a released model is still resident.
        let _ = self.inner.notify.send(());

        outcome
    }

    /// The settings digest of a request, computed with the ffmpeg build this
    /// keeper captured on its first use.
    fn identity(&self, request: &Request) -> String {
        let ffmpeg = self
            .inner
            .ffmpeg
            .get_or_init(verse_audio::ffmpeg_version);

        settings_digest(
            request,
            effective_threads(request),
            ffmpeg.as_deref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fixed;
    use std::path::{Path, PathBuf};
    use verse_audio::VadSettings;
    use verse_core::Transcript;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("verse-pipeline-test-keep-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn a_request(dir: &Path) -> Request {
        Request {
            input: dir.join("audio.wav"),
            models_dir: dir.to_path_buf(),
            engine: "sensevoice".to_string(),
            vad_model: dir.join("silero-vad").join("silero_vad.onnx"),
            inverse_text_normalization: true,
            vad: VadSettings::default(),
            max_output_tokens: None,
            guard: crate::GuardSettings::default(),
            hotwords: None,
            threads: None,
            cache: crate::CachePolicy::Disabled,
        }
    }

    /// A keeper whose loader hands back a fake engine, counting through the
    /// keeper's own counter.
    fn a_keeper(bus: EventBus, idle: Option<Duration>) -> ModelKeeper {
        ModelKeeper::with_loader(
            bus,
            idle,
            Box::new(|request: &Request| {
                Ok(Transcriber::with_engine(
                    request.clone(),
                    Box::new(Fixed(Some(Transcript::default()))),
                ))
            }),
        )
    }

    /// A keeper whose loader always fails, so the failure path can be driven
    /// without arranging a corrupt model.
    fn a_failing_keeper(bus: EventBus, idle: Option<Duration>) -> ModelKeeper {
        ModelKeeper::with_loader(
            bus,
            idle,
            Box::new(|_request: &Request| {
                Err(Error::new(verse_core::ErrorKind::Model, "no usable model"))
            }),
        )
    }

    fn transcribe(keeper: &ModelKeeper, request: &Request) -> Result<Transcription, Error> {
        keeper.transcribe(request.clone(), JobId(1), &CancelToken::new())
    }

    #[test]
    fn the_keeper_can_be_shared_between_threads() {
        // The window hands it to a different thread per job, and a service
        // would too. Stated as a bound rather than discovered by a caller.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ModelKeeper>();
    }

    #[test]
    fn a_second_job_reuses_the_model() {
        // The whole point. The transcriptions themselves fail — there is no
        // audio at these paths — and that is fine: what is asserted is the
        // number of times the loader ran.
        let dir = scratch("reuse");
        let keeper = a_keeper(EventBus::new(), None);
        let request = a_request(&dir);

        let _ = transcribe(&keeper, &request);
        let _ = transcribe(&keeper, &request);
        let _ = transcribe(&keeper, &request);

        assert_eq!(keeper.loads(), 1, "three jobs, one load");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_setting_loads_again() {
        // The identity test that matters. A narrower "same engine" check would
        // pass here and then run the *old* settings, because `Transcriber::run`
        // reads the VAD and guard settings out of the request it stored.
        let dir = scratch("changed");
        let keeper = a_keeper(EventBus::new(), None);

        let mut request = a_request(&dir);
        let _ = transcribe(&keeper, &request);
        assert_eq!(keeper.loads(), 1);

        request.hotwords = Some("球拍".to_string());
        let _ = transcribe(&keeper, &request);

        assert_eq!(keeper.loads(), 2, "different settings are different work");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unchanged_setting_does_not_load_again() {
        // The other half: a field that differs but does not change the digest
        // must not cost a load. `input` is the one that matters — it changes for
        // every file in a batch.
        let dir = scratch("input-only");
        let keeper = a_keeper(EventBus::new(), None);

        let mut request = a_request(&dir);
        let _ = transcribe(&keeper, &request);

        request.input = dir.join("another.wav");
        let _ = transcribe(&keeper, &request);

        assert_eq!(keeper.loads(), 1, "a different file is not a different model");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_reports_what_is_happening() {
        let dir = scratch("status");
        let keeper = a_keeper(EventBus::new(), None);
        assert_eq!(keeper.status(), ModelStatus::Unloaded);

        let request = a_request(&dir);
        keeper.preload(&request).expect("preload");
        assert_eq!(keeper.status(), ModelStatus::Standby);

        assert!(keeper.release());
        assert_eq!(keeper.status(), ModelStatus::Unloaded);
        assert!(!keeper.release(), "releasing nothing reports nothing");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preload_reads_the_weights_without_a_job() {
        let dir = scratch("preload");
        let keeper = a_keeper(EventBus::new(), None);
        let request = a_request(&dir);

        keeper.preload(&request).expect("preload");
        assert_eq!(keeper.loads(), 1);

        // And a job afterwards does not load again.
        let _ = transcribe(&keeper, &request);
        assert_eq!(keeper.loads(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preloading_twice_loads_once() {
        let dir = scratch("preload-twice");
        let keeper = a_keeper(EventBus::new(), None);
        let request = a_request(&dir);

        keeper.preload(&request).expect("preload");
        keeper.preload(&request).expect("preload");

        assert_eq!(keeper.loads(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_idle_model_is_released() {
        let dir = scratch("idle");
        let keeper = a_keeper(EventBus::new(), Some(Duration::from_millis(50)));
        let request = a_request(&dir);

        keeper.preload(&request).expect("preload");
        assert_eq!(keeper.status(), ModelStatus::Standby);

        // The sweeper wakes on its own interval, so allow a few of them — and
        // give up eventually rather than hanging the suite.
        let deadline = Instant::now() + Duration::from_secs(5);
        while keeper.status() != ModelStatus::Unloaded && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }

        assert_eq!(keeper.status(), ModelStatus::Unloaded, "an idle model should go");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_load_announces_the_job_before_the_failure() {
        // The order is load-bearing, not cosmetic. A job id is claimed by
        // `JobStarted`; the window's screen ignores output for a job it has not
        // claimed. Publishing the failure alone left it on "正在准备…" forever,
        // with a cancel button for a job that was never running.
        let bus = EventBus::new();
        let subscription = bus.subscribe_all();
        let keeper = a_failing_keeper(bus, None);

        let outcome = transcribe(&keeper, &a_request(&scratch("failed-load")));

        assert!(outcome.is_err());
        let events: Vec<&str> = subscription
            .drain()
            .iter()
            .map(|event| match &**event {
                Event::JobStarted { .. } => "started",
                Event::JobFailed { .. } => "failed",
                _ => "other",
            })
            .collect();

        assert_eq!(events, ["started", "failed"]);
        assert_eq!(keeper.status(), ModelStatus::Unloaded);
        assert_eq!(keeper.loads(), 0, "a failed load is not a load");
    }

    #[test]
    fn two_callers_at_once_load_once() {
        // One model, one job at a time. Two threads asking together must not
        // each read a gigabyte of weights.
        let dir = scratch("concurrent");
        let keeper = a_keeper(EventBus::new(), None);
        let request = a_request(&dir);

        std::thread::scope(|scope| {
            for _ in 0..4 {
                let keeper = &keeper;
                let request = &request;
                scope.spawn(move || {
                    let _ = keeper.transcribe(request.clone(), JobId(1), &CancelToken::new());
                });
            }
        });

        assert_eq!(keeper.loads(), 1, "four callers, one model");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dropping_the_keeper_stops_its_sweeper() {
        // `Inner::drop` joins the thread, so returning from this test at all is
        // the assertion — a sweeper that never stopped would hang the suite
        // rather than fail it, which is why it is worth having a test that
        // simply gets to the end.
        let dir = scratch("drop");
        let request = a_request(&dir);

        for _ in 0..3 {
            let keeper = a_keeper(EventBus::new(), Some(Duration::from_millis(10)));
            keeper.preload(&request).expect("preload");
            drop(keeper);
        }

        // And a keeper with no timeout owns no thread to stop.
        let keeper = a_keeper(EventBus::new(), None);
        keeper.preload(&request).expect("preload");
        drop(keeper);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
