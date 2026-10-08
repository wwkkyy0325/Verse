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

## 2026-10-05 — P1b started: interface designed, state model built

**GUI framework: Slint, and it is not a permissive licence.** `design.md` §4.1
had named Slint since the first draft without recording what it costs. It is
triple-licensed — GPLv3, which would force the whole distributed work under the
GPL and is incompatible with this project's Apache-2.0; a commercial licence;
or a royalty-free desktop licence that **requires attribution**. Verse takes the
third. The obligation is an `AboutSlint` widget reachable from the About dialog,
which is where the FunASR attribution has to live anyway, so one dialog
discharges both. Recorded in `THIRD_PARTY_NOTICES.md` with the other options
written out, because the next person to read the licence text will have the same
question.

Its technical fit is better than expected: the femtovg and software renderers
are pure Rust, so this introduces **no C++ toolchain requirement** — the
property `sherpa-onnx` was chosen to preserve. The Skia renderer is the one that
needs MSVC and its feature is off. The software renderer also matters on its own
terms: a 2013 machine with weak OpenGL can be switched to pure CPU raster at
runtime, which is the difference between a window and no window.

**`ui-design.md` is the frontend design.** Six requirements derived from the
"no technical background" brief rather than asserted as taste — one-step default
path, no user-facing technical decisions, no jargon, everything over a second
cancellable, progress as visible output rather than motion, every failure naming
a next action. Five screens, a token set, a component list, and the copy rules.
The point of writing it first is that P6 and P3 are the two that get violated
under schedule pressure, and a document is what makes violating them visible.

**The state model came before the window.** `state.rs` is `AppState`, `Screen`,
`Working`, `Done`, `Failed`, `Recovery` and `Effect` — ordinary Rust over
ordinary values, no Slint import, side effects named and returned rather than
performed. 23 tests drive the whole machine without opening anything. Steps 2
and 3 of the task need a human looking at a screen; this step did not, so it
went first and the round trip with a person is now about only the things a
person can judge.

**Two things the design got wrong, corrected by building it.** A `Phase` enum
distinguishing decoding from recognition was dropped — the event vocabulary
carries no stage event, and adding one to `verse-core` to drive a label the user
cannot act on is a bad trade. `Working::has_output()` derives the distinction
that was actually needed. And the planned `UiMessage` enum between `EventBus`
and the UI was cut: the bus already carries the shape both halves agreed on, and
translating between two identical shapes is only somewhere for them to drift
apart.

**Cancelling now waits for acknowledgement.** The first draft returned to the
drop target on the click. That shows an idle screen while the job is still
winding down — a lie, and an invitation to start a second job on top of the
first. It sets `stopping` and waits for `JobCancelled`, at a cost of up to a
second of "正在停止…".

**Noted and left alone:** `verse_core::ModelState` and the
`ModelStateChanged` event are referenced only by a test inside `verse-core`;
nothing produces them. The downloader has its own `DownloadState` and the UI
reads that, because `ModelState` cannot express progress. One unused
model-state type now sits beside a used one. Worth consolidating; not worth
doing as a side effect of this step.

**Awaiting a human:** step 2, whether Chinese renders. `app.slint` is a
specimen page — punctuation, traditional/simplified mixing, rare glyphs, both
text sizes — with an explicit font-family list, because which CJK font the
platform fallback picks is not something to leave to chance. Building cannot
answer this; looking at it can.

## 2026-10-05 — Frontend switched to Tauri 2 and Svelte

The native-toolkit decision lasted one session. The requirement it missed was
not stated until it was: the interface should be written in a browser language
and should have a hot-reloading dev loop. A compiled GUI toolkit offers
neither, and the DSL is the wrong kind of constraint for iterating on a layout.

**What replaced it: Tauri 2 + Svelte 5 + TypeScript, built by Vite.** The Rust
crates below the interface are untouched. On licensing this is a straight
improvement — Tauri is MIT/Apache-2.0 and Svelte is MIT, matching the project,
so the attribution obligation the previous toolkit required is gone, along with
the About dialog requirement it created. `THIRD_PARTY_NOTICES.md` loses an
entry rather than gaining one.

**What it costs: WebView2.** Tauri ships no browser engine, so on Windows the
supported *operating system* floors at Windows 10 1803, while the pipeline is
happy on 2013 hardware. That gap is a product decision, not a technical one,
and it is recorded as an open question rather than buried. WebView2 is also
multi-process, so the empty-window memory baseline is a step back from a native
toolkit — it does not threaten the no-leak constraint, but "lightweight" takes
a hit that should be measured rather than assumed.

**The state model crossed unchanged.** `state.rs` was written with no framework
types in it, and it moved from a native toolkit to a web frontend without an
edit. That was the entire argument for writing it that way, and it is now a
demonstrated result rather than a claim.

**The dev server port got a contract, after the first attempt went wrong.**
Vite's default 5173 was already occupied by an unrelated dev server, and the
window silently attached itself to *that* project — a failure indistinguishable
from success. `tauri.conf.json` is now the single place the port exists;
`vite.config.ts` reads `build.devUrl` for its own `server.port` and refuses to
start without it. `strictPort` turns a busy port into an error instead of a
quiet relocation, and the host is an explicit `127.0.0.1` so `localhost` cannot
resolve to `::1` against an IPv4 listener. Port 17321: above 1024, below
Windows' dynamic range, and not a default of anything a developer is likely to
have running.

**The icon is generated by a script, not checked in as a binary.**
`tools/make-icon.mjs` draws it with signed-distance functions and writes the
PNG itself. It produced two bugs worth keeping: supersampling offsets applied
in pixel units instead of normalised ones, which magnified and cropped the
mark, and a capsule distance written in the rounded-box form — that is
`|dx| + |dy|`, which is a diamond. Both were caught by looking at the output,
and both are commented where they were made.

**Also:** the provisional scaffolding from `create-vite` was removed rather
than left in place, and `node_modules/` and the build output are ignored.

**Awaiting a human:** step 3, Chinese renders and the palette is readable.
Everything else in the loop — compilation, window, IPC, port agreement — is
verified by machine.

## 2026-10-06 — Interface rebuilt on shadcn-svelte

The requirement was to stop hand-grinding the interface and stand on something
mature. **shadcn-svelte** was the answer, mostly because of how it delivers:
components are written into `src/lib/components/ui/` as source rather than
installed as a dependency, so "take the whole thing" is literal — the code is
ours, and there is no upstream to fork.

Eight components — button, card, dialog, progress, scroll-area, separator,
alert, sonner — cover all five screens. Tailwind v4 came with them, and the
hand-written `tokens.css` is gone, replaced by shadcn's CSS-variable theme.

**The preset took a detour worth recording.** `init` demands a `--preset`, and
a preset turns out to be a base62-encoded config blob generated by a *website*
— there is no list of valid names, and an invalid one only prints "not a valid
preset" before falling back to an interactive prompt that does not read stdin.
Rather than guess, the CLI's own package was unpacked and `encodePreset()` was
called directly to produce `bdxlHoz4q`: neutral base, blue accent, **noto-sans**,
lucide icons. That is the honest route and it is reproducible, which guessing
would not have been.

**Two configuration traps, both silent until they are not:**

- shadcn's CLI reads `paths` from the *root* `tsconfig.json`. The Vite template
  uses project references, so the real config lives in `tsconfig.app.json` and
  the root file has no `compilerOptions` at all. The CLI refuses to write
  anything until the alias is declared in both, and nothing warns about the
  duplication.
- TypeScript 6 rejects `baseUrl` as deprecated. The `paths` entries are
  relative, so it is not needed — but every shadcn installation guide still
  includes it.

**The font default was 400 kB of glyphs nobody would draw.** shadcn installs
`@fontsource-variable/noto-sans` wholesale, which pulls eight subsets —
Devanagari, Cyrillic in two blocks, Greek in two, Vietnamese, Latin extended —
into every build. A Chinese transcription tool draws none of them. Pointing at
the single Latin file took the payload from ~440 kB to 36 kB. Full-width
punctuation falls outside the Latin ranges and reaches the Chinese system font
anyway, which renders it better to begin with.

**`strictPort` earned its place immediately.** Relaunching hit "Port 17321 is
already in use" — a `vite` child process from an earlier run had survived its
parent being killed. That is exactly the failure the setting exists for: with
Vite's default behaviour the server would have moved to 17324 and the window
would have opened blank, with no indication why. The orphan was identified by
port and removed.

**Awaiting a human:** the same question as before, now against a real
component set — does Chinese render correctly, and is the palette readable in
both light and dark.

## 2026-10-06 — The path is connected: drop a file, get text

The specimen page is gone and the interface now does the one thing it exists
for. Drop an audio file on the window, or click to choose one, and the
transcript appears as it is recognised.

**Rust owns the state; the window holds a copy.** `state.rs` gained an
`Applied` return from `apply()`, saying what visibly changed — the screen, one
more segment, progress — and the bridge turns that into an update. Not a
snapshot: a two-hour recording is a few thousand segments, and resending the
list to move one number is not affordable. `Applied` carries no serialization,
so the state machine still knows nothing about the window.

**Three new files, one direction each:**

- `pipeline.rs` — the same chain `verse transcribe` runs, with progress
  published to the bus instead of printed. It is a second copy of that
  orchestration, which is a real cost; the CLI will want to move onto it.
- `bridge.rs` — a worker thread that runs a job, and a forwarding thread that
  drains the bus into the state and out to the window.
- Capabilities and the dialog plugin, so the window may open a file picker.

**A test caught a contract bug immediately.** Nothing connects the Rust update
types to the TypeScript that reads them: a renamed field compiles on both
sides and fails at runtime, in a window, as blank values. So the JSON shape is
now asserted directly. The very first run failed — `rename_all` on an enum
renames its *variants*, not their fields, so the backend was sending
`start_ms` while the frontend read `startMs`. Every timestamp would have been
`undefined`. Both facts are commented where they bit.

**Dark is the default, not a preference.** The `dark` class sits on the root
element rather than being resolved from the OS.

**Two smaller things.** The decoder reports no total length, so a percentage
progress bar is not available without a probe pass over the file; the header
shows elapsed time and recognised segment count instead, which is honest and
free. And a retry needs the full path while the interface is deliberately only
ever sent file *names* — the window keeps the path it was handed for its own
use.

**A plugin version trap:** `cargo add tauri-plugin-dialog` selects
`2.0.0-rc.8`. Pinned to `2` instead.

**Still open from this step:** export is not built. The transcript is on
screen and selectable, but there is no way to write it to a file yet — which
is the `verse-core::export` code that already exists and is tested, waiting
for a button.

## 2026-10-06 — Measured, and the measurement changed the picture

Everything before this was reasoning about the recogniser. This is the first
time it has been scored against labelled audio, and the first three findings
each contradict something that had been assumed.

**The harness.** `crates/verse-bench` scores a manifest of
`id / wav / reference` and reports a character error rate plus the worst
utterances, because a number alone is not a diagnosis. `tools/asr-eval/`
unpacks the datasets. The **pipeline was extracted into `verse-pipeline`**
first — it had already been copied once between the CLI and the app, and a
benchmark scoring a third copy would have been measuring a program nobody
ships. The recogniser is also now loaded once per run rather than per file;
at 228 MB a load, scoring thousands of utterances any other way is mostly a
disk test.

### Finding 1: the first number was wrong because of what it measured

AISHELL-1 test, 2307 utterances, **CER 8.44%**. Then looking at the worst
cases rather than the mean:

```
ref  十七万套        got  17万套
ref  百分之四        got  4%
ref  零三年          got  03年
```

Reference transcripts spell numbers out; the recogniser normalises them. Both
are correct — they are written differently. **1153 of the 2824 "errors", 41%,
were formatting.** With ITN off the same run scores **5.00%**.

That number is now a knob rather than a hidden constant:
`EngineConfig::inverse_text_normalization`. On in the product, because that is
what a person would write; off in the benchmark, because the yardstick spells
numbers out.

### Finding 2: punctuation had to be scored separately, and is worse

The character rate strips punctuation, which is standard practice and also
hides the thing this project chose its engine for. SenseVoice is the default
**because it punctuates**; scoring it off a character rate makes that decision
invisible.

Speechio-Formal, conversation subset, 898 utterances, punctuated references.
Punctuation is scored the way the restoration literature does — precision,
recall and F1 **per mark**, aligned by character so that a mark is judged by
where it lands, not merely by appearing:

| mark | precision | recall | F1 |
|---|---|---|---|
| 。 | 80.5% | 87.4% | 83.8% |
| ， | 86.4% | **60.6%** | **71.3%** |
| ？ | 91.7% | 81.6% | 86.4% |
| ！ | 0.0% | 0.0% | 0.0% |
| **overall** | **83.6%** | **75.2%** | **79.2%** |

Two things stand out. **Commas are placed correctly when placed at all but
missed 40% of the time** — 255 of 648. Full stops and question marks are
fine. And **exclamation marks are never produced at all**; only four occur in
the sample, but the direction is unambiguous.

### Finding 3: whole utterances were being discarded before recognition

The worst list for the conversation set was not full of wrong transcripts. It
was full of **empty ones**:

```
[000242] ref 逛集市喽，去逛集市喽。妈妈，你快点儿。
         got (empty)
```

17 of 898, just under 2%. The audio is intact — checked with `ffprobe` and
`volumedetect`: 2.4 to 5.0 seconds at −7 to −11 dB, normal speech level.
`verse transcribe` on the same file reports **"0 spans"**: the voice detector
found no speech in it at all.

This dataset is conversation; AISHELL is read news. **The failure mode only
appears on the material that resembles real use**, which is exactly what the
first dataset could not show.

The threshold was hardcoded. It is now `vad_threshold`, plumbed through the
pipeline and settable with `--vad-threshold`, and the first sweep says the
default was too high:

| threshold | CER (200 utterances) |
|---|---|
| 0.30 (was) | 8.58% |
| 0.10 | 6.75% |
| 0.05 | 6.00% |
| 0.03 | 5.93% |

**A 31% reduction in error rate from one constant**, and the default had been
chosen by reasoning rather than measurement.

### The default threshold was wrong, and is now 0.05

The sweep completed across both datasets, 200 utterances each. It settles the
question rather than pointing at it:

| threshold | read news | conversation |
|---|---|---|
| 0.30 (was) | 2.28% | 8.58% |
| 0.10 | 2.07% | 6.75% |
| 0.05 | 2.11% | 6.00% |
| 0.02 | 2.14% | 5.76% |

On clean read speech the threshold hardly matters. On conversation it is worth
a third of the error rate. **0.05 is the new default** — not the 0.02 that
scores marginally better, because sherpa-onnx refuses anything at or below
0.01 and the margin is worth more than two hundredths of a point.

Full conversation set, 898 utterances, before and after:

| | 0.30 | 0.05 |
|---|---|---|
| CER | 9.07% | **6.84%** |
| exact matches | 47.4% | **51.7%** |
| p90 | 28.6% | **20.0%** |
| **p99** | **100%** | **57.1%** |
| utterances with no output | 17 | **1** |
| punctuation overall F1 | 79.2% | **83.0%** |
| comma F1 | 71.3% | **76.7%** |

Punctuation improves as a side effect, for the obvious reason: a sentence that
was never transcribed has no punctuation to score.

A related trap fixed on the way: sherpa-onnx rejects a threshold at or below
0.01, but reports it as **"failed to load VAD model"** — sending anyone who
reads that message to inspect a model that is perfectly fine. `verse-audio`
now range-checks first and names the parameter.

### What is left

The worst remaining cases are mostly **partial** truncations rather than
missing utterances — half a sentence survives and the rest is gone:

```
ref  逛集市喽，去逛集市喽。妈妈，你快点儿。
got  逛集市了去逛集市了。
ref  妈妈，妈妈，快来，别淋湿啦！
got  别淋湿啦。
```

That is a boundary decision, not a lost utterance: `min_silence_duration` is
0.25 s, and conversation is full of pauses shorter than that which still split
a sentence. Raising it would keep sentences whole at the cost of coarser
subtitles, which is a trade to measure rather than assume — the same way this
one was.

Two smaller clusters: rare proper nouns heard as common words (萝卜头儿 →
老八头, 萝卜头儿 → 龙头), and counting sequences (一、二、三…七 → 567). The
second is partly ITN again.

**Also outstanding:** the AISHELL tail-truncation seen earlier (供求关系 →
供求, 消费环境 → 消费) is the same boundary problem in a different dataset.

## 2026-10-06 — Across domains, and the reference was wrong

Two datasets was enough to find the threshold. It was not enough to say
anything about accuracy, so five more domains were scored — meeting, phone
call, documentary, live commerce and a second conversation set, 300
utterances each.

The first table said the worst domain was five times worse than read speech.
It was not. The measurement was wrong.

**`target_text` is not a transcript.** It is formal written Chinese — filler
removed, phrasing rewritten, expressions normalised — supplied by the dataset
to train text-normalisation models. Scoring recognition against it measures
the recogniser *plus a rewriting stage this product does not have*, and marks
it down for faithfully reproducing what was said:

```
ref  颜色、色料以及上唇效果都非常出色，而且其切面很大，便于涂抹。
got  颜色啊、色料啊，包括它的上唇的效果啊，真的非常厉害。
```

The recogniser is right. The reference is a different task. The dataset
carries `original_text` — the verbatim spoken form — for exactly this reason,
and `extract.py` takes `--reference` to choose. Both were extracted and scored
against the same audio:

| dataset | vs written | vs verbatim | gap |
|---|---|---|---|
| conversation | 6.66% | **5.00%** | −1.7 |
| meeting | 10.67% | **7.64%** | −3.0 |
| phone call | 12.16% | **8.19%** | −4.0 |
| documentary | 14.71% | **6.93%** | −7.8 |
| live commerce | **37.30%** | **11.31%** | **−26.0** |

**Twenty-six points of the worst number were the yardstick.** The real spread
is 5.00% to 11.31%, which is a range someone can act on.

This is the second time in two days that a measurement mistake looked like a
product defect — the first was counting ITN as error, worth 41% of a rate.
Both were found by reading the failures rather than the number.

**The arrangement that follows:** recognition is scored against
`original_text`, punctuation against `target_text`. That is not a compromise.
`punct.rs` compares marks only where both sides agree on the character, so
rewritten phrasing is skipped rather than penalised — the punctuation figures
were never affected by this, and the character rates were affected badly.

The tool now takes several manifests and prints the table itself, so the
spread is the default view rather than something assembled by hand.

**A label to distrust:** `ZH00007` is named *sports commentary* and contains,
in the sampled rows, live commerce — a presenter recommending cosmetics. The
audio is what it is; the label is the dataset's.

### What the corrected numbers say

| dataset | CER | exact | punct F1 |
|---|---|---|---|
| conversation | 5.00% | 59.0% | 83.2% |
| documentary | 6.93% | 34.3% | 73.2% |
| meeting | 7.64% | 47.3% | 88.2% |
| phone call | 8.19% | 30.7% | 76.5% |
| live commerce | 11.31% | 27.7% | 66.3% |

Phone calls and live commerce are the hard cases, and the audio explains both:
narrowband, and fast overlapping speech over music.

Meeting punctuation is the **best** of the five, which is the opposite of what
its character rate suggests. Meetings are hard to hear and easy to punctuate,
because the pauses are real.

## 2026-10-06 — The punctuation model was surplus, and is gone

Both registered engines punctuate internally. The 294 MB CT-Transformer model,
`verse_asr::Punctuator`, and the pipeline stage built for it in the previous
commit have all been removed. `models.json` lists three models again:
SenseVoice, Qwen3-ASR, and the VAD.

The stage existed for less than a day. It was built to make the Paraformer
comparison fair — which it did, and which is how Paraformer was found to be
not worth keeping. With Paraformer gone and both survivors punctuating
themselves, nothing used it. Keeping a mechanism because it might be needed
later is how a codebase accumulates the thing this project keeps removing.

**`TextProcessor` and `TextChain` went with it.** They were the abstraction
the stage was built on, and after the removal they had no implementation and
no caller. Worth noting *why* they were easy to drop: §4.2 of `design.md`,
the canonical list of core traits, never contained `TextProcessor` — it had
been added alongside the punctuation work and only ever appeared as a name in
a sentence in §4.1. An abstraction that never made it into the design's own
trait list, and has no implementation, is not a foundation.

`TextSink` stays: it *is* in §4.2, defined and unimplemented, which is a
different thing from being added in passing.

**What a third engine would need.** If one is added that emits bare text —
most CTC and transducer models do — the punctuation stage comes back. That is
a problem to solve when there is such an engine, and the previous commit's
diff is where to find the solution.

### Where the model list landed

| model | size | role |
|---|---|---|
| SenseVoice-Small int8 | 228 MB | default — fastest, smallest, best punctuation |
| Qwen3-ASR-0.6B int8 | 982 MB | most accurate on every domain measured |
| Silero VAD | 2.3 MB | required |

Three, down from four. The rule applied was not "is it used" but "does
anything reach it" — and for both the punctuation model and `TextChain`, the
answer was no.

## 2026-10-06 — The sentences that lost their second half

§5 of `tasks/asr-evaluation.md` left a question open: some utterances come back
with half the sentence missing, and neither `min_silence_duration` nor the
decoder's token budget explained it. Traced one utterance through the pipeline
and found the answer — **the missing half was never handed to the recogniser.**

`verse transcribe` on `000242`, a 5.03-second file, reports one span covering
**0.00 → 1.83 s**. On `000465`, a 3.87-second file, one span covering
**2.67 → 3.84 s** — the first two thirds discarded. No error, no warning; a
fragment of audio becomes a short, confident, wrong subtitle.

The detector, not the recogniser. Silero's own per-window probability, measured
directly with onnxruntime, peaks at **0.338** on that file where healthy audio
sits at 1.000. Across all 898 conversation utterances, 887 reach 0.5 or above
and score 5.74% CER; the 11 that never do score **28%**.

### What this changed

Nothing in the product yet — the section is a measurement, not a fix. What it
changed is the standing explanation: the truncation was an audio-loss bug, not
a segmentation-tuning problem, and the pipeline has no way to notice it.

### Three hypotheses eliminated

Each looked convincing from the numbers and each was tested.

- **Low-frequency loss.** The failing files have 1.5% of their energy below
  300 Hz against 32% for a healthy one, and a missing fundamental is a
  reasonable way to defeat a speech detector. High-passing a healthy file to
  the same energy distribution leaves its probability at 1.000, and the phone
  dataset has 42% low-frequency energy with zero failures. Correlation only.
- **Harmonic periodicity.** The failing files are *more* periodic, not less.
- **The threshold.** 0.02 instead of 0.05 improves the 11 files from 37.76% to
  32.65% and does not come close to fixing them.

The root cause of Silero's low score is still unknown. That is recorded as
unknown rather than as the least-refuted hypothesis.

### A measurement that had to be repaired before it was believed

The first probability probe returned 0.001 for every file, including ones that
transcribe perfectly — Silero v5 needs the previous window's trailing 64
samples prepended as context. It was only trusted after reproducing a
known-good file at 1.000. Similarly, the first span count was read off a
`target/release/verse.exe` built fourteen hours before the VAD change and
reported `0 spans` for files the benchmark had just scored; rebuild before
believing a difference.

### Also corrected

§1 of `tasks/asr-evaluation.md` claimed the CLI shares `verse-pipeline` with
the window. It does not — the chain was extracted from `verse-app`, and
`verse-cli` still carries its own copy. The two are behaviourally equivalent,
so no measurement is invalidated, but the claim was wrong and is now marked.

## 2026-10-06 — A guard against silent audio loss, and one fewer pipeline

§13 of `tasks/asr-evaluation.md` established that the segmenter sometimes
discards most of a file and the pipeline believes it. This builds the defence,
and removes the duplicate pipeline found on the way.

### The guard

`verse-pipeline` now measures what the segmenter did — the audio decoded, the
audio kept, and the audio that was not silence — and re-recognises a file whole
when the kept fraction is implausible. The measurement came first and changed
nothing: the run that introduced it produced a transcript byte-identical to the
one before it.

The three quantities are chosen so the measure means something. **Kept audio is
the union of the spans' time ranges, merged rather than summed**, because spans
carry leading padding and summing would credit the same audio twice — inflating
it makes the guard fire less often, which is the wrong direction to be wrong
in. **Silence is judged against the file's own loudest window**, not an absolute
level, so a quietly recorded file is not mistaken for an empty one.

The fallback cuts at a fixed interval rather than holding the file, because the
memory ceiling is what makes long recordings workable at all and the fallback
must not spend it.

### The floor, and what it cost

Chosen by arming the guard at 0.995 to get every file's recovered transcript,
then composing any other floor from it — exact, because the decision is per
file. Verified afterwards: the composed prediction for 0.70 was 9 fired and 798
errors, and running it gave 9 and 798.

**0.70 is the default.** Nine files fire on the conversation set and **all nine
improve**, the worst going from 15 errors in 16 characters to 1; three become
exact. The mean falls 6.84% → 6.52%. Higher floors recover more — 0.97 fires 44
times for 0.56 points — but start damaging files that were already right, by a
character or two. The first such file sits at coverage 0.730, so 0.70 is the
highest round value with any margin.

**No false positives.** Of 5049 utterances across five datasets, the guard
recovers 9 and all 9 are in the conversation set: meeting, phone, documentary
and sports recovered **zero**. On a held-out sample of 200 healthy conversation
files, 199 were byte-identical and one improved.

### The four-hour case

Re-measured, because the guard's worst behaviour would be a full second pass
over a long recording. On a 4.01-hour file: coverage **100%**, so it does not
fire; 931 segments; 454 s wall (**31.8× realtime**); peak RSS **414 MB**, flat
from 409 MB once loaded to 412 MB at the end. The accumulator costs about
1.2 MB for four hours. Run twice, identical output.

### What it does not fix

**Coverage measures damage, not failure.** Of the eleven files §13 identified
as defeating the detector, only four have low coverage. The others kept 80–100%
of their audio and were mis-recognised anyway. The guard catches the losses,
which are the catastrophic ones, and misses the rest — recorded rather than
papered over.

### A wrong number, corrected

An analysis script divided normalised error counts by raw reference lengths,
punctuation included, and reported 5.95% where the truth is 6.84%. The figures
in §13 that came from it are corrected; the error was in the analysis, never in
`verse-bench`.

### One fewer copy of the pipeline

`verse-cli` did not depend on `verse-pipeline` — it carried its own decode →
segment → recognise, which `asr-evaluation.md` §1 had wrongly claimed was
shared. It now uses `Transcriber`, and the duplicate `recognize`, VAD setup and
decoder setup are gone. Verified by 30 files whose SRT text matches the
pipeline's output exactly, and by three whose output is unchanged from the old
binary.

The CLI gained the coverage line in exchange for the span count it used to
print — the span count is what cracked §13, but coverage is the better number
and the guard now reports itself when it fires.

`verse-asr` moved to the CLI's dev-dependencies: only the `transcribe` example
still reaches an engine directly, deliberately skipping the detector to
separate a recogniser that failed from one that was never given the audio.

## 2026-10-06 — The CLI learns to be driven by a program

Verse was usable by a person through the window and by a person through the
terminal, and not by an agent, for three reasons that reading the code made
concrete: one file per invocation, nothing machine-readable, and a single exit
code where `ErrorKind` distinguishes nine.

### The Qwen decision rested on a bug

The choice not to bundle Qwen3 is only sound if fetching it works. **It did
not.** `Downloader::fetch` created `models/<id>/` and nothing else, while three
of Qwen3's six entries name a subdirectory — `tokenizer/vocab.json` and
friends. On a machine where that directory did not already exist,
`verse model fetch qwen3-asr` would have pulled 982 MB and then failed on the
last three files. It never bit here because the tokenizer had been unpacked by
hand into this checkout.

Reproduced first, with a catalogue override pointing at the 2.3 MB VAD file so
the test cost seconds rather than a gigabyte: `os error 3`, nothing written.
Fixed by creating the file's parent before the transfer starts rather than
beside the write, so a layout that cannot be created fails immediately instead
of after the download.

Every configured mirror was also probed. **`silero-vad`'s hf-mirror entry is
dead** — 404 — and is harmless only because modelscope is listed first.

### Batch, and what it is worth

One invocation now takes files and directories, loads the model once, and keeps
going when a file fails. On twenty files: **7.3 s against 33.3 s**, because the
second way pays a 228 MB model load twenty times. A corrupt file among eight
leaves the other seven written and exits 3.

`--jobs N` is opt-in and on the evidence: **21 s and 347 MB at 1, 10 s and
1245 MB at 4** over sixty files. 2.1× for 3.6× the memory, sublinear because
the workers contend for the same cores, and kept because the memory is not much
on a machine this project already requires 8 GB of. Output verified identical
to the sequential run across all sixty files.

### Machine-readable, and honest about it

`--json` puts one document on stdout and nothing else there. **One shape for
one file and for many**, so a caller never branches on arity. Every field is
always present, `null` rather than absent. `coverage` and `recovered` travel
with each result, because a transcript that is short because the recording was
short and one that is short because audio was dropped are otherwise identical.

Exit codes now separate the failures that call for different actions. 4 means
"no model — run `verse model fetch`", which is the one an agent can act on.

### `hotwords`, and the intelligence claim

The user asked whether bundling Qwen would add intelligence. It would not —
Qwen3 is a bigger transcription model, not a reasoning one. What it *does* have
that SenseVoice does not is `hotwords`: a lexicon fed to the decoder, which
changes what is heard rather than how it is written.

Demonstrated rather than asserted. `000030`, reference `该本王子用那个球拍了。`:

```text
without --hotwords        本王不用那个酒吧啦。
with    --hotwords 球拍   本王不用那个球拍啦。
```

A second clip, 球拍's sibling: `滑得` corrected from `划的`. The delimiter is
undocumented in the binding, so it was determined by trying — comma, full-width
comma, space and newline all work — and the CLI passes the string through
unchanged rather than normalising a grammar it did not define.

`--hotwords` with an engine that cannot use it warns and records itself in the
report, with `hotwords` set to `null`. Not a hard error, because a batch across
mixed engines should not lose the run to an inapplicable flag; not silence
either, because that is the failure this project has now paid for three times.

### Documentation, and five things that were not true

`README.md`, `llms.txt` and `AGENTS.md`, none of which existed. Before writing
them, five stale passages were corrected — the worst being `design.md` §10
instructing a default engine that was removed, contradicting §5.3 of the same
document, and a task log still claiming Paraformer as the fallback.

Every flag named in the new documents was checked against `verse transcribe
--help`, which also revealed that `verse transcribe --help` was itself an
error. Fixed.

**On "increasing AI search ranking": that cannot be promised and nothing here
claims it.** What was built is legibility to an AI that has already found the
project. Whether a crawler ranks it well is not something this repository
decides.

## 2026-10-06 — The window's three dead ends

Four screens were deferred when the agent-facing CLI was built. This closes
three of them. The frontend was never the blocker: `App.svelte` already
rendered all five screens and three of them led nowhere — `NeedsModel` offered
only "返回", and `Done` had a transcript and no way to write it out.

Each hole was a missing **command**, which is why this is backend work.

### Export

`export(app, path)` renders the finished transcript and writes it. The window
chooses the path rather than the backend, because the dialog plugin's `save()`
returns one and doing it in Rust means a callback-shaped API threaded through a
command that otherwise returns immediately.

`dialog:allow-save` joined the capability file. Without it the window cannot
ask where to put anything — which is the whole reason the screen was stuck.

The format comes from the extension and an unrecognised one is refused rather
than guessed. `ScreenView::Done` now carries `exported`, so the window can stop
offering an export that has already happened; that is what `note_exported` was
written for and had no caller until now.

### The model screen

`Effect::FetchModel` had been produced by the state machine and handled nowhere
since it was written. It now has a handler that runs the download on its own
thread, moves the state machine *and* emits byte progress, and on `Ready`
starts the job that was waiting — what `NeedsModel { input }` has been carrying
the path for all along.

`import_model` accepts a folder the user already has, in either of the two
layouts people end up with after unpacking a download, and copies it into
`models/<id>/`. Copying rather than referencing, because the pipeline looks in
one place and a second lookup path is a second thing that can disagree with it.
A folder missing a file is refused with the file named — the same nested
`tokenizer/` shape whose absence silently broke the downloader last round.

**Progress does not go through the event bus**, and that is a deliberate
departure. `verse-core` has an unused `Event::ModelStateChanged` that looked
like the obvious channel, but its `ModelState` carries no byte counts, and
`verse-model`'s `DownloadState` cannot be named from `verse-core`. Routing
through the bus would mean two download vocabularies and a lossy round-trip
between them. The app layer owns the download — it is not pipeline output — and
`lib.rs` already emitted `Update::Cleared` directly, so this follows the shape
already there. A `ModelState` carrying progress was written and then reverted:
it had no user. **`Event::ModelStateChanged` remains unused**, which is now a
recorded fact rather than an oversight.

### About

`crates/verse-app/src/about.rs` holds the attribution the model licence
requires. A test asserts "SenseVoiceSmall" is spelled exactly as upstream
spells it, because the licence requires the name be kept and a tidied-up
rendering would not be it.

**The licence-text obligation cannot be discharged from here, and the dialog
says so.** The file beside the model on hf-mirror is 71 bytes reading "Ref to
https://github.com/modelscope/FunASR", and GitHub is unreachable from mainland
China — so neither this machine nor an installed copy can fetch the text. The
checklist item stays open in `THIRD_PARTY_NOTICES.md`.

### What is not verified

Every one of these screens ends in a click, and a click is the thing an agent
cannot make: a native save dialog, a progress bar filling, a folder being
chosen. The logic underneath each is unit-tested — 156 tests, clippy clean —
and all nine commands are registered, the frontend type-checks against them,
the bundle builds, and the window opens with an empty log. **But "the button
works" is a claim that needs a person at the window**, and step 4 of
`tasks/p1b-screens.md` is left open for exactly that.

## 2026-10-06 — The licence texts, now that GitHub answers

`THIRD_PARTY_NOTICES.md` has carried a release checklist since it was written.
The previous round closed the attribution item in the interface and left the
rest, for a stated reason: the licence file beside the model on hf-mirror is 71
bytes reading "Ref to https://github.com/modelscope/FunASR", and it points at a
host that does not answer from mainland China.

A proxy is up and `github.com` answers in 0.87 s. The texts were fetched, and
the blocker is gone rather than worked around.

### What was archived

Three texts, not one, because two more were sitting in the notices file marked
"not yet verified" and can now stop being:

| file | upstream | SHA-256 (first 16) |
|---|---|---|
| `FunASR-Model-License-1.1.txt` | `modelscope/FunASR` `MODEL_LICENSE` | `7dba975a2069691d` |
| `Silero-VAD-MIT.txt` | `snakers4/silero-vad` `LICENSE` | `2e63e9a38b6e8fc0` |
| `sherpa-onnx-Apache-2.0.txt` | `k2-fsa/sherpa-onnx` `LICENSE` | `cfc7749b96f63bd3` |

Copied rather than linked, for a reason the agreement itself gives. §6: "This
agreement may be updated and revised occasionally... and will take effect
automatically." A link records nothing — it shows whatever the agreement says
next time someone follows it. The copy with its hash is the evidence of the
terms accepted. `licences/README.md` carries the upstream commit, the fetch
date and the full hash for each.

**Two traps in the fetching, both recorded in the file.** The FunASR repository
serves two licences and only one is ours: `LICENSE` at the root is MIT and
covers the *code*, `MODEL_LICENSE` is the custom agreement and covers the
*weights*. We use the weights. Fetching the obvious URL would have archived the
wrong document. And Silero VAD's README renders a badge labelled "CC BY-NC 4.0"
that links to an MIT `LICENSE` — the file is MIT and the README says so in
prose ("Published under permissive license (MIT) Silero VAD has zero strings
attached"), and the badge is stale. Anyone re-verifying this should expect the
badge to look alarming and should not act on it without reading the file.

### "Ship the licence" now means the installer

`tauri.conf.json` declared `bundle.targets` and no `bundle.resources`. A licence
file in the repository is shipped to *us*; nothing beyond the binary reached the
`.msi`. All four files are now listed by name — not by glob, because a glob that
silently matches nothing is the failure this project keeps having — and the
build puts them in `$RESOURCES/licences/`.

### Checklist, corrected rather than ticked

The attribution and name-retention items close. So does Silero. The licence-text
item was **split in two**, and this is the part worth keeping: *the text ships*
is done; *the text corresponds to the model revision used* is not, because
`models.json` fetches every file through `.../resolve/main/`. One done thing and
one undone thing were wearing the same line.

Revision pinning is left open with its reason written down: it needs a revision
per mirror — HuggingFace takes a commit SHA in `resolve/<sha>/`, ModelScope's
revision semantics are its own — and a downloader that can express one.

The 关于 dialog no longer says the text is 尚未内置. The test that asserted that
sentence was changed with it, which is the point: a test pinning a stale claim
pins the wrong thing.

## 2026-10-06 — A correction to a recorded measurement

The agent-CLI round recorded that `silero-vad`'s hf-mirror entry was "dead —
404". With the proxy up, every mirror in `models.json` was re-probed. The entry
is dead, but the code was wrong, and so was the reason.

**The old probe used the wrong filename.** It requested
`.../resolve/main/silero_vad.onnx` — the *local* name the catalogue writes the
file to — where the catalogue's `remote` path is `model.onnx`. A 404 for a file
that was never going to be there.

**The real answer is 401, on the mirror and upstream alike.**
`csukuangfj/sherpa-onnx-vad-silero-v5-2023-12-25` answers 401 for both
filenames from `hf-mirror.com` and from `huggingface.co`, which is a repo that
no longer resolves rather than a mirror that is down. The rest of the list is
healthy: 11 of 12 URLs return 206, and the control URL — a sense-voice file on
the same mirror — returns 206 as well, so the instrument was validated before
its readings were believed.

Still not fixed, and still harmless only because modelscope is listed first for
that model. The conclusion stands; what changed is the evidence under it, and
the earlier entry above is left as it was written.

## 2026-10-06 — The installer that had never been built

Wiring `bundle.resources` so the licence texts reach the installer turned up
something larger behind it: **this project had never produced an installer.**

The first `tauri build` failed with `Couldn't find a .ico icon`. The icon set
was already committed — `crates/verse-app/icons/` holds a complete Tauri set
including `icon.ico` — but `bundle.icon` was never added to `tauri.conf.json`,
so nothing referenced it. Present at HEAD, unused since it was added.

With the icon wired up, both bundles build: `Verse_0.1.0_x64_en-US.msi` at
10.55 MiB and `Verse_0.1.0_x64-setup.exe` at 7.43 MiB.

**Verified by reading what the bundler wrote, not by trusting the config.** The
generated `target/release/wix/x64/main.wxs` carries a `licences` directory with
all four files and their real source paths; `target/release/nsis/x64/
installer.nsi` agrees and deletes them again on uninstall. The staged copies
hash-match the archived originals, so what reaches a user is what was fetched.

What is *not* proven: that the installer runs. It was built, not installed.

**One process note, recorded because it cost a cycle.** The first build was
launched from `crates/verse-app/ui`, where the CLI cannot find
`tauri.conf.json`, and it panicked — but the command ended `| tail`, so the
shell reported **exit 0** and the failure stayed invisible until the log was
read. A non-zero exit is not the only way a command can fail to do its job;
piping to a formatter discards the status that would have said so.

## 2026-10-06 — Where results go, and not doing the same work twice

An audit of the product's own main line — "drop any audio in, get text out" —
found two of its four parts missing. Accepting any audio is done, and does it
without a temp file: ffmpeg streams PCM to stdout. Automatic processing is
done, except that a missing model stops and waits instead of fetching itself.
But **the result had nowhere of its own to go** — the CLI wrote beside the
input, the window had no default at all and destroyed an unexported transcript
the moment the next file arrived — and **caching and deletion did not exist**.
Not "were incomplete": `缓存`, `临时文件` and `cleanup` appeared nowhere in
`design.md` or `ui-design.md`, and production code contained no `remove_file`
call of any kind.

This round designs that, having first established the constraint that shapes
it.

### The constraint, verified before designing

A cache hit must republish **every segment**, not just the final transcript.
The window builds its segment list from `Event::TranscriptSegment`
(`state.rs:375` → `bridge.rs:273` → `Update::Segment` → `App.svelte`), and
`TranscriptFinal` (`state.rs:391`) only finalises. A hit publishing only the
final transcript would render the Done screen as "没有识别到内容" while
reporting success — an empty result that looks like a finished job.

That is why the hook goes inside `Transcriber::transcribe`, which the CLI, the
window and the harness all pass through, rather than into each front-end.

### Decisions

A new leaf crate, `verse-store`, owns "where data lives": per-user directories,
the result cache, resume checkpoints, and output naming. It depends on no
workspace crate, so the command line can resolve a path without linking the
engine, and the cache's wire types live with the wire as `report.rs` and
`bridge.rs` already do.

The output directory resolves `VERSE_OUTPUT` → the shell's Documents folder →
`home/Documents` → local app data. OneDrive redirection is followed but
**detected and reported**, because the program makes no network call while the
user's sync client will happily upload a transcript written into a synced
folder.

A **decoded-PCM cache was cut.** The result cache already answers the common
case — a re-run does not reach the decoder at all — so PCM on disk only earns
its keep when the same audio goes through a *different* engine, which is
evaluation work and is what `verse-bench` is for. It would cost about 1.4 GB
per two hours of audio with its own eviction problem. Recorded in `design.md`
§4.1 so it is not re-proposed as a new idea.

Noted while editing §4.1: its crate table was missing `verse-bench`
altogether. Added.

The work is staged in `tasks/storage-cache-output.md`. Step 6 is a **breaking
change** — the CLI's default output directory moves — and the published
contracts (`llms.txt`, `README.md`, the usage text, the existing tests) move in
the same commit rather than after it.

## 2026-10-06 — The main line: where results go, and not doing work twice

The round this file has been building towards since the audit at the top of the
day. Four things were asked for and one was cut; the plan is
`tasks/storage-cache-output.md` and every step is done.

**A result cache.** A new leaf crate, `verse-store`, keys a finished
transcription by the audio it came from and the settings that produced it. A
re-run is instant. Measured: `elapsedMs` 307 → 0, output byte-identical, and a
one-byte change to the file with the length left alone correctly misses.

The hook is inside `Transcriber::transcribe`, which the command line, the
window and the harness all pass through. That placement is the design: a hit
republishes **every** segment through the same events a run would, because the
window builds its list from `TranscriptSegment` and `TranscriptFinal` only
finalises. A hit publishing the final transcript alone would have shown an
empty result and called it success.

**Resume.** A long file interrupted at 11 of 27 spans resumed in 7.6 s against
9.5 s; at 20 of 27, 4.6 s. Both outputs identical. Only the recognition is
skipped — the decode and the segmentation are redone, because the detector's
state is not exposed — and a span is identified by its own samples, so the
failure mode is redoing work rather than inventing it.

**Model and cache cleanup.** `verse model list` reports two sizes, one of them
"how much of this is a transfer that never landed". `remove` and `clean` and
`cache size` and `cache clean` come with it. Nothing runs on its own: a model
directory is the largest thing this program puts on a disk, and deleting one on
a heuristic is a destructive act taken on someone's behalf.

**Automatic saving.** Transcripts now go to a `Verse` folder in the user's
Documents on both interfaces. In the window it happens the moment the result
exists, so dropping a two-hour recording and then another one no longer
destroys the first. The CLI's default changed with it — a breaking change, and
`llms.txt`, `README.md` and the usage text moved in the same commit rather than
after it.

**A decoded-PCM cache was cut**, with the reasoning in `design.md` §4.1 so it
is not re-proposed as a new idea: the result cache already answers the common
case, and PCM on disk only pays when the same audio goes through a different
engine, which is evaluation work and is what `verse-bench` is for.

### What running it found that reading it did not

Five bugs, none visible to `cargo test`, clippy, or a careful reading of the
diff. They are the argument for the rule this project already had.

1. **`recognize` collected the segments it produced and never stored them.**
   The checkpoint call was simply missing, so every log stayed empty and resume
   did nothing while the code read as though it worked. Clippy had nothing to
   say: a `Vec` that is only pushed to counts as used.

2. **The ownership record was loaded, used and never saved**, so every run
   looked like a first run and the output directory filled with `zh.srt`,
   `zh (2).srt`, `zh (3).srt`.

3. **Pruning the record before saving erased the claims just made**, because
   none of those transcripts had been written yet. Pruning now happens on the
   way in, never on the way out.

4. **`is_free` asked the filesystem whether a name was taken**, and during
   planning nothing is on disk yet — so two inputs in one batch were both
   handed `会议.srt`. The record is consulted first now.

5. **`rename_all` on an enum renames its variants, not their fields.** The new
   `saveError` arrived as `save_error`; checking the neighbours found the same
   mistake in `DownloadView::Fetching`, which has been there since the download
   screen was built two rounds ago — the progress bar has been reading a field
   that was never sent and showing **"NaN MB"** at zero per cent ever since.
   Neither side complained because nothing connects them but a comment.

Each now has a test, and each of those tests was checked to bite by putting the
bug back.

### What is still not verified

The window. Every path in `autosave` is unit-tested, the wire format is pinned,
the frontend type-checks and builds — but "the transcript appeared in Documents
and the error shows when it does not" needs a person at the window. That is
step 4 of `p1b-screens.md`, and it is now also the last thing standing between
this round and being finished rather than merely correct.

The models directory is the other loose end: `verse-store` now resolves a
per-user data location and the cache, the checkpoints and the output record all
use it, but the weights still resolve beside the executable. Recorded in
`ui-design.md` §11 as a smaller and more concrete job than it was this morning.

## 2026-10-07 — A process that owns a model

Asked whether the backend was "a service" that could control a model's start and
stop, and whether it could be given to other applications. It was not, and it
could not: the backend is a library plus two in-process consumers, and **nothing
in the tree held a model across jobs**.

That last part was worse than an absence. The window loaded a 228 MB model
*inside* every per-job worker thread, and the thread ended with the job —
while `Transcriber`'s own doc comment says the opposite is the entire reason it
is a struct rather than a function.

### Measured first

| 1-second clip | |
|---|---|
| recognition | 181 ms |
| whole run | **1641 ms** |

89% of the run was the load. Then, over five files:

| | per file | loads | total |
|---|---|---|---|
| load per file — what the window did | 1549, 1501, 1711, 1512, 1547 ms | 5 | 7820 ms |
| one keeper — what it does now | 1564, **284, 306, 276, 309** ms | 1 | **2739 ms** |

The shape matters more than the total: loading per file is flat at 1.56 s
however many files there are; a keeper is `1564 + (N−1) × 290`.

### `ModelKeeper`

A module in `verse-pipeline`, not a new crate — everything it needs is already
there, and a crate would have added a manifest, a member and a dependency edge
for no boundary. Three controls, which are the three things anyone asks of a
long-lived resource: `preload` starts it, `release` stops it, `status` says what
it is doing. Between those it releases itself once idle.

**Identity is the settings digest**, and the reason is correctness rather than
economy. `Transcriber::run` reads the VAD and guard settings and the VAD model
path out of the request it stored when it was loaded, so a narrower "same
engine" test would reuse across a change in those and run the *old* settings —
a wrong answer, not a slow one. The cost is that the digest over-covers, so a
changed detector threshold costs a reload; the window cannot hit that, and a
service would pay one reload per change.

**The model mutex is held for the whole job.** Forced rather than chosen —
`Transcriber` is `Send` and not `Sync` — and it gives the honest semantics of one
model doing one job. `release` therefore means "after the current job"; a caller
who wants to stop now has the job's `CancelToken`. `status` reads an atomic and
never the lock, because the only time anyone asks is while a job is running.

### A dead end this round found and closed

The window's own load failure was being discarded. `is_present` checks size
only, so a model present at the expected size but unusable gave `ready = true`;
the worker's `Transcriber::load` then failed and published `JobFailed` — with a
job id **nobody had claimed**, because an id is claimed by `JobStarted` and
`JobStarted` is published by `transcribe`, which is never reached. The screen's
ownership guard discarded it, and the window sat on "正在准备…" for good, with a
cancel button for a job that was never running.

The comment at `lib.rs:128-130` even says a broken catalogue is fine because
*"the pipeline will say so"*. It could not. The keeper announces `JobStarted`
before `JobFailed`, and the dead end is gone.

**A correction worth recording.** The plan claimed a particular test "fails
today". It did not — it drove the fixed sequence and would pass either way, and
the existing test could not catch the bug because its helper injects
`JobStarted` itself. What pins it is a pair: the keeper's test that the sequence
is `[JobStarted, JobFailed]`, and a new state test that a bare failure *is*
discarded. The second asserts the guard's real behaviour and was checked to
bite. Neither claims more than it shows.

### No event, and no service

`Event::ModelStateChanged` has been defined and unused since the first phase.
Adding a second event that nothing subscribes to would repeat that; reusing the
first would make `Ready` mean both "on disk" and "resident". `status()` is a
query, and it is the accessor a service would call.

**The loopback service is not built here.** The maintainer asked for model reuse
first, deliberately, and this is that. `ModelKeeper::status()` and
`with_timeout` are the two seams such a thing needs; they exist and are used.

The command line and the benchmark still load for themselves. They already do
the right thing, and the CLI's `--jobs N` loads one model per worker on purpose
— one keeper's mutex would serialise them back into `-j 1`.

### Still unverified

The window. Every path in the keeper is unit-tested, the wire is unchanged, and
the measurement drives the same type the window holds — but "the second file no
longer reloads" needs a person at the window.

## 2026-10-07 — `verse serve`, and what a corrupt model does

Asked whether the backend could be a component other applications use. It could
not: a library plus two in-process consumers. The previous round built the
prerequisite, `ModelKeeper`; this one builds the transport.

**A subcommand of `verse`, for a structural reason.** `verse-cli` has no `[lib]`
target, so `report.rs` — the pinned result wire and the contract tests that pin
it — is unreachable from another crate. A separate binary would describe a
transcript a third time. This way a job's result **is** the `FileResult` that
`verse transcribe --json` emits, and `GET /models` is the `ModelList` that
`verse model list --json` emits. One vocabulary, one parser.

**The offline guarantee, amended rather than broken.** §4.5 said all network
access lives in `verse-model`, and `verse-model/src/lib.rs` said it is the only
crate permitted to perform network I/O. The distinction that keeps it intact:
**an outbound connection is a request to a server; an inbound one is a request
from a local peer.** The first is what "offline" forbids. A listener accepts and
initiates nothing, adds no HTTP client, and is bound only while someone runs
`serve`. The socket and every document restating the rule moved in one commit,
and a grep confirms no restatement was missed.

**Hand-rolled HTTP over `std::net` and `httparse`**, which was already in the
graph through Tauri. Chunked encoding is refused rather than mis-parsed, bodies
are capped at 1 MiB, connections at 64. No async anywhere, matching the rest of
the tree.

**Job-based**: submit, poll, cancel. A one-hour file takes minutes, so a
synchronous call could show no progress and could not be stopped. One job runs
at a time and the rest queue — a keeper is one model, and a pool is deferred with
its cost named rather than taken by default.

**Nothing grows with uptime**, which §3 makes a hard constraint for a
long-running process and this is the first of. Measured over 45 jobs: `retained`
pinned at 32 while uptime grew.

### The finding that contradicts the plan

The plan said a model present at the expected size but malformed would land as
a failed job, showing the size-only check not failing silently. It does not. It
**takes the process down**.

Planted a decoy catalogue declaring sizes two files of garbage actually have, so
`is_present` passes. `health` says present, the submit is accepted, sherpa-onnx
prints `ReadTokens: Error: not tokens`, and the server is gone. The library
terminates the process rather than returning an error — the command line exits
127 on the same input, not one of its documented codes. There is no `Err` for
`ModelKeeper` to announce, because there is no error to return.

**Not new** — the command line has always had it. What is new is that the
process is now a service other programs depend on, so the blast radius is a
running server rather than a finished command. Not fixable under this project's
rules: catching a C library's exit needs signal handling or `unsafe`, and
running recognition in a subprocess would kill the keeper, which exists to hold
the model in process. The common case — no model downloaded — is refused cleanly
at submit.

### Three languages, one wire

The claim "usable from any language" is checked rather than asserted. The same
job, from three:

- **Python**, standard library only, `tools/verse-serve-client.py`;
- **PowerShell**, `Invoke-RestMethod`, comparing the transcript by codepoint so
  the console's encoding is not what is being tested — `text matches Python's:
  True`;
- **Rust**, an integration test spawning the real binary and driving a real
  socket.

All three return `开放时间早上9点至下午5点。` with one segment.

### What running it found

Four bugs, none visible to `cargo test`, clippy, or a careful reading.

1. `submittedAtMs` was measured from the job's own start and `finishedAtMs` from
   the worker's, so a job reported `submittedAtMs: 1604` and `finishedAtMs: 5`.
   Three timestamps from three origins are not timestamps.
2. My own documentation claimed `elapsedMs` was "recognition time only,
   comparable with the CLI's report". It is — for every job except the one that
   pays the model load: 1558 ms against 272 ms and 242 ms for the same clip.
3. `serve --port 0` reported "unknown option 0", because the subcommand's
   argument loop was missing the step past a value that every other parser here
   ends with.
4. Two tests of mine were racy, asserting on the queue length without first
   pinning the worker.

And one correction to a *test*: it opened 71 sockets expecting the connection cap
to refuse some, and got none — TCP accepts a connection long before the server
sees it, so the cap is only observable by sending a request. The refusal is a
503 response, not a refused handshake.

## 2026-10-07 — A pool of workers, sized from the machine and the model

`verse serve` ran one worker, so four concurrent requests serialised. It now
runs a pool whose size comes from a rule rather than a guess.

The command line's `--jobs` deliberately defaults to 1, and its log gives the
reason: *"the memory cost is real and invisible, so it is opted into rather than
paid by everyone."* The service answers that objection by making the cost
visible — `/health` reports the pool, its per-worker cost, the budget, and that
the budget is an assumption — rather than by defaulting to 1.

```
per_worker_bytes = model_bytes + RUNTIME_OVERHEAD_BYTES
pool = min(MEMORY_BUDGET_BYTES / per_worker_bytes, engine_threads)
         .clamp(1, MAX_WORKERS)
```

**The budget is 2 GiB and it says what it is.** Reading the machine's RAM needs
FFI and `unsafe` on every platform this project targets, and the project has
neither, so the ceiling is a written assumption anchored to the 8 GB floor §3
commits to — and it is reported to clients so they can disagree with it rather
than merely obey it.

**One thread budget for the whole pool**, which is an invariant rather than
tidiness: the settings digest includes the resolved thread count, so unequal
budgets would give workers different model identities and a job would be
runnable on only some of them. It is the same `per_worker_threads` the command
line's `--jobs` now calls, so the batch and the service cannot disagree.

### The curve, and the two numbers that disagree

24 jobs of one clip, SenseVoice, cache off, warming every worker before the
measured batch so nothing has to be dropped afterwards:

| N | total wall | median per job | peak RSS |
|---|---|---|---|
| 1 | 6.03 s | 249.0 ms | **361 MB** |
| 2 | 3.51 s | 287.0 ms | 686 MB |
| 4 | 2.43 s | 375.0 ms | 1324 MB |
| 8 | 2.02 s | 589.5 ms | 2619 MB |

**All four instrument checks passed before the curve was read.** `loads` equals
N at every size. N=1 run twice gave 2.98 s and 3.01 s, agreeing within 1%, so
the machine is quiet enough to read a curve on. N=1 held **361 MB** against the
**347 MB** this project already recorded for the command line — within 4%, which
is what anchors the memory sampler to a known-good reading. And N=1's median of
249 ms lands inside the 240–310 ms band the service was already known to
produce.

**Throughput keeps improving; latency keeps getting worse.** Six workers finish
three times as many jobs per second as one, while a single job takes a little
under twice as long, because each worker holds fewer threads as the pool grows.
Neither number alone describes the trade. The default of six is the pool the
budget and the cores allow; `--workers` overrides it.

`RUNTIME_OVERHEAD_BYTES` is set from that table rather than from an estimate:
marginal cost per worker is (2619 − 361) / 7 = 322 MB against 239.5 MB of
weights, so the overhead is about 82 MB. The rule's own per-worker figure is
323.4 MB, landing on the measurement.

Qwen3-ASR gets **two** workers where SenseVoice gets six. That is the rule
working, not a shortfall.

### Two mistakes worth recording

**A test of mine was near-tautological.** It asserted that `digest(None)` equals
`digest(Some(digest(None)))`, which holds for almost any resolution rule — and
it passed when I deliberately broke `effective_threads` to resolve `Some(n)` as
`n + 1`. The rewritten test asserts what actually decides the cache key: that
the two spellings resolve to the same number. It fails under that falsification
now, with `left: 16, right: 15`.

**A constant I had guessed was wrong by a third.** The plan's arithmetic put the
per-worker cost at 306.6 MB and predicted six workers from a 64 MiB overhead.
The measurement said 322 MB marginal, so the overhead is ~82 MB and the constant
is now 80 MiB. The prediction and the measurement agreed on six only by
coincidence; they agree by construction now.

## 2026-10-07 — The pool's budget becomes a measurement

The maintainer asked whether the memory budget could be read from the machine
rather than assumed. It could, and the assumption was the worse of the two.

The budget was a constant — a quarter of the 8 GB floor §3 requires — which gave
a machine with 31 GiB the same pool as one with 8. Worse, it came out roughly the
size of a **single Qwen3 worker**, so the budget was most binding on the model
that benefits most from a pool. On this machine: **SenseVoice 6 → 8, Qwen3-ASR
2 → 7**.

**I had claimed a real reading was impossible, and that was an assertion rather
than a finding.** Checked: memory needs FFI on Windows and macOS, and this
project forbids `unsafe` in its own code. Linux would be free through
`/proc/meminfo` — which would have made the pool's behaviour differ by platform
for a reason no user could see. So it was a dependency or no probe.

`sysinfo` supplies it now, the same arrangement as sherpa-onnx doing the FFI for
recognition. `verse-core` keeps its empty dependency list, because the sizing
*rule* takes bytes and the service reads them. Three packages joined the lock —
`sysinfo`, `ntapi`, and `objc2-io-kit`, which is macOS-only and not compiled
here; `windows` and `libc` were already present.

**The first probe returned zero.** `System::new()` uses `RefreshKind::nothing()`
and leaves the memory total unset; it needs a `refresh_memory()`. Zero is
indistinguishable from a machine with no memory, and dividing it would have
collapsed every pool to one worker — silently. The test caught it, and was
checked to bite by removing the refresh again.

**Total, not available.** Total is a property of the machine; available is a
property of this moment, and sizing a long-lived pool from what happened to be
running at startup would shrink it because somebody else was compiling, with
nothing visible to explain why.

When the probe cannot answer, the old constant is the fallback and `/health`
reports which of the two was used, so a client can tell a measurement from a
document.

**Two corrections to my own earlier statements.** I told the maintainer the probe
would buy "2.5× more workers"; that arithmetic forgot `MAX_WORKERS` and the real
gap is 6→8 and 2→7. And the note read "33 GB" where Windows shows the same
machine as "31.2 GB" — both right, one decimal one binary, and it looked like a
disagreement. It says GiB now, which is what a person sees in Settings.

## 2026-10-07 — The notice nobody could see

Asking whether the frontend was the next thing to build turned up a feature that
had been reporting success at every layer while reaching nobody. **The
reduced-mode notice** — the one line that tells a person their machine is being
worked around — was produced, wrapped, and named by a command, and rendered
nowhere.

What was there: `Tier::Reduced` with its reason; `verse` printing the notice to
stderr (`main.rs:456`); the `hardware` command returning it
(`verse-app/src/lib.rs:80`); and a typed `probeHardware()` in `api.ts` with **no
caller**. `App.svelte` never asked and never drew it. A machine without AVX2 got
a window that looked entirely normal and was simply slower, with nothing saying
why.

**Two dead paths found at the same time, and left in place.** The convention here
is to report dead code rather than remove it, and neither of these is mine to
delete, so both are recorded instead:

- `Event::HardwareProbed` **has no publisher**. It is defined, handled at
  `state.rs:368` and tested at `state.rs:928`, but the only `publish` in the
  workspace is inside a test of the bus itself. Nothing in production emits it.
- `AppState::hardware_notice` **could not reach the wire even if it did** —
  `bridge.rs` never reads the field and `Update` has no variant for it. The
  handler returns `Applied::Screen`, so the state machine believes it has told
  the user, and the `Update::Screen` it produces carries the unchanged screen.

**Not a one-line change, because of language.** `ui-design.md` §8 keeps the
window's copy in Chinese and the command line's in English, and `Tier::notice()`
is English — it is what `verse` writes to stderr, and it has to stay that way.
The window cannot reuse it and should not translate it: the only structured
handle on *which* problem a machine has was a prose string, and matching on prose
to choose copy is how two renderings of one fact drift apart.

So `Tier::Reduced { reason: &'static str }` became
`Tier::Reduced { weakness: Weakness }`, with `NoAvx2` and `SingleCore`
exhaustively matched. `reason()` and `notice()` derive their English from it
unchanged; the window derives Chinese from the same value. Both renderings now
read one fact instead of each parsing the other's sentence.

`hardware_summary()` was split out of the command so the reduced case can be
tested against a machine that is not this one — the `degraded` arm is `None`
here, and a test that only ever sees `None` would pass while the notice was
broken. **Checked to bite:** putting `tier().notice()` back makes
`a_reduced_machine_gets_a_sentence_the_window_can_show` fail, which it does.

The bar is hand-written rather than the vendored `Alert` that `ui-design.md` §5
names for it: that table admits a component when **two** screens need it, and
only this one does. §3 asks for a full-width one-line bar, and an `Alert` is a
rounded box inside padding.

**Also corrected:** `MEMORY_BUDGET_BYTES` still described itself as *"a written
assumption, not a probe"* and said reading memory *"needs FFI and `unsafe`, which
this project does not have"* — the opposite of what the previous round made true.
A comment that contradicts its code is how the next reader builds on a false
premise; it was mine, from the round that added the probe.

**What is still not verified.** The bar has never been on screen. This machine
has AVX2, so `degraded` is `None` here; seeing it would take a machine without
AVX2 or a forced profile written only for a screenshot. The data path is pinned
to the JSON boundary by a contract test — field names and all — and the
`{#if notice}` that draws it is unexercised. It joins the click-through in
`tasks/p1b-screens.md` step 4 as something that needs a person at the window.

334 tests, clippy clean.

Noticed and not touched: `ui-design.md` §8 still says static labels "live in the
`.slint` files", which the toolkit has not been since the window moved to Tauri
and Svelte.

## 2026-10-07 — Removing the notice's second, dead implementation

The reduced-mode notice had two implementations: the one the window now uses,
and an older one built on `Event::HardwareProbed` that could not work. Both are
gone, at the maintainer's instruction.

Left in place, the dead copy was a trap rather than merely clutter. It had a
handler (`state.rs`), two passing tests, and no publisher — so the next person
to notice the event was never emitted and add a publisher would have got a
second silent failure out of the same mistake, because `bridge.rs` never read
the field and `Update` had no variant to carry it.

| removed | where |
|---|---|
| `Event::HardwareProbed` | `verse-core/src/event.rs` — the variant, the `job()` arm, and the `HardwareProfile` import it was the last user of |
| its only `publish` | `verse-core/src/lib.rs` — replaced by `ModelStateChanged` |
| the handler and its state | `verse-app/src/state.rs` — the arm, `hardware_notice`, `hardware_notice()`, `dismiss_hardware_notice()`, and two tests |

**`Event::ModelStateChanged` was left alone on purpose.** It is equally
unpublishable, but it is *documented* as considered and reverted
(`tasks/p1b-screens.md` §2), which makes it a decision someone took rather than a
path someone abandoned — and it now carries the case the bus test exists for: an
event belonging to no job reaching `subscribe_all()` and not a job-filtered
subscriber. Deleting it would have quietly weakened that test to two events that
both belong to jobs.

**The deletion forced one change of its own.** `apply`'s first `match` had two
arms and one of them was this; removing it left a single-pattern match, which
clippy flags, so it is an `if let` now. Its placement ahead of the `owns` check
is load-bearing and was already so: `owns` compares `event.job()` against the id
this arm is what sets, so running it first would reject the event that would
have claimed the job — and every event after it.

Also corrected: `ui-design.md` §8 said static labels live in "the `.slint`
files", a toolkit this project has not used since the window moved to Tauri and
Svelte, and credited the hardware notice to `state.rs`, which is no longer where
it comes from. `design.md` §9 and `tasks/agent-cli.md` record the same class of
correction being made before.

332 tests — two fewer than before, and they are precisely the two that asserted
a field nothing could set. Clippy clean.

## 2026-10-07 — Five components no screen ever used

`ui-design.md` §5 claimed eight shadcn-svelte components were taken, "each
earning its place by being needed on two or more screens". Measured, three are
imported by anything at all — `Button`, `Dialog`, `Progress`. `Card`,
`ScrollArea`, `Separator`, `Alert` and `Sonner` are referenced nowhere: not in
`App.svelte`, not in the CSS, not in `components.json`, the Vite config or
`index.html`.

**The obvious defence of them does not survive checking.** §5 lists each against
screens by name, which made the claim testable: `Card` for "empty, model,
working, done", `ScrollArea` for "transcript", `Alert` for the failed screen and
the hardware notice, `Sonner` for export confirmation. **Every one of those
screens exists**, and every one was built without the component named for it —
the transcript scrolls in a hand-written element with auto-follow, and `Done`
reports a failed write in its footer. They are not scaffolding for work still to
come. They are what a screen leaves behind when it is built a different way.

Both costs are measured, not reasoned:

- **Every build paid for them.** Tailwind scans component sources for class
  names, so the stylesheet is 31.55 kB with them gone and was 37.23 kB with
  them in the tree — 5.7 kB of rules for classes nothing used.
- **`Sonner` was the only reason two npm dependencies existed.** It imported
  `svelte-sonner` and `mode-watcher`, and nothing else referenced either.

Twenty files and 450 lines of vendored source removed, plus those two packages.

**A trap worth recording, because it nearly produced a wrong answer.** The first
check for orphaned dependencies grepped the *current* source and reported
`mode-watcher` as unreferenced — which read as pre-existing dead weight rather
than something this change had caused. It was caused by this change:
`sonner.svelte` had imported it, and that file was already deleted by the time
the grep ran. Checking the deleted files (`git show HEAD:<path>`) is what gives
the right answer; checking what is left cannot.

`@internationalized/date` is also unreferenced and was **left alone** — no
deleted file imported it, so it is not this change's orphan, and it is a common
sibling of `bits-ui`, which is still used.

**One measurement I chased down instead of assuming.** `svelte-check` reported
657 files before the deletion and 597 after — 60 for 20 files, which is the kind
of tidy ratio that invites a wrong explanation. Restoring one two-file component
moved the count by 42, so the number is not a file count at all: it is the whole
type-checking program, packages included, and `sonner.svelte` was dragging
`svelte-sonner` into it.

§5 now lists the three in use and says plainly what the other five were. The
admission rule it was written with — two or more screens — is unchanged, and is
what the three survivors satisfy.

Rust untouched; 332 tests and clippy clean, `svelte-check` 0 errors.

## 2026-10-07 — One page: the model, the files, a real percentage, an extractive summary

The window was five screens that replaced one another. It is now one page with
a sidebar — which engine is running, and every file this session has been given
— and a detail region that shows whichever file is selected. Four decisions
recorded in the design had to be amended rather than quietly broken; each is
marked in place with what changed and why.

### The progress bar that "could not be built", and the measurement that says otherwise

`tasks/p1b-gui.md` step 9 recorded that there would be no progress bar, for a
stated reason: *"The decoder does not report a total length, and getting one
means a probe pass over the file before decoding it."*

The second half is a sound objection to a **probe pass** and does not apply
here. ffmpeg prints `Duration:` in the banner of the decode that is already
running — at `info` level, which `-loglevel error` was suppressing. Measured:

| how ffmpeg is called | stderr |
|---|---|
| as the pipeline called it (`-loglevel error`) | **0 bytes** |
| `-nostats -loglevel info` | **808 bytes**, incl. `Duration: 00:00:05.59` |

So the denominator was free the whole time. `verse-audio` now parses it on the
drain thread that was already reading stderr; `Event::JobProgress.fraction`
became `Option<f32>` because a stream can declare `Duration: N/A`, and the
window draws a bar with no number in it rather than an invented one. **The bar
sits beside the growing transcript, never instead of it** — P5 says progress is
visible as output rather than as motion, and a bare bar would be a regression
against it.

`-loglevel info` also put the whole input banner into the failure message,
where it would have buried the complaint; the lines that describe the input are
now filtered out, which is tested.

**Verified end to end on a real job**, through `verse serve`: the fraction went
**0.0716 → 1.0** on a real file with the real model.

### The state model: kept, not replaced

The obvious move was to replace `Screen` with a per-file record and rewrite the
state machine. It was not taken, because the ~28 tests in `state.rs` are not
tests of "there is one file" — they are tests of the one-file transition graph,
and they encode things the project learned the hard way ("a failure for a job
nobody announced is ignored", "output from a job that is over is ignored"). A
replacement re-homes every one of those assertions and risks dropping a
regression pin.

So `Screen` keeps its name and its variants, and becomes **per file**:
`FileEntry { input, screen }`, a roster beside it, and `screen()` keeps
returning `&Screen` — the selected entry's. The result is that the existing
assertions did not change at all, and the six new tests are the roster's own.

`Screen::Queued { input }` was added and is not cosmetic: a queued file with a
`Working` screen would let 取消 on the *waiting* file cancel the *running* one.

### A race that the design would have shipped

Dropping a folder is one gesture that arrives as many paths, all within a
millisecond. `file_chosen` decided whether to start or to queue by asking
whether a job was running — but a job's id is only known when `JobStarted`
arrives, asynchronously. The second path therefore saw "no job" and started
one, **cancelling the first**. The guard now keys on `running`, which is set
the instant a file is accepted, and `several_files_dropped_at_once_...` pins
it.

### An extractive summarizer, labelled as one

`sherpa-onnx` has no text generation at all, and `qwen3-asr` is an ASR decoder
conditioned on audio — its config struct has no prompt field and its API is
`decode`/`get_result`. Making those weights summarize means retraining them, so
a small local LLM is a separate project, not a setting.

What shipped instead is `verse-core::summary`: character-bigram term frequency,
top sentences restored to source order. It is **extractive and said to be** —
"摘录式摘要", with a line under the heading saying the sentences are quoted
from the transcript in their original order and were not rewritten. The test
that matters is `every_sentence_is_verbatim_from_the_transcript`, and it was
falsified by appending one character to the output, which failed it.

### The instrument, twice

**A stale binary produced a false negative.** The check that a release build
cannot publish a demo job was first run while the release build was still
linking, so it read the *previous* binary — which predated the demo module
entirely — and reported the demo absent from both. Re-run against the linked
binary, with a debug build as the positive control and a real UI string as the
negative one: release 0, debug 2 and 1, control 1. Three readings, because
"absent" and "my search is broken" look identical.

**A parser bug produced a false positive.** The check that every IPC command
the window calls is registered flagged `reset` as missing. It was the last
entry in the handler list and had no trailing comma. The check is now a test in
`verse-app`, with both directions asserted, and it was falsified by removing a
command — it named the missing one.

### What is not verified

**The layout has never been seen.** The window opens, does not panic, and every
command it calls is answered — but whether the regions are the right size, the
sidebar readable, the bar convincing, is a look, and a look needs a person.
This is the largest gap in the round and it is stated rather than implied.

Also unverified: that the percentage matches wall-clock; that the Chinese
summary *reads* well (only that it is verbatim and ordered); `Duration:` on
containers other than WAV; and a two-hour transcript, where both the summarizer
and a several-thousand-row pane would really be tested.

**Two findings left as findings.** The `export` command still does not update
`outputs.json` — only autosave does — so a transcript saved only via *save as*
has no record; that would matter to any future history and does not today. And
the guard's recovery pass clears the transcript and re-sweeps, so the bar
**resets to zero** on a recovered file, which is arguably honest and is at least
now written down.

364 tests, clippy clean, `svelte-check` 0 errors.

### Two more found by insisting the demonstration actually work

Making the dev-only demonstration run end to end — and *checking the filesystem
afterwards* rather than assuming — turned up two things that unit tests could
not have.

**The window's startup was a chain of awaits, and one failure killed the rest.**
`currentState()` → `models()` → `probeHardware()` → subscribe → the demo call,
all inside one `async` block with nothing catching. A catalogue that failed to
load would have taken the *subscriptions* with it, leaving a window that renders
and never updates — precisely the failure mode the project names most often. The
steps are now independent and a failure is shown rather than swallowed.

**The demonstration used a path that did not exist.** It reported
`演示音频.wav` as its input, which was deliberate — a person watching must not
mistake it for their own recording — and wrong: the automatic save identifies an
input by its length and modification time, so a path that cannot be stat'd is
refused. Correctly refused, and already tested
(`an_input_that_cannot_be_read_is_reported`). The demonstration therefore ended
with a transcript on screen and nothing written anywhere, and only looking in
the output directory said so. It now creates a real placeholder file, and the
whole path — bus, state, roster, bridge, autosave — is exercised for real:
running the app produces `演示音频.srt` with the twelve lines and correct
timecodes.

**The window's startup was also where the demonstration hid.** While the call
sat at the end of that chain it never ran, and the command swallows nothing —
it was simply never reached. Moving it to the front is what made the probe
print, and is what identified the chain as the problem.

**What the extractive summarizer actually picks**, for a twelve-line meeting:
two sentences, "整体交付比计划晚了大概两周。" and "我建议先把测试设备的钱留出来。"
`Budget::default()` is at most 7 sentences *and* at most 20% of the candidates,
whichever is smaller, so a short transcript is bounded by the ratio and a long
one by the cap. Two of twelve is defensible and thin; whether it should be more
is a judgement the maintainer can make on sight, which is why the number is
written down here rather than tuned invisibly.

365 tests, clippy clean.

## 2026-10-07 — The window, corrected after the maintainer looked at it

Nine things, most of them things no test could have caught because they are
about how the page *looks* rather than what it holds.

**Scrollbars had never been styled.** `app.css` had no `scrollbar` rule at all,
so the webview drew the operating system's: a wide grey trough with stepper
arrows, taking about a fifth of a 288 px sidebar. Now thin, `--border` coloured,
and darkening on hover so it can still be found. The comment says why the
colour is furniture rather than foreground.

**The title bar is gone.** It held the application name, the running job's
numbers and two buttons. The numbers moved beside the bar they describe, the
name and version moved to the bottom-left of the sidebar where they cost no row
of their own, and 关于 went with them. That is one whole horizontal band of
window reclaimed, which is what was asked for.

**取消 moved next to the progress bar and turned red.** It belongs to the bar —
P4 says anything over a second can be stopped, and P5 says the bar is where the
waiting happens — and `variant="destructive"` in this component set is a tint
(`bg-destructive/10`), not the solid red that was asked for, so the class
overrides it.

**添加文件 is pinned.** It was inside the scrolling list, so it moved down as
files accumulated. A button people have to go looking for is a button that
should not have been scrolling.

**The summary and the transcript were fighting for the same space.** The
summary section was `shrink-0` with no height bound: a long one pushed the
transcript off the top of the pane, and the transcript lost. It is now capped
and scrolls on its own.

**A real bug: the per-file view state was not reset on a click.** `noteShown`
ran only after a whole snapshot, so selecting another file left the previous
one's summary and messages on screen — one file's conclusions under another
file's transcript. It now runs after every update, which is what the user
described as "going forward but not back".

**A regression I had introduced: the failure screen lost its retry.** The
`Recovery` enum exists so that every failure names an action it can actually
take — retry, fetch a model, or choose another file — and rewriting the screen
had flattened all three into one 返回, which is only ever the third. The buttons
are back, one per variant. That needed a **new `retry` command**, because the
new deduplication reads a re-dropped path as "here it is" and starts nothing,
so retrying had no way in at all.

**Re-dropping a file is now a look-up, not a second row.** It selects the row
that file already has. Keyed on the path, so two copies under different names
are still two files.

The IPC-name test caught the `retry` command the moment it was registered and
the window did not yet call it — which is what it is for.

**A question with an unwelcome answer: the evaluation dataset is gone.** A
search of the whole drive for `manifest.tsv` and for `.parquet` finds nothing
outside the recycle bin. What survives is `tools/asr-eval/extract.py`, the
script that made it, and the numbers in `tasks/asr-evaluation.md`. Rebuilding
it means running that script against the source parquet again.

367 tests, clippy clean, `svelte-check` 0 errors.

## 2026-10-07 — The service documented inside the window, and two layout corrections

**关于 now carries the API, written for a program.** The dialog lists the
service's routes in Chinese, for a person skimming, and holds the full
description in English behind 查看完整说明, with 复制给 agent to hand it over.
The brief is built to be self-contained: what starts the service, where the
discovery file is, that every request needs the bearer token, that an `Origin`
header is refused, the routes, the 202-and-poll cycle, and that `fraction` can
be null.

**The rows and the brief are generated from one list**, so the dialog and the
text somebody pastes cannot describe different APIs. What that does *not*
prove, and the comment says so: the list is a copy of `crates/verse-cli`'s
router, which reaches this crate only as a running process — the CLI has no
library target to import. A route added there and not here would be documented
wrongly, and `llms.txt` has the same exposure.

**The brief was half Chinese and the comment claimed it was English.** Caught
by reading the assembled text rather than the substring tests, which passed
either way. The route descriptions are for the person at the dialog; the brief
now prints method and path only, and says what the routes are for in prose, in
one language.

**摘录式摘要's heading row is gone and its button moved to the footer**, beside
另存为 and 再来一个 — the three things you can do to a finished transcript now
sit together. The summary area only exists once there is something in it.

**A drop anywhere shows a glowing frame.** The empty state is a drop target and
lights up on its own; every other state accepted a drop with nothing on screen
saying so. The overlay is `pointer-events-none`, so it cannot swallow the drop
it is advertising.

**The demonstration was leaving files in Documents.** It gave each run a unique
path, and the automatic save names a result after its source — so every run
produced a new numbered `演示音频.srt` in a person's documents folder. Nine had
accumulated. One path, and the run re-runs a file already in the list through
`retry`, which is what `retry` is for; the output overwrites itself now.

370 tests, clippy clean.

## 2026-10-07 — One bar, one button convention, and a lock that locks the right thing

**The engine was lockable in the wrong place.** The model buttons were disabled
by asking the *screen being shown* whether it was working. A job runs on its own
entry, so selecting a finished file while another was mid-run made the buttons
look available; pressing one raised an error, because the backend checks
`running` and knows better. The window now asks the list — is any entry working
— which is the question that matters, and it says why in a line above the
buttons rather than going quietly dead.

**Which engine made which transcript is now recorded.** `FileEntry` carries the
engine it was handed to, set when the file actually starts and sent with every
row. Without it, changing engines mid-session left a list of results with
nothing distinguishing them, and reading the session's current engine at display
time would have relabelled everything already done. The finished bar now says
完成 · 用 SenseVoice-Small 识别 · 共 N 段.

**The status bar is one row with two states.** 取消 lived above the transcript
and the action buttons below it, which is why they disagreed about the edge of
the pane. They never happen at the same time, so they are the same row in the
same place: a progress bar with 取消 beside it while working, and the summary
with 摘录 · 另存为… · 再来一个 when finished.

**Buttons follow one convention, and none is a bare ghost.** The primary action
a screen exists for takes the filled variant, everything else takes the
secondary fill, and stopping takes the destructive one. All of them are the
vendored `Button`; nothing is hand-styled except that cancel is forced to a
solid red, because this component set's `destructive` is a tint.

**The demonstration was still littering, and the first fix did not work.**
Giving it one path was not enough: the automatic save identifies a source by
path, length **and modification time**, and the placeholder was rewritten every
run — so a fixed path with a fresh mtime is a *different* source, and each run
produced another numbered file. It is now written once and left alone.
Checked by running it three times rather than once: three runs, one file. The
first fix looked right and was verified too briefly.

370 tests, clippy clean, `svelte-check` 0 errors.

**The locked-model explanation is a hover, not a permanent line.** It was a
paragraph that appeared whenever anything was being recognised — which is
correct and also a line that is always there, so it becomes a line nobody
reads. The model rows grey out and stop responding, and the reason appears on
hover over the section.

Anchored to the *section* rather than to the buttons, and that is not cosmetic:
a disabled button still hovers its parent, so a tooltip attached to the buttons
themselves would never appear on the one thing somebody would be pointing at.

**A drop is checked before it starts, not after it fails.** Dragging a folder or
a spreadsheet onto the window used to hand it to the decoder, watch it fail, and
show a decode error — which reads as the program being broken rather than as the
file being wrong. The window now asks `check_files` first and refuses with a
dialog naming each file and the reason, before any progress bar appears.

**The list of what this program reads is now in one place.** It was in
`crates/verse-cli` *and* copied into the window's file-picker filter. It lives
in `verse-core` now, and both callers use it — the picker asks for it on mount,
the command line expands folders with it, and `check_files` refuses with it. One
list, three readers, none able to drift from another.

Two extensions were added while moving it (`m4b`, `ape`, `wv`, `oga`), and the
test is now case-insensitive: `.MP3` is an mp3, and on Windows a capitalised
extension usually is exactly that — a file something else renamed.

**The window is deliberately stricter than the command line, and the code says
so.** `verse-cli` attempts a file named explicitly whatever it is called,
because refusing by extension would turn a decodable file into a usage error and
the decoder is the only thing that really knows. A person who drags the wrong
thing onto a window should hear about it before waiting rather than after. The
consequence — a file with an unusual but decodable extension is refused by the
window and accepted by the command line — is recorded in `verse-core` beside the
list, as an intended difference rather than a bug to be discovered later.

373 tests, clippy clean, `svelte-check` 0 errors.

## 2026-10-07 — The download moves into the model card, and the demonstration stops firing by itself

**A model can be fetched from its own card.** The panel said 需要下载 and
nothing else: the only way to get a model was to drop a file that needed it and
wait to be told. Each missing model now carries a 下载 button, and the progress
bar and byte counts appear on the row that is downloading. That is what the
panel is for — getting a model ready *before* there is a file that wants it.

`fetch_model` takes the model id now, and `Update::Download` says which model it
is about, so one model's progress cannot appear on another's card. The screen
that waits for a model and the card both call the same command; they differ only
in what happens afterwards, and a finished download resumes the waiting file
**only when the waiting file wanted that exact model** — fetching something else
from the panel must not start a job whose model is still missing.

The card is a `div` now rather than a `button`: a button cannot hold a button,
and the download control belongs on the row that says the model is absent.

**The demonstration no longer runs on its own.** It fired whenever `VERSE_DEMO`
was set, and the development instructions tell you to set it — so every launch
did a demo job and the program looked like it was doing something by itself.
Asking is a click now: `demo_available` decides whether the window *offers* the
demonstration, and `demo_progress` runs it.

**What "it cannot ship" means, exactly, and where it is weaker than it sounds.**
The release binary is searched and has no demonstration in it — the command's
body is `#[cfg]`-compiled out, so there is no publisher to reach. The *frontend*
bundle is a different matter: `演示` appears in it twice, inside a branch guarded
by `import.meta.env.DEV`, which the build replaces with `false`. So the code
cannot run — the button cannot render and the command answers `false` — but the
strings are shipped in the JavaScript. Recorded rather than described as absent.

Verified by launching without `VERSE_DEMO` and checking that the demonstration's
output file was not rewritten, rather than by reading the guard.

373 tests, clippy clean, `svelte-check` 0 errors.

**The download bar was measuring the wrong thing, and it was reported as a
number.** The card said 987 MB and then showed "44 MB" — which is
`conv_frontend.onnx`, the first of Qwen3-ASR's five files. The downloader pulls
one file at a time and its `Fetching` state is per-file, with a comment saying
so in as many words; the window passed that straight through. A bar that fills
once per file fills five times and looks finished on the first.

The running total is now added up from the catalogue, which knows every file's
size. `ModelProgress` notices the file-name change to decide what is finished —
and a mirror retry re-reports the same name, so a name that has not changed is
not counted twice. A model with any unknown size reports *no* total rather than
a partial one.

It is a struct with tests rather than a closure, because the arithmetic is the
part that goes wrong and a closure inside a download callback cannot be tested
at all. Falsified by making it return the per-file figure: `left: Some(1),
right: Some(101)` — which is the reported bug, in a test.

The same number was wrong on the screen that waits for a model, for the same
reason; it reads the panel's figures now.

**A model being downloaded cannot be chosen as the engine.** Selecting it would
put the window in a state where the next file has no recogniser — the download
would have to finish before anything could happen, and the row already says so
with a progress bar.

## 2026-10-07 — The downloader, after being asked whether it was good enough

The question was whether it handled resume, concurrency, hash verification and
the ways a download goes wrong. Auditing it turned up more than expected and
one genuine bug that had nothing to do with the four.

### What was already there

Resume, with the classic corruption handled — a mirror that ignores `Range`
answers 200 rather than 206, and the partial is **discarded rather than appended
to**. Atomic promotion through `<name>.part` and a rename, so an interrupted
transfer never looks complete. Truncation refused before promotion. Mirror
rotation with every failure reported together. Cancellation that leaves the
partial. Connect and response timeouts, aimed at a mirror that accepts a
connection and then says nothing. That is a better downloader than most, and
the gaps below are only visible against it.

### The bug: a failed download never reached the window

Every failure path ended `return Ok(Failed { .. })` **without calling the
callback**. The return value was the only place the failure existed — and the
window does not read it. A download that failed sat on "fetching" for ever.

Found while restructuring that function, not by looking for it. Pinned by
`a_failure_reaches_the_callback_and_not_only_the_return_value`, and falsified:
with the report removed the callback sees `Idle` and three `Fetching` and never
a failure, which is exactly what a person would have been looking at.

### The four that were asked for

**SHA-256, computed while streaming.** `ModelFile.sha256`, fed from the same
buffer that is written to disk, compared before the rename — so nothing that
fails verification is ever at the destination where `is_present` would trust it
by length, and a mismatch **deletes the partial**, because resuming would
continue from bytes already known to be wrong.

**Where the hashes came from matters, and is recorded rather than implied.**
They were computed from the copies on this machine. The mirrors are hand-written
and one is a third-party upload, so there is no signed list to compare against:
this pins *the bytes we have*, and would not catch a mirror that served
something wrong from the beginning. Five of nine files are hashed; the rest are
Qwen3-ASR's, which are not downloaded yet. `sha256_file` lives beside the check
that consumes it, so the two cannot be different algorithms — that failure would
make every recorded hash wrong at once and look like every mirror being corrupt.
It is tested against the published vector for "abc", because a hash compared
only against itself would agree with any mistake it made.

**A mirror is tried three times.** `qwen3-asr` has a single mirror; one dropped
connection was the end of the download. Backoff doubles from 400 ms and is
**cancellable in slices**, so pressing 取消 does not wait it out. A server that
answered with a 4xx is **not** retried — it will answer the same way — while a
5xx and a dropped connection are.

**Free space is checked first.** Summed from the catalogue for the files that
are missing, against what the filesystem reports, with 64 MB of headroom. A
download that does not fit says so before it starts rather than failing a
gigabyte in. `sysinfo` was already in the graph for the memory probe, so this
is a manifest line and a feature.

**Files are fetched together**, up to three at a time — they are independent,
and `qwen3-asr` is 987 MB whose largest file is 756, so the other four cost
nothing to fetch alongside it. Three rather than five because they come from one
host.

That last one **changed the progress arithmetic, for the better**. The window
decided a file was finished by noticing the name change, which is only true when
they arrive one at a time. It is now the latest figure *per file*, summed, and
seeded with the files already on disk — so a resumed download starts at what is
there rather than at zero, and concurrency needs no special case.

### A test that was slow for the wrong reason

One test pointed a mirror at `127.0.0.1:1` to make the transfer fail. With
retries it went from ~2 s to 6.4 s — a refused connection costs about two
seconds here and the retry paid it three times. It now uses an address that
fails at the request rather than after a round trip: the same assertion, and the
suite is back to 1.2 s, faster than it was before any of this.

### Not done, and recorded

Many connections for one file — splitting a file needs seeks and reassembly, and
the gain is smaller than parallel files. A lock against two processes writing
one `.part`. `Retry-After` on a 429, which the backoff covers roughly. And
`is_present` is still length-only: hashing 987 MB on every check is not
affordable, so a corrupt file already on disk is trusted.

379 tests, clippy clean. The task log is `tasks/downloader-robustness.md`.

## 2026-10-07 — The extractive summary is judged not worth having, and the first measurement of what would replace it

Three things from one report: the summary panel could not be closed, an engine
that is not on disk could still be chosen, and — the maintainer's judgement,
having looked at it — **the extractive summary is not worth having.**

### The two fixes

**The summary opens and closes from the same button.** It could not be closed at
all: the only control that touched it was the button that made it, and a second
press looked like it had done nothing.

**An engine that is not on disk cannot be chosen.** The previous round blocked
choosing a model *while it was downloading*; that was not the whole rule. A
model that is simply absent could still be selected, leaving the window in a
state where the next file has no recogniser — and the row already says 需要下载
with the button to fix it. Refused in the backend as well as greyed in the
window, because a rule enforced only by the interface is a rule the interface
can be talked out of.

### Why the extractive summary is going

It can only quote. It cannot merge two sentences about one thing, cannot say
what was decided, and cannot answer the only question anybody has about a
meeting. "关键句" would be a fairer name, which is another way of saying it does
not do the job. `tasks/llm-summary.md` carries the reasoning, and it does not
propose keeping it as a consolation.

### The measurement, and what it is actually about

A model has to write a summary, so a small one was measured — **on a real
transcript**, `标准录音 13.srt`, 5,718 characters of Chinese, not a sample
written for the occasion.

| Qwen2.5-0.5B-Instruct Q4_K_M | |
|---|---|
| prompt | 3,831 tokens |
| prefill | **279 s** |
| generation | 65 s for 221 tokens |
| **total** | **5 min 45 s** |

**That number is about candle, not about small models.** Prefill is *linear in
the prompt* — 233 tokens took 13.5 s, 3,831 took 279 s, both about 58 ms a
token. A prompt handled in one batched matmul does not get more expensive per
token the longer it is. Candle's quantized CPU path is building this one token
at a time, and reporting "small LLMs are too slow on CPU" from this run would be
**comparing two things that are not comparable**, which is the mistake this
project names first.

The achievable speed could not be measured here: no `cmake`, no `ninja`, no
`make`, and llama.cpp lives on GitHub. That toolchain is the prerequisite for
any speed conclusion at all.

**The quality needed no runtime to judge, and it is the part that matters.**
The first run, greedy, repeated one sentence seventeen times — which said more
about the decoder than the model, so a presence penalty was added and it was run
again. The second output is fluent and on topic and **wrong in the ways that
matter**: it frames the conversation as instructions to 被申请人 when the
transcript is somebody asking how to *file*; it says material may be submitted
by email or WeChat, which is **not in the transcript**; and three of its eight
points are the same point.

A summary somebody is meant to trust that is confidently wrong is worse than no
summary, because the value of the feature is that reading it is cheaper than
reading the transcript.

**1.5B was not measured**, and that is deliberate: at candle's prefill rate one
run is about fifteen minutes, and measuring it badly would be worse than saying
it was not measured. The order is a runtime that batches, then 1.5B, then the
decision.

The spike is `tools/llm-summary-spike/` — its own workspace so none of this
weighs on `cargo test --workspace`, with a `.gitignore` for the 1.7 GB of cargo
output and 476 MB of weights, because the root ignore covers `/models` and this
directory is not that one.

## 2026-10-07 — The extractive summary comes out, and yesterday's transcripts come back

### The summary is gone

It was measured and judged not worth having — `tasks/llm-summary.md` carries the
numbers and the reasoning. Removed rather than kept as a consolation, which is
what that document said should happen if the measurement went this way:
`verse-core::summary`, the `summary` command, `SummaryView`, and the panel. Ten
tests went with it, because they were pinning behaviour nobody wants.

The spike that measured a replacement stays, outside the workspace, until the
question is settled.

### What closed, and what could not be chosen

**The summary opens and closes from the same button** — it could not be closed
at all, and the only control that touched it was the button that made it, so a
second press looked like it had done nothing.

**An engine that is not on disk cannot be chosen.** The previous round blocked
*downloading* models; that was not the whole rule. Absent ones could still be
selected, leaving the window in a state where the next file has no recogniser.
Refused in the backend as well as greyed in the window, because a rule the
interface enforces alone is a rule the interface can be talked out of.

### History: the list survives the window closing

It did not. The file list lived in memory, so closing the window lost it while
the `.srt` files it had written stayed in `Documents/Verse` — the results
survived and the record of them did not, and somebody who transcribed a meeting
last week had no way to reach it from inside the program.

**The roster, written down.** `verse-store::history` records the input, the
output, the engine and when it finished, capped and written atomically like the
output record beside it. A pointer, not a copy: the `.srt` on disk **is** the
result, and a second transcript in a database would be a second thing that can
drift from the one a person can open in a player.

**And read back, not duplicated.** Restoring a transcript means parsing the file
it was written to, so `verse_core::export::parse_srt` was added — the inverse of
`render`, in the same module so the two cannot disagree about the format.

**A file that is not subtitles is refused rather than half-read.** The cue
number is discarded (a position, not information, and files get renumbered by
hand); a comma or a dot between seconds and milliseconds is accepted (SubRip
says comma, several tools write a dot, players take both); cue settings after
the end timestamp are ignored; a block with no `-->` is skipped rather than
becoming a subtitle line.

**Validated on a real file, not a fixture.** The maintainer's own recording —
124 cues, 5,595 characters, with English fragments and mixed punctuation in it —
parses, and **re-rendering reproduces the file byte for byte**. That is the
strongest form of "the two cannot disagree".

A restored row is `Done` **with `exported` already set**, which is what stops
the automatic save from writing it out a second time on the first event after
launch, into a numbered file beside the one it came from. Nothing is selected on
launch: the drop target shows, with what was done before sitting beside it.

**Verified end to end, in the two halves that can be seen from outside.** The
record: the demonstration produced an entry naming its input, output, engine and
time. The restore: deleting the result and relaunching pruned the entry and
rewrote the file, which is only possible if the record was loaded and walked at
startup. **Whether the row appears in the list needs a person** — that is the
click this project keeps having to hand over.

382 tests, clippy clean, `svelte-check` 0 errors.

## 2026-10-07 — The history list learns to be used: shown, filtered, removed

The history worked and could not be *managed*. Four things, all from the
maintainer looking at it.

**Show the result in the file manager.** After reading a transcript the next
thing a person usually wants is the file, and the program already knows where it
is. `explorer /select,` on Windows, `open -R` on macOS, `xdg-open` on the folder
elsewhere — spawned with `std::process`, so no plugin and no new dependency.

**The exit status is not checked**, and that is deliberate: `explorer.exe`
returns non-zero when it *succeeds*, so treating that as failure would report a
broken action every time it worked. What is checked is the file existing, and
that check is three lines in front of the spawn.

**A filter, once the list is long enough to need one.** Above six rows a small
box appears; below that it would be furniture. The row's index travels with it
through the filter, because everything that acts on a row — selecting,
forgetting — addresses it by its place in the roster rather than its place in
the filtered view. That is the bug a filter invites, and it is why the filtered
list carries pairs rather than the entries alone.

**Removing a row, in both senses.** The 🔥× on a row is the reversible half:
the transcript stays on disk and only the record forgets it — and the record
really forgets it, because forgetting the row without forgetting the entry would
put it back on the next launch and "remove this" would mean nothing. The 删除
button in the finished bar is the irreversible half, behind a confirmation,
because a file that is gone is gone.

Keeping them separate is the point. "I do not want to see this" and "I want this
deleted" are different sentences, and one button that guessed between them would
be a button that sometimes deletes something.

**Not done, and now the largest gap in this feature:** a file already in the
list *still* cannot be transcribed again. That rule was the maintainer's — "re-doing
a file that is already here should be skipped" — and it was right when the list
lived for one session. With history it is permanent: drop a recording that was
re-cut since, and the window shows the old transcript with no way to refresh it.
The maintainer judged it lower priority than the four above; it is written here
so it is not quietly forgotten.

385 tests, clippy clean, `svelte-check` 0 errors.

---

## 2026-10-07 — CI, a release path, and two platforms dropped for different reasons

### CI, which immediately caught something

`ci.yml` runs the tests, clippy and the frontend type check on every push. Its
first run failed, and the failure is the most useful thing in this entry.

```
---- bridge::tests::only_the_file_name_is_shown_never_the_whole_path ----
  left: "C:\Users\someone\录音\第三季度会议.m4a"
 right: "第三季度会议.m4a"
```

The test is Windows-only; `file_label` is not. It hands `Path::file_name()` a
path spelled with backslashes, which on Linux are ordinary characters, so the
whole string comes back. The build compiled and 87 of the crate's 88 tests
passed.

### macOS, and then Linux

macOS went first, for the reason already recorded: no LGPL ffmpeg build exists
for it. Linux went next, and **the reason matters, because the obvious reading
is the wrong one.** Linux did not fail to build. It was dropped as a scope
decision after that one test, which is a test that needs a `#[cfg]`, not a
program that needs a port.

Both removals are written into the workflow rather than left in a commit
message, because "Linux was dropped" and "Linux did not work" are different
sentences and only one of them is true.

### Three things in the release path that were wrong

Found by reading the source of `tauri-action` and `tauri-bundler`, which is the
only reason they were found at all — every one of them would have looked fine
in a green run.

**`bundle.targets` is intersected with the platform, silently.**
`Settings::package_types()` keeps only the types the current platform supports,
and `bundle_project` returns an empty vector when none survive. A
`targets: ["msi", "nsis"]` config on Linux therefore **succeeds and produces
nothing** — the worst shape a failure can take in this project. It does not
bite on Windows, where the config is correct as written, and Linux is gone. It
is recorded because the next platform added will meet it.

**Manual dispatch would have created a release tagged `main`.** `github.ref_name`
on a `workflow_dispatch` run from `main` is `main`, and that is what the action
would have used as the tag — on the one trigger added specifically so a bundle
could be tried without publishing. The tag input is now empty unless the ref is
a tag.

**`FFMPEG_VERSION: "8.0.1"` pinned nothing, and 8.0.1 does not exist.** It was
declared, never referenced, and named in `THIRD_PARTY_NOTICES.md` as the version
the licence belongs to — a notice that was wrong in a way nothing would have
caught. BtbN publishes per-release branches inside `latest`; the fetch now names
`ffmpeg-n8.1-latest-win64-lgpl-8.1.zip`, which is a version a licence can point
at and still takes patches.

### One thing removed with the platform

`ffmpeg.rs`'s `../lib/Verse` candidate was the `.deb`/AppImage layout, reasoned
from the bundle's structure and never seen on a machine — its own comment said
so. Same removal as macOS's `../Resources`, one release later. `bundled()` went
with it: with a single candidate left it was a wrapper around `beside()`, so
`locate()` calls `beside()`.

### Still true

**Neither workflow has ever run.** The YAML parses, has no control characters,
and that is the whole of what is known. **Nothing is code-signed**, so Windows
shows a SmartScreen warning.

388 tests, clippy clean, `svelte-check` 0 errors.

---

## 2026-10-08 — The release ran, and four things came out of it

### The first release run failed at step 3

```
find: missing argument to `-exec'
```

`find ... -exec cp {} dest +` — GNU find as shipped in Git Bash rejects the `+`
form whenever anything follows `{}`. That is the POSIX rule for `;`, which GNU
normally relaxes and does not relax here. Three commands in the same shell:

| command | result |
|---|---|
| `find . -maxdepth 0 -exec echo PREFIX {} +` | ok |
| `find . -maxdepth 0 -exec echo {} SUFFIX +` | `missing argument to '-exec'` |
| `find ... -exec cp {} destfile +` | `missing argument to '-exec'` |

Quoting the plus does not help. Passing it through a variable does not help.
`MSYS2_ARG_CONV_EXCL='*'` does not help — the argument is not mangled on the way
in, it is rejected. `-exec ... \;` is unaffected.

**The expensive part was never the bug.** It was that the step lived in a YAML
`run:` block, where the only way to try a fix is a push. On this machine the
whole diagnosis took six shell commands.

### So the steps became scripts

`tools/ci.sh` mirrors the CI job and can be run here. `tools/fetch-ffmpeg.sh` is
the release step that failed, as a file — run it, and the whole fetch, unpack
and placement is exercised in one command. It also refuses to exit 0 on a `find`
that matched nothing, which is the failure that would otherwise surface much
later inside the bundler as something unrelated.

Invoked as `bash tools/fetch-ffmpeg.sh`, not `./`, because the executable bit is
a POSIX file mode and this repository is developed on Windows where git will not
set one.

### The bundled ffmpeg's licence had been asserted, never checked

Three documents said "an LGPL build" and none had looked at one. Running the
fetched binary:

```console
$ ffmpeg -version | tr ' ' '\n' | grep -E '^--enable-(gpl|nonfree|version3|lgpl)'
--enable-version3
```

`--enable-version3` present, `--enable-gpl` absent. It is **LGPLv3** — a version
the prose had never named, and the version that matters, because LGPLv3 §3 pulls
GPLv3's terms into the distribution.

**Which exposed a gap: the bundle carried an LGPLv3 binary and neither licence
text.** Both are now in `licences/`, taken from FFmpeg's own `COPYING.LGPLv3`
and `COPYING.GPLv3`, and both are listed under `bundle.resources`. The reported
build, `n8.1.3-14-g330caae0c1-20261007`, is in `THIRD_PARTY_NOTICES.md` so the
notice names what actually shipped rather than a filename.

### Where the release stands

Step 3 is now a script that has been run here and exits 0. Everything after it —
the `--config` merge, `tauri-action`, the installers, the draft release itself —
has still never executed. The tag `v0.1.0` exists, the run failed, and no release
was created.

388 tests, clippy clean, `svelte-check` 0 errors.

---

## 2026-10-08 — The release built

Second run of `release.yml`, on `cc45f87`, on the tag moved from the failed
commit. **Every step green**, including the one that had died at ninety seconds:

| | |
|---|---|
| 3 ffmpeg — the LGPL build | success |
| 9 Build and attach | success |

Both of those had never executed. The `--config` merge worked, `tauri-action`
built the installers, and it did not throw `No artifacts were found` — which,
with `tagName` set, is what an empty artifact list does. So a draft release
exists with at least one installer attached.

**The draft, once seen:** `Verse v0.1.0`, drafted by `github-actions`, with
`Verse_0.1.0_x64-setup.exe` (47.5 MB) and `Verse_0.1.0_x64_en-US.msi` (63.7 MB),
plus the two source archives GitHub adds to everything.

**The ffmpeg is inside it, and that is arithmetic rather than a guess.**
`verse-app.exe` is 29,945,856 bytes and the bundled ffmpeg 134,092,288; together
they `gzip -9` to 63.1 MiB. NSIS uses LZMA, typically 15–25% below that, which
puts the expected installer near 49 MiB against an actual 47.5 MiB. Without the
ffmpeg the payload gzips to 10 MiB and the installer would be single digits.

**A wrong turn worth the ink.** Before the log arrived, a report that the
release had no binaries was taken at face value, and a proof was built on it:
`tauri-action` throws on an empty artifact list, `core.setFailed` wraps its whole
body, therefore the step could only have gone green with `tagName` empty. That
contradicted `head_branch: v0.1.0`. **The contradiction was the finding** — the
inventory was incomplete, because `/releases` does not return drafts to
unauthenticated requests and there were two releases on the tag: the
maintainer's hand-made one, published and empty, and the workflow's draft with
both installers. The step log settled it in one line: `Couldn't find release
with tag v0.1.0. Creating one.`

**Still open, and now the only thing:** nothing has been installed. Where the
installer puts the ffmpeg, and whether `beside(exe)` finds it there, needs a real
installation — the one link a local run cannot reach.

**The tag moved, which is worth remembering.** `v0.1.0` was force-pushed from
`ff12817` to `cc45f87`, because a tag points at a commit and the commit that
failed cannot be the one a release is built from. It cost nothing this time —
the first run published nothing, so there was no release to contradict. There is
no such margin on the next one.

One warning on the job, not acted on: `actions/checkout@v4` and
`actions/setup-node@v4` target Node.js 20 and were forced onto Node.js 24.
Another release run to silence a warning that is not yet an error is the wrong
trade; it will have to be paid when it stops being a warning.

388 tests, clippy clean, `svelte-check` 0 errors.

---

## 2026-10-08 — A model the window could not see, and a model in the wrong place

Two reports, one day after the first release. `tasks/model-availability.md` has
the detail; this is what changed and why.

### The card did not know

Download a model and the window kept saying 需要下载 until it was restarted.
`model.present` gates whether a card can be picked, it is computed by the backend
from the files on disk, and the window asked for it **once, at startup** — so a
download that finished during the session wrote a gigabyte and told nobody who
was listening. `importModel` had the same hole from the other direction.

Both now re-ask. Re-asking rather than setting the flag locally, because a local
guess stops being true the first time a model is removed underneath it.

A comment in `fetch_model` had been claiming this was handled: "The panel's card
has to stop saying 需要下载" above a `push_state`, which pushes the file roster
and the screen and no part of the catalogue. Corrected — the comment was the
reason nobody looked.

### The model was in the install directory, and that was a bug and not a mess

The second report was "卸载的时候没删除模型". The leftover was the smaller half.

**The `.msi` installs per machine, into `C:\Program Files\Verse`**, and models
resolved to `C:\Program Files\Verse\models` — where a standard user cannot create
a directory. A `.msi` install could not download a model at all. The `.exe`
installer takes the other path, `%LOCALAPPDATA%\Verse`, where "beside the
executable" happened to already be correct; **one of the two installers could not
have shown the fault**, which is how it reached a release.

`models_dir()` now resolves `VERSE_MODELS`, else `<per-user data dir>/models` —
beside the cache and the history, which have used that directory since October.
`ui-design.md` §11 had asked the question; it is closed. No migration, because
0.1.0 was the first release and there is nothing to migrate.

### The checkbox was already there

Asked what uninstall should do with 1.2 GB of weights, the maintainer asked for a
box to tick. Tauri's uninstaller already has one, labelled **Delete app data** —
and it removes `%APPDATA%\<bundle id>`, a directory this application has never
written to. So it was tickable, honest, and removed nothing.

`nsis-hooks.nsh` reads the same variable and removes `%LOCALAPPDATA%\Verse` when
it was ticked and not otherwise. Four lines that make the box mean what it says.
The `.msi` has no equivalent hook, so that installer keeps the weights.

### The hook was falsified, not assumed

An invalid command appended to the `.nsh` fails the build with
`!include: error in script: ... on line 32`. That is how we know `makensis` reads
the file rather than skipping it — every hook in the template is guarded by
`!ifmacrodef`, so a file that was included and never reached would compile
silently. This is the same class of mistake as the release step that had never
been run anywhere, and it was worth thirty seconds to not make it twice.

**A number came out of it.** The debug NSIS bundle with no ffmpeg is 9.02 MiB —
the first measurement of the "without it, the installer would be single digits"
half of the argument that the 47.5 MiB release carries the 128 MiB ffmpeg.

### Still unverified

The uninstaller has not been run and the checkbox has not been ticked. The
refresh needs a click — this repository has no frontend test runner, so it is
checked by types and by reading. And the `.msi`, the one install that was
actually broken, has not been installed by anyone.

388 tests, clippy clean, `svelte-check` 0 errors.

---

## 2026-10-08 — The window stops being the program

The maintainer asked for a tray, with the close button extended into "hide" and
"quit", and the logic around it finished. `tasks/tray.md` has the detail.

**What was actually wrong.** Nothing handled `CloseRequested`, `RunEvent` or
`on_exit`, and `run()` was a bare `Builder::default()...run(...)`. Closing the
window was Tauri's default, so the process ended — and a ten-minute
transcription went with it: no partial result, no warning, and the only way to
keep a job alive was to leave a window open and not touch it.

**The tray is the vehicle, not the point.** It exists so that closing is a
choice, and it carries the interrupted queue back.

### Quitting does not lose the work, and that is why the dialog can be honest

Checked before designing anything: the pipeline flushes a resume log on every
span and the GUI already used it. So the confirmation says what is kept and
where to find it, rather than threatening to throw away ten minutes of
recognition. `design.md` §4.9 gains that as a second reader — until now the log
was crash recovery, and quitting is a deliberate act that leaves it on purpose.

**One correctness catch made while writing the restore, not after.** The resume
log's key is a digest of the settings *and* the input, and the engine is one of
the settings. A queue put back under a different engine would miss every key and
redo the work — so the interrupted queue carries the engine with it.

### The two dialogs are constrained by where they run

Both go through `show(callback)`, never `blocking_show`: the handlers are on the
main thread, and `tauri-plugin-dialog`'s own documentation says blocking there
"will freeze your application". And both are **native** dialogs rather than
anything in the webview, because the window may be hidden at the moment the
question is asked — from the tray, it usually is.

### One thing taken off a "do not do this" list

`ui-design.md` §10 named the tray icon among things deliberately out of scope.
It came off, and the entry records why: not a change of taste, but that the
absence of it was killing running work. The same edit adds the reason live
subtitles stay out — Windows 11's Live Captions already captions system audio in
Chinese, on device, for free.

### Verified, and not

`cargo build -p verse-app` then running it: **still alive after twelve seconds**,
which means `setup()` returned `Ok` and the tray was created. A tray that cannot
be built fails there and takes the application with it.

Nobody has clicked anything. The tray by eye, both dialogs, and — the one that
matters — whether 继续 really continues rather than starting over, are all still
unverified.

404 tests, clippy clean, `svelte-check` 0 errors.

---

## 2026-10-08 — A fresh install could not transcribe anything

Found by the maintainer on the **published draft**, which is the first time
anything was ever installed rather than run from a checkout:

```
VAD model not found: silero_vad.onnx
```

**The VAD was unobtainable from the window.** Every transcription segments with
Silero VAD, the pipeline looks for it at one fixed path
(`Request::vad_for` — `<models>/silero-vad/silero_vad.onnx`), and it is not
something a person chooses. It sat in `models.json` as a peer of the engines,
and the window lists only models an engine can load — so it had no card, and
there was no way to ask for it. A fresh install downloaded the default model,
was told it was ready, and failed on the first file.

Nobody saw it because no development machine has ever lacked
`models/silero-vad/` — 2.3 MB, downloaded once, years of not being noticed.

**Fixed by naming the dependency rather than by teaching two front ends about
it.** `ModelSpec` gains `requires`, `sensevoice` and `qwen3-asr` name
`silero-vad`, and the catalogue offers `all_present` (the question a front end
actually has before starting a job) and `to_fetch` (what a download should
actually download). The window's card, its three readiness guards and the CLI's
`model fetch` all ask those instead of `is_present`.

Hard-coding the VAD in the window and again in the CLI was the alternative, and
it is the thing this project already has a rule against: *two callers inventing
it separately is how the CLI and the window end up disagreeing.*

**Verified end to end, not by reasoning about it.** Downloading into an empty
directory:

```
fetching Silero VAD (needed by SenseVoice-Small (zh, en, yue, ja, ko))
  silero_vad.onnx from modelscope: 2 / 2 MB  … done
fetching SenseVoice-Small (zh, en, yue, ja, ko)
  model.int8.onnx from hf-mirror: 0 / 239 MB  …
```

The VAD comes first, named, on its own mirror. Five new tests pin the rest:
the two ASR models declare it, the VAD declares nothing (which is why one level
of resolution is enough), the model is fetched before what it needs, and a
requirement naming a model that is not in the catalogue — or naming itself — is
refused at load. A typo there would otherwise not fail: `requirements` skips an
id it cannot resolve, so it would silently not download and surface later as a
model that says it is ready and does not work.

**The two READMEs were wrong before this and are right now.** Both said the VAD
comes with `verse model fetch sensevoice`. It did not.

404 tests, clippy clean, `svelte-check` 0 errors.

## 2026-10-08 — Three things only an installation could show, and a fourth it uncovered

The maintainer ran the 0.1.0 draft on a machine that is not this one, which is
the first time anything here has been installed rather than run from a
checkout. Three reports, and every one of them was invisible from the
development machine for the same reason: the dev machine has what the other one
lacks. Fixing the second turned up a fourth, which is the last section here.

**A black console window, three times.** 闪烁两个，第三个常驻不消失. `verse-app`
is a GUI program (`windows_subsystem = "windows"`) and ffmpeg is a console
program, so Windows gives each child a console of its own and draws it: two that
flash past — `locate`'s `-version` probe on `PATH`, then `version()`'s own — and
one held for as long as the decoder lives, which is the length of the file.

Fixed at the only place a spawn happens. `ffmpeg::command` is now the sole way
to build one, and on Windows it sets `CREATE_NO_WINDOW`; all four call sites go
through it. `DETACHED_PROCESS` would also have hidden the window and is the
wrong answer — it costs the pipes and the exit status, and the whole sidecar
design rests on both.

**A file dropped while the model was missing, stuck for good.** The guard in
`file_chosen` covered a running job and stopped there, and a file waiting for a
model sets neither `job` nor `running`. So a second drop found the slot free and
took it, and with the slot it took everything that follows the *selected*
screen: the download's progress (`download_changed` writes to the active
screen, so the first file's bar froze where it stood) and the job that starts
when the model arrives (`model_ready` starts the selected file, so the download
ran the second file and left the first waiting for a model that was by then on
disk).

The dead end was complete. Selecting the stranded row offered 下载模型, pressing
it hit `fetch_model`'s `all_present` guard, and the answer was `已经在本机了`.
The only way out was to remove the row and drop the file again.

`awaiting_model` makes a file waiting for a model hold the slot exactly as a
running job does, and the second drop queues behind it — 等待中 — and follows it
through the ordinary queue. `fetch_model` on a model that is already present now
says so on `UPDATE` and starts whatever was waiting, instead of refusing.

**The finished line could not be read in full.** It is one fixed width beside
three buttons, so it truncates; and the path in it was a bare file name, because
`view_of` passed `file_label`, so the folder was never on screen to be truncated.
The done screen now carries the whole path, the bar shows its last segment, and
the line has a `title` — hovering is where the sentence gets to be read.

**Five new tests, and three of them were run against the old code to check they
were worth having.** All three failed, naming the bug:

```
the second file took the wait that belongs to the first
the download never reached the file waiting for it: Idle
assertion `left == right` failed: the model started the wrong file
  left: Transcribe("b.wav")   right: Transcribe("a.wav")
```

The first attempt at the download test passed against the old code and had to be
rewritten: it asked the *screen*, and on the old code the screen belonged to the
second file. Asking the stranded file's own entry is the thing that pins it.

**Two of the three fixes are not verified from here.** No console window is
drawn, or not, on somebody's screen — what the new test can assert is that
`CREATE_NO_WINDOW` did not detach the process, which is the property that made
it the right flag. And the `fetch_model` branch is a command, so the only way to
run it is with a window. Both are tasks/first-run-fixes.md's "by eye" column.

**And the wart the second fix exposed, taken in the same pass.** `pending()` and
`unfinished()` read `running` and `queue`, and a file waiting for a model is in
neither — so it was not written to `pending.json`, was not counted by the quit
dialog, and was gone from the list on the next launch. Nothing was lost, since
nothing had started; the file itself was.

Including it was a line. What made it more than a line is what the restore does
with it: `restore_pending` put every path through `enqueue`, so the row came back
as 等待中 — a state it was never in, contradicting that function's own comment
— and 继续 would have started it, because `take_next` walks past whatever is in
front and what was in front was a file holding the slot rather than a running
job. `restore_pending` now takes `model_ready` and puts the first file back on
`NeedsModel` with the rest behind it, `start_next` refuses while a file holds the
slot, and `resume` says which model is missing instead of doing nothing.
`model_present` came out of that, so the drop and the restore ask one question
with one answer.

**Five new tests, then three more, and six of the eight were run against the old
code to check they were worth having.** All six failed, naming the bug:

```
the second file took the wait that belongs to the first
the download never reached the file waiting for it: Idle
assertion `left == right` failed: the model started the wrong file
  left: Transcribe("b.wav")   right: Transcribe("a.wav")
assertion `left == right` failed: Some("a.wav") vs None   ← 继续 started one anyway
the first file has to be the one offering the download: Queued { input: "a.wav" }
assertion `left == right` failed: 1 vs 2                  ← pending() missed the row
```

The first attempt at the download test passed against the old code and had to be
rewritten: it asked the *screen*, and on the old code the screen belonged to the
second file. Asking the stranded file's own entry is the thing that pins it.

**Two of the fixes are not verified from here.** No console window is drawn, or
not, on somebody's screen — what the new test can assert is that
`CREATE_NO_WINDOW` did not detach the process, which is the property that made it
the right flag. And the `fetch_model` and `resume` branches are commands, so the
only way to run them is with a window. Both are tasks/first-run-fixes.md's "by
eye" column.

**One thing deliberately left alone:** the 继续 bar says 上次有 N 个文件没跑完
whenever anything is queued, including a file dropped a moment ago in this
session. Pre-existing, and not this.

The entry above this one said 404 tests. The same command on `91d1453` says 409;
it was taken from the run before the five tests that entry describes. What is
measured now: **417 tests, clippy clean, `svelte-check` 0 errors** — 409 at
`91d1453`, plus the five above and the three here.

### Shipped as 0.1.1, which is the first version bump this project has done

`v0.1.0` moved twice and published nothing, so the tag was the only version
anybody had to think about. A second build is different: the installers are named
from `tauri.conf.json`'s `version`, and the window's 关于 dialog from
`env!("CARGO_PKG_VERSION")`. Tagging `v0.1.1` over a 0.1.0 tree would have
produced a release called *Verse v0.1.1* holding `Verse_0.1.0_x64-setup.exe`,
next to the 0.1.0 draft that was already there — two installers claiming the same
version and doing different things.

So both are 0.1.1, in the two files that carry a version and nowhere else.
Everything else in the tree that says `0.1.0` is a record of something that
happened, and stays.
