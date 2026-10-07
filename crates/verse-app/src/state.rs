//! The interface's state, as data.
//!
//! Nothing here knows what a webview is, or what a window is. This is ordinary
//! Rust over ordinary values — a screen, the fields on it, and the moves
//! between screens — which is what makes the whole state machine testable
//! without opening a window.
//!
//! Side effects are named, not performed. Methods return an [`Effect`] saying
//! what should happen next, and the caller does it. Nothing in this module
//! spawns a thread, opens a file or sends a message.
//!
//! **A screen belongs to a file.** The window shows one file at a time in its
//! detail pane, but it keeps every file this session has been given, each
//! carrying its own [`Screen`] — see [`FileEntry`]. The screen enum is
//! unchanged: it was always the story of one file, and what changed is that
//! there can now be more than one of them.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use verse_core::{ErrorInfo, ErrorKind, Event, JobId, Segment, Transcript};
use verse_model::DownloadState;

/// The engine used when nothing else is configured.
pub const DEFAULT_MODEL: &str = "sensevoice";

/// What the detail pane shows when no file is selected.
///
/// A `static` rather than a temporary, because [`AppState::screen`] returns a
/// reference and a reference to a temporary cannot outlive the call.
static NO_SCREEN: Screen = Screen::Empty;

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

    /// Waiting for the job slot, because another file has it.
    ///
    /// Not `Working`: a queued file has no job to cancel, and pretending it
    /// did would make pressing 取消 on the waiting file stop the running one.
    Queued {
        input: PathBuf,
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
    ///
    /// Set by the automatic save as well as by *save as*: to anything watching
    /// the screen, a transcript that has already been written somewhere is the
    /// same fact however it got there.
    pub exported: Option<PathBuf>,
    /// Why the automatic save did not happen, when it did not.
    ///
    /// The transcript is still here and *save as* still works, so this is not a
    /// failure of the job — but it is why there is nothing in the output folder,
    /// and staying quiet about that would leave a person looking for a file
    /// that was never written.
    pub save_error: Option<String>,
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
    /// The engine changed, so any loaded model is the wrong one.
    EngineChanged,
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
    /// A file that is *not* being shown moved on.
    ///
    /// The list carries each file's state, so a background file finishing is
    /// visible even while the pane is showing another one. Sent instead of
    /// [`Applied::Screen`] because redrawing the pane would replace the
    /// transcript a person is reading with one they are not.
    Roster,
    /// A different file now owns the pane.
    ///
    /// The window holds one segment list, so it is replaced rather than
    /// appended to — a snapshot, but on a click rather than per event.
    Selected,
    /// One more segment was recognised and appended.
    Segment(Segment),
    /// Everything recognised so far is void; the list starts again.
    ///
    /// The segmenter can be distrusted after it has already been believed, and
    /// the segments it produced have been shown by then. Without this the
    /// window would keep the discarded fragment and append the replacement to
    /// it, displaying both.
    Cleared,
    /// Progress moved within a screen that did not change.
    Progress {
        position: Duration,
        fraction: Option<f32>,
    },
}

/// One file this session has been given, and what is happening to it.
///
/// The screen travels with the file rather than with the window, so switching
/// between files is a change of which entry is *shown* and never a change of
/// what any entry *is*.
#[derive(Debug, PartialEq)]
pub struct FileEntry {
    pub input: PathBuf,
    /// The engine this file was handed to.
    ///
    /// Recorded per file rather than read from the session at display time,
    /// because the engine can be changed between files: reading it later would
    /// relabel everything already done, and a transcript attributed to the
    /// wrong recogniser is worse than one that says nothing.
    pub engine: String,
    pub screen: Screen,
}

impl FileEntry {
    /// A short label for the list: the file's own name, not its path.
    pub fn label(&self) -> String {
        self.input
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.input.display().to_string())
    }
}

/// A transcript that was finished before this run.
///
/// Carries the transcript rather than a path to it, because reading and parsing
/// the file is I/O and this module does none. The caller has already done it.
#[derive(Debug, Clone, PartialEq)]
pub struct Restored {
    pub input: PathBuf,
    pub engine: String,
    pub output: PathBuf,
    pub transcript: Transcript,
}

/// The whole of the interface's state.
#[derive(Debug)]
pub struct AppState {
    /// Every file this session has been given, in the order it arrived.
    files: Vec<FileEntry>,
    /// Which entry the detail pane is showing. `None` is the drop target.
    selected: Option<usize>,
    /// Files waiting for the one job slot, as indices into `files`.
    ///
    /// Advanced by [`AppState::take_next`], which the bridge calls when a job
    /// finishes. The window never starts a second job, because starting one
    /// while another runs would cancel it.
    queue: VecDeque<usize>,
    /// The job in flight.
    ///
    /// Events carry a job id, and a late event from a cancelled or replaced
    /// job must not disturb whatever replaced it. Matching on this is what
    /// stops that; see [`AppState::owns`].
    job: Option<JobId>,
    /// Which entry `job` is working on.
    ///
    /// Kept beside `job` rather than derived from it, because the two answer
    /// different questions: `job` says whether an event is ours, this says
    /// where its output goes. A file that is not on screen still has to be
    /// recognised correctly, so output follows *this* and not the selection.
    /// They are set and cleared together — see `claim_job` and `release_job`.
    running: Option<usize>,
    /// The engine this session uses.
    ///
    /// SenseVoice unless the user says otherwise: the design requires that the
    /// default path involve no technical decisions, so the picker is visible
    /// but already answered.
    model: String,
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
            files: Vec::new(),
            selected: None,
            queue: VecDeque::new(),
            job: None,
            running: None,
            model: DEFAULT_MODEL.to_string(),
            about_open: false,
        }
    }

    /// The entry the detail pane is showing.
    pub fn screen(&self) -> &Screen {
        self.selected
            .and_then(|index| self.files.get(index))
            .map_or(&NO_SCREEN, |entry| &entry.screen)
    }

    /// The screen being shown, when there is one to change.
    fn active(&mut self) -> Option<&mut Screen> {
        let index = self.selected?;
        self.files.get_mut(index).map(|entry| &mut entry.screen)
    }

    /// Everything this session has been given, for the list region.
    pub fn files(&self) -> &[FileEntry] {
        &self.files
    }

    /// Which entry the detail pane is showing.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn about_open(&self) -> bool {
        self.about_open
    }

    pub fn set_about_open(&mut self, open: bool) {
        self.about_open = open;
    }

    // ------------------------------------------------------------ user input

    /// The user chose a file.
    ///
    /// `model_ready` is answered by the caller, because answering it means
    /// touching the filesystem and this module does not do that.
    pub fn file_chosen(&mut self, input: PathBuf, model_ready: bool) -> Effect {
        // Already in the list. Show it rather than adding a second row for the
        // same file: dropping a file again is how somebody asks "where did that
        // go", and answering with a second row that recognises it a second time
        // is a worse answer than the result already sitting there.
        //
        // Keyed on the path. Two copies of a recording under different names do
        // get two rows, which is right — they are two files.
        if let Some(index) = self.files.iter().position(|entry| entry.input == input) {
            self.selected = Some(index);
            return Effect::None;
        }

        // Something is already running, or is about to be. The new file waits
        // its turn instead of cancelling it: the window has one job slot, and
        // starting a second job stops the first.
        //
        // Keyed on `running` rather than `job`, and that is the whole point:
        // `running` is set the moment a file is accepted, while `job` is only
        // filled in when the pipeline announces the id. Dropping five files at
        // once calls this five times within a millisecond, long before any
        // `JobStarted` has arrived — checking `job` would let the second call
        // start a job and cancel the first.
        if self.running.is_some() {
            self.enqueue(input);
            return Effect::None;
        }

        let screen = if model_ready {
            Screen::Working(Working::new(input.clone()))
        } else {
            Screen::NeedsModel {
                input: input.clone(),
                model: self.model.clone(),
                download: DownloadState::Idle,
            }
        };

        // Appended and shown, so the one-click path behaves exactly as it did
        // when there was only ever one file.
        self.files.push(FileEntry {
            input: input.clone(),
            engine: self.model.clone(),
            screen,
        });
        let index = self.files.len() - 1;
        self.selected = Some(index);

        if model_ready {
            // The job has not been announced yet; `running` marks the entry it
            // will belong to. See `claim_job`.
            self.running = Some(index);
            Effect::Transcribe(input)
        } else {
            // Deliberately no automatic fetch. Downloads are user-initiated
            // everywhere in this project.
            Effect::None
        }
    }

    /// The user asked to fetch the missing model.
    /// Whether the file being shown is waiting for this exact model.
    ///
    /// Asked after a download finishes, so that fetching a model from the panel
    /// — which need not be the one the waiting file wants — cannot start a job
    /// that would then fail for want of the model it actually needed.
    pub fn waiting_for(&self, model: &str) -> bool {
        matches!(self.screen(), Screen::NeedsModel { model: wanted, .. } if wanted == model)
    }

    /// A download progressed.
    ///
    /// Dropped unless the shown screen wants one, so a download that finishes
    /// after the user gave up cannot move a screen that has moved on.
    pub fn download_changed(&mut self, state: DownloadState) {
        if let Some(Screen::NeedsModel { download, .. }) = self.active() {
            *download = state;
        }
    }

    /// A model has arrived, by download or by hand.
    ///
    /// The caller has already put it in place; this starts the job that was
    /// waiting on it.
    pub fn model_ready(&mut self) -> Effect {
        let Some(index) = self.selected else {
            return Effect::None;
        };
        let Some(entry) = self.files.get(index) else {
            return Effect::None;
        };
        let input = match &entry.screen {
            Screen::NeedsModel { input, .. } => input.clone(),
            _ => return Effect::None,
        };

        self.files[index].screen = Screen::Working(Working::new(input.clone()));
        self.running = Some(index);
        Effect::Transcribe(input)
    }

    /// The user asked to stop.
    ///
    /// The screen does not return to the drop target yet. Recognition can take
    /// a moment to notice, and showing an idle screen while a job is still
    /// winding down would both lie and allow a second job to start on top of
    /// the first.
    pub fn cancel(&mut self) -> Effect {
        match self.active() {
            Some(Screen::Working(working)) if !working.stopping => {
                working.stopping = true;
                Effect::Cancel
            }
            // A second press while the first is still being honoured.
            _ => Effect::None,
        }
    }

    /// Run the file being shown again.
    ///
    /// Not the same as dropping it: `file_chosen` treats a file already in the
    /// list as "here it is" and starts nothing, which is right for a drop and
    /// useless for a retry. A failure has to be able to try again, and that is
    /// what this is for.
    pub fn retry(&mut self) -> Effect {
        let Some(index) = self.selected else {
            return Effect::None;
        };
        if self.running.is_some() {
            return Effect::None;
        }
        let Some(entry) = self.files.get_mut(index) else {
            return Effect::None;
        };

        let input = entry.input.clone();
        entry.screen = Screen::Working(Working::new(input.clone()));
        self.running = Some(index);
        Effect::Transcribe(input)
    }

    /// Leave the file being shown, back to the drop target.
    ///
    /// The list is left alone: "再来一个" means another file, not forgetting
    /// the ones already done.
    pub fn reset(&mut self) {
        self.selected = None;
    }

    /// Put back what a previous run finished.
    ///
    /// The rows come back as [`Screen::Done`] with `exported` already set,
    /// because the result is where it says it is. That is also what stops the
    /// automatic save from writing a restored transcript out a second time —
    /// which it would otherwise do on the first event, into a numbered file
    /// beside the one that already exists.
    ///
    /// Nothing is selected: opening the window should show the drop target,
    /// with what was done before sitting in the list beside it.
    pub fn restore(&mut self, restored: Vec<Restored>) {
        for past in restored {
            // A file already in the list — dropped again while this window has
            // been open — keeps the row it has.
            if self.files.iter().any(|entry| entry.input == past.input) {
                continue;
            }

            self.files.push(FileEntry {
                input: past.input.clone(),
                engine: past.engine,
                screen: Screen::Done(Done {
                    input: past.input,
                    transcript: past.transcript,
                    exported: Some(past.output),
                    save_error: None,
                }),
            });
        }
    }

    /// Show a different file, reporting whether anything moved.
    ///
    /// Selecting the file that is already shown changes nothing, so a repeated
    /// click does not blank the pane and refill it.
    pub fn select(&mut self, index: usize) -> Applied {
        if index >= self.files.len() || self.selected == Some(index) {
            return Applied::Nothing;
        }

        self.selected = Some(index);
        Applied::Selected
    }

    /// Start the next queued file, if there is one.
    ///
    /// Called by the bridge once a job is over. Answers the file to run, so
    /// the caller does not have to work out which entry was meant. A queued
    /// file whose model turned out to be missing is left for `file_chosen`
    /// to deal with rather than started here — this only moves waiting files
    /// into the job slot.
    pub fn take_next(&mut self) -> Option<PathBuf> {
        while let Some(index) = self.queue.pop_front() {
            let Some(entry) = self.files.get_mut(index) else {
                continue;
            };
            let Screen::Queued { input } = &entry.screen else {
                continue;
            };

            let input = input.clone();
            entry.engine = self.model.clone();
            entry.screen = Screen::Working(Working::new(input.clone()));
            self.selected = Some(index);
            self.running = Some(index);
            return Some(input);
        }

        None
    }

    /// Whether a job is in flight.
    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    /// Queue a file behind the running job.
    ///
    /// Answers whether it was accepted. The same path is not queued twice:
    /// dropping a folder that contains a file already in the list would
    /// otherwise recognise it again and overwrite its own result.
    pub fn enqueue(&mut self, input: PathBuf) -> bool {
        if self.files.iter().any(|entry| entry.input == input) {
            return false;
        }

        self.files.push(FileEntry {
            screen: Screen::Queued { input: input.clone() },
            input,
            // The engine chosen now, but the file waits behind others while the
            // engine may change — `take_next` sets this again when it actually
            // starts, and that is the value that ends up recorded.
            engine: self.model.clone(),
        });
        self.queue.push_back(self.files.len() - 1);
        true
    }

    /// The finished transcript being shown, if there is one to write out.
    pub fn finished(&self) -> Option<&Done> {
        match self.screen() {
            Screen::Done(done) => Some(done),
            _ => None,
        }
    }

    /// A finished transcript that nobody has tried to write out yet.
    ///
    /// Answered with its index rather than its `Done`, because the automatic
    /// save has to record where it wrote *that* file — and by then the user may
    /// have clicked to another one. It is also why this is not
    /// [`AppState::finished`]: a file that finished in the background still
    /// needs writing out, and it is not the file being shown.
    ///
    /// A file counts as untried until one of `exported` or `save_error` is set,
    /// so an attempt that failed is not retried on every subsequent event —
    /// which would turn one unwritable folder into a loop.
    pub fn awaiting_save(&self) -> Option<(usize, PathBuf, Transcript)> {
        self.files.iter().enumerate().find_map(|(index, entry)| {
            let Screen::Done(done) = &entry.screen else {
                return None;
            };

            (done.exported.is_none() && done.save_error.is_none())
                .then(|| (index, done.input.clone(), done.transcript.clone()))
        })
    }

    /// Record where the transcript was written.
    pub fn note_exported(&mut self, index: usize, path: PathBuf) {
        if let Some(Screen::Done(done)) = self.files.get_mut(index).map(|e| &mut e.screen) {
            done.exported = Some(path);
            // A later success supersedes an earlier refusal: *save as* having
            // worked means the transcript is written, and a stale complaint
            // about the automatic attempt would contradict the screen.
            done.save_error = None;
        }
    }

    /// Record that the automatic save did not happen.
    ///
    /// Worded for the person reading it, like every other reason on a screen.
    pub fn note_save_failed(&mut self, index: usize, reason: String) {
        if let Some(Screen::Done(done)) = self.files.get_mut(index).map(|e| &mut e.screen) {
            done.save_error = Some(reason);
        }
    }

    /// The engine this session uses.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Choose an engine.
    ///
    /// Refused while a job is running: the model is loaded and in use, and
    /// swapping it now would either be ignored or pull the model out from
    /// under the job.
    pub fn set_model(&mut self, id: String) -> Effect {
        // `running`, not `job`, for the same reason as `file_chosen`: the
        // window between accepting a file and learning its job id is short but
        // real, and a model swapped inside it would be swapped under a job
        // that is already on its way.
        if self.running.is_some() || self.model == id {
            return Effect::None;
        }

        self.model = id;
        Effect::EngineChanged
    }

    // -------------------------------------------------------- pipeline input

    /// Take the job id for the entry that is waiting for one.
    ///
    /// The id is assigned by the pipeline, so the entry can only learn it from
    /// the `JobStarted` event.
    fn claim_job(&mut self, id: JobId) {
        if self.job.is_none() {
            self.job = Some(id);
        }
    }

    /// Forget the job, leaving the entry and its screen alone.
    fn release_job(&mut self) {
        self.job = None;
        self.running = None;
    }

    /// Handle one event from the bus, reporting what the user can see change.
    pub fn apply(&mut self, event: &Event) -> Applied {
        // Claimed before the ownership check below — `owns` reads the very id
        // this sets, so running it first would reject the event that would
        // have claimed the job.
        if let Event::JobStarted { id, .. } = event {
            self.claim_job(*id);
            return Applied::Nothing;
        }

        if !self.owns(event) {
            return Applied::Nothing;
        }

        match event {
            Event::JobProgress {
                position, fraction, ..
            } => {
                let Some(working) = self.running_working() else {
                    return Applied::Nothing;
                };

                working.position = *position;
                working.fraction = *fraction;
                // Worth showing only if it is the file on screen; a background
                // file's bar is not on anyone's screen to move.
                if self.running == self.selected {
                    Applied::Progress {
                        position: *position,
                        fraction: *fraction,
                    }
                } else {
                    Applied::Nothing
                }
            }

            Event::TranscriptSegment { segment, .. } => {
                let Some(working) = self.running_working() else {
                    return Applied::Nothing;
                };
                working.segments.push(segment.clone());

                // The state keeps every segment whatever is on screen, so
                // switching to a file mid-job shows what has been recognised
                // so far — but the window is only sent the ones it is showing.
                if self.running == self.selected {
                    Applied::Segment(segment.clone())
                } else {
                    Applied::Nothing
                }
            }

            Event::TranscriptDiscarded { .. } => {
                let Some(working) = self.running_working() else {
                    return Applied::Nothing;
                };
                working.segments.clear();

                if self.running == self.selected {
                    Applied::Cleared
                } else {
                    Applied::Nothing
                }
            }

            Event::TranscriptFinal { transcript, .. } => {
                let Some(index) = self.running else {
                    return Applied::Nothing;
                };
                let input = match self.files.get(index).map(|entry| &entry.screen) {
                    Some(Screen::Working(working)) => working.input.clone(),
                    _ => return Applied::Nothing,
                };

                self.files[index].screen = Screen::Done(Done {
                    input,
                    transcript: transcript.clone(),
                    exported: None,
                    save_error: None,
                });
                self.release_job();
                self.changed_at(index)
            }

            // A clean finish carries its result in TranscriptFinal, which has
            // already been applied.
            Event::JobFinished { .. } => Applied::Nothing,

            Event::JobFailed { error, .. } => {
                let Some(index) = self.running else {
                    return Applied::Nothing;
                };
                if !matches!(self.files.get(index).map(|e| &e.screen), Some(Screen::Working(_))) {
                    return Applied::Nothing;
                }

                // Cancellation is not a failure and must not be shown as one.
                if error.kind == ErrorKind::Cancelled {
                    return self.abandon(index);
                }

                let input = match &self.files[index].screen {
                    Screen::Working(working) => working.input.clone(),
                    _ => return Applied::Nothing,
                };

                self.files[index].screen = Screen::Failed(Failed {
                    input,
                    reason: message_for(error),
                    recovery: Recovery::for_error(error.kind),
                });
                self.release_job();
                self.changed_at(index)
            }

            Event::JobCancelled { .. } => match self.running {
                Some(index) => self.abandon(index),
                None => Applied::Nothing,
            },

            _ => Applied::Nothing,
        }
    }

    /// The working screen of the entry the job belongs to, if there is one.
    fn running_working(&mut self) -> Option<&mut Working> {
        let index = self.running?;
        match &mut self.files.get_mut(index)?.screen {
            Screen::Working(working) => Some(working),
            _ => None,
        }
    }

    /// What to tell the window after the entry at `index` changed.
    ///
    /// The screen and the list are separate regions, so a change to a file that
    /// is not being shown is still a change worth sending — the list shows its
    /// state — while a change to the shown one redraws both.
    fn changed_at(&self, index: usize) -> Applied {
        if self.selected == Some(index) {
            Applied::Screen
        } else {
            Applied::Roster
        }
    }

    /// Drop the entry whose job was cancelled, and stop following it.
    ///
    /// The entry goes rather than becoming a screen saying "cancelled": the
    /// user asked for it to stop happening, and a row about it would be a
    /// record of something they did not want.
    fn abandon(&mut self, index: usize) -> Applied {
        self.release_job();
        self.remove_at(index)
    }

    /// Take one entry out of the list, answering whether the pane was showing it.
    ///
    /// Every index after the removed one shifts down, so the queue and the
    /// selection are rewritten rather than merely filtered — and the pane only
    /// blanks when the file it was showing is the one that went.
    fn remove_at(&mut self, index: usize) -> Applied {
        if index < self.files.len() {
            self.files.remove(index);
        }

        self.queue = self
            .queue
            .drain(..)
            .filter(|queued| *queued != index)
            .map(|queued| if queued > index { queued - 1 } else { queued })
            .collect();

        let was_showing = self.selected == Some(index);
        self.selected = match self.selected {
            Some(selected) if selected == index => None,
            Some(selected) if selected > index => Some(selected - 1),
            other => other,
        };

        if was_showing {
            Applied::Screen
        } else {
            Applied::Roster
        }
    }

    /// Take a file out of the list, leaving what it points at where it is.
    ///
    /// The row is a pointer. Forgetting it does not delete the transcript it
    /// points at — that is the whole reason this is a separate act from
    /// deleting the result, one being reversible and the other not. Answers the
    /// input path so the caller can forget it in the record too, or the row
    /// would be back on the next launch.
    pub fn forget(&mut self, index: usize) -> Option<PathBuf> {
        let input = self.files.get(index)?.input.clone();
        self.remove_at(index);
        Some(input)
    }

    /// The segments recognised so far for the file being shown.
    pub fn segments(&self) -> &[Segment] {
        match self.screen() {
            Screen::Working(working) => &working.segments,
            Screen::Done(done) => &done.transcript.segments,
            _ => &[],
        }
    }

    /// Whether an event belongs to the job this window is following.
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
    use verse_core::SegmentId;

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
    fn only_the_model_that_is_actually_waited_for_counts_as_waited_for() {
        // The panel can fetch any model, and fetching one that nothing is
        // waiting for must not start the job that is waiting for another.
        let mut state = AppState::new();
        state.file_chosen(input("a.wav"), false);

        assert!(state.waiting_for(DEFAULT_MODEL));
        assert!(!state.waiting_for("qwen3-asr"));

        // And a file that is not waiting for anything never counts.
        let mut working = AppState::new();
        working.file_chosen(input("b.wav"), true);
        assert!(!working.waiting_for(DEFAULT_MODEL));
    }

    #[test]
    fn a_finished_model_download_starts_the_waiting_job() {
        let mut state = AppState::new();
        state.file_chosen(input("a.wav"), false);
        state.download_changed(DownloadState::Ready);

        assert_eq!(state.model_ready(), Effect::Transcribe(input("a.wav")));
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
    fn only_a_finished_screen_has_something_to_export() {
        let mut state = working();
        assert!(state.finished().is_none(), "nothing to export while working");

        state.apply(&Event::TranscriptFinal {
            job: JobId(1),
            transcript: Transcript {
                segments: vec![a_segment("开放时间")],
                language: None,
            },
        });

        let done = state.finished().expect("a finished transcript");
        assert_eq!(done.transcript.segments.len(), 1);
        assert!(done.exported.is_none(), "nothing has been written yet");

        state.note_exported(0, PathBuf::from("/out/a.srt"));
        assert_eq!(
            state.finished().and_then(|d| d.exported.as_deref()),
            Some(std::path::Path::new("/out/a.srt"))
        );
    }

    #[test]
    fn a_discarded_transcript_takes_its_segments_with_it() {
        let mut state = working();
        for text in ["逛集市了", "去逛集市了"] {
            state.apply(&Event::TranscriptSegment {
                job: JobId(1),
                segment: a_segment(text),
            });
        }
        assert_eq!(segments(&state).len(), 2);

        let applied = state.apply(&Event::TranscriptDiscarded { job: JobId(1) });

        assert_eq!(applied, Applied::Cleared);
        assert!(segments(&state).is_empty());
    }

    #[test]
    fn a_discarded_transcript_leaves_the_job_running() {
        // Clearing must not look like a cancel: the replacement is on its way,
        // and dropping the job id here would leave it unrecognised and unshown.
        let mut state = working();
        state.apply(&Event::TranscriptSegment {
            job: JobId(1),
            segment: a_segment("逛集市了"),
        });

        state.apply(&Event::TranscriptDiscarded { job: JobId(1) });

        assert!(matches!(state.screen(), Screen::Working(_)));

        // And the replacement still lands.
        state.apply(&Event::TranscriptSegment {
            job: JobId(1),
            segment: a_segment("逛集市喽"),
        });
        assert_eq!(segments(&state).len(), 1);
        assert_eq!(segments(&state)[0].text, "逛集市喽");
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
    fn a_load_failure_reaches_the_screen_when_its_job_was_announced() {
        // The sequence the keeper publishes when it cannot load a model:
        // JobStarted, then JobFailed. Note what this test does *not* use —
        // the `working()` helper above, because it injects JobStarted itself
        // and so cannot tell whether anything else did.
        let mut state = AppState::new();
        assert!(matches!(
            state.file_chosen(input("a.wav"), true),
            Effect::Transcribe(_)
        ));

        state.apply(&Event::JobStarted {
            id: JobId(7),
            kind: verse_core::JobKind::FileTranscribe,
        });
        let applied = state.apply(&Event::JobFailed {
            id: JobId(7),
            error: error(ErrorKind::Model, "the model file is not usable"),
        });

        assert_eq!(applied, Applied::Screen);
        match state.screen() {
            Screen::Failed(failed) => assert_eq!(failed.recovery, Recovery::GetModel),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn a_failure_for_a_job_nobody_announced_is_ignored() {
        // Why the `JobStarted` above is load-bearing rather than tidiness.
        //
        // A job id is claimed by `JobStarted` and by nothing else. Output for an
        // unclaimed id is discarded, which is deliberate — it is what stops a
        // cancelled job's stragglers landing on the screen that replaced it.
        // The consequence is that a failure published *without* its
        // `JobStarted` is discarded too, and the screen waits for ever.
        //
        // That was real: a model present at the expected size but unusable fails
        // to load, and loading happened before the pipeline published anything,
        // so the failure arrived unclaimed and the window sat on "正在准备…"
        // with a cancel button for a job that was never running.
        let mut state = AppState::new();
        state.file_chosen(input("a.wav"), true);

        let applied = state.apply(&Event::JobFailed {
            id: JobId(7),
            error: error(ErrorKind::Model, "the model file is not usable"),
        });

        assert_eq!(applied, Applied::Nothing);
        assert!(
            matches!(state.screen(), Screen::Working(_)),
            "still waiting, which is the dead end this pins"
        );
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
            fraction: Some(0.5),
        });

        let Screen::Working(working) = state.screen() else {
            panic!("should be working");
        };
        assert_eq!(working.position, Duration::ZERO);
    }

    /// Take the state to one finished file, as a real run would leave it.
    fn finish(state: &mut AppState, text: &str) {
        state.apply(&Event::TranscriptFinal {
            job: JobId(1),
            transcript: Transcript {
                segments: vec![a_segment(text)],
                language: None,
            },
        });
    }

    #[test]
    fn a_second_file_does_not_replace_the_first() {
        // Before the list existed this was the whole behaviour: dropping a
        // second file threw the first away, including its transcript.
        let mut state = working();
        finish(&mut state, "第一段");

        state.file_chosen(input("b.wav"), true);

        assert_eq!(state.files().len(), 2);
        assert_eq!(state.files()[0].label(), "a.wav");
        assert_eq!(state.files()[1].label(), "b.wav");
        assert_eq!(state.selected(), Some(1), "the new file is the one shown");
    }

    #[test]
    fn showing_a_finished_file_again_brings_its_transcript_back() {
        let mut state = working();
        finish(&mut state, "第一段");
        state.file_chosen(input("b.wav"), true);

        let applied = state.select(0);

        assert_eq!(applied, Applied::Selected);
        assert_eq!(state.segments().len(), 1);
        assert_eq!(state.segments()[0].text, "第一段");
        assert!(matches!(state.screen(), Screen::Done(_)));
    }

    #[test]
    fn selecting_the_file_already_shown_changes_nothing() {
        // Otherwise a second click blanks the pane and refills it, which reads
        // as a flicker and loses the scroll position.
        let mut state = working();
        assert_eq!(state.select(0), Applied::Nothing);
    }

    #[test]
    fn a_file_dropped_while_another_runs_waits_its_turn() {
        let mut state = working();

        let effect = state.file_chosen(input("b.wav"), true);

        assert_eq!(
            effect,
            Effect::None,
            "starting a second job here would cancel the first"
        );
        assert!(matches!(state.files()[1].screen, Screen::Queued { .. }));
        assert!(
            matches!(state.screen(), Screen::Working(_)),
            "the file being transcribed keeps the pane"
        );

        finish(&mut state, "第一段");

        assert_eq!(state.take_next(), Some(input("b.wav")));
        assert!(matches!(state.screen(), Screen::Working(_)));
        assert_eq!(state.selected(), Some(1));
    }

    #[test]
    fn several_files_dropped_at_once_leave_one_running_and_the_rest_waiting() {
        // Dropping a folder is one gesture that arrives as many paths, all
        // within a millisecond and all before any `JobStarted` has been seen.
        // The guard has to hold without knowing the first job's id yet — which
        // is the bug this pins: keyed on the id, the second call starts a job
        // and cancels the first.
        let mut state = AppState::new();

        let effects: Vec<Effect> = ["a.wav", "b.wav", "c.wav"]
            .iter()
            .map(|name| state.file_chosen(input(name), true))
            .collect();

        assert_eq!(effects[0], Effect::Transcribe(input("a.wav")));
        assert_eq!(effects[1], Effect::None, "a second job would stop the first");
        assert_eq!(effects[2], Effect::None);

        assert!(matches!(state.files()[1].screen, Screen::Queued { .. }));
        assert!(matches!(state.files()[2].screen, Screen::Queued { .. }));
    }

    #[test]
    fn the_same_file_is_not_queued_twice() {
        // Dropping a folder that contains a file already in the list would
        // otherwise recognise it a second time and overwrite its own result.
        let mut state = working();

        assert!(!state.enqueue(input("a.wav")));
        assert_eq!(state.files().len(), 1);
    }

    #[test]
    fn a_restored_transcript_is_a_finished_row_that_can_be_shown() {
        let mut state = AppState::new();

        state.restore(vec![Restored {
            input: input("yesterday.wav"),
            engine: "sensevoice".to_string(),
            output: input("/out/yesterday.srt"),
            transcript: Transcript {
                segments: vec![a_segment("昨天说过的话")],
                language: None,
            },
        }]);

        assert_eq!(state.files().len(), 1);
        assert_eq!(
            state.selected(),
            None,
            "opening the window shows the drop target, not somebody's old file"
        );
        assert!(matches!(state.files()[0].screen, Screen::Done(_)));

        state.select(0);
        assert_eq!(state.segments()[0].text, "昨天说过的话");
    }

    #[test]
    fn a_restored_transcript_is_not_written_out_again() {
        // The bug this prevents: a restored row with no `exported` would be
        // saved a second time by the first event after launch, into a numbered
        // file beside the one it was restored from.
        let mut state = AppState::new();
        state.restore(vec![Restored {
            input: input("yesterday.wav"),
            engine: "sensevoice".to_string(),
            output: input("/out/yesterday.srt"),
            transcript: Transcript::default(),
        }]);

        assert!(
            state.awaiting_save().is_none(),
            "it already knows where it was written"
        );
    }

    #[test]
    fn restoring_a_file_already_in_the_list_keeps_the_row_it_has() {
        let mut state = working();

        state.restore(vec![Restored {
            input: input("a.wav"),
            engine: "sensevoice".to_string(),
            output: input("/out/a.srt"),
            transcript: Transcript::default(),
        }]);

        assert_eq!(state.files().len(), 1);
        assert!(matches!(state.files()[0].screen, Screen::Working(_)));
    }

    #[test]
    fn dropping_a_file_already_in_the_list_shows_it_rather_than_adding_a_row() {
        let mut state = working();
        finish(&mut state, "第一段");
        state.file_chosen(input("b.wav"), true);

        let effect = state.file_chosen(input("a.wav"), true);

        assert_eq!(effect, Effect::None, "nothing to run; the answer is already here");
        assert_eq!(state.files().len(), 2, "no second row for the same file");
        assert_eq!(state.selected(), Some(0), "and it is the one now shown");
        assert_eq!(state.segments()[0].text, "第一段");
    }

    #[test]
    fn retrying_a_failed_file_runs_it_again() {
        // The case dedup would otherwise break: dropping the file again shows
        // the failure, because it is already in the list. Trying again is a
        // different request and has to have its own way in.
        let mut state = working();
        state.apply(&Event::JobFailed {
            id: JobId(1),
            error: error(ErrorKind::Decode, "unreadable"),
        });
        assert!(matches!(state.screen(), Screen::Failed(_)));

        assert_eq!(state.retry(), Effect::Transcribe(input("a.wav")));
        assert!(matches!(state.screen(), Screen::Working(_)));
        assert_eq!(state.files().len(), 1, "a retry is not a second row");
    }

    #[test]
    fn forgetting_a_row_takes_it_out_and_answers_which_file_it_was() {
        // The answer matters: the caller forgets it in the record too, or the
        // row is back on the next launch and "remove this" meant nothing.
        let mut state = working();
        finish(&mut state, "第一段");
        state.file_chosen(input("b.wav"), true);

        let forgotten = state.forget(0);

        assert_eq!(forgotten, Some(input("a.wav")));
        assert_eq!(state.files().len(), 1);
        assert_eq!(state.files()[0].label(), "b.wav");

        assert_eq!(state.forget(9), None, "nothing there to forget");
    }

    #[test]
    fn forgetting_the_file_being_shown_blanks_the_pane_and_keeps_the_other() {
        let mut state = working();
        finish(&mut state, "第一段");
        state.file_chosen(input("b.wav"), true);
        state.select(0);

        state.forget(0);

        assert_eq!(*state.screen(), Screen::Empty);
        assert_eq!(state.selected(), None, "and the selection followed");

        // The one that is left is still selectable, at its new index.
        state.select(0);
        assert_eq!(state.files()[0].label(), "b.wav");
    }

    #[test]
    fn a_cancelled_file_leaves_the_list() {
        let mut state = working();

        state.apply(&Event::JobCancelled { id: JobId(1) });

        assert!(
            state.files().is_empty(),
            "a row about something the user asked to stop is a record they did not want"
        );
        assert_eq!(*state.screen(), Screen::Empty);
    }

    #[test]
    fn output_for_a_file_that_is_not_shown_still_lands() {
        // The point of the roster: a background file has to be recognised
        // correctly even while somebody is reading a different one.
        let mut state = working();
        finish(&mut state, "第一段");
        state.file_chosen(input("b.wav"), true);
        state.select(0);

        // The pipeline names the job when it starts; until then the second
        // file has no id and its output would be rejected as belonging to
        // nobody — which is what the first run of this test did.
        state.apply(&Event::JobStarted {
            id: JobId(2),
            kind: verse_core::JobKind::FileTranscribe,
        });
        state.apply(&Event::TranscriptSegment {
            job: JobId(2),
            segment: a_segment("第二段"),
        });

        // Nothing moves on screen — the shown file is not the one running —
        // but the running file keeps its text for when it is selected.
        assert_eq!(state.segments().len(), 1);
        assert_eq!(state.segments()[0].text, "第一段");

        let applied = state.select(1);
        assert_eq!(applied, Applied::Selected);
        assert_eq!(state.segments()[0].text, "第二段");
    }

    #[test]
    fn a_finished_background_file_is_still_written_out() {
        // Autosave reads this rather than the shown screen, because the file
        // that finished need not be the file on screen.
        let mut state = working();
        finish(&mut state, "第一段");
        state.file_chosen(input("b.wav"), true);
        state.select(0);

        let (index, path, transcript) = state
            .awaiting_save()
            .expect("the background file has not been written anywhere");

        assert_eq!(index, 0);
        assert_eq!(path, input("a.wav"));
        assert_eq!(transcript.segments.len(), 1);
    }

    #[test]
    fn a_save_that_failed_is_not_attempted_on_every_event() {
        let mut state = working();
        finish(&mut state, "第一段");

        state.note_save_failed(0, "read-only".to_string());

        assert!(
            state.awaiting_save().is_none(),
            "retrying on every event turns one unwritable folder into a loop"
        );
    }

    #[test]
    fn the_engine_cannot_be_changed_while_a_job_holds_the_model() {
        let mut state = working();

        assert_eq!(state.set_model("qwen3-asr".to_string()), Effect::None);
        assert_eq!(state.model(), DEFAULT_MODEL);
    }

    #[test]
    fn choosing_the_engine_that_is_already_chosen_changes_nothing() {
        let mut state = AppState::new();

        assert_eq!(state.set_model(DEFAULT_MODEL.to_string()), Effect::None);
        assert_eq!(
            state.set_model("qwen3-asr".to_string()),
            Effect::EngineChanged
        );
        assert_eq!(state.model(), "qwen3-asr");
    }

    #[test]
    fn an_export_is_recorded_so_the_button_can_stop_offering() {
        let mut state = working();
        state.apply(&Event::TranscriptFinal {
            job: JobId(1),
            transcript: Transcript::default(),
        });

        state.note_exported(0, input("a.srt"));

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
