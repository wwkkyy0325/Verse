//! Getting a job run, and its progress to the window.
//!
//! Two directions, and they are not symmetrical. Work goes one way: a command
//! starts a thread, the thread runs the pipeline, the pipeline publishes to
//! the bus. State comes back the other way: one thread drains the bus into
//! `AppState` and tells the window what changed.
//!
//! The window never drives the pipeline and the pipeline never touches the
//! window. Everything crossing between them is in this file.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use verse_core::{CancelToken, EventBus, JobId, Segment, Subscription, Transcript};
use verse_pipeline::{ModelKeeper, Request, DEFAULT_IDLE};
use crate::state::{AppState, Applied, FileEntry, Recovery, Screen};

/// The event name the window listens on.
pub const UPDATE: &str = "verse://update";

/// How long the forwarding thread waits before looking around.
///
/// Only there so the thread can notice the application going away; a shorter
/// wait would burn wakeups for nothing.
const POLL: Duration = Duration::from_millis(250);

/// The application's shared state.
pub struct App {
    pub state: Mutex<AppState>,
    pub bus: EventBus,
    /// The model, held between jobs.
    ///
    /// Here rather than in the worker thread, because a thread that ends with
    /// its job cannot hold anything across jobs — which is what the window used
    /// to do, at a cost of 228 MB re-read per file.
    pub keeper: ModelKeeper,
    /// Set while a job is running, so it can be stopped.
    job: Mutex<Option<RunningJob>>,
    next_job: AtomicU64,
}

struct RunningJob {
    id: JobId,
    cancel: CancelToken,
}

impl App {
    pub fn new() -> Self {
        let bus = EventBus::new();

        Self {
            state: Mutex::new(AppState::new()),
            keeper: ModelKeeper::with_timeout(bus.clone(), idle_timeout()),
            bus,
            job: Mutex::new(None),
            // Jobs are numbered from one; zero is left free so a default-initialised
            // id can never match a real one.
            next_job: AtomicU64::new(1),
        }
    }
}

/// How long the window keeps a model after the last job.
///
/// `VERSE_MODEL_IDLE_SECS` overrides [`DEFAULT_IDLE`]; `0` pins the model for
/// the life of the process. An unparseable value falls back **and says so** —
/// silently ignoring a switch somebody set is worse than refusing it, because
/// they will conclude the switch does not work.
fn idle_timeout() -> Option<Duration> {
    let Some(raw) = std::env::var_os("VERSE_MODEL_IDLE_SECS") else {
        return Some(DEFAULT_IDLE);
    };
    if raw.is_empty() {
        return Some(DEFAULT_IDLE);
    }

    let text = raw.to_string_lossy().into_owned();
    match text.trim().parse::<u64>() {
        Ok(0) => None,
        Ok(seconds) => Some(Duration::from_secs(seconds)),
        Err(_) => {
            eprintln!(
                "warning: VERSE_MODEL_IDLE_SECS={text:?} is not a number of seconds; \
                 using {}",
                DEFAULT_IDLE.as_secs()
            );
            Some(DEFAULT_IDLE)
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// The next job id.
    ///
    /// One place hands these out, so a caller that is not starting a real job
    /// — the window's demonstration — cannot collide with one that is.
    pub(crate) fn claim_id(&self) -> JobId {
        JobId(self.next_job.fetch_add(1, Ordering::Relaxed))
    }
}

/// Release the job slot, if it still belongs to `job`.
///
/// Guarded rather than unconditional: a cancelled job's worker can finish
/// after the next one has already claimed the slot, and clearing it then
/// would leave the newer job uncancellable.
fn clear_job(shared: &App, job: JobId) {
    let mut running = shared.job.lock().expect("job mutex poisoned");
    if running.as_ref().is_some_and(|r| r.id == job) {
        *running = None;
    }
}

// ---------------------------------------------------------------- to the window

/// What the window is told when something changes.
///
/// Sent as an increment rather than a snapshot: a two-hour recording is a few
/// thousand segments, and resending the whole list to move one progress bar
/// is not affordable. The window keeps its own copy and applies these.
// `rename_all` on the enum renames the *variants*; the per-variant attribute
// is what renames their fields. Without it the tag reads `segment` while the
// numbers arrive as `start_ms`, which compiles on both sides and shows up as
// undefined timestamps in the window.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Update {
    /// The screen changed. The window replaces what it is showing.
    Screen { screen: ScreenView },
    /// The list of files changed, and possibly which one is shown.
    ///
    /// Sent whenever a file's state moves, including files that are not on
    /// screen — the list carries every file's state, so a background file
    /// finishing is still something to show.
    #[serde(rename_all = "camelCase")]
    Roster {
        entries: Vec<EntryView>,
        selected: Option<usize>,
    },
    /// A different file now owns the pane.
    ///
    /// Carries the transcript as well as the screen because the window holds
    /// one segment list: switching files replaces it rather than appending to
    /// whatever the previous file had accumulated.
    #[serde(rename_all = "camelCase")]
    Selected {
        screen: ScreenView,
        segments: Vec<SegmentView>,
    },
    /// One more segment was recognised.
    Segment(SegmentView),
    /// The job moved forward without the screen changing.
    #[serde(rename_all = "camelCase")]
    Progress {
        elapsed_ms: u64,
        /// `null` when the file's length is not known. A bar with no number in
        /// it is honest; a bar at zero is not.
        fraction: Option<f32>,
    },
    /// A new job is starting; drop everything from the previous one.
    Cleared,
    /// A model download moved. Sent often, so it carries only the download
    /// and not the whole screen — plus which model, because the panel has one
    /// card per model and each shows its own.
    Download {
        model: String,
        download: DownloadView,
    },
    /// Everything at once.
    ///
    /// The increments above are for the bus, which fires often enough that
    /// resending the whole state for each one would be wasteful. This is for
    /// the moments where there is no increment to send: the first paint, and
    /// anything a command did on its own thread.
    Snapshot { state: StateView },
}

/// The whole window's worth of state, as the frontend reads it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateView {
    pub screen: ScreenView,
    pub entries: Vec<EntryView>,
    pub selected: Option<usize>,
    /// The segments of whichever file is being shown, so a window that has
    /// just opened or just switched files has the transcript, not an empty
    /// pane waiting for the next event.
    pub segments: Vec<SegmentView>,
}

impl StateView {
    pub fn of(state: &AppState) -> Self {
        Self {
            screen: view_of(state.screen()),
            entries: state.files().iter().map(EntryView::of).collect(),
            selected: state.selected(),
            segments: state.segments().iter().map(SegmentView::from).collect(),
        }
    }
}

/// One segment, as the window reads it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentView {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

impl From<&Segment> for SegmentView {
    fn from(segment: &Segment) -> Self {
        Self {
            start_ms: segment.start.as_millis() as u64,
            end_ms: segment.end.as_millis() as u64,
            text: segment.text.clone(),
        }
    }
}

/// A file the window will not hand over, and why.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Refusal {
    /// The file's name, not its path — a path is too long to show and never
    /// what tells two files apart.
    pub file: String,
    /// A sentence naming what is wrong, written for the person who dragged it.
    pub reason: String,
}

/// What came of looking at a drop or a selection before starting anything.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    /// The paths that can be processed, in the order they arrived.
    pub usable: Vec<String>,
    pub refused: Vec<Refusal>,
    /// What this build does accept, so the dialog can name the formats rather
    /// than leaving somebody to guess. The same list the command line expands
    /// a directory with.
    pub accepted: Vec<String>,
}

/// Whether a path is something to refuse, before any work starts.
///
/// The window checks before it starts; the command line does not, and the
/// difference is deliberate. A caller that typed a path gets ffmpeg's verdict,
/// which is the only correct one — a decodable file with an odd name must not
/// become a usage error. A person who drags a folder onto a window should be
/// told so at once, rather than watching a progress bar and then reading a
/// decode failure.
pub fn refusal_for(path: &std::path::Path) -> Option<Refusal> {
    let file = file_label(path);

    if path.is_dir() {
        return Some(Refusal {
            file,
            reason: "这是一个文件夹，不是文件。请打开它，选中里面的音频文件。".to_string(),
        });
    }

    if !path.exists() {
        return Some(Refusal {
            file,
            reason: "找不到这个文件。".to_string(),
        });
    }

    if !verse_core::looks_like_audio(path) {
        return Some(Refusal {
            file,
            reason: "看起来不是音频或视频文件。".to_string(),
        });
    }

    None
}

/// One model, as the panel shows it.
///
/// Carries what a person needs to choose rather than what the downloader
/// needs: a name, a sentence, a size, and whether it is already here.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChoice {
    pub id: String,
    /// The catalogue's own `display_name`, verbatim.
    ///
    /// Verbatim matters: the FunASR licence requires the name be retained as
    /// upstream spells it, so this is passed through rather than tidied.
    pub name: String,
    /// `null` for a catalogue written before descriptions existed.
    pub description: Option<String>,
    /// Whether every file is on disk at its expected size.
    pub present: bool,
    /// What the download would be, from the catalogue's declared sizes. Not
    /// what is on disk — the panel is telling a person what choosing this
    /// costs, and that is the same number before and after fetching it.
    pub bytes: u64,
    /// The engine used when the user expresses no preference. Sent rather than
    /// hardcoded in the window, so the default is decided in one place.
    pub default: bool,
}

/// One row of the file list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryView {
    /// The file's own name, not its path — a full path is too long to show and
    /// never what the user needs to read.
    pub name: String,
    pub state: EntryState,
    /// Which engine this file was handed to.
    ///
    /// The engine can be changed between files, so a list of results is not
    /// necessarily a list from one recogniser. Saying which is which is the
    /// difference between a mixed list and a misleading one.
    pub engine: String,
    /// Whether there is a file to delete for this row.
    ///
    /// Carried rather than inferred from the state, because a finished
    /// transcript whose automatic save was refused has no file — and offering
    /// to delete it would offer something that cannot be done.
    pub has_result: bool,
}

/// What a row is doing, as a word rather than a screen.
///
/// The list does not need the whole screen of a file it is not showing; it
/// needs to know which of five things the row is doing, so it can say so in a
/// few characters.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryState {
    Queued,
    Working,
    Done,
    Failed,
    NeedsModel,
}

impl EntryView {
    fn of(entry: &FileEntry) -> Self {
        Self {
            name: entry.label(),
            engine: entry.engine.clone(),
            has_result: matches!(&entry.screen, Screen::Done(done) if done.exported.is_some()),
            state: match entry.screen {
                Screen::Queued { .. } => EntryState::Queued,
                Screen::Working(_) => EntryState::Working,
                Screen::Done(_) => EntryState::Done,
                Screen::Failed(_) => EntryState::Failed,
                Screen::NeedsModel { .. } => EntryState::NeedsModel,
                Screen::Empty => EntryState::Queued,
            },
        }
    }
}

/// How a model download is going, in the form the window reads.
///
/// Bytes rather than a bare "downloading": a progress bar needs a number, and
/// a state with nothing in it leaves the window spinning.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum DownloadView {
    Idle,
    // Per-variant, and it has to be on every variant that has a multi-word
    // field: `rename_all` on the enum renames the *variants*, not their fields.
    // Without this the window reads `download.receivedBytes`, finds nothing,
    // and shows a progress bar stuck at zero with "NaN MB" beside it — which
    // is exactly what it did.
    #[serde(rename_all = "camelCase")]
    Fetching {
        file: String,
        received_bytes: u64,
        /// `null` when the host did not say how large the file is.
        total_bytes: Option<u64>,
        /// The same two numbers for the *model*, not the file in flight.
        ///
        /// A model is several files and the downloader pulls them one at a
        /// time, so its own figures fill up once per file: Qwen3-ASR showed
        /// "44 MB" and looked finished while it was on its first of five. A
        /// person who chose a 987 MB model is watching the model.
        ///
        /// `null` when the reading does not know — the state carries one file's
        /// worth and has no catalogue to add the rest up from.
        model_received_bytes: Option<u64>,
        model_total_bytes: Option<u64>,
    },
    Verifying,
    Ready,
    Failed { reason: String },
}

impl From<&verse_model::DownloadState> for DownloadView {
    fn from(state: &verse_model::DownloadState) -> Self {
        use verse_model::DownloadState;

        match state {
            DownloadState::Idle => DownloadView::Idle,
            DownloadState::Fetching {
                file,
                received,
                total,
                ..
            } => DownloadView::Fetching {
                file: file.clone(),
                received_bytes: *received,
                total_bytes: *total,
                // Not knowable here: this conversion sees one file's state and
                // has no catalogue. The caller that does adds them.
                model_received_bytes: None,
                model_total_bytes: None,
            },
            DownloadState::Verifying => DownloadView::Verifying,
            DownloadState::Ready => DownloadView::Ready,
            DownloadState::Failed { reason } => DownloadView::Failed {
                reason: reason.clone(),
            },
        }
    }
}

/// Which screen, and what it needs to say.
///
/// Only what the window cannot work out for itself: file names rather than
/// paths, because the full path is usually too long to show and never what the
/// user needs to read.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ScreenView {
    Empty,
    /// Waiting for the job slot.
    Queued { file: String },
    NeedsModel {
        file: String,
        /// The engine's *id*, not its display name.
        ///
        /// This carried the display name's slot while actually holding the id,
        /// so the window rendered `sensevoice` at a person. The window has the
        /// model list and resolves the name from it; sending the id and saying
        /// so is the honest version.
        model_id: String,
        download: DownloadView,
    },
    #[serde(rename_all = "camelCase")]
    Working {
        file: String,
        /// True once the user has asked to stop and the job has not yet.
        stopping: bool,
        /// Carried on the screen as well as sent incrementally, so that
        /// switching to a running file shows its bar where it actually is
        /// rather than at zero until the next tick.
        elapsed_ms: u64,
        fraction: Option<f32>,
    },
    #[serde(rename_all = "camelCase")]
    Done {
        file: String,
        /// Where the transcript was written, once it has been. The window uses
        /// it to stop offering an export that has already happened.
        exported: Option<String>,
        /// Why it was not written automatically, when it was not.
        ///
        /// The transcript is on screen either way and *save as* still works —
        /// this says where the result did not go, which is the one thing the
        /// window cannot work out for itself.
        save_error: Option<String>,
    },
    Failed {
        file: String,
        reason: String,
        recovery: RecoveryView,
    },
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RecoveryView {
    Retry,
    GetModel,
    PickAnotherFile,
}

impl From<Recovery> for RecoveryView {
    fn from(recovery: Recovery) -> Self {
        match recovery {
            Recovery::Retry => RecoveryView::Retry,
            Recovery::GetModel => RecoveryView::GetModel,
            Recovery::PickAnotherFile => RecoveryView::PickAnotherFile,
        }
    }
}

/// The file's name, or the whole path if it has none.
fn file_label(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

pub fn view_of(screen: &Screen) -> ScreenView {
    match screen {
        Screen::Empty => ScreenView::Empty,
        Screen::Queued { input } => ScreenView::Queued {
            file: file_label(input),
        },
        Screen::NeedsModel {
            input,
            model,
            download,
        } => ScreenView::NeedsModel {
            file: file_label(input),
            model_id: model.clone(),
            download: download.into(),
        },
        Screen::Working(working) => ScreenView::Working {
            file: file_label(&working.input),
            stopping: working.stopping,
            elapsed_ms: working.position.as_millis() as u64,
            fraction: working.fraction,
        },
        Screen::Done(done) => ScreenView::Done {
            file: file_label(&done.input),
            exported: done.exported.as_ref().map(|path| file_label(path)),
            save_error: done.save_error.clone(),
        },
        Screen::Failed(failed) => ScreenView::Failed {
            file: file_label(&failed.input),
            reason: failed.reason.clone(),
            recovery: failed.recovery.into(),
        },
    }
}

/// Tell the window what the state currently looks like.
///
/// Used after anything that moves the state outside the bus — a command, or
/// the initial load. It sends everything rather than an increment because a
/// command may have moved the list, the selection and the screen together, and
/// three updates where one will do is three chances to be out of order.
pub fn push_state(app: &AppHandle) {
    let app_state = app.state::<App>();
    let state = app_state.state.lock().expect("state mutex poisoned");
    let _ = app.emit(
        UPDATE,
        Update::Snapshot {
            state: StateView::of(&state),
        },
    );

    // What is unfinished, kept in step here because this is the one place every
    // move outside the bus already passes through. Written unconditionally
    // rather than compared against the last write first: this runs on commands
    // and on job boundaries — a dozen times in a session — and not on the
    // progress path, which is the one that fires often enough to care.
    let _ = crate::settings::Pending::of(state.model().to_string(), state.unfinished())
        .save(&crate::pending_path());
}

/// Start the thread that turns bus events into window updates.
///
/// Runs for the life of the process. There is nothing to shut down cleanly:
/// the thread holds a subscription and an `AppHandle`, and when the process
/// exits both go with it.
pub fn spawn_forwarder(app: AppHandle, subscription: Subscription) {
    std::thread::spawn(move || loop {
        let Some(event) = subscription.recv_timeout(POLL) else {
            continue;
        };

        let (update, follow, unsaved) = {
            let app_state = app.state::<App>();
            let mut state = app_state.state.lock().expect("state mutex poisoned");

            // One match rather than two: `Applied::Segment` carries the segment
            // by value, so matching twice would move out of it the first time.
            //
            // Three slots because two of the arms have two things to say: a
            // change to a shown file moves both the pane and its row, and
            // switching files moves the pane and the selection. Slot order is
            // send order.
            match state.apply(&event) {
                Applied::Nothing => (None, None, None),

                Applied::Screen => {
                    let update = Some(Update::Screen {
                        screen: view_of(state.screen()),
                    });
                    // The row for the file being shown carries its state, so a
                    // file that just finished is a row that just changed.
                    let follow = Some(roster_of(&state));
                    // Taken here and written *below*, deliberately: the write
                    // happens outside the state lock, and a save into a
                    // synchronised Documents folder can take long enough to
                    // notice.
                    (update, follow, state.awaiting_save())
                }

                Applied::Roster => (Some(roster_of(&state)), None, state.awaiting_save()),

                Applied::Selected => (
                    Some(Update::Selected {
                        screen: view_of(state.screen()),
                        segments: segments_of(&state),
                    }),
                    Some(roster_of(&state)),
                    None,
                ),

                Applied::Segment(segment) => (
                    Some(Update::Segment(SegmentView::from(&segment))),
                    None,
                    None,
                ),

                Applied::Cleared => (Some(Update::Cleared), None, None),

                Applied::Progress { position, fraction } => (
                    Some(Update::Progress {
                        elapsed_ms: position.as_millis() as u64,
                        fraction,
                    }),
                    None,
                    None,
                ),
            }
        };

        // A failed emit means the window is gone; there is nothing to do about
        // it and nothing worth logging.
        if let Some(update) = update {
            let _ = app.emit(UPDATE, update);
        }
        if let Some(follow) = follow {
            let _ = app.emit(UPDATE, follow);
        }

        if let Some((index, input, transcript)) = unsaved {
            autosave(&app, index, &input, &transcript);
        }
    });
}

/// The list, as the window reads it.
fn roster_of(state: &AppState) -> Update {
    Update::Roster {
        entries: state.files().iter().map(EntryView::of).collect(),
        selected: state.selected(),
    }
}

/// The segments of the file being shown.
fn segments_of(state: &AppState) -> Vec<SegmentView> {
    state.segments().iter().map(SegmentView::from).collect()
}

/// Write a finished transcript out, and tell the window what happened.
///
/// Runs on the forwarding thread, immediately after `TranscriptFinal` has been
/// applied. That ordering is the reason it is here rather than on the worker
/// that did the recognition: the worker would race the forwarder, and the
/// screen would end up recording a save that had not happened yet, or losing
/// one that had.
///
/// By index rather than by screen: the file that finished is not necessarily
/// the one on screen, and the user may click elsewhere while the write is in
/// flight.
fn autosave(app: &AppHandle, index: usize, input: &std::path::Path, transcript: &Transcript) {
    let roots = verse_store::Roots::from_env();
    let rendered = verse_core::ExportFormat::Srt.render(transcript);

    let outcome = crate::autosave::save_into(
        &verse_store::output_dir(&roots),
        &verse_store::data_dir(&roots).join("outputs.json"),
        input,
        &rendered,
    );

    let engine = {
        let app_state = app.state::<App>();
        let mut state = app_state.state.lock().expect("state mutex poisoned");

        let engine = state
            .files()
            .get(index)
            .map(|entry| entry.engine.clone())
            .unwrap_or_default();

        match &outcome {
            crate::autosave::Saved::Written(path) => state.note_exported(index, path.clone()),
            crate::autosave::Saved::Refused(why) => state.note_save_failed(index, why.clone()),
        }

        engine
    };

    // So the next launch can find this. Only when it was actually written:
    // an entry pointing at a file that is not there is not history, and
    // `History::prune` would drop it on the next read anyway.
    if let crate::autosave::Saved::Written(path) = &outcome {
        let history_path = verse_store::data_dir(&roots).join("history.json");
        let mut history = verse_store::History::load(&history_path);

        history.record(verse_store::Past {
            input: input.to_path_buf(),
            output: path.clone(),
            engine,
            finished_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_millis() as u64)
                .unwrap_or(0),
        });

        // Best effort, like the transcript itself: the result is on disk and
        // the record of it is a convenience.
        let _ = history.save(&history_path);
    }

    // So the screen gains the path or the reason. Without this the window would
    // show the transcript and never mention either.
    push_state(app);
}

// ---------------------------------------------------------------- from the window

/// Start transcribing a file.
///
/// Returns as soon as the work is under way — the outcome arrives on the bus
/// like everything else.
pub fn start(app: &AppHandle, input: std::path::PathBuf, models_dir: std::path::PathBuf) {
    let (job, cancel) = {
        let app_state = app.state::<App>();
        let id = app_state.claim_id();
        let cancel = CancelToken::new();

        let mut running = app_state.job.lock().expect("job mutex poisoned");
        if let Some(previous) = running.replace(RunningJob {
            id,
            cancel: cancel.clone(),
        }) {
            // Should be unreachable: the interface does not offer a second job
            // while one is running. Cancelling anyway keeps a bug in the front
            // end from leaving two engines competing for the same cores.
            previous.cancel.cancel();
        }

        (id, cancel)
    };

    let handle = app.clone();
    std::thread::spawn(move || {
        // Held for the thread's life. Bindings are kept explicit rather than
        // chained, so the guards drop in a known order against it.
        let shared = handle.state::<App>();

        let engine = {
            let state = shared.state.lock().expect("state mutex poisoned");
            state.model().to_string()
        };

        let request = Request {
            vad_model: Request::vad_for(&models_dir),
            input,
            models_dir,
            engine,
            // The window shows what a person would write.
            inverse_text_normalization: true,
            vad: verse_audio::VadSettings::default(),
            max_output_tokens: None,
            guard: verse_pipeline::GuardSettings::default(),
            // The window offers no vocabulary; the plan defers its screens.
            hotwords: None,
            threads: None,
            // The window uses the cache, so dropping a file that has been
            // transcribed before returns it immediately. There is no switch
            // for it here: nothing in the interface suggests a reason a person
            // would want to wait twice.
            cache: verse_pipeline::CachePolicy::under(&verse_store::data_dir(
                &verse_store::Roots::from_env(),
            )),
        };

        // The keeper owns loading, and it announces a load failure itself —
        // including the `JobStarted` that claims the job id. This thread used
        // to publish the failure on its own, without that claim, and the screen
        // discarded it: a model present at the right size but unusable left the
        // window on "正在准备…" for good.
        //
        // Everything else publishes its own outcome.
        let _ = shared.keeper.transcribe(request, job, &cancel);

        clear_job(&shared, job);
    });
}

/// Ask the running job to stop.
///
/// The screen does not return to the drop target here. It waits for
/// `JobCancelled`, so the window never shows an idle screen while a job is
/// still winding down.
pub fn cancel(app: &AppHandle) {
    let shared = app.state::<App>();
    let running = shared.job.lock().expect("job mutex poisoned");

    // The slot is left occupied. The job is still winding down, and the worker
    // is what clears it once the pipeline actually returns.
    if let Some(running) = running.as_ref() {
        running.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use serde_json::json;

    use crate::state::{Done, Failed, Working};

    /// These pin the wire format the TypeScript in `ui/src/lib/api.ts` reads.
    ///
    /// Nothing else connects the two halves: a renamed field would compile on
    /// both sides and fail only at runtime, in a window, as a blank screen.
    /// Serde's renames are the contract, so they get tested like one.

    #[test]
    fn an_empty_screen_serializes_as_the_frontend_expects() {
        let value = serde_json::to_value(Update::Screen {
            screen: ScreenView::Empty,
        })
        .expect("serializes");

        assert_eq!(value, json!({ "kind": "screen", "screen": { "kind": "empty" } }));
    }

    #[test]
    fn a_working_screen_carries_the_file_name_the_flag_and_where_the_bar_is() {
        let value = serde_json::to_value(Update::Screen {
            screen: ScreenView::Working {
                file: "会议录音.m4a".to_string(),
                stopping: true,
                elapsed_ms: 12_500,
                // A half, because it is exact in binary. The fraction is an
                // `f32` and JSON numbers are `f64`, so a tenth would arrive
                // widened to 0.10000000149011612 — harmless for a bar, but it
                // would make this assertion about floating point rather than
                // about the field name it is testing.
                fraction: Some(0.5),
            },
        })
        .expect("serializes");

        assert_eq!(
            value,
            json!({
                "kind": "screen",
                "screen": { "kind": "working", "file": "会议录音.m4a", "stopping": true,
                            "elapsedMs": 12_500, "fraction": 0.5 }
            })
        );
    }

    #[test]
    fn a_working_screen_with_no_known_length_sends_a_null_fraction() {
        // `null` and `0.0` have to be distinguishable on the wire: one means
        // "no number is known" and the other means "at the beginning". The
        // window renders them differently — an indeterminate bar against one
        // sitting at zero — so a collapsed distinction would be a lie the
        // frontend could not detect.
        let value = serde_json::to_value(Update::Progress {
            elapsed_ms: 0,
            fraction: None,
        })
        .expect("serializes");

        assert_eq!(
            value,
            json!({ "kind": "progress", "elapsedMs": 0, "fraction": null })
        );
    }

    #[test]
    fn a_segment_uses_camel_case_milliseconds() {
        let value = serde_json::to_value(Update::Segment(SegmentView {
            start_ms: 1500,
            end_ms: 3200,
            text: "开放时间".to_string(),
        }))
        .expect("serializes");

        assert_eq!(
            value,
            json!({ "kind": "segment", "startMs": 1500, "endMs": 3200, "text": "开放时间" })
        );
    }

    #[test]
    fn a_download_sends_camel_case_byte_counts() {
        // The test that was missing, and its absence had a cost. The window
        // reads `download.receivedBytes`; without a per-variant `rename_all`
        // the field arrived as `received_bytes`, nothing on either side
        // complained, and the progress bar sat at zero saying "NaN MB".
        //
        // `rename_all` on an enum renames its variants. It does not touch
        // their fields, which is the whole trap.
        let value = serde_json::to_value(Update::Download {
            model: "sensevoice".to_string(),
            download: DownloadView::Fetching {
                file: "model.onnx".to_string(),
                received_bytes: 1024,
                total_bytes: Some(2048),
                model_received_bytes: Some(1024),
                model_total_bytes: Some(4096),
            },
        })
        .expect("serializes");

        let download = &value["download"];
        assert!(download.get("receivedBytes").is_some(), "got: {value}");
        assert!(download.get("totalBytes").is_some(), "got: {value}");
        assert!(
            download.get("received_bytes").is_none(),
            "snake_case reached the window: {value}"
        );
        // And the variant name itself, which the enum-level rename does cover.
        assert_eq!(download["state"], json!("fetching"));
    }

    #[test]
    fn every_multi_word_field_on_the_wire_is_camel_case() {
        // A guard against the same mistake on the next field somebody adds.
        // Serialises one of everything and looks for an underscore in a key.
        fn keys_with_underscores(value: &serde_json::Value, found: &mut Vec<String>) {
            match value {
                serde_json::Value::Object(map) => {
                    for (key, nested) in map {
                        if key.contains('_') {
                            found.push(key.clone());
                        }
                        keys_with_underscores(nested, found);
                    }
                }
                serde_json::Value::Array(items) => {
                    for item in items {
                        keys_with_underscores(item, found);
                    }
                }
                _ => {}
            }
        }

        let samples = vec![
            Update::Download {
                model: "sensevoice".to_string(),
                download: DownloadView::Fetching {
                    file: "a.onnx".to_string(),
                    received_bytes: 1,
                    total_bytes: None,
                    model_received_bytes: None,
                    model_total_bytes: None,
                },
            },
            Update::Download {
                model: "sensevoice".to_string(),
                download: DownloadView::Ready,
            },
            Update::Screen {
                screen: ScreenView::Done {
                    file: "a.wav".to_string(),
                    exported: Some("a.srt".to_string()),
                    save_error: Some("why not".to_string()),
                },
            },
            Update::Screen {
                screen: ScreenView::Working {
                    file: "a.wav".to_string(),
                    stopping: true,
                    elapsed_ms: 1,
                    fraction: Some(0.5),
                },
            },
            Update::Screen {
                screen: ScreenView::Queued {
                    file: "a.wav".to_string(),
                },
            },
            Update::Roster {
                entries: vec![EntryView {
                    name: "a.wav".to_string(),
                    state: EntryState::Working,
                    engine: "sensevoice".to_string(),
                    has_result: false,
                }],
                selected: Some(0),
            },
            Update::Selected {
                screen: ScreenView::Empty,
                segments: vec![SegmentView {
                    start_ms: 0,
                    end_ms: 1,
                    text: "x".to_string(),
                }],
            },
            Update::Snapshot {
                state: StateView {
                    screen: ScreenView::Empty,
                    entries: vec![EntryView {
                        name: "a.wav".to_string(),
                        state: EntryState::Done,
                        engine: "sensevoice".to_string(),
                        has_result: true,
                    }],
                    selected: None,
                    segments: Vec::new(),
                },
            },
            Update::Segment(SegmentView {
                start_ms: 0,
                end_ms: 1,
                text: "x".to_string(),
            }),
            Update::Progress {
                elapsed_ms: 1,
                fraction: None,
            },
            Update::Cleared,
        ];

        let mut found = Vec::new();
        for sample in samples {
            keys_with_underscores(&serde_json::to_value(sample).expect("serializes"), &mut found);
        }

        // A command return value rather than a bus update, reaching the same
        // window through the same serde. Leaving it out of this sweep is how a
        // `model_id` arrives as `model_id`.
        keys_with_underscores(
            &serde_json::to_value(ModelChoice {
                id: "sensevoice".to_string(),
                name: "SenseVoice-Small".to_string(),
                description: Some("d".to_string()),
                present: true,
                bytes: 1,
                default: true,
            })
            .expect("serializes"),
            &mut found,
        );

        assert!(found.is_empty(), "snake_case keys on the wire: {found:?}");
    }

    #[test]
    fn a_done_screen_reports_whether_it_has_been_written_out() {
        // The window uses this to stop offering an export that has already
        // happened, so `null` and a path have to be distinguishable.
        let unsaved = serde_json::to_value(Update::Screen {
            screen: ScreenView::Done {
                file: "会议.m4a".to_string(),
                exported: None,
                save_error: None,
            },
        })
        .expect("serializes");

        assert_eq!(
            unsaved,
            json!({ "kind": "screen", "screen": {
                "kind": "done", "file": "会议.m4a", "exported": null, "saveError": null
            } })
        );

        let saved = serde_json::to_value(Update::Screen {
            screen: ScreenView::Done {
                file: "会议.m4a".to_string(),
                exported: Some("会议.srt".to_string()),
                save_error: None,
            },
        })
        .expect("serializes");

        assert_eq!(saved["screen"]["exported"], json!("会议.srt"));
    }

    #[test]
    fn a_cleared_update_carries_nothing_but_its_name() {
        // Emitted when a job is replaced, and again when the guard discards a
        // transcript it has already published. A frontend that does not
        // recognise it would keep showing the discarded segments and append
        // the replacement to them.
        let value = serde_json::to_value(Update::Cleared).expect("serializes");

        assert_eq!(value, json!({ "kind": "cleared" }));
    }

    #[test]
    fn a_failure_carries_its_recovery_as_a_lower_case_name() {
        let value = serde_json::to_value(Update::Screen {
            screen: ScreenView::Failed {
                file: "a.wav".to_string(),
                reason: "无法读取这个文件".to_string(),
                recovery: RecoveryView::PickAnotherFile,
            },
        })
        .expect("serializes");

        assert_eq!(
            value,
            json!({
                "kind": "screen",
                "screen": {
                    "kind": "failed",
                    "file": "a.wav",
                    "reason": "无法读取这个文件",
                    "recovery": "pickAnotherFile"
                }
            })
        );
    }

    #[test]
    fn only_the_file_name_is_shown_never_the_whole_path() {
        let working = Working {
            input: PathBuf::from(r"C:\Users\someone\录音\第三季度会议.m4a"),
            position: Default::default(),
            fraction: None,
            segments: Vec::new(),
            stopping: false,
        };

        match view_of(&Screen::Working(working)) {
            ScreenView::Working { file, .. } => assert_eq!(file, "第三季度会议.m4a"),
            other => panic!("expected a working screen, got {other:?}"),
        }
    }

    #[test]
    fn every_screen_becomes_a_view() {
        // Not a test of behaviour so much as of completeness: a new variant
        // that forgets its view stops compiling here.
        let screens = [
            Screen::Empty,
            Screen::Working(Working {
                input: PathBuf::from("a.wav"),
                position: Default::default(),
                fraction: None,
                segments: Vec::new(),
                stopping: false,
            }),
            Screen::Done(Done {
                input: PathBuf::from("a.wav"),
                transcript: Default::default(),
                exported: None,
                save_error: None,
            }),
            Screen::Failed(Failed {
                input: PathBuf::from("a.wav"),
                reason: "x".to_string(),
                recovery: Recovery::Retry,
            }),
        ];

        let serialized: Vec<_> = screens
            .iter()
            .map(|screen| serde_json::to_value(view_of(screen)).expect("serializes"))
            .collect();

        assert_eq!(serialized[0], json!({ "kind": "empty" }));
        assert_eq!(serialized[1]["kind"], json!("working"));
        assert_eq!(serialized[2]["kind"], json!("done"));
        assert_eq!(serialized[3]["kind"], json!("failed"));
    }
}
