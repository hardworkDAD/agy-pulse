#!/usr/bin/env bash
# Remove agy-pulse from this machine (zero Node.js dependency).
# Runs native `agypl uninstall` / `agy-pulse uninstall` (forwarding flags such as --purge-ledger),
# then cleans up the agy plugin registration & plugin directory.
set -uo pipefail

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

UNINSTALL_RC=0
if [ -n "$NATIVE_BIN" ]; then
  "$NATIVE_BIN" uninstall "$@" || UNINSTALL_RC=$?
else
  echo "agy-pulse uninstall: native binary not found — skipping settings & cache cleanup" >&2
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
exit "$UNINSTALL_RC"
