# Third-party notices

Verse bundles or downloads third-party components. This file records what each
one requires of us. **Read it before packaging a release.**

The distinction that matters: some components are *linked or invoked* (code),
others are *downloaded as data* (model weights). Licenses often differ between
the two, even for the same project.

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
   it, with the product.
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

License status **not yet verified**. Confirm before shipping. Upstream is
<https://github.com/snakers4/silero-vad>.

### sherpa-onnx — library

**Apache-2.0** for the toolkit. Note this covers the *code*; the model weights
it hosts carry their own licenses, which is the entire point of the SenseVoice
entry above.

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

**Before shipping: decide how ffmpeg reaches the user.** Three options, in
increasing order of legal comfort:

1. Require the user to install ffmpeg themselves — no distribution, no
   obligation, but it breaks the "install and run" goal.
2. Bundle an **LGPL** build and ship its license and source offer.
3. Bundle a GPL build and accept the GPL's terms for the distribution.

Option 2 is the likely target. This needs a decision, not a default.

Upstream: <https://ffmpeg.org/legal.html>

---

## Development-only

These are used to build and test, and are not distributed with the product:

- **Rust crates** — see `Cargo.lock`; each crate carries its own license.
- The model repositories' own `test_wavs/` audio, used only as test input.

---

## Checklist for a release

- [ ] Attribution to Alibaba / FunAudioLLM present in the UI or docs
- [ ] "SenseVoiceSmall" name retained in distributed metadata
- [ ] FunASR Model License v1.1 text shipped, pinned to the model revision used
- [ ] Silero VAD license verified
- [ ] FFmpeg distribution strategy decided and its license honoured
- [ ] Model revisions pinned so the archived licenses still correspond
