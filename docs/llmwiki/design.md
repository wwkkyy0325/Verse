# Verse — Design

Offline-first Chinese speech-to-text. Status: design approved, P1a not yet started.

## 1. What this is

A desktop tool that turns audio into text, locally. No account, no network at runtime, no GPU.

Two delivery modes:

- **File transcription** (Phase 1) — drop in an audio/video file, get a transcript and subtitles.
- **Live subtitles** (Phase 2) — capture system audio playback, show subtitles in a floating window; optionally translate.

## 2. Goals and non-goals

### Goals

- Usable by someone with no technical background: install, run, drop a file, get text.
- Chinese primary, English secondary.
- Runs on modest hardware: 2013+ x86-64 (AVX2), Apple Silicon, ARMv8. No GPU required.
- Small binary. Model weights are never compiled or embedded into the binary — they live as external files, whether fetched on first launch or shipped in the installer (see §10, open question 1).
- No memory growth over long sessions.

### Non-goals

- **Real-time dictation was explicitly removed from scope.** This drops global hotkeys, text injection (SendInput/CGEvent/XTest), and the platform-specific input-method surface. Do not reintroduce without a new design pass.
- No mobile, no embedded/MCU.
- No GPU acceleration in v1.
- No cloud ASR in v1. The earlier "local-first, cloud-fallback" idea is deferred; the `AsrEngine` trait keeps room for it.

## 3. Hard constraints

| ID | Constraint | Consequence |
|----|-----------|-------------|
| C1 | Rust | — |
| C2 | No memory-leak risk in a long-running process | Rules out `whisper-rs` (see §5.1) |
| C3 | Chinese primary | Rules out Whisper-family and Moonshine (see §5.1) |
| C4 | CPU-only, 2013+ baseline | Model must run at usable speed without GPU |
| C5 | Must be obtainable from mainland China | Multi-source download, §6 |
| C6 | Zero configuration for end users | Auto model fetch, hardware auto-adaptation |

## 4. Architecture

### 4.1 Crate layout

```
verse-core     Domain model, traits, events, registry, router, export   [lib]
verse-audio    decode + convert (ffmpeg sidecar) / capture (P2)         [lib]
verse-asr      Engine implementations — sherpa-onnx FFI lives here      [lib]
verse-model    Model catalog + multi-source resumable downloader        [lib]
verse-cli      Command-line entry point                                 [bin]
verse-app      slint GUI                                                [bin]
```

Three boundaries justify the splits:

- **All traits live in `verse-core`; only implementations live elsewhere.** `AsrEngine` is defined in `verse-core` (pure abstraction, no FFI) and implemented in `verse-asr`. This is what lets the registry in §4.6 hold engine factories without `verse-core` ever linking sherpa-onnx. `verse-asr` isolates the FFI and its build-time cost, so `verse-core` unit tests stay fast.
- `verse-model` isolates all network access. This is what makes the offline guarantee structural (§4.5).
- `verse-audio` holds two modules with different platform dependencies, sharing one resampler: `decode` (pure Rust, P1) and `capture` (cpal + platform loopback, P2). Feature flags keep `cpal` out of P1 builds.

### 4.2 Core traits

```rust
pub trait AudioSource: Send {
    fn format(&self) -> AudioFormat;
    /// Pull the next PCM chunk. `Ok(None)` signals end of stream.
    fn next_chunk(&mut self) -> Result<Option<AudioChunk>, AudioError>;
}

pub trait AsrEngine: Send {
    fn is_streaming(&self) -> bool;
    fn accept(&mut self, chunk: &AudioChunk) -> Result<(), AsrError>;
    /// Streaming: yields confirmed incremental text. Offline: returns `None`
    /// until `finalize`.
    fn poll(&mut self) -> Result<Option<TranscriptDelta>, AsrError>;
    fn finalize(&mut self) -> Result<Transcript, AsrError>;
    fn reset(&mut self);
}

pub trait TextSink: Send {
    fn emit(&mut self, segment: &Segment) -> Result<(), SinkError>;
}
```

`Transcript` carries `Vec<Segment { start, end, text }>` from the start. Timestamps are required for SRT output; retrofitting them later would touch every layer.

### 4.3 Data flow

Phase 1:

```
file ──▶[decode]──▶[VAD segment]──▶[parallel ASR]──▶[punctuate]──▶[segments]──▶ CLI/GUI, txt/srt
      ffmpeg         Silero           N threads     CT-Transformer    +timestamps   verse-core::export
      sidecar
```

Phase 2:

```
system audio ──▶[loopback capture]──▶[ring buffer]──▶[streaming ASR]──▶[subtitle window]
                cpal + platform API        VAD          sherpa-onnx      ──▶[MT]──▶ bilingual
```

### 4.4 Threading

Audio input must never block; recognition is CPU-heavy. They are always separate threads.

```
[capture thread] --ring buffer--> [ASR thread] --channel--> [UI thread]
 realtime-ish                      CPU-bound                 never blocks
 memcpy only                       N = parallelism - 1
```

The ASR thread count is `available_parallelism() - 1`. Reserving one core for the UI is the difference between a responsive window and a frozen one on older machines.

Long files must not be loaded whole. A 2-hour recording decodes to roughly 1.4 GB of PCM. The pipeline streams decode into VAD-delimited segments, so peak memory is bounded by segment length, not file length.

### 4.5 Runtime boundary: offline

`verse-core` must contain **no HTTP client**. All network access lives in `verse-model` and runs only on explicit user action. "Works offline" is then an architectural property, not a discipline.

### 4.6 Communication backbone

Three layers, all in `verse-core`, containing no I/O and no FFI. This is the foundation later phases build on.

```
Registry    (what exists)      pluggable implementations, looked up by ID
    ↓
Router      (where work goes)  job kind → pipeline
    ↓
EventBus    (who hears what)   publish/subscribe — the only inter-module channel
```

**Registry** — engines, audio sources, sinks, and translators each register under a string ID along with a factory. Adding a Phase 2 engine means registering a factory; orchestration code does not change.

**Router** — a `Job` carries a `JobKind` (`FileTranscribe`, `LiveSubtitle`, `Translate`). The router maps kind to a `Pipeline`. A pipeline runs with a `RuntimeContext` carrying the registry, the bus, and a cancellation token.

**EventBus** — modules never call each other directly. They publish events and subscribe with a filter. This decouples the UI from pipelines entirely and makes progress reporting uniform across every job kind, which is what lets Phase 2's subtitle window reuse Phase 1's plumbing.

Event vocabulary (initial):

```rust
enum Event {
    // job lifecycle
    JobStarted   { id: JobId, kind: JobKind },
    JobProgress  { id: JobId, position: Duration, fraction: f32 },
    JobFinished  { id: JobId },
    JobFailed    { id: JobId, error: ErrorInfo },
    JobCancelled { id: JobId },
    // pipeline output
    TranscriptDelta   { job: JobId, delta: TranscriptDelta },
    TranscriptSegment { job: JobId, segment: Segment },
    TranscriptFinal   { job: JobId, transcript: Transcript },
    // environment
    HardwareProbed    { profile: HardwareProfile },
    ModelStateChanged { model: ModelId, state: ModelState },
}
```

Progress and cancellation flow through the bus rather than return values, so a long transcription is observable and interruptible without the pipeline knowing who is listening.

### 4.7 Audio I/O: the ffmpeg sidecar

Every format decision collapses into one: use ffmpeg, and never link it.

**Why ffmpeg at all.** "Any format in, any format out" is not reachable with a pure-Rust decoder. `symphonia` reads common audio formats but cannot encode, so the conversion half of the requirement is impossible with it alone. ffmpeg covers both halves and every container, video included.

**Why a child process, not a library.** The no-leak constraint (§3, C2) is the second hard requirement in this project. Linking FFmpeg's C libraries is the largest FFI surface available and would contradict it outright. Running `ffmpeg` as a child and reading raw PCM from its stdout gives full coverage with *stronger* isolation than even a careful binding: a leak, crash, or memory blow-up inside ffmpeg cannot reach this process. Dropping the decoder kills the child.

**The cost.** Verse now depends on an ffmpeg executable. Discovery order is `VERSE_FFMPEG` → `PATH` → a copy shipped beside the binary. Only the third satisfies "install and run", and it is a P1b task; P1a uses whatever the machine already has.

That obligation is real rather than a footnote: requiring a separate ffmpeg install would break the zero-configuration goal (§3, C6) harder than any model download, because it is a system-level install rather than a file fetch.

**Resampling happens in ffmpeg.** Decoding asks for 16 kHz mono directly (`-ar 16000 -ac 1`) so ffmpeg's proper resampler does the work. sherpa-onnx also ships a `LinearResampler`, but linear interpolation is a poor fit for 44.1 kHz → 16 kHz; letting ffmpeg handle it is both simpler and better.

## 5. Engine and model selection

### 5.1 Why not Whisper

Whisper is the default assumption and it is the wrong answer here. Measured Chinese error rates:

| Model | Chinese CER (real-world clips) |
|---|---|
| Whisper large-v3-turbo | 16–23 |
| Whisper small | ~17 |
| Domestic first tier | 3–8 |

Whisper is 3–5x worse on Chinese than domestic models, and large-v3-turbo runs at roughly 0.5x realtime on an i7 with 4 threads. Moonshine scores 25–36 CER on Chinese and `distil-whisper` has no Chinese support at all.

Separately, `whisper-rs` has **open, accumulating memory leaks** — `#280` leaks a `CString` on every language/initial-prompt set, `#271` leaks `FullParams` callback memory. One downstream app measured 5.8 GB of swap accumulated after 300 transcriptions. Its main repository was archived to Codeberg in 2025 and is lightly maintained. This violates C2 directly.

This also rules out the pure-Rust `candle` route: candle's ASR path is Whisper, so "zero FFI" would be bought with a 3–5x Chinese accuracy regression.

### 5.2 Engine: official sherpa-onnx Rust API

Use the Rust API **inside the `k2-fsa/sherpa-onnx` main repository**.

> The widely-referenced `thewh1teagle/sherpa-rs` crate was **archived and deprecated on 2026-06-06**. Most tutorials still point at it. Do not use it.

Why this one:

- Safe RAII wrappers; documentation states runtime wrappers free underlying C resources on drop. No leak issues found.
- It is the only mature path to the models that actually work for Chinese: SenseVoice, Paraformer, FireRedASR, Dolphin are all delivered as sherpa-onnx ONNX.
- Built-in VAD and streaming support.

Cost: it remains FFI, and it pulls a native dependency (§7.2). Accepted, because the alternative costs Chinese accuracy.

### 5.3 Model tiers (Phase 1, file transcription)

| Tier | Model | Size | Distribution | License |
|------|-------|------|--------------|---------|
| **Bundled default** | Paraformer-large int8 | ~220 MB | **Ships in the installer** | **Apache-2.0, clean** |
| Optional | SenseVoice-Small int8 | ~230 MB | Download on demand | ⚠️ **ambiguous — see below** |
| Optional | FireRedASR2-AED int8 | ~1.1 GB | Download on demand | Apache-2.0 |

**Distribution model:** the installer bundles one lightweight model so the app transcribes correctly on first launch with no network at all. Heavier models are opt-in downloads from the settings page. This costs installer size (~250 MB) and buys a genuinely zero-setup first run.

The bundled model must have an unambiguous license, which is why it is Paraformer-large and not SenseVoice. SenseVoice is additionally excluded from bundling because *redistributing* it poses the unresolved license question more sharply than downloading it does.

The exact bundled model is selected in P1a step 4 — prefer a smaller offline Chinese model if one benchmarks acceptably below ~220 MB.

**SenseVoice license conflict (must resolve before it can become a default):** the HuggingFace model card says `apache-2.0`, while the upstream FunASR repository ships a custom `MODEL_LICENSE`. These contradict each other. Treat as unresolved for commercial use.

Cross-benchmark CER numbers are not strictly comparable — different test sets (AISHELL, WenetSpeech, FLEURS, real-clip micro-CER). Do not rank models from the table above on CER alone.

### 5.4 Punctuation (resolved)

Paraformer outputs unpunctuated text. The first real transcription confirmed how unusable that is: `对我做了介绍啊那么我想说的是呢大家如果对我的研究感兴趣呢嗯`.

The model is `csukuangfj/sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12` (CT-Transformer), loaded through sherpa-onnx's `OfflinePunctuation`. It needs no tokens file — the vocabulary is embedded in the ONNX graph.

**It costs 294 MB — more than the recognition model itself.** That is real weight against the lightweight goal and belongs in the size budget. SenseVoice punctuates internally and would have avoided it, but its unresolved license keeps it out of the default path.

### 5.5 Phase 2: streaming and translation

**Streaming ASR:** `Paraformer-zh-streaming` int8 (~220 MB, Apache-2.0). It is the only mature *native* streaming Chinese model. Not the fastest option available, but the lowest-risk one. Use sherpa-onnx's 2-pass pattern (stream for low latency, offline re-decode to correct) so displayed subtitles improve after the fact.

**Translation:** OPUS-MT (Marian), one model per direction (`zh-en`, `en-zh`), ~80–150 MB each quantized. Run via `candle`, whose `marian` implementation is pure Rust with `--cpu` and `--quantized` support — no additional C++ dependency. At ~700 tok/s int8, a subtitle line translates in tens of milliseconds, which fits the realtime budget.

Rejected alternatives:

- **NLLB-200 distilled 600M** — CC-BY-NC-4.0, **non-commercial only**. A licensing landmine. Also 0.6–1.4 s per sentence, too slow for live subtitles.
- **Small LLMs** (Qwen2.5-0.5B GGUF etc.) — 0.5–3 s per sentence on CPU. Too slow.
- **Whisper's built-in translate task** — translates *into English only*. It cannot produce Chinese, and Chinese is the primary target, so it cannot replace an MT model.

### 5.6 Deferred: LLM-architecture models

Qwen3-ASR-0.6B (Jan 2026, Whisper encoder + Qwen3 decoder, Apache-2.0, ~7x CPU realtime) matches the "lightweight LLM-architecture ASR" brief. However its ONNX form is a **community port**, not an official release, so integration risk is materially higher.

Do not put it in the main path. Revisit after P1a with a real accuracy comparison against Paraformer-large.

## 6. Model acquisition (China-first)

No model in this project is natively hosted in mainland China. Multi-source failover is a requirement, not an optimization.

### 6.1 Source priority

```
1. ModelScope direct          (fastest where a ready build exists)
      ↓ timeout 30s / failure
2. hf-mirror.com              (primary channel for csukuangfj/sherpa-onnx-*)
      ↓
3. GitHub prefix proxy        (asr-models tarballs)
      ↓
4. Manual import              (user points at a folder — never dead-ends)
```

Layer 4 is mandatory from day one. It is what guarantees the app cannot hard-fail on network grounds.

### 6.2 Verified availability

| Model | China-accessible source |
|---|---|
| SenseVoice-Small int8 | ModelScope `xiaowangge/sherpa-onnx-sense-voice-small` — **third-party upload, verify checksums** |
| Paraformer-large int8 (227 MB) | hf-mirror only: `csukuangfj/sherpa-onnx-paraformer-zh-2024-03-09` |
| Streaming Paraformer-zh | hf-mirror only |
| FireRedASR int8 | hf-mirror only: `csukuangfj/sherpa-onnx-fire-red-asr-large-zh_en-2025-02-16` |

**Correction to an earlier assumption:** `csukuangfj` does *not* publish the classic model families on ModelScope. API probes return 404 for paraformer-zh, streaming-paraformer-zh, sense-voice, and fire-red-asr there. ModelScope web pages return 200 only via SPA fallback — the JSON API is authoritative. Only a partial set of newer models is published, and that channel has been toggled off before.

### 6.3 Verified URL patterns

ModelScope (public repos need no token):

```
resolve:  https://www.modelscope.cn/models/{owner}/{repo}/resolve/{rev}/{path}   # rev defaults to master
native:   https://www.modelscope.cn/api/v1/models/{owner}/{repo}/repo?Revision={rev}&FilePath={path}
listing:  https://www.modelscope.cn/api/v1/models/{owner}/{repo}/repo/files?Revision=master&Recursive=true
          → returns Size and Sha256 per file; use for integrity and resume decisions
```

hf-mirror:

```
https://hf-mirror.com/{repo}/resolve/main/{path}
```

### 6.4 Gotchas found during verification

- **Set `HF_HUB_DISABLE_XET=1`.** Large files on hf-mirror are 307-redirected to `cas-bridge.xethub.hf.co`, which is slow or unstable from mainland China.
- **ModelScope may answer a range request with `200` + `Content-Range` instead of `206`.** Resume logic must not assume 206. Validate by length and SHA-256 instead.
- **SHA-256 verification is mandatory**, not optional — especially for the third-party ModelScope SenseVoice upload.
- **hf-mirror is a community project** and GitHub prefix proxies (`ghfast.top`, `gh-proxy.com`, `ghproxy.net`) are third-party services that can disappear. Configure more than one.

### 6.5 Downloader requirements

Resume via range requests, parallel chunks, SHA-256 verification, system proxy respected, 30s timeout before failing over, and actionable errors ("network unreachable — or download manually to `<path>`"), never a bare "download failed".

## 7. Development environment (China)

This section is about *building*, not running. End users receive prebuilt binaries.

### 7.1 crates.io mirror

`%USERPROFILE%\.cargo\config.toml`:

```toml
[source.crates-io]
replace-with = 'rsproxy-sparse'

[source.rsproxy-sparse]
registry = "sparse+https://rsproxy.cn/index/"

[registries.rsproxy-sparse]
index = "sparse+https://rsproxy.cn/index/"

[net]
git-fetch-with-cli = true
```

rsproxy.cn (ByteDance) verified reachable. TUNA sparse index also verified. Sparse protocol requires Rust ≥1.68.

### 7.2 sherpa-onnx-sys native archive

**No cmake and no C++ toolchain are required.** The build script downloads a prebuilt static archive that already bundles onnxruntime; only a linker (MSVC on Windows) is needed. This was the main feared blocker and it does not exist.

The remaining problem: **the download URL is hardcoded to GitHub Releases and no environment variable can rewrite it.** Mainland speeds of ~10 KB/s are reported (upstream issue #1988).

Three verified escape hatches:

```bash
# Pre-place the exact-named archive; build.rs uses it instead of downloading
SHERPA_ONNX_ARCHIVE_DIR=/path/to/archive
# Or point directly at already-extracted libs
SHERPA_ONNX_LIB_DIR=/path/to/libs
# Or let the proxy be honored (build.rs supports this as of PR #3507)
HTTPS_PROXY=http://127.0.0.1:7890
```

Both directory variables are registered as `rerun-if-env-changed`. `ALL_PROXY` is not supported.

Archive filenames are version-locked to the crate version (e.g. `sherpa-onnx-v{version}-win-x64-static-MT-Release-lib.tar.bz2`). A version bump changes the filename that must be pre-staged.

**As built:** `SHERPA_ONNX_ARCHIVE_DIR` is set from the repository's `.cargo/config.toml` through cargo's `[env]` section rather than a shell export, so command-line and IDE builds both resolve it. The archive sits in `.sherpa-onnx-libs/`, which is gitignored — `cargo clean` does not discard it, and it is never committed. The archive for Windows x64 at version 1.13.8 is 117 MB.

### 7.3 Truly offline builds

`cargo vendor` captures crate sources only. It does **not** capture the build script's native archive download. A fully offline build requires **both**: vendored crates *and* a pre-staged archive via `SHERPA_ONNX_ARCHIVE_DIR`.

### 7.4 Stale Windows proxy breaks cargo (observed on the dev machine)

Windows keeps a `ProxyServer` value under
`HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings` even after
the proxy is switched off (`ProxyEnable = 0`). libcurl reads it anyway, so cargo
sends every request to a dead local port:

```
Failed to connect to mirrors.tuna.tsinghua.edu.cn port 443 via 127.0.0.1
```

Tells that identify this specific cause:

- `curl` reaches the same URL fine, cargo does not.
- No `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` is set anywhere.
- **Setting `HTTPS_PROXY` to a different value does not change the error** — the
  registry value wins over the environment. Only `NO_PROXY` takes effect.

Workaround:

```bash
NO_PROXY='*' cargo fetch
```

Permanent fix: clear the stale `ProxyServer` value from the registry.

This is worth fixing before P1a rather than working around, because every
dependency added from here on goes through the same path.

## 8. Hardware adaptation

Probe at startup, adapt rather than refuse:

```rust
is_x86_feature_detected!("avx2");
is_x86_feature_detected!("fma");
std::thread::available_parallelism();
```

Absent AVX2 means: fall back to the SSE kernel and restrict the model tier to small models. The user sees "your CPU is older than ideal, switched to fast mode" — not "this program cannot start". Degrading with an explanation is what makes C6 real.

## 9. Phasing

| Phase | Deliverable | Verification |
|-------|-------------|--------------|
| **P0** | Framework: workspace, domain types, event bus, registry, router, pipeline trait | Unit tests: events reach subscribers by filter; registry resolves a registered factory; a stub pipeline runs a job to completion while emitting progress and honoring cancellation |
| **P1a** | core + CLI + model fetcher | `verse model fetch` retrieves a model via failover; `verse transcribe a.mp3 -o a.srt` produces correct subtitles |
| P1b | slint GUI shell | Drag file → progress → result → export, no terminal involved |
| P2a | Loopback capture + streaming subtitles | Play a video on Windows; floating window shows text live |
| P2b | Translation | Bilingual zh/en subtitles |

P1a is where the risk lives: the model fetcher under real Chinese network conditions, and the sherpa-onnx build with a pre-staged archive.

## 10. Risks and open questions

| Risk | Impact | Mitigation |
|------|--------|-----------|
| SenseVoice license contradiction | Cannot ship as default | Default to Paraformer-large; resolve before promoting SenseVoice |
| Third-party ModelScope upload | Supply-chain integrity | SHA-256 pinning; prefer hf-mirror for that model if hashes cannot be trusted |
| Punctuation model not yet selected | Unreadable SRT | P1a task; blocking for the phase exit criterion |
| GitHub prefix proxies are third-party | Build/fetch failures | Configure two; document manual pre-staging |
| sherpa-onnx archive filename is version-locked | Breaks on every crate bump | Pin the crate version; record the archive name |
| No single stable China model host | Download failures | 4-layer fallback including mandatory manual import |
| `sherpa-onnx` remains FFI | Residual leak surface | Accepted; monitor upstream. `verse-asr` isolates it so a swap stays local |
| OS loopback capture differs per platform (P2) | macOS needs a virtual audio device | Out of P1 scope; revisit at P2a |

### Open questions

1. ~~Model bundled or fetched on first launch?~~ **Decided:** one lightweight model ships inside the installer; heavier models are opt-in downloads. See §5.3.
2. ~~Exact punctuation model repository?~~ **Decided:** `csukuangfj/sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12`. See §5.4.
3. ~~Windows audio/video container support in `symphonia`?~~ **Moot:** the decoder is the ffmpeg sidecar, which covers every container ffmpeg does — including video. See §4.7.
