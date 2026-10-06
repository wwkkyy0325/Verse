//! Model catalogue and downloader.
//!
//! This is the **only** crate permitted to originate network I/O, and keeping
//! that isolated is what makes the offline guarantee structural rather than a
//! matter of discipline. The one listening socket — in `verse-cli`'s `serve`
//! command — accepts connections and makes none of its own, which is a
//! different thing from reaching out. See `design.md` §4.5.
//!
//! **Downloads are always user-initiated.** There is no fetch on startup and
//! no background refresh: `fetch` is only reached because someone called it,
//! and `is_present` exists so a caller can check before offering.

pub mod catalog;
pub mod cleanup;
pub mod downloader;
pub mod state;

pub use catalog::{Catalog, Mirror, ModelFile, ModelSpec};
pub use cleanup::{Partial, Usage};
pub use downloader::Downloader;
pub use state::DownloadState;
