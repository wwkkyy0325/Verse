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

use verse_core::{CancelToken, Event, EventBus, JobId, Segment, Subscription};
use verse_pipeline::{Request, Transcriber};
use crate::state::{AppState, Applied, Recovery, Screen};

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
        Self {
            state: Mutex::new(AppState::new()),
            bus: EventBus::new(),
            job: Mutex::new(None),
            // Jobs are numbered from one; zero is left free so a default-initialised
            // id can never match a real one.
            next_job: AtomicU64::new(1),
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
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
    /// One more segment was recognised.
    #[serde(rename_all = "camelCase")]
    Segment {
        start_ms: u64,
        end_ms: u64,
        text: String,
    },
    /// The job moved forward without the screen changing.
    #[serde(rename_all = "camelCase")]
    Progress { elapsed_ms: u64 },
    /// A new job is starting; drop everything from the previous one.
    Cleared,
    /// A model download moved. Sent often, so it carries only the download
    /// and not the whole screen.
    Download { download: DownloadView },
}

/// How a model download is going, in the form the window reads.
///
/// Bytes rather than a bare "downloading": a progress bar needs a number, and
/// a state with nothing in it leaves the window spinning.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum DownloadView {
    Idle,
    Fetching {
        file: String,
        received_bytes: u64,
        /// `null` when the host did not say how large the file is.
        total_bytes: Option<u64>,
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
    NeedsModel {
        file: String,
        model: String,
        download: DownloadView,
    },
    Working {
        file: String,
        /// True once the user has asked to stop and the job has not yet.
        stopping: bool,
    },
    Done {
        file: String,
        /// Where the transcript was written, once it has been. The window uses
        /// it to stop offering an export that has already happened.
        exported: Option<String>,
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
        Screen::NeedsModel {
            input,
            model,
            download,
        } => ScreenView::NeedsModel {
            file: file_label(input),
            model: model.clone(),
            download: download.into(),
        },
        Screen::Working(working) => ScreenView::Working {
            file: file_label(&working.input),
            stopping: working.stopping,
        },
        Screen::Done(done) => ScreenView::Done {
            file: file_label(&done.input),
            exported: done.exported.as_ref().map(|path| file_label(path)),
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
/// the initial load.
pub fn push_screen(app: &AppHandle) {
    let app_state = app.state::<App>();
    let state = app_state.state.lock().expect("state mutex poisoned");
    let _ = app.emit(
        UPDATE,
        Update::Screen {
            screen: view_of(state.screen()),
        },
    );
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

        let update = {
            let app_state = app.state::<App>();
            let mut state = app_state.state.lock().expect("state mutex poisoned");

            match state.apply(&event) {
                Applied::Nothing => None,
                Applied::Screen => Some(Update::Screen {
                    screen: view_of(state.screen()),
                }),
                Applied::Segment(Segment { start, end, text, .. }) => Some(Update::Segment {
                    start_ms: start.as_millis() as u64,
                    end_ms: end.as_millis() as u64,
                    text,
                }),
                Applied::Cleared => Some(Update::Cleared),
                Applied::Progress { position, .. } => Some(Update::Progress {
                    elapsed_ms: position.as_millis() as u64,
                }),
            }
        };

        // A failed emit means the window is gone; there is nothing to do about
        // it and nothing worth logging.
        if let Some(update) = update {
            let _ = app.emit(UPDATE, update);
        }
    });
}

// ---------------------------------------------------------------- from the window

/// Start transcribing a file.
///
/// Returns as soon as the work is under way — the outcome arrives on the bus
/// like everything else.
pub fn start(app: &AppHandle, input: std::path::PathBuf, models_dir: std::path::PathBuf) {
    let (job, cancel) = {
        let app_state = app.state::<App>();
        let id = JobId(app_state.next_job.fetch_add(1, Ordering::Relaxed));
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
        let bus = shared.bus.clone();

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
        };

        // Loading is where a missing or unusable model shows up, and it
        // happens before the pipeline has published anything — so this is the
        // one failure the caller has to announce itself.
        let mut transcriber = match Transcriber::load(request) {
            Ok(transcriber) => transcriber,
            Err(error) => {
                bus.publish(Event::JobFailed {
                    id: job,
                    error: (&error).into(),
                });
                clear_job(&shared, job);
                return;
            }
        };

        // Everything else publishes its own outcome.
        let _ = transcriber.transcribe(job, &bus, &cancel);

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
    fn a_working_screen_carries_the_file_name_and_the_stopping_flag() {
        let value = serde_json::to_value(Update::Screen {
            screen: ScreenView::Working {
                file: "会议录音.m4a".to_string(),
                stopping: true,
            },
        })
        .expect("serializes");

        assert_eq!(
            value,
            json!({
                "kind": "screen",
                "screen": { "kind": "working", "file": "会议录音.m4a", "stopping": true }
            })
        );
    }

    #[test]
    fn a_segment_uses_camel_case_milliseconds() {
        let value = serde_json::to_value(Update::Segment {
            start_ms: 1500,
            end_ms: 3200,
            text: "开放时间".to_string(),
        })
        .expect("serializes");

        assert_eq!(
            value,
            json!({ "kind": "segment", "startMs": 1500, "endMs": 3200, "text": "开放时间" })
        );
    }

    #[test]
    fn a_done_screen_reports_whether_it_has_been_written_out() {
        // The window uses this to stop offering an export that has already
        // happened, so `null` and a path have to be distinguishable.
        let unsaved = serde_json::to_value(Update::Screen {
            screen: ScreenView::Done {
                file: "会议.m4a".to_string(),
                exported: None,
            },
        })
        .expect("serializes");

        assert_eq!(
            unsaved,
            json!({ "kind": "screen", "screen": { "kind": "done", "file": "会议.m4a", "exported": null } })
        );

        let saved = serde_json::to_value(Update::Screen {
            screen: ScreenView::Done {
                file: "会议.m4a".to_string(),
                exported: Some("会议.srt".to_string()),
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
