# One page: model panel, real progress, a file list, an extractive summary

The window is five screens where each replaces the whole view. This makes it one
page with regions, and adds what the regions need: a model panel, a real
percentage, this session's files, and an extractive summary.

Full reasoning and the measurements behind each decision are in the plan; this
log records what was done and how it was checked.

## [x] 1. The state model: a roster beside the detail pane

Keep `Screen`, make it per file, and put a roster and a selection beside it.
`AppState::screen()` keeps its `&Screen` signature so the existing tests'
assertions do not change.

**Verify:** `cargo test -p verse-app` — existing assertions unchanged, plus a
second drop keeping the first, selection returning a finished file's screen and
segments, a cancelled entry disappearing, and the queue advancing in order.

## [x] 2. The real progress percentage

`Duration:` is already on the stderr the decoder drains; `-loglevel error` was
hiding it. Parse it live, thread it to `fraction`, stop discarding the fraction
at the bridge.

**Verify:** duration-parser unit tests including `Duration: N/A`; the fraction is
`Some` when the length is known and `None` otherwise; a bridge contract test; an
existing cache entry still hits.

## [x] 3. The extractive summarizer

`crates/verse-core/src/summary.rs`, pure `std`, character bigrams as the term
unit, output in source order.

**Verify:** every sentence verbatim from the input and in order; a transcript
with no shared terms still returns something; empty and single-segment inputs do
not panic.

## [x] 4. The model panel

`ModelSpec::description`, a `models()` command, and an engine switch that
releases the old model.

**Verify:** a `ModelChoice` contract test; an unknown id refused; every registry
engine id has a catalogue entry.

## [x] 5. The processed-files list

Session-only, backed by the roster.

**Verify:** a finished file stays selectable after another starts.

## [x] 6. The single page

Regions; `DropZone` and `TranscriptView` moved rather than rewritten; a
multi-file drop enqueues everything.

**Verify:** `npm run check` and `npm run build`. The layout needs a person.

## [x] 7. The two demonstrations

Dev-only synthetic progress that cannot run in a release build; a real run on a
clip long enough to watch.

**Verify:** the release build has no working demo path; the real numbers are
recorded.

## [x] 8. Documents

The four recorded decisions amended with the stage that changes each.

**Verify:** `cargo test --workspace` and clippy clean.
