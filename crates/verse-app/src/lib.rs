//! Verse desktop application — the backend half.
//!
//! The interface is a web frontend (`ui/`, Svelte + TypeScript) rendered in
//! the system webview, and this crate is what it talks to. Everything above
//! the pipeline lives in the library crates; this is the thin layer that
//! exposes it to a window.
//!
//! Two files carry the work:
//!
//! - `state.rs` — what the interface is showing, as plain data with no
//!   framework types in it. This is the part with tests.
//! - `bridge.rs` — the two directions between the window and the pipeline.
//!
//! The chain itself is `verse-pipeline`, shared with the command line and the
//! benchmark so that all three are measuring and shipping the same thing.

mod about;
mod autosave;
mod bridge;
mod settings;
mod tray;


// The drop-a-file path is wired end to end. The rest of the state machine —
// the model screen, export, the about dialog — is written and tested but has
// no caller yet, because those screens are steps 12 and 13 of
// tasks/p1b-gui.md. Deleting it would mean deleting tested behaviour and
// writing it again; the allow is here rather than there for that reason.
#[allow(dead_code)]
mod state;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use verse_core::{CancelToken, ExportFormat, HardwareProfile, Weakness};
use verse_model::{Catalog, Downloader, DownloadState};

use bridge::{App, DownloadView, Update, UPDATE};

#[cfg(debug_assertions)]
mod demo;
use state::Effect;

/// What the interface is told about the machine it is running on.
#[derive(Debug, Serialize)]
pub struct HardwareSummary {
    pub avx2: bool,
    pub cores: usize,
    /// Cores reserved for recognition. One is always left for the interface,
    /// which is the difference between a responsive window and a frozen one
    /// on an older machine.
    pub threads: usize,
    /// Why this machine is being worked around, if it is — the sentence the
    /// window puts in its notice bar, in Chinese. `None` when nothing is wrong,
    /// which is the usual case.
    pub degraded: Option<String>,
}

/// Where models live.
///
/// `VERSE_MODELS` wins when set, which is how a development build finds the
/// checkout's `models/` directory — the executable sits several levels away in
/// `target/`. Otherwise it is `models` under the per-user data directory, the
/// same one the cache, the resume logs and the history already use.
///
/// **It used to be a `models` directory beside the executable, and that was
/// wrong twice over.** The `.msi` installs per-machine, into
/// `C:\Program Files\Verse`, where a standard user cannot create a directory —
/// so a `.msi` install could not download a model at all. And the weights
/// belong to the person, not to the program: they should not live somewhere the
/// installer owns and an uninstall takes away with it.
///
/// `ui-design.md` §11 asked this question; this function is the answer to it.
fn models_dir() -> PathBuf {
    models_root(
        // An empty `VERSE_MODELS` means "I did not set this", the same reading
        // `verse_store::dirs` gives its own variables.
        std::env::var_os("VERSE_MODELS")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
        &verse_store::Roots::from_env(),
    )
}

/// The resolution above, as a function of what was read from the environment.
///
/// Split out for the reason `verse_store::dirs` gives for doing the same: the
/// fallbacks only run on somebody else's machine, and a branch nobody can run
/// is a branch nobody has tested.
fn models_root(configured: Option<PathBuf>, roots: &verse_store::Roots) -> PathBuf {
    match configured {
        Some(dir) => dir,
        None => verse_store::data_dir(roots).join("models"),
    }
}

/// Probe the machine.
///
/// Detection never fails and never blocks startup: an older CPU runs smaller
/// models and is told why.
#[tauri::command]
fn hardware() -> HardwareSummary {
    hardware_summary(HardwareProfile::probe())
}

/// The summary for a profile that has already been read.
///
/// Split out from the command so the answer can be tested against a machine
/// that is not this one — the `degraded` arm is `None` here whatever the test
/// machine is, and a test that only ever sees `None` would pass while the
/// notice was broken.
fn hardware_summary(profile: HardwareProfile) -> HardwareSummary {
    HardwareSummary {
        avx2: profile.avx2,
        cores: profile.cores,
        threads: profile.engine_threads(),
        degraded: profile.tier().weakness().map(window_notice),
    }
}

/// The reduced-mode line the window shows.
///
/// Chinese, and composed here rather than reusing `Tier::notice()`: that one is
/// English, because it is also what `verse` writes to stderr. The window is
/// Chinese throughout (ui-design.md §8), so the same fact has to be rendered
/// twice — but both renderings read the same [`Weakness`], so the two can
/// disagree about wording and not about what is wrong with the machine.
///
/// The sentence ends by saying the app still works, because that is the point:
/// a machine in reduced mode is slower, not broken.
fn window_notice(weakness: Weakness) -> String {
    let reason = match weakness {
        Weakness::NoAvx2 => "这台电脑的 CPU 不支持 AVX2",
        Weakness::SingleCore => "这台电脑只有一个 CPU 核心",
    };

    format!("正在以精简模式运行：{reason}。转写仍然可用，只是会更慢。")
}

/// Everything the window needs to draw itself.
///
/// Called once when the frontend mounts. Everything after that arrives on
/// [`UPDATE`] — as an increment where there is one, and as a whole snapshot
/// where there is not.
#[tauri::command]
fn current_state(app: AppHandle) -> bridge::StateView {
    let app_state = app.state::<App>();
    let state = app_state.state.lock().expect("state mutex poisoned");
    bridge::StateView::of(&state)
}

/// Begin transcribing a file.
///
/// Returns as soon as the work is under way. The result, the progress and any
/// failure all arrive on [`UPDATE`] — this deliberately has nothing to report
/// beyond "the path made sense".
#[tauri::command]
fn transcribe(app: AppHandle, path: String) -> Result<(), String> {
    let models = models_dir();
    let input = PathBuf::from(&path);

    if !input.is_file() {
        return Err(format!("找不到这个文件：{}", input.display()));
    }

    let effect = {
        let app_state = app.state::<App>();
        let model = {
            let state = app_state.state.lock().expect("state mutex poisoned");
            state.model().to_string()
        };

        let ready = match Catalog::load_or_embedded(&models.join("catalog.json")) {
            Ok(catalog) => catalog
                .find(&model)
                .map(|spec| catalog.all_present(spec, &models))
                .unwrap_or(false),
            // A broken catalogue is not worth refusing to work over; the model
            // is either on disk or it is not, and the pipeline will say so.
            Err(_) => false,
        };

        let mut state = app_state.state.lock().expect("state mutex poisoned");
        state.file_chosen(input, ready)
    };

    // A new job's transcript must not be appended to the last one's. Only when
    // one is actually starting: a file that was queued behind another has no
    // transcript of its own yet, and clearing would wipe the one on screen.
    if matches!(effect, Effect::Transcribe(_)) {
        let _ = app.emit(UPDATE, Update::Cleared);
    }
    bridge::push_state(&app);

    if let Effect::Transcribe(input) = effect {
        bridge::start(&app, input, models);
    }

    Ok(())
}

/// Adds up a whole model's download from the downloader's one-file-at-a-time
/// reports.
///
/// Pulled out of the callback so it can be tested. Which files count as done is
/// decided by noticing the name change, and an off-by-one there shows up as a
/// bar that stalls or jumps rather than as anything that fails.
struct ModelProgress {
    /// The whole model's size, or `None` if any file's is unknown — a total
    /// missing a part of itself is a worse answer than no total.
    total: Option<u64>,
    /// The latest figure seen for each file, by name.
    ///
    /// Replaced rather than added to, which is what makes this work for files
    /// fetched several at a time: a retry that restarts one file from zero
    /// corrects itself, and three files in flight need no special case. The
    /// previous version decided a file was finished by noticing the name
    /// change, which is only true when they arrive one at a time.
    ///
    /// Seeded with the files that are already here, so a resumed download
    /// starts at what is on disk rather than at zero.
    seen: HashMap<String, u64>,
}

impl ModelProgress {
    fn of(spec: &verse_model::ModelSpec, root: &std::path::Path) -> Self {
        let dir = verse_model::Downloader::directory_for(spec, root);
        let mut seen = HashMap::new();

        for file in &spec.files {
            if verse_model::file_is_complete(file, &dir) {
                if let Some(size) = file.size {
                    seen.insert(file.local.clone(), size);
                }
            }
        }

        Self {
            total: spec.files.iter().map(|file| file.size).sum(),
            seen,
        }
    }

    /// Bytes received for the whole model, when its size is known.
    fn observe(&mut self, state: &verse_model::DownloadState) -> Option<u64> {
        if let verse_model::DownloadState::Fetching { file, received, .. } = state {
            self.seen.insert(file.clone(), *received);
        }

        self.total?;
        Some(self.seen.values().sum())
    }
}

/// The engines this build can run, with what a person needs to choose one./// The engines this build can run, with what a person needs to choose one.
///
/// Filtered to ids the registry can actually load. A catalogue entry with no
/// engine behind it would be a row that can never run — and the two lists are
/// maintained separately, so the filter is what keeps them honest rather than
/// a comment asking people to keep them in step.
#[tauri::command]
fn models() -> Result<Vec<bridge::ModelChoice>, String> {
    let root = models_dir();

    let catalog = Catalog::load_or_embedded(&root.join("catalog.json"))
        .map_err(|e| format!("读不了模型清单：{e}"))?;

    let mut registry = verse_core::Registry::new();
    verse_asr::register_builtin_engines(&mut registry);

    Ok(catalog
        .models
        .iter()
        .filter(|spec| registry.engine(&spec.id).is_some())
        .map(|spec| bridge::ModelChoice {
            id: spec.id.clone(),
            name: spec.display_name.clone(),
            description: spec.description.clone(),
            // Not `is_present`: a card that says 已安装 for a model that
            // cannot run is how a fresh install ended up unable to
            // transcribe anything. See `ModelSpec::requires`.
            present: catalog.all_present(spec, &root),
            bytes: spec.files.iter().filter_map(|file| file.size).sum(),
            default: spec.id == state::DEFAULT_MODEL,
        })
        .collect())
}

/// Where the record of finished transcripts lives.
fn history_path() -> PathBuf {
    verse_store::data_dir(&verse_store::Roots::from_env()).join("history.json")
}

/// Where the window's preferences live.
pub(crate) fn settings_path() -> PathBuf {
    verse_store::data_dir(&verse_store::Roots::from_env()).join("settings.json")
}

/// Where the list of interrupted files lives.
pub(crate) fn pending_path() -> PathBuf {
    verse_store::data_dir(&verse_store::Roots::from_env()).join("pending.json")
}

/// The argument `explorer` needs to open a folder with this file selected.
///
/// Quoted here and not by `Command::arg`, which quotes the whole argument
/// rather than the path inside it — see the note at the call site.
///
/// A function rather than an inline `format!` so the shape is visible and
/// testable: the quotes are load-bearing and are exactly the sort of thing a
/// tidy-up removes.
fn explorer_select_argument(path: &Path) -> String {
    format!("/select,\"{}\"", path.display())
}

/// Show a file in the platform's file manager.
///
/// The program knows the path and the next thing a person wants after reading a
/// transcript is usually the transcript *as a file*. Spawning the file manager
/// is how every desktop does it; doing it with `std::process` rather than a
/// plugin keeps the dependency list where it is.
///
/// **The exit status is not checked.** `explorer.exe` returns non-zero when it
/// succeeds, so treating that as failure would report a broken action every
/// time it worked. What can fail is the file being gone, which is checked
/// first.
fn reveal_in_folder(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("找不到 {}。", path.display()));
    }

    #[cfg(target_os = "windows")]
    let spawned = {
        use std::os::windows::process::CommandExt as _;

        let mut command = std::process::Command::new("explorer");
        // Raw rather than `arg`, and this is the whole of a bug that had it
        // opening the folder *above* the right one.
        //
        // Rust quotes any argument containing a space by wrapping the whole
        // thing: `/select,C: b\c.srt` becomes `"/select,C: b\c.srt"`. The
        // quote lands before `/select` instead of around the path, explorer
        // stops the path at the space, and what it is left with is a file that
        // is not there — so it opens the enclosing folder and the file is not
        // highlighted. Every recording whose name has a space in it.
        command.raw_arg(explorer_select_argument(path));
        command.spawn()
    };

    #[cfg(target_os = "macos")]
    let spawned = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();

    #[cfg(all(unix, not(target_os = "macos")))]
    let spawned = std::process::Command::new("xdg-open")
        // No portable "select this file": the nearest thing is the folder.
        .arg(path.parent().unwrap_or(path))
        .spawn();

    spawned
        .map(|_| ())
        .map_err(|e| format!("打不开文件管理器：{e}"))
}

/// Take a file out of the list, leaving its transcript where it is.
#[tauri::command]
fn forget(app: AppHandle, index: usize) {
    let input = {
        let app_state = app.state::<App>();
        let mut state = app_state.state.lock().expect("state mutex poisoned");
        state.forget(index)
    };

    // Forgetting the row without forgetting the record would put it back on the
    // next launch, which is not what "remove this" means.
    if let Some(input) = input {
        let path = history_path();
        let mut history = verse_store::History::load(&path);
        if history.forget(&input) {
            let _ = history.save(&path);
        }
    }

    bridge::push_state(&app);
}

/// Show the transcript being displayed in the platform's file manager.
#[tauri::command]
fn reveal(app: AppHandle) -> Result<(), String> {
    let path = {
        let app_state = app.state::<App>();
        let state = app_state.state.lock().expect("state mutex poisoned");
        state
            .finished()
            .and_then(|done| done.exported.clone())
            .ok_or_else(|| "这个转写还没有写到文件里。".to_string())?
    };

    reveal_in_folder(&path)
}

/// Delete one row's transcript, from the disk as well as the list.
///
/// Takes the row rather than the selection, because the window asks from the
/// list: a ✕ on a row offers to remove it or to delete its file, and neither
/// should depend on what happens to be on screen at the time. If the row being
/// deleted *is* the one on screen, removing it blanks the pane, which is what
/// `forget` does with the selection.
///
/// The irreversible half of taking a file out of the list, and separate from it
/// for that reason. The window asks which of the two is meant; nothing here
/// asks again, because a command that pops its own confirmation cannot be
/// driven by a test.
#[tauri::command]
fn delete_result(app: AppHandle, index: usize) -> Result<(), String> {
    let (path, input) = {
        let app_state = app.state::<App>();
        let state = app_state.state.lock().expect("state mutex poisoned");

        let entry = state
            .files()
            .get(index)
            .ok_or_else(|| "列表里没有这一行。".to_string())?;

        let state::Screen::Done(done) = &entry.screen else {
            return Err("这个文件还没有转写结果，只能从列表里移出。".to_string());
        };

        let path = done
            .exported
            .clone()
            .ok_or_else(|| "这个转写还没有写到文件里。".to_string())?;

        (path, entry.input.clone())
    };

    std::fs::remove_file(&path).map_err(|e| format!("删不掉 {}：{e}", path.display()))?;

    {
        let app_state = app.state::<App>();
        let mut state = app_state.state.lock().expect("state mutex poisoned");
        state.forget(index);
    }

    let record = history_path();
    let mut history = verse_store::History::load(&record);
    if history.forget(&input) {
        let _ = history.save(&record);
    }

    bridge::push_state(&app);
    Ok(())
}

/// Put the previous run's finished transcripts back into the list.
///
/// Runs before the window exists, so the list is already populated when the
/// frontend asks for it and there is no moment where a restored row appears
/// after the drop target has been drawn.
///
/// A row is only restored when its result can actually be read: the entry is a
/// pointer to a file, and one that has been deleted or emptied since is not
/// history. Those are pruned from the record as well, so the next launch does
/// not read them again.
fn restore_history(app: &AppHandle) {
    let path = history_path();

    let mut history = verse_store::History::load(&path);
    let before = history.entries().len();
    history.prune();
    if history.entries().len() != before {
        let _ = history.save(&path);
    }

    let restored: Vec<state::Restored> = history
        .entries()
        .iter()
        .filter_map(|past| {
            let text = std::fs::read_to_string(&past.output).ok()?;
            // A file that is not subtitles is skipped rather than shown as an
            // empty transcript: `None` means "no cue in it", and an empty row
            // that opens to nothing is worse than no row.
            let transcript = verse_core::export::parse_srt(&text)?;

            Some(state::Restored {
                input: past.input.clone(),
                engine: past.engine.clone(),
                output: past.output.clone(),
                transcript,
            })
        })
        .collect();

    let app_state = app.state::<App>();
    app_state
        .state
        .lock()
        .expect("state mutex poisoned")
        .restore(restored);
}

/// Put back the files the last process never finished.
///
/// The record is skipped entirely when there is nothing in it, which is the
/// usual case and the one that must not write anything.
fn restore_pending(app: &AppHandle) {
    let record = settings::Pending::load(&pending_path());
    if record.files.is_empty() {
        return;
    }

    // The engine comes back too, because the resume log is keyed on it and a
    // queue resumed under a different one would redo the work. Checked against
    // the registry first: a record written by a version whose engines have
    // since changed should run under the default rather than refuse to start.
    let mut registry = verse_core::Registry::new();
    verse_asr::register_builtin_engines(&mut registry);

    // Scoped so the lock is released before the push below — `push_state` takes
    // the same mutex, and the standard one is not reentrant.
    {
        let app_state = app.state::<App>();
        let mut state = app_state.state.lock().expect("state mutex poisoned");

        if !record.engine.is_empty() && registry.engine(&record.engine).is_some() {
            let _ = state.set_model(record.engine.clone());
        }

        // Filtered here and not there: the record is a list of paths, and one
        // that has been moved since is not work waiting to be done — but
        // finding that out is a question for the filesystem, and the state
        // module does not touch it.
        let still_there: Vec<PathBuf> = record
            .files
            .into_iter()
            .filter(|input| input.is_file())
            .collect();
        state.restore_pending(still_there);
    }

    // Rewrite the record in the form the restore left it: a path that has been
    // moved since is dropped there, and the file should stop claiming it.
    bridge::push_state(app);
}

/// Whether the demonstration can be run at all.
///
/// Asked on mount so the window can offer it *without* running it. It used to
/// run itself whenever `VERSE_DEMO` was set, which meant a shell that had that
/// variable exported — as the development instructions do — got a demo job on
/// every launch, and looked like the program doing something on its own.
#[cfg(debug_assertions)]
#[tauri::command]
fn demo_available() -> bool {
    demo::enabled()
}

#[cfg(not(debug_assertions))]
#[tauri::command]
fn demo_available() -> bool {
    false
}

/// Put the window through a job it did not have to wait for.
///
/// Two functions with one name rather than one function with a `#[cfg]` block:
/// each configuration compiles exactly one of them, so a release build has no
/// code that can publish a job at all. That is a stronger statement than
/// "unreachable", and it is the one that matters — this is a development tool
/// sitting in the same binary a person installs.
#[cfg(debug_assertions)]
#[tauri::command]
fn demo_progress(app: AppHandle) -> Result<bool, String> {
    demo::run(app)
}

#[cfg(not(debug_assertions))]
#[tauri::command]
fn demo_progress(_app: AppHandle) -> Result<bool, String> {
    // Not an error: a release build not having a development tool is the
    // correct state of affairs, and a window that complained about it would be
    // complaining about working as intended.
    Ok(false)
}

/// Look at what is about to be handed over, before anything starts.
///
/// The window asks this on every drop and every selection. The rule lives here
/// rather than in the frontend so there is one list of what this program reads
/// — the same one `verse-core` gives the command line for expanding a folder.
#[tauri::command]
fn check_files(paths: Vec<String>) -> bridge::CheckResult {
    let mut usable = Vec::new();
    let mut refused = Vec::new();

    for path in paths {
        let path = PathBuf::from(path);
        match bridge::refusal_for(&path) {
            None => usable.push(path.to_string_lossy().into_owned()),
            Some(refusal) => refused.push(refusal),
        }
    }

    bridge::CheckResult {
        usable,
        refused,
        accepted: verse_core::AUDIO_EXTENSIONS
            .iter()
            .map(|extension| (*extension).to_string())
            .collect(),
    }
}

/// Run the file being shown again, after a failure.
#[tauri::command]
fn retry(app: AppHandle) -> Result<(), String> {
    let models = models_dir();

    let effect = {
        let app_state = app.state::<App>();
        let mut state = app_state.state.lock().expect("state mutex poisoned");
        state.retry()
    };

    if matches!(effect, Effect::Transcribe(_)) {
        let _ = app.emit(UPDATE, Update::Cleared);
        bridge::push_state(&app);
    }

    if let Effect::Transcribe(input) = effect {
        bridge::start(&app, input, models);
    }

    Ok(())
}

/// Show a different file from the list.
#[tauri::command]
fn select(app: AppHandle, index: usize) {
    {
        let app_state = app.state::<App>();
        let mut state = app_state.state.lock().expect("state mutex poisoned");
        state.select(index);
    }
    bridge::push_state(&app);
}

/// Choose an engine.
///
/// Refused while a job is running — the state decides that — and refused for a
/// model that is not on disk. The second one is not a formality: choosing an
/// engine that cannot run leaves the window in a state where the next file has
/// no recogniser, and the row already says 需要下载 with the button to fix it.
#[tauri::command]
fn select_model(app: AppHandle, id: String) -> Result<(), String> {
    let models_root = models_dir();

    let catalog = Catalog::load_or_embedded(&models_root.join("catalog.json"))
        .map_err(|e| e.message().to_string())?;
    let spec = catalog
        .find(&id)
        .ok_or_else(|| format!("模型清单里没有 {id} 这个模型。"))?;

    if !catalog.all_present(spec, &models_root) {
        return Err(format!("{id} 还没有下载完，先在左边的卡片里下载。"));
    }

    let (changed, current) = {
        let app_state = app.state::<App>();
        let mut state = app_state.state.lock().expect("state mutex poisoned");
        let changed = state.set_model(id.clone()) == Effect::EngineChanged;
        (changed, state.model().to_string())
    };

    if changed {
        // The loaded model belongs to the engine that was just replaced.
        // Without this it stays resident — up to a gigabyte — until some later
        // job happens to want a different one, or until the idle sweep
        // notices. Safe to call here for the very reason the state refused the
        // change while a job was running: nothing is holding the model.
        let app_state = app.state::<App>();
        app_state.keeper.release();
        bridge::push_state(&app);
        return Ok(());
    }

    if current == id {
        // Already the chosen engine; nothing to do and nothing to report.
        Ok(())
    } else {
        // The state refused, which it only does while a job holds the model.
        // Saying so is better than a picker that silently does not take.
        Err("正在转写，先等它结束或者取消，再换模型。".to_string())
    }
}

/// Start the next file waiting in the queue.
///
/// The queue normally advances on its own: `take_next` is called by the bridge
/// as a job finishes. This command exists for the one queue that has nothing in
/// front of it — the files a previous process was interrupted part-way through,
/// put back as waiting rows by `restore_pending`. Until this is called they sit
/// there, which is deliberate: a window that started recognising a batch the
/// moment it opened would be doing work nobody asked for.
///
/// Returns as soon as the work is under way. Everything else arrives on
/// [`UPDATE`], as `transcribe` does.
#[tauri::command]
fn resume(app: AppHandle) {
    let models = models_dir();

    let next = {
        let app_state = app.state::<App>();
        let mut state = app_state.state.lock().expect("state mutex poisoned");
        state.start_next()
    };

    match next {
        Some(input) => bridge::start(&app, input, models),
        // Nothing waiting, or something already running. Either way the window
        // is owed an answer, and the current state is the answer.
        None => bridge::push_state(&app),
    }
}

/// Ask the running job to stop.
#[tauri::command]
fn cancel(app: AppHandle) {
    bridge::cancel(&app);

    // Reflect the request immediately. The job acknowledges with
    // `JobCancelled`, which is what returns the screen to the drop target.
    {
        let app_state = app.state::<App>();
        let mut state = app_state.state.lock().expect("state mutex poisoned");
        state.cancel();
    }
    bridge::push_state(&app);
}

/// Write the finished transcript out.
///
/// The window chooses the path rather than the backend: it already has the
/// dialog plugin, whose `save()` returns a path, whereas opening a dialog in
/// Rust means a callback-shaped API threaded through a command that otherwise
/// returns immediately.
///
/// The format comes from the chosen file's extension, and an unrecognised one
/// is refused rather than guessed at — writing SRT into a file named `.txt`
/// would be worse than saying so.
#[tauri::command]
fn export(app: AppHandle, path: String) -> Result<(), String> {
    let destination = PathBuf::from(&path);

    let format = destination
        .extension()
        .and_then(|extension| extension.to_str())
        .and_then(ExportFormat::from_extension)
        .ok_or_else(|| format!("看不懂这个扩展名：{path}，请用 .srt 或 .txt 结尾。"))?;

    let (index, rendered, written) = {
        let app_state = app.state::<App>();
        let state = app_state.state.lock().expect("state mutex poisoned");

        let index = state
            .selected()
            .ok_or_else(|| "现在没有可以导出的转写结果。".to_string())?;
        let done = state
            .finished()
            .ok_or_else(|| "现在没有可以导出的转写结果。".to_string())?;

        (index, format.render(&done.transcript), destination.clone())
    };

    std::fs::write(&written, rendered)
        .map_err(|e| format!("写不进 {}：{e}", written.display()))?;

    {
        let app_state = app.state::<App>();
        let mut state = app_state.state.lock().expect("state mutex poisoned");
        state.note_exported(index, written);
    }
    bridge::push_state(&app);

    Ok(())
}

/// Fetch a model, from the panel or from the file that is waiting for it.
///
/// One command for both, because they are the same job: download what the
/// catalogue lists under this id. The difference is only what happens
/// afterwards — a file waiting on this exact model starts on its own, which is
/// what the `NeedsModel` screen has been carrying the path for since it was
/// written.
#[tauri::command]
fn fetch_model(app: AppHandle, model: String) -> Result<(), String> {
    let models_root = models_dir();

    let catalog = Catalog::load_or_embedded(&models_root.join("catalog.json"))
        .map_err(|e| e.message().to_string())?;
    let Some(spec) = catalog.find(&model) else {
        return Err(format!("模型清单里没有 {model} 这个模型。"));
    };

    if catalog.all_present(spec, &models_root) {
        return Err(format!("{model} 已经在本机了。"));
    }

    // Whatever this model cannot run without. Fetched first, and quietly:
    // these are small, they have no card of their own, and a second progress
    // bar for two megabytes nobody chose would be noise. A failure still has
    // to reach the window, though — it is why the download did not happen.
    let needed: Vec<verse_model::ModelSpec> = catalog
        .requirements(spec)
        .into_iter()
        .filter(|extra| !Downloader::is_present(extra, &models_root))
        .cloned()
        .collect();

    let reporting = app.clone();
    let wanted = model.clone();
    let spec = spec.clone();

    std::thread::spawn(move || {
        for extra in &needed {
            let done = matches!(
                Downloader::new().fetch(extra, &models_root, |_| {}, &CancelToken::new()),
                Ok(DownloadState::Ready)
            );
            if !done {
                let _ = reporting.emit(
                    UPDATE,
                    Update::Download {
                        model: wanted.clone(),
                        download: DownloadView::Failed {
                            reason: format!("{} 下载失败，这个模型还不能用。", extra.display_name),
                        },
                    },
                );
                return;
            }
        }

        // The whole model, not the file in flight. The downloader reports one
        // file at a time and knows nothing about the others, so its own figures
        // fill up once per file — Qwen3-ASR showed "44 MB" and looked finished
        // while it was on the first of five.
        let mut progress = ModelProgress::of(&spec, &models_root);
        let model_total = progress.total;

        let outcome = Downloader::new().fetch(
            &spec,
            &models_root,
            move |state| {
                let model_received = progress.observe(state);

                let app_state = reporting.state::<App>();
                app_state
                    .state
                    .lock()
                    .expect("state mutex poisoned")
                    .download_changed(state.clone());

                let download = match DownloadView::from(state) {
                    DownloadView::Fetching {
                        file,
                        received_bytes,
                        total_bytes,
                        ..
                    } => DownloadView::Fetching {
                        file,
                        received_bytes,
                        total_bytes,
                        model_received_bytes: model_received,
                        model_total_bytes: model_total,
                    },
                    other => other,
                };

                let _ = reporting.emit(
                    UPDATE,
                    Update::Download {
                        model: wanted.clone(),
                        download,
                    },
                );
            },
            &CancelToken::new(),
        );

        if !matches!(outcome, Ok(DownloadState::Ready)) {
            return;
        }

        // Only the file that was waiting for *this* model. The panel can fetch
        // anything, and starting a job whose model is still missing would turn
        // a working download into a failure about something else.
        let start = {
            let app_state = app.state::<App>();
            let mut state = app_state.state.lock().expect("state mutex poisoned");
            if state.waiting_for(&model) {
                matches!(state.model_ready(), Effect::Transcribe(_))
            } else {
                false
            }
        };

        // The file screens, whether or not a job was waiting on this model.
        //
        // **Not the model cards.** They read a catalogue the window fetches by
        // a command of its own, and this push carries no part of it. An earlier
        // version of this comment claimed otherwise, and the window believed it:
        // the card went on saying 需要下载 and refusing to be picked until the
        // next launch. The window re-reads the catalogue when the `ready` event
        // reaches it; this line is for the screens that are not that card.
        bridge::push_state(&app);

        if start {
            let input = {
                let app_state = app.state::<App>();
                let state = app_state.state.lock().expect("state mutex poisoned");
                match state.screen() {
                    crate::state::Screen::Working(working) => Some(working.input.clone()),
                    _ => None,
                }
            };
            if let Some(input) = input {
                bridge::start(&app, input, models_root);
            }
        }
    });

    Ok(())
}

/// Take a model the user already has.
///
/// The files are copied into `models/<id>/` rather than referenced where they
/// lie. The pipeline looks in one place; a second lookup path is a second
/// thing that can disagree with it.
#[tauri::command]
fn import_model(app: AppHandle, path: String) -> Result<(), String> {
    let models_root = models_dir();
    let source = PathBuf::from(&path);

    if !source.is_dir() {
        return Err(format!("这不是一个文件夹：{}", source.display()));
    }

    let model = {
        let app_state = app.state::<App>();
        let state = app_state.state.lock().expect("state mutex poisoned");
        state.model().to_string()
    };

    let catalog = Catalog::load_or_embedded(&models_root.join("catalog.json"))
        .map_err(|e| e.message().to_string())?;
    let spec = catalog
        .find(&model)
        .ok_or_else(|| format!("模型目录里没有 {model} 这个模型。"))?;

    let root = model_root(spec, &source)?;

    let target = Downloader::directory_for(spec, &models_root);
    for file in &spec.files {
        let to = target.join(&file.local);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("建不了 {}：{e}", parent.display()))?;
        }
        std::fs::copy(root.join(&file.local), &to)
            .map_err(|e| format!("复制 {} 失败：{e}", file.local))?;
    }

    // Checked after the copy rather than trusting it. A half-copied model is
    // the kind of thing that only shows up as a strange recognition failure.
    if !Downloader::is_present(spec, &models_root) {
        return Err("复制过去的文件不完整，请重新选择一个模型文件夹。".to_string());
    }

    start_waiting_job(&app);
    Ok(())
}

/// Where the model's files are inside the folder the user pointed at.
///
/// Two layouts are worth accepting, because both are what people end up with
/// after unpacking a download: the folder *is* the model directory, or it
/// contains one named after the model.
fn model_root(spec: &verse_model::ModelSpec, source: &Path) -> Result<PathBuf, String> {
    if all_present(spec, source) {
        return Ok(source.to_path_buf());
    }

    let nested = source.join(&spec.id);
    if all_present(spec, &nested) {
        return Ok(nested);
    }

    let missing: Vec<&str> = spec
        .files
        .iter()
        .filter(|file| !source.join(&file.local).is_file())
        .map(|file| file.local.as_str())
        .collect();

    Err(format!("这个文件夹里缺少模型文件：{}", missing.join("、")))
}

/// Whether every file of `spec` is present under `root`.
fn all_present(spec: &verse_model::ModelSpec, root: &Path) -> bool {
    spec.files.iter().all(|file| root.join(&file.local).is_file())
}

/// The model is in place; start whatever was waiting on it.
fn start_waiting_job(app: &AppHandle) {
    let effect = {
        let app_state = app.state::<App>();
        let mut state = app_state.state.lock().expect("state mutex poisoned");
        state.model_ready()
    };

    bridge::push_state(app);

    if let Effect::Transcribe(input) = effect {
        bridge::start(app, input, models_dir());
    }
}

/// What the 关于 dialog shows: attribution, licence, and what this is.
#[tauri::command]
fn about() -> about::About {
    about::about()
}

/// Leave a finished or failed screen, back to the drop target.
#[tauri::command]
fn reset(app: AppHandle) {
    {
        let app_state = app.state::<App>();
        let mut state = app_state.state.lock().expect("state mutex poisoned");
        state.reset();
    }
    bridge::push_state(&app);
}

/// Start the application.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            app.manage(App::new());

            let handle = app.handle().clone();
            restore_history(&handle);
            restore_pending(&handle);
            let bus = {
                let state = handle.state::<App>();
                state.bus.clone()
            };

            // One subscriber for the whole application. The forwarding thread
            // owns it and folds everything it receives into the state.
            bridge::spawn_forwarder(handle.clone(), bus.subscribe_all());

            // **The window is not the program.** A long recording is minutes of
            // work, and until there was a tray it died with the window: no
            // partial result, no warning. Everything below is what makes
            // closing the window a choice rather than an accident.
            tray::build(&handle)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                tray::close_requested(window, api);
            }
        })
        .invoke_handler(tauri::generate_handler![
            hardware,
            current_state,
            demo_progress,
            demo_available,
            models,
            select,
            retry,
            check_files,
            forget,
            reveal,
            delete_result,
            select_model,
            transcribe,
            resume,
            cancel,
            export,
            fetch_model,
            import_model,
            about,
            reset
        ])
        .run(tauri::generate_context!())
        .expect("failed to start the Verse window");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use bridge::refusal_for;
    use verse_model::{ModelFile, ModelSpec};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("verse-app-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn models_live_under_the_users_data_directory() {
        // Two failures in one assertion. A per-machine `.msi` installs into
        // `C:\Program Files`, where a standard user cannot create a directory,
        // so models beside the executable meant a `.msi` install could never
        // download one. And weights sitting in the install directory are taken
        // away by an uninstall that was only ever meant to remove the program.
        let roots = verse_store::Roots {
            local_app_data: Some(PathBuf::from("C:/Users/someone/AppData/Local")),
            ..Default::default()
        };

        assert_eq!(
            models_root(None, &roots),
            PathBuf::from("C:/Users/someone/AppData/Local/Verse/models")
        );
    }

    #[test]
    fn an_explicit_model_directory_beats_the_data_directory() {
        // `VERSE_MODELS` is how a checkout's `models/` is found, and how anyone
        // who has staged weights somewhere else says so.
        let roots = verse_store::Roots {
            local_app_data: Some(PathBuf::from("/data")),
            ..Default::default()
        };

        assert_eq!(
            models_root(Some(PathBuf::from("models")), &roots),
            PathBuf::from("models")
        );
    }

    #[test]
    fn models_follow_the_data_directorys_own_fallbacks() {
        // Not a second set of rules: whatever `data_dir` decides is where
        // models go, so a machine without local app data is not a case this
        // function has to know about.
        let roots = verse_store::Roots {
            home: Some(PathBuf::from("/home/me")),
            ..Default::default()
        };

        assert_eq!(
            models_root(None, &roots),
            PathBuf::from("/home/me/.verse/models")
        );
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, b"x").expect("write");
    }

    fn machine(avx2: bool, cores: usize) -> HardwareProfile {
        HardwareProfile {
            avx2,
            fma: avx2,
            cores,
        }
    }

    #[test]
    fn a_machine_with_nothing_to_report_gets_no_notice() {
        // The common case, and the one that has to stay silent: a bar about
        // hardware on a machine where nothing is wrong is noise.
        assert_eq!(hardware_summary(machine(true, 8)).degraded, None);
    }

    #[test]
    fn a_reduced_machine_gets_a_sentence_the_window_can_show() {
        let notice = hardware_summary(machine(false, 8))
            .degraded
            .expect("a machine without AVX2 is worked around, and must say so");

        assert!(
            notice.contains("AVX2"),
            "it has to name what is missing, or it is not worth showing: {notice}"
        );
        assert!(
            notice.contains("仍然可用"),
            "a reduced machine is slower, not broken, and the sentence has to \
             say so rather than read as a failure: {notice}"
        );
        assert!(
            notice.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "ui-design.md §8 keeps the window's copy in Chinese: {notice}"
        );
    }

    #[test]
    fn the_two_weaknesses_do_not_get_the_same_sentence() {
        // A cursor on the wrong arm would give every reduced machine the same
        // sentence, which is a quieter bug than no sentence at all.
        assert_ne!(window_notice(Weakness::NoAvx2), window_notice(Weakness::SingleCore));
    }

    #[test]
    fn the_hardware_summary_is_shaped_the_way_the_window_reads_it() {
        // The window takes this apart by name. A field renamed here arrives as
        // `undefined` and the notice simply does not appear — a missing bar
        // rather than an error, which is the failure this pins shut.
        let value = serde_json::to_value(hardware_summary(machine(false, 8))).expect("serializes");

        assert_eq!(
            value,
            serde_json::json!({
                "avx2": false,
                "cores": 8,
                "threads": 7,
                "degraded": window_notice(Weakness::NoAvx2),
            })
        );
    }

    /// The shape Qwen3 actually has: a nested tokenizer directory.
    fn spec() -> ModelSpec {
        ModelSpec {
            id: "sensevoice".to_string(),
            description: None,
            requires: Vec::new(),
            display_name: "SenseVoice".to_string(),
            files: vec![
                ModelFile {
                    remote: "model.int8.onnx".to_string(),
                    local: "model.int8.onnx".to_string(),
                    size: None,
                    sha256: None,
                },
                ModelFile {
                    remote: "vocab.json".to_string(),
                    local: "tokenizer/vocab.json".to_string(),
                    size: None,
                    sha256: None,
                },
            ],
            mirrors: Vec::new(),
        }
    }

    /// A model of two files with known sizes, for the progress arithmetic.
    fn two_file_spec() -> ModelSpec {
        ModelSpec {
            id: "sensevoice".to_string(),
            display_name: "SenseVoice-Small".to_string(),
            description: None,
            requires: Vec::new(),
            files: vec![
                ModelFile {
                    remote: "a.onnx".to_string(),
                    local: "a.onnx".to_string(),
                    size: Some(100),
                    sha256: None,
                },
                ModelFile {
                    remote: "b.onnx".to_string(),
                    local: "b.onnx".to_string(),
                    size: Some(50),
                    sha256: None,
                },
            ],
            mirrors: Vec::new(),
        }
    }

    fn fetching(file: &str, received: u64) -> verse_model::DownloadState {
        verse_model::DownloadState::Fetching {
            mirror: "hf-mirror".to_string(),
            file: file.to_string(),
            received,
            total: None,
        }
    }

    #[test]
    fn the_progress_counts_the_whole_model_and_not_the_file_in_flight() {
        // The bug this pins: the downloader's own figures are per file, so the
        // window showed "44 MB" and looked finished while Qwen3-ASR was on the
        // first of its five files.
        let mut progress = ModelProgress::of(&two_file_spec(), Path::new("nonexistent-root"));

        assert_eq!(progress.total, Some(150));
        assert_eq!(progress.observe(&fetching("a.onnx", 40)), Some(40));
        assert_eq!(progress.observe(&fetching("a.onnx", 100)), Some(100));

        // Moving to the second file keeps what the first one put in.
        assert_eq!(progress.observe(&fetching("b.onnx", 1)), Some(101));
        assert_eq!(progress.observe(&fetching("b.onnx", 50)), Some(150));
    }

    #[test]
    fn a_retried_file_is_not_counted_twice() {
        // A mirror that fails and is retried re-reports the same name from
        // zero. Counting the name change alone would have added the first
        // attempt to the total and left the bar past its own end.
        let mut progress = ModelProgress::of(&two_file_spec(), Path::new("nonexistent-root"));

        progress.observe(&fetching("a.onnx", 80));
        // The same file, from the start, on another mirror.
        assert_eq!(progress.observe(&fetching("a.onnx", 5)), Some(5));
    }

    #[test]
    fn a_model_with_an_unknown_size_reports_no_total_rather_than_a_wrong_one() {
        let mut spec = two_file_spec();
        spec.files[1].size = None;

        let mut progress = ModelProgress::of(&spec, Path::new("nonexistent-root"));

        assert_eq!(progress.total, None, "a partial total is not a total");
        assert_eq!(progress.observe(&fetching("a.onnx", 40)), None);
    }

    #[test]
    fn the_explorer_argument_quotes_the_path_and_not_the_switch() {
        // Properties rather than an expected string, which would be nothing but
        // quotes and separators and would amount to asserting that the test was
        // typed correctly.
        //
        // `Command::arg` puts one quote around the whole argument —
        // `/select,C:/a b/c.srt` becomes `"/select,C:/a b/c.srt"` — and explorer
        // then stops the path at the space, finds no such file, and opens the
        // folder above the right one.
        //
        // Forward slashes because a path is a path either way on Windows and
        // this file has enough escaping in it.
        let argument = explorer_select_argument(Path::new("C:/a b/c.srt"));

        assert!(
            argument.starts_with("/select,"),
            "the switch comes first and unquoted: {argument}"
        );
        assert!(
            argument.ends_with('"'),
            "and the path is closed at its end: {argument}"
        );
        assert!(
            argument.contains("C:/a b/c.srt"),
            "with every character of it, the space included: {argument}"
        );
        assert_eq!(
            argument.find('"'),
            Some("/select,".len()),
            "the opening quote is around the path and not around the switch: {argument}"
        );
    }

    #[test]
    fn a_file_it_cannot_read_is_refused_before_anything_starts() {
        let dir = scratch("refusal");

        let sheet = dir.join("notes.txt");
        touch(&sheet);
        let folder = dir.join("a-folder");
        std::fs::create_dir_all(&folder).expect("create dir");

        let missing = dir.join("gone.mp3");

        let cases: [(&std::path::Path, &str); 3] = [
            (&folder, "文件夹"),
            (&missing, "找不到"),
            (&sheet, "不是音频"),
        ];

        for (path, expected) in cases {
            let refusal = refusal_for(path).unwrap_or_else(|| {
                panic!("{} should be refused", path.display())
            });
            assert!(
                refusal.reason.contains(expected),
                "{}: expected {expected:?} in {:?}",
                path.display(),
                refusal.reason
            );
        }
    }

    #[test]
    fn an_audio_file_is_let_through_whatever_its_case() {
        // `.MP3` is an mp3. Windows capitalises extensions without asking, and
        // refusing a file over the case of its name would be the wrong way
        // round in every sense.
        let dir = scratch("refusal-ok");

        for name in ["a.wav", "b.MP3", "c.M4A", "d.opus", "e.mkv"] {
            let path = dir.join(name);
            touch(&path);
            assert!(
                refusal_for(&path).is_none(),
                "{name} should be accepted"
            );
        }
    }

    #[test]
    fn the_check_sorts_what_it_was_given_without_dropping_any_of_it() {
        let dir = scratch("refusal-mixed");
        let good = dir.join("keep.wav");
        let bad = dir.join("drop.txt");
        touch(&good);
        touch(&bad);

        let result = check_files(vec![
            good.to_string_lossy().into_owned(),
            bad.to_string_lossy().into_owned(),
        ]);

        assert_eq!(result.usable.len(), 1, "the audio file goes through");
        assert_eq!(result.refused.len(), 1, "the other does not");
        assert!(
            !result.accepted.is_empty(),
            "the dialog needs the formats to name them"
        );
    }

    #[test]
    fn every_command_the_window_calls_is_registered() {
        // A command the window reaches for but nobody registered fails inside
        // a webview console nobody is watching, and the person sees a button
        // that does nothing. The names exist in two languages and nothing but
        // this keeps them together — which matters because renaming a screen's
        // worth of commands is exactly the kind of change that misses one.
        //
        // Both lists are read from source rather than from the built program:
        // `generate_handler!` is a macro, and the frontend is not Rust.
        let frontend = include_str!("../ui/src/lib/api.ts");
        let backend = include_str!("lib.rs");

        let invoked: BTreeSet<String> = frontend
            .match_indices("invoke<")
            .filter_map(|(at, _)| {
                let rest = &frontend[at..];
                let open = rest.find('(')?;
                let quote = rest[open..].find('"')? + open + 1;
                let close = rest[quote..].find('"')? + quote;
                Some(rest[quote..close].to_string())
            })
            .collect();

        let handlers = backend
            .split_once("generate_handler![")
            .and_then(|(_, rest)| rest.split_once(']'))
            .map(|(list, _)| list)
            .expect("the handler list is in this file; if it moved, move this test");

        let registered: BTreeSet<String> = handlers
            .split(',')
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect();

        assert!(!invoked.is_empty(), "the frontend parser found nothing");

        let unanswered: Vec<&String> = invoked.difference(&registered).collect();
        let uncalled: Vec<&String> = registered.difference(&invoked).collect();

        assert!(
            unanswered.is_empty(),
            "the window calls commands nothing answers: {unanswered:?}"
        );
        assert!(
            uncalled.is_empty(),
            "commands are registered that the window never calls: {uncalled:?}"
        );
    }

    #[test]
    fn every_engine_the_registry_offers_is_named_the_same_way_in_the_catalogue() {
        // The picker is the catalogue filtered by the registry, so an engine
        // missing from the catalogue can never be chosen, and a catalogue entry
        // with no engine behind it is a row that could never run. The two lists
        // are written by hand in different crates, which is exactly why this is
        // a test rather than a comment asking people to keep them in step.
        let mut registry = verse_core::Registry::new();
        verse_asr::register_builtin_engines(&mut registry);

        let catalog = Catalog::embedded().expect("the catalogue ships in the binary");

        for descriptor in registry.engines() {
            let spec = catalog.find(descriptor.id).unwrap_or_else(|| {
                panic!(
                    "engine {} has no catalogue entry, so the picker cannot offer it",
                    descriptor.id
                )
            });

            assert_eq!(
                spec.display_name, descriptor.display_name,
                "{} is named one way in the catalogue and another in the registry;                  the window shows the catalogue's, so they would disagree",
                descriptor.id
            );
        }
    }

    #[test]
    fn every_catalogue_model_says_what_it_is_for() {
        // The panel shows these. A model with no description is a row a person
        // has to choose between without being told the difference.
        let catalog = Catalog::embedded().expect("the catalogue ships in the binary");

        for spec in &catalog.models {
            let description = spec
                .description
                .as_deref()
                .unwrap_or_else(|| panic!("{} has no description", spec.id));

            assert!(
                !description.trim().is_empty(),
                "{} has an empty description",
                spec.id
            );
        }
    }

    #[test]
    fn a_folder_that_is_the_model_directory_is_accepted() {
        let dir = scratch("flat");
        touch(&dir.join("model.int8.onnx"));
        touch(&dir.join("tokenizer/vocab.json"));

        assert_eq!(model_root(&spec(), &dir).unwrap(), dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_containing_the_model_directory_is_accepted() {
        // What someone ends up with after unpacking an archive that kept the
        // top-level directory.
        let dir = scratch("nested");
        touch(&dir.join("sensevoice/model.int8.onnx"));
        touch(&dir.join("sensevoice/tokenizer/vocab.json"));

        assert_eq!(model_root(&spec(), &dir).unwrap(), dir.join("sensevoice"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_missing_a_file_says_which() {
        let dir = scratch("incomplete");
        touch(&dir.join("model.int8.onnx"));
        // The tokenizer is missing, which is exactly the case that used to be
        // a silent failure on the download path.

        let message = model_root(&spec(), &dir).expect_err("should refuse");
        assert!(message.contains("tokenizer/vocab.json"), "got: {message}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_folder_is_refused_rather_than_half_accepted() {
        let dir = scratch("empty");
        assert!(model_root(&spec(), &dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
