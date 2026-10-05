# Changelog

Append-only record of meaningful changes.

## 2026-10-05 — Initial design

Created `design.md`. Nothing is implemented yet.

Decisions reached:

- **Scope narrowed.** Real-time dictation is out; only file transcription (Phase 1) and live subtitles (Phase 2) remain. This removes global hotkeys and text injection from the project entirely.
- **Engine: official `sherpa-onnx` Rust API**, not the deprecated `thewh1teagle/sherpa-rs`, and not `whisper-rs`.
- **Whisper ruled out on Chinese accuracy**, not just on licensing. Measured Chinese CER of 16–23 for large-v3-turbo versus 3–8 for domestic models. `whisper-rs` also carries open, accumulating memory leaks, which violates the no-leak constraint.
- **candle ruled out** as the ASR runtime for the same reason — its ASR path is Whisper. It is retained for Phase 2 translation (Marian), where it is pure Rust with no accuracy penalty.
- **Default model: Paraformer-large int8** (Apache-2.0). SenseVoice-Small is faster but its license is self-contradictory upstream; it cannot be a default until resolved.
- **China model acquisition corrected.** The assumption that these models are natively on ModelScope is false — API probes 404 for the classic families. Priority is ModelScope (SenseVoice only) → hf-mirror → GitHub proxy → mandatory manual import.
- **Build blocker disproved.** `sherpa-onnx-sys` needs no cmake or C++ toolchain; it downloads a prebuilt archive. The URL is hardcoded to GitHub, but `SHERPA_ONNX_ARCHIVE_DIR` / `SHERPA_ONNX_LIB_DIR` / `HTTPS_PROXY` provide verified escape hatches.

Open at time of writing: punctuation model selection, whether the model ships inside the installer, and the SenseVoice license question.

## 2026-10-05 — P0 framework implemented

Workspace, domain model, and communication backbone are in place. `cargo test --offline` → 9 passed, 0 failed. See `tasks/p0-framework.md`.

Added to `design.md`:

- §4.6 documents the three-layer backbone (registry → router → event bus) and the initial event vocabulary.
- **All traits now live in `verse-core`**, with implementations elsewhere. This is what lets the registry hold engine factories without core linking sherpa-onnx; it corrects the earlier layout note that put `AsrEngine` in `verse-asr`.
- **Model distribution decided:** one lightweight model ships inside the installer for a zero-network first run; heavier models are opt-in downloads. The bundled model must have a clean license, so it is Paraformer-large, not SenseVoice.
- §7.4 records a stale Windows registry proxy that makes cargo send all requests to a dead port. `NO_PROXY='*'` is the workaround; clearing the registry value is the fix.

Dropped `crossbeam-channel` in favour of `std::sync::mpsc`. The bus only needs `recv` / `try_recv` / `recv_timeout` / `try_iter`, all of which std provides. The workspace now has **no external dependencies** and builds fully offline.

## 2026-10-05 — Repository initialized, dev environment fixed

`git init` on `main`. Initial commit covers the workspace, the P0 backbone and the docs.

The cargo network failure recorded above was deeper than first diagnosed. Deleting the `ProxyServer` registry value did not fix it, because the value libcurl actually reads is the binary `DefaultConnectionSettings` blob under `...\Internet Settings\Connections` — which still embeds `127.0.0.1:7890`.

Rewriting that blob was rejected: it is opaque binary, and a mistake would break system-wide network settings. The fix instead is `[http] proxy = ""` in `CARGO_HOME/config.toml`. It applies to cargo alone, touches nothing system-wide, and TUNA is fast enough to go direct. `NO_PROXY='*'` remains a valid per-command workaround.
