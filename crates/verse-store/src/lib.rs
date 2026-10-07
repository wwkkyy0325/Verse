//! Where Verse keeps things, and which file a result belongs in.
//!
//! This crate answers three questions that all reduce to *"where does this live
//! on the user's machine"*:
//!
//! - [`dirs`] — which directory output goes to, and which one belongs to the
//!   program.
//! - [`identity`] — whether the file in hand is the same file as last time.
//! - [`output`] — which name inside that directory this particular transcript
//!   takes, given that a folder can hold two recordings with one name.
//! - [`key`] — what makes two runs the same run.
//! - [`cache`], [`entry`] — the transcription cache itself, built on the two
//!   above.
//!
//! **It depends on no workspace crate.** `verse-core`'s empty dependency list
//! is the project's strongest structural rule, and the wire-format convention
//! that goes with it — the format lives in the crate that owns the wire, and
//! the boundary converts — is what lets this crate hold the cache's serialised
//! shape while the pipeline keeps its own types. The practical consequence is
//! that the command line can resolve an output path without linking the engine
//! or the FFI behind it.
//!
//! **Nothing here is allowed to be load-bearing for correctness of a
//! transcription.** A cache that cannot be read, a record that has been
//! corrupted, a directory that will not resolve — each costs at most a
//! repeated computation or a differently numbered filename. If any of them
//! could fail a transcription, a bookkeeping file would be more important than
//! the product.

pub mod cache;
pub mod dirs;
pub mod entry;
pub mod history;
pub mod identity;
pub mod key;
pub mod output;
pub mod resume;

pub use cache::{Cache, Usage};
pub use dirs::{data_dir, notices, output_dir, Roots, FOLDER};
pub use entry::{CoverageDto, Entry, SegmentDto, Span, TranscriptDto};
pub use identity::FileId;
pub use key::{caching_refused, content_digest, key, Fingerprint, Verify, SEMANTICS};
pub use history::{History, Past};
pub use output::{destination, Choice, Ownership};
pub use resume::{span_key, Progress};
