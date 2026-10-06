# The main line: where results go, and not doing the same work twice

The product says "drop any audio in, get text out" with zero configuration
(`design.md` C6). An audit of the main line found half of it missing.

| part | before this round |
|---|---|
| Accept any audio | done — ffmpeg streams anything; **no temp file at all** |
| Automatic processing | done, except a missing model stops and does not fetch itself |
| Result goes somewhere | CLI beside the input; **GUI has no default, and an unexported transcript is destroyed when the next file starts** |
| Caching and deletion | **did not exist** |

"缓存 / 临时文件 / cleanup" appeared **nowhere** in `design.md` or
`ui-design.md`. This is not finishing a half-built feature — it is designing one.

Decisions taken with the maintainer:

1. A **result cache** keyed by audio content, so re-transcribing is instant.
2. **Model cleanup** — stale `.part` files, remove a model, report sizes.
3. **Resume**, so an interrupted long file does not redo the recognition.
4. **Automatic saving** on both interfaces to `<Documents>/Verse`; the GUI
   dialog becomes 另存为. **The CLI default changes too** — a breaking change
   to a published contract, handled head-on in step 6.
5. **A decoded-PCM cache is cut.** The result cache subsumes it: it only helps
   when re-running with a *different* engine, which is evaluation work, and
   `verse-bench` exists for that. It would cost ~1.4 GB per two hours of audio
   with its own eviction problem. Recorded in `design.md` so it is not
   re-proposed as new.

## The constraint that shapes the design

A cache hit must **republish every segment**, not just the final transcript.
Verified before designing: the window builds its segment list from
`Event::TranscriptSegment` (`state.rs:375` → `bridge.rs:273` → `Update::Segment`
→ `App.svelte`), and `TranscriptFinal` (`state.rs:391`) only finalises. A hit
that published only `TranscriptFinal` would render **"没有识别到内容"** while
reporting success — the silent-failure class this project has been bitten by
twice.

## [ ] 1. `verse-store`: per-user directories, output naming

A new leaf crate. It owns "where data lives on this machine" and nothing else:
directory resolution, the result cache, resume checkpoints, output naming.

Resolution chain: `VERSE_OUTPUT` → `dirs::document_dir()/Verse` →
`home/Documents/Verse` → `data_local_dir/Verse/output`.

OneDrive redirection is **followed but detected and warned about once**. The
program makes no network call, but the user's own sync client will upload
whatever lands there, and that is a thing they should be told rather than
discover.

Collisions: same source overwrites idempotently; a different source gets
` (2)`, ` (3)`; a file with no ownership record is **never** overwritten,
because silently replacing another recording's transcript is the failure being
prevented.

**Verify:** tests for the fallback chain, for idempotent same-source overwrite,
for a serial suffix on a different source, and that a foreign file is never
overwritten. A scratch-directory transcript showing `meeting.srt` then
`meeting (2).srt`.

## [ ] 2. `verse-store`: the result cache

Key = canonical path + size + nanosecond mtime + **SHA-256 over a bounded
512 KiB window** (first 256 KiB + last 256 KiB) + a full settings fingerprint
(engine, model files, VAD model and settings, ITN, token budget, guard
settings, hotwords, threads, `ffmpeg -version`, and a hand-bumped
`PIPELINE_SEMANTICS`).

Reading 2 GB per run is not acceptable, so the digest is bounded. The residual
false hit requires path, size, nanosecond mtime *and* both end-windows to match
while the middle differs — realistically only a timestamp-preserving restore.
`VERSE_CACHE_VERIFY=full` hashes the whole file for a caller who wants
certainty.

**Threads are in the key** although it costs cross-mode hits: ONNX reduction
order is not thread-count-invariant, and a false hit is the disaster this is
built to avoid.

Writes are atomic (`.tmp` then rename, never read back), versioned
(`cache/v1/`), and a corrupt or unreadable entry is a **miss, not an error** —
a cache is never allowed to break the product.

**Verify:** key stability; a changed mtime, setting, or model file each bust
the key; a truncated edit is caught by the bounded digest; a corrupt entry is a
miss; a `.tmp` file is never read.

## [ ] 3. The pipeline consults the cache

`Request` gains the policy; `Transcription` gains `cached`. On a hit publish
`JobStarted` → every `TranscriptSegment` → `TranscriptFinal` → `JobFinished`.

`verse-bench` disables the cache explicitly. A cached benchmark would measure
nothing, which is this project's recorded "comparing two things that are not
comparable" mistake.

**Verify:** an event-order test asserting one segment per cached segment; a CLI
run twice showing the second marked `cached` with a much smaller `elapsedMs`.

## [ ] 4. Resume checkpoints

Per-span identity: `(start, sample_count, sha256(span samples)) -> [segments]`.
`recognize` already resets the engine per span (`lib.rs:274-302`), so spans are
independent — that is what makes per-span reuse valid rather than hopeful.

On resume the file is re-decoded and re-segmented from scratch; **only the ASR
is skipped**. Reused segments are republished in order, so a resumed run looks
identical to an uninterrupted one. Namespaced by pass kind, so a
guard-triggered `FixedBlocks` pass can never cross with the first.

**The failure mode is always redoing work, never fabricating it.** A span whose
digest does not match is simply recognised again.

**Verify:** interrupt a long file mid-way and re-run; `diff` the resumed `.srt`
against an uninterrupted one prints nothing. A digest mismatch must re-run ASR.
Wall time recorded for both.

## [ ] 5. Model cleanup

`stale_partials`, `remove_model` (canonicalised, and required to be inside the
models root), `usage`. `verse model` gains `remove` and `clean`; `model list`
gains sizes; a `cache clean|size` command.

`.part` files are deleted **only by explicit command**. The downloader keeps
them on failure deliberately, for resume (`downloader.rs:77-79`); deleting them
at startup would silently defeat that.

**Verify:** a planted `.part` is listed and removed; a completed file is never
mistaken for a partial; removal refuses to escape the models root.

## [ ] 6. CLI default output, and the contract fallout

`resolve_outputs` takes the resolved default directory. `-o -` and an explicit
`-o` are unchanged. The `a.wav`/`a.mp3` refusal becomes a serial suffix, and
**the CLI says so** when it serialises — a changed output name must not be
silent.

`FileResult` gains `cached`. Additive, so `VERSION` stays 1, and the rule is
written down rather than assumed.

`llms.txt`, `README.md`, `transcribe_usage()` and the existing
`resolve_outputs` tests all move in this commit — a doc that still describes
the old default is worse than no doc.

**Verify:** `VERSE_OUTPUT=$TMP verse transcribe zh.wav` lands in `$TMP`;
re-running creates no `(2)`; a second source with the same stem creates
`zh (2).srt`; `-o -` still writes to stdout. A test asserts the usage text
names the default directory, so help cannot drift from the code.

## [ ] 7. GUI: automatic saving

Auto-save when a `TranscriptFinal` produces `Done`, **on the forwarder thread**
so the ordering against the event is guaranteed — on the worker thread it would
race the forwarder and be cleared again. The file write happens outside the
state lock. `ScreenView::Done` gains `saveError`; the dialog becomes 另存为 and
defaults to the auto-saved path; a write failure shows its reason while save-as
still works.

**Verify:** `cargo test -p verse-app` contract tests pin `saveError` on the
wire. Manual, recorded: drop a file, a transcript lands in `<Documents>/Verse`
with no dialog; re-drop it and confirm no `(2)`; point `VERSE_OUTPUT` at a
read-only directory and confirm the error is visible and 另存为 still saves.

## [ ] 8. Consolidation

`design.md`, this log (all steps `[x]` with their evidence), `changelog.md`,
`llms.txt`, `README.md`. Every flag named in the docs checked against
`verse --help`.

**Verify:** `cargo test --workspace` and
`cargo clippy --workspace --all-targets` both clean, and the old
default-output sentence gone from every doc.

---

## Not in this round

- **Decoded-PCM cache** — cut, recorded above.
- **A `probe` API to skip the model load on a hit.** A cache hit still pays
  `Transcriber::load`, so it saves the recognition and not the 228 MB model
  read. Amortised across a batch, visible on a single short file. Worth doing
  only if a measurement says the load dominates.
- **Cache eviction or a size cap.** `cache clean` and `cache size` are the
  explicit tools; no automatic eviction until there is evidence it is needed.
- **A GUI for model removal.** Deleting a model while the window is downloading
  one is a race worth designing on its own rather than bolting on here.
