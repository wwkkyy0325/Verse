# What the first real installation showed

The maintainer ran the 0.1.0 build on a machine that was not this one and
reported three things. All three are things no development machine could have
shown, which is the theme.

A fourth came out of fixing the second, and is §4: a file waiting for a model
was not written to `pending.json`, so quitting lost the row.

## 1. [x] A black console window, three times

**Reported as:** 应用运行时会弹出黑框，闪烁两个，第三个常驻不消失，识别运行完才消失.

`verse-app` is a GUI program — `#![cfg_attr(not(debug_assertions),
windows_subsystem = "windows")]` in `main.rs` — and ffmpeg is a console program.
Windows therefore gives each spawned ffmpeg a console of its own, and it is
drawn: two that flash by (`locate`'s `-version` probe on `PATH`, then
`version()`'s own) and one that stays up for as long as the job runs (the
decoder, which lives for the length of the file).

**Fixed with `CREATE_NO_WINDOW` on every spawn**, through one helper in
`ffmpeg.rs` so a fourth call site cannot be added without it. The flag
suppresses the console without detaching the process, so pipes and exit codes
are unaffected.

**Verify:** one new test that the helper still runs ffmpeg and reads its output;
`tools/ci.sh`; and — the one that matters — the installed build, counting black
boxes by eye. The decoder and transcoder paths are exercised by
`tests/ffmpeg_sidecar.rs`, which now goes through the helper, so "the flag does
not break stdio" is asserted rather than assumed.

## 2. [x] A file dragged in while the model is missing, stuck for good

**Reported as:** 如果模型没下载或者没下完，期间拖入的文件处理会卡住，就是得取消然后重新拖入.

**Root cause: `file_chosen` only guarded the job slot, not the wait for a
model.** `running` is set when a job starts and a file on `NeedsModel` sets
nothing, so a second drop found the slot free and took it — the second file
became the shown screen and the *first* kept a `NeedsModel` screen nothing
would ever move again.

Two visible symptoms, one cause:

- `download_changed` writes to `AppState::active`, which is the selected
  screen. The second drop moved the selection, so the first file's download
  progress froze where it was. *卡到一个模型已经下载完以前的进度状态，并且进度不动.*
- `model_ready` is reached through `waiting_for`/`model_ready`, both of which
  read the selected screen. The download therefore started the second file and
  left the first waiting for a model that was by then on disk.

The dead end was total. Selecting the stranded row showed 下载模型, pressing it
hit `fetch_model`'s `all_present` guard, and the answer was
`Err("sensevoice 已经在本机了。")`. Nothing short of removing the row and
dropping the file again could move it.

**Fixed in two places, because there were two ways in:**

- [`AppState::awaiting_model`] — a file waiting for a model occupies the slot
  exactly as a running job does, and `file_chosen` queues behind it. The second
  file becomes 等待中 and follows the first through the ordinary queue, once
  the model arrives.
- `fetch_model`, on a model that is already present, now says so on `UPDATE`
  and starts whatever was waiting, instead of refusing. A refusal there was a
  dead end with no way out; the screen is not wrong, it is stale.

**Verify:** three tests in `state.rs` — a second drop behind a waiting model is
queued rather than shown, the download's progress keeps reaching the file that
is waiting for it, and both files run in order once the model arrives. All
three were run against the guard put back the way it was, and all three failed:

```
the second file took the wait that belongs to the first
the download never reached the file waiting for it: Idle
assertion `left == right` failed: the model started the wrong file
  left: Transcribe("b.wav")   right: Transcribe("a.wav")
```

That first run was not enough — the download test passed against the old code,
because it asked the *screen* rather than the file, and the screen was the
second file's. It is the stranded file's entry that has to be asked about, and
it was rewritten to say so.

**The `fetch_model` half has no test and cannot have one here.** It is a
`#[tauri::command]`, and the only way to run it is with a window. It is three
lines over `all_present`, `emit` and `start_waiting_job`, all of which are
exercised elsewhere; the branch itself is by eye.

**And the wart it exposed, taken as part of the same job:** `unfinished()`
counted a running job and the queue, so a file sitting on `NeedsModel` was not
written to `pending.json` and did not survive a quit. Nothing was lost — it was
never started — but the row is gone from the list on the next launch. That is
§4.

## 4. [x] A row that came back from the last session could not come back

`pending()` and `unfinished()` read `running` and `queue`. A file waiting for a
model is in neither, so it was invisible to both: not in `pending.json`, not in
the count the quit dialog uses, and gone from the list.

Including it was one line. What made it more than one is what the restore does
with it. `restore_pending` put every path back through `enqueue`, so a file that
went away waiting for a model came back as 等待中 — a row claiming a slot that
nothing was ever going to move, in a state it was never in, contradicting that
function's own doc comment ("in exactly the state they were in"). And 继续
would have started it: `take_next` walks *past* whatever is in front, and what
is in front was not a running job but the file holding the slot.

So `restore_pending` takes `model_ready` — answered by the caller, for the
reason everything else about the filesystem is — and puts the first file back on
`NeedsModel` with the rest behind it, which is where a drop would have left
them. `start_next` refuses while a file holds the slot. `resume` says why rather
than doing nothing, because a model is something the person can go and get.

**Verify:** three more tests, and all three were run against the guards put back
the way they were. All three failed:

```
assertion `left == right` failed      ← continuing_a_queue_...: Some("a.wav") vs None
the first file has to be the one offering the download: Queued { input: "a.wav" }
assertion `left == right` failed      ← a_file_waiting_...: 1 vs 2
```

`model_present` was extracted while doing this, because `transcribe` and the
restore now ask the same question and the answer has to agree: a restore that
disagreed with a drop would hand back a file the window refused a moment
earlier.

**One thing left, and it is pre-existing:** the 继续 bar says 上次有 N 个文件没跑完
whenever anything is queued, including a file dropped a moment ago in this
session. Not introduced here and not fixed here.

## 3. [x] The finished line truncated, with no way to read it

**Reported as:** 完成信息需要鼠标悬浮显示完整内容在鼠标提示框.

The bar under a finished transcript is one line in a fixed width:
`完成 · 用 SenseVoice 识别 · 共 42 段 · 已保存到 会议.srt`. It is `truncate`d,
and the path was a file *name* anyway — `view_of` passed `file_label(path)`, so
the folder was never on the screen to be truncated.

**Fixed by sending the whole path and titling the line with it.** The window
shows the base name and puts the full text in `title`, so hovering gives what
the line cannot hold.

**Verify:** the two `bridge` tests that pin the `done` wire format; the title
read by eye.

No frontend test runner exists in this repository (see `p1b-gui.md`), so the
tooltip itself is verified by a person pointing at it.
