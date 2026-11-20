#!/usr/bin/env bash
# One-shot bootstrap for agy-pulse. Idempotent — re-run anytime to repair settings.json.
# Installs runtime to ~/.gemini/antigravity-cli/agy-pulse-runtime/ and configures statusLine.
#
# Env:
#   AGY_PULSE_REPO_RAW  raw GitHub base URL (e.g. https://raw.githubusercontent.com/hardworkDAD/agy-pulse/main)
#   AGY_PULSE_REPO      owner/repo shorthand (used if AGY_PULSE_REPO_RAW is unset; defaults to hardworkDAD/agy-pulse)
set -euo pipefail

if [ -n "${BASH_SOURCE[0]:-}" ]; then
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
else
  SCRIPT_DIR=""
  REPO_ROOT=""
fi

REPO_SLUG="${AGY_PULSE_REPO:-hardworkDAD/agy-pulse}"
REPO_RAW="${AGY_PULSE_REPO_RAW:-https://raw.githubusercontent.com/$REPO_SLUG/main}"

if ! command -v node >/dev/null 2>&1; then
  echo "agy-pulse bootstrap: node is required but not found in PATH" >&2
  exit 1
fi

if [ -n "$REPO_ROOT" ] && [ -f "$REPO_ROOT/scripts/bootstrap.js" ] && [ -f "$REPO_ROOT/runtime/bin/agy-pulse.js" ]; then
  exec node "$REPO_ROOT/scripts/bootstrap.js"
fi

if ! command -v curl >/dev/null 2>&1; then
  echo "agy-pulse bootstrap: curl is required but not found in PATH" >&2
  exit 1
fi

AGY_PULSE_REPO_RAW="$REPO_RAW" exec node <(curl -fsSL "$REPO_RAW/scripts/bootstrap.js")
