# The four backend pieces the window is missing

`tasks/p1b-gui.md` steps 10–13 were deferred when the agent-facing CLI was
built. This is that work coming due.

The frontend is not the blocker. `App.svelte` already branches on all five
screens and renders them; what is missing is that three of them lead nowhere.
`NeedsModel` offers only "返回" — a dead end — and `Done` has no way to write
the transcript out. Each hole is a missing **command**, not missing markup.

| screen | frontend | backend before this |
|---|---|---|
| Empty | complete | complete |
| Working | complete | complete |
| Done | shows the text, footer has only "再来一个" | no write path, no save permission |
| Failed | reason + retry | works, but the recovery variants are coarse |
| NeedsModel | **only a back button** | no command, no progress events |
| About | does not exist | no content served |

Evidence for the backend half: `Effect::FetchModel` is produced by
`state.rs:259` and handled nowhere in `lib.rs`, which matches only
`Effect::Transcribe`; `capabilities/default.json` grants `dialog:allow-open`
and not `dialog:allow-save`; and nothing in `verse-app` writes a file —
`verse-core::export` renders to a string and stops there.

---

## [x] 1. Export

A command that renders the finished transcript and writes it, plus the
permission that lets the window ask where.

The window chooses the path, not the backend: `@tauri-apps/plugin-dialog` is
already a frontend dependency and its `save()` returns a path, whereas doing
it in Rust means a callback-shaped dialog API threaded through a command that
otherwise returns immediately.

**Done.** `export(app, path)` renders the finished transcript, writes it, and
records where it went. `dialog:allow-save` joined the capability file — without
it the window cannot ask where to put anything, which is why this screen had a
transcript and no way out of it.

Format comes from the extension and an unrecognised one is refused rather than
guessed: writing SRT into a file someone named `.txt` is worse than saying so.

`ScreenView::Done` gained `exported`, so the window can stop offering an export
that has already happened — the reason `note_exported` was written in the first
place and had no caller until now. Both new shapes have contract tests,
including that `null` and a path are distinguishable.

**Verified:** 2 new tests — that a finished screen is the only one with
something to export, and that `null` and a path are distinguishable on the
wire. The click-through — export, then open the file in a player — is step 4,
and needs a person.

## [x] 2. The model screen

Two things it cannot do today: download, and accept a folder the user already
has.

**Download.** `Effect::FetchModel` gets a handler, `Downloader::fetch`'s
callback moves the state machine *and* emits byte progress, and on `Ready` the
job that was waiting starts on its own — which is what `NeedsModel { input }`
has been carrying the path for since it was written.

**Import.** A folder the user points at is checked against the catalogue's file
list and copied into `models/<id>/`. Copying rather than referencing, because
the pipeline looks in one place and a second lookup path is a second thing to
get wrong.

**Done.** `fetch_model` runs the download on its own thread, moves the state
machine *and* emits `Update::Download` with bytes as it goes, and on `Ready`
starts the job that was waiting — what `NeedsModel { input }` has been carrying
the path for since it was written. `import_model` validates a folder the user
points at and copies it into `models/<id>/`.

**Copying rather than referencing.** The pipeline looks in one place; a second
lookup path is a second thing that can disagree with it.

**Two layouts accepted**, because both are what people end up with after
unpacking a download: the folder *is* the model directory, or it contains one
named after the model. Extracted as `model_root` and tested, including the
nested `tokenizer/` case — the same shape whose absence silently broke the
downloader in the previous round. A folder missing a file is refused with the
file named.

**How progress reaches the window, and why not through the bus.** `verse-core`
already has an unused `Event::ModelStateChanged`, which was the obvious
channel, but its `ModelState` carries no byte counts and `verse_model`'s
`DownloadState` cannot be named from `verse-core`. Routing through the bus
would therefore mean two download vocabularies and a lossy round-trip between
them. The app layer owns the download — it is not pipeline output — and
`lib.rs` already emits `Update::Cleared` directly, so this follows the
established shape instead of inventing a second one. A `ModelState` that
carried progress was written and reverted; it had no user.

**Verified:** 4 tests on the folder resolver. A download that actually runs,
and a folder that actually gets copied, are step 4.

## [x] 3. About

The attribution the model licence requires: Alibaba / FunAudioLLM, the verbatim
name "SenseVoiceSmall", and the licence.

**Recorded rather than solved:** the obligation is to *ship the licence text*,
and it cannot be discharged here. The licence file beside the model on
hf-mirror is 71 bytes reading "Ref to https://github.com/modelscope/FunASR",
and GitHub is not reachable from mainland China — so neither this machine nor
an installed copy can fetch the text. `THIRD_PARTY_NOTICES.md` already carries
the checklist item; the dialog states the attribution and names the licence
without pretending to include it.

**Done, with the gap stated in the interface rather than hidden.**
`crates/verse-app/src/about.rs` holds the attribution rows; `about()` serves
them. Four tests hold the required ones in place, including that
"SenseVoiceSmall" is spelled exactly as upstream spells it — the licence
requires the name be retained, and a tidied-up rendering would not be it.

The dialog also says the licence text is not bundled, because it is not.

**Verified:** the rows are asserted present and the naming is asserted exact.
That the dialog renders them is step 4.

## [ ] 4. End to end

The P1b exit criterion, run as a person would.

**What is verified so far.** All nine commands are registered, the frontend
type-checks against them, the bundle builds, and the window opens with an empty
log — no panic, no missing asset.

**What is not, and cannot be from here.** Every one of these screens ends in a
click, and a click is the thing an agent cannot make: choosing a save location
in a native dialog, watching a progress bar fill, pointing at a folder. The
backend logic underneath each one is unit-tested, but "the button works" is a
claim that needs a person at the window.

**Verify:** model present, no terminal touched — open Verse, drop a Chinese
audio file, watch it transcribe, export an SRT, open it in a player. Then again
with the model deleted, to exercise the download path.
