#!/usr/bin/env bash
# Remove agy-pulse from this machine.
# 1. Run runtime/uninstall.js (clears settings.statusLine, removes runtime dirs, tmp tokens)
# 2. Run `agy plugin uninstall agy-pulse`
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

RUNTIME_UNINSTALL=""
CANDIDATES=()
if [ -n "${AGY_PULSE_DATA_DIR:-}" ]; then
  CANDIDATES+=("${AGY_PULSE_DATA_DIR}/agy-pulse-runtime/runtime/uninstall.js")
fi
CANDIDATES+=("${HOME}/.gemini/antigravity-cli/agy-pulse-runtime/runtime/uninstall.js")
if [ -n "${XDG_DATA_HOME:-}" ]; then
  CANDIDATES+=("${XDG_DATA_HOME}/antigravity-cli/agy-pulse-runtime/runtime/uninstall.js")
fi
if [ -n "${APPDATA:-}" ]; then
  CANDIDATES+=("${APPDATA}/antigravity-cli/agy-pulse-runtime/runtime/uninstall.js")
fi
if [ -n "${LOCALAPPDATA:-}" ]; then
  CANDIDATES+=("${LOCALAPPDATA}/antigravity-cli/agy-pulse-runtime/runtime/uninstall.js")
fi
CANDIDATES+=("${SCRIPT_DIR}/runtime/uninstall.js")

for candidate in "${CANDIDATES[@]}"; do
  if [ -f "$candidate" ]; then
    RUNTIME_UNINSTALL="$candidate"
    break
  fi
done

if command -v node >/dev/null 2>&1 && [ -n "$RUNTIME_UNINSTALL" ]; then
  node "$RUNTIME_UNINSTALL"
else
  echo "agy-pulse uninstall: runtime not found — skipping settings cleanup" >&2
fi

PLUGIN_NAME="agy-pulse"
PLUGIN_DIRS=("${HOME}/.gemini/config/plugins/${PLUGIN_NAME}")
if [ -n "${XDG_CONFIG_HOME:-}" ]; then
  PLUGIN_DIRS+=("${XDG_CONFIG_HOME}/gemini/plugins/${PLUGIN_NAME}")
fi

if command -v agy >/dev/null 2>&1; then
  if ! agy plugin uninstall "$PLUGIN_NAME" 2>/dev/null; then
    for dir in "${PLUGIN_DIRS[@]}"; do
      if [ -d "$dir" ]; then
        echo "agy-pulse uninstall: 'agy plugin uninstall $PLUGIN_NAME' failed — removing dir $dir" >&2
        rm -rf "$dir"
      fi
    done
  fi
else
  for dir in "${PLUGIN_DIRS[@]}"; do
    if [ -d "$dir" ]; then
      echo "agy-pulse uninstall: agy CLI not on PATH — removing $dir directly" >&2
      rm -rf "$dir"
    fi
  done
fi

echo "agy-pulse uninstall complete"
