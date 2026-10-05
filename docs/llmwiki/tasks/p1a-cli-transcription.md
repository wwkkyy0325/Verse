# P1a — CLI file transcription

Goal: end-to-end file transcription from the command line on Windows, fully offline at runtime.

Exit criteria: `verse transcribe <file> -o <out.srt>` produces a punctuated Chinese SRT with correct timestamps, on a machine with no network access after models are cached.

Ordering note: steps 1–10 get a working pipeline using a manually placed model. The downloader (step 11) automates what steps 4 and 10 do by hand. This surfaces the real risks (sherpa-onnx build, Chinese network) before investing in infrastructure.

---

## [x] 1. Workspace skeleton

Done in P0 — see `p0-framework.md`. Six crates, dependency graph settled.

**Verify:** `cargo build` succeeds; `cargo tree` shows the six members. ✅

## [x] 2. Configure the crates mirror

This machine already runs TUNA rather than the rsproxy.cn suggested in `design.md` §7.1 — TUNA is verified reachable and faster here, so it stays. The real blocker was a stale Windows registry proxy pointing cargo at a dead port; `[http] proxy = ""` in `CARGO_HOME/config.toml` resolves it (see `design.md` §7.4).

**Verify:** `cargo fetch` completes with a fresh dependency and no per-command workaround. ✅

## [x] 3. Get sherpa-onnx to build

The highest-risk step, and it passed. `sherpa-onnx` pinned at `1.13.8` with the default `static` feature.

What actually happened:

- The build script's URL is confirmed hardcoded with **no override variable**: `github.com/k2-fsa/sherpa-onnx/releases/download/v1.13.8/sherpa-onnx-v1.13.8-win-x64-static-MT-Release-lib.tar.bz2`, 123,206,268 bytes.
- Direct GitHub is unreachable from here; `ghfast.top`, `gh-proxy.com` and `ghproxy.net` all serve the file with range support.
- **No cmake and no C++ toolchain were needed** — the archive bundles prebuilt static libraries including onnxruntime, confirming the research.
- `SHERPA_ONNX_ARCHIVE_DIR` is set in `.cargo/config.toml` via cargo's `[env]` section rather than a shell export, so command-line and IDE builds both resolve it. The archive lives in `.sherpa-onnx-libs/` (gitignored) so `cargo clean` does not discard it.

**Verify:** `cargo test -p verse-asr` links and runs. An empty crate would never exercise the linker, so the test constructs a `LinearResampler` (48 kHz → 16 kHz, no model files needed) and checks the output length. Passing proves the native library is genuinely linked and callable. ✅

**Bonus findings:** sherpa-onnx ships its own `vad`, `offline_punctuation` and `resampler` modules, so steps 6 and 8 need no extra libraries. `OfflineQwen3ASRModelConfig` also exists — see the changelog.

## [ ] 4. Manually obtain Paraformer-large int8

Fetch `csukuangfj/sherpa-onnx-paraformer-zh-2024-03-09` via hf-mirror, with `HF_HUB_DISABLE_XET=1`. Place it under the local model directory.

**Verify:** files present, sizes match the listing API, SHA-256 recorded.

## [ ] 5. Audio decoding

`verse-audio::decode` using symphonia. Decode to mono 16 kHz PCM.

**Verify:** a known WAV decodes to the expected sample count and duration. Test an MP3 and an M4A too — container coverage is an open question (`design.md` §10).

## [ ] 6. VAD segmentation

Silero VAD via sherpa-onnx. Split the PCM stream into speech segments. Peak memory must be bounded by segment length, not file length.

**Verify:** a 30-minute file produces a plausible segment count; peak RSS stays flat during decode.

## [ ] 7. AsrEngine trait and Paraformer backend

Implement the trait from `design.md` §4.2 plus the offline Paraformer implementation. Timestamps must survive.

**Verify:** unit test transcribes a short Chinese clip and produces the expected text with monotonic timestamps.

## [ ] 8. Punctuation

Wire the sherpa-onnx punctuation model (repository to be confirmed).

**Verify:** output contains sentence-ending punctuation. Unpunctuated output is not shippable.

## [ ] 9. Export

`verse-core::export` — plain text and SRT.

**Verify:** the produced SRT loads in a video player with correct timing and no encoding errors.

## [ ] 10. CLI end to end

`verse transcribe <file> -o <out.srt>`, plus progress output.

**Verify:** the exit criterion above, run manually on a real Chinese recording. This is the first honest check of overall accuracy.

## [ ] 11. Model downloader

`verse-model`: multi-source failover, resume, SHA-256 verification, 30s timeout before failover, actionable errors, and manual-import fallback. See `design.md` §6.

**Verify:** each source can be forced to fail and the next is used. A corrupted download is detected. Resume works against ModelScope's non-standard `200` + `Content-Range` response.

## [ ] 12. Hardware probe and long-run check

AVX2/FMA detection driving model tier and thread count, with a user-visible message on older CPUs.

**Verify:** thread count is `parallelism - 1`. Transcribe a 2-hour file repeatedly; RSS returns to baseline and does not grow across runs.
