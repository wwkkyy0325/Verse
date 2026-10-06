# ASR evaluation

Goal: turn "勉强能用" into numbers that can be acted on. Every finding here
changed something, which is the only reason to have a benchmark.

Tooling:

- `tools/asr-eval/extract.py` — unpacks a HuggingFace audio parquet into wav
  files plus a `id / wav / reference` manifest.
- `crates/verse-bench` — scores a manifest and reports.

---

## [x] 1. Take the pipeline out of the application

`verse-pipeline` now holds the decode → segment → recognise chain, shared by
the CLI, the window and the benchmark.

It had already been copied once, between `verse-cli` and `verse-app`. A
benchmark scoring a third copy would have been measuring a program nobody
ships — and the boundary decisions are precisely what is under examination.

The recogniser is also loaded **once per run** rather than once per file. At
228 MB a load, scoring thousands of utterances any other way is mostly a test
of the disk.

## [x] 2. Character error rate, and what it was actually measuring

`cer.rs`. Character-level edit distance over normalised text — punctuation
and whitespace stripped, case folded — which is the convention Kaldi, ESPnet
and the HuggingFace evaluators all use, and the right one: references and
recognisers disagree about punctuation by design, so comparing raw text scores
formatting.

Measured on AISHELL-1 test, 2307 utterances: **8.44%**. Then, reading the
worst cases instead of the mean:

```
ref  十七万套    got  17万套
ref  百分之四    got  4%
ref  零三年      got  03年
```

**41% of the "errors" were ITN.** Both forms are correct; the yardstick
spells numbers out and the recogniser normalises them.

`EngineConfig::inverse_text_normalization` now exists. On in the product, off
in the benchmark. Same run with it off: **5.00%**.

## [x] 3. Punctuation, scored on its own terms

The character rate strips punctuation, which is standard and also hides the
thing this engine was chosen for. `punct.rs` scores it the way the restoration
literature does — precision, recall and F1 **per mark** — aligned by character
so a mark is judged by where it lands, not merely by appearing. A mark hanging
off a misrecognised character is neither credited nor blamed, because there is
no shared anchor to judge it against.

Speechio-Formal, conversation subset, 898 utterances:

| mark | precision | recall | F1 |
|---|---|---|---|
| 。 | 91.9% | 85.0% | 88.3% |
| ， | 84.7% | **70.1%** | 76.7% |
| ？ | 91.3% | 81.9% | 86.3% |
| ！ | 0.0% | 0.0% | 0.0% |
| **overall** | **88.8%** | **77.8%** | **83.0%** |

Commas are placed correctly when placed at all and missed 30% of the time.
Exclamation marks are never produced.

## [x] 4. The VAD threshold was guessed, and guessed wrong

The conversation set exposed something the read-news set could not: **17 of
898 utterances came back empty.** Their audio was intact — verified with
`ffprobe` and `volumedetect`, 2.4–5.0 s at −7 to −11 dB. `verse transcribe`
reported "0 spans": the detector found no speech at all.

The threshold had been set to 0.3 by reasoning. Measured:

| threshold | read news | conversation |
|---|---|---|
| 0.30 | 2.28% | 8.58% |
| 0.10 | 2.07% | 6.75% |
| **0.05** | 2.11% | **6.00%** |
| 0.02 | 2.14% | 5.76% |

Clean speech does not care. Conversation gains a third. Default is now 0.05 —
not 0.02, which scores marginally better, because sherpa-onnx refuses anything
at or below 0.01 and the margin is worth more than two hundredths of a point.

Full conversation set:

| | 0.30 | 0.05 |
|---|---|---|
| CER | 9.07% | 6.84% |
| exact | 47.4% | 51.7% |
| **p99** | **100%** | **57.1%** |
| empty outputs | 17 | 1 |
| punctuation F1 | 79.2% | 83.0% |

## [x] 5. Sentence boundaries — the hypothesis was wrong

The worst cases looked like partial truncations, and the obvious reading was
that `min_silence_duration: 0.25` was splitting sentences:

```
ref  逛集市喽，去逛集市喽。妈妈，你快点儿。
got  逛集市了去逛集市了。
ref  妈妈，妈妈，快来，别淋湿啦！
got  别淋湿啦。
```

Swept it, 300 utterances of conversation:

| min_silence | CER | exact | segments | chars/segment |
|---|---|---|---|---|
| 0.25 | 6.66% | 154 | 303 | 13.9 |
| 0.60 | 6.61% | 154 | 301 | 14.0 |
| 1.00 | 6.56% | 154 | 300 | 14.1 |
| 1.50 | 6.56% | 154 | 300 | 14.1 |

**Nothing moves.** Three hundred utterances produce three hundred segments at
every setting — one each. These files are one sentence long and the detector
was never splitting anything. 13.9 characters per segment is not a symptom of
over-cutting; it is the length of the reference sentences.

The parameter is settable now and costs nothing to keep, but it was not the
problem.

**A plausible mechanism is not a diagnosis.** This is the third time in this
work that reading the numbers was not enough, but the first time the
mechanism I inferred from the failures was simply wrong.

Why those sentences lose their second half is still unknown. The next step is
one utterance traced through the pipeline — what spans come out, and what the
recogniser is handed — rather than another parameter sweep.

## [x] 6. The generation budget is not a bottleneck either

Qwen3's decoder has a `max_new_tokens`, defaulting to 128, and upstream's own
release notes mention adding a truncation warning. Our spans are capped at 20
seconds, so the arithmetic looked close: 20 seconds of Chinese is 60–100
characters, and the budget is in tokens.

Swept on 200 utterances of conversation: **32, 64, 128, 256 and 512 give
5.40% CER, 120 exact matches, 83.0% punctuation F1, and identical timings.**
The parameter is wired correctly — set to 1 it emits `language`, the first
token, and CER jumps to 101% — so the budget simply never binds.

Tested again on 43 seconds of unbroken speech, ten utterances concatenated
with no gaps: seven segments came out, averaging six seconds each. **The
detector cuts at natural pauses well before the 20-second cap**, so spans
never grow long enough to need a large budget.

Kept at the upstream default. It has no measurable cost and nothing here
argues for lowering it.

## [x] 7. No GPU — and no runtime either

Two questions with opposite answers.

**No runtime is needed.** sherpa-onnx statically links onnxruntime into the
archive it downloads, so the product is a single executable with nothing to
install alongside it. That is what the "no cmake, no C++ toolchain" finding in
`design.md` §7.2 has been buying all along.

**No GPU is used.** The archive is `win-x64-static-MT-Release` — CPU only.
sherpa-onnx publishes CUDA and DirectML builds, but they are a different
archive, and the CUDA one carries a runtime dependency measured in hundreds
of megabytes. For a tool whose premise is install-and-run, that is the wrong
trade; the hardware probe and `engine_threads` are the CPU-side mitigation.

Worth stating plainly because it is not obvious from outside: an LLM decoder
sounds like something that wants a GPU, and this one does not use one. If it
ever should, the change is an archive swap plus a probe rather than anything
structural — `Registry` already selects engines at runtime.

## [x] 8. A single average was hiding a factor of five

Six datasets now, 300 utterances each. The result that matters is not any one
number but the spread:

| dataset | CER | exact | punct F1 |
|---|---|---|---|
| AISHELL (read news) | 6.96% | 65.7% | — |
| conversation | 6.66% | 51.3% | 83.2% |
| meeting | 10.67% | 30.3% | 88.2% |
| phone call | 12.16% | 20.3% | 76.5% |
| **sports commentary** | **37.30%** | **7.0%** | 66.3% |
| documentary | 14.71% | 20.7% | 73.2% |

**A factor of five between the best and worst condition.** Any single figure
quoted for this recogniser would be describing one of these and hiding the
rest — which is exactly how 8.44% came to look like an answer.

The tool now takes several manifests and prints this table, so the spread is
the default view rather than something assembled by hand.

## [x] 9. The wrong reference was being scored against

The 37% was real but it was not measuring recognition. Reading the failures:

```
ref  颜色、色料以及上唇效果都非常出色，而且其切面很大，便于涂抹。
got  颜色啊、色料啊，包括它的上唇的效果啊，真的非常厉害。
```

`target_text` is not a transcript. It is *formal written Chinese* — filler
removed, phrasing rewritten, expressions normalised — supplied by the dataset
specifically for training text-normalisation models. Scoring recognition
against it measures the recogniser **plus a rewriting stage that does not
exist in this product**, and marks the recogniser down for faithfully
reproducing what was said.

The dataset carries `original_text` for exactly this reason: the verbatim
spoken form. `extract.py` takes `--reference`, and both are now extracted
side by side.

This also means the early numbers are worth re-reading. The conversation
subset was scored against `target_text` too.

**Verified.** Both reference styles, same audio, 300 utterances each:

| dataset | vs written reference | vs verbatim reference | gap |
|---|---|---|---|
| conversation | 6.66% | **5.00%** | −1.7 |
| meeting | 10.67% | **7.64%** | −3.0 |
| phone call | 12.16% | **8.19%** | −4.0 |
| documentary | 14.71% | **6.93%** | −7.8 |
| live commerce | **37.30%** | **11.31%** | **−26.0** |

The worst domain was not five times worse than read speech. It was 1.6 times
worse, and the rest of the gap was the yardstick.

**The arrangement that follows:** recognition is scored against
`original_text`; punctuation is scored against `target_text`. That is not a
compromise — `punct.rs` compares marks only at positions where both sides
agree on the character, so a written reference's rewritten phrasing is
skipped rather than counted. The punctuation numbers were never affected by
this; the character rates were, badly.

Also noted: `ZH00007` is labelled *sports commentary* and contains, in the
sampled rows, live commerce — a host recommending cosmetics. The label is the
dataset's; the content is what it is. Either way it is not read speech, and
the label is worth trusting less than the audio.

### Corrected numbers

Every utterance in five domains, 5049 of them. Recognition against the
verbatim reference; punctuation against the written one, which is the only
one that has any:

| dataset | utterances | CER | exact | punct F1 |
|---|---|---|---|---|
| conversation | 898 | **4.80%** | 63.4% | 83.0% |
| meeting | 1559 | **6.95%** | 48.4% | **85.8%** |
| documentary | 856 | **7.16%** | 35.2% | 74.5% |
| phone call | 996 | **8.42%** | 28.8% | 76.5% |
| live commerce | 740 | **11.30%** | 25.0% | 66.5% |

Scoring 300 instead of the full set changes no figure by more than 0.7
points, so sample size was never the problem — the reference was.

Two things the domain breakdown shows that a single figure could not:

- **Recognition spread is 4.80% to 11.30%.** Phone calls and live commerce
  are the hard cases, and the audio explains both: narrowband, and fast
  overlapping speech over music.
- **Punctuation and recognition are not the same difficulty.** They mostly
  track each other, but documentary is the third-best to hear and the
  second-worst to punctuate — it is narrated in long fluent clauses with
  little pause to go on. Neither number predicts the other, which is the
  argument for measuring both.

## [x] 10. Exclamation marks

F1 is zero for SenseVoice: it never emits one. Seven occur in the sample, so
the evidence is thin, but the direction is not ambiguous.

**Qwen3-ASR does emit them** — it produced ten false positives in the same
200 utterances, meaning it writes ！where the reference does not. So this is a
property of SenseVoice, not of the pipeline, and the pipeline is not dropping
anything. Logged rather than acted on; a subtitle without exclamation marks
loses tone, not content.

## [x] 11. Punctuation as a stage, not a property of the engine

`Request::punctuation_model` exists and the pipeline applies it per span.

It was missing for a reason worth recording: punctuation was implicit in the
engine choice. SenseVoice punctuates internally, so nothing ever asked where
punctuation came from, and the requirement stayed invisible until a second
engine was tried. `design.md` §5.4 had described this stage as a
`TextChain::push` since the first draft; it had never been written.

The stage is now exercised — it is what made the Paraformer comparison fair —
but **neither registered engine needs it**, which is worth knowing before
someone assumes it is load-bearing.

## [x] 12. Three engines, and one removed

Same 200 utterances of conversation, then four more domains:

| domain | | SenseVoice | Paraformer+punct | Qwen3-ASR |
|---|---|---|---|---|
| conversation | CER | 6.00% | 5.90% | **5.40%** |
| | punct F1 | **84.3%** | 81.4% | 83.0% |
| meeting | CER | 11.52% | 8.38% | **7.23%** |
| | punct F1 | **88.1%** | 83.1% | 87.4% |
| phone call | CER | 12.77% | 11.64% | **11.24%** |
| | punct F1 | 75.3% | 67.0% | **78.7%** |
| documentary | CER | 14.91% | 14.36% | **13.84%** |
| | punct F1 | 76.0% | 69.7% | **77.2%** |
| live commerce | CER | 33.17% | 31.31% | **31.57%** |
| | punct F1 | 65.5% | 64.7% | **71.9%** |

Per 200 utterances: SenseVoice ~50 s, Paraformer ~115 s, Qwen3 ~255 s.

**Paraformer is removed.** It wins 0.6–3.1 points of character accuracy and
loses 1–8 points of punctuation, 2.3× the time, and twice the disk. Not beaten
on every axis, but beaten on enough that a third option cost more than it
returned — and its one advantage was the axis least visible to the person
using the thing.

**Qwen3-ASR is kept and is not the default.** It is the most accurate on
every domain measured, and it is a different architecture, which makes the
comparison worth having. It costs 4–5× the time and 4.3× the size: four hours
of audio is 5 minutes against 22.

**Live commerce is hard for all three** — around 31–33%. Worth noting that
this is against the *written* reference; against the verbatim one SenseVoice
scores 11.30% on the same audio. Both numbers are real; they answer different
questions.

### What the measurements changed

- The default was going to be re-examined anyway. It was not changed:
  SenseVoice is fastest, smallest and best-at-punctuation, and the two
  strengths it lacks are both now quantified rather than suspected.
- An earlier note in this file said Paraformer looked better on meeting
  audio. It does — but that note was written before the punctuation stage
  existed, so it was comparing a two-stage pipeline against a one-stage
  one. The comparison above is the first fair one.
