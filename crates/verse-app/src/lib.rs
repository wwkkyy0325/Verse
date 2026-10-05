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

mod bridge;


// The drop-a-file path is wired end to end. The rest of the state machine —
// the model screen, export, the about dialog — is written and tested but has
// no caller yet, because those screens are steps 12 and 13 of
// tasks/p1b-gui.md. Deleting it would mean deleting tested behaviour and
// writing it again; the allow is here rather than there for that reason.
#[allow(dead_code)]
mod state;

use std::path::PathBuf;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use verse_core::HardwareProfile;
use verse_model::{Catalog, Downloader};

use bridge::{App, Update, UPDATE};
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
            reset
        ])
        .run(tauri::generate_context!())
        .expect("failed to start the Verse window");
}
