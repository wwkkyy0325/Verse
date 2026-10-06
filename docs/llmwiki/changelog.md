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
