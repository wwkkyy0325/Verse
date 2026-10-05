/**
 * The backend, as the frontend sees it.
 *
 * Every call into Rust goes through this file, and every shape it returns is
 * declared here rather than inferred at the call site. That gives the IPC
 * boundary one place to look for a mismatch, which is the failure mode a
 * loosely typed bridge hides until runtime.
 *
 * The shapes mirror `ScreenView` and `Update` in `src/bridge.rs`.
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

/** Which screen is showing. Mirrors `ScreenView`. */
export type Screen =
  | { kind: "empty" }
  | { kind: "needsModel"; file: string; model: string }
  | { kind: "working"; file: string; stopping: boolean }
  | { kind: "done"; file: string }
  | { kind: "failed"; file: string; reason: string; recovery: Recovery };

/** One segment of recognised speech. */
export interface Segment {
  startMs: number;
  endMs: number;
  text: string;
}

/** What the backend says when something changes. Mirrors `Update`. */
export type Update =
  | { kind: "screen"; screen: Screen }
  | { kind: "segment"; startMs: number; endMs: number; text: string }
  | { kind: "progress"; elapsedMs: number }
  | { kind: "cleared" };

const UPDATE_EVENT = "verse://update";

export function probeHardware(): Promise<HardwareSummary> {
  return invoke<HardwareSummary>("hardware");
}

/** The screen to show on load, before any update arrives. */
export function currentScreen(): Promise<Screen> {
  return invoke<Screen>("current_screen");
}

export function transcribe(path: string): Promise<void> {
  return invoke<void>("transcribe", { path });
}

export function cancel(): Promise<void> {
  return invoke<void>("cancel");
}

/** Leave a finished or failed screen, back to the drop target. */
export function reset(): Promise<void> {
  return invoke<void>("reset");
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
