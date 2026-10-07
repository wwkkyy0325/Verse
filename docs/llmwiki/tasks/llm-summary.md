# Can a small model summarise a transcript on this machine?

The extractive summary was built because it needed no runtime and no model, and
it was judged — by the maintainer, looking at it — **not worth having**. It can
only quote. It cannot merge two sentences about one thing, cannot say what was
decided, and cannot answer the only question anybody has about a meeting
transcript. "关键句" would be a fairer name for it, which is another way of
saying it does not do the job.

So: a generated summary, which needs a model that generates. **This is a
measurement first.** `design.md` §5.5 already rejected small LLMs once, in as
many words: *"0.5–3 s per sentence on CPU. Too slow."* That was measured, for
**translation**, on older hardware. Summarising is worse than translating —
you must read the whole transcript before writing anything — so the old
conclusion may well hold. It may not. Re-measuring is cheap compared to
building on a stale number, and building is what comes after.

## The design, fixed before anything is measured

**Two sizes, or the answer is meaningless.** Running only a 0.5B model and
concluding "too weak" compares nothing: the question is *at what size does it
become useful*, not *is the smallest one good*. This project has already made
that mistake once, with a two-stage pipeline against a one-stage one.

| | |
|---|---|
| Models | Qwen2.5-**0.5B**-Instruct and Qwen2.5-**1.5B**-Instruct, GGUF Q4_K_M |
| Runtime | **candle** — the runtime `design.md` §5.5 chose for P2b translation. Measuring with llama.cpp and shipping candle would be measuring something else. |
| Input | `Documents/Verse/标准录音 13.srt` — 5,595 characters of real Chinese, 124 lines. Real recordings, not a sample written for the occasion. |
| Machine | this one: 16 logical cores, 31.2 GiB, CPU only. |

**What is measured, per model:**

1. **Wall time** for the whole transcript, in one prompt where it fits and by
   chunking where it does not. A 5,595-character transcript is roughly 4–6k
   tokens, which the 0.5B model's context can hold — so this measures the
   *short* case. The long case (two hours, tens of thousands of tokens) is
   extrapolated from it **and said to be extrapolated**.
2. **Peak memory**, sampled from outside the process.
3. **The output itself**, read rather than scored. Whether a summary is worth
   showing is a judgement, and the judgement needs the text.

**What would make this worth building:**

- A summary that says something the transcript does not say in one sentence —
  i.e. that it merged, or concluded. Quoting is what we already have.
- Under about a minute for this transcript, since a person is waiting.
- Under about a gigabyte of memory, against the 8 GB floor C4 commits to.

**What falsifies it:** output that is wrong (invents facts, gets names wrong),
output that is just as extractive as what it replaces, or a time that makes the
wait longer than reading would have been.

## [x] 1. The spike

A throwaway cargo project under `tools/`, **not a workspace member**, so none of
this weighs on `cargo test --workspace` until the numbers justify it.

**Verify:** it builds; it summarises the real transcript; it reports its own
wall time and the model it loaded.

## [ ] 2. Measure both sizes — one of them measured

**Verify:** a table with time, peak memory and the output for each, measured on
this machine and not estimated.

## [ ] 3. Decide, in writing

Either the numbers justify a `verse-llm` crate and a fourth catalogue entry, or
they do not and the negative result is recorded as a first-class finding — which
is what this project did with `-j` on the command line and with the pool's
first curve.

**Verify:** the changelog carries the numbers, whichever way it goes.


---

## What was measured, and what it turned out to be about

**One model, one transcript, this machine.**

| | Qwen2.5-0.5B-Instruct Q4_K_M |
|---|---|
| load | 1.6 s |
| prompt | 3,831 tokens (5,718 characters) |
| **prefill** | **279 s — 73 ms per token** |
| generation | 65 s for 221 tokens (3.4 tok/s) |
| **total** | **5 min 45 s** |
| parallelism | 6.3× measured against wall clock |

**The speed number is about candle, not about small models.** Prefill is
**linear in the prompt** — 233 tokens took 13.5 s and 3,831 took 279 s, both
about 58 ms per token. A prompt processed in one batched matmul would not cost
more per token the more tokens there are. So candle's quantized CPU path is
building this one token at a time, and the 5 min 45 s says what that path
costs, not what the model costs.

**This is the project's own named mistake and it was not made.** Reporting
"small LLMs are too slow on CPU" from this run would be comparing two things
that are not comparable — a runtime that cannot batch against a runtime that
can. The number is recorded as what it is.

**And the achievable speed could not be measured here.** No `cmake`, no `ninja`,
no `make`, and llama.cpp's source and releases are on GitHub. Installing a
toolchain to measure with is the next step before any speed conclusion at all.

### The quality, which needs no runtime to judge

With plain greedy decoding the model repeated one sentence **seventeen times** —
which said more about the decoder than the model, so a presence penalty was
added and it was run again. The second output is fluent, on topic, and **wrong
in the ways that matter**:

- It frames the conversation as instructions to **被申请人**, the respondent.
  The transcript is somebody asking how to *file* a labour arbitration — the
  roles are the other way round.
- It says material may be submitted 「通过邮件或微信等方式」. That is not in the
  transcript. It was invented.
- Points 4, 7 and 8 are the same point three times.

A summary a person is meant to trust that is confidently wrong is worse than no
summary, because the whole value of the feature is that reading it is cheaper
than reading the transcript.

**That judgement is about this transcript and this model.** It is one sample,
not a benchmark, and it is recorded as one.

### What 1.5B would need

The size that would plausibly fix the quality is 1.5B — and at candle's prefill
rate one run is about **fifteen minutes**. That is not something to iterate on,
so it was not measured rather than measured badly.

**The order is therefore: a runtime that can prefill in batches, then 1.5B, then
the decision.** Measuring 1.5B with this runtime would take a quarter of an hour
per attempt and would still not answer the speed question.

### The spike

`tools/llm-summary-spike/`, deliberately **not a workspace member** — its own
`[workspace]`, and a `.gitignore` for the 1.7 GB of cargo output and 476 MB of
weights, because the root ignore covers `/models` and this directory is not
that one. It stays until the question is settled; it is the instrument.

## What is not on the table

- **Shipping the extractive summary as a consolation.** If the measurement says
  a generated summary is out of reach, the honest answer is to remove the
  feature rather than keep a weaker thing wearing its name.
- **Measuring with a different runtime from the one that would ship.**
- **Judging quality by a number.** There is no reference summary, and inventing
  one would measure agreement with the person who wrote it.
