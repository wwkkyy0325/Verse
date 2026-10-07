# Verse

Offline Chinese speech-to-text. Drop in an audio or video file, get subtitles
with punctuation already in them.

**Recognition runs entirely on your machine.** Nothing is uploaded, no account,
no connection out. Exactly two commands touch the network or a port at all:
`verse model fetch` downloads a model, and `verse serve` listens — on
`127.0.0.1` only, and it connects to nothing.

[中文](README.md)

## Three ways in, one pipeline

| | |
|---|---|
| **A desktop window** | Drop a file on it. One page: the engine and this session's files on the left, the transcript on the right. Close it and open it again, and what you did before is still listed |
| **A command line** | `verse transcribe`. Every command takes `--json`, a batch is a directory, and failures come back as **distinct exit codes** rather than one non-zero |
| **A local service** | `verse serve` lets another program — an agent, a script — use this machine's recognition as one node in a workflow |

## Why use it

**Punctuation is built in, not bolted on.** Subtitles come out with commas and
full stops already there, which is why the default engine is SenseVoice
(punctuation F1 between 83% and 86%).

**The progress bar does not lie.** Most tools spin, or invent a percentage.
Here the total length comes from the ffmpeg pass that was **already going to
run** — its `Duration:` line was always on stderr, just hidden by the log level.
When a length genuinely is not available (a stream, `Duration: N/A`) the bar says
so instead of **making a number up**.

**It tells you when a transcript might not be trustworthy.** Every result
carries `coverage` and `recovered`: how much of the non-silent audio actually
reached the recogniser, and whether that fraction was so low the file was
recognised whole instead. A transcript that is short because the recording was
short and one that is short because audio was dropped are **identical as text** —
those two fields are the only thing that separates them.

**It does not do the same work twice.** A result is cached against the audio's
content, so re-transcribing the same file is instant; a long file interrupted
halfway resumes rather than starting over. The cache is local files and nothing
else, and `--no-cache` turns it off.

**The downloader can be relied on.** Resume that also detects a server ignoring
`Range` — the case that silently corrupts a file, where it re-downloads rather
than appending — writes to a `.part` and renames atomically, verifies
**SHA-256 while streaming**, retries a mirror with a backoff, **checks free
space before the first byte**, and fetches several files at once.

**The pool is sized from the machine, not from a constant.** The memory budget
comes from what the machine actually reports (a quarter of it) divided by what
one worker actually costs, and `/health` tells you the number, how it was
arrived at, and whether the budget was **probed or assumed**.

## Measured

Everything below was measured on this machine (16 logical cores, 31.2 GiB, CPU
only). The method, and which of these numbers were wrong at first, are in
`docs/llmwiki/tasks/`.

### Accuracy

Five datasets, 5049 utterances, **scored per domain rather than averaged** —
because the spread is the finding:

| domain | character error | exact | punctuation F1 |
|---|---|---|---|
| conversation | **4.80%** | 63.4% | 83.0% |
| meeting | 6.95% | 48.4% | **85.8%** |
| documentary | 7.16% | 35.2% | 74.5% |
| phone call | 8.42% | 28.8% | 76.5% |
| live commerce | 11.30% | 25.0% | 66.5% |

**A single figure would be meaningless.** The best and worst conditions differ by
more than a factor of two; phone calls and live commerce are hard because they
are narrowband, and because speech overlaps quickly over music. Any sentence
beginning "this recogniser is about X%" is describing one of these rows and
hiding the rest.

### The two engines

| | SenseVoice-Small | Qwen3-ASR-0.6B |
|---|---|---|
| size | **228 MB** | 982 MB |
| speed | **fast** | ~4× slower |
| punctuation | **best** | good |
| accuracy | good | **better on every domain measured** |
| vocabulary bias | no | **yes** |
| licence | FunASR Model Licence v1.1 | Apache-2.0 |

**SenseVoice is the default**: best punctuation, fastest, a quarter of the size.
Qwen3 is more accurate, but four times the time and four times the disk buy a
small margin — so it is the one you fetch yourself, not the one you get.

### Concurrency

24 jobs, SenseVoice, cache off. **Total wall time falls while per-job latency
rises** — the two have to be read together, and either alone misleads:

| workers | total wall | median per job | peak RSS |
|---|---|---|---|
| 1 | 6.03 s | **249 ms** | **361 MB** |
| 2 | 3.51 s | 287 ms | 686 MB |
| 4 | 2.43 s | 375 ms | 1324 MB |
| 8 | **2.02 s** | 589 ms | 2619 MB |

One job at a time is fastest in a pool of one; two dozen finish sooner in a pool
of eight. The command line's `-j` is an **explicit choice** (60 files: `-j 1`
takes 21 s and 347 MB, `-j 4` takes 10 s and 1245 MB); the service sizes itself
from the machine.

### Tried, and not shipped

**Generated summaries.** An extractive summary — picking sentences out of the
transcript — was built, looked at, and judged pointless: it can only quote. It
cannot merge, and it cannot conclude. So a small language model was measured: a
0.5B model on a real 5,718-character transcript took **279 s to prefill and 65 s
to generate, and the output was wrong** — it swapped who was who, and invented a
submission method that is not in the transcript.

**But that 5 min 45 s measures the runtime, not the model.** Prefill was a flat
50 ms per token with the cost rising linearly with prompt length, which means no
batched matrix multiply was happening. So this is **not** the conclusion "small
models are too slow on CPU"; it is "this runtime is". The full method and
reasoning are in `docs/llmwiki/tasks/llm-summary.md`.

## Install

```
cargo build --release
```

Two things are needed at runtime:

- **ffmpeg** on `PATH` (or set `VERSE_FFMPEG` to it). Formats are read by
  **invoking** ffmpeg, never by linking it — a crash or a leak inside it cannot
  reach the main process.
- **A model.** `verse model fetch sensevoice` gets the default, 228 MB, and the
  VAD model with it. `verse model list` shows what is installed and how much
  disk it uses, `verse model remove <id>` deletes one, `verse model clean` clears
  half-finished downloads.

The build downloads a prebuilt sherpa-onnx archive; behind a slow link, see
`docs/llmwiki/design.md` §7.2 for staging it by hand.

## Use

```console
$ verse model fetch sensevoice
$ verse transcribe meeting.m4a                        # writes <Documents>/Verse/meeting.srt
$ verse transcribe recordings/                        # every recording, into that one folder
$ verse transcribe meeting.m4a -o meeting.srt         # or say exactly where
$ verse transcribe a.m4a --engine qwen3-asr           # the more accurate one
```

Without `-o`, transcripts collect in a `Verse` folder inside your Documents. Run
the same command twice and the second run **replaces** the first's file rather
than making a copy; two different recordings with the same name get numbered
apart. `VERSE_OUTPUT` puts them somewhere else.

| flag | what it does |
|---|---|
| `-o, --output <path>` | a file, or a directory when there are several inputs |
| `--json` | one JSON document on stdout; everything else stays on stderr |
| `-j, --jobs <n>` | files at once. Each worker loads its own model |
| `--hotwords <terms>` | domain vocabulary, so `球拍` is not heard as `酒吧`. Qwen3 only |
| `--fail-fast` | stop at the first file that fails instead of finishing the batch |
| `--no-cache` | transcribe again even though the result is already cached |

## Use Verse from another program

`verse serve` runs until you stop it and lets anything on this machine ask for a
transcription over HTTP. It listens on `127.0.0.1` and initiates no connection,
so it does not change what leaves your machine.

```console
$ verse serve
verse serve listening on http://127.0.0.1:17322
  token and port: .../Verse/serve.json
```

It writes `serve.json` with the port and a token. Read them, then:

```console
$ curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:17322/health
$ curl -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    -d '{"input": "C:/audio/meeting.m4a"}' http://127.0.0.1:17322/jobs
{"version":1,"id":1,"state":"queued",...}
$ curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:17322/jobs/1
```

Poll until `state` is `done`, and `result` is the same JSON that
`verse transcribe --json` puts in `.results[0]`. `tools/verse-serve-client.py` is
a worked example using the standard library alone — no SDK, no dependencies —
and the same job runs unchanged from PowerShell's `Invoke-RestMethod`.

The API is also in the window's 关于 dialog, behind a **copy-for-an-agent**
button.

## For a program driving it

Data on **stdout**, diagnostics on **stderr**, so this works:

```console
$ verse transcribe recordings/ -o out/ --json 2>/dev/null | jq -r '.results[].text'
```

**One shape for one file and for many.** A single file is a batch of one, and
`.results` is always the array. Nothing has to branch on how many inputs were
passed.

```json
{
  "version": 1, "engine": "sensevoice", "ok": false,
  "succeeded": 7, "failed": 1,
  "results": [{
    "input": "a.m4a", "output": "a.srt", "format": "srt", "ok": true,
    "segmentCount": 1, "coverage": 0.998, "recovered": false,
    "text": "...", "segments": [{"startMs": 0, "endMs": 1500, "text": "..."}],
    "error": null
  }]
}
```

Every field is always present, `null` rather than absent.

**Exit codes** separate the failures that call for different actions:

| code | meaning | what to do |
|---|---|---|
| 0 | transcribed | |
| 1 | a bug | report it |
| 2 | the command line was wrong | fix the invocation |
| 3 | the input could not be read or decoded | try another file |
| 4 | no usable model | `verse model fetch <id>` |
| 5 | the engine failed | retry |
| 6 | network | retry, or stage the model by hand |
| 7 | cancelled | |
| 8 | the output could not be written | check permissions |

## Not in it

Written here so they do not creep in:

- **Live subtitles and translation** — planned, not built.
- **Summarisation** — tried; see *Tried, and not shipped* above.
- **No weights in the installer** — Qwen3 is 1 GB and its ONNX weights come from
  a third-party upload; bundling it would make every user pay for a capability
  most will not use.
- **Windows first** — the pipeline is portable; the window and the installer are
  polished for Windows today.

## Development

```
cargo test --workspace
cargo clippy --workspace --all-targets
```

386 tests, clippy clean. `docs/llmwiki/` holds the design, the task logs and the
changelog; start at `design.md`. `AGENTS.md` describes the conventions if you are
an agent working in this repository.

## Licence

**MIT or Apache-2.0, at your option** (`LICENSE-MIT` / `LICENSE-APACHE`).

Model weights carry their own licences and are not distributed with this
project; see `THIRD_PARTY_NOTICES.md`.
