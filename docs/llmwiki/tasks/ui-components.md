# Five vendored components that no screen ever used

Found while checking `ui-design.md` §5's claim that eight shadcn-svelte
components were taken, "each earning its place by being needed on two or more
screens". Measured against the source:

```
button      3 files      card         0
dialog      1            scroll-area  0
progress    1            separator    0
                         alert        0
                         sonner       0
```

`App.svelte` imports `Button`, `Dialog` and `Progress`. The other five are
imported by nothing, anywhere — including by CSS, `components.json`, the Vite
config and `index.html`.

## The argument for keeping them did not survive checking

The obvious reading was "scaffolding for screens not yet built". §5 lists them
against screens by name, so the claim was checkable: `Card` for "empty, model,
working, done", `ScrollArea` for "transcript", `Alert` for "failed,
degraded-hardware notice", `Sonner` for "export confirmation".

**All of those screens exist**, and every one was built without the component
§5 named for it. The transcript scrolls in a hand-written element with
auto-follow; the failed screen and the notice are hand-written markup; `Done`
reports a failed write in its footer. So these are not scaffolding — they are
components left behind by screens that were built a different way.

Two costs, both measured rather than asserted:

- **Every build pays for them.** Tailwind scans component sources for class
  names, so the stylesheet went from 31.55 kB to 37.23 kB when they were in the
  tree.
- **`Sonner` was the only reason two dependencies existed.** It imported
  `svelte-sonner` and `mode-watcher`, neither of which anything else used.

---

## [x] 1. Remove the five directories

`card`, `scroll-area`, `separator`, `alert`, `sonner` — 20 files, 450 lines of
vendored source. `git rm`, so the removal is in the history rather than a
`rm` that leaves it invisible.

**Verify:** `npm run check` reports 0 errors and `npm run build` succeeds. Both
did, before and after.

## [x] 2. Remove the dependencies they were the last users of

Found by walking what the deleted files imported: `svelte-sonner` and
`mode-watcher`, and nothing else in the tree referenced either.

**A trap worth recording.** A first pass grepped the *current* source for each
dependency and reported `mode-watcher` as unused — which looked like pre-existing
dead weight rather than something this change caused. It was caused by this
change: `sonner.svelte` had imported it, and the file was already gone when the
grep ran. The check that works is against the deleted files
(`git show HEAD:<path>`), not against what is left.

`npm uninstall svelte-sonner mode-watcher`. `npm uninstall` rather than editing
`package.json` by hand, so the lock file moves with it.

`@internationalized/date` also has no importer, and **was left alone**: no
deleted file imported it, so it is not this change's orphan, and it is a common
sibling of `bits-ui`, which is still in use.

**Verify:** `npm run build` succeeds and the bundle is unchanged in size, which
is what says these were never in it.

## [x] 3. Correct §5, which was the thing that was wrong

The table now lists the three components in use, and states plainly that five
were taken and removed, and why. The admission rule — two or more screens —
stays as written; it is what the three survivors satisfy.

Also updated the comment in `App.svelte` that cited §5 as naming `Alert` for the
notice bar, which stopped being true when `Alert` was removed.

**Verify:** neither `Sonner` nor `svelte-sonner` nor `mode-watcher` appears
anywhere outside this log and the changelog.

## [x] 4. Documents

Changelog entry, and this log.

**Verify:** `cargo test --workspace` and clippy clean — the Rust side is
untouched, so this is confirming it stayed that way.
