# Audio coverage guard, and folding the CLI into the pipeline

Follows `asr-evaluation.md` §13. That section established that the segmenter
silently drops audio on about 1.2% of utterances — 11 of 898, scoring 28% CER
against 5.7% for everything else — and that the audio itself is fine: bypassing
the detector entirely recovers it, with two of the eleven becoming exact.

This task builds the defence §13 says follows from that, and removes the
duplicate pipeline found while investigating it.

**Standing constraint: no false positives.** A guard that fires on healthy
recordings would trade a rare silent failure for a common expensive one. Every
step below is gated on measurement, and the healthy case is re-measured after
each change rather than assumed unchanged.

---

## [x] 1. Measure what the segmenter keeps, without changing what it does

`verse-pipeline` sees both quantities that matter — the audio the decoder
produced and the audio the segmenter passed on — and currently throws the
relationship away.

Add a `Coverage` accumulator, fed by the existing loop, reporting three
numbers per file:

- **decoded** — everything the decoder produced;
- **voiced** — the union of the spans' time ranges, merged rather than summed,
  because spans overlap once the segmenter's leading padding is included and
  summing would credit the same audio twice;
- **energetic** — the audio that is *not silent*, judged against the file's own
  peak rather than an absolute floor, so a quiet recording is not mistaken for
  an empty one.

Print them from `verse-bench`. No behaviour changes in this step; the point is
to have numbers before choosing any.

**Verify:** all five datasets report coverage. **Done** — and the step produced
a transcript byte-identical to the run before it, which is the point of
measuring without changing anything.

The reading, on 898 conversation utterances: mean coverage 98.8%, lowest 0.0%.
Coverage tracks the error rate monotonically and by a lot:

| coverage | utterances | CER |
|---|---|---|
| 0.00 – 0.30 | 2 | 95.0% |
| 0.30 – 0.50 | 2 | 56.0% |
| 0.50 – 0.70 | 5 | 31.2% |
| 0.70 – 0.80 | 7 | 22.5% |
| 0.80 – 0.90 | 12 | 26.2% |
| 0.90 – 0.95 | 11 | 10.9% |
| 0.95 – 0.99 | 45 | 5.3% |
| 0.99 – 1.00 | 814 | 6.2% |

**A measuring error worth recording.** The first pass at these numbers divided
normalised error counts by *raw* reference lengths, punctuation included, and
reported 5.95% for the set where the correct figure is 6.84%. The number came
from an analysis script rather than from `verse-bench`, and it was wrong; the
figures in `asr-evaluation.md` §13 that came from it have been corrected.

## [x] 2. Choose the floor from the data, not from reasoning

**Reordered.** This was originally to be done before the fallback existed. It
cannot be: what a floor costs depends on what recovery does, and the sweep has
to measure the outcome rather than predict it.

Measured by arming the guard at 0.995 — the fallback's transcript for every
file that could ever fire — and composing any other floor from that, since the
decision is per file and independent. Verified exact afterwards: the composed
prediction for 0.70 was 9 fired / 798 errors, and running it gave 9 / 798.

| floor | fires | better | same | worse | net chars | CER |
|---|---|---|---|---|---|---|
| 0.70 | 9 | 9 | 0 | **0** | −39 | 6.52% |
| 0.80 | 16 | 15 | 0 | 1 | −52 | 6.42% |
| 0.90 | 28 | 19 | 7 | 2 | −59 | 6.36% |
| 0.97 | 44 | 27 | 12 | 5 | −69 | 6.28% |

**Chose 0.70.** Every floor improves the mean, but above 0.72 the guard starts
damaging files that were already right — a character or two each. The first
such file sits at coverage 0.730, so 0.70 is the highest round value with any
margin. Beyond it the return flattens (0.32 points at 0.70 against 0.56 at
0.97) while regressions appear, and a guard whose entire justification is that
being wrong is cheap should not be tuned to the last thousandth of a point.

**The uncomfortable finding: coverage measures damage, not failure.** Of the
eleven files §13 identified as defeating the detector, only four have low
coverage. The others kept 80–100% of their audio and were mis-recognised
anyway — `000590` kept 98.7%, `000000` and `000134` and `000475` kept all of
it. The guard catches the losses, which are the catastrophic ones, and does not
catch the rest.

## [x] 3. Make the pipeline recover

When the guard fires, re-run the file with segmentation off and use that
transcript instead. The fallback must stay bounded — a four-hour recording can
never be held resident, which is what segmentation exists to prevent — so it
recognises in fixed-length blocks exactly as long as the longest span the
detector may emit.

**Verify:** the recovered files improve; a 200-file healthy sample is unchanged.

The nine files that fire at 0.70, every one improving:

| file | coverage | errors | reference |
|---|---|---|---|
| 000778 | 0.219 | 15/16 → **1/16** | 你为什么不像那些正常的土豆那样小 |
| 000242 | 0.379 | 8/15 → 4/15 | 逛集市喽，去逛集市喽。妈妈，你快点儿。 |
| 000465 | 0.403 | 6/10 → **1/10** | 妈妈，妈妈，快来，别淋湿啦！ |
| 000366 | 0.557 | 6/21 → 3/21 | 着火啦！着火啦！你好，消防站… |
| 000453 | 0.622 | 4/9 → **0/9** | 你好，丹尼，我们迷路了。 |
| 000614 | 0.667 | 5/12 → **1/12** | 妈妈快看，是雪人！雪人，我来喽！ |
| 000580 | 0.689 | 2/8 → **0/8** | 但是他开着一艘船。 |
| 000855 | 0.695 | 2/11 → **0/11** | 妈妈，我也想要那个大熊猫。 |
| 000563 | 0.000 | 4/4 → 3/4 | 晾上去了。 |

Whole set: 6.84% → 6.52%, and 37 errors recovered of the 837.

**On the healthy sample of 200: 199 identical, one improved.** `000580` — the
same file as in the table above — went from 2 errors to exact. No file was made
worse.

**The four-hour case, measured.** A 4.01-hour file (14432 s, 461 MB) built by
looping the 43-second clip:

| | |
|---|---|
| coverage | **100%** — the guard does not fire |
| segments | 931, spanning the full duration |
| wall time | 454 s, **31.8× realtime** |
| peak RSS | **414 MB** |

RSS over the run: 288 MB at 1 s, 409 MB once loaded at 17 s, 412 MB at 423 s —
flat across the whole file. The coverage accumulator costs about 1.2 MB for
four hours, one `(rms, length)` pair per 100 ms window.

Run twice; both transcripts identical.

The fallback's own bound is enforced by `FixedBlocks`, which cuts at the span
cap and is unit-tested to drop nothing at any length. No long file fires, so
that path has no end-to-end case — it is a bound by construction, verified at
unit level, not by a measurement.

## [x] 4. Tell the interface it happened

A file that recovers produces a transcript twice, and the interface has already
been shown the first one. It needs an event that says "discard what you have",
and the state machine needs to act on it. Silence here would leave the user
looking at the fragment the guard just replaced.

**Verify:** a contract test on the event's serialised shape, and a state-machine
test that a reset clears segments and keeps the job. **Done** — `Update::Cleared`
already existed and the frontend already handled it (clear the segment list),
so the work was carving the path from the pipeline to it: an
`Event::TranscriptDiscarded`, an `Applied::Cleared`, and two tests — one that
the segments go and the job stays, one that the replacement still lands
afterwards.

## [x] 5. Fold `verse-cli` into `verse-pipeline`

Found while investigating §13: the CLI does not depend on `verse-pipeline` at
all. It carries its own copy of decode → segment → recognise, and
`asr-evaluation.md` §1 wrongly claimed otherwise. The two agree today — checked
line by line — but two implementations of the boundary rules is exactly what
`verse-pipeline` was extracted to stop, and the CLI is the copy that should go.

**Verify:** the CLI's output matches the pipeline's, and the duplicate
`recognize`, VAD setup and decoder setup are gone.

**Done.** `verse-cli` now calls `Transcriber`; its own decode → segment →
recognise loop and the 20-line `recognize` beside it are deleted, and
`verse-asr` dropped to a dev-dependency. Verified on three files whose SRT is
unchanged from the old binary, and on 30 whose text matches the pipeline's
exactly — the two paths cannot drift now because there is one path.

The CLI trades its span count for the coverage line. The span count is what
cracked §13, but coverage is the better number for the same purpose, and the
guard now announces itself when it fires.

One thing found on the way: the `transcribe` example reaches an engine
directly and deliberately skips the detector — which is the same measurement
this task needed and hand-built twice. It kept working by moving `verse-asr` to
dev-dependencies, and its doc comment now says why it exists.

---

## Open questions

- **Long files are unmeasured.** Every failure observed is whole-file, on
  utterances of two to five seconds. A two-hour recording where one speaker is
  missed is a different shape of problem and there is no data for it. The guard
  as specified is a whole-file ratio and may not catch that; this is recorded
  rather than guessed at.
- **The root cause is still unknown.** §13 eliminated three mechanisms without
  confirming one. The guard is written to not care why, but if the cause were
  ever found, a better detector would beat a guard.
