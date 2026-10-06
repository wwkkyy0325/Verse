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


// The drop-a-file path is wired end to end. The rest of the state machine —
// the model screen, export, the about dialog — is written and tested but has
// no caller yet, because those screens are steps 12 and 13 of
// tasks/p1b-gui.md. Deleting it would mean deleting tested behaviour and
// writing it again; the allow is here rather than there for that reason.
#[allow(dead_code)]
mod state;

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use verse_core::{CancelToken, ExportFormat, HardwareProfile};
use verse_model::{Catalog, Downloader, DownloadState};

use bridge::{App, DownloadView, Update, UPDATE};
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
    /// Why this machine is being worked around, if it is.
    pub degraded: Option<String>,
}

/// Where models live.
///
/// `VERSE_MODELS` wins when set, which is how a development build finds the
/// checkout's `models/` directory — the executable sits several levels away in
/// `target/`. Otherwise it is a `models` directory beside the executable, which
/// is the installed layout.
///
/// This is one of the questions `ui-design.md` §11 leaves open: a real install
/// should use per-user application data. Nothing here should be built on
/// until that is settled.
fn models_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("VERSE_MODELS") {
        return PathBuf::from(dir);
    }

    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.join("models")))
        .unwrap_or_else(|| PathBuf::from("models"))
}

/// Probe the machine.
///
/// Detection never fails and never blocks startup: an older CPU runs smaller
/// models and is told why.
#[tauri::command]
fn hardware() -> HardwareSummary {
    let profile = HardwareProfile::probe();

    HardwareSummary {
        avx2: profile.avx2,
        cores: profile.cores,
        threads: profile.engine_threads(),
        degraded: profile.tier().notice(),
    }
}

/// What the window should be showing right now.
///
/// Called once when the frontend mounts. Everything after that arrives as an
/// update on [`UPDATE`].
#[tauri::command]
fn current_screen(app: AppHandle) -> bridge::ScreenView {
    let app_state = app.state::<App>();
    let state = app_state.state.lock().expect("state mutex poisoned");
    bridge::view_of(state.screen())
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
                .map(|spec| Downloader::is_present(spec, &models))
                .unwrap_or(false),
            // A broken catalogue is not worth refusing to work over; the model
            // is either on disk or it is not, and the pipeline will say so.
            Err(_) => false,
        };

        let mut state = app_state.state.lock().expect("state mutex poisoned");
        state.file_chosen(input, ready)
    };

    // A new job's transcript must not be appended to the last one's.
    if matches!(effect, Effect::Transcribe(_)) {
        let _ = app.emit(UPDATE, Update::Cleared);
    }
    bridge::push_screen(&app);

    if let Effect::Transcribe(input) = effect {
        bridge::start(&app, input, models);
    }

    Ok(())
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
    bridge::push_screen(&app);
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

    let (rendered, written) = {
        let app_state = app.state::<App>();
        let state = app_state.state.lock().expect("state mutex poisoned");

        let done = state
            .finished()
            .ok_or_else(|| "现在没有可以导出的转写结果。".to_string())?;

        (format.render(&done.transcript), destination.clone())
    };

    std::fs::write(&written, rendered)
        .map_err(|e| format!("写不进 {}：{e}", written.display()))?;

    {
        let app_state = app.state::<App>();
        let mut state = app_state.state.lock().expect("state mutex poisoned");
        state.note_exported(written);
    }
    bridge::push_screen(&app);

    Ok(())
}

/// Fetch the model the waiting file needs.
///
/// Returns as soon as the download is under way. Progress arrives on
/// [`UPDATE`], and when the model lands the transcription that was waiting
/// starts on its own — which is what the `NeedsModel` screen has been
/// carrying the file path for since it was written.
#[tauri::command]
fn fetch_model(app: AppHandle) -> Result<(), String> {
    let models_root = models_dir();

    let effect = {
        let app_state = app.state::<App>();
        let mut state = app_state.state.lock().expect("state mutex poisoned");
        state.fetch_model()
    };

    let Effect::FetchModel { model, .. } = effect else {
        return Err("现在不需要下载模型。".to_string());
    };

    let catalog = Catalog::load_or_embedded(&models_root.join("catalog.json"))
        .map_err(|e| e.message().to_string())?;
    if catalog.find(&model).is_none() {
        return Err(format!("模型目录里没有 {model} 这个模型。"));
    }

    std::thread::spawn(move || {
        let Some(spec) = catalog.find(&model) else {
            return;
        };

        let reporting = app.clone();
        let outcome = Downloader::new().fetch(
            spec,
            &models_root,
            move |state| {
                let app_state = reporting.state::<App>();
                app_state
                    .state
                    .lock()
                    .expect("state mutex poisoned")
                    .download_changed(state.clone());

                let _ = reporting.emit(
                    UPDATE,
                    Update::Download {
                        download: DownloadView::from(state),
                    },
                );
            },
            &CancelToken::new(),
        );

        if matches!(outcome, Ok(DownloadState::Ready)) {
            start_waiting_job(&app);
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

    bridge::push_screen(app);

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
    bridge::push_screen(&app);
}

/// Start the application.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            app.manage(App::new());

            let handle = app.handle().clone();
            let bus = {
                let state = handle.state::<App>();
                state.bus.clone()
            };

            // One subscriber for the whole application. The forwarding thread
            // owns it and folds everything it receives into the state.
            bridge::spawn_forwarder(handle, bus.subscribe_all());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            hardware,
            current_screen,
            transcribe,
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
    use verse_model::{ModelFile, ModelSpec};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("verse-app-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, b"x").expect("write");
    }

    /// The shape Qwen3 actually has: a nested tokenizer directory.
    fn spec() -> ModelSpec {
        ModelSpec {
            id: "sensevoice".to_string(),
            display_name: "SenseVoice".to_string(),
            files: vec![
                ModelFile {
                    remote: "model.int8.onnx".to_string(),
                    local: "model.int8.onnx".to_string(),
                    size: None,
                },
                ModelFile {
                    remote: "vocab.json".to_string(),
                    local: "tokenizer/vocab.json".to_string(),
                    size: None,
                },
            ],
            mirrors: Vec::new(),
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
