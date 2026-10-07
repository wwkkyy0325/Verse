# Third-party notices

Verse bundles or downloads third-party components. This file records what each
one requires of us. **Read it before packaging a release.**

The distinction that matters: some components are *linked or invoked* (code),
others are *downloaded as data* (model weights). Licenses often differ between
the two, even for the same project.

The texts themselves are archived verbatim in [`licences/`](licences/), with
the upstream commit, the fetch date and the SHA-256 of each. This file says what
each one requires; that directory is the paperwork.

---

## Bundled or downloaded at runtime

### SenseVoice-Small — model weights

**License: FunASR Model Open Source License Agreement v1.1** (custom, not
Apache-2.0, not OSI-approved).

The widespread "apache-2.0" label for this model is **wrong**. It originates
from a ModelScope metadata field; the official HuggingFace card has always said
`license: other` pointing to the same custom agreement, and both GitHub repos
agree. Do not rely on the Apache tag.

What it requires of us:

1. **Attribute the source.** Credit Alibaba Group / FunAudioLLM in the app's
   about box or credits screen.
2. **Retain the model name.** "SenseVoiceSmall" must remain in the distributed
   metadata; do not strip or rename it.
3. **Ship the license text.** Include this agreement, or a NOTICE pointing to
   it, with the product. **Done**: `licences/FunASR-Model-License-1.1.txt`,
   listed in `crates/verse-app/tauri.conf.json` under `bundle.resources` so it
   reaches the installer rather than only the repository.
4. **Do not disparage the project.** Section 4.2 terminates the license
   automatically on "unjustified denigration, malicious smearing, or baseless
   insults".

Known risks, accepted with eyes open:

- Section 3 says the software is "for reference and learning purposes only",
  which reads as a non-commercial restriction. The maintainers have published a
  clarification stating it is a risk disclaimer rather than a usage
  restriction, and that commercial use of the official weights is permitted.
  Be aware the clause is still there.
- Section 6 lets the license be revised unilaterally, with continued use
  implying acceptance. **Pin the model revision and archive the license text as
  fetched**, so the terms we agreed to remain on record.
- Section 7 leaves the governing law blank.

Upstream: <https://github.com/modelscope/FunASR/blob/main/MODEL_LICENSE>

### Silero VAD — model weights

**MIT**, `Copyright (c) 2020-present Silero Team`. Verified against the upstream
licence file and archived as `licences/Silero-VAD-MIT.txt`.

**Read the file, not the badge.** The README in `snakers4/silero-vad` renders a
badge labelled "CC BY-NC 4.0" while linking to the MIT `LICENSE`. The licence
file is MIT and the README says so in prose — "Published under permissive
license (MIT) Silero VAD has zero strings attached — no telemetry, no keys, no
registration, no built-in expiration, no keys or vendor lock" — so the badge is
stale. Anyone re-verifying this should expect the badge to look alarming.

Upstream: <https://github.com/snakers4/silero-vad>

### sherpa-onnx — library

**Apache-2.0** for the toolkit, archived as
`licences/sherpa-onnx-Apache-2.0.txt`. Note this covers the *code*; the model
weights it hosts carry their own licenses, which is the entire point of the
SenseVoice entry above.

Upstream: <https://github.com/k2-fsa/sherpa-onnx>

### Tauri — application shell

**MIT OR Apache-2.0**, the same terms as this project. No attribution
obligation, no copyleft, nothing to discharge in the interface. This is the
reason the shell is Tauri rather than a GUI toolkit with a bespoke licence.

Upstream: <https://github.com/tauri-apps/tauri>

### WebView2 — required at runtime on Windows

Tauri does not ship a browser engine. It renders the interface in the system
webview, which on Windows is **Microsoft Edge WebView2** — a Microsoft
component, distributed under Microsoft's own terms, free to use and
redistribute.

Where it comes from, and the reason this is listed as a *requirement* rather
than a *dependency*:

- **Windows 11** — preinstalled.
- **Windows 10, version 1803 or later** — distributed as part of the operating
  system.
- **Anything older, including Windows 7 and 8** — must be installed. Tauri's
  installer can bootstrap it, but Microsoft has ended WebView2 support on
  Windows 7, so this is a genuine floor rather than a formality.

**This is the constraint to weigh against the "runs on old machines" goal**
(`design.md` §3, C4). The pipeline itself is happy on a 2013 CPU; the shell is
not happy on a 2013 *operating system*. Windows 10 1803 shipped in April 2018.
Decide before shipping whether that floor is acceptable, and say so in the
installer rather than failing at launch.

Upstream: <https://learn.microsoft.com/microsoft-edge/webview2/>

### Paraformer-large — model weights (no longer used)

**Apache-2.0**, and the cleanest license of the bunch. It was the fallback
engine until it was removed: measured across five domains it won 0.6–3.1 points
of character accuracy and lost 1–8 points of punctuation while taking 2.3× as
long, so keeping a third option cost more than it returned. Nothing here ships
these weights any more; the section is kept because the licence comparison is
still the reason the choice was worth measuring.

---

## Required at runtime, not bundled

### FFmpeg — invoked as a subprocess

**License depends on the build.** Most Windows builds in circulation — including
the `gyan.dev` "essentials" build used during development — are **GPL**
builds. A GPL build of FFmpeg has obligations that an LGPL build does not.

We invoke `ffmpeg` as a **child process** and communicate over pipes; we do not
link against its libraries. That is the arrangement most commonly held to keep
the caller independent of FFmpeg's license. It is also the reason no FFmpeg
code is compiled into Verse.

**Decided: an LGPL build is bundled (option 2).** The alternative was to
require the user to install ffmpeg, which breaks the "install and run" goal the
whole project is built around.

The source is **BtbN/FFmpeg-Builds**, whose release listing publishes `-lgpl`
archives for Windows and Linux — checked rather than assumed: `win64`,
`winarm64`, `linux64` and `linuxarm64` all exist in both static and shared
forms. The static one is fetched, so the bundle carries a single file.

**That project publishes nothing for macOS**, and the well-known macOS ffmpeg
builds elsewhere are GPL. Rather than accept a GPL binary on one platform or
build ffmpeg from source there, **macOS is not supported**. See the release
workflow for the whole of that reasoning.

The licence text ships with the product, and the source offer it needs is:

- ffmpeg itself — <https://ffmpeg.org/releases/> (the version is pinned in the
  release workflow as `FFMPEG_VERSION`)
- how that binary was built — <https://github.com/BtbN/FFmpeg-Builds>

The bundled file is a **sidecar**: `tauri.conf.json` declares it under
`externalBin`, and `tauri-bundler` installs it beside the program with the
target-triple suffix stripped. `verse-audio` looks for it there before `PATH`,
so an installed copy uses the ffmpeg it shipped with rather than whatever the
machine happens to have.

Upstream: <https://ffmpeg.org/legal.html>

---

## Development-only

These are used to build and test, and are not distributed with the product:

- **Rust crates** — see `Cargo.lock`; each crate carries its own license.
- The model repositories' own `test_wavs/` audio, used only as test input.

---

## Checklist for a release

- [x] Attribution to Alibaba / FunAudioLLM present in the UI or docs
      — the 关于 dialog; `crates/verse-app/src/about.rs`
- [x] "SenseVoiceSmall" name retained in distributed metadata
      — `models.json` and the engine descriptor carry "SenseVoice-Small",
      which is upstream's own prose spelling (the model card writes both, and
      its `model_dir` is `FunAudioLLM/SenseVoiceSmall`). The name is retained;
      the hyphen is upstream's, not a tidying-up.
- [x] FunASR Model License v1.1 text shipped — `licences/`, listed under
      `bundle.resources`
- [x] Silero VAD license verified — MIT
- [ ] **FunASR Model License v1.1 text corresponds to the model revision
      used.** The text is archived, but the weights it governs are fetched
      through `.../resolve/main/`, so "the revision used" is whatever the host
      served that day. Fixing this is the next item; splitting them keeps a
      done thing from looking undone and an undone thing from looking done.
- [ ] Model revisions pinned so the archived licenses still correspond.
      `crates/verse-model/models.json` resolves every file through `/main/` or
      `/master/`. Needs a revision per mirror — HuggingFace accepts a commit
      SHA in `resolve/<sha>/`, ModelScope's revision semantics are its own —
      and a downloader that can express one.
- [ ] FFmpeg distribution strategy decided and its license honoured
