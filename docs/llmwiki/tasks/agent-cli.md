# Making the CLI drivable by an agent

Follows the plan approved for this round. Motivation: Verse is usable by a
person through the window and by a person through the terminal, and not by an
agent, for three reasons that reading the code made concrete — one file per
invocation, nothing machine-readable, and a single exit code where `ErrorKind`
distinguishes nine failure classes.

Scope: **CLI and backend only.** The four unwired GUI screens are deferred.
Qwen3 is **not** bundled; it stays an on-demand download, and §1 exists because
that decision is only sound if the download works.

---

## [x] 1. The downloader, which the Qwen decision rests on

`Downloader::fetch` created `models/<id>/` and nothing else
(`crates/verse-model/src/downloader.rs`). `try_mirror` then writes each file
beside a `.part` sibling — and three of Qwen3's six entries name a
subdirectory: `tokenizer/vocab.json`, `tokenizer/merges.txt`,
`tokenizer/tokenizer_config.json`. On a machine where
`models/qwen3-asr/tokenizer/` did not already exist, `File::create` failed on a
directory nobody made.

It had never bitten in this checkout because the tokenizer directory was
unpacked by hand. On a user's machine `verse model fetch qwen3-asr` would have
pulled 982 MB and then failed on the last three files.

**Reproduced before fixing.** A catalogue override
(`models/catalog.json`, an existing override point) declaring one model whose
`local` is `sub/dir/model.onnx`, pointed at the 2.3 MB VAD file so the test cost
seconds rather than a gigabyte:

```text
$ verse model fetch nested-test --models /tmp/verse-dl-test/models
error: could not fetch model.onnx from any mirror: modelscope (系统找不到指定的路径。 (os error 3))
```

Nothing landed — not even a partial file.

**Fix:** create the file's parent directory before the transfer starts, rather
than beside the write. Failing before the request means a layout we cannot
create is reported immediately instead of after the download that precedes it.

**Verified after:**

```text
$ verse model fetch nested-test --models /tmp/verse-dl-test/models
ready: .../nested-test
$ find /tmp/verse-dl-test/models -type f
.../nested-test/sub/dir/model.onnx        # 2327524 bytes, complete
```

Regression test `a_nested_local_path_has_its_directory_made` in
`crates/verse-model/src/downloader.rs`: a spec with a nested `local` and a
mirror on a closed loopback port, so the fetch fails at the network step and
the assertion is about the directory existing by then. No network needed.

### Also found while checking the mirrors

Every configured mirror was probed directly. **`silero-vad`'s hf-mirror entry
is dead** — `404` for both `model.onnx` and `silero_vad.onnx`. It is harmless
today only because modelscope is listed first and answers `302`. Not fixed
here; recorded because a failover list with a permanently dead entry is a
failover that has never been tested.

> **Corrected 2026-10-06.** The `404` was my instrument, not the host. The probe
> requested `silero_vad.onnx` — the *local* name — where the catalogue's
> `remote` for that file is `model.onnx`, so it asked for a file that was never
> there. Re-probed with the right path, the entry answers **`401`**, and so does
> upstream `huggingface.co`: the repo `csukuangfj/sherpa-onnx-vad-silero-v5-
> 2023-12-25` no longer resolves, which is a different thing from a mirror that
> is down. The conclusion is unchanged; the reason under it was wrong. See the
> changelog entry of the same date.

`qwen3-asr`'s single mirror (hf-mirror, third-party upload) answers `302`, so
the on-demand path is viable.

## [x] 2. Batch input, one model load

`TranscribeOptions.inputs` is now a `Vec<PathBuf>`. A directory expands
recursively to the files beneath it, **sorted and deduplicated** so the same
command produces the same order and the same report. A file named explicitly
is never filtered by extension — only directory expansion filters, because
naming a file is an instruction and the decoder is the only thing that can
decide whether it is readable.

Output resolution keeps the single-input rules exactly as they were, and adds
what several inputs force: `-o` may only name a directory. Two guards that did
not exist before, both raised before the model is loaded rather than after:
`-o out.srt` with several inputs is refused instead of silently becoming
`out.srt/a.srt`, and two inputs that would share one output (`a.wav` and
`a.mp3`) are refused rather than auto-suffixed, because an unpredictable file
name is worse for whoever reads the report than being told to rename one.

Failure of one file does not end the batch; `--fail-fast` restores that.

**Verified.** Seven files plus a `notes.txt` plus a nested directory, in one
invocation:

```text
loading sensevoice ...
[1/7] 000001.wav -> 000001.srt (1 segment, 100% kept)
[3/7] 000242.wav -> 000242.srt (1 segment, 38% kept, detector lost it and the file was re-read)
[7/7] 000778.wav -> 000778.srt (1 segment, 22% kept, detector lost it and the file was re-read)
```

`notes.txt` skipped, the nested file found, and **one `loading` line for all
seven**.

**The measurement that justifies the whole step.** Twenty files, same audio,
two ways:

| | wall time |
|---|---|
| one invocation over a directory | **7.3 s** |
| the same twenty files, one process each | 33.3 s |

4.5×, and the gap is entirely the fixed 228 MB model load that the second
approach pays twenty times.

A deliberately corrupt `.wav` among eight: seven outputs written, the bad one
reported, `1 of 8 file(s) failed`, exit 3.

## [x] 3. `--jobs N`

Kept, on the evidence. The work is now one function — `run_one`, which
recognises a single file and describes the outcome — so the sequential and
parallel paths differ only in who calls it and in what order the answers come
back, never in what a file's outcome means. Results are re-sorted into input
order before reporting, so the same command always produces the same document.

`Request` gained `threads: Option<usize>`. Without it four workers would each
take the hardware probe's figure, which is already cores − 1 — eight cores
would have been asked for twenty-eight threads. The budget is divided instead.

**Measured on 60 files:**

| | wall time | peak RSS |
|---|---|---|
| `--jobs 1` | 21 s | 347 MB |
| `--jobs 4` | **10 s** | 1245 MB |

2.1× for 3.6× the memory. Sublinear because the workers contend for the same
cores, but a real gain on a batch worth parallelising, and 1.2 GB is not much
on a machine this project already requires 8 GB of. **Default stays 1** — the
memory cost is real and invisible, so it is opted into rather than paid by
everyone.

**Verified identical:** `diff -r` over the two output directories reports no
difference across all 60 transcripts. Parallelism changes only the order the
work is done in, which is why the results are sorted before they are reported
and never before.

## [x] 4. `--json`

`crates/verse-cli/src/report.rs` holds the wire types. It is in the CLI, not
in `verse-core`, because core has an empty `[dependencies]` and that is
deliberate — the same arrangement `verse-app/src/bridge.rs` uses for the
window, and it keeps the contract next to the only thing that emits it. Two
dependencies are added (`serde`, `serde_json`), both already in this
workspace's build graph.

**One shape for one file and for many.** A single file is a batch of one, so an
agent never branches on how many inputs it passed:

```json
{"version":1,"engine":"sensevoice","modelsDir":"models","hotwords":null,
 "ok":false,"succeeded":7,"failed":1,"elapsedMs":4180,
 "results":[{"input":"...","output":"...","format":"srt","ok":true,
             "elapsedMs":597,"segmentCount":1,"coverage":0.379,
             "recovered":true,"language":null,"text":"...",
             "warnings":[],"segments":[{"startMs":0,"endMs":5026,"text":"..."}],
             "error":null}]}
```

**Every field is always present**, `null` rather than absent. Optional keys are
pleasant to write and miserable to read: the consumer needs a fallback on
every access and can never tell "absent" from "this build does not have it".
A contract test asserts the full key list for that reason.

`coverage` and `recovered` are carried through because they are the two fields
that say whether a transcript can be trusted — a short transcript and a
truncated one look identical without them. `error.kind` is lower case and uses
the same words the exit codes are documented with, so reading the code and
reading the JSON teach the same thing.

**Verified:**

- `stdout` is pure JSON — parsed with a strict reader and no cleanup, so
  anything else on that stream would have failed it.
- Batch and single answer to the same expression: `.results` has 1 element for
  a single file and 8 for a directory.
- A failing file carries `"ok": false`, `"error": {"kind":"decode", ...}`, and
  `"text": null`; `ok`/`succeeded`/`failed` at the top agree. Exit code 3.
- stderr still carries the message explaining *why*, under `--json` as well as
  without it — the report says what failed, only the message says why.
- Six contract tests pin camelCase, milliseconds, the always-present key list,
  and the error-kind vocabulary.

`verse model list --json` too: whether a model is installed is the question an
agent asks before deciding between a transcription and a download, and it
should not require reading a table.

**Note on the verification tooling:** this machine has no `jq`, so the checks
above were run with Python reading the same JSON. Same property, different
reader.

## [x] 5. Exit codes

`Failure { code, message }` replaces the single `ExitCode::FAILURE`, and every
command returns it. The codes separate the failures that call for *different
actions*, not the ones that differ only in wording:

| code | meaning | from |
|---|---|---|
| 0 | everything transcribed | |
| 1 | a bug | `Internal` |
| 2 | the command line was wrong | the CLI itself |
| 3 | the input could not be read or decoded | `Io`, `Decode` |
| 4 | no usable model — run `verse model fetch` | `Model`, `Registry` |
| 5 | the engine failed | `Engine` |
| 6 | network | `Network` |
| 7 | cancelled | `Cancelled` |
| 8 | the output could not be written | `Sink` |

4 exists so an agent can branch to `verse model fetch` without matching on
message text. This is wider than the four codes first sketched; the wider set
costs nothing and each entry maps to a different recovery, which is what a
caller actually branches on.

**Verified** — every code induced deliberately:

```text
unknown option                     2      missing input file                3
--format with no value             2      unknown engine                    4
no input at all                    2      missing model dir                 4
unknown command                    2      -o - with several inputs          2
empty directory                    2      success                           0
```

Eight unit tests cover expansion and output planning: a named file taken as
given, a directory expanded recursively and sorted, an empty directory refused,
and each of the output-planning rules including both collision guards.

## [x] 6. `hotwords`

One parameter threaded down the chain `max_output_tokens` already follows:
`EngineConfig` → `Request` → the factory in `verse-asr/src/lib.rs` →
`OfflineEngine::qwen3` → `OfflineQwen3ASRModelConfig`. Neither `EngineConfig`
nor `Request` derives `Default`, so every literal site had to be updated; that
was the whole cost. No new dependency, no `sherpa-onnx-sys` work — the field
already existed and already reached the C layer.

`EngineDescriptor` gained `supports_hotwords`, for the same reason it already
has `streaming`: a caller has to know *before* it builds the engine. Without
it the CLI would have to name an engine to find out, which is what the
registry exists to prevent.

### The evidence

`000030`, whose reference is `该本王子用那个球拍了。`. Qwen3 renders 球拍
(a racket) as 酒吧 (a bar) — a homophone substitution, which is precisely
what a lexicon should fix:

```text
without --hotwords        本王不用那个酒吧啦。
with    --hotwords 球拍   本王不用那个球拍啦。
```

And `000023`, reference `多练练，就能和我滑得一样好了。`:

```text
without --hotwords        你多练练，就能和我划的一样好了。
with    --hotwords 滑得   你多练练，就能和我滑的一样好了。
```

Both substitutions corrected. **A measured change in output, not a plumbing
claim.**

### The delimiter

Not documented in the Rust binding or the vendored sys crate — it passes a
bare C string and ships no header. So it was determined by trying rather than
assumed:

| form | result |
|---|---|
| `球拍` | works |
| `球拍,羽毛球` | works |
| `球拍，羽毛球` (full width) | works |
| `球拍 羽毛球` | works |
| one per line | works |

The CLI therefore **passes the string through unchanged** rather than
normalising it. Reformatting a grammar we did not define would be a second
place to be wrong, and the harder one to notice. `--hotwords-file` reads the
file verbatim down the same path.

### Refusing to be silent

`--hotwords` with an engine that cannot use it **warns on stderr and records
it in the report's `warnings`, with `hotwords` set to `null`** — the field
means "the vocabulary that was applied", not "the vocabulary that was typed".
Exit code stays 0.

Not a hard error: a batch agent running one command across mixed engines
should not lose the whole run to an inapplicable flag. But not silence
either, because a feature quietly doing nothing is the failure this project
has already been caught by twice (`asr-evaluation.md` §5 and §13). Verified:

```text
$ verse transcribe 000001.wav --engine sensevoice --hotwords "Kubernetes,gRPC" --json
warning: engine 'sensevoice' cannot use a domain vocabulary; --hotwords was ignored
{"hotwords": null, "warnings": ["engine 'sensevoice' cannot use a domain vocabulary; --hotwords was ignored"], ...}
```

### Tests

Five unit tests on the extracted `qwen3_config` helper — config assembly split
out of the constructor so this is checkable **without a 982 MB model**: a
lexicon reaches the decoder, it is passed through unchanged in all three
delimiter forms, a blank one counts as none, and the token budget still
defaults rather than to zero.

### What is not claimed

Those two clips prove the mechanism works. They are **not** a measurement of
how much it helps in general — that needs a domain with its own held-out set,
and building a lexicon from the same clips you measure on would only be
measuring the test set leaking into the input.

## [x] 7. Documentation, and five corrections

Three files at the repository root:

- **`README.md`** — the front door, which did not exist. What Verse is, the two
  commands, the output contract, the exit codes, the measured accuracy per
  domain, and the engine comparison. It states plainly that the models are not
  bundled and why.
- **`llms.txt`** — the convention for AI crawlers: the commands, the JSON shape,
  the exit codes, the engines, in the form an agent reads rather than a form a
  person reads.
- **`AGENTS.md`** — conventions for an agent working *in* this repository: the
  `docs/llmwiki` workflow, English docs and Chinese conversation, commit rules,
  the `verse-core`-has-no-dependencies constraint, and a section on the two
  mistakes this project has paid most for — comparing things that are not
  comparable, and letting a feature fail silently.

**Verified:** every flag named in `llms.txt` and `README.md` was checked
against `verse transcribe --help`; the set in `llms.txt` is exactly the set the
parser accepts, and nothing is documented that does not exist.

One gap this found: `verse transcribe --help` was an error, because only the
top-level `--help` was handled. Fixed — exploring a command's own surface is
the first thing an agent does.

### The five corrections

A README that repeats a stale design doc as fact is worse than no README, so
these were fixed first:

1. `design.md` §9 still said **"P1b: slint GUI shell"** — the window has been
   Tauri + Svelte since the migration.
2. `design.md` §10 said **"Default to Paraformer-large"**, contradicting §5.3
   of the same document. Paraformer was removed; SenseVoice *is* the default,
   and there is no fallback engine to default to.
3. `design.md` §10 listed a **punctuation model** as blocking the phase exit.
   Punctuation was removed from the product entirely.
4. `THIRD_PARTY_NOTICES.md` still described Paraformer as **"the fallback
   engine"**.
5. `tasks/p1a-cli-transcription.md` step 7 was unchecked and named a
   **Paraformer backend** that no longer exists. The trait it was for does.

## On "AI search ranking"

Stated plainly because the plan said it would be: **search ranking cannot be
promised, and nothing here claims it.** What is in a project's control is being
legible to an AI that has already found it — an accurate `README.md`, an
`llms.txt` that describes the real interface, machine-readable output, and
distinct exit codes. That is what was built. Whether a crawler ranks it well is
not something this repository can determine.
