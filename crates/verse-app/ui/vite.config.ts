import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

/**
 * The dev server port, read from `tauri.conf.json`.
 *
 * That file is the only place the port is written down. Tauri loads
 * `build.devUrl` in the webview, so if the two disagree the window shows
 * whatever else is listening on that port — or nothing at all. Reading it from
 * the same file makes disagreement impossible rather than merely discouraged.
 *
 * This is not hypothetical: the default 5173 was in use by an unrelated Vite
 * instance during setup, and the window quietly attached to it.
 */
function devPort(): number {
  const configPath = new URL("../tauri.conf.json", import.meta.url);

  let config: unknown;
  try {
    config = JSON.parse(readFileSync(configPath, "utf8"));
  } catch (cause) {
    throw new Error(`could not read ${configPath.pathname}: ${String(cause)}`);
  }

  const url = (config as { build?: { devUrl?: unknown } })?.build?.devUrl;
  if (typeof url !== "string") {
    throw new Error(
      `${configPath.pathname} has no build.devUrl, so the dev port is unknown`,
    );
  }

  const port = Number(new URL(url).port);
  if (!Number.isInteger(port) || port <= 0) {
    throw new Error(`build.devUrl is not a usable address: ${url}`);
  }

  return port;
}

export default defineConfig({
  plugins: [tailwindcss(), svelte()],
  resolve: {
    // Must match `paths` in tsconfig.app.json. shadcn-svelte's components
    // import each other as `$lib/...`, and the type checker and the bundler
    // have to agree on what that means.
    alias: {
      $lib: fileURLToPath(new URL("./src/lib", import.meta.url)),
    },
  },
  server: {
    // Pinned to IPv4 on purpose. `localhost` can resolve to ::1 on Windows
    // while the server is bound to 127.0.0.1, and the webview then reaches
    // nothing. An explicit address cannot disagree with itself.
    host: "127.0.0.1",
    port: devPort(),
    // Fail rather than slide to the next free port. Vite's default is to move
    // on when a port is busy, which here would leave the dev server somewhere
    // the webview is not looking — a blank window instead of an error.
    strictPort: true,
  },
});
