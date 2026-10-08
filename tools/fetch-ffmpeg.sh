#!/usr/bin/env bash
#
# Fetch the LGPL ffmpeg that releases bundle, and put it where the Tauri
# bundler expects a sidecar.
#
# `.github/workflows/release.yml` runs this, and so can you — which is the
# reason it is a script and not a `run:` block. The first release run this
# project ever did failed on this step, and the fault reproduces in Git Bash in
# seconds; it only stayed expensive because a step that lives in YAML can be
# debugged one CI run at a time.
#
# Usage: tools/fetch-ffmpeg.sh [output-dir]        (default: ffmpeg-bin)
#
# Behind a proxy, set HTTPS_PROXY — curl reads it, this does not.
#
# The output is named with the target triple because `tauri-bundler` strips
# exactly that suffix on the way in: it copies `ffmpeg-x86_64-pc-windows-msvc.exe`
# to `ffmpeg.exe` beside the program, which is where `verse-audio` looks for it.
# Read out of `tauri-bundler`'s `Settings::copy_binaries` rather than guessed —
# naming it `ffmpeg.exe` here would install a file the config cannot resolve.
set -euo pipefail

# BtbN rebuilds its `master` asset daily, and "latest" is not a version a
# licence notice can name. `n8.1` is one of its per-FFmpeg-release branches:
# a nameable version that still takes patch releases. See
# THIRD_PARTY_NOTICES.md.
asset="${FFMPEG_ASSET:-ffmpeg-n8.1-latest-win64-lgpl-8.1.zip}"
url="https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/${asset}"

out="${1:-ffmpeg-bin}"
dest="${out}/ffmpeg-x86_64-pc-windows-msvc.exe"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$out"

echo "fetching ${asset}"
curl -fsSL "$url" -o "${work}/ffmpeg.zip"
unzip -q "${work}/ffmpeg.zip" -d "${work}/unpacked"

# `\;`, not `+`. GNU find as shipped in Git Bash rejects the `+` form whenever
# anything follows `{}`: `-exec echo {} SUFFIX +` fails, `-exec echo PREFIX {} +`
# does not, and the copy here has a destination after `{}`. Measured, not
# reasoned — the first release run died on exactly this, with "find: missing
# argument to `-exec'".
find "${work}/unpacked" -name ffmpeg.exe -exec cp {} "$dest" \;

# Loud, because the failure it catches is otherwise silent: a `find` that
# matches nothing leaves `dest` missing, this step still exits 0, and the error
# surfaces much later inside the bundler as something else entirely.
test -s "$dest" || {
  echo "::error::${dest} was not produced from ${asset}" >&2
  exit 1
}

ls -l "$dest"
