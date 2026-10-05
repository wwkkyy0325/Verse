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

## [ ] 6. More datasets

Only two so far, and they disagree in ways that matter. Speechio-Formal has 27
subsets and the rest are undownloaded:

| subset | domain | subset | domain |
|---|---|---|---|
| ZH00007 | sports commentary | ZH00020/21 | **meeting** |
| ZH00010/11 | news broadcast | ZH00022/23 | **phone call** |
| ZH00017 | court recording | ZH00024/25 | medical |
| ZH00018/19 | conversation | ZH00026 | education |

Meeting and phone are the two that were said earlier to be unavailable. They
are right here.

**Verify:** score every domain and report per-domain. A single number across
all of them would repeat the mistake the AISHELL run made — a clean-read
average that hides the conditions where the product is actually used.

## [ ] 7. Exclamation marks

F1 is zero: the engine never emits one. Only seven occur in the sample, so the
evidence is thin, but the direction is not ambiguous. Either the engine cannot
produce them or the pipeline drops them somewhere.

**Verify:** find whether SenseVoice can emit ！at all, and if not, decide
whether it matters enough to add a stage that can.
