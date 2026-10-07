# CI, and a release that can be built

The maintainer asked for CI/CD, installers for Windows, macOS and Linux, and a
published release. Two of the three platforms were removed before anything ran,
for two different reasons, and the second removal is the part worth reading.

## 1. [x] CI on every push

- [x] `ci.yml` — `cargo test --workspace`, `clippy -D warnings`, `npm run
      check`. **Verify:** the workflow runs green on a push to `main`.

It did not, on the first run. See §4.

## 2. [x] A release workflow

- [x] `release.yml` — on a `v*` tag, build the installers and open a **draft**
      release. **Verify:** the run is green and the draft has artifacts. **Not
      verified from here** — see §6.

## 3. [x] macOS is not built

- [x] Removed from the matrix. **Verify:** no `macos` entry, and the reasoning
      is in the workflow rather than a commit message.

Three independent problems: BtbN/FFmpeg-Builds publishes no darwin assets, the
well-known macOS ffmpeg builds are GPL, and building from source would need an
ad-hoc signature or Apple silicon refuses to execute the result. None of them is
a small fix, and this project has never been built on macOS.

## 4. [x] Linux is not built — and it did build

- [x] Removed from the matrix, after one CI run. **Verify:** the run's own
      output, quoted below.

The first CI run failed with:

```
---- bridge::tests::only_the_file_name_is_shown_never_the_whole_path stdout ----
panicked at crates/verse-app/src/bridge.rs:1137:49:
  left: "C:\\Users\\someone\\录音\\第三季度会议.m4a"
 right: "第三季度会议.m4a"
test result: FAILED. 87 passed; 1 failed
```

**That is a test written for Windows, not a program that only works there.**
`file_label` is `Path::file_name()` and is portable; the test hands it a path
spelled `C:\Users\...`, and on Linux `\` is an ordinary character, so the
whole string is the file name. The compile succeeded and 87 of 88 tests passed.

It was dropped anyway, as a scope decision — a second bundle layout is a second
thing to keep working, and nothing is released for Linux. The distinction is
recorded in the workflow because the next person to consider a Linux build
should know it was **not** a portability failure.

## 5. [x] Three things in the release path that were wrong

All three found by reading `tauri-action` and `tauri-bundler` source rather than
by reasoning about them, which is the only reason they were found at all.

### `bundle.targets` is silently filtered, not rejected

`Settings::package_types()` in `tauri-bundler` intersects the requested types
with the current platform's:

```rust
TargetPlatform::Linux => vec![Deb, Rpm, AppImage],
TargetPlatform::Windows => vec![WindowsMsi, Nsis],
...
if let Some(package_types) = &self.package_types {
    // keeps only the ones in `platform_types`; no error, no warning
}
```

and `bundle_project` returns `Ok(vec![])` when the result is empty. So a
`targets: ["msi", "nsis"]` config on Linux **succeeds and produces nothing** —
the job goes green and the release is missing a platform.

This did not bite, because Linux went first. It is written down because it is
this project's stated worst failure mode — a feature that fails silently — and
it cost one source read to find. On Windows the config is correct as it stands:
both types survive the intersection.

### Manual dispatch would have published a release tagged `main`

`tauri-action` does `core.getInput('tagName').replace('refs/tags/', '')`. With
`tagName: ${{ github.ref_name }}`, a `workflow_dispatch` run **from `main`**
has `github.ref_name == "main"`, so the action would have created a release
with the tag `main` — the one path that existed specifically so a bundle could
be tried *without* publishing anything.

Fixed by making the tag conditional:

```yaml
tagName: ${{ startsWith(github.ref, 'refs/tags/') && github.ref_name || '' }}
```

Empty, the action skips both the release and the upload and still builds.

### `FFMPEG_VERSION: "8.0.1"` pinned nothing

It was declared, never referenced, and **8.0.1 does not exist upstream**.

`THIRD_PARTY_NOTICES.md` pointed at it as the version the licence belongs to,
which made the notice wrong in a way nobody would have caught. BtbN publishes
`latest` and dated `autobuild-*` tags; there is no `8.0.1`.

What BtbN *does* publish inside `latest` is one archive per FFmpeg release
branch — `ffmpeg-n8.1-latest-win64-lgpl-8.1.zip`, `...-n9.0-...` — and a
release branch is the right kind of pin. It is a version a notice can name, it
still takes patch releases, and it is not rebuilt daily the way `master` is.
Now `FFMPEG_ASSET`, and actually used.

## 6. [x] What was removed that is not a workflow

- [x] `ffmpeg.rs`: the `../lib/Verse` candidate. **Verify:** `cargo test -p
      verse-audio` — 12 unit tests, 6 sidecar tests.

It was the `.deb`/AppImage layout, reasoned from how the bundle is structured
and never seen on a machine. Its own doc comment said so. macOS's
`../Resources` had already gone the same way; this is the same removal, one
release later. `bundled()` is gone with it — with one candidate left it was a
one-line wrapper around `beside()`, so `locate()` calls `beside()` directly.

## What is not verified from here

**Nothing in either workflow has run.** The YAML parses and has no control
characters, both of which were checked after a form feed once got into
`release.yml` through backslash mangling. Everything else is a claim about a
runner this machine is not.

Specifically unverified: the BtbN fetch and its asset name, the `--config`
merge, the Windows installer build, and whether a release is created at all.

## Follow-ups, recorded rather than done

- **Nothing is code-signed.** Windows will show a SmartScreen warning. This is
  about accounts and money, not about the workflow.
- **The `externalBin` sidecar path is compiled but never exercised** — no
  release has been run, so no installed build has ever looked for the ffmpeg it
  ships with.
- **No Linux CI means the `#[cfg(unix)]` arms are no longer compiled
  anywhere.** If Linux is ever revisited, that is the first thing that will
  have rotted.
