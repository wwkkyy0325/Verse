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

## [ ] 5. Sentence boundaries

The worst remaining cases are partial truncations, not missing utterances:

```
ref  逛集市喽，去逛集市喽。妈妈，你快点儿。
got  逛集市了去逛集市了。
ref  妈妈，妈妈，快来，别淋湿啦！
got  别淋湿啦。
```

`min_silence_duration` is 0.25 s, and conversation is full of pauses shorter
than that which still split a sentence. Raising it keeps sentences whole at
the cost of coarser subtitle lines.

**Verify:** sweep `min_silence_duration` across both datasets and report both
CER and punctuation F1, since the two pull in opposite directions here. Make
it settable the same way the threshold is.

## [x] 6. A single average was hiding a factor of five

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

## [x] 7. The wrong reference was being scored against

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

## [ ] 7. Exclamation marks

F1 is zero: the engine never emits one. Only seven occur in the sample, so the
evidence is thin, but the direction is not ambiguous. Either the engine cannot
produce them or the pipeline drops them somewhere.

**Verify:** find whether SenseVoice can emit ！at all, and if not, decide
whether it matters enough to add a stage that can.
