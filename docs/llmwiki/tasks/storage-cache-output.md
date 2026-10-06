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

## [x] 1. `verse-store`: per-user directories, output naming

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

**Done.** 25 tests. `cargo run -p verse-store --example naming` prints the run
rather than asserting it — two recordings called `会议.m4a` from different
months, written twice:

```
writing into an empty folder:
  january   -> 会议.srt
  february  -> 会议 (2).srt

running both again, unchanged:
  january   -> 会议.srt
  february  -> 会议 (2).srt
  folder now holds 2 files: ["会议 (2).srt", "会议.srt"]
  january's text is still january's transcript

a file that appeared without a record:
  march     -> 手写 (2).srt
  the hand-written file still says: typed by hand
```

Two runs leave two files, not four. The hand-written file is untouched and the
new transcript goes beside it.

**A bug found by review before it could ship.** The number on a reused filename
is read back off the path — the record stays a plain map and cannot disagree
with the filesystem about what a file is called. That parse stripped a trailing
`)` from the *whole* filename, which ends in `.srt`, so it returned `None` for
every numbered file: a re-run would have reported a file as unnumbered. Fixed
by stripping the extension first, and the test that pins it was **checked to
bite** — restored to the old expression it fails with `left: None, right:
Some(2)`, rather than being assumed to.

The dependency claim in the manifest was verified rather than asserted:
`git diff Cargo.lock` adds exactly ten lines and the only new package is
`verse-store` itself. `sha2` and `dirs` were already in the lock, so they add
manifest lines and nothing to build.

## [x] 2. `verse-store`: the result cache

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

**Done.** 30 tests, and the fingerprint is described by the caller rather than
computed here, so `verse-store` still depends on no workspace crate.

Timestamps are stored as seconds-and-nanoseconds, not milliseconds. The CLI's
report rounds to milliseconds because a person reads it; an entry is *replayed*
into a `Duration` and handed on as though the work had just been done, so a
rounding there would make a cached transcript quietly differ from a computed
one. A test round-trips sub-millisecond offsets to pin it.

**The documented limit is asserted, not described.** `the_bounded_digest_
misses_a_changed_middle_and_full_catches_it` writes a 700 KiB file, changes one
byte in the middle, and asserts the bounded digest is *unchanged* while the
full one is not. If that test ever starts failing, the window grew and the doc
comment above `Verify` is now wrong.

**A test premise was wrong and the test said so.** `two_floats_that_print_
alike_do_not_share_a_key` failed: Rust's `Display` for floats prints the
shortest string that round-trips, so two different values never render alike
and the mechanism I had written into the doc comment was not real. The comment
now says what is true — bits are used because shortest-round-trip is a choice
the standard library makes and not a contract this crate can hold it to, and a
rendering rounded for readability would collapse two thresholds into one
string. The test now asserts that rounding premise explicitly before relying
on it.

## [x] 3. The pipeline consults the cache

`Request` gains the policy; `Transcription` gains `cached`. On a hit publish
`JobStarted` → every `TranscriptSegment` → `TranscriptFinal` → `JobFinished`.

`verse-bench` disables the cache explicitly. A cached benchmark would measure
nothing, which is this project's recorded "comparing two things that are not
comparable" mistake.

**Verify:** an event-order test asserting one segment per cached segment; a CLI
run twice showing the second marked `cached` with a much smaller `elapsedMs`.

**Done.** Four tests on the announcement, and the end-to-end run:

```
run 1 (cold)   cached = False   elapsedMs = 307
run 2 (warm)   cached = True    elapsedMs = 0
```

The outputs of the cold and warm runs are byte-identical, and so are the runs
under `--no-cache` and `VERSE_NO_CACHE=1` — the cache changes the time and
nothing else.

**The escape hatches work, and the empty string is not one of them.** `--no-cache`
and `VERSE_NO_CACHE=1` both report `cached = false` against a warm cache, while
`VERSE_NO_CACHE=` (set but empty) reports `cached = true`, because an empty
value means unset — the same rule the directory resolution applies.

**A one-byte change is caught.** In a copy of the sample, one byte was flipped
with the length left alone: the next run reported `cached = false`. The bounded
digest is doing its job on a small file, where it reads the whole thing.

The first attempt at that test silently did nothing — the byte flip ran in a
Python subprocess, which does not translate `/tmp` the way the shell does, so
the file was never touched and the cache correctly reported a hit for an
unchanged file. The result looked like a failure of the digest and was a
failure of the test. Second attempt passes a Windows path.

**A correction to what this buys.** `elapsedMs` measures the transcription and
not the model load, and `Transcriber::load` still runs on a hit — so the saving
here is 307 ms of a ~1.4 s run on a five-second clip, where the 228 MB model
load dominates. For a long file the ASR is the bulk and the cache takes nearly
all of it. The `probe` API that would skip the load is in "not in this round"
below, and this measurement is the reason it stays there: on short files the
load is the cost, and on long ones it is already amortised.

`verse-bench` passes `CachePolicy::Disabled`, deliberately and without a flag:
a cached benchmark measures the cache, and comparing that against a real run
would be the "two things that are not comparable" mistake this project has
already paid for once.

## [x] 4. Resume checkpoints

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

**Done, and the end-to-end caught a bug that nothing else would have.**

Measured on a 257-second file (the sample repeated 46 times), 27 spans:

| run | spans reused | wall time | output |
|---|---|---|---|
| uninterrupted | 0 | 9.5 s | — |
| killed at 4 s, then resumed | 11 of 27 | 7.6 s | identical |
| killed at 7.5 s, then resumed | 20 of 27 | 4.6 s | identical |

The checkpoint is written after each span and removed when the run completes.
Both resumed outputs are `diff`-identical to the uninterrupted run, so the
saving is time and nothing else.

**The bug: `recognize` collected the segments it produced and never stored
them.** The call that writes the checkpoint was missing, so every log stayed
empty and resume did nothing — while the code read as though it worked. Clippy
had nothing to say, because a `Vec` that is only pushed to counts as used, and
the file *was* created, so even looking for it proved nothing. Only running an
interrupted job and finding zero spans exposed it.

Two tests now pin it: `recognising_a_span_records_it_for_a_later_attempt` and
`a_second_attempt_reuses_the_span_without_asking_the_engine`, the second driving
an engine that has nothing to say so the text can only have come from the log.
**Both were checked to bite** — removing the record call again makes the first
fail.

**Two design points worth stating.** The resume log is tied to the cache
switch: `--no-cache` and `VERSE_NO_CACHE` mean "reuse nothing", and reading a
checkpoint is reusing. And the checkpoint is namespaced by pass, so the
guard's whole-file second pass can never pick up the detector's first-pass
spans — they are different segmentations and splicing them would produce a
transcript that never existed.

A span is identified by its own samples rather than by its position, so a
segmenter that drifted by a few milliseconds between runs simply produces a
miss and the span is recognised again. The failure mode is redoing work, never
inventing it.

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
