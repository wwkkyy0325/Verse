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

## 10. [x] The release built, on the second run

- [x] Run 2 of `release.yml`, on `cc45f87`. **Verify:** every step green —
      quoted below.

| step | |
|---|---|
| 1 Set up job | success |
| 2 `actions/checkout@v4` | success |
| **3 ffmpeg — the LGPL build** | **success** |
| 4 Declare the bundled ffmpeg for this build | success |
| 5 `dtolnay/rust-toolchain@stable` | success |
| 6 `Swatinem/rust-cache@v2` | success |
| 7 `actions/setup-node@v4` | success |
| 8 Frontend dependencies | success |
| **9 Build and attach** | **success** |
| 16–18 the three post steps | success |
| 19 Complete job | success |

Step 3 is the one that died in ninety seconds on run 1. It takes about that long
to fail and a good deal longer to succeed — the whole run was around nine
minutes, and the ffmpeg step passing was visible from the job page while
`Build and attach` was still going.

The only annotation on the job is a warning: `actions/checkout@v4` and
`actions/setup-node@v4` target Node.js 20 and were forced onto Node.js 24. Not
acted on here — changing the actions means another release run, and the warning
is not an error yet. It will become one.

**The draft release, on inspection:** `Verse v0.1.0`, drafted by
`github-actions`, with

| asset | size | |
|---|---|---|
| `Verse_0.1.0_x64-setup.exe` | 47.5 MB | NSIS |
| `Verse_0.1.0_x64_en-US.msi` | 63.7 MB | WiX |
| `Source code (zip)` / `(tar.gz)` | | GitHub adds these to every release |

**And the ffmpeg is inside it, which is the part that had never been shown.**
The installer size is the evidence, and it is arithmetic rather than a guess:

| | bytes | `gzip -9` |
|---|---|---|
| `verse-app.exe` | 29,945,856 | 10.3 MiB |
| `ffmpeg-x86_64-pc-windows-msvc.exe` | 134,092,288 | |
| both together | 164,038,144 | 63.1 MiB |

NSIS compresses with LZMA, typically 15–25% under `gzip -9`, which puts the
expected installer near 49 MiB against an actual 47.5 MiB. Without the ffmpeg
the whole payload gzips to 10 MiB and the installer would be single digits.
Where the file *lands* is still unverified — that needs an install — but it is
being packaged.

### The wrong turn, recorded because it was expensive

**A report that the release had no binaries was accepted too readily, and the
reasoning built on it was wrong.** `GET /repos/{owner}/{repo}/releases`
**excludes drafts from unauthenticated requests**, and there were *two* releases
on the tag: the maintainer's, made by hand and published, carrying nothing; and
the workflow's, a draft, carrying both installers. Only the first was visible
from here.

Reading `tauri-action`'s source gave what looked like a proof — a throw on empty
artifacts, a `core.setFailed` around the whole body, therefore "no upload means
`tagName` was empty" — and it contradicted `head_branch: v0.1.0`, which said the
ref *was* a tag. **That contradiction was the finding.** It meant the model was
missing something, and the something was the second release.

The lesson is narrower than "read the source": a proof built on *everything I
can see* is only as good as the inventory, and the inventory had a hole in it
that the API documents on the page nobody reads. The step log settled it in one
line — `Couldn't find release with tag v0.1.0. Creating one.`

## What is still not verified

- **The installed program.** No release has been downloaded and run, so
  `beside(exe)` — the lookup for the ffmpeg the installer is supposed to place —
  has still never executed against a real installation. It is the one part of
  the sidecar chain that a local run cannot reach.
- **The licence files inside the bundle.** `bundle.resources` names six paths and
  all six exist in the repository, but nothing here has opened an installer to
  see where they land.
- **Where the installer puts things.** The package demonstrably carries the
  ffmpeg; nothing has run it. That and the entry above are the same missing
  step, and it is the only one left.
- **A tag is now load-bearing.** `v0.1.0` had to be moved from `ff12817` to
  `cc45f87` because the first run failed. Nothing was published from either, so
  the move cost nothing — but the next tag will not have that luxury.

## Follow-ups, recorded rather than done

- **Nothing is code-signed.** Windows will show a SmartScreen warning. This is
  about accounts and money, not about the workflow.
- **The `externalBin` sidecar path is compiled but never exercised** — no
  release has been run, so no installed build has ever looked for the ffmpeg it
  ships with.
- **No Linux CI means the `#[cfg(unix)]` arms are no longer compiled
  anywhere.** If Linux is ever revisited, that is the first thing that will
  have rotted.
