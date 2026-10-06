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

        let (update, unsaved) = {
            let app_state = app.state::<App>();
            let mut state = app_state.state.lock().expect("state mutex poisoned");

            // One match rather than two: `Applied::Segment` carries the segment
            // by value, so matching twice would move out of it the first time.
            match state.apply(&event) {
                Applied::Nothing => (None, None),
                Applied::Screen => {
                    let update = Some(Update::Screen {
                        screen: view_of(state.screen()),
                    });

                    // The finished transcript, if it has just arrived and
                    // nobody has written it anywhere. Taken here and written
                    // below, and deliberately not written *here*: this holds
                    // the state lock, and a save into a synchronised Documents
                    // folder can take long enough to notice.
                    let unsaved = state.finished().and_then(|done| {
                        done.exported
                            .is_none()
                            .then(|| (done.input.clone(), done.transcript.clone()))
                    });

                    (update, unsaved)
                }
                Applied::Segment(Segment { start, end, text, .. }) => (
                    Some(Update::Segment {
                        start_ms: start.as_millis() as u64,
                        end_ms: end.as_millis() as u64,
                        text,
                    }),
                    None,
                ),
                Applied::Cleared => (Some(Update::Cleared), None),
                Applied::Progress { position, .. } => (
                    Some(Update::Progress {
                        elapsed_ms: position.as_millis() as u64,
                    }),
                    None,
                ),
            }
        };

        // A failed emit means the window is gone; there is nothing to do about
        // it and nothing worth logging.
        if let Some(update) = update {
            let _ = app.emit(UPDATE, update);
        }

        if let Some((input, transcript)) = unsaved {
            autosave(&app, &input, &transcript);
        }
    });
}

/// Write a finished transcript out, and tell the window what happened.
///
/// Runs on the forwarding thread, immediately after `TranscriptFinal` has been
/// applied. That ordering is the reason it is here rather than on the worker
/// that did the recognition: the worker would race the forwarder, and the
/// screen would end up recording a save that had not happened yet, or losing
/// one that had.
fn autosave(app: &AppHandle, input: &std::path::Path, transcript: &Transcript) {
    let roots = verse_store::Roots::from_env();
    let rendered = verse_core::ExportFormat::Srt.render(transcript);

    let outcome = crate::autosave::save_into(
        &verse_store::output_dir(&roots),
        &verse_store::data_dir(&roots).join("outputs.json"),
        input,
        &rendered,
    );

    {
        let app_state = app.state::<App>();
        let mut state = app_state.state.lock().expect("state mutex poisoned");

        match outcome {
            crate::autosave::Saved::Written(path) => state.note_exported(path),
            crate::autosave::Saved::Refused(why) => state.note_save_failed(why),
        }
    }

    // So the screen gains the path or the reason. Without this the window would
    // show the transcript and never mention either.
    push_screen(app);
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
    fn a_download_sends_camel_case_byte_counts() {
        // The test that was missing, and its absence had a cost. The window
        // reads `download.receivedBytes`; without a per-variant `rename_all`
        // the field arrived as `received_bytes`, nothing on either side
        // complained, and the progress bar sat at zero saying "NaN MB".
        //
        // `rename_all` on an enum renames its variants. It does not touch
        // their fields, which is the whole trap.
        let value = serde_json::to_value(Update::Download {
            download: DownloadView::Fetching {
                file: "model.onnx".to_string(),
                received_bytes: 1024,
                total_bytes: Some(2048),
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
                download: DownloadView::Fetching {
                    file: "a.onnx".to_string(),
                    received_bytes: 1,
                    total_bytes: None,
                },
            },
            Update::Download {
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
                },
            },
            Update::Segment {
                start_ms: 0,
                end_ms: 1,
                text: "x".to_string(),
            },
            Update::Progress { elapsed_ms: 1 },
            Update::Cleared,
        ];

        let mut found = Vec::new();
        for sample in samples {
            keys_with_underscores(&serde_json::to_value(sample).expect("serializes"), &mut found);
        }

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
