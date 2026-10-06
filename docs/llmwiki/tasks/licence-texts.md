# Shipping the licence texts

`THIRD_PARTY_NOTICES.md` has carried a release checklist since it was written.
The previous round closed the attribution item in the interface and left the
rest, on the grounds that GitHub was unreachable from this machine: the licence
file beside the model on hf-mirror is 71 bytes reading "Ref to
https://github.com/modelscope/FunASR", and it points at a host that does not
answer from mainland China.

That blocker is gone. A proxy is up, `github.com` answers in 0.87 s, and the
texts can be fetched and archived as §6 of the model licence requires — the
agreement "may be updated and revised occasionally", so the copy we agreed to
is only evidence if we keep it.

**Scope: the two checklist items that were blocked on reachability, and the
packaging change that makes "ship the text" true rather than nominal.** The
revision-pinning item is not in this round; it is a change to the downloader
and every mirror URL, and it is named at the end.

---

## [x] 1. Fetch and archive the texts

Three, not one. The model licence is the obligation; the other two were
"not yet verified" in the notices file and can now stop being.

| what | upstream | why |
|---|---|---|
| FunASR Model License v1.1 | `modelscope/FunASR/blob/main/MODEL_LICENSE` | the obligation — weights |
| Silero VAD | `snakers4/silero-vad/blob/master/LICENSE` | weights, status unknown |
| sherpa-onnx | `k2-fsa/sherpa-onnx/blob/master/LICENSE` | linked library |

Archived under `licences/` with the URL, the fetch date and the SHA-256 of the
bytes we took, because "we ship the licence" is worth nothing if nobody can
check which revision of it we shipped.

**Verify:** each archived file's SHA-256 matches the hash of the fetch, and the
FunASR text is the agreement itself rather than the repository's MIT code
licence — those are different documents and the repo serves both.

## [x] 2. Make "ship the licence" mean something in the installer

`crates/verse-app/tauri.conf.json` declared `bundle.targets` and no
`bundle.resources`. Nothing beyond the binary reached the `.msi` or the `.exe`,
so a licence file in the repository was shipped to *us*, not to a user. All four
files are now listed in `bundle.resources`, by name rather than by glob — a glob
that silently matches nothing is this project's recurring failure.

**A second gap was behind the first, and it was worse.** The build failed with
`Couldn't find a .ico icon`. The icon set was already committed under
`crates/verse-app/icons/` — a full Tauri set including `icon.ico` — but
`bundle.icon` was never wired up, so nothing referenced it. Absent at HEAD too:
**this project had never once produced an installer**, and the missing
`bundle.resources` was not the reason anyone would have found first.

**Verified end to end, by reading what the bundler wrote rather than trusting
the config.**

`target/release/wix/x64/main.wxs` — the definition `light` compiled — carries a
`licences` directory holding all four files:

```
<Directory Name="licences">
  <File ... Source="...\licences\FunASR-Model-License-1.1.txt" Name="FunASR-Model-License-1.1.txt" />
  <File ... Source="...\licences\Silero-VAD-MIT.txt"           Name="Silero-VAD-MIT.txt" />
  <File ... Source="...\licences\sherpa-onnx-Apache-2.0.txt"   Name="sherpa-onnx-Apache-2.0.txt" />
  <File ... Source="...\THIRD_PARTY_NOTICES.md"                Name="THIRD_PARTY_NOTICES.md" />
</Directory>
```

`target/release/nsis/x64/installer.nsi` agrees, and removes them again on
uninstall:

```
CreateDirectory "$INSTDIR\licences"
File /a "/oname=licences\FunASR-Model-License-1.1.txt" "..."
...
RMDir /REBOOTOK "$INSTDIR\licences"
```

Both bundles were produced — `Verse_0.1.0_x64_en-US.msi` (10.55 MiB) and
`Verse_0.1.0_x64-setup.exe` (7.43 MiB) — and the staged copies hash-match the
archived originals, so the bytes that reach a user are the bytes that were
fetched:

| file | archived | in the installer |
|---|---|---|
| `FunASR-Model-License-1.1.txt` | `7dba975a2069691d` | match |
| `Silero-VAD-MIT.txt` | `2e63e9a38b6e8fc0` | match |
| `sherpa-onnx-Apache-2.0.txt` | `cfc7749b96f63bd3` | match |

One thing this does *not* prove: that the installer runs. It was built, not
installed, and installing it would replace whatever the maintainer has.

**A note on the first attempt.** The first `tauri build` was launched from
`crates/verse-app/ui`, where the CLI cannot find the config, and it panicked —
but the pipeline ended `| tail`, so the shell reported exit 0 and the failure
was invisible until the log was read. The same shape as the stale-binary
finding this project already records: a command that looks like it succeeded.

## [x] 3. Correct the notices

- Silero VAD stops being "not yet verified". It is MIT, `Copyright (c)
  2020-present Silero Team`, and the README says so in prose as well as in the
  licence file — record the badge discrepancy rather than the claim it looks
  like, since the README's own badge renders "CC BY-NC 4.0" for an MIT file.
- The checklist gains the file paths, so a release check is a `ls` rather than
  a research task.

**Verify:** every licence claim in the file names a document that exists in
`licences/` or an upstream URL that resolves.

## [x] 4. The dialog stops saying the text is missing

`about.rs::LICENCE_NOTE` says the text is 尚未内置, which was true and is not
any more. Its test asserts the string "尚未内置" is present, so the test has to
change with it — the test is what makes the sentence honest, and a test that
pins a stale claim pins the wrong thing.

**Verify:** the note names where the text is, and the test asserts that
instead; the attribution rows are untouched.

## [x] 5. Record what is still open

The revision-pinning item stays unchecked, with the reason: `models.json`
resolves every file through `.../resolve/main/`, which is a moving target, and
§6 lets the licence move under it. Pinning needs a revision per mirror
(HuggingFace takes a commit SHA; ModelScope's revision semantics are its own)
and a downloader that can express it.

**Verify:** the checklist item is still `[ ]` and the text under it says what
would have to change.

---

## Out of scope, and why

**Model revision pinning.** Named above. It is a downloader change with a
per-mirror answer and its own round.

**FFmpeg distribution.** The notices file lists three options and says it needs
a decision, not a default — bundling an LGPL build, bundling a GPL build, or
requiring the user to install it. That is the maintainer's call, and it is
unchanged by this round.
