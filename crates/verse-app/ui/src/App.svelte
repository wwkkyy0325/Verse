<!--
  The interface. One page, two columns: the sidebar holds what the engine is
  and every file this session has been given, and the detail region shows
  whichever of them is selected.

  No title bar. It held the application name, the running job's numbers and two
  buttons, all of which have somewhere better to be — the numbers belong beside
  the bar they describe, and the name belongs out of the way. Rust owns the
  state and this holds a copy of it, updated by events rather than replaced
  wholesale — see ui-design.md §6. The window decides nothing.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { open, save } from "@tauri-apps/plugin-dialog";

  import {
    about,
    cancel,
    checkFiles,
    currentState,
    deleteResult,
    demoAvailable,
    demoProgress,
    exportTranscript,
    fetchModel,
    forget,
    importModel,
    models,
    onFileDrop,
    onUpdate,
    probeHardware,
    reset,
    resumeQueue,
    retry,
    reveal,
    select,
    selectModel,
    transcribe,
    type About,
    type Download,
    type DownloadFetching,
    type Refusal,
    type EntryState,
    type ModelChoice,
    type State,
  } from "$lib/api";
  import { Button } from "$lib/components/ui/button";
  import {
    Dialog,
    DialogContent,
    DialogDescription,
    DialogHeader,
    DialogTitle,
  } from "$lib/components/ui/dialog";
  import { Progress } from "$lib/components/ui/progress";

  let view = $state<State>({
    screen: { kind: "empty" },
    entries: [],
    selected: null,
    segments: [],
  });

  let catalog = $state<ModelChoice[]>([]);
  let chosen = $state<string>("");

  // The name and the version, for the corner they live in.
  let appInfo = $state<About | null>(null);

  // The reduced-mode line, when this machine is being worked around.
  let notice = $state<string | null>(null);

  // Set while a file is over the window, so the drop target can say so.
  let dragging = $state(false);

  // What the backend said when it refused something the user asked for. Its own
  // state rather than a failure screen, because the screen has not changed and
  // only the sentence has.
  let message = $state<string | null>(null);

  let aboutOpen = $state(false);

  // Whether this build can run the demonstration. Asked on mount so the window
  // can offer it; running it is a click.
  let demoReady = $state(false);

  // Each model's own download, keyed by id. The panel has one card per model
  // and they can be in different states, so one download field shared between
  // them would show one model's progress on another's row.
  let downloads = $state<Record<string, Download>>({});

  // What was refused on the last drop, and what can be read, for the dialog
  // that says so before anything starts.
  let refusals = $state<Refusal[]>([]);
  let accepted = $state<string[]>([]);
  let refuseOpen = $state(false);

  // What the list is filtered to. Empty shows everything.
  let filter = $state("");

  // The row the ✕ was pressed on, and what can be done with it. `null` when
  // nothing is being asked. Deleting a file cannot be undone, and removing a
  // record is a different thing that happens to live behind the same button —
  // so the question is which was meant, asked once, here.
  let asking = $state<{ index: number; name: string; hasResult: boolean } | null>(null);

  // Set briefly after a copy, so the button can say it worked.
  let copied = $state(false);

  // Shown instead of the brief until it is asked for: it is a page of English
  // API notes, and the dialog is a dialog.
  let briefOpen = $state(false);

  /// Hand the service's API to an agent.
  ///
  /// The clipboard is the point — the text is written for a program, and
  /// selecting a `<pre>` by hand is how it gets to one otherwise. When the
  /// clipboard is refused the text is on screen and selectable, so the failure
  /// costs a drag rather than the feature.
  async function copyBrief() {
    try {
      await navigator.clipboard.writeText(appInfo?.agentBrief ?? "");
      copied = true;
      setTimeout(() => (copied = false), 2000);
    } catch {
      briefOpen = true;
      message = "复制不了，请手动选中下面的文字。";
    }
  }

  let transcriptEl = $state<HTMLElement | null>(null);
  // Follow new text while the job runs, and stop the moment the user scrolls
  // away from the bottom. Fighting someone who is reading is worse than not
  // following at all.
  let following = $state(true);

  // Which file the pane is showing, so a change of selection can drop what
  // belonged to the previous one.
  let shown = $state<number | null>(null);

  /// Whether any file is being recognised right now.
  ///
  /// Read from the list rather than from the screen being shown, and that is
  /// the whole point: a job runs on its own entry, so selecting a *finished*
  /// file while another is mid-run used to make the engine buttons look
  /// available. Pressing one then did nothing but raise an error, because the
  /// backend knows better than the window does.
  const busy = $derived(view.entries.some((entry) => entry.state === "working"));

  /// Rows waiting for the slot. Non-zero on its own means a queue came back
  /// from a previous process, because a queue belonging to this session always
  /// has something running in front of it.
  const waiting = $derived(view.entries.filter((entry) => entry.state === "queued").length);

  /// The rows the filter lets through, with the index they keep in the roster.
  ///
  /// The index travels because everything that acts on a row — selecting it,
  /// forgetting it — addresses it by its position in `view.entries`, not by its
  /// position in the filtered view.
  const matching = $derived.by(() => {
    const needle = filter.trim().toLowerCase();
    const all = view.entries.map((entry, index) => ({ entry, index }));

    if (needle === "") return all;
    return all.filter(({ entry }) => entry.name.toLowerCase().includes(needle));
  });

  /// Which engine produced the file being shown.
  function shownEngine(): string | null {
    if (view.selected === null) return null;
    return view.entries[view.selected]?.engine ?? null;
  }

  onMount(() => {
    let stopUpdates: (() => void) | null = null;
    let stopDrop: (() => void) | null = null;

    void (async () => {

      // Subscriptions first, and each step independent of the others. They used
      // to be one chain of awaits with nothing catching: a catalogue that failed
      // to load would have taken the subscriptions down with it and left a
      // window that renders and never updates.
      try {
        stopUpdates = await onUpdate((update) => {
          switch (update.kind) {
            case "snapshot":
              view = update.state;
              break;
            case "screen":
              view.screen = update.screen;
              break;
            case "roster":
              view.entries = update.entries;
              view.selected = update.selected;
              break;
            case "selected":
              // The index arrives in the roster update that follows; this only
              // has to replace what the pane is showing.
              view.screen = update.screen;
              view.segments = update.segments;
              break;
            case "segment":
              view.segments.push({
                startMs: update.startMs,
                endMs: update.endMs,
                text: update.text,
              });
              break;
            case "progress":
              if (view.screen.kind === "working") {
                view.screen = {
                  ...view.screen,
                  elapsedMs: update.elapsedMs,
                  fraction: update.fraction,
                };
              }
              break;
            case "download":
              downloads[update.model] = update.download;
              // And the screen, when the file waiting on it is the one being
              // shown — the same event feeds both readings.
              if (view.screen.kind === "needsModel" && view.screen.modelId === update.model) {
                view.screen = { ...view.screen, download: update.download };
              }
              // The third reading, and the one that was missing: whether the
              // model can now be chosen. `present` is the backend's answer and
              // the panel only ever asked once, at startup — so a download that
              // finished while the window was open left its card saying
              // 需要下载 and refusing to be picked until the next launch.
              if (update.download.state === "ready") void refreshCatalog();
              break;
            case "cleared":
              view.segments = [];
              following = true;
              message = null;
              break;
          }

          // After every update, not only after a snapshot. A message belongs
          // to the file it came from, and leaving it on screen after switching
          // files says something about the wrong one.
          noteShown();
        });

        stopDrop = await onFileDrop((paths) => {
          dragging = false;
          void start(paths);
        });
      } catch (cause) {
        message = `连不上后台：${cause}`;
        return;
      }

      try {
        view = await currentState();
        shown = view.selected;
      } catch (cause) {
        message = `读不到当前状态：${cause}`;
      }

      try {
        catalog = await models();
        chosen = catalog.find((model) => model.default)?.id ?? catalog[0]?.id ?? "";
      } catch (cause) {
        message = `读不到模型清单：${cause}`;
      }

      try {
        appInfo = await about();
      } catch {
        // The corner shows a name and a version. Losing them costs nothing.
      }

      try {
        demoReady = (await demoAvailable()) && import.meta.env.DEV;
      } catch {
        // A build that cannot say has no demonstration. Saying nothing is the
        // right answer to a question about a development tool.
      }

      try {
        notice = (await probeHardware()).degraded;
      } catch {
        // Likewise: the notice explains slowness, it is not a function.
      }

      // Asked with nothing to check, purely to learn what this build reads.
      // The picker's filter has to name extensions before a file is chosen,
      // and a second hardcoded list there would be a second thing to keep in
      // step with the one the backend refuses by.
      try {
        accepted = (await checkFiles([])).accepted;
      } catch {
        // An empty filter is a picker that shows everything, which is worse
        // than a filtered one and better than a picker that will not open.
      }
    })();

    // Tauri's drag-drop event reports the file arriving; these report the
    // pointer crossing the window, which that event does not.
    const onOver = (event: DragEvent) => {
      event.preventDefault();
      dragging = true;
    };
    const onLeave = () => {
      dragging = false;
    };

    window.addEventListener("dragover", onOver);
    window.addEventListener("dragleave", onLeave);

    return () => {
      stopUpdates?.();
      stopDrop?.();
      window.removeEventListener("dragover", onOver);
      window.removeEventListener("dragleave", onLeave);
    };
  });

  /// Everything that belongs to the file being shown, dropped when it changes.
  function noteShown() {
    if (view.selected === shown) return;
    shown = view.selected;
    following = true;
    message = null;
  }

  // Keep the newest text in view while following.
  $effect(() => {
    view.segments.length;
    if (following && transcriptEl) {
      transcriptEl.scrollTop = transcriptEl.scrollHeight;
    }
  });

  function onScroll() {
    if (!transcriptEl) return;
    const distance =
      transcriptEl.scrollHeight - transcriptEl.scrollTop - transcriptEl.clientHeight;
    following = distance < 40;
  }

  /// Put the window through a job it did not have to wait for.
  async function runDemo() {
    message = null;
    try {
      await demoProgress();
    } catch (cause) {
      message = `演示没有启动：${cause}`;
    }
  }

  /// Hand one or more files over, after checking them.
  ///
  /// The check comes first so a folder or a spreadsheet is refused before a
  /// progress bar appears and then fails — which is what used to happen, and
  /// reads as the program being broken rather than as the file being wrong.
  ///
  /// All of the usable ones at once: the backend starts the first and queues
  /// the rest, so the window never decides which job runs — starting two would
  /// cancel the first. A file already in the list is shown rather than
  /// recognised again.
  async function start(paths: string[]) {
    message = null;
    try {
      const check = await checkFiles(paths);

      if (check.refused.length > 0) {
        refusals = check.refused;
        accepted = check.accepted;
        refuseOpen = true;
      }

      for (const path of check.usable) {
        await transcribe(path);
      }
    } catch (cause) {
      message = String(cause);
    }
  }

  async function pick() {
    const picked = await open({
      multiple: true,
      directory: false,
      filters: [{ name: "音频与视频", extensions: accepted }],
    });
    if (!picked) return;
    await start(Array.isArray(picked) ? picked : [picked]);
  }

  async function writeTranscript() {
    message = null;

    const suggested =
      view.screen.kind === "done"
        ? (view.screen.exported ?? view.screen.file.replace(/\.[^.]*$/, "") + ".srt")
        : "字幕.srt";

    const picked = await save({
      defaultPath: suggested,
      filters: [
        { name: "字幕（SubRip，播放器可加载）", extensions: ["srt"] },
        { name: "纯文本", extensions: ["txt"] },
      ],
    });
    if (!picked) return;

    try {
      await exportTranscript(picked);
    } catch (cause) {
      message = String(cause);
    }
  }

  async function chooseModel(id: string) {
    message = null;
    try {
      await selectModel(id);
      chosen = id;
    } catch (cause) {
      message = String(cause);
    }
  }

  /// Take a file out of the list, leaving its transcript on disk.
  async function forgetRow(index: number) {
    asking = null;
    message = null;
    try {
      await forget(index);
    } catch (cause) {
      message = String(cause);
    }
  }

  /// Delete one row's transcript from the disk as well as the list.
  async function deleteRow(index: number) {
    asking = null;
    message = null;
    try {
      await deleteResult(index);
    } catch (cause) {
      message = String(cause);
    }
  }

  async function showInFolder() {
    message = null;
    try {
      await reveal();
    } catch (cause) {
      message = String(cause);
    }
  }

  async function showFile(index: number) {
    message = null;
    try {
      await select(index);
    } catch (cause) {
      message = String(cause);
    }
  }

  async function runAgain() {
    message = null;
    try {
      await retry();
    } catch (cause) {
      message = String(cause);
    }
  }

  /// Fetch one model, by id — from its card, or from the file waiting on it.
  async function startDownload(model: string) {
    message = null;
    try {
      await fetchModel(model);
    } catch (cause) {
      message = String(cause);
    }
  }


  async function pickModelFolder() {
    message = null;
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked !== "string") return;

    try {
      await importModel(picked);
      // A model arriving by hand is the same event as one arriving over the
      // network, as far as the panel is concerned.
      await refreshCatalog();
    } catch (cause) {
      message = String(cause);
    }
  }

  /// Ask the backend what is on disk now.
  ///
  /// `present` is computed there from the files themselves, and the panel used
  /// to take that reading once, at startup. Anything that puts a model in place
  /// during the session — a finished download, a folder chosen by hand — has to
  /// ask again, or its card keeps saying 需要下载 and stays unselectable until
  /// the window is reopened.
  async function refreshCatalog() {
    try {
      const fresh = await models();
      catalog = fresh;
      // The choice is an id, so replacing the list does not disturb it. Only an
      // empty one needs filling, which happens when the first reading found no
      // catalogue at all.
      if (chosen === "") {
        chosen = fresh.find((model) => model.default)?.id ?? fresh[0]?.id ?? "";
      }
    } catch (cause) {
      message = `读不到模型清单：${cause}`;
    }
  }

  /// A download that is in flight, from any reading of one.
  ///
  /// TypeScript will not narrow an index expression across a template branch,
  /// so the narrowing happens here once and the branches use the result.
  function fetching(download: Download | undefined): DownloadFetching | null {
    return download?.state === "fetching" ? download : null;
  }

  /// This model's download, when it is in flight.
  function downloading(id: string): DownloadFetching | null {
    return fetching(downloads[id]);
  }

  /// Why this model's download failed, when it did.
  function failure(id: string): string | null {
    const entry = downloads[id];
    return entry?.state === "failed" ? entry.reason : null;
  }

  /// The numbers to show for a download: the whole model when they are known,
  /// the file in flight otherwise.
  ///
  /// Per-file figures are what the downloader has. A person who chose a 987 MB
  /// model is watching the model, and a bar that fills once per file fills five
  /// times and looks finished on the first.
  function weights(active: DownloadFetching): { received: number; total: number | null } {
    if (active.modelTotalBytes !== null) {
      return { received: active.modelReceivedBytes ?? 0, total: active.modelTotalBytes };
    }
    return { received: active.receivedBytes, total: active.totalBytes };
  }

  /// How far along a download is, as a whole percent, when that is knowable.
  function percent(download: DownloadFetching | null): number | null {
    if (!download) return null;
    const { received, total } = weights(download);
    if (!total) return null;
    return Math.round((received / total) * 100);
  }

  function megabytes(bytes: number): string {
    return (bytes / 1_000_000).toFixed(0);
  }

  function timecode(ms: number): string {
    const total = Math.floor(ms / 1000);
    const minutes = String(Math.floor(total / 60)).padStart(2, "0");
    const seconds = String(total % 60).padStart(2, "0");
    return `${minutes}:${seconds}`;
  }

  const STATE_LABEL: Record<EntryState, string> = {
    queued: "等待中",
    working: "识别中",
    done: "已完成",
    failed: "失败",
    needsModel: "缺模型",
  };

  function modelName(id: string | null): string {
    if (!id) return "";
    return catalog.find((model) => model.id === id)?.name ?? id;
  }
</script>

<div class="relative flex h-full flex-col">
  <!-- Something is over the window and a file would be accepted. The empty
       state is a drop target itself and glows on its own; this is for every
       other state, where dropping used to work with nothing on screen saying
       so. `pointer-events-none` so the overlay cannot swallow the drop it is
       advertising. -->
  {#if dragging && view.screen.kind !== "empty"}
    <div class="pointer-events-none absolute inset-0 z-50 flex items-center justify-center">
      <div
        class="bg-background/90 border-primary ring-primary/25 rounded-xl border-2 px-6 py-4 shadow-2xl ring-8"
      >
        <p class="text-sm font-medium">松开即加入处理</p>
        <p class="text-muted-foreground mt-1 text-xs">会排到正在处理的文件后面</p>
      </div>
    </div>
  {/if}

  <!-- Hand-written, like the rest of this file. The vendored `Alert` that
       ui-design.md §5 used to name for this has been removed. -->
  {#if notice}
    <div class="bg-muted/40 flex shrink-0 items-center gap-3 border-b px-5 py-2">
      <span class="text-muted-foreground text-xs">{notice}</span>
      <span class="flex-1"></span>
      <Button variant="secondary" size="sm" onclick={() => (notice = null)}>知道了</Button>
    </div>
  {/if}

  <div class="flex min-h-0 flex-1">
    <!-- What the engine is, and what this session has been given. Both are
         always on screen, which is the point — the model used to be invisible,
         and a second file used to erase the first. -->
    <aside class="flex w-72 shrink-0 flex-col border-r">
      <div class="min-h-0 flex-1 overflow-y-auto">
        <!-- `group` so the reason for the grey can hang off the whole section
             rather than sitting on the page: a line of explanation that is
             always there is a line people stop reading, and this one only
             matters at the moment somebody tries to press something. -->
        <section class="group relative px-4 py-3">
          <h2 class="text-muted-foreground mb-2 text-xs font-medium">识别模型</h2>

          {#if busy}
            <!-- Shown on hover over the section, including over the disabled
                 buttons — a disabled button still hovers its parent, which is
                 the whole reason this is anchored here and not on them. -->
            <div
              class="bg-popover text-popover-foreground pointer-events-none absolute inset-x-4 top-7 z-10 hidden rounded-md border px-2 py-1.5 text-xs leading-relaxed shadow-md group-hover:block"
            >
              正在识别，暂时不能换模型。等这一批处理完再换。
            </div>
          {/if}

          <div class="flex flex-col gap-1.5">
            {#each catalog as model (model.id)}
              {@const active = downloading(model.id)}
              <!-- Unselectable when it cannot run: something is being
                   recognised, this model is still arriving, or it is not here
                   at all. The card says which of the three, and the 下载 button
                   is on the same row. -->
              {@const blocked = busy || active !== null || !model.present}
              <!-- A card, not a button. Fetching a model belongs on the row
                   that says it is missing, and a button cannot hold a button. -->
              <div
                class="rounded-md border transition-colors
                       {blocked && !busy
                  ? 'border-border opacity-80'
                  : busy
                    ? 'border-border opacity-50'
                    : chosen === model.id
                      ? 'border-primary bg-primary/5'
                      : 'border-border'}"
              >
                <button
                  class="block w-full px-3 pt-2 text-left
                         {blocked ? 'cursor-not-allowed' : ''}"
                  onclick={() => void chooseModel(model.id)}
                  disabled={blocked}
                >
                  <span class="flex items-baseline gap-2">
                    <span class="text-[13px] font-medium">{model.name}</span>
                    <span class="flex-1"></span>
                    <span class="text-muted-foreground text-xs tabular-nums">
                      {megabytes(model.bytes)} MB
                    </span>
                  </span>
                  {#if model.description}
                    <span class="text-muted-foreground mt-1 block text-xs leading-relaxed">
                      {model.description}
                    </span>
                  {/if}
                </button>

                <!-- What this model's own state is. Fetching it here means a
                     model can be got ready before there is a file that needs
                     it, which is what the panel is for. -->
                <div class="px-3 pt-1.5 pb-2">
                  {#if active}
                    <Progress value={percent(active) ?? 0} />
                    <p class="text-muted-foreground mt-1 text-[11px] tabular-nums">
                      {megabytes(weights(active).received)} MB
                      {#if weights(active).total}
                        / {megabytes(weights(active).total!)} MB · {percent(active)}%
                      {/if}
                    </p>
                  {:else if downloads[model.id]?.state === "verifying"}
                    <p class="text-muted-foreground text-[11px]">正在校验…</p>
                  {:else if failure(model.id)}
                    <p class="text-destructive text-[11px] leading-relaxed">
                      {failure(model.id)}
                    </p>
                    <Button
                      variant="secondary"
                      size="xs"
                      class="mt-1"
                      onclick={() => void startDownload(model.id)}
                    >
                      再试一次
                    </Button>
                  {:else if model.present}
                    <p class="text-muted-foreground/70 text-[11px]">已安装</p>
                  {:else}
                    <div class="flex items-center gap-2">
                      <span class="text-muted-foreground/70 text-[11px]">需要下载</span>
                      <span class="flex-1"></span>
                      <Button
                        variant="secondary"
                        size="xs"
                        onclick={() => void startDownload(model.id)}
                      >
                        下载
                      </Button>
                    </div>
                  {/if}
                </div>
              </div>
            {/each}
          </div>
        </section>

        <section class="border-t px-4 py-3">
          <h2 class="text-muted-foreground mb-2 text-xs font-medium">本次处理的文件</h2>
          {#if view.entries.length === 0}
            <p class="text-muted-foreground/70 text-xs">还没有文件。</p>
          {:else}
            {#if view.entries.length > 6}
              <!-- Only worth a box once the list is long enough to scroll. -->
              <input
                class="border-input focus:border-ring mb-1.5 w-full rounded-md border bg-transparent px-2 py-1 text-xs outline-none"
                placeholder="按文件名筛选"
                bind:value={filter}
              />
            {/if}
            {#if matching.length === 0}
              <p class="text-muted-foreground/70 text-xs">没有匹配的文件。</p>
            {:else}
              <ul class="flex flex-col gap-0.5">
                {#each matching as { entry, index } (index)}
                  <li
                    class="hover:bg-muted/60 group/row flex items-center gap-1 rounded
                           {view.selected === index ? 'bg-muted' : ''}"
                  >
                    <button
                      class="flex min-w-0 flex-1 items-center gap-2 px-2 py-1.5 text-left"
                      onclick={() => void showFile(index)}
                    >
                      <span class="min-w-0 flex-1 truncate text-[13px]">{entry.name}</span>
                      <!-- A fixed width, right-aligned. The label is two
                           characters for 失败 and three for everything else, so
                           without this the column resized as a row moved
                           between states and the file names beside it were
                           re-truncated at a different character each time. -->
                      <span class="text-muted-foreground w-10 shrink-0 text-right text-xs">
                        {STATE_LABEL[entry.state]}
                      </span>
                    </button>
                    <!-- Hidden until the row is pointed at, but *invisible*
                         rather than `hidden`: `display: none` takes no space,
                         so the ✕ appearing on hover pushed the file name
                         narrower and moved the column beside it. Reserved
                         space is the price of a list that does not twitch. -->
                    <button
                      class="text-muted-foreground/50 hover:bg-destructive/10 hover:text-destructive invisible flex h-6 w-6 shrink-0 items-center justify-center rounded text-xs group-hover/row:visible"
                      title="移除这一行"
                      onclick={() => (asking = { index, name: entry.name, hasResult: entry.hasResult })}
                    >
                      ✕
                    </button>
                  </li>
                {/each}
              </ul>
            {/if}
          {/if}
        </section>
      </div>

      <!-- Pinned, not scrolling with the list it adds to. A button that moves
           as the list grows is a button people have to go looking for. -->
      <div class="shrink-0 space-y-2 border-t px-4 py-3">
        <!-- A queue left over from a previous session. It is not started by
             itself: that is the whole reason it waits. -->
        {#if waiting > 0 && !busy}
          <div class="bg-muted/40 rounded-md border px-3 py-2">
            <p class="text-muted-foreground text-xs">上次有 {waiting} 个文件没跑完</p>
            <Button class="mt-2 w-full" size="sm" onclick={() => void resumeQueue()}>
              继续
            </Button>
          </div>
        {/if}
        <Button class="w-full" onclick={() => void pick()}>添加文件</Button>
      </div>

      <!-- Bottom-left, where it costs no row of its own. -->
      <div class="text-muted-foreground flex shrink-0 items-center gap-2 border-t px-4 py-2 text-xs">
        <span class="truncate">
          {appInfo?.name ?? "Verse"}{appInfo?.version ? ` ${appInfo.version}` : ""}
        </span>
        <span class="flex-1"></span>
        {#if demoReady}
          <!-- Offered, never run on its own. It used to fire whenever the
               environment said so, which meant a development shell got a
               demonstration job on every launch and the program looked like it
               was doing something by itself. -->
          <button
            class="hover:text-foreground underline-offset-2 hover:underline"
            onclick={() => void runDemo()}
          >
            演示
          </button>
        {/if}
        <button
          class="hover:text-foreground underline-offset-2 hover:underline"
          onclick={() => (aboutOpen = true)}
        >
          关于
        </button>
      </div>
    </aside>

    <!-- The detail region: whichever file is being shown. -->
    <main class="flex min-w-0 flex-1 flex-col">
      {#if message && view.screen.kind !== "done" && view.screen.kind !== "failed" && view.screen.kind !== "needsModel"}
        <p class="text-destructive shrink-0 px-6 pt-3 text-xs leading-relaxed">{message}</p>
      {/if}

      {#if view.screen.kind === "empty"}
        <button
          class="m-6 flex flex-1 cursor-pointer flex-col items-center justify-center gap-3 rounded-lg border-2 border-dashed transition-colors
                 {dragging
            ? 'border-primary bg-primary/5'
            : 'border-border hover:border-muted-foreground/40'}"
          onclick={() => void pick()}
        >
          <p class="text-base">{dragging ? "松开即可开始" : "把音频文件拖到这里"}</p>
          <p class="text-muted-foreground text-sm">或者点击选择文件，可以一次选多个</p>
          <p class="text-muted-foreground/70 mt-2 text-xs">
            支持 mp3、wav、m4a、mp4 等常见格式
          </p>
        </button>
      {:else if view.screen.kind === "queued"}
        <div class="flex flex-1 flex-col items-center justify-center gap-3 px-8 text-center">
          <p class="text-sm">{view.screen.file}</p>
          <p class="text-muted-foreground text-sm">正在等前面的文件处理完。</p>
        </div>
      {:else if view.screen.kind === "working" || view.screen.kind === "done"}
        <div
          bind:this={transcriptEl}
          onscroll={onScroll}
          class="min-h-0 flex-1 overflow-y-auto px-6 py-5 select-text"
        >
          {#if view.segments.length === 0}
            <p class="text-muted-foreground text-sm">
              {view.screen.kind === "working" ? "正在准备…" : "没有识别到内容。"}
            </p>
          {:else}
            <div class="flex flex-col gap-2.5">
              {#each view.segments as segment, index (index)}
                <p class="flex gap-4 text-[15px] leading-relaxed">
                  <span class="text-muted-foreground/70 w-12 shrink-0 pt-0.5 text-right font-mono text-xs tabular-nums">
                    {timecode(segment.startMs)}
                  </span>
                  <span>{segment.text}</span>
                </p>
              {/each}
            </div>
          {/if}
        </div>

        <!-- One bar, two states. Working and finished never happen at the same
             time, so they are the same row in the same place rather than one at
             the top and one at the bottom — which is what made 取消 and the
             action buttons disagree about where the edge of the pane is. -->
        <div class="flex shrink-0 items-center gap-3 border-t px-6 py-3">
          {#if view.screen.kind === "working"}
            <div class="min-w-0 flex-1">
              {#if view.screen.fraction === null}
                <!-- No length was found, so there is no number to show. A bar
                     creeping along a percentage it does not have would be a
                     fabrication; this says only that work is happening. -->
                <div class="bg-muted h-1.5 w-full animate-pulse overflow-hidden rounded-full"></div>
              {:else}
                <Progress value={view.screen.fraction * 100} />
              {/if}
              <p class="text-muted-foreground mt-1.5 truncate text-xs tabular-nums">
                {view.screen.file} · 已识别 {view.segments.length} 段 ·
                {timecode(view.screen.elapsedMs)}{#if view.screen.fraction !== null}
                  · {Math.round(view.screen.fraction * 100)}%{/if}
              </p>
            </div>
            <Button
              variant="destructive"
              class="bg-destructive hover:bg-destructive/90 text-white"
              onclick={() => void cancel()}
              disabled={view.screen.stopping}
            >
              {view.screen.stopping ? "正在停止…" : "取消"}
            </Button>
          {:else}
            <span class="text-muted-foreground min-w-0 truncate text-xs">
              完成 · 用 {modelName(shownEngine())} 识别 · 共 {view.segments.length} 段{#if view.screen.exported}
                · 已保存到 {view.screen.exported}{/if}
            </span>
            {#if view.screen.saveError}
              <!-- Not a failure of the job: the transcript is here and 另存为
                   still works. It says the result is not in the output folder,
                   which is the one thing the window cannot work out itself. -->
              <span class="text-destructive text-xs">未能自动保存：{view.screen.saveError}</span>
            {/if}
            {#if message}
              <span class="text-destructive text-xs">{message}</span>
            {/if}
            <span class="flex-1"></span>
            <Button
              variant="secondary"
              size="sm"
              onclick={() => void showInFolder()}
              disabled={!view.screen.exported}
            >
              在文件夹中
            </Button>
            <Button variant="secondary" size="sm" onclick={() => void writeTranscript()}>
              另存为…
            </Button>
            <Button variant="secondary" size="sm" onclick={() => void reset()}>再来一个</Button>
          {/if}
        </div>
      {:else if view.screen.kind === "failed"}
        <div class="flex flex-1 flex-col items-center justify-center gap-4 px-8 text-center">
          <p class="text-destructive text-sm font-medium">转写失败</p>
          <p class="text-muted-foreground max-w-md text-sm leading-relaxed">{view.screen.reason}</p>
          <p class="text-muted-foreground/70 text-xs">{view.screen.file}</p>
          {#if message}
            <p class="text-destructive max-w-md text-xs leading-relaxed">{message}</p>
          {/if}
          <!-- One action per `Recovery`, because the backend already decided
               what this failure allows and a single "try again" for all three
               would tell the user to do something that cannot work. -->
          <div class="mt-2 flex gap-2">
            {#if view.screen.recovery === "retry" || view.screen.recovery === "getModel"}
              <Button onclick={() => void runAgain()}>
                {view.screen.recovery === "getModel" ? "检查模型后重试" : "重试"}
              </Button>
              <Button variant="secondary" onclick={() => void reset()}>换一个文件</Button>
            {:else}
              <Button onclick={() => void reset()}>换一个文件</Button>
            {/if}
          </div>
        </div>
      {:else if view.screen.kind === "needsModel"}
        {@const waitingModel = view.screen.modelId}
        <!-- The panel's reading of this download when there is one: it knows
             the whole model's size, and the screen's copy of the state only
             knows the file in flight. -->
        {@const download = downloads[waitingModel] ?? view.screen.download}
        {@const active = fetching(download)}
        <div class="flex flex-1 flex-col items-center justify-center gap-4 px-8 text-center">
          <p class="text-sm font-medium">还缺少识别模型</p>
          <p class="text-muted-foreground max-w-md text-sm leading-relaxed">
            需要先准备 <span class="font-medium">{modelName(view.screen.modelId)}</span>，之后才能转写。
            下载好之后会自动开始，不用再点一次。
          </p>
          <p class="text-muted-foreground/70 text-xs">{view.screen.file}</p>

          {#if active}
            <div class="w-72 space-y-2">
              <Progress value={percent(active) ?? 0} />
              <p class="text-muted-foreground text-xs tabular-nums">
                {megabytes(weights(active).received)} MB
                {#if weights(active).total}
                  / {megabytes(weights(active).total!)} MB · {percent(active)}%
                {/if}
              </p>
            </div>
          {:else if download.state === "verifying"}
            <p class="text-muted-foreground text-xs">正在校验…</p>
          {:else if download.state === "failed"}
            <p class="text-destructive max-w-md text-xs leading-relaxed">
              {download.reason}
            </p>
            <Button onclick={() => void startDownload(waitingModel)}>再试一次</Button>
          {:else}
            <div class="flex gap-2">
              <Button onclick={() => void startDownload(waitingModel)}>下载模型</Button>
              <Button variant="secondary" onclick={() => void pickModelFolder()}>
                我已有模型文件夹
              </Button>
            </div>
          {/if}

          {#if message}
            <p class="text-destructive max-w-md text-xs leading-relaxed">{message}</p>
          {/if}

          <Button variant="secondary" size="sm" onclick={() => void reset()}>返回</Button>
        </div>
      {/if}
    </main>
  </div>

  <!-- What most browser download lists do, and for the same reason: one ✕
       means two things, so it asks which. Removing a row is reversible — the
       transcript stays where it is — and deleting the file is not, so they are
       never the same button.

       Laid out like the other two dialogs: the two sentences in rows, the
       actions in a row at the bottom right. -->
  <Dialog open={asking !== null} onOpenChange={(open) => !open && (asking = null)}>
    <DialogContent class="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>这一行怎么处理？</DialogTitle>
        <DialogDescription>{asking?.name ?? ""} 的转写已经写好了。</DialogDescription>
      </DialogHeader>

      <dl class="space-y-1.5 text-xs leading-relaxed">
        <div class="flex gap-3">
          <dt class="text-muted-foreground w-20 shrink-0">移出列表</dt>
          <dd>只从这里去掉，字幕文件留在原处。</dd>
        </div>
        <div class="flex gap-3">
          <dt class="text-muted-foreground w-20 shrink-0">删除文件</dt>
          <!-- Said in the row rather than left to a greyed-out button, which
               is a thing people press twice. -->
          <dd>
            {asking?.hasResult
              ? "连字幕文件一起删掉，无法恢复。"
              : "这一行还没有字幕文件，没有东西可以删。"}
          </dd>
        </div>
      </dl>

      <p class="text-muted-foreground text-xs leading-relaxed">
        录音文件本身不受影响，两种做法都不会动它。
      </p>

      <div class="flex justify-end gap-2">
        <Button variant="secondary" size="sm" onclick={() => (asking = null)}>取消</Button>
        <Button variant="secondary" size="sm" onclick={() => void forgetRow(asking!.index)}>
          移出列表
        </Button>
        <Button
          variant="destructive"
          class="bg-destructive hover:bg-destructive/90 text-white"
          size="sm"
          disabled={!asking?.hasResult}
          onclick={() => void deleteRow(asking!.index)}
        >
          删除文件
        </Button>
      </div>
    </DialogContent>
  </Dialog>

  <!-- Said before the work rather than after it. A drop that reaches the
       decoder and fails reads as the program being broken; this says which
       files were not accepted, and why, at the moment they arrive. -->
  <Dialog bind:open={refuseOpen}>
    <DialogContent class="sm:max-w-lg">
      <DialogHeader>
        <DialogTitle>这些文件处理不了</DialogTitle>
        <DialogDescription>
          {refusals.length === 1 ? "这个文件已跳过。" : `这 ${refusals.length} 个文件已跳过。`}
        </DialogDescription>
      </DialogHeader>

      <ul class="flex flex-col gap-1.5">
        {#each refusals as refusal, index (index)}
          <li class="text-xs leading-relaxed">
            <span class="font-medium">{refusal.file}</span>
            <span class="text-muted-foreground"> — {refusal.reason}</span>
          </li>
        {/each}
      </ul>

      {#if accepted.length > 0}
        <p class="text-muted-foreground text-xs leading-relaxed">
          可以处理：{accepted.join("、")}
        </p>
      {/if}

      <div class="flex justify-end">
        <Button variant="secondary" size="sm" onclick={() => (refuseOpen = false)}>
          知道了
        </Button>
      </div>
    </DialogContent>
  </Dialog>

  <Dialog bind:open={aboutOpen}>
    <DialogContent class="max-h-[85vh] overflow-y-auto sm:max-w-2xl">
      <DialogHeader>
        <DialogTitle>
          {appInfo?.name ?? "Verse"} {appInfo?.version ?? ""}
        </DialogTitle>
        <DialogDescription>{appInfo?.summary ?? ""}</DialogDescription>
      </DialogHeader>

      <section>
        <h3 class="text-sm font-medium">作为服务调用</h3>
        <p class="text-muted-foreground mt-1 text-xs leading-relaxed">
          运行 <code class="bg-muted rounded px-1">verse serve</code> 之后，Verse 会在本机
          127.0.0.1 上开一个服务，别的程序（包括 agent）可以直接驱动它，把它当成工作流里的一个节点。
          下面这段是写给 agent 看的完整说明，复制过去即可。
        </p>

        <dl class="mt-2 space-y-1">
          {#each appInfo?.routes ?? [] as route}
            <div class="flex gap-2 text-xs">
              <dt class="w-14 shrink-0 font-mono text-[11px]">{route.method}</dt>
              <dt class="w-26 shrink-0 font-mono text-[11px]">{route.path}</dt>
              <dd class="text-muted-foreground">{route.what}</dd>
            </div>
          {/each}
        </dl>

        <div class="mt-2 flex items-center gap-2">
          <Button size="sm" onclick={() => void copyBrief()}>
            {copied ? "已复制" : "复制给 agent"}
          </Button>
          <button
            class="text-muted-foreground hover:text-foreground text-xs underline-offset-2 hover:underline"
            onclick={() => (briefOpen = !briefOpen)}
          >
            {briefOpen ? "收起完整说明" : "查看完整说明"}
          </button>
        </div>

        {#if briefOpen}
          <!-- Selectable as well as copyable: the clipboard can be refused,
               and a document somebody cannot get at is worse than one they
               have to select by hand. -->
          <pre
            class="bg-muted/50 mt-2 max-h-64 overflow-auto rounded-md p-3 font-mono text-[11px] leading-relaxed whitespace-pre select-text">{appInfo?.agentBrief ?? ""}</pre>
        {/if}

        {#if message}
          <p class="text-destructive mt-2 text-xs">{message}</p>
        {/if}
      </section>

      <section class="border-t pt-3">
        <h3 class="text-sm font-medium">开源许可</h3>
        <dl class="mt-2 space-y-2">
          {#each appInfo?.attributions ?? [] as row}
            <div class="flex gap-3 text-xs">
              <dt class="text-muted-foreground w-24 shrink-0">{row.what}</dt>
              <dd>{row.who}</dd>
            </div>
          {/each}
        </dl>

        <p class="text-muted-foreground mt-2 text-xs leading-relaxed">
          {appInfo?.licenceNote ?? ""}
        </p>
      </section>
    </DialogContent>
  </Dialog>
</div>
