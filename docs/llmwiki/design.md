# Verse — Design

Offline-first Chinese speech-to-text. Status: P1a complete, P1b (GUI) in progress.

## 1. What this is

A desktop tool that turns audio into text, locally. No account, no outbound network at runtime, no GPU.

Two delivery modes:

- **File transcription** (Phase 1) — drop in an audio/video file, get a transcript and subtitles.
- **Live subtitles** (Phase 2) — capture system audio playback, show subtitles in a floating window; optionally translate.

## 2. Goals and non-goals

### Goals

- Usable by someone with no technical background: install, run, drop a file, get text.
- Chinese primary, English secondary.
- Runs on modest hardware: 2013+ x86-64 (AVX2). No GPU required. Windows only — see C7.
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
| C6 | Zero configuration for end users | Auto model fetch, hardware auto-adaptation. **Amended 2026-10-07:** the window now *shows* the engine and lets it be changed. What C6 protects is that the default path requires no decision, and it still does not — the engine is chosen before anybody looks and is shown as chosen. What changed is that a person who wants to choose can now see what the choice is, which `ui-design.md` §9's licence obligation required anyway. |
| C7 | **Windows is the only platform built or supported** | Added 2026-10-07, with the first release workflow. macOS was dropped because no LGPL ffmpeg build exists for it and the alternatives are all GPL or a signed source build. Linux was dropped as a scope decision — **it built**, and the one CI failure was a test that spells a path with backslashes rather than anything in the program. The source stays portable where it costs nothing, but nothing checks that any more, and no `#[cfg(unix)]` arm is compiled by CI. Reasoning and both removals: `.github/workflows/release.yml`, `tasks/ci-release.md`. |

## 4. Architecture

### 4.1 Crate layout

```
verse-core     Domain model, traits, events, registry, router, text chain  [lib]
verse-audio    decode + convert (ffmpeg sidecar), VAD segmentation         [lib]
verse-asr      Engine implementations — sherpa-onnx FFI lives here         [lib]
verse-model    Model catalog + multi-source resumable downloader           [lib]
verse-store    Per-user directories, result cache, resume, output naming   [lib]
verse-cli      Command-line entry point                                    [bin]
verse-app      Tauri 2 shell — Rust backend + web frontend in `ui/`        [bin]
verse-bench    Evaluation harness; deliberately not shipped                 [bin]
```

Four boundaries justify the splits:

- **All traits live in `verse-core`; only implementations live elsewhere.** `AudioSource`, `Segmenter`, `AsrEngine` and `TextSink` are defined there as pure abstractions. This is what lets the registry (§4.6) hold implementations without `verse-core` ever linking sherpa-onnx, and it is what makes swapping an engine a configuration change rather than a code change.
- `verse-model` isolates all **outbound** network access; `verse-cli`'s `serve` command adds one loopback listener and initiates nothing — that pair is what makes the offline guarantee structural (§4.5).
- `verse-audio` and `verse-asr` are the two crates that touch native code — ffmpeg through a child process, sherpa-onnx through FFI. `verse-core` unit tests therefore link neither, and stay fast.
- **`verse-store` owns "where data lives on this machine"** — the per-user directories, the transcription result cache, the resume checkpoints, and the naming rules that decide which file a transcript is written to. It is a leaf: it depends on no workspace crate, so the command line can resolve an output path without linking the engine. The cache's wire types live here for the same reason the CLI's live in `verse-cli/src/report.rs` — the format belongs to the crate that owns it, and `verse-core` keeps its empty dependency list.

**A decoded-PCM cache was considered and rejected.** It would let a re-run skip
ffmpeg, but the result cache (§4.9) already answers the common case without the
decode happening at all; the PCM cache only earns its keep when the *same* audio
is re-run through a *different* engine, which is evaluation work and is what
`verse-bench` is for. It would cost roughly 1.4 GB per two hours of audio, with
an eviction problem of its own. Recorded here so it is not re-proposed as a new
idea.

`verse-app` is a **Tauri 2** application. The interface is a web frontend —
Svelte 5 and TypeScript, built by Vite — rendered in the system webview, and
this crate is the backend it calls. Two reasons it won over a native toolkit:
the licence is MIT/Apache-2.0, identical to this project's, so the shell adds
no obligation of its own; and the development loop has real hot reload, which
a compiled GUI toolkit cannot offer.

**The cost is WebView2 on Windows.** Tauri ships no browser engine, so the
supported operating system floors at Windows 10 1803 rather than at the
hardware floor the pipeline is built for. On a 2013 CPU with a 2013 operating
system, the recognizer would run and the window would not. That is recorded in
`THIRD_PARTY_NOTICES.md` as a decision to make deliberately rather than
discover at launch.

The interface, its tokens and the contract between it and the pipeline are in
`ui-design.md`. Note what survived the move: the state model in
`crates/verse-app/src/state.rs` is plain Rust over plain values with no
framework types in it, so it crossed from a native toolkit to a web frontend
unchanged. That was the point of writing it that way.

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
file ──▶[decode]──▶[VAD segment]──▶[ASR]──▶[segments]──▶ CLI/GUI, txt/srt
      ffmpeg         Silero         per span   +timestamps   verse-core::export
      sidecar
```

The segmenter is the one stage that can lose audio, so it is not trusted
blindly — see §4.8. Both registered engines punctuate internally, so there is
no punctuation stage (§5.4).

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

`verse-core` must contain **no HTTP client**, and no crate outside `verse-model` may **originate** a network connection. Everything Verse sends out is one model download, and it runs only because someone asked for it. "Works offline" is then an architectural property, not a discipline.

**One listening socket, and it is bounded.** `verse serve` (§4.11) opens a TCP
listener on `127.0.0.1` and only that — never `0.0.0.0`, never a routable
interface — so that other programs on the same machine can drive the pipeline.
The distinction that keeps the guarantee intact:

> **An outbound connection is a request to a server; an inbound one is a request
> from a local peer.**

The first is what "offline" forbids, and only `verse-model` may make it. The
second is a local program using this one, which is the point of that command. A
listener accepts connections and initiates none: it adds no HTTP client, reads
and writes no remote host, and no transcript, audio or model byte crosses it to
anywhere but a loopback peer. It is bound only while a user is running `serve` —
there is no autostart, no daemon and no service registration, and with the
command not running no socket is bound at all. The check is a `grep` for a
listening call, not a promise.

The caches of §4.9 hold this too. They read and write local files and nothing
else: no remote validation, no checking whether a cached result is still
current, no fetch on a miss. A cache miss means doing the work, not asking
anyone about it. `verse-store` depends on no workspace crate and has no HTTP
client, so this is checkable by reading its manifest rather than by trusting a
convention.

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
    ModelStateChanged { model: ModelId, state: ModelState },
}
```

Progress and cancellation flow through the bus rather than return values, so a long transcription is observable and interruptible without the pipeline knowing who is listening.

### 4.7 Audio I/O: the ffmpeg sidecar

Every format decision collapses into one: use ffmpeg, and never link it.

**Why ffmpeg at all.** "Any format in, any format out" is not reachable with a pure-Rust decoder. `symphonia` reads common audio formats but cannot encode, so the conversion half of the requirement is impossible with it alone. ffmpeg covers both halves and every container, video included.

**Why a child process, not a library.** The no-leak constraint (§3, C2) is the second hard requirement in this project. Linking FFmpeg's C libraries is the largest FFI surface available and would contradict it outright. Running `ffmpeg` as a child and reading raw PCM from its stdout gives full coverage with *stronger* isolation than even a careful binding: a leak, crash, or memory blow-up inside ffmpeg cannot reach this process. Dropping the decoder kills the child.

**The cost.** Verse now depends on an ffmpeg executable. Discovery order is `VERSE_FFMPEG` → a copy shipped beside the binary → `PATH`. The second is what satisfies "install and run", and it wins over `PATH`: the copy that ships is the one the release was built and tested against, and a user who wants their own says so with `VERSE_FFMPEG`, which is still checked first. (This line read `PATH` before the shipped copy until 2026-10-08, which was the order in the comment the code shipped with — and was the opposite of what the code did.)

The bundled copy is an **LGPLv3** build placed by the installer as a sidecar. Which build, and how the licence is honoured, is `THIRD_PARTY_NOTICES.md`'s job.

That obligation is real rather than a footnote: requiring a separate ffmpeg install would break the zero-configuration goal (§3, C6) harder than any model download, because it is a system-level install rather than a file fetch.

**Resampling happens in ffmpeg.** Decoding asks for 16 kHz mono directly (`-ar 16000 -ac 1`) so ffmpeg's proper resampler does the work. sherpa-onnx also ships a `LinearResampler`, but linear interpolation is a poor fit for 44.1 kHz → 16 kHz; letting ffmpeg handle it is both simpler and better.

**Every spawn is `CREATE_NO_WINDOW`, and on Windows that is a requirement rather than a preference.** The window is a GUI program (`windows_subsystem = "windows"` in `main.rs`) and ffmpeg is a console program, so each child is given a console of its own and Windows draws it: a black rectangle once per spawn, plus one held for as long as a file takes to decode. That was the first thing anyone who *installed* this reported, and it is invisible from the command line, which already has a console. `ffmpeg::command` is the only way a spawn happens, so a fourth call site cannot be added without it. `DETACHED_PROCESS` would hide it too, and is wrong: it costs the pipes and the exit status the rest of this design is built on.

### 4.8 The coverage guard

**Every other stage can fail loudly. The segmenter fails silently and destroys
the input.** A recogniser that cannot hear produces wrong text, which is
visible; a segmenter that decides a file contains no speech produces a short
subtitle, which is indistinguishable from a short recording. Measured: 11 of
898 utterances, where the detector's own confidence never rises above 0.5 and
whose character error rate is 28% against 5.7% for everything else — see
`tasks/asr-evaluation.md` §13.

So the pipeline measures what the segmenter did and refuses to believe an
implausible reading. Three quantities, in samples of the decoded stream:
**decoded**, **kept** — the *union* of the spans' time ranges, merged rather
than summed, because spans carry leading padding and summing would credit the
same audio twice — and **energetic**, the audio that is not silence, judged
against the file's own loudest window rather than an absolute level so that a
quiet recording is not mistaken for an empty one.

When the kept fraction falls below `GuardSettings::floor`, and there was at
least `min_energetic_seconds` of sound to judge, the file is recognised whole
in fixed-length blocks and the first transcript is discarded —
`Event::TranscriptDiscarded` tells the interface, which has already been shown
it.

Two properties are load-bearing and neither is incidental:

- **The fallback does not lift the memory ceiling.** It cuts at the same
  interval the detector's spans are capped at, because holding a four-hour
  recording resident is the failure segmentation exists to prevent.
- **It is tuned to be safe rather than effective.** The floor is 0.70, below
  the point where recovery starts damaging files that were already right.
  Across 5049 utterances in five datasets it fires nine times, all in the one
  dataset with the problem, and every one improves.

**It is a guard, not a repair.** Of the eleven files known to defeat the
detector, only four have low coverage; the rest kept nearly all their audio
and were mis-recognised anyway. This catches the losses, which are the
catastrophic ones, and does not catch the rest. The root cause of the low
confidence is still unknown.

### 4.9 Storage: the cache, resume, and where a transcript goes

`verse-store` answers three questions that all reduce to "where does this live
on the user's machine": which file a transcript is written to, whether this
exact work has been done before, and how far an interrupted file got.

**Everything is per-user, and nothing is guessed.** The output directory
resolves `VERSE_OUTPUT` → the shell's Documents folder + `Verse` → `home/
Documents/Verse` → local app data. OneDrive redirection is followed, because
that is where the user's Documents actually is, but it is **detected and
reported once**: the program makes no network call, while the user's sync
client will upload whatever lands there, and that is theirs to know.

**A cache is never allowed to break the product.** An entry that will not
parse, an unknown version, a half-written file — each is a *miss*, never an
error, and the work is simply redone. Writes go to a temporary name and are
renamed into place, so a crash cannot leave a file that reads as complete.

**A cache hit must be indistinguishable from a run, to everything downstream.**
The hook sits inside `Transcriber::transcribe` rather than in each front-end
precisely so this is true once instead of three times: a hit publishes the same
`JobStarted` → one `TranscriptSegment` per segment → `TranscriptFinal` →
`JobFinished` sequence. Publishing only the final transcript would leave the
window showing an empty result while reporting success. The one thing a hit
skips is the work.

**Resume is per span, and its failure mode is redoing work.** `recognize`
already resets the engine for every span, so spans are genuinely independent
and a span's stored result can be reused when its samples are identical. A
resumed run re-decodes and re-segments the file from the start — sherpa-onnx
does not expose the detector's internal state — but skips the recognition,
which is the dominant cost. If a span's identity does not match, it is
recognised again. It is never approximated, and it is never fabricated.

**A second reader, added 2026-10-08.** The window can now be closed without
ending the process (`tasks/tray.md`), so the log is no longer only the crash
recovery it was written as: quitting with work in flight is a deliberate act
that leaves the log behind on purpose, and the files it was working through are
written to `pending.json` beside it and put back as waiting rows on the next
launch. The key is the settings *and* the input, so the queue carries the engine
with it — a queue restored under a different engine would miss every key and
redo the work the confirmation dialog promised would be kept.

### 4.10 Model residency: `ModelKeeper`

A model is the largest thing this program holds — 228 MB for the default, about
a gigabyte for Qwen3 — and reading it takes longer than recognising a short
utterance. `Transcriber` was built to be loaded once and used for many files;
`ModelKeeper` is what owns it across those uses.

**Why it exists, measured.** The window used to load a model *inside* every
per-job worker thread, and the thread ended with the job. Five files of one
short clip:

| | per file | loads | total |
|---|---|---|---|
| load per file | 1549, 1501, 1711, 1512, 1547 ms | 5 | 7820 ms |
| one keeper | 1564, **284, 306, 276, 309** ms | 1 | **2739 ms** |

The shape is the point: loading per file is flat at 1.56 s however many files
there are, while a keeper is `1564 + (N−1) × 290`. `cargo run -p verse-pipeline
--example reuse` reproduces it, and validates its own instrument first.

**Identity is the settings digest of §4.9.** Not a narrower "same engine" test,
and the reason is correctness rather than tidiness: `Transcriber::run` reads the
VAD and guard settings and the VAD model path out of the request it stored when
it was loaded, so reusing across a change in any of those would run the *old*
settings and produce a transcript for a configuration nobody asked for. A second
identity function that drifted from the digest would cause exactly that, which
is why there is one.

The cost of that choice is that the digest over-covers: it includes settings
that are not baked into the engine, so changing a detector threshold costs a
reload. The window cannot hit this — it builds the same defaults on every job —
and a service would pay one reload per change. Revisit with a number.

**The model mutex is held for the whole job.** `Transcriber` is `Send` and not
`Sync` (`AsrEngine: Send` and nothing more), so exclusive access is forced rather
than chosen. It also gives the honest semantics: one model, one job. `release`
therefore means "after the current job" and not "cancel" — a caller who wants to
stop now has the job's `CancelToken`. `status` reads an atomic and never the
lock, because the only time anyone asks is while a job is running.

**No event.** `Event::ModelStateChanged` has been defined and unused since the
first phase. Adding a second event that nothing subscribes to would repeat that;
reusing the first would make `Ready` mean both "the file is on disk" and "the
weights are resident", which is one word for two facts. `status()` is a query
and answers the same question for the window and for whatever service comes
next. When something needs to be *pushed*, that is the moment to decide what it
should say.

**A load failure announces its job before it announces the failure.** That order
is load-bearing, not tidiness: a job id is claimed by `JobStarted`, and the
window's screen discards output for an id it has not claimed. A failure arriving
alone left the window on "正在准备…" with a cancel button for a job that was
never running — reachable whenever a model is present at the expected size but
unusable, since `is_present` checks size only.

**The command line and the benchmark do not use it.** Both already load once and
reuse through `set_input`; neither has a second caller, an idle state, or a
reason to release. And the CLI's `--jobs N` loads one model *per worker*
deliberately — one keeper's single mutex would serialise the workers and turn
`-j 4` back into `-j 1`.

### 4.11 `verse serve`: the pipeline behind a loopback socket

Other programs on this machine can ask Verse to transcribe a file. `verse serve`
runs until killed, listening on `127.0.0.1` and only that, and §4.5 carries the
argument for why a listener does not weaken the offline guarantee.

**It is a subcommand of `verse`, and the reason is structural.** `verse-cli` has
no `[lib]` target, so `report.rs` — the pinned result wire and the contract tests
that pin it — is unreachable from another crate. A separate binary would have to
describe a transcript a third time. This way a job's result **is** the
`FileResult` that `verse transcribe --json` emits, and `GET /models` is the
`ModelList` that `verse model list --json` emits.

**Job-based, because a one-hour file takes minutes.** `POST /jobs` returns an id
and `202`; `GET /jobs/{id}` reports state, progress and — once settled — the
result; `DELETE /jobs/{id}` cancels. A synchronous call could show no progress
and could not be stopped.

**A pool of workers, sized from the machine and the model.** Each worker is a
`ModelKeeper`, and therefore a resident model, so the size is a rule rather than
a guess:

```
per_worker_bytes = model_bytes + RUNTIME_OVERHEAD_BYTES
pool = min(MEMORY_BUDGET_BYTES / per_worker_bytes, engine_threads)
         .clamp(1, MAX_WORKERS)
```

The budget is **a quarter of the machine's memory, read from the machine**.
Reading it needs FFI on Windows and macOS, which this project forbids in its own
code, so `sysinfo` does it — the same arrangement as sherpa-onnx doing the FFI
for recognition. `verse-core` keeps its empty dependency list: the sizing *rule*
takes bytes, and the service is what supplies them.

**This was a constant first, and the constant was wrong in the direction that
costs most.** A quarter of the 8 GB floor §3 requires gave a machine with 31 GiB
the same pool as one with 8 — and the budget came out about the size of a single
Qwen3 worker, so the model that benefits most from a pool was given two where
the machine could hold seven. When the probe cannot answer, that constant is the
fallback, and `/health` says which of the two it used.

**Total, not available.** Total is a property of the machine; available is a
property of this moment. Sizing a long-lived pool from what happened to be
running at startup would shrink it because somebody else was compiling, with
nothing visible to explain why.

On this 16-core, 31 GiB machine that gives eight workers for SenseVoice and seven
for Qwen3-ASR.

**One thread budget for the whole pool**, which is an invariant and not a
tidiness: the settings digest includes the resolved thread count, so workers
with different budgets would hold different model identities and a job would be
runnable on only some of them.

Measured, 24 jobs of one clip:

| N | total wall | median per job | peak RSS |
|---|---|---|---|
| 1 | 6.03 s | 249 ms | 361 MB |
| 4 | 2.43 s | 375 ms | 1324 MB |
| 8 | 2.02 s | 590 ms | 2619 MB |

**The two numbers disagree and that is the point.** Throughput keeps improving —
three times as many jobs finish per second at six workers as at one — while
per-job latency gets worse, because each worker holds fewer threads as the pool
grows. A service makes that trade deliberately: two dozen jobs finish three
times sooner, and one job on its own is a little under twice as slow.

**The HTTP is hand-rolled and the subset is deliberate.** `httparse` parses the
request line and headers — already in the graph through Tauri, so a manifest
line rather than a crate — and everything else is `std::net` and threads. No
chunked encoding (a second framing to get wrong, and a loopback client knows its
length), no HTTP/2, no upgrades, no ranges, no CORS. Requests are capped at
8 KiB of request line, 16 KiB of headers and 1 MiB of body; connections at 64.

**Nothing grows with uptime.** This is the first thing in the project that is a
long-running process, which §3 makes a hard constraint for. Finished jobs are
evicted FIFO at 32 — a result carries the whole transcript, the largest thing
that accumulates. The queue is bounded at 32 and refuses with `429` rather than
blocking. The bus is drained by exactly one thread, because its channels are
unbounded and a subscriber that stopped reading would accumulate every event of
every job. Measured: `retained` pins at 32 while uptime grows.

**The token is a bearer token for a loopback listener, and worth being precise
about.** It stops a **browser** — which cannot read the discovery file, and
whose cross-origin requests are preflighted and never answered; a request
carrying an `Origin` header is refused outright. It stops another **user** on the
machine. It does **not** stop a process already running as this user, which can
read the same file: the boundary there is the operating system's, not this
string. And the API will transcribe any path that user can read.

**A corrupt model takes the process down, and that cannot be fixed here.**
`is_present` compares sizes, so a model present at the expected size but
malformed passes every check, reaches the engine, and then **sherpa-onnx
terminates the process** rather than returning an error — the command line exits
127 on the same input, which is not one of its documented codes. There is no
`Err` for `ModelKeeper` to announce, because there is no error to return.
Catching it would need signal handling or `unsafe`, both excluded, and running
recognition in a subprocess would kill the keeper. The common case — no model
downloaded at all — is refused at submit with a message naming the command to
run.

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

### 5.3 Which engine, decided by measurement

Two engines, both punctuating internally so neither needs a second model.

| | SenseVoice-Small int8 | Qwen3-ASR-0.6B int8 |
|---|---|---|
| Size | **228 MB** | 982 MB |
| License | ⚠️ FunASR Model License v1.1 | Apache-2.0 |
| Languages | zh / en / yue / ja / ko | 52 |
| Architecture | encoder-only, CTC-ish | Whisper frontend + encoder, LLM decoder |

Measured over 200 utterances of conversation, all three engines that were
tried (see `tasks/asr-evaluation.md`):

| | SenseVoice | Paraformer + punct | Qwen3-ASR |
|---|---|---|---|
| CER | 6.00% | 5.90% | **5.40%** |
| exact | 54.5% | 55.5% | **60.0%** |
| punctuation F1 | **84.3%** | 81.4% | 83.0% |
| time | **44.6 s** | 109.2 s | 186.0 s |

Across four more domains Paraformer was 0.6–3.1 points better on characters
and 1–8 points worse on punctuation, while taking 2.3× as long and twice the
disk. **It has been removed.** It was not beaten on every axis, but it was
beaten on enough of them that keeping a third option would have cost more
than it returned — and its one advantage was the axis least visible to the
person using the thing.

Qwen3 is kept because it is genuinely better at the job and because it is a
different *kind* of model: a second architecture makes the comparison mean
something. It is not the default. Four times the time and four times the size
buys roughly half a point of character accuracy on conversation, and on a
four-hour recording that is 22 minutes against 5.

**Why SenseVoice is still the default.** Best on punctuation, fastest, and a
quarter of the size. Its two known weaknesses are both visible in the
measurements: it is weakest on the messiest material (live commerce at
11.30% CER against 4.80% for conversation), and it never emits an exclamation
mark.

**Its license is not Apache-2.0**, despite a widely-copied ModelScope metadata
field saying so. The official HuggingFace card has always pointed at the
custom FunASR Model License v1.1, which *does* permit commercial use but
requires attribution, retention of the model name, and shipping the license
text. `THIRD_PARTY_NOTICES.md` records the obligations. Acceptable for an
open-source project; it would need review for a closed-source one.

**An earlier version of this section ranked the engines on three sample
clips.** They agreed on one, disagreed on two, and both disagreements landed
on English content — which was read as "Paraformer is better at code
switching, keep it as an alternative". Five thousand scored utterances later,
that reading does not survive. Three clips is a hypothesis, not a
measurement, and the fact that it was recorded as a caveat did not stop it
from being relied on.

**Switching engines is configuration.** Both are registered in
`verse-asr::register_builtin_engines`. SenseVoice and Qwen3 use the same
`model.int8.onnx` + `tokens.txt` layout; Qwen3 needs four graphs and a
tokenizer directory instead, which the factory hides.

### 5.4 Punctuation

**Both registered engines punctuate internally.** There is no punctuation
stage, no punctuation model, and nothing to configure. That is a change from
earlier drafts of this document, which described a `TextProcessor` stage and
a 294 MB CT-Transformer model for the engines that needed one.

The history is worth one paragraph, because it is the same mistake twice.
Punctuation was treated as a property of the engine rather than a stage, on
the grounds that the default engine happened to do it. When a second engine
was tried, the comparison between them silently compared a two-stage pipeline
against a one-stage one — and the difference was read as accuracy. The stage
was built, the comparison was redone properly, and the answer was that
neither engine needs it: Paraformer lost on enough other axes that it was
removed rather than accommodated (see `tasks/asr-evaluation.md` §9), and both
survivors punctuate themselves.

So the model is gone, along with `Punctuator` and the stage. A third engine
that emits bare text would need one again; that is a problem to solve when
there is such an engine, not before.

**Measured quality, since it is now the only thing deciding this.** Per-mark
F1 on the conversation subset, from `tasks/asr-evaluation.md`:

| mark | SenseVoice | Qwen3-ASR |
|---|---|---|
| 。 | 89.5% | 88.5% |
| ， | 77.0% | 77.6% |
| ？ | 90.6% | **95.1%** |
| ！ | never emitted | emitted |

Commas are the weak mark for both — placed correctly when placed at all, and
missed roughly a third of the time.

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
| P1b | desktop window (Tauri + Svelte; this said "slint" until it was corrected) | Drag file → progress → result → export, no terminal involved. The **progress** half of that was not built until 2026-10-07 — see `ui-design.md` §2 for why, and for what made it possible |
| P2a | Loopback capture + streaming subtitles | Play a video on Windows; floating window shows text live |
| P2b | Translation | Bilingual zh/en subtitles |

P1a is where the risk lives: the model fetcher under real Chinese network conditions, and the sherpa-onnx build with a pre-staged archive.

## 10. Risks and open questions

| Risk | Impact | Mitigation |
|------|--------|-----------|
| SenseVoice license is custom, not Apache-2.0 | Attribution and name-retention obligations on a distributed build | **SenseVoice is the default.** This row said "default to Paraformer-large" and was overtaken: Paraformer was removed for being beaten on three axes out of four, so there is no fallback engine to default to. The obligations are real and listed in `THIRD_PARTY_NOTICES.md`; the licence text still has to be shipped. |
| Third-party ModelScope upload | Supply-chain integrity | SHA-256 pinning; prefer hf-mirror for that model if hashes cannot be trusted |
| ~~Punctuation model not yet selected~~ | — | **Moot.** Both engines punctuate internally and the model was removed; see §5.4. |
| GitHub prefix proxies are third-party | Build/fetch failures | Configure two; document manual pre-staging |
| sherpa-onnx archive filename is version-locked | Breaks on every crate bump | Pin the crate version; record the archive name |
| No single stable China model host | Download failures | 4-layer fallback including mandatory manual import |
| `sherpa-onnx` remains FFI | Residual leak surface | Accepted; monitor upstream. `verse-asr` isolates it so a swap stays local |
| OS loopback capture differs per platform (P2) | macOS needs a virtual audio device | Out of P1 scope; revisit at P2a |

### Open questions

1. ~~Model bundled or fetched on first launch?~~ **Decided:** one lightweight model ships inside the installer; heavier models are opt-in downloads. See §5.3.
2. ~~Exact punctuation model repository?~~ **Moot:** both engines punctuate internally, so there is no punctuation model. See §5.4.
3. ~~Windows audio/video container support in `symphonia`?~~ **Moot:** the decoder is the ffmpeg sidecar, which covers every container ffmpeg does — including video. See §4.7.
