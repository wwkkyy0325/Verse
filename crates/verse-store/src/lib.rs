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
//!
//! A fourth, the transcription cache itself, builds on [`identity`] and lives
//! alongside them.
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

pub mod dirs;
pub mod identity;
pub mod output;

pub use dirs::{data_dir, notices, output_dir, Roots, FOLDER};
pub use identity::FileId;
pub use output::{destination, Choice, Ownership};
