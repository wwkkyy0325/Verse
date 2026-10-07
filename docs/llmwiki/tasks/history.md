# Yesterday's transcripts, after the window is closed

The file list is session-only. Close the window and everything it knew is gone,
while the `.srt` files it wrote are still in `Documents/Verse` — the results
survive and the record of them does not. Somebody who transcribed a meeting last
week has no way to find it from inside the application.

## What the answer is not

**A separate SRT viewer.** The window already has a reading pane — the one the
live transcript fills. A restored transcript goes into that same pane, because
one view is one thing to keep working and two views are two. An "SRT viewer" as
a distinct surface would also invite the question of what it is *for*: the pane
already shows timecoded lines, which is what an SRT is.

**A scan of the output folder.** Listing every `.srt` in `Documents/Verse` would
find files this program wrote, files the command line wrote, and files somebody
put there by hand — and would say nothing about which recording each came from
beyond the file's own name. It answers "what is in this folder", which is a
question the file manager already answers.

## What it is

**The list the window already keeps, persisted.** The roster is the record:
which file, which engine, when it finished, where the result went. Writing it
down and reading it back is the smallest thing that makes the results findable
again, and it is the same list rather than a second one.

An entry whose `.srt` has been deleted since is dropped on load — the record is
a pointer, and a pointer to nothing is not history.

**And reading the result back, rather than storing a second copy of it.** The
`.srt` on disk is the result. Restoring a transcript means parsing it, not
keeping a parallel transcript that can drift from the file it duplicates. That
needs the inverse of `verse_core::export::render`, which is the right place for
it: one module that knows the SRT format, in both directions.

## Design

```rust
// crates/verse-store/src/history.rs
pub struct History { version: u32, entries: Vec<Past> }

pub struct Past {
    pub input: PathBuf,
    pub output: PathBuf,
    pub engine: String,
    pub finished_at_ms: u64,
}
```

Atomic write through a `.tmp` and a rename, as `Ownership` does. Capped, because
a record that grows without limit is a leak with a friendlier name.

**Verify:** a written history reads back; a partial write leaves the previous
one intact; an entry whose output is gone is dropped; the cap holds.

## Stages

### [x] 1. Remove the extractive summary

It was measured and judged not worth having (`tasks/llm-summary.md`). Removed
rather than kept as a consolation: `verse-core::summary`, the `summary` command,
`SummaryView`, and the panel — with the reading that its tests were pinning
behaviour nobody wants anymore.

**Verify:** `cargo test --workspace` green with the module gone; the window
still type-checks; nothing references `summary` outside this task log and the
changelog.

### [x] 2. Read an SRT back

`verse_core::export::parse` — the inverse of `render`, in the same module so the
two cannot disagree about the format.

**Verify:** render → parse → render is the same document; a file that is not an
SRT is refused rather than half-read; an empty transcript round-trips.

### [x] 3. Write the history down

`verse-store::history`, written when a transcript finishes and its result
exists.

**Verify:** as above.

### [x] 4. Restore it into the list

On mount the window loads the history and seeds the roster with the entries
whose results are still on disk. Clicking one reads and parses its `.srt` and
puts the transcript in the pane.

**Verify:** the window type-checks and builds. **The restoring itself needs a
person** — close the window, open it, and the list is there.

## What this does not do

- **Re-run a restored entry.** It has already been done; the result is what is
  on disk.
- **Watch the output folder for changes.** A `.srt` deleted while the window is
  open leaves a stale row until the next launch, which is a smaller problem than
  a filesystem watcher.
- **`outputs.json`.** It stays as it is — the automatic save's own record of
  where it put things, for naming. The history is a different question and does
  not try to replace it. That the `export` command never writes it is still the
  latent bug it was.

---

## Added after the maintainer looked at it

- **Show the result in the file manager**, from the finished bar.
- **Filter by name**, appearing once the list is past six rows.
- **Remove a row** (reversible — the transcript stays) and **delete the result**
  (irreversible, behind a confirmation), kept as two acts because they are two
  different sentences.

## Still open, and it is a bug rather than a missing feature

**A file already in the list cannot be transcribed again.** `file_chosen`
answers a re-drop by selecting the row that file already has, which was the
maintainer's own instruction — *"re-doing a file that is already here should be
skipped"* — and correct while the list lasted one session. With history it is
permanent: a recording that has been re-cut shows its old transcript for ever.

Not fixed in this round because the maintainer ranked it below the three above.
It is recorded because a rule that was right for a session-long list is not right
for a permanent one, and the next person to hit it should find this paragraph
rather than a puzzle.
