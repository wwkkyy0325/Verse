# Verse — Frontend design

The interface for Phase 1: file transcription. Live subtitles (P2a) will add
screens to this design rather than replace it.

Companion to `design.md`, which covers the pipeline. This document covers the
layer above it and the contract between them.

## 1. What the interface has to be

The project brief is "usable by someone with no technical background". That is
a testable statement, and it rules out most of what a desktop app normally
does. Concretely, the interface must satisfy:

**P1 — The default path is one step.** Open the app, drop a file, get text.
No wizard, no project concept, no account, no first-run setup beyond what
cannot be avoided.

**P2 — The user makes no technical decisions.** Engine, thread count, segment
thresholds, punctuation, output format: all chosen automatically. Anything
adjustable lives behind a settings affordance that the default path never
opens.

**P3 — No jargon anywhere.** Not "VAD", not "ONNX", not "quantised", not
"model inference". The user sees "准备中", "识别中", "导出". If a concept
cannot be expressed in ordinary Chinese, the user does not need it.

**P4 — Every operation over a second is cancellable.** A 2-hour file is a
multi-minute operation. Being unable to stop it is the difference between a
tool and a hostage situation.

**P5 — Progress is visible as output, not as motion.** A spinner says "still
running"; a growing transcript says "it is working and here is what it found".
The second is what makes a long wait tolerable, and it is the same machinery
P2a needs for live subtitles.

**P6 — Every failure names a next action.** "下载失败" is not an error message,
it is a dead end. Each failure carries something the user can do: retry, pick
another mirror, choose a folder they already have.

P3 and P6 are the two that get violated under schedule pressure, so they are
stated as rules rather than guidelines.

## 2. Screens

Five, and no more, in P1b.

```
                          ┌────────────────────────┐
                          │  Empty                 │
                          │  drop target           │
                          └───────────┬────────────┘
                                      │ file dropped
                                      ▼
                        ┌──────────────────────────┐
                        │ models present?          │
                        └────┬────────────────┬────┘
                          no │                │ yes
                             ▼                │
                ┌──────────────────────┐      │
                │ NeedsModel           │      │
                │ fetch / import       │      │
                └──────────┬───────────┘      │
                           │ ready            │
                           └──────────────────┤
                                              ▼
                              ┌────────────────────────────┐
                              │ Working                    │
                              │ progress + live segments   │
                              └─────┬────────────────┬─────┘
                                    │                │
                              ok    │                │ failed
                                    ▼                ▼
                        ┌────────────────────┐  ┌──────────────────┐
                        │ Done               │  │ Failed           │
                        │ transcript, export │  │ reason + action  │
                        └────────────────────┘  └──────────────────┘
```

`NeedsModel` is the only screen that interrupts the one-step path, and it is
unavoidable: a 228 MB model cannot ship inside a "lightweight" binary. It is
also the first place the deferred *manual import* fallback from P1a becomes
meaningful — the user can point at a folder they already have instead of
downloading.

### State

```rust
enum Screen {
    Empty,

    /// Blocking: no usable model. Reuses verse-model's own state machine,
    /// so the UI does not define a second, drifting copy of it.
    NeedsModel { input, model, download },

    /// Decoding, loading, or recognising. `stopping` is set while a
    /// cancellation is being honoured.
    Working { input, position, fraction, segments, stopping },

    Done { input, transcript, exported },

    Failed { input, reason, recovery },
}
```

`Working` carries the segments recognised so far, not just a count — that is
what P5 asks for, and it is why the pipeline emits `TranscriptSegment` as it
goes rather than only at the end.

**No phase field.** An earlier revision of this document had one, to label
decoding apart from recognition. The event vocabulary carries no stage event,
and adding one to `verse-core` to drive a label the user can act on in no way
is not a good trade. `Working::has_output()` derives the same distinction from
whether a first segment has arrived, which gives the two labels that were
actually needed.

**Cancelling does not return to the drop target at once.** It sets `stopping`
and waits for `JobCancelled`. Returning early would show an idle screen while
the job was still winding down — both a lie and an invitation to start a
second job on top of the first. The cost is up to a second of "正在停止…".

## 3. Layout

One window. Minimum 640×480, default 900×640, remembers its size.

```
┌──────────────────────────────────────────────────────┐
│  Verse                          [降级提示]   [关于]  │  48
├──────────────────────────────────────────────────────┤
│                                                      │
│                                                      │
│                  main area                           │  flexible
│                  (screen-dependent)                  │
│                                                      │
│                                                      │
├──────────────────────────────────────────────────────┤
│  status bar — only when Working or Failed            │  40
└──────────────────────────────────────────────────────┘
```

The status bar is reserved rather than created on demand, so content does not
jump when a task starts. It is empty and collapsed when idle.

**The degraded-hardware notice** is a dismissible one-line bar under the
header, not a dialog. `verse-core::hardware` already produces the sentence;
the UI's only job is to show it once. Interrupting startup with a modal about
a workaround the user cannot act on would violate P1.

**The About dialog is a licensing obligation, not a nicety.** The FunASR
licence requires attribution to Alibaba / FunAudioLLM and the retention of the
model name, and the only place a user can see either is here. See §9.

## 4. Design tokens

**Provided by shadcn-svelte, not defined here.** The theme is a set of CSS
variables in `src/app.css` — `--background`, `--foreground`, `--card`,
`--primary`, `--muted`, `--destructive`, `--border`, `--radius` and the rest —
with light values in `:root` and dark in `.dark`, consumed by Tailwind
utilities. Spacing, radii and type scale come from Tailwind. Nothing in the
interface hardcodes a colour.

This replaces an earlier hand-written token file. A component set that ships
its own theme cannot be half-adopted, and keeping a parallel palette would
have meant restating every colour in two places and keeping them in step by
hand.

What we do choose, and where:

| | Value | Where |
|---|---|---|
| Base colour | neutral | preset, baked into `components.json` |
| Accent | blue | same |
| Icon set | lucide | same |
| Radius | 0.625 rem | `--radius` in `app.css` |
| Font | Noto Sans over the system Chinese face | `@theme inline` in `app.css` |

To change any of these, edit the variables in `app.css` directly. The preset
is a starting point, not a lock.

### 4.1 Type

Chinese-first, and the one place the defaults needed correcting.

Body text is **15 px**, not the 13–14 a Latin-only app would use: Chinese
glyphs need more size to reach the same apparent weight, and below about 14 px
dense 汉字 turns into a block of ink. Line height 1.6, for the same reason.

The font stack is two fonts, and the split is deliberate:

```
Noto Sans Variable          Latin, digits, punctuation — bundled, 36 kB
Microsoft YaHei UI          Chinese — from the system
PingFang SC / Noto Sans CJK SC    the same, on macOS and Linux
```

shadcn installs Noto Sans as a webfont, and it carries no CJK glyphs. Naming
the Chinese faces after it is what decides which one renders 汉字 — without
the list the outcome is whatever the platform picks, and 微软雅黑 and 宋体 are
both plausible and look nothing alike.

Full-width punctuation (，。「」) sits outside the Latin ranges and so falls
through to the Chinese face, which draws it better anyway.

**Only the Latin subset of the webfont is loaded.** The package's entry point
drags in eight subsets — Devanagari, two Cyrillic blocks, two Greek,
Vietnamese, Latin extended — for roughly 400 kB of glyphs this application
will never draw. `app.css` points at the single file instead: 440 kB becomes
36 kB.

## 5. Components

**shadcn-svelte, copied into the project.** `src/lib/components/ui/` holds
source rather than a dependency, so these are ours to edit and there is no
upstream to wait on. Eight were taken, each earning its place by being needed
on two or more screens:

| Component | Used by |
|---|---|
| `Button` | every screen |
| `Card` | empty, model, working, done |
| `Dialog` | 关于 |
| `Progress` | model download, transcribing |
| `ScrollArea` | transcript |
| `Separator` | done, about |
| `Alert` | failed, degraded-hardware notice |
| `Sonner` | export confirmation, background failures |

Two stay hand-written, because no component set has an opinion about them:

**`DropZone`** — the dashed drop target, in three states: idle, hover, and
active while a file is over it. It is the first thing the user meets and the
entire first step of the interface, which is worth more than a styled `Card`.

**`TranscriptView`** — a scroller of `[timecode] text` with auto-follow:
during `Working` new segments scroll into view, but the moment the user
scrolls up it stops. Losing that is a small thing that makes an application
feel hostile, and no general-purpose list does it.

## 6. Architecture

```
crates/verse-app/
  src/
    main.rs         entry point; hides the console window in release
    lib.rs          Tauri builder, and the commands the frontend calls
    state.rs        AppState, Screen, Working, Done, Failed, Effect — plain Rust
    bridge.rs       worker thread, EventBus subscription, updates out
  ui/               Svelte 5 + TypeScript, built by Vite
    src/
      App.svelte
      lib/api.ts      the only file that calls into Rust
      lib/tokens.css  design tokens (§4)
    vite.config.ts    reads its port from tauri.conf.json — see §6.1
  tauri.conf.json     window, bundle, and the dev-server address
  icons/              generated by tools/make-icon.mjs, never hand-drawn
```

The rule that makes this testable: **`state.rs` never mentions a window, a
webview, or a framework.** It is ordinary Rust over ordinary data, and the
whole state machine is covered without opening anything.

That rule has already paid once. This design started on a native toolkit and
moved to a web frontend, and `state.rs` crossed unchanged — but only because
nothing framework-shaped had been allowed into it.

### Where the state lives

**Rust owns it.** `AppState` is the authority; the frontend holds a mirror it
renders and decides nothing. That is what keeps the 23 tests in `state.rs`
meaningful — they exercise the logic that actually runs, rather than a
TypeScript reimplementation of it that would drift.

The mirror is fed by **updates, not snapshots**. A two-hour recording is a few
thousand segments, and re-sending the list on every progress tick would push
megabytes across the IPC boundary to change one number. So what crosses is a
small enum — the screen changed, a segment arrived, progress moved — and the
frontend applies each to its own copy.

### The bridge

```
worker thread                          webview
─────────────                          ───────
run pipeline                           apply(update) to the mirror
     │ publish                              ▲
     ▼                                      │
  EventBus ──drain──► AppState ──emit("verse://update", …)
```

**Events, not direct UI calls.** The worker publishes to the `EventBus`, which
already carries exactly the events the interface needs (§4.6 of `design.md`),
and a loop folds them into `AppState`. An earlier revision had the bridge
translate `Event` into a second, UI-shaped enum; that layer was cut, because
the event vocabulary is already the interface both halves agreed on and a
translation between two identical shapes is only somewhere for them to drift
apart. This is also what lets P2a's subtitle window reuse the bridge as-is.

**Model downloads are the one exception.** They are not pipeline events —
nothing routes them, no job produces them — and `verse_core::ModelState`, the
event that exists for the purpose, cannot express "file 2 of 3, 40 MB of
228 MB, from hf-mirror". The downloader's own `DownloadState` can, and it is
what the model screen reads. That leaves two model-state types in the tree
with one unused; `tasks/p1b-gui.md` step 4 records it as worth consolidating
rather than quietly tolerated.

Cancellation is a `CancelToken` from `verse-core`, cloned into the worker and
triggered by the frontend. No new mechanism.

### 6.1 The dev server address

**`tauri.conf.json` is the only place the port is written down.** Tauri loads
`build.devUrl` in the webview; `vite.config.ts` reads that same field for its
own `server.port`. Two files cannot disagree if only one of them holds the
value.

Three things make this robust rather than merely tidy:

- **`strictPort: true`.** Vite's default is to slide to the next free port
  when the configured one is busy, which would leave the dev server somewhere
  the webview is not looking. That shows up as a blank window, not an error.
- **The host is `127.0.0.1`, not `localhost`.** On Windows `localhost` can
  resolve to `::1` while the server is bound to IPv4, and the webview then
  reaches nothing. An explicit address cannot disagree with itself.
- **`vite.config.ts` throws if it cannot find the field**, rather than
  defaulting to a port that happens to work.

**Port 17321 is the dev server's.** `verse serve` uses **17322**, chosen by
the same rules below and one above the dev server rather than on top of it
(`design.md` §4.11). Both bind `127.0.0.1` explicitly, for the reason given.

**Why 17321.** Chosen to be unremarkable and unused: above 1024 so it needs
no privileges, below 49152 so it is outside the dynamic range Windows hands
out, and not a default of Vite (5173), Tauri's own template (1420), React
(3000), Vue (8080), or anything else likely to be running on a developer's
machine.

This is not a hypothetical precaution. During setup the default 5173 was
already taken by an unrelated Vite instance, and the window attached itself to
that project instead — the failure is silent, and it looks like the app
working.

## 7. Chinese text

This was the largest unverified assumption in the design while the frontend
was a native toolkit: platform font resolution there is opaque, and the
default face has no CJK coverage, so whether the fallback would find a Chinese
font — and which one — was anyone's guess.

**Moving to a web frontend settled it.** Font handling in a browser engine is
a solved, specified problem, and Chromium's fallback chain is thorough. The
family list in `lib/tokens.css` is a preference, not a lifeline:

```css
--font-ui: "Microsoft YaHei UI", "Microsoft YaHei", "PingFang SC",
           "Noto Sans CJK SC", system-ui, sans-serif;
```

The first entry present wins; the rest cover macOS and Linux. Zero bytes, and
every target platform has one of them. An embedded subset would only be needed
if the app had to look identical everywhere, which it does not.

Two things still need eyes rather than reasoning, and both are checked in step
2 of `tasks/p1b-gui.md` before anything is built on top:

- **Which face actually wins**, since 微软雅黑 and 宋体 are both plausible
  outcomes on a Chinese Windows and look nothing alike.
- **Full-width punctuation at 15 px** — the ，。、；： and the 引号 pairs
  render at different optical weights across faces, and a bad one is obvious
  only on screen.

## 8. Copy

Two kinds of text, two places.

**Static labels** live in the `.slint` files as literals — button text, screen
titles, section headings. They change with layout, they are part of the view.

**Computed text** comes from Rust: error messages, progress descriptions,
model names, the hardware notice. It is produced by `state.rs`, which means it
is testable (`assert!(msg.contains("重试"))`) and it keeps message construction
out of the view layer.

Rules for what these strings say:

- Chinese, always. English is a supported *input* language, not a UI language.
- No sentence ends without telling the user what happens next, when the
  sentence is reporting a problem.
- Numbers carry units: `已识别 12 段`, not `12`.

## 9. Licensing obligations the UI must discharge

Recorded here because they are implemented in the interface, and forgetting
one is a distribution problem rather than a cosmetic one.

| Obligation | Source | Where it lands |
|---|---|---|
| Attribution to Alibaba Group / FunAudioLLM | FunASR Model License v1.1 § | 关于 dialog, opened from the header |
| "SenseVoiceSmall" name retained | same | model name shown verbatim in 设置 and the dialog |
| FunASR licence text shipped | same | 关于 dialog links a bundled copy |

**The frontend and shell add nothing to this list.** Tauri is MIT/Apache-2.0
and Svelte is MIT, both the same terms as this project. An earlier revision of
this design used a GUI toolkit whose licence required shipping an attribution
widget — that obligation, and the dialog requirement it created, went away
with the toolkit.

`THIRD_PARTY_NOTICES.md` is the authority; this table is a pointer to it.

## 10. Not in P1b

Stated so they do not creep in:

- Settings screen. Configurable values exist in the pipeline already; exposing
  them is a later decision, and the default path must not need them.
- Batch / queue of multiple files. One file at a time.
- Transcript editing. The result is exported, not edited.
- Live subtitles, translation, recording.
- Custom title bar, tray icon, global hotkey — the latter is out of the
  project entirely (see `design.md` §2).

## 11. Open questions

1. **Is the WebView2 floor acceptable?** It puts the supported *operating
   system* at Windows 10 1803 while the pipeline is happy on 2013 hardware.
   That gap is a product decision rather than a technical one, and it is
   recorded in `THIRD_PARTY_NOTICES.md`. Decide before shipping, and say so in
   the installer instead of failing at launch.
2. **Does the transcript stay smooth with 2000+ segments?** A two-hour
   recording is a few thousand rows, which is where a plain list starts to
   cost more than it should. Windowed rendering is the answer if it does.
   Deferred until there is a real long file, because guessing the threshold is
   how the wrong amount of machinery gets built.
3. **How much memory does the webview baseline cost?** WebView2 is
   multi-process; an empty window is tens of megabytes before the app does
   anything. It does not threaten the no-leak constraint — Chromium is not the
   leaky component here — but it is a real step back from a native toolkit on
   the "lightweight" goal. Worth measuring rather than assuming, once there is
   something worth measuring.
4. **Where does the model directory live in a real install?** Currently
   `models/` beside the binary. For an installed app it should be per-user
   application data. Belongs with packaging, not with P1b, but the interface
   must not hardcode a path that packaging will change. (Raised while writing
   §6.1: the same question applies to the dev-server address, which is why
   that one has a single source.)

   **Half answered, 2026-10-06.** The *mechanism* now exists: `verse-store`
   resolves a per-user data directory (`VERSE_CACHE` → `%LOCALAPPDATA%\Verse` →
   `~/.verse`) and the result cache, the resume logs and the output record all
   live under it. Models do not: they still resolve beside the executable, via
   `VERSE_MODELS` and then `current_exe()`. So the question has gone from "we
   have nowhere to put per-user files" to "the models are the one thing not
   using the place that exists", which is a smaller and more concrete job — a
   single resolution function, and a migration for anyone who already has
   weights in the old location.

   Worth doing before packaging, and not before: moving 1.2 GB of models on an
   existing install is a real cost to impose for a tidiness that only matters
   once there is an installer.
