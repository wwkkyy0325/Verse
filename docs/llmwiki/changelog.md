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

## 2026-10-05 — P1a step 3 complete: sherpa-onnx builds and links

The highest-risk step passed. Every prediction from the research held: no cmake or C++ toolchain needed, the archive URL hardcoded with no override, and the escape hatches functional.

The archive (117 MB, version-locked filename) is fetched through a GitHub prefix proxy and pointed to by `SHERPA_ONNX_ARCHIVE_DIR`, set in `.cargo/config.toml` via cargo's `[env]` section rather than a shell export so IDE builds work too.

An empty crate would not have exercised the linker, so the verification test constructs a `LinearResampler` — no model files required — and checks the output. That is what proves the native library is genuinely linked.

Two findings that change later steps:

- **sherpa-onnx ships its own `vad`, `offline_punctuation` and `resampler` modules.** P1a steps 6 and 8 therefore need no additional libraries, and resampling may not need `rubato` either.
- **`OfflineQwen3ASRModelConfig` exists in 1.13.8**, contradicting the earlier research note that Qwen3-ASR had only a community ONNX port and thus high integration risk. Having a config is not the same as having official converted weights, so no decision changes yet — but §5.6 of `design.md` needs revisiting once that is checked.

## 2026-10-05 — P1a step 4 complete: offline Chinese transcription works

First end-to-end proof that the stack produces Chinese text. Two files from the model repository's own `test_wavs/` were transcribed:

- `0.wav` → 对我做了介绍啊那么我想说的是呢大家如果对我的研究感兴趣呢嗯
- `2-zh-en.wav` → yesterday was 星期一 today is tuesday 明天是星期三

Three things this settled:

- **Chinese works, and so does code-switching.** The mixed file came back cleanly, which matters because English is the secondary language here and mixed speech is the realistic input.
- **Punctuation is empirically required.** The output carries none. §5.4 already called the punctuation model non-optional; this is confirmation rather than prediction.
- **The Xet CDN warning was too pessimistic.** hf-mirror does redirect large files to `cas-bridge.xethub.hf.co`, but the 227 MB download finished without a single retry.

Added `crates/verse-asr/examples/transcribe_paraformer.rs` as a manual smoke check, kept out of the test suite because it depends on model files that are never committed.

## 2026-10-05 — P1a steps 5 and 8 complete: decoding, conversion, punctuation

**Audio I/O switched to an ffmpeg sidecar**, replacing the symphonia plan. The requirement for "any format in, any format out" ruled symphonia out: it reads common audio formats but cannot encode at all, so the conversion half is unreachable with it. ffmpeg covers both directions and every container including video.

Integration is a **child process, not a linked library**. Linking FFmpeg's C libraries would contradict the no-leak constraint outright, and process isolation is stronger than even a careful binding — ffmpeg's leaks and crashes cannot reach this process. `verse-audio::decode` pulls 0.1 s chunks from ffmpeg's stdout as an `AudioSource`, so memory stays bounded by chunk size rather than file length. `verse-audio::convert` handles arbitrary transcoding, with a raw-argument escape hatch for options not modelled.

New obligation: Verse now depends on an ffmpeg executable. Discovery is `VERSE_FFMPEG` → `PATH` → bundled (P1b). Only the bundled copy satisfies the zero-configuration goal, and it does not exist yet — this is the largest outstanding gap against "install and run", larger than any model download, because it is a system-level install rather than a file fetch.

**Punctuation done, ahead of VAD**, because the first transcription made the need obvious. `verse_asr::Punctuator` wraps sherpa-onnx's CT-Transformer model; no tokens file is needed because the vocabulary is embedded in the ONNX graph.

**The punctuation model is 294 MB — larger than the recognizer's 227 MB.** Combined with the streaming model Phase 2 will need, the size budget is becoming a genuine problem for the lightweight goal. Nothing is decided yet, but it should not stay unexamined.

## 2026-10-05 — Size problem addressed: SenseVoice default, pipeline decoupled

The 521 MB default (Paraformer + punctuation) is replaced by **SenseVoice-Small at 228 MB**, which punctuates and normalizes internally. That is a 56% reduction, and it adds Cantonese, Japanese and Korean for free. Paraformer and its punctuation stage stay registered — switching is a configuration change.

**The license is resolved, and the earlier reading was wrong.** SenseVoice is not Apache-2.0: the official HuggingFace card has always pointed at the custom FunASR Model License v1.1, and the Apache label comes from a ModelScope metadata field that other hosts copied. The custom license *does* permit commercial use, subject to attribution, retaining the model name, and shipping the license text. `THIRD_PARTY_NOTICES.md` now records every obligation, including that the license can be revised unilaterally — hence pinning model revisions and archiving the terms.

**Accuracy measured rather than assumed.** Across three clips SenseVoice matched Paraformer on one, made a character error on another, and produced a repeated-word error (`Tuesdayesday`) on Chinese/English mixed audio. Both errors fell on English content. Three clips is not a verdict, but it is why Paraformer remains one configuration change away instead of deleted. Recorded in `design.md` §5.3.

**Decoupling.** Two new abstractions, both in `verse-core`:

- `TextProcessor` / `TextChain` — punctuation, translation and ITN are all text-in/text-out, so they share one trait instead of each being a special case. The contract forbids swallowing data: a failing stage returns its input unchanged.
- `Segmenter` — VAD is the first implementation; it also bounds memory, since spans are recognized independently and capped at 20 seconds.

`verse-asr` gained `OfflineEngine`, implementing `AsrEngine` for both models, and `register_builtin_engines` which wires them into the registry. Adding an engine is adding a descriptor.

Also added: Silero VAD (2.3 MB), and the `Segmenter` verification uncovered that span offsets are cumulative from the stream start — established by a differential test after an absolute assertion failed.

## 2026-10-05 — End to end: export, CLI, and a VAD defect found by using it

`verse transcribe <file>` now runs the whole chain and writes SRT. Getting there surfaced a real bug that no unit test would have caught.

**The first word came back wrong.** `开放时间` transcribed as `派饭时间`. The recognition was fine in isolation; the difference appeared only once VAD was in the path.

Diagnosis, in order:

1. Suspected VAD clipped real speech. It had not — the 0.766 s it skipped transcribes to nothing on its own.
2. Suspected the offsets were wrong. They were not — the file is 16-bit PCM, ~5.6 s, and `0.766 → 5.318` is inside that. The first reading had assumed float samples and looked impossible.
3. Fed the same audio from different offsets. That was the answer:

```text
from 0.25 s  ->  开放时间   (correct)
from 0.50 s  ->  开放时间   (correct)
from 0.75 s  ->  菜饭时间   (wrong, and where the detector chose)
whole file   ->  开饭时间   (also wrong)
```

**Leading context is worth more than completeness.** The whole file scored worse than a padded slice, so "give the model everything" is not the fix — giving it a sensible window is.

Spans are now padded with 0.4 s of leading context, drawn from a bounded history buffer. The bound holds because a span cannot outrun the maximum span length, so this stays a ceiling rather than accumulation. Result on the same clip: `开放时间`, correct.

**Also fixed:** a defect in the VAD tests themselves. Three of them shared one temp filename and deleted each other's input when run in parallel. It only surfaced once padding changed the timing.

Export and the CLI are covered by tests; the CLI is what found the above.

## 2026-10-05 — Downloader and hardware detection: P1a complete

The last two steps of P1a are in. Both were shaped by the same principle: separate the decision from the mechanism.

**The catalogue moved into `models.json`.** Mirrors go dead — a host moves, a path changes, a region gets blocked — and editing a file beats shipping a new binary. A copy is embedded so a fresh install needs nothing else on disk; an external file overrides it. A file that exists but does not parse is an error rather than a reason to fall back, because silently ignoring a catalogue someone deliberately placed would hide their mistake until download time.

**Downloads are user-initiated.** No fetch on startup, no background refresh. `verse model fetch <id>` is the only way in, and `is_present` exists so a caller can check before offering.

**Size checking paid for itself within the hour.** `verse model list` reported SenseVoice as missing when it was not. The catalogue had recorded 114,688 bytes for `tokens.txt` against an actual 315,894 — a client-reported transfer size that had been written down in place of the host's `Content-Length`. The check was correct; the figure was not. `catalog.rs` now carries a note about where sizes must come from.

Two smaller things the same work exposed: progress reporting was calling back once per 64 KB, which is a wall of output for a 240 MB model, now throttled to one percent; and model directories had to be renamed to match catalogue ids, since the ids are also the engine names.

**Hardware detection reports capability and stops there.** It does not know what a model needs — that belongs to whoever chooses one. A reduced machine runs smaller models and says why; detection never fails and never blocks startup.

**Still outstanding for P1a's exit criteria:** a long-file run confirming flat memory, which needs a real recording. And two items from the downloader plan were not built: SHA-256 verification (length checking catches truncation, not corruption that preserves length) and the manual-import fallback (which needs a UI to be meaningful).
