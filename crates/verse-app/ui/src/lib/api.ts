/**
 * The backend, as the frontend sees it.
 *
 * Every call into Rust goes through this file, and every shape it returns is
 * declared here rather than inferred at the call site. That gives the IPC
 * boundary one place to look for a mismatch, which is the failure mode a
 * loosely typed bridge hides until runtime.
 *
 * The shapes mirror `ScreenView`, `Update`, `StateView`, `EntryView`,
 * `ModelChoice` and `SummaryView` in `src/bridge.rs`.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";

/** Mirrors `HardwareSummary` in `src/lib.rs`. */
export interface HardwareSummary {
  avx2: boolean;
  cores: number;
  /** Cores reserved for recognition; one is always left for the interface. */
  threads: number;
  /** Why this machine is being worked around, if it is. */
  degraded: string | null;
}

/** What the user can do about a failure. Mirrors `RecoveryView`. */
export type Recovery = "retry" | "getModel" | "pickAnotherFile";

/** A download with byte counts on it. */
export type DownloadFetching = {
  state: "fetching";
  file: string;
  /** The file being pulled, which is one of the model's several. */
  receivedBytes: number;
  totalBytes: number | null;
  /**
   * The same two numbers for the whole model.
   *
   * `null` when the reading does not know them. A model is several files pulled
   * one at a time, so the per-file figures fill up once per file and look like
   * a finished download five times over.
   */
  modelReceivedBytes: number | null;
  modelTotalBytes: number | null;
};

/** How a model download is going. Mirrors `DownloadView`. */
export type Download =
  | { state: "idle" }
  | DownloadFetching
  | { state: "verifying" }
  | { state: "ready" }
  | { state: "failed"; reason: string };

/** What the detail region is showing. Mirrors `ScreenView`. */
export type Screen =
  | { kind: "empty" }
  | { kind: "queued"; file: string }
  | { kind: "needsModel"; file: string; modelId: string; download: Download }
  | {
      kind: "working";
      file: string;
      stopping: boolean;
      elapsedMs: number;
      /** `null` when the file's length is not known, which is not zero. */
      fraction: number | null;
    }
  | { kind: "done"; file: string; exported: string | null; saveError: string | null }
  | { kind: "failed"; file: string; reason: string; recovery: Recovery };

/** One segment of recognised speech. */
export interface Segment {
  startMs: number;
  endMs: number;
  text: string;
}

/** What a row of the file list is doing. Mirrors `EntryState`. */
export type EntryState = "queued" | "working" | "done" | "failed" | "needsModel";

/** One row of the file list. Mirrors `EntryView`. */
export interface Entry {
  name: string;
  state: EntryState;
  /** Which engine this file was handed to. The engine can change between files. */
  engine: string;
  /** Whether there is a file to delete for this row. */
  hasResult: boolean;
}

/** Everything the window draws. Mirrors `StateView`. */
export interface State {
  screen: Screen;
  entries: Entry[];
  selected: number | null;
  segments: Segment[];
}

/** What the backend says when something changes. Mirrors `Update`. */
export type Update =
  | { kind: "screen"; screen: Screen }
  | { kind: "roster"; entries: Entry[]; selected: number | null }
  | { kind: "selected"; screen: Screen; segments: Segment[] }
  | { kind: "segment"; startMs: number; endMs: number; text: string }
  | { kind: "progress"; elapsedMs: number; fraction: number | null }
  | { kind: "download"; model: string; download: Download }
  | { kind: "cleared" }
  | { kind: "snapshot"; state: State };

/** A model the user can choose. Mirrors `ModelChoice`. */
export interface ModelChoice {
  id: string;
  /** The catalogue's own name, verbatim — a licence requires it unaltered. */
  name: string;
  description: string | null;
  present: boolean;
  bytes: number;
  default: boolean;
}

/** One route of the local service. Mirrors `Route`. */
export interface Route {
  method: string;
  path: string;
  what: string;
}

/** What the 关于 dialog shows: attribution, licence, and how to drive it. */
export interface About {
  name: string;
  version: string;
  summary: string;
  attributions: { what: string; who: string }[];
  licenceNote: string;
  /** The service's routes, listed beside the brief. */
  routes: Route[];
  /**
   * The same API written out for a program rather than a person, in English,
   * ready to hand to an agent.
   */
  agentBrief: string;
}

const UPDATE_EVENT = "verse://update";

export function probeHardware(): Promise<HardwareSummary> {
  return invoke<HardwareSummary>("hardware");
}

/** Everything, on load. After this the window applies `Update`s. */
export function currentState(): Promise<State> {
  return invoke<State>("current_state");
}

/** The engines this build can run, with what choosing one costs. */
export function models(): Promise<ModelChoice[]> {
  return invoke<ModelChoice[]>("models");
}

export function transcribe(path: string): Promise<void> {
  return invoke<void>("transcribe", { path });
}

/**
 * Take a file out of the list, leaving its transcript where it is.
 *
 * The record is forgotten too, or the row would be back on the next launch.
 */
export function forget(index: number): Promise<void> {
  return invoke<void>("forget", { index });
}

/** Show the transcript being displayed in the platform's file manager. */
export function reveal(): Promise<void> {
  return invoke<void>("reveal");
}

/**
 * Delete one row's transcript, from the disk as well as the list.
 *
 * The irreversible half, and it takes the row rather than the selection because
 * the window asks from the list. Where the window asks which of the two is
 * meant; this does not ask again, because a command that pops its own
 * confirmation cannot be driven by a test.
 */
export function deleteResult(index: number): Promise<void> {
  return invoke<void>("delete_result", { index });
}

/** Show a different file from the list. */
export function select(index: number): Promise<void> {
  return invoke<void>("select", { index });
}

/** Choose an engine. Rejected while a job is running. */
export function selectModel(id: string): Promise<void> {
  return invoke<void>("select_model", { id });
}

export function cancel(): Promise<void> {
  return invoke<void>("cancel");
}

/**
 * Start the next file waiting in the queue.
 *
 * The queue advances by itself as jobs finish; this is for the one that came
 * back from a previous session and has nothing in front of it to advance from.
 * Until it is called those rows wait, deliberately — a window that started
 * recognising a batch the moment it opened would be doing work nobody asked
 * for.
 */
export function resumeQueue(): Promise<void> {
  return invoke<void>("resume");
}

/** A file the window will not hand over, and why. Mirrors `Refusal`. */
export interface Refusal {
  file: string;
  reason: string;
}

/** What came of checking a drop before starting. Mirrors `CheckResult`. */
export interface CheckResult {
  /** The paths that can be processed, in the order they arrived. */
  usable: string[];
  refused: Refusal[];
  /** What this build reads, so the dialog can name the formats. */
  accepted: string[];
}

/**
 * Look at what is about to be handed over, before anything starts.
 *
 * The rule lives in Rust — one list of what this program reads, shared with the
 * command line. This only carries the answer.
 */
export function checkFiles(paths: string[]): Promise<CheckResult> {
  return invoke<CheckResult>("check_files", { paths });
}

/**
 * Run the file being shown again, after a failure.
 *
 * Distinct from handing the same path over again: that is read as "here it
 * is", because the file is already in the list, and starts nothing.
 */
export function retry(): Promise<void> {
  return invoke<void>("retry");
}

/** Leave the file being shown, back to the drop target. */
export function reset(): Promise<void> {
  return invoke<void>("reset");
}

/** Write the finished transcript out, in the format the extension names. */
export function exportTranscript(path: string): Promise<void> {
  return invoke<void>("export", { path });
}

/**
 * Whether the demonstration can be run at all.
 *
 * Asked so the window can *offer* it rather than run it. A release build
 * answers `false` and has no code behind the other half.
 */
export function demoAvailable(): Promise<boolean> {
  return invoke<boolean>("demo_available");
}

/**
 * Put the window through a job it did not have to wait for.
 *
 * A development tool. Answers `false` when the demonstration was not asked for
 * — the ordinary case, and not a failure — and `true` when it started. Only a
 * refusal to start is an error, and only that is worth showing.
 */
export function demoProgress(): Promise<boolean> {
  return invoke<boolean>("demo_progress");
}

/** Fetch the model the waiting file needs. Progress arrives as an update. */
export function fetchModel(model: string): Promise<void> {
  return invoke<void>("fetch_model", { model });
}

/** Use a model the user already has, copied in from wherever it is. */
export function importModel(path: string): Promise<void> {
  return invoke<void>("import_model", { path });
}

export function about(): Promise<About> {
  return invoke<About>("about");
}

export function onUpdate(handler: (update: Update) => void): Promise<UnlistenFn> {
  return listen<Update>(UPDATE_EVENT, (event) => handler(event.payload));
}

/**
 * Files dropped onto the window.
 *
 * Handled by Tauri rather than by the browser's drag events: the webview is a
 * native window, and an HTML drop target never sees a file arrive from the
 * operating system.
 */
export function onFileDrop(handler: (paths: string[]) => void): Promise<UnlistenFn> {
  return getCurrentWebview().onDragDropEvent((event) => {
    if (event.payload.type === "drop" && event.payload.paths.length > 0) {
      handler(event.payload.paths);
    }
  });
}
