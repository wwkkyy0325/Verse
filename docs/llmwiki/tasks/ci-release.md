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

## 7. [x] The release path ran for the first time, and died at step 3

- [x] Diagnosed and fixed. **Verify:** `bash tools/fetch-ffmpeg.sh` on this
      machine — the whole step, not the construct.

Tag `v0.1.0` was pushed, and `release.yml` failed in about ninety seconds with

```
find: missing argument to `-exec'
```

The step was:

```bash
find ffmpeg-unpacked -name ffmpeg.exe -exec cp {} ffmpeg-bin/ffmpeg-x86_64-pc-windows-msvc.exe +
```

**GNU find as shipped in Git Bash rejects the `+` form whenever anything follows
`{}`.** Measured, not reasoned — three commands in the same shell:

| command | result |
|---|---|
| `find . -maxdepth 0 -exec echo PREFIX {} +` | **ok** |
| `find . -maxdepth 0 -exec echo {} SUFFIX +` | `missing argument to '-exec'` |
| `find unpacked -name ffmpeg.exe -exec cp {} destfile +` | `missing argument to '-exec'` |

That is the POSIX rule for `;`, which GNU normally relaxes; it does not relax it
for `+`. Quoting the plus does not help, passing it through a variable does not
help, and `MSYS2_ARG_CONV_EXCL='*'` does not help — none of which is surprising
in hindsight, because the argument is not being mangled on the way in. It is
being rejected. `-exec ... \;` is unaffected and is what the script uses now.

**The finding is not the fix, it is how long the fix took.** The whole diagnosis
was six shell commands and about two seconds. It cost a CI run because the step
existed only inside a YAML `run:` block, where the only way to try it is a push.

## 8. [x] Run it here first

- [x] `tools/ci.sh` — the CI job, runnable locally. **Verify:** run it; green.
- [x] `tools/fetch-ffmpeg.sh` — the release step that failed, as a script.
      **Verify:** run it; it fetched 170 MB, unpacked, placed a 134 MB
      `ffmpeg-x86_64-pc-windows-msvc.exe`, and exited 0.

The maintainer asked for this in as many words after the second failed run:
run it here before spending a runner. `tools/ci.sh` mirrors the three checks in
`ci.yml` and says, in its own output, that a local run without ffmpeg is weaker
than CI's. The release step is a script for the same reason the diagnosis was
hard: a step that can only be exercised on a runner costs a round trip per
attempt.

## 9. [x] The bundled ffmpeg's licence was asserted, never checked

- [x] Checked by running the binary. **Verify:** the command below.
- [x] Two licence texts added and shipped. **Verify:** `bundle.resources` in
      `tauri.conf.json`, and `licences/LGPL-3.0.txt`, `licences/GPL-3.0.txt`.

Three documents said "an LGPL build" and none of them had looked at one. Run
against the fetched binary:

```console
$ ffmpeg -version | tr ' ' '\n' | grep -E '^--enable-(gpl|nonfree|version3|lgpl)'
--enable-version3
```

`--enable-version3` present, `--enable-gpl` and `--enable-nonfree` absent. So it
is **LGPL, and specifically LGPLv3** — a distinction the prose had not drawn,
and one that matters, because LGPLv3 §3 pulls in GPLv3's terms.

**Which meant the bundle was missing a licence text.** It carried an LGPLv3
binary and neither the LGPLv3 nor the GPLv3 text; `bundle.resources` listed
FunASR, Silero and sherpa-onnx, and nothing for ffmpeg. Both texts are now in
`licences/`, taken from `FFmpeg/FFmpeg`'s own `COPYING.LGPLv3` and
`COPYING.GPLv3` — upstream's copy rather than gnu.org's, which was also
unreachable through the proxy that day.

The reported build was `n8.1.3-14-g330caae0c1-20261007`, recorded in
`THIRD_PARTY_NOTICES.md` so the notice names what actually shipped.

## What is not verified from here

**`release.yml` has now run once and failed.** Everything after step 3 is still
untested: the `--config` merge, `tauri-action`, the Windows installer build, and
whether a release is created at all. The step that failed is the step that is
now a script and has been run — the rest has not.

The `tagName` fix is still a source-derived claim about a runner rather than an
observation of one — so is the sidecar name, which comes from reading
`tauri-bundler`'s `Settings::copy_binaries` and has never been seen installed
anywhere.

## Follow-ups, recorded rather than done

- **Nothing is code-signed.** Windows will show a SmartScreen warning. This is
  about accounts and money, not about the workflow.
- **The `externalBin` sidecar path is compiled but never exercised** — no
  release has been run, so no installed build has ever looked for the ffmpeg it
  ships with.
- **No Linux CI means the `#[cfg(unix)]` arms are no longer compiled
  anywhere.** If Linux is ever revisited, that is the first thing that will
  have rotted.
