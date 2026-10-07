# An adaptive pool of workers for `verse serve`

`verse serve` runs one worker: one `ModelKeeper`, one thread, a queue. Four
concurrent requests serialise. This gives it a pool, sized from the machine and
the model.

Two decisions taken with the maintainer:

- **Adaptive by default, and the cost must be visible.** The command line's `-j`
  defaults to 1 because, as its log records, *"the memory cost is real and
  invisible, so it is opted into rather than paid by everyone"*. The service
  answers that by making the cost visible — `/health` reports the pool size and
  the bytes it implies — rather than by defaulting to 1.
- **Memory budget: 2 GiB**, a quarter of the 8 GB floor `design.md` §3 already
  commits to. RAM cannot be probed: `HardwareProfile` has no memory field, and
  reading it needs FFI and `unsafe`, both excluded. So the budget is a **written
  assumption anchored to a number the project already states**, not a probe —
  and it is reported to clients so it can be disagreed with.

## The rule

```
per_worker_bytes = model_bytes + RUNTIME_OVERHEAD_BYTES
affordable       = (MEMORY_BUDGET_BYTES / per_worker_bytes).max(1)
pool             = affordable.min(engine_threads).clamp(1, MAX_WORKERS)
```

`pool_size` takes **bytes, not a model**, so `verse-core` keeps its empty
dependency list and its stated property that it knows nothing about models. The
catalogue lookup stays in the service.

---

## [x] 1. The pure rule, in `verse-core`

Beside `engine_threads`, so the thread half and the memory half cannot drift, and
so the command line's `run_parallel` and the service share one expression for the
per-worker thread budget instead of two.

**Verify:** integer tests — the budget binds, the thread count binds,
`per_worker_bytes == 0` is 1 rather than a guess, and `MAX_WORKERS` binds.

**Done.** Seven tests. The rule gives **7** workers for SenseVoice and **2** for
Qwen3-ASR on this machine, and the CLI's `run_parallel` now calls the same
`per_worker_threads` so the two cannot disagree about what a worker asks for.

**An arithmetic correction to the plan.** It predicted 6 for SenseVoice; with a
64 MiB overhead the per-worker cost is 306.6 MB and 2 GiB affords **7**. Both
numbers move when `RUNTIME_OVERHEAD_BYTES` is set from the measured curve, and
the test asserts the current value precisely so that moving it is a decision
rather than a surprise.

## [x] 2. `Jobs` runs a pool; `--workers` plumbed; **default still 1**

`Queue.running: Option<u64>` becomes a count — that field, `Jobs::running()` and
`JobList.running` are the only three single-worker assumptions.

**One thread budget for the whole pool**, which is an invariant rather than an
optimisation: the settings digest includes the resolved thread count, so unequal
budgets would give workers **different model identities** and a job would be
runnable on only some of them.

**Verify:** existing tests pass with one keeper; two blocking runners reach a
running count of 2; `--workers 0` is refused; and a test pinning that
`threads: Some(engine_threads)` and `threads: None` produce the **same** digest,
so a pool of 1 invalidates no existing cache entry.

## [x] 3. `/health` reports the pool

Because making the cost visible was the price of defaulting to adaptive.

**Verify:** the contract test asserts every new key with pool 1, and the
integration test reads `pool.workers` and `pool.impliedPeakBytes` over a socket.

**Done, and folded into the same commit as step 2** — separating them would have
left a committed state with a dead field and a warning. Measured against a
running service with `--workers 3`:

```
workers 3 · workerSource "flag" · perWorkerThreads 5
modelBytes 239549735 · overheadBytes 67108864 · perWorkerBytes 306658599
budgetBytes 2147483648 · impliedPeakBytes 919975797
budgetAssumption "assumed, not probed: a quarter of the 8 GB machine floor"
model.residentKeepers 0 · jobs.runningCount 0
```

`modelBytes` matches the file on disk, and `perWorkerThreads` is 15 ÷ 3.

**A test of mine was near-tautological and I caught it by falsifying.** The first
version asserted that `digest(None)` equals `digest(Some(digest(None)))` — which
holds for almost any resolution rule, so it passed even when I deliberately broke
`effective_threads` to resolve `Some(n)` as `n + 1`. The rewritten test asserts
what actually decides the cache key: that the two spellings **resolve to the same
number**. It now fails under that falsification, with `left: 16, right: 15`.

## [x] 4. Measure the curve

Through the real service — the queue, the drain and the HTTP layer are part of
what is changing. Every worker gets `floor(engine_threads / N)` threads, so
**total threads stay constant across N** and the configurations are comparable.
`noCache` on every submit, because a cache hit would measure the cache.

**Two numbers per N, always together.** Total wall time includes the N model
loads a bigger pool pays; the steady-state median, after dropping the first N
jobs, is what answers whether a pool helps. Reporting only one of them either
hides the load cost or lets it pass as a speedup.

**The instrument is validated in the same run, before any curve is read:**

1. `sum(loads) == N` — one load per worker, or the workload is mis-sized;
2. N=1 twice, agreeing — or the machine is too noisy to read;
3. N=1 peak RSS reproduces the **347 MB** this project already measured;
4. N=1 steady-state per-job time lands in the known ~240–310 ms band.

**Falsified if** every N>1 has a steady-state median at or above N=1, or the
smallest N that wins already exceeds the budget. Then the default stays 1 and the
negative result is written down, which is what the CLI did for `-j`.

**Done. Not falsified.** 24 jobs of the same clip, SenseVoice, `noCache`:

| N | total wall | median per job | loads | peak RSS |
|---|---|---|---|---|
| 1 | 6.03 s | 249.0 ms | 1 | **361 MB** |
| 2 | 3.51 s | 287.0 ms | 2 | 686 MB |
| 4 | 2.43 s | 375.0 ms | 4 | 1324 MB |
| 8 | 2.02 s | 589.5 ms | 8 | 2619 MB |

**The instrument first, and all four checks pass.** `loads` equals N at every
size. N=1 run twice gave 2.98 s and 3.01 s, agreeing within 1% — the machine is
quiet enough to read a curve on. N=1 held **361 MB** against the **347 MB** this
project already recorded for the command line, within 4%, which is the anchor
that says the memory sampler is reading the right thing. And N=1's median of
249 ms lands in the 240–310 ms band the service was already known to produce.

**The curve is the classic one, and the two numbers disagree.**

- **Throughput keeps improving**: 24 jobs take 6.03 s at one worker and 2.02 s
  at eight. The gains are 72%, then 44%, then 20% — steeply diminishing.
- **Per-job latency keeps getting worse**: 249 ms at one worker, 590 ms at
  eight. Each worker gets fewer threads as the pool grows, so a job takes longer
  even as more of them finish.

That is the trade a service makes, and neither number alone describes it. At the
chosen default of six, a single job is a little under twice as slow as it would
be alone, and two dozen of them finish three times sooner.

`RUNTIME_OVERHEAD_BYTES` is set from this table rather than from an estimate.
Marginal cost per worker is `(2619 − 361) / 7 = 322 MB` against 239.5 MB of
weights, so the overhead is about 82 MB — the constant is 80 MiB, and the rule's
per-worker figure of 323.4 MB lands on the measurement.

`MAX_WORKERS` stays 8: the curve was still improving there, so it is a
deliberate stop rather than a knee that was found.

## [x] 5. The default becomes the rule

Measured against a live service with no `--workers`:

```
sensevoice   workers 6 (adaptive)  2 threads each  1.94 GB of a 2 GB budget
qwen3-asr    workers 2 (adaptive)  7 threads each  2.14 GB of a 2 GB budget
--workers 2  workers 2 (flag)      7 threads each
```

The gigabyte model landing on two while the small one lands on six is the rule
working, not a shortfall. Re-measuring at the chosen default confirmed the
prediction: 2.01 s for 24 jobs with all six workers resident.

## [x] 5. The default becomes the rule

`RUNTIME_OVERHEAD_BYTES` and `MAX_WORKERS` set **from the curve**, not from a
guess.

## [x] 6. Documents

---

## Not in this round

- **Probing RAM.** Needs FFI and `unsafe`. The budget stays an assumption, and it
  is reported.
- **The CLI adopting the pool.** It is finite and already measured, and its
  `thread::scope` path has no lock on the hot path. It shares the rule instead.
- **Preload / release routes.** Still deferred.
