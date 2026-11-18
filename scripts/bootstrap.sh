#!/usr/bin/env bash
# One-shot bootstrap for agy-pulse. Idempotent — re-run anytime to repair settings.json.
# Installs runtime to ~/.gemini/antigravity-cli/agy-pulse-runtime/ and configures statusLine.
#
# Env:
#   AGY_PULSE_REPO_RAW  raw GitHub base URL (e.g. https://raw.githubusercontent.com/<user>/agy-pulse/main)
#   AGY_PULSE_REPO      owner/repo shorthand (used if AGY_PULSE_REPO_RAW is unset)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_SLUG="${AGY_PULSE_REPO:-${AGY_HUD_REPO:-}}"
REPO_RAW="${AGY_PULSE_REPO_RAW:-${AGY_HUD_REPO_RAW:-}}"
if [ -z "$REPO_RAW" ] && [ -n "$REPO_SLUG" ]; then
  REPO_RAW="https://raw.githubusercontent.com/$REPO_SLUG/main"
fi

if ! command -v node >/dev/null 2>&1; then
  echo "agy-pulse bootstrap: node is required but not found in PATH" >&2
  exit 1
fi

if [ -f "$REPO_ROOT/scripts/bootstrap.js" ] && [ -f "$REPO_ROOT/runtime/bin/agy-pulse.js" ]; then
  exec node "$REPO_ROOT/scripts/bootstrap.js"
fi

if [ -z "$REPO_RAW" ]; then
  echo "agy-pulse bootstrap: set AGY_PULSE_REPO_RAW or AGY_PULSE_REPO when running remotely" >&2
  exit 1
fi

if ! command -v curl >/dev/null 2>&1; then
  echo "agy-pulse bootstrap: curl is required but not found in PATH" >&2
  exit 1
fi

AGY_PULSE_REPO_RAW="$REPO_RAW" exec node <(curl -fsSL "$REPO_RAW/scripts/bootstrap.js")
