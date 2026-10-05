//! Audio decoding (Phase 1) and capture (Phase 2).
//!
//! Empty until P1a step 5 adds `decode` (symphonia → 16 kHz mono PCM), and
//! P2a adds `capture` (cpal plus per-platform loopback). The crate exists now
//! so the workspace layout is settled before implementation starts.
//!
//! `capture` will sit behind a feature flag so Phase 1 builds do not pull in
//! cpal.
