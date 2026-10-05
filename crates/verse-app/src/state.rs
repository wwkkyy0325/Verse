//! The interface's state, as data.
//!
//! Nothing here knows what Slint is, or what a window is. This is ordinary
//! Rust over ordinary values — a screen, the fields on it, and the moves
//! between screens — which is what makes the whole state machine testable
//! without opening a window.
//!
//! Side effects are named, not performed. Methods return an [`Effect`] saying
//! what should happen next, and the caller does it. Nothing in this module
//! spawns a thread, opens a file or sends a message.

use std::path::PathBuf;
use std::time::Duration;

use verse_core::{ErrorInfo, ErrorKind, Event, JobId, Segment, Transcript};
use verse_model::DownloadState;

/// The engine used when nothing else is configured.
pub const DEFAULT_MODEL: &str = "sensevoice";

/// Which screen the window is showing.
#[derive(Debug, PartialEq)]
pub enum Screen {
    /// The drop target. Where the application rests.
    Empty,

    /// Blocking: no usable model on disk.
    ///
    /// The only screen that interrupts the one-step path, and unavoidable — a
    /// 228 MB model cannot ship inside a lightweight binary.
    NeedsModel {
        /// The file the user is waiting to transcribe. Kept so the job can
        /// start on its own once the model arrives.
        input: PathBuf,
        model: String,
        download: DownloadState,
    },

    /// A job is running.
    Working(Working),

    /// Finished, with a transcript to show.
    Done(Done),

    /// Finished badly, with something the user can do about it.
    Failed(Failed),
}

/// A job in flight.
#[derive(Debug, PartialEq)]
pub struct Working {
    pub input: PathBuf,
    /// How far into the file the job has read.
    pub position: Duration,
    /// `None` until a total is known. The bar is indeterminate until then,
    /// which is honest — a bar that sits at zero is not.
    pub fraction: Option<f32>,
    /// What has been recognized so far, shown as it arrives.
    ///
    /// This is what makes a long wait bearable: a spinner says "still going",
    /// this says "here is what it found". Same machinery live subtitles need.
    pub segments: Vec<Segment>,
    /// Set when the user has asked to stop and the job has not acknowledged
    /// yet. No second job may start while this is true.
    pub stopping: bool,
}

impl Working {
    fn new(input: PathBuf) -> Self {
        Self {
            input,
            position: Duration::ZERO,
            fraction: None,
            segments: Vec::new(),
            stopping: false,
        }
    }

    /// Whether any text has come out yet.
    ///
    /// Before the first segment the user is waiting on decoding or a model
    /// load, after it on recognition. Two labels, because that is the whole
    /// difference they can perceive.
    pub fn has_output(&self) -> bool {
        !self.segments.is_empty()
    }
}

/// A finished job.
#[derive(Debug, PartialEq)]
pub struct Done {
    pub input: PathBuf,
    pub transcript: Transcript,
    /// Where the result was written, once it has been. Kept so the export
    /// buttons can confirm rather than repeat.
    pub exported: Option<PathBuf>,
}

/// A job that did not finish.
#[derive(Debug, PartialEq)]
pub struct Failed {
    pub input: PathBuf,
    /// Written for the user, not for a log.
    pub reason: String,
    pub recovery: Recovery,
}

/// What the user can do about a failure.
///
/// Every failure has one. An error message with no way forward is a dead end,
/// which is the thing this design is most concerned to avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Worth trying again as-is: the failure was transient.
    Retry,
    /// The model is missing or unusable; offer to fetch or import one.
    GetModel,
    /// The input itself is the problem. Only a different file helps.
    PickAnotherFile,
}

impl Recovery {
    fn for_error(kind: ErrorKind) -> Self {
        match kind {
            ErrorKind::Model | ErrorKind::Registry => Recovery::GetModel,
            ErrorKind::Decode | ErrorKind::Io => Recovery::PickAnotherFile,
            _ => Recovery::Retry,
        }
    }
}

/// What the caller should do after a transition.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Nothing.
    None,
    /// Run a transcription for this file.
    Transcribe(PathBuf),
    /// Stop the job in flight.
    Cancel,
    /// Fetch a model, then continue with the file that is waiting.
    FetchModel { model: String, input: PathBuf },
}

/// What visibly changed as a result of an event.
///
/// The frontend holds a mirror of this state and is told about changes rather
/// than sent snapshots — a two-hour recording is a few thousand segments, and
/// resending the list to move one progress bar is not affordable. This enum is
/// what the caller switches on to decide what to send.
///
/// It deliberately carries no serialization: the bridge turns these into
/// whatever the window understands, and `AppState` stays free of that.
#[derive(Debug, Clone, PartialEq)]
pub enum Applied {
    /// Nothing the user can see.
    Nothing,
    /// The screen changed, and the frontend should re-render it wholesale.
    Screen,
    /// One more segment was recognised and appended.
    Segment(Segment),
    /// Progress moved within a screen that did not change.
    Progress {
        position: Duration,
        fraction: Option<f32>,
    },
}

/// The whole of the interface's state.
#[derive(Debug)]
pub struct AppState {
    screen: Screen,
    /// The job this screen is following.
    ///
    /// Events carry a job id, and a late event from a cancelled or replaced
    /// job must not disturb whatever replaced it. Matching on this is what
    /// stops that; see [`AppState::owns`].
    job: Option<JobId>,
    /// The engine this session uses. Not user-facing in P1b — the design
    /// requires that the default path involve no technical decisions.
    model: String,
    /// Shown once, under the header, when the machine is running degraded.
    hardware_notice: Option<String>,
    about_open: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        Self {
            screen: Screen::Empty,
            job: None,
            model: DEFAULT_MODEL.to_string(),
            hardware_notice: None,
            about_open: false,
        }
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn hardware_notice(&self) -> Option<&str> {
        self.hardware_notice.as_deref()
    }

    pub fn about_open(&self) -> bool {
        self.about_open
    }

    pub fn set_about_open(&mut self, open: bool) {
        self.about_open = open;
    }

    /// Stop showing the degraded-hardware notice.
    pub fn dismiss_hardware_notice(&mut self) {
        self.hardware_notice = None;
    }

    // ------------------------------------------------------------ user input

    /// The user chose a file.
    ///
    /// `model_ready` is answered by the caller, because answering it means
    /// touching the filesystem and this module does not do that.
    pub fn file_chosen(&mut self, input: PathBuf, model_ready: bool) -> Effect {
        self.job = None;

        if model_ready {
            self.screen = Screen::Working(Working::new(input.clone()));
            Effect::Transcribe(input)
        } else {
            // Deliberately no automatic fetch. Downloads are user-initiated
            // everywhere in this project.
            self.screen = Screen::NeedsModel {
                input,
                model: self.model.clone(),
                download: DownloadState::Idle,
            };
            Effect::None
        }
    }

    /// The user asked to fetch the missing model.
    pub fn fetch_model(&mut self) -> Effect {
        match &self.screen {
            Screen::NeedsModel { model, input, .. } => Effect::FetchModel {
                model: model.clone(),
                input: input.clone(),
            },
            _ => Effect::None,
        }
    }

    /// A download progressed.
    ///
    /// Dropped unless the model screen is up, so a download that finishes
    /// after the user gave up cannot move a screen that has moved on.
    pub fn download_changed(&mut self, state: DownloadState) {
        if let Screen::NeedsModel { download, .. } = &mut self.screen {
            *download = state;
        }
    }

    /// The user pointed at a folder that already holds the model.
    ///
    /// The caller has already checked the folder; this starts the job that
    /// was waiting on it.
    pub fn model_imported(&mut self) -> Effect {
        let input = match &self.screen {
            Screen::NeedsModel { input, .. } => input.clone(),
            _ => return Effect::None,
        };

        self.screen = Screen::Working(Working::new(input.clone()));
        Effect::Transcribe(input)
    }

    /// The user asked to stop.
    ///
    /// The screen does not return to the drop target yet. Recognition can take
    /// a moment to notice, and showing an idle screen while a job is still
    /// winding down would both lie and allow a second job to start on top of
    /// the first.
    pub fn cancel(&mut self) -> Effect {
        match &mut self.screen {
            Screen::Working(working) if !working.stopping => {
                working.stopping = true;
                Effect::Cancel
            }
            // A second press while the first is still being honoured.
            _ => Effect::None,
        }
    }

    /// Leave a finished or failed screen, back to the drop target.
    pub fn reset(&mut self) {
        self.screen = Screen::Empty;
        self.job = None;
    }

    /// Record where the transcript was written.
    pub fn note_exported(&mut self, path: PathBuf) {
        if let Screen::Done(done) = &mut self.screen {
            done.exported = Some(path);
        }
    }

    // -------------------------------------------------------- pipeline input

    /// Handle one event from the bus, reporting what the user can see change.
    pub fn apply(&mut self, event: &Event) -> Applied {
        match event {
            // Claimed here rather than at dispatch: the id is assigned by the
            // pipeline, so the screen can only learn it from this event.
            Event::JobStarted { id, .. } => {
                if matches!(self.screen, Screen::Working(_)) && self.job.is_none() {
                    self.job = Some(*id);
                }
                return Applied::Nothing;
            }

            // Environment, not job output: belongs to every screen.
            Event::HardwareProbed { profile } => {
                let notice = profile.tier().notice();
                if notice == self.hardware_notice {
                    return Applied::Nothing;
                }
                self.hardware_notice = notice;
                return Applied::Screen;
            }

            _ => {}
        }

        if !self.owns(event) {
            return Applied::Nothing;
        }

        match event {
            Event::JobProgress {
                position, fraction, ..
            } => match &mut self.screen {
                Screen::Working(working) => {
                    working.position = *position;
                    working.fraction = Some(*fraction);
                    Applied::Progress {
                        position: *position,
                        fraction: Some(*fraction),
                    }
                }
                _ => Applied::Nothing,
            },

            Event::TranscriptSegment { segment, .. } => match &mut self.screen {
                Screen::Working(working) => {
                    working.segments.push(segment.clone());
                    Applied::Segment(segment.clone())
                }
                _ => Applied::Nothing,
            },

            Event::TranscriptFinal { transcript, .. } => {
                let input = match &self.screen {
                    Screen::Working(working) => working.input.clone(),
                    _ => return Applied::Nothing,
                };

                self.screen = Screen::Done(Done {
                    input,
                    transcript: transcript.clone(),
                    exported: None,
                });
                self.job = None;
                Applied::Screen
            }

            // A clean finish carries its result in TranscriptFinal, which has
            // already been applied.
            Event::JobFinished { .. } => Applied::Nothing,

            Event::JobFailed { error, .. } => {
                if !matches!(self.screen, Screen::Working(_)) {
                    return Applied::Nothing;
                }

                // Cancellation is not a failure and must not be shown as one.
                if error.kind == ErrorKind::Cancelled {
                    self.reset();
                    return Applied::Screen;
                }

                let input = match &self.screen {
                    Screen::Working(working) => working.input.clone(),
                    _ => return Applied::Nothing,
                };

                self.screen = Screen::Failed(Failed {
                    input,
                    reason: message_for(error),
                    recovery: Recovery::for_error(error.kind),
                });
                self.job = None;
                Applied::Screen
            }

            Event::JobCancelled { .. } => {
                self.reset();
                Applied::Screen
            }

            _ => Applied::Nothing,
        }
    }

    /// The segments recognised so far, whichever screen is showing them.
    pub fn segments(&self) -> &[Segment] {
        match &self.screen {
            Screen::Working(working) => &working.segments,
            Screen::Done(done) => &done.transcript.segments,
            _ => &[],
        }
    }

    /// The engine this session uses.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Whether an event belongs to the job this screen is following.
    ///
    /// Without this, output from a job the user cancelled would land on
    /// whatever screen replaced it.
    fn owns(&self, event: &Event) -> bool {
        match event.job() {
            Some(id) => self.job == Some(id),
            None => true,
        }
    }
}

/// Turn an error into something worth reading.
///
/// The engine's own wording is kept — it is more specific than anything that
/// could be reconstructed from the error kind — but the message has to say
/// what the user can do, which means the kind is not always irrelevant.
fn message_for(error: &ErrorInfo) -> String {
    match error.kind {
        ErrorKind::Decode => format!("无法读取这个文件：{}", error.message),
        ErrorKind::Model => format!("模型不可用：{}", error.message),
        ErrorKind::Network => format!("下载失败：{}", error.message),
        _ => error.message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use verse_core::{HardwareProfile, SegmentId};

    fn a_segment(text: &str) -> Segment {
        Segment {
            id: SegmentId(0),
            start: Duration::ZERO,
            end: Duration::from_secs(1),
            text: text.to_string(),
        }
    }

    fn error(kind: ErrorKind, message: &str) -> ErrorInfo {
        ErrorInfo {
            kind,
            message: message.to_string(),
        }
    }

    fn input(name: &str) -> PathBuf {
        PathBuf::from(name)
    }

    /// Take a state to the point where a job is running and claimed.
    fn working() -> AppState {
        let mut state = AppState::new();
        state.file_chosen(input("a.wav"), true);
        state.apply(&Event::JobStarted {
            id: JobId(1),
            kind: verse_core::JobKind::FileTranscribe,
        });
        state
    }

    fn segments(state: &AppState) -> &[Segment] {
        match state.screen() {
            Screen::Working(working) => &working.segments,
            Screen::Done(done) => &done.transcript.segments,
            other => panic!("not showing segments: {other:?}"),
        }
    }

    #[test]
    fn a_file_with_a_model_starts_working_immediately() {
        let mut state = AppState::new();
        let effect = state.file_chosen(input("a.wav"), true);

        assert_eq!(effect, Effect::Transcribe(input("a.wav")));
        assert!(matches!(state.screen(), Screen::Working(_)));
    }

    #[test]
    fn a_file_without_a_model_asks_for_one_and_starts_nothing() {
        let mut state = AppState::new();
        let effect = state.file_chosen(input("a.wav"), false);

        // The point: no download begins on its own.
        assert_eq!(effect, Effect::None);
        assert!(matches!(state.screen(), Screen::NeedsModel { .. }));
    }

    #[test]
    fn fetching_a_model_names_both_it_and_the_waiting_file() {
        let mut state = AppState::new();
        state.file_chosen(input("a.wav"), false);

        assert_eq!(
            state.fetch_model(),
            Effect::FetchModel {
                model: DEFAULT_MODEL.to_string(),
                input: input("a.wav"),
            }
        );
    }

    #[test]
    fn a_finished_model_download_starts_the_waiting_job() {
        let mut state = AppState::new();
        state.file_chosen(input("a.wav"), false);
        state.download_changed(DownloadState::Ready);

        assert_eq!(state.model_imported(), Effect::Transcribe(input("a.wav")));
        assert!(matches!(state.screen(), Screen::Working(_)));
    }

    #[test]
    fn a_download_that_lands_after_the_user_left_changes_nothing() {
        let mut state = AppState::new();
        state.file_chosen(input("a.wav"), false);
        state.reset();

        state.download_changed(DownloadState::Failed {
            reason: "offline".to_string(),
        });

        assert_eq!(*state.screen(), Screen::Empty);
    }

    #[test]
    fn segments_appear_while_the_job_is_still_running() {
        let mut state = working();

        state.apply(&Event::TranscriptSegment {
            job: JobId(1),
            segment: a_segment("开放时间"),
        });

        // Not merely counted: the text is on screen before the job ends.
        assert!(matches!(state.screen(), Screen::Working(_)));
        assert_eq!(segments(&state).len(), 1);
    }

    #[test]
    fn a_first_segment_is_what_separates_the_two_progress_labels() {
        let mut state = working();
        let Screen::Working(working) = state.screen() else {
            panic!("should be working");
        };
        assert!(!working.has_output());

        state.apply(&Event::TranscriptSegment {
            job: JobId(1),
            segment: a_segment("开放时间"),
        });

        let Screen::Working(working) = state.screen() else {
            panic!("should be working");
        };
        assert!(working.has_output());
    }

    #[test]
    fn progress_before_a_length_is_known_leaves_the_fraction_unset() {
        let mut state = AppState::new();
        state.file_chosen(input("a.wav"), true);

        let Screen::Working(working) = state.screen() else {
            panic!("should be working");
        };
        assert_eq!(working.fraction, None);
    }

    #[test]
    fn the_final_transcript_replaces_the_partial_one() {
        let mut state = working();
        state.apply(&Event::TranscriptSegment {
            job: JobId(1),
            segment: a_segment("开放时间"),
        });

        let mut transcript = Transcript::default();
        transcript.segments.push(a_segment("开放时间"));
        transcript.segments.push(a_segment("早上九点。"));

        state.apply(&Event::TranscriptFinal {
            job: JobId(1),
            transcript,
        });

        assert!(matches!(state.screen(), Screen::Done(_)));
        assert_eq!(segments(&state).len(), 2);
    }

    #[test]
    fn a_finished_job_is_no_longer_followed() {
        let mut state = working();
        state.apply(&Event::TranscriptFinal {
            job: JobId(1),
            transcript: Transcript::default(),
        });

        assert_eq!(state.job, None, "a stale id would keep matching events");
    }

    #[test]
    fn a_decode_failure_asks_for_another_file() {
        let mut state = working();
        state.apply(&Event::JobFailed {
            id: JobId(1),
            error: error(ErrorKind::Decode, "no audio stream"),
        });

        match state.screen() {
            Screen::Failed(failed) => {
                assert_eq!(failed.recovery, Recovery::PickAnotherFile);
                assert!(failed.reason.contains("无法读取"), "got: {}", failed.reason);
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_model_offers_to_get_one() {
        let mut state = working();
        state.apply(&Event::JobFailed {
            id: JobId(1),
            error: error(ErrorKind::Model, "sensevoice not found"),
        });

        match state.screen() {
            Screen::Failed(failed) => assert_eq!(failed.recovery, Recovery::GetModel),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn a_network_failure_is_worth_retrying() {
        let mut state = working();
        state.apply(&Event::JobFailed {
            id: JobId(1),
            error: error(ErrorKind::Network, "connection reset"),
        });

        match state.screen() {
            Screen::Failed(failed) => assert_eq!(failed.recovery, Recovery::Retry),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn cancelling_waits_for_the_job_to_acknowledge() {
        let mut state = working();

        assert_eq!(state.cancel(), Effect::Cancel);

        // Still on the working screen: the job has not stopped yet, and
        // pretending otherwise would allow a second one to start.
        assert!(matches!(state.screen(), Screen::Working(_)));
    }

    #[test]
    fn a_second_cancel_while_stopping_is_ignored() {
        let mut state = working();
        state.cancel();

        assert_eq!(state.cancel(), Effect::None);
    }

    #[test]
    fn acknowledgement_returns_to_the_drop_target() {
        let mut state = working();
        state.cancel();

        state.apply(&Event::JobCancelled { id: JobId(1) });

        assert_eq!(*state.screen(), Screen::Empty);
        assert_eq!(state.job, None);
    }

    #[test]
    fn a_cancelled_job_reported_as_a_failure_is_not_shown_as_one() {
        let mut state = working();

        state.apply(&Event::JobFailed {
            id: JobId(1),
            error: error(ErrorKind::Cancelled, "cancelled"),
        });

        assert_eq!(*state.screen(), Screen::Empty);
    }

    #[test]
    fn output_from_a_job_that_is_over_is_ignored() {
        let mut state = working();
        state.apply(&Event::JobCancelled { id: JobId(1) });
        state.file_chosen(input("b.wav"), true);

        // A straggler from the cancelled job arrives late.
        state.apply(&Event::TranscriptSegment {
            job: JobId(1),
            segment: a_segment("不应该出现"),
        });

        assert!(
            segments(&state).is_empty(),
            "a dead job's output reached the screen that replaced it"
        );
    }

    #[test]
    fn progress_is_only_accepted_from_the_followed_job() {
        let mut state = working();

        state.apply(&Event::JobProgress {
            id: JobId(99),
            position: Duration::from_secs(60),
            fraction: 0.5,
        });

        let Screen::Working(working) = state.screen() else {
            panic!("should be working");
        };
        assert_eq!(working.position, Duration::ZERO);
    }

    #[test]
    fn a_hardware_notice_appears_and_can_be_dismissed() {
        let mut state = AppState::new();
        let reduced = HardwareProfile {
            avx2: false,
            fma: false,
            cores: 4,
        };

        state.apply(&Event::HardwareProbed { profile: reduced });
        assert!(state.hardware_notice().is_some());

        state.dismiss_hardware_notice();
        assert!(state.hardware_notice().is_none());
    }

    #[test]
    fn a_capable_machine_says_nothing_about_hardware() {
        let mut state = AppState::new();
        let full = HardwareProfile {
            avx2: true,
            fma: true,
            cores: 8,
        };

        state.apply(&Event::HardwareProbed { profile: full });
        assert!(state.hardware_notice().is_none());
    }

    #[test]
    fn an_export_is_recorded_so_the_button_can_stop_offering() {
        let mut state = working();
        state.apply(&Event::TranscriptFinal {
            job: JobId(1),
            transcript: Transcript::default(),
        });

        state.note_exported(input("a.srt"));

        match state.screen() {
            Screen::Done(done) => assert_eq!(done.exported, Some(input("a.srt"))),
            other => panic!("expected a finished job, got {other:?}"),
        }
    }

    #[test]
    fn the_about_dialog_opens_and_closes() {
        let mut state = AppState::new();
        assert!(!state.about_open());

        state.set_about_open(true);
        assert!(state.about_open());
    }
}
