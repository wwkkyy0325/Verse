# When a model becomes available, and where it lives

Two reports from the maintainer, the day after the first release went out:

> 有个严重bug，我下载完模型得重启应用，而不是自动变成可选中可使用
>
> 还有卸载的时候没删除模型，模型目录残存了

They turned out to be different faults with one word in common — a model that
the program and the window disagreed about — and the second one was worse than
it sounded.

## 1. [x] The panel did not learn that a download had finished

- [x] `App.svelte`: re-read the catalogue when a download reaches `ready`, and
      after a model is taken from a folder. **Verify:** `bash tools/ci.sh` —
      `svelte-check` 597 files, 0 errors; 388 tests, clippy clean.

The card is gated on `model.present`, and the card is what decides whether a
model can be chosen. `present` comes from the `models` command, which computes
it from what is on disk — **and which the window called exactly once, at
startup.** Nothing refreshed it. So a download that finished while the window
was open wrote 1 GB to disk, told the backend, and left the card saying 需要下载
and refusing to be picked until the next launch.

The same held for `importModel`, the other way a model can arrive.

`refreshCatalog()` is now called from both. It re-asks the backend rather than
setting `present = true` locally, because the backend's answer comes from the
files and a local guess would drift the first time a model was removed from
underneath it.

**A comment in `fetch_model` claimed this was already handled:**

```rust
// The panel's card has to stop saying 需要下载, whether or not a job
// was waiting on it.
bridge::push_state(&app);
```

`push_state` pushes the file roster and the current screen. It carries no part
of the catalogue. The comment described an intention the code did not have, and
is now corrected to say what the push is actually for.

## 2. [x] Models were stored in the install directory

- [x] `models_dir()` resolves under the per-user data directory.
      **Verify:** three new tests in `crates/verse-app/src/lib.rs`.
- [x] The resolution is a pure function of the environment, so the fallbacks are
      testable. **Verify:** `models_live_under_the_users_data_directory`,
      `an_explicit_model_directory_beats_the_data_directory`,
      `models_follow_the_data_directorys_own_fallbacks`.

This is the maintainer's "模型目录残存了", and the leftover was the smaller half
of it.

**The `.msi` installs per machine.** Read out of Tauri's WiX template rather
than assumed:

```
InstallScope="perMachine"
<?define PlatformProgramFilesFolder = "ProgramFiles64Folder" ?>
<Directory Id="INSTALLDIR" Name="{{product_name}}"/>
```

So an `.msi` install lands in `C:\Program Files\Verse`, and models resolved to
`C:\Program Files\Verse\models`. **A standard user cannot create a directory
there.** A `.msi` install could not download a model at all — no error anyone had
seen, because the `.exe` installer takes the other path: Tauri's NSIS default is
documented as "a directory that doesn't require Administrator access", which is
`%LOCALAPPDATA%\Verse`, where "beside the executable" happened to already be the
right answer.

One of the two installers could not have shown the fault. That is the whole
reason it survived until someone uninstalled.

`ui-design.md` §11 had asked this question and half-answered it in October: the
mechanism for a per-user directory already existed and the cache, the resume logs
and the history already used it. Models were the last thing not doing so. The
answer is now `models_root()` — `VERSE_MODELS`, else `<data dir>/models` — and
§11 records that the question is closed.

No migration, and none needed: 0.1.0 is the first release, there are no installs
to migrate, and a source checkout is already pointed at its own `models/` by
`VERSE_MODELS`.

## 3. [x] The uninstaller gets to ask

- [x] `crates/verse-app/nsis-hooks.nsh`, wired through
      `bundle.windows.nsis.installerHooks`. **Verify:** built an installer here
      and watched `makensis` compile the file — see below.

The maintainer's answer to "what should uninstall do with 1.2 GB of weights":

> 卸载哪里有选项目，勾选了就一起删，不勾选就不删

**Tauri's uninstaller already has that checkbox.** It is on the confirm page,
it is labelled "Delete app data", and ticking it removes `%APPDATA%\<bundle
id>` — a directory this application has never written to. So the box existed,
did what it said, and removed nothing: an empty folder, while the weights stayed.

`NSIS_HOOK_POSTUNINSTALL` is expanded *after* the template's own block reads
`$DeleteAppDataCheckboxState`, so the hook can read the same variable and remove
`%LOCALAPPDATA%\Verse` when — and only when — the box was ticked. In a silent
uninstall the page never runs and the variable stays 0, so nothing is removed
without a person saying so.

The hook is not the checkbox. It is four lines that make the checkbox true.

## 4. [x] The hook was compiled before it was trusted

- [x] Falsified on purpose. **Verify:** an invalid command appended to
      `nsis-hooks.nsh` fails the build; removing it passes.

```
Invalid command: "ThisIsNotANSISCommand"
!include: error in script: "...\crates\verse-app\nsis-hooks.nsh" on line 32
```

This is the point of doing it. A `.nsh` that is `!include`d but never reached
would compile silently — the template guards every hook with `!ifmacrodef` — and
shipping one would repeat, exactly, the mistake that cost a release run two days
earlier: a shell step that had never been executed anywhere. `makensis` succeeded
with the file, which is not the same as the file being read; the invalid line is
what tells the two apart.

**A side-confirmation, unasked for.** The debug NSIS bundle built here with no
ffmpeg attached is **9.02 MiB**. That is the same arithmetic used earlier to
argue that the 47.5 MiB release installer carries the 128 MiB ffmpeg, and it is
the first time the "without it, the number would be single digits" half of that
argument has been measured rather than computed.

## What is not verified

- **The uninstall itself.** The script compiles and the hook is at the right
  place in the uninstall section; nobody has installed the result and unticked
  and ticked that box. That is a real installation on a real machine and it has
  not happened.
- **The refresh, end to end.** There is no frontend test runner in this
  repository — `package.json` has `check` and no `test` — so the fix is checked
  by types and by reading, and the behaviour needs a click: download a model
  with the window open, and watch the card become 已安装 without a restart.
- **The `.msi` path.** The per-machine install is the reason for this work and
  the only install that was ever broken. Nobody has run one.
