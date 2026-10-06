//! Model catalogue and downloader.
//!
//! This is the **only** crate permitted to perform network I/O. Keeping it
//! isolated is what makes the offline guarantee structural rather than a
//! matter of discipline — see `design.md` §4.5.
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
