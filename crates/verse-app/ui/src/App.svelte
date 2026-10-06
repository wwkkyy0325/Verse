<!--
  The interface. One job: take a file, hand it over, show what comes back.

  Rust owns the state and this holds a copy of it, updated by events rather
  than replaced wholesale — see ui-design.md §6. The window decides nothing.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { open, save } from "@tauri-apps/plugin-dialog";

  import {
    about,
    cancel,
    currentScreen,
    exportTranscript,
    fetchModel,
    importModel,
    onFileDrop,
    onUpdate,
    reset,
    transcribe,
    type About,
    type Download,
    type Screen,
    type Segment,
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

  let screen = $state<Screen>({ kind: "empty" });
  let segments = $state<Segment[]>([]);
  let elapsedMs = $state(0);

  // Set while a file is over the window, so the drop target can say so.
  let dragging = $state(false);

  // What the backend said when it refused something the user asked for — a
  // folder that is not a model, or an export it could not write. Its own state
  // rather than a failure screen, because the screen has not changed and only
  // the sentence has. Shown on whichever screen raised it; an error that sets
  // a variable nothing renders is a failure that looks like success.
  let modelError = $state<string | null>(null);

  let aboutOpen = $state(false);
  let aboutContent = $state<About | null>(null);

  // The path of the file in hand. The backend reports file *names* — a full
  // path is usually too long to show and never what the user needs to read —
  // so the window keeps the path it was handed, which is what a retry needs.
  let lastPath: string | null = $state(null);

  let transcriptEl = $state<HTMLElement | null>(null);
  // Follow new text while the job runs, and stop the moment the user scrolls
  // away from the bottom. Fighting someone who is reading is worse than not
  // following at all.
  let following = $state(true);

  onMount(() => {
    // Subscribing is asynchronous and `onMount` is not, so the teardown has to
    // cope with being called before the subscriptions exist. Both are
    // optional-chained for that reason.
    let stopUpdates: (() => void) | null = null;
    let stopDrop: (() => void) | null = null;

    void (async () => {
      screen = await currentScreen();

      stopUpdates = await onUpdate((update) => {
        switch (update.kind) {
          case "screen":
            screen = update.screen;
            break;
          case "segment":
            segments.push({
              startMs: update.startMs,
              endMs: update.endMs,
              text: update.text,
            });
            break;
          case "progress":
            elapsedMs = update.elapsedMs;
            break;
          case "download":
            // The screen already carries the download, but it arrives rarely;
            // this is the one that moves often enough to be worth its own
            // message.
            if (screen.kind === "needsModel") {
              screen = { ...screen, download: update.download };
            }
            break;
          case "cleared":
            segments = [];
            elapsedMs = 0;
            following = true;
            modelError = null;
            break;
        }
      });

      stopDrop = await onFileDrop((paths) => {
        dragging = false;
        void start(paths[0]);
      });
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

  // Keep the newest text in view while following.
  $effect(() => {
    segments.length;
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

  async function start(path: string) {
    lastPath = path;
    segments = [];
    elapsedMs = 0;
    following = true;
    try {
      await transcribe(path);
    } catch (cause) {
      // The backend reports failures on the bus; reaching here means the call
      // itself did not go through, which is a different problem.
      screen = {
        kind: "failed",
        file: path,
        reason: String(cause),
        recovery: "pickAnotherFile",
      };
    }
  }

  /// Retry the file that failed.
  ///
  /// In the script rather than inline because `lastPath` narrows to `string`
  /// here and does not in the template, where `$state` compiles to a getter.
  function retry() {
    if (lastPath) void start(lastPath);
  }

  async function pick() {
    const chosen = await open({
      multiple: false,
      directory: false,
      filters: [
        { name: "音频与视频", extensions: ["mp3", "wav", "m4a", "flac", "aac", "ogg", "opus", "wma", "mp4", "mkv", "mov", "avi", "webm"] },
      ],
    });
    if (typeof chosen === "string") await start(chosen);
  }

  async function writeTranscript() {
    modelError = null;

    // The name it already has, when it has one, so the copy lands beside its
    // own record rather than under a second name for the same thing.
    const suggested =
      screen.kind === "done"
        ? (screen.exported ?? screen.file.replace(/\.[^.]*$/, "") + ".srt")
        : "字幕.srt";

    const chosen = await save({
      defaultPath: suggested,
      filters: [
        { name: "SubRip 字幕", extensions: ["srt"] },
        { name: "纯文本", extensions: ["txt"] },
      ],
    });
    if (!chosen) return;

    try {
      await exportTranscript(chosen);
    } catch (cause) {
      // The backend says why — an extension it cannot render, or a directory
      // it cannot write to — and repeating it verbatim is more use than a
      // generic apology.
      modelError = String(cause);
    }
  }

  async function startDownload() {
    modelError = null;
    try {
      await fetchModel();
    } catch (cause) {
      modelError = String(cause);
    }
  }

  async function pickModelFolder() {
    modelError = null;
    const chosen = await open({ directory: true, multiple: false });
    if (typeof chosen !== "string") return;

    try {
      await importModel(chosen);
    } catch (cause) {
      modelError = String(cause);
    }
  }

  async function showAbout() {
    aboutContent = await about();
    aboutOpen = true;
  }

  /// How far along a download is, as a whole percent, when that is knowable.
  function percent(download: Download): number | null {
    if (download.state !== "fetching") return null;
    if (!download.totalBytes) return null;
    return Math.round((download.receivedBytes / download.totalBytes) * 100);
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

</script>

<div class="flex h-full flex-col">
  <header class="flex h-12 shrink-0 items-center gap-3 border-b px-5">
    <span class="font-heading text-sm font-semibold tracking-tight">Verse</span>
    {#if screen.kind === "working"}
      <span class="text-muted-foreground text-xs">
        {screen.file} · 已识别 {segments.length} 段 · {timecode(elapsedMs)}
      </span>
    {/if}
    <span class="flex-1"></span>
    {#if screen.kind === "working"}
      <Button
        variant="ghost"
        size="sm"
        onclick={() => void cancel()}
        disabled={screen.stopping}
      >
        {screen.stopping ? "正在停止…" : "取消"}
      </Button>
    {/if}
    <Button variant="ghost" size="sm" onclick={() => void showAbout()}>关于</Button>
  </header>

  {#if screen.kind === "empty"}
    <button
      class="m-6 flex flex-1 cursor-pointer flex-col items-center justify-center gap-3 rounded-lg border-2 border-dashed transition-colors
             {dragging
        ? 'border-primary bg-primary/5'
        : 'border-border hover:border-muted-foreground/40'}"
      onclick={() => void pick()}
    >
      <p class="text-base">{dragging ? "松开即可开始" : "把音频文件拖到这里"}</p>
      <p class="text-muted-foreground text-sm">或者点击选择文件</p>
      <p class="text-muted-foreground/70 mt-2 text-xs">
        支持 mp3、wav、m4a、mp4 等常见格式
      </p>
    </button>
  {:else if screen.kind === "working" || screen.kind === "done"}
    <div
      bind:this={transcriptEl}
      onscroll={onScroll}
      class="min-h-0 flex-1 overflow-y-auto px-6 py-5 select-text"
    >
      {#if segments.length === 0}
        <p class="text-muted-foreground text-sm">
          {screen.kind === "working" ? "正在准备…" : "没有识别到内容。"}
        </p>
      {:else}
        <div class="flex flex-col gap-2.5">
          {#each segments as segment, index (index)}
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

    {#if screen.kind === "done"}
      <footer class="flex shrink-0 items-center gap-3 border-t px-6 py-3">
        <span class="text-muted-foreground text-xs">
          完成 · 共 {segments.length} 段{#if screen.exported}
            · 已保存到 {screen.exported}{/if}
        </span>
        {#if screen.saveError}
          <!-- Not a failure of the job: the transcript is here and 另存为 still
               works. It says the result is not in the output folder, which is
               the one thing the window cannot work out for itself. -->
          <span class="text-destructive text-xs">未能自动保存：{screen.saveError}</span>
        {/if}
        {#if modelError}
          <span class="text-destructive text-xs">{modelError}</span>
        {/if}
        <span class="flex-1"></span>
        <Button variant="ghost" size="sm" onclick={() => void writeTranscript()}>另存为…</Button>
        <Button variant="ghost" size="sm" onclick={() => void reset()}>再来一个</Button>
      </footer>
    {/if}
  {:else if screen.kind === "failed"}
    <div class="flex flex-1 flex-col items-center justify-center gap-4 px-8 text-center">
      <p class="text-destructive text-sm font-medium">转写失败</p>
      <p class="text-muted-foreground max-w-md text-sm leading-relaxed">{screen.reason}</p>
      <p class="text-muted-foreground/70 text-xs">{screen.file}</p>
      <div class="mt-2 flex gap-2">
        {#if lastPath && screen.recovery !== "pickAnotherFile"}
          <Button onclick={retry}>重试</Button>
          <Button variant="ghost" onclick={() => void reset()}>换一个文件</Button>
        {:else}
          <Button onclick={() => void reset()}>换一个文件</Button>
        {/if}
      </div>
    </div>
  {:else if screen.kind === "needsModel"}
    <div class="flex flex-1 flex-col items-center justify-center gap-4 px-8 text-center">
      <p class="text-sm font-medium">还缺少识别模型</p>
      <p class="text-muted-foreground max-w-md text-sm leading-relaxed">
        需要先准备 <span class="font-medium">{screen.model}</span>，之后才能转写。
        下载好之后会自动开始，不用再点一次。
      </p>
      <p class="text-muted-foreground/70 text-xs">{screen.file}</p>

      {#if screen.download.state === "fetching"}
        <div class="w-72 space-y-2">
          <Progress value={percent(screen.download) ?? 0} />
          <p class="text-muted-foreground text-xs">
            {megabytes(screen.download.receivedBytes)} MB
            {#if screen.download.totalBytes}
              / {megabytes(screen.download.totalBytes)} MB · {percent(screen.download)}%
            {/if}
          </p>
        </div>
      {:else if screen.download.state === "verifying"}
        <p class="text-muted-foreground text-xs">正在校验…</p>
      {:else if screen.download.state === "failed"}
        <p class="text-destructive max-w-md text-xs leading-relaxed">
          {screen.download.reason}
        </p>
        <Button onclick={() => void startDownload()}>再试一次</Button>
      {:else}
        <div class="flex gap-2">
          <Button onclick={() => void startDownload()}>下载模型</Button>
          <Button variant="ghost" onclick={() => void pickModelFolder()}>
            我已有模型文件夹
          </Button>
        </div>
      {/if}

      {#if modelError}
        <p class="text-destructive max-w-md text-xs leading-relaxed">{modelError}</p>
      {/if}

      <Button variant="ghost" size="sm" onclick={() => void reset()}>返回</Button>
    </div>
  {/if}

  <Dialog bind:open={aboutOpen}>
    <DialogContent class="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>
          {aboutContent?.name ?? "Verse"} {aboutContent?.version ?? ""}
        </DialogTitle>
        <DialogDescription>{aboutContent?.summary ?? ""}</DialogDescription>
      </DialogHeader>

      <dl class="space-y-2">
        {#each aboutContent?.attributions ?? [] as row}
          <div class="flex gap-3 text-xs">
            <dt class="text-muted-foreground w-24 shrink-0">{row.what}</dt>
            <dd>{row.who}</dd>
          </div>
        {/each}
      </dl>

      <p class="text-muted-foreground text-xs leading-relaxed">
        {aboutContent?.licenceNote ?? ""}
      </p>
    </DialogContent>
  </Dialog>
</div>
