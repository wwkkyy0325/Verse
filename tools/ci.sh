#!/usr/bin/env bash
#
# What `.github/workflows/ci.yml` checks, checked here first.
#
# The point is to spend a runner only on what has already passed locally. The
# first release run this project ever did died on a shell construct that
# reproduces in this shell in about two seconds; it cost a CI run and a round
# trip to find out. Run this before pushing.
#
# Two deliberate differences from the workflow:
#
#   - `npm ci` is not run here. It needs the network and wipes node_modules;
#     CI runs it because CI starts empty. This only checks that node_modules
#     is there at all.
#   - ffmpeg is not installed here. CI does that with choco. If it is missing
#     locally the audio tests skip, which is exactly the thing the workflow's
#     `ffmpeg -version` step exists to make visible, so this says so too.
#
# Usage: tools/ci.sh
set -euo pipefail

cd "$(dirname "$0")/.."

step() { printf '\n=== %s ===\n' "$1"; }
fail() { printf '\nFAILED: %s\n' "$1" >&2; exit 1; }

step "ffmpeg"
if ffmpeg -version >/dev/null 2>&1; then
  ffmpeg -version | head -1
else
  echo "not on PATH. The audio tests will skip; CI installs it, so a green run"
  echo "here is weaker than a green run there."
fi

step "frontend types"
if [ -d crates/verse-app/ui/node_modules ]; then
  ( cd crates/verse-app/ui && npm run check ) || fail "frontend types"
else
  fail "crates/verse-app/ui/node_modules is missing — run npm ci there first"
fi

step "tests"
cargo test --workspace || fail "tests"

step "clippy"
cargo clippy --workspace --all-targets -- -D warnings || fail "clippy"

printf '\n=== all green ===\n'
