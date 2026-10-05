//! Tauri build script.
//!
//! Reads `tauri.conf.json`, validates the window and bundle settings, and
//! generates the context that `tauri::generate_context!` compiles in — the
//! window list and, in a release build, the embedded frontend.

fn main() {
    tauri_build::build()
}
