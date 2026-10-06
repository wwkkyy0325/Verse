# Holding a model: the lifecycle round

The maintainer wants the backend to become a component other local programs can
use. Before any transport exists, one thing must: **a process that owns a
model**. Nothing does today.

Measured on a 1-second clip:

| | |
|---|---|
| recognition | 181 ms |
| whole run | **1641 ms** |

**89% of the run is loading the model.** The window loads one *inside* every
per-job worker thread (`verse-app/src/bridge.rs:430`), and the thread ends with
the job — so it reloads 228 MB per file and throws it away. `Transcriber`'s own
doc comment says the opposite is the entire reason the type exists.

**This round is only the lifecycle.** The loopback service is the next round; it
is not designed here, only left possible.

## A pre-existing dead end this round also fixes

Traced by reading, then verified against every line:

1. `Downloader::is_present` checks **size only**, so a model present at the
   expected size that the engine cannot construct from gives `ready = true`.
2. The screen goes `Working`; the worker's `Transcriber::load` fails.
3. It publishes `Event::JobFailed` with a job id **nobody has claimed** — the id
   is claimed only by `JobStarted`, which `Transcriber::transcribe` publishes,
   and `transcribe` is never reached.
4. `AppState::apply` falls through to the ownership guard (`state.rs:380`,
   `owns` at `:487-492`); `self.job` is `None`, so the event is dropped.
5. **The window stays on "正在准备…" forever, with a cancel button that cancels
   nothing.**

The comment at `lib.rs:128-130` says a broken catalogue is not worth refusing
over because *"the pipeline will say so"*. It cannot — the saying is dropped.

Publishing `JobStarted` before `JobFailed` fixes it, and the keeper is where that
belongs, because the keeper becomes the only thing that loads.

---

## [x] 1. Expose the effective threads and the loaded settings

Extract `pub fn effective_threads(&Request) -> usize` from `Transcriber::load`,
so the keeper and `load` resolve threads by one rule rather than two. Add
`Transcriber::settings() -> &str`.

**Verify:** a test pinning the digest to a literal captured before the change,
so the refactor is proven not to have invalidated existing cache entries; the
existing `cache-digest-*` tests unmodified and green.

**Done.** The pin is
`b6001e1dd53d72d02300cc8811efcd1f848a119e6b4465360d1127a214ef749b`, over a
request chosen to be machine-independent: an empty model directory, a VAD model
that is not there, an explicit thread count so the hardware probe is not
consulted, and an explicit ffmpeg string so no process is spawned.

**Honest correction to the plan's wording.** The plan said "captured before the
change". It was not: the pin was written *with* the refactor. What replaces that
claim is a check — `git diff` shows `settings_digest` is byte-identical to HEAD
and the thread rule is the same expression moved into a function, so the digest
is unchanged by construction. That is weaker than a capture and stronger than an
assumption, and it is the thing actually done.

The pin was **checked to bite**: adding one field to the fingerprint changes the
digest to `d3a1f08e…` and the test fails.

31 tests in the crate, 264 across the workspace, clippy clean.

## [x] 2. The keeper

`crates/verse-pipeline/src/keep.rs`: `ModelKeeper` with `preload` (启),
`status` (待机), `release` (停), and `transcribe`. Identity is the settings
digest — not a narrower engine-only key, because `Transcriber::run` reads `vad`,
`guard` and `vad_model` from its *stored* request, so reuse across a change in
any of those would silently run the old settings.

The model mutex is held for the whole job: `Transcriber` is `Send + !Sync`, so
that is forced, and it gives the honest semantics of one model doing one job.
`status()` reads atomics and never the lock, so a query cannot block behind a
multi-minute job.

The sweeper follows `spawn_forwarder`'s shape — an `mpsc` notification,
`recv_timeout`, the keeper held through a `Weak`, and a join on drop.

**Verify:** tests asserting on a **load counter**, not a timer, with a fake
engine so no weights are needed: reuse twice shows `loads=1`; a changed
`hotwords` shows `loads=2`; a 50 ms timeout reaches `Unloaded`; dropping joins
the sweeper; two concurrent callers show `loads=1`; a failing loader publishes
`JobStarted` then `JobFailed`, in that order.

**Done.** Eleven tests, 42 in the crate. The fake engine moved out of `lib.rs`'s
test module into `#[cfg(test)] pub(crate) mod testing`, because two test modules
need it now and a second copy of a fake is a second thing that can drift from
what it imitates.

`Transcriber::with_engine` computes the **real** digest — only
`Registry::create_engine` is skipped — so a test watching for reuse is watching
the same decision the real thing makes, not a stubbed string that would agree
with anything.

**Two tests were checked to bite**, and both carry a decision rather than a
behaviour:

- Removing the `JobStarted` publish before a load failure gives
  `["failed"]` where the test requires `["started", "failed"]`.
- Making the identity check reuse unconditionally gives `loads=1` where the
  changed-settings test requires `2` — which is the false reuse the design exists
  to prevent, reachable in one line of carelessness.

**`the_keeper_can_be_shared_between_threads` is a compiled assertion**, not a
comment: `ModelKeeper: Send + Sync`. The window hands it to a different thread
per job and a service would too, and without that bound neither is possible.

**One thing the plan did not anticipate.** `Transcriber` reads its cache policy
from the request it stored at load, and `settings_digest` does not cover the
policy — so a keeper reusing one model across callers would serve every later
job's results under the *first* caller's policy. Rather than document that as a
footgun, `Transcriber::set_cache_policy` re-points it for every job. It is safe
after loading precisely because the digest does not cover it, so `settings`
stays true.

**A naming correction.** The plan called the state `Held`; the code calls it
`ModelStatus`, because `Held` reads as a value rather than a state and the
method is `status()`.

## [ ] 3. The window keeps its model

`App` gains a keeper; the worker stops loading and stops publishing its own
`JobFailed`. No preload at startup: loading 228 MB before the user has chosen a
file contradicts "downloads are user-initiated everywhere".

**Verify:** a test driving `file_chosen(ready)` → `JobStarted` → `JobFailed`
lands on `Screen::Failed` with `Recovery::GetModel`. It fails today. The window
itself stays unverified and is recorded as such.

## [ ] 4. Measure

`crates/verse-pipeline/examples/reuse.rs`: the same clip twice, once loading per
file as the window does today and once through one keeper, printing per-file
milliseconds and a load counter.

**The counter is the instrument and is validated in the same run**: `--times 1`
must report `loads=1` in both phases. If it does not, the instrument is broken
and the timings mean nothing. `CachePolicy::Disabled` in both, because a cache
hit in the second phase would be measuring the cache.

## [ ] 5. Record

`design.md` §4.10, the changelog, and this log. Including the `ModelStateChanged`
decision — no new event, because a second unused one repeats a mistake already
written down twice, and reusing `ModelState` would make `Ready` mean both "on
disk" and "in memory".

---

## Not in this round

- **The loopback service.** `ModelKeeper::status()` and `with_timeout` are the
  seams it will need. They exist, unused, and are named as such rather than built
  around.
- **Showing model state in the window.** Queryable, not displayed.
- **The CLI and benchmark adopting the keeper.** The CLI's `--jobs N` loads one
  model *per worker* deliberately; one keeper's single mutex would serialise the
  workers and turn `-j 4` back into `-j 1`.
