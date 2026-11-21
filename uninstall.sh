#!/usr/bin/env bash
# Remove agy-pulse from this machine.
# 1. Prefer native v2 binary (`agypl uninstall` / `agy-pulse uninstall`), forwarding flags such as --purge-ledger.
# 2. Fallback to legacy v1 runtime/uninstall.js if no native binary is installed.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

NATIVE_BIN=""
BIN_CANDIDATES=()
if [ -n "${AGY_PULSE_BIN_DIR:-}" ]; then
  BIN_CANDIDATES+=("${AGY_PULSE_BIN_DIR}/agypl" "${AGY_PULSE_BIN_DIR}/agy-pulse")
fi
BIN_CANDIDATES+=("${HOME}/.local/bin/agypl" "${HOME}/.local/bin/agy-pulse")

for candidate in "${BIN_CANDIDATES[@]}"; do
  if [ -x "$candidate" ]; then
    NATIVE_BIN="$candidate"
    break
  fi
done

if [ -z "$NATIVE_BIN" ]; then
  if command -v agypl >/dev/null 2>&1; then
    NATIVE_BIN="agypl"
  elif command -v agy-pulse >/dev/null 2>&1; then
    NATIVE_BIN="agy-pulse"
  fi
fi

if [ -n "$NATIVE_BIN" ]; then
  exec "$NATIVE_BIN" uninstall "$@"
fi

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
  node "$RUNTIME_UNINSTALL" "$@"
else
  echo "agy-pulse uninstall: binary and legacy runtime not found — skipping settings cleanup" >&2
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
