# Living in the tray, and what happens to work in flight

The maintainer asked for the window to be able to stay in the system tray, with
the close button extended into "hide" and "quit", and the logic around it
finished properly.

**The tray is the vehicle; the point is something else.** Nothing handled
`CloseRequested`, `RunEvent` or `on_exit`, and `run()` was a bare
`Builder::default()...run(...)`. Closing the window was therefore Tauri's default
and the process ended — so a ten-minute transcription died with the window, with
no partial result and no warning.

### Decisions taken with the maintainer

1. ✕ asks once and remembers the answer.
2. Quitting while busy asks for confirmation, then quits.
3. The interrupted queue is remembered and can be continued in one click.

### Four things checked before anything was designed

- **Quitting mid-transcription does not lose the work already done.** The
  pipeline flushes a resume log per span (`verse-store/src/resume.rs:106-137`)
  and the GUI already used it (`bridge.rs:783-785`, gated in
  `verse-pipeline/src/lib.rs:431-438`). A truncated final line is skipped on
  read, which is the crash case. This is what makes an honest confirmation
  dialog possible at all — without it the dialog would have to say "you lose
  everything", and that would be a different feature.
- **A tray-resident app does not hold a gigabyte.** `DEFAULT_IDLE` is 300 s
  (`keep.rs:36`), so the model goes five minutes after the last job.
- **No capability entries are needed.** `capabilities/default.json` lists no
  custom command, and the tray is built in Rust.
- **The busy signal is `running`, not `is_running()`** — the latter reads `job`,
  which is empty between accepting a file and `JobStarted` (`state.rs:572`
  against `state.rs:369-375`).

## 1. [x] A preferences file

- [x] `crates/verse-app/src/settings.rs`. **Verify:** four tests — an
      unanswered question stays unanswered, an answer survives a restart, a
      corrupt or future-version file asks again, and the queue round-trips.

Also holds `Pending`, the interrupted queue. Two structs in one module rather
than one merged file: preferences change when somebody answers a question and
the queue changes on every job, and a shared file would mean rewriting the
preferences every time a file finished.

## 2. [x] The close button

- [x] `tray::close_requested`, hooked with `on_window_event`.
      **Verify:** by hand — not done, see below.

`prevent_close()` is called first in every branch, so the window cannot vanish
while the question about it is on screen. The dialog uses `.show(callback)` and
not `blocking_show`: the handler is on the main thread, and
`tauri-plugin-dialog`'s own documentation says blocking there "will freeze your
application".

## 3. [x] The tray

- [x] `tray::build`, with 显示主窗口, 隐藏窗口 and 退出, and left-click to
      toggle. Both directions are in the menu as separate always-present
      items: Tauri has no "menu about to open" event, so a single item
      whose label tracked the window's state would be stale under the
      cursor.
      **Verify:** `cargo build -p verse-app`, then run it — **the process was
      still alive after twelve seconds**, which means `setup()` returned `Ok`,
      which means `TrayIconBuilder::build` did not fail. A tray that cannot be
      created fails there and takes the whole application with it.

`tauri`'s `tray-icon` feature was enabled for this; it is not in the default set.

## 4. [x] One exit path

- [x] `tray::leave`, called by the tray menu and by the `Quit` answer.
      **Verify:** `quit_question` is a pure function of the count, with two
      tests — one that the number is in the sentence, one that it says what is
      kept and where to find it again.

It does not cancel and wait. Cancellation is cooperative and would take as long
as the current span, and the resume log is already durable, so waiting would buy
nothing and make the button look broken.

## 5. [x] The interrupted queue

- [x] `AppState::pending`, `unfinished`, `start_next`, `restore_pending`; the
      `resume` command; `pending.json` written from `push_state`; a 继续 bar in
      the window. **Verify:** five state tests, and `svelte-check`.

**The queue carries the engine, which is not decoration.** The resume log's key
is a digest of the settings *and* the input, and the engine is one of the
settings — so a queue restored under a different engine misses every key and
redoes the file. The confirmation dialog promises it will not. This was noticed
while writing the restore, not after.

Written from `push_state` rather than only on the way out, so a crash and a
task-manager kill are covered by the same code as a deliberate quit. That is
affordable because `push_state` has twelve callers, all on commands and job
boundaries — it is not on the progress path.

`restore_pending` on `AppState` does **not** check the filesystem: filtering
paths that have moved is I/O, `state.rs` does none by design, and the caller in
`lib.rs` does it instead — the same split as `Restored`.

## 6. [x] Documents

- [x] `ui-design.md` §10's "not in P1b" list — **tray icon came off it**, with
      the reason, because it was on that list until today. `design.md` §4.9
      gains the second reader of the resume log. Both READMEs say what ✕ does.

## What is not verified

- **The tray, by eye.** The icon at 16 px, the menu, left-click to reveal.
  Whether it is *correct* is unverified; that it *builds* is not.
- **Both native dialogs.** Nothing has been clicked.
- **"继续" actually continuing.** This is the claim the design rests on and the
  one worth watching: start a long file, quit mid-way, relaunch, press 继续, and
  see it pick up rather than start over.
- **What happens on a machine where the data directory cannot be written.** The
  settings and queue saves are best-effort with the error discarded, so the
  window works and the preference is simply forgotten. Nobody has run it that
  way.
