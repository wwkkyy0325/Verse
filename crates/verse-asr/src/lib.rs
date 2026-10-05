//! Speech recognition engine implementations.
//!
//! Empty until P1a step 7. This is the only crate that links sherpa-onnx, and
//! therefore the only one whose build needs the native archive — see
//! `design.md` §7.2. The traits it implements live in `verse-core::traits`,
//! so nothing above this crate depends on the FFI.
