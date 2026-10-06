# Verse

Offline Chinese speech-to-text for Windows. Drop in an audio or video file, get
subtitles out. Nothing is sent anywhere: the models run locally, and the only
network access in the whole program is the one command that downloads a model.

- **Runs offline.** No account, no API key, no upload.
- **Punctuates.** Subtitles come out with commas and full stops already in
  them, which is the reason the default engine was chosen.
- **Drivable by a program.** Every command takes `--json`, batches take a
  directory, and failures come back as distinct exit codes.
- **Honest about its own failures.** A measurement of how much audio actually
  reached the recogniser travels with every result, because a short transcript
  and a truncated one look identical without it.
- **Does not do the same work twice.** A result is cached against the audio it
  came from, so transcribing the same file again is instant, and a long file
  interrupted halfway resumes without redoing the recognition. The cache is
  local files and nothing else; `--no-cache` turns it off.

## Install

```
cargo build --release
```

Two things are needed at runtime:

- **ffmpeg** on `PATH` (or set `VERSE_FFMPEG` to it). Formats are read by
  invoking ffmpeg, never by linking it.
- **A model.** `verse model fetch sensevoice` gets the default, 228 MB. The
  VAD model comes with it. `verse model list` shows what is installed and how
  much disk it is using, `verse model remove <id>` deletes one, and
  `verse model clean` clears half-finished downloads.

The build downloads a prebuilt sherpa-onnx archive; behind a slow link, see
`docs/llmwiki/design.md` §7.2 for staging it by hand.

## Use

```console
$ verse model fetch sensevoice
$ verse transcribe meeting.m4a -o meeting.srt
$ verse transcribe recordings/ -o subtitles/          # a whole directory
$ verse transcribe a.m4a --engine qwen3-asr           # the more accurate one
```

`verse --help` lists everything. The parts worth knowing:

| flag | what it does |
|---|---|
| `--json` | one JSON document on stdout; everything else stays on stderr |
| `-j, --jobs <n>` | files at once. Each worker loads its own model — about 250 MB for SenseVoice, 1 GB for Qwen3 |
| `--hotwords <terms>` | domain vocabulary, so `球拍` is not heard as `酒吧`. Qwen3 only |
| `--fail-fast` | stop at the first file that fails instead of finishing the batch |

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

**`coverage` and `recovered` are the two fields that say whether to trust a
result.** `coverage` is the fraction of the file's non-silent audio that
reached the recogniser; `recovered` is true when that fraction was implausible
and the file was recognised whole instead. A transcript with low coverage and
`recovered: false` in an older version is one that lost audio.

## Accuracy, measured

Five datasets, 5049 utterances, scored per domain rather than averaged —
because the spread is the point:

| domain | character error | exact |
|---|---|---|
| conversation | 4.80% | 63.4% |
| meeting | 6.95% | 48.4% |
| documentary | 7.16% | 35.2% |
| phone call | 8.42% | 28.8% |
| live commerce | 11.30% | 25.0% |

Punctuation is scored separately, on its own terms. The full account, including
which of these numbers changed once the measurement was fixed, is in
`docs/llmwiki/tasks/asr-evaluation.md`.

## Engines

| | SenseVoice-Small | Qwen3-ASR-0.6B |
|---|---|---|
| size | **228 MB** | 982 MB |
| speed | **fast** | ~4× slower |
| punctuation | **best** | good |
| accuracy | good | **best on every domain measured** |
| vocabulary bias | no | **yes** |
| licence | FunASR Model Licence v1.1 | Apache-2.0 |

SenseVoice is the default. Qwen3 is a `verse model fetch qwen3-asr` away and is
the only one that takes `--hotwords`.

**Neither is bundled with the installer** — Qwen3 is 1 GB and its ONNX weights
come from a third-party upload, and bundling it would make every user pay for
a capability most will not use.

## Development

```
cargo test --workspace
cargo clippy --workspace --all-targets
```

`docs/llmwiki/` holds the design, the task logs, and the changelog. Start at
`design.md`. `AGENTS.md` describes the conventions if you are an agent working
in this repository.

## Licence

Apache-2.0. Model weights carry their own licences; see
`THIRD_PARTY_NOTICES.md`.
