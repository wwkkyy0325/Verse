# Working in this repository

Conventions for an agent making changes here. They are not style preferences —
each one exists because something went wrong without it.

## Read `docs/llmwiki/` before writing code

- **`design.md`** — architecture, the constraints, and why each decision was
  taken. It records what was *rejected*, which is usually the more useful half.
- **`tasks/`** — one log per piece of work, with numbered `[ ]` steps that
  become `[x]` as they are done. Every step carries a **verify** line.
- **`changelog.md`** — append-only, newest at the bottom.

## Language

Code, comments, identifiers, commit messages and every document are **English**.
Conversation with the maintainer is **Chinese**. Keep status markers and
headings in English regardless.

## Commits

Conventional commits (`feat:`, `fix:`, `refactor:`, `docs:`, `test:`,
`chore:`), English body. **Zero AI traces** — no `Co-Authored-By`, no
"Generated with", no tool signatures, no emoji. The history must read as
human-authored. Commit only when asked.

## Verify, do not assume

This is the strongest rule here, and the changelog is mostly a record of it
being broken and then fixed.

- **Measure before changing a default.** Several parameters in this project
  were set by reasoning and were wrong; the ones set by measurement are marked
  as such in their doc comments.
- **A plausible mechanism is not a diagnosis.** Three separate times, a
  confident explanation taken from reading the numbers did not survive a test.
  When you have a hypothesis, write the test that would falsify it.
- **A stale binary produces false findings.** A `0 spans` result that looked
  like a code-path discrepancy was a release build fourteen hours out of date.
  Rebuild before believing a difference.
- **Validate the instrument.** A probability probe reported 0.001 for every
  file until a missing context window was fixed. An instrument needs a
  known-good reading before its readings mean anything.
- **Report negative results.** "The sweep showed nothing" and "the flag does
  not justify its cost" are findings, and they belong in the task log.

## Constraints

- **`verse-core` has an empty `[dependencies]`.** No serde, no utilities. Wire
  formats live in the crate that owns the wire — see
  `crates/verse-app/src/bridge.rs` for the pattern, and
  `crates/verse-cli/src/report.rs` for the same thing done for the CLI.
- **No `unsafe`.** The project has none and intends to keep it that way; this
  is also why the downloader uses no signal handling.
- **ffmpeg is a subprocess, never linked.** A crash or leak inside it cannot
  reach this process.
- **Dependencies need a reason.** Argument parsing is hand-rolled for a
  handful of flags. When something is added, say in the commit or the code why
  it earns its place — `crates/verse-cli/Cargo.toml` has an example.
- **Nothing in the binary names a specific engine.** Engines are looked up in
  the registry, and capabilities are declared on `EngineDescriptor` so a caller
  can ask rather than guess.

## Don't repeat this project's two most expensive mistakes

1. **Comparing two things that are not comparable.** An engine comparison once
   put a two-stage pipeline against a one-stage one and read the difference as
   accuracy. Make sure both sides of a measurement do the same work.
2. **Letting a feature fail silently.** A lexicon handed to an engine that
   cannot use it, audio discarded by the detector, a token budget that
   produced nothing — all three looked like success. If something is ignored
   or unavailable, say so on stderr and in the JSON.

## Practical

```
cargo test --workspace
cargo clippy --workspace --all-targets
```

Both must be clean before a change is finished. Model weights and test audio
are gitignored and are not in the repository; see `design.md` §6 for how they
are fetched.
