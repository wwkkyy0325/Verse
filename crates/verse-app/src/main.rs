//! Verse desktop application — entry point.

// A release build must not open a console window behind itself. Debug builds
// keep it, because that is where panics and `eprintln!` land.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    verse_app::run();
}
