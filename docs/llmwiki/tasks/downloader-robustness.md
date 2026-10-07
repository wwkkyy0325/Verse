# Four things the downloader was missing

An audit against the question "is this good enough": resume, concurrency, hash
verification, and the various ways a download goes wrong.

## What was already there

Worth writing down, because the answer is "more than expected" and the gaps are
only visible against it:

- **Resume**, with the classic bug handled — a mirror that ignores `Range`
  answers 200 instead of 206, and the partial is **discarded rather than
  appended to**.
- **Atomic promotion**: `<name>.part` → `sync_all` → `rename`, so an
  interrupted transfer never looks complete.
- **Truncation refused before promotion**, with the file retried elsewhere.
- **Mirror rotation**, every failure reported in one message.
- **Cancellation** in the read loop, leaving the partial for next time.
- **Connect and response timeouts**, aimed at a mirror that accepts a
  connection and then says nothing.

## What was not

1. **No hash verification.** Integrity was the file's *length*. A corrupt file
   of the right size passed, was renamed into place, and `is_present` — which
   also only checks length — never looked again.
2. **A mirror was tried once.** With two mirrors that is survivable; `qwen3-asr`
   has **one**, so one flaky moment was terminal.
3. **No free-space check.** 987 MB against 200 MB free failed partway through
   with an I/O error instead of before it started.
4. **Files sequentially.** They are independent of each other.

---

## [x] 1. SHA-256, computed while streaming

`ModelFile.sha256: Option<String>` (`#[serde(default)]`, so version 1 stays
valid). The digest is fed from the same buffer that is written to disk, so no
second read.

A mismatch fails the file **and deletes the partial**, because a corrupt partial
cannot be resumed — resuming would keep the corruption.

**Populating it is the hard half, not the code.** The mirrors are hand-written
and one is a third-party upload. The hashes are computed from the copies already
on disk and recorded with that provenance, which pins *the bytes we have* rather
than *the bytes upstream published*. Recorded as such rather than implied.

**What this does not cover**: a file already on disk is still trusted by length,
because hashing 987 MB on every check is not affordable. The hash is checked
when the bytes pass through, and that is all.

**Verify:** hashing a file gives the same digest as `sha256sum`; a file whose
digest does not match is refused and its partial removed; a model with no
recorded hash still downloads.

## [x] 2. Retry a mirror before giving it up

Attempts per mirror with a capped, doubling backoff. **A server that answered
with a client error is not retried** — it will answer the same way — while a
connection that dropped is.

The backoff must be cancellable: somebody who pressed 取消 should not wait out a
sleep.

**Verify:** a mirror that fails twice and then succeeds is used; a 404 is not
retried; cancellation during a backoff returns promptly.

## [x] 3. Look at the disk before starting

Sum the bytes still needed and compare with what is free where the models live.
The failure becomes a sentence before the download rather than an I/O error
in the middle of it.

Needs a way to ask the filesystem, which is a dependency — `sysinfo` is already
in this workspace's graph for the memory probe, so this is a manifest line and
a feature flag rather than a new crate.

**Verify:** a download larger than the free space is refused with the shortfall
named.

## [x] 4. Download files at the same time

They are independent. Several threads, each taking the next missing file, each
with its own `.part`.

**This changes the progress arithmetic, and the change is an improvement.**
`ModelProgress` in `verse-app` currently decides a file is finished by noticing
the name change — which is wrong the moment two files are in flight. It becomes
a map of the latest figure *per file*, summed: a file's entry is replaced rather
than added to, so a retry that restarts from zero corrects itself, and
concurrency needs no special case.

**Verify:** a model whose files download together produces the same totals as
one whose files download one at a time.

---

## Not in this round

- **Many connections for one file.** Splitting one file across connections needs
  seeks to a shared destination and reassembly, and the gain is smaller than
  parallel files: `qwen3-asr`'s largest file is 756 MB of 987.
- **A lock against two processes writing one `.part`.**
- **`Retry-After` on a 429**, which the backoff covers roughly.
- **fsync of the directory** after the rename.
