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

## [x] 4. Manually obtain Paraformer-large int8

Fetched `csukuangfj/sherpa-onnx-paraformer-zh-2024-03-09` from hf-mirror into `models/paraformer-zh/` (gitignored): `model.int8.onnx` (227,330,205 bytes, matching the server's Content-Length exactly), `tokens.txt`, and `0.wav` / `2-zh-en.wav` from the repository's own `test_wavs/`.

hf-mirror 302-redirects large files to the Xet CDN (`cas-bridge.xethub.hf.co`), as the research warned. Contrary to that warning, the Xet path was fast and reliable here — 227 MB with no retries. Keep `HF_HUB_DISABLE_XET=1` as a fallback, not a default.

**Verify:** transcribed both test files with `cargo run -p verse-asr --example transcribe_paraformer`. Both produced correct text; see the changelog. This is stronger than the planned "files present, sizes match" check — it proves the model actually runs.

## [x] 5. Audio decoding

`verse-audio` decodes through the **ffmpeg sidecar** rather than symphonia — see `design.md` §4.7 for why. `FfmpegDecoder` implements `AudioSource`, pulling 0.1 s chunks from ffmpeg's stdout, so memory stays bounded by chunk size rather than file length. Dropping the decoder kills the child.

`verse-audio::convert` was added at the same time: a `TranscodeRequest` covering codec, sample rate, channels and bitrate, plus a raw-argument escape hatch so "any format to any format" stays reachable.

**Verify:** four integration tests, all passing. Decoding a 2 s stereo 44.1 kHz tone to the 16 kHz mono target produces the expected frame count within a 0.1 s tolerance — so resampling and downmixing genuinely happen. Chunks come back bounded and in order. A WAV→MP3 conversion produces a non-empty file that decodes back. A missing input fails before ffmpeg is even invoked, with the path in the message. ✅

## [x] 6. VAD segmentation

`verse_audio::SileroVad` implements a new `Segmenter` trait. Spans are capped at 20 seconds, which is the actual ceiling on memory — a span is held resident while being recognized, so without a cap unbroken speech would defeat the point of segmenting.

One thing had to be established rather than assumed: the detector reports span offsets as sample indices relative to everything it has seen, not to the current buffer. That is pinned down by a differential test rather than an absolute one — the same speech is segmented with and without two seconds of leading silence, and the shift has to match. The first attempt asserted an absolute offset and failed, because the sample is a real recording that opens with silence of its own.

**Verify:** two integration tests pass. Offsets accumulate from the stream start; spans carry real audio and never exceed the input length. ✅

Not yet verified: the 30-minute file and flat-RSS check from the original plan. That needs a long recording; it belongs with step 10, where a real end-to-end run happens anyway.

## [x] 7. AsrEngine trait and an offline backend

Implemented from `design.md` §4.2, with Paraformer as the first backend — which
was later removed (see `asr-evaluation.md` §12) and replaced by SenseVoice and
Qwen3-ASR. The trait itself is what this step was for, and it is what made
swapping the backend a registry change rather than a rewrite.

**Verify:** unit test transcribes a short Chinese clip and produces the expected text with monotonic timestamps.

## [x] 8. Punctuation

Done ahead of VAD because the first transcription made the need obvious. `verse_asr::Punctuator` wraps sherpa-onnx's `OfflinePunctuation` with the CT-Transformer model from `design.md` §5.4. No tokens file needed — the vocabulary is in the ONNX graph.

**Verify:** `对我做了介绍啊那么我想说的是呢大家如果对我的研究感兴趣呢嗯` becomes `对我做了介绍啊，那么我想说的是呢，大家如果对我的研究感兴趣呢嗯。` ✅

**Cost noted:** the model is 294 MB, larger than the recognizer. It belongs in the size budget.

## [x] 9. Export

`verse-core::export` renders plain text and SRT. Cues are numbered from one, blank-line separated, with `HH:MM:SS,mmm` timestamps — commas, not dots, or most players reject the file. Empty segments are skipped rather than numbered, since a cue with no text shows as a stray blank flash.

**Verify:** six unit tests cover timestamp formatting (including past one hour), numbering, blank-line separation, skipped empties, empty transcripts, and extension guessing. Not yet opened in a player — that is worth doing once, with a real file, before P1b. ✅

## [x] 10. CLI end to end

`verse transcribe <file>` with `--output`, `--format`, `--engine`, `--model-dir`, `--punct` and `--vad`.

The engine is reused across spans and reset between each, so a long recording is never resident in memory at once — this is where the memory bound from step 6 becomes real rather than theoretical.

**Verify:** run on real Chinese audio, producing a correct SRT. ✅ — and it caught a real defect immediately: the first word came back wrong (`派饭时间` for `开放时间`), which turned out to be VAD span boundaries rather than recognition. Fixed by padding spans with leading context; see step 6.

Still outstanding from the original plan: a long-file run to confirm flat memory. Deferred to a real recording rather than a synthetic one.

## [x] 11. Model downloader

`verse-model` holds the catalogue, the downloader and the progress state machine.

The catalogue moved out of code into `models.json`, parsed at load and validated. Mirrors go dead; editing a file is a better answer to that than shipping a binary. A copy is embedded so a fresh install needs nothing else on disk, and an external file overrides it.

**Downloads are user-initiated only.** There is no fetch on startup and no background refresh — `verse model fetch <id>` is the only entry point, and `is_present` exists so a caller can check before offering.

Failover tries each mirror in turn; a mirror that fails is named in the error. Interrupted transfers resume, and a transfer lands under a temporary name so an interrupted one never looks complete.

**Verify:** a real download from ModelScope completed and landed at the expected size; a second run skipped it. Progress reporting is throttled — it was calling back per 64 KB, which is a wall of output for a 240 MB model. ✅

**Not done, and worth knowing:** SHA-256 verification and the manual-import fallback from the original plan. Length checking is in place and caught a real error, but it cannot detect corruption that preserves length. Manual import needs a UI before it is meaningful.

## [x] 12. Hardware probe

`verse-core::hardware` detects AVX2, FMA and core count, and maps them to a `Tier`. Detection never fails and never blocks startup; a reduced machine runs smaller models and says why.

The module knows nothing about models — it reports capability, and what that permits is decided by whoever picks a model. That is what keeps the fallback from becoming a web of special cases.

Thread count is `cores - 1`, leaving one for the UI.

**Verify:** unit tests cover the tier decisions and the thread reserved for the UI. The CLI probes once and reports reduced mode only when something is actually being worked around. ✅

**Not done:** the 2-hour long-run memory check. Still needs a real recording rather than a synthetic one.
