# P0 — Framework skeleton

Goal: the workspace layout, domain model, and communication backbone that P1a and every later phase build on.

Exit criteria: tests prove that events reach subscribers by filter, that the registry resolves registered factories and names missing ids in its errors, and that a pipeline runs to completion while emitting progress and honouring cancellation.

Status: **complete**. `cargo test --offline` → 9 passed, 0 failed.

## [x] 1. Workspace and crate stubs

Six members: `verse-core`, `verse-audio`, `verse-asr`, `verse-model`, `verse-cli`, `verse-app`. All but `verse-core` are documented stubs — they exist so the dependency graph is settled before implementation starts.

**Verify:** `cargo build` succeeds for all six. ✅

## [x] 2. Domain model

`verse-core::domain` — `JobId`, `SegmentId`, `ModelId`, `AudioFormat`, `AudioChunk`, `Segment`, `Transcript`, `TranscriptDelta`, `HardwareProfile`, `ModelState`.

`Transcript` carries `Vec<Segment>` with timestamps from the outset, so SRT export in P1a step 9 needs no changes here.

**Verify:** covered indirectly by the pipeline tests. ✅

## [x] 3. Traits

`verse-core::traits` — `AudioSource`, `AsrEngine`, `TextSink`. All three live in core; implementations live elsewhere. This is what lets the registry hold engine factories without core ever linking sherpa-onnx.

**Verify:** stub implementations compile against them in the test module. ✅

## [x] 4. Event bus

`verse-core::event` — `Event`, `JobKind`, `EventBus`, `Subscription`.

Fan-out with per-subscriber filters. Publishing is synchronous; channels are unbounded so a slow subscriber accumulates rather than blocking the publisher. Dropping a `Subscription` unsubscribes, and dead subscribers are reaped lazily on publish.

**Verify:** events reach only matching subscribers; dropping a subscription unsubscribes; jobless events are not attributed to a job. ✅

## [x] 5. Registry

`verse-core::registry` — `Registry` plus engine/source/sink descriptors and factories.

Lookup failures return `ErrorKind::Registry` naming the missing id, rather than panicking — a typo should produce a usable message.

**Verify:** a registered factory resolves; an unknown id fails with the id in the message. ✅

## [x] 6. Router

`verse-core::router` — `Job`, `JobInput`, `Pipeline`, `Router`, `RuntimeContext`, `CancelToken`.

Registers pipelines per `JobKind`. An unregistered kind fails at submit time naming the kind. A fresh pipeline is built per job, so pipelines may hold per-job state without being `Sync`.

Cancellation is cooperative: `CancelToken` is polled between work units. No forced abort — that is what keeps model resources and partial output clean.

**Verify:** a pipeline emits four progress events and finishes; cancelling before the run produces `ErrorKind::Cancelled` and no completion event; an unregistered kind names itself in the error. ✅

## Notes

- **No external dependencies.** `crossbeam-channel` was dropped in favour of `std::sync::mpsc`, which covers every operation the bus needs (`recv`, `try_recv`, `recv_timeout`, `try_iter`). The workspace therefore builds and tests fully offline. This was chosen on its merits, not just to dodge the network problem below — a lighter dependency graph is a project goal.
- **Windows stale-proxy problem found and worked around.** See `design.md` §7.4. `NO_PROXY='*'` is required for cargo to reach the mirror on this machine. Should be fixed properly before P1a.
