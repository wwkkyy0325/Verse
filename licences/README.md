# Licence texts

The verbatim texts this product is obliged to pass on, archived rather than
linked. `THIRD_PARTY_NOTICES.md` says what each one requires of us; this
directory is the paperwork itself.

## Why they are copied rather than pointed at

FunASR Model License §6 lets the agreement be revised unilaterally, with
continued use taken as acceptance:

> This agreement may be updated and revised occasionally. The revised
> agreement will be published in the official repository of [FunASR Software]
> and will take effect automatically.

A link therefore records nothing. It shows whatever the agreement says the next
time someone follows it, which is not necessarily what we agreed to. The copy
below, with its SHA-256, is the evidence of the terms we accepted.

## What is here

The first three were fetched on **2026-10-06**; the two FFmpeg texts on
**2026-10-08**, the day ffmpeg started being bundled rather than expected on
`PATH`. The SHA-256 is of the file as archived, so a release check can confirm
the bytes have not been edited in place.

| file | upstream | upstream HEAD at fetch | SHA-256 |
|---|---|---|---|
| `FunASR-Model-License-1.1.txt` | [modelscope/FunASR `MODEL_LICENSE`](https://github.com/modelscope/FunASR/blob/main/MODEL_LICENSE) | `66d7a4c264a5993a2a63ed00c1f402c296ee521a` | `7dba975a2069691db4992b0592d70828b330d2f8a30a71450f4e152a554e84f8` |
| `Silero-VAD-MIT.txt` | [snakers4/silero-vad `LICENSE`](https://github.com/snakers4/silero-vad/blob/master/LICENSE) | `1e261b036686cd0017d500ee96acd1c4ba572a9d` | `2e63e9a38b6e8fc0c7bc37ce174caca1862870856c6daf5697cfb785e925520b` |
| `sherpa-onnx-Apache-2.0.txt` | [k2-fsa/sherpa-onnx `LICENSE`](https://github.com/k2-fsa/sherpa-onnx/blob/master/LICENSE) | `99ddefaa92129858b80a71a426903dd4215c83fa` | `cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30` |
| `LGPL-3.0.txt` | [FFmpeg `COPYING.LGPLv3`](https://github.com/FFmpeg/FFmpeg/blob/master/COPYING.LGPLv3) | `7ef5a794c42abcb4a62e1ab22f4ae3ce40aee729` | `da7eabb7bafdf7d3ae5e9f223aa5bdc1eece45ac569dc21b3b037520b4464768` |
| `GPL-3.0.txt` | [FFmpeg `COPYING.GPLv3`](https://github.com/FFmpeg/FFmpeg/blob/master/COPYING.GPLv3) | `7ef5a794c42abcb4a62e1ab22f4ae3ce40aee729` | `8ceb4b9ee5adedde47b31e975c1d90c73ad27b6b165a1dcd80c7c545eb65b903` |

**The GPL text is here because the LGPL one is not enough on its own.** LGPLv3
§3 applies the terms of GPLv3 to the combined work, so a distribution that
carries an LGPLv3 binary and only the lesser licence is carrying half of what
it is required to pass on. Both are shipped.

They come from `FFmpeg/FFmpeg` rather than from gnu.org — not because gnu.org is
wrong, but because FFmpeg's copy is the one attached to the work being
distributed, and it is the copy FFmpeg's own `LICENSE.md` points at. gnu.org was
also unreachable from here, which made the choice for us; the provenance is
recorded either way.

The commit is the repository head at the moment of the fetch, not the commit
that last touched the file. It says when we looked; the hash says what we got.
Where the two disagree, the hash is the one that matters.

## Two things worth knowing about these files

**The FunASR repository serves two different licences, and only one of them is
ours.** `LICENSE` at the repository root is MIT and covers the *code*;
`MODEL_LICENSE` is the custom agreement and covers the *weights*. We use the
weights, so `MODEL_LICENSE` is the operative document — and it is the one whose
terms are stricter, which is why the distinction is worth writing down rather
than assuming.

**Silero VAD's README renders its own licence incorrectly.** The badge in
`snakers4/silero-vad`'s README is labelled "CC BY-NC 4.0" while linking to the
MIT `LICENSE` file. The file is MIT, the README says so in prose —

> Published under permissive license (MIT) Silero VAD has zero strings
> attached

— and the badge is stale. MIT is taken here, on the strength of the licence
file and the prose, not the badge. Anyone re-verifying this should expect the
badge to look alarming and should not act on it without reading the file.

## Not here

- **Rust crates** — licences are per-crate and recorded in `Cargo.lock`.
- **Tauri, WebView2** — no attribution obligation that requires a file here.
- **The ffmpeg binary itself** — it is not source, and the offer of source is a
  URL in `THIRD_PARTY_NOTICES.md` rather than a file. A link is enough there
  because what it points at is a release, not an agreement that can be revised.
