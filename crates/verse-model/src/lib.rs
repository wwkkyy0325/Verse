//! Model catalogue and multi-source downloader.
//!
//! Empty until P1a step 11. This is the **only** crate permitted to perform
//! network I/O — keeping it isolated is what makes the offline guarantee
//! structural rather than a matter of discipline (see `design.md` §4.5).
//!
//! Source priority and the verified URL patterns are in `design.md` §6.
