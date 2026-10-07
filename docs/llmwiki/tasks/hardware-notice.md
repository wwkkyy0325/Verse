# The reduced-mode notice never reaches the window

Found while checking whether the frontend was finished, and it is the failure
mode `AGENTS.md` names as one of the two most expensive: **a feature that fails
silently.** Every layer reports success.

## What is there, and where it stops

| layer | state | evidence |
|---|---|---|
| design | required | `ui-design.md` §3: a dismissible one-line bar under the header |
| the tier | works | `verse-core/src/hardware.rs` — `Tier::Reduced`, with the reason |
| the command line | works | `verse-cli/src/main.rs:456` prints `Tier::notice()` to stderr |
| the command | works | `verse-app/src/lib.rs:80` — `hardware()` returns `degraded` |
| the frontend wrapper | exists | `ui/src/lib/api.ts:62` — `probeHardware()`, typed |
| **the window** | **nothing** | `App.svelte` never calls it and renders no notice |

The notice is produced, wrapped, reachable by name — and no user has ever seen
it. On a machine without AVX2 the window looks completely normal and is simply
slower, with nothing saying why.

## Two dead paths found at the same time

The convention here is to report dead code rather than remove it, so these are
recorded and left alone. Both are reported to the maintainer with the change.

1. **`Event::HardwareProbed` has no publisher.** It is defined in
   `verse-core/src/event.rs:73`, handled in `verse-app/src/state.rs:368`, and
   tested at `state.rs:928` — but the only `publish` call in the workspace is
   inside a test of the bus itself (`verse-core/src/lib.rs:141`). Nothing in
   production ever publishes it, so the handler cannot run.
2. **`AppState::hardware_notice` cannot reach the wire even if it did.**
   `bridge.rs` never reads the field, and `Update` has no variant that could
   carry it. The handler returns `Applied::Screen`, so the state machine
   *believes* it has told the user; the `Update::Screen` it produces carries the
   unchanged screen.

Taken together: a second, non-functional copy of the notice — including
`hardware_notice()`, `dismiss_hardware_notice()` and two passing tests that
prove nothing about what the user sees.

**Both were removed, with the maintainer's agreement.** Deleting them is part of
discharging the finding rather than a separate tidy-up: leaving a second copy of
a feature next to a working one is how the next person adds a publisher to the
dead event and gets a second silent failure out of the same mistake. What went:

| removed | where |
|---|---|
| `Event::HardwareProbed` | `verse-core/src/event.rs` — variant, `job()` arm, and its `HardwareProfile` import |
| its only `publish` | `verse-core/src/lib.rs` — swapped for `ModelStateChanged`, which keeps the jobless-event case the bus test exists for |
| the handler arm and `hardware_notice` | `verse-app/src/state.rs` — field, getter, `dismiss_hardware_notice`, and the two tests |

`Event::ModelStateChanged` was left alone. It is equally unused, but it is
*documented* as considered and reverted (`p1b-screens.md` §2), so it is a
decision someone made rather than a path someone left half-built — and it now
carries the bus test's jobless case.

Removing the arm left `apply`'s first `match` with a single pattern, so it
became an `if let`; that is the only line the deletion changed beyond removing
things.

## The language question, which is why this is not a one-line change

`ui-design.md` §8: *"Chinese, always. English is a supported input language, not
a UI language."* But `Tier::notice()` is English, and it has to stay English —
it is what `verse` prints to stderr, and the command line's output is English.

So the window cannot reuse that sentence. And it cannot sensibly translate it
either: the only structured handle on *which* weakness a machine has is
`Tier::reason()`, a **prose string** (`"this CPU does not support AVX2"`).
Matching on prose to choose copy is how two renderings of one fact drift apart,
which is the other mistake this project names.

Hence step 1: give the tier a structured weakness, and derive both renderings
from it.

---

## [x] 1. A structured weakness in `verse-core`

`Weakness { NoAvx2, SingleCore }`; `Tier::Reduced { weakness }` replaces
`Tier::Reduced { reason }`; `Tier::weakness() -> Option<Weakness>`.
`reason()` and `notice()` are derived from it and their output is **unchanged** —
the command line's wording must not move.

**Verify:** `cargo test -p verse-core` green, and the existing tests at
`hardware.rs:244` and `:254` still assert `"AVX2"` and `"single core"` in
`notice()`, which is what pins the English wording in place.

## [x] 2. The window's own copy, in `verse-app`

`hardware()` returns `degraded` rendered in Chinese from the weakness, with a
test for each weakness and one for a capable machine. Doc comment says why this
sentence is here rather than reusing `Tier::notice()`.

**Verify:** a given test — reduced machine gets a Chinese sentence naming the
reason, capable machine gets `None`. `cargo test -p verse-app` green.

## [x] 3. The bar, in `App.svelte`

`probeHardware()` on mount, beside `currentScreen()`; a dismissible line under
the header; dismissing clears local state. Nothing in Rust changes for this step.

**Verify:** `npm run check` reports 0 errors, `npm run build` succeeds. **The
rendering itself is not verifiable from here** — see step 5.

## [x] 4. Fix the stale budget comment — my own mess

`verse-core/src/hardware.rs:141` says `MEMORY_BUDGET_BYTES` is *"a written
assumption, not a probe"* and that reading memory *"needs FFI and `unsafe`, which
this project does not have"*. The previous round added a real probe
(`verse-cli/src/serve/memory.rs`, via `sysinfo`) and demoted the constant to the
fallback for a machine that will not answer. The comment states the opposite of
what the code does.

**Verify:** the comment names the probe and says the constant is the fallback.
`cargo test --workspace` green.

## [ ] 5. What stays unverified

The bar has never been on screen. This machine has AVX2, so `degraded` is
`None` here and the honest test would need a machine that lacks it — or a forced
profile, which is a code change made only to take a screenshot. It joins the
click-through in `p1b-screens.md` step 4 as something that needs a person.

**Verify:** stated here and in the changelog, not glossed.

## [x] 6. Documents

Changelog entry naming the evidence; this log's boxes ticked.

**Verify:** `cargo test --workspace` and `cargo clippy --workspace --all-targets`
clean.

## [x] 7. The two dead paths come out

Agreed with the maintainer rather than done unilaterally, because deleting
someone else's code is not a tidy-up. `Event::HardwareProbed` and everything
built on `AppState::hardware_notice` are gone; `ModelStateChanged` stays, since
it is documented as deliberated rather than half-built. Table above.

**Verify:** `cargo test --workspace` green at 332 — two fewer, which are exactly
the two tests that proved nothing — and clippy clean, including the new
`if let` in `apply` that the deletion forced.

## [x] 8. `ui-design.md` §8 stops naming a toolkit this project does not use

It said static labels live in "the `.slint` files", which has not been true
since the window moved to Tauri and Svelte; `design.md` §9 and
`tasks/agent-cli.md` record the same correction being made before. §8 also said
the hardware notice is produced by `state.rs`, which this change makes untrue —
it now comes from the window's own commands, because it is a property of the
machine rather than of the job.

The changelog is append-only, so its two historical `.slint` mentions stay as
they were written.

**Verify:** neither sentence survives a search for a toolkit or a file that no
longer holds the code.
