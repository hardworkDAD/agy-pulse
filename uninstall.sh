#!/usr/bin/env bash
# Remove agy-pulse (and any legacy agy-hud) from this machine.
# 1. Run runtime/uninstall.js (clears settings.statusLine, removes runtime dirs, tmp tokens)
# 2. Run `agy plugin uninstall` for agy-pulse and agy-hud
set -uo pipefail

RUNTIME_UNINSTALL="$HOME/.gemini/antigravity-cli/agy-pulse-runtime/runtime/uninstall.js"
LEGACY_UNINSTALL="$HOME/.gemini/antigravity-cli/agy-hud-runtime/runtime/uninstall.js"

if command -v node >/dev/null 2>&1 && [ -f "$RUNTIME_UNINSTALL" ]; then
  node "$RUNTIME_UNINSTALL"
elif command -v node >/dev/null 2>&1 && [ -f "$LEGACY_UNINSTALL" ]; then
  node "$LEGACY_UNINSTALL"
else
  echo "agy-pulse uninstall: runtime not found — skipping settings cleanup" >&2
fi

for PLUGIN_NAME in agy-pulse agy-hud; do
  PLUGIN_DIR="${HOME}/.gemini/config/plugins/${PLUGIN_NAME}"
  if command -v agy >/dev/null 2>&1; then
    if ! agy plugin uninstall "$PLUGIN_NAME" 2>/dev/null; then
      if [ -d "$PLUGIN_DIR" ]; then
        echo "agy-pulse uninstall: 'agy plugin uninstall $PLUGIN_NAME' failed — removing dir" >&2
        rm -rf "$PLUGIN_DIR"
      fi
    fi
  elif [ -d "$PLUGIN_DIR" ]; then
    echo "agy-pulse uninstall: agy CLI not on PATH — removing $PLUGIN_DIR directly" >&2
    rm -rf "$PLUGIN_DIR"
  fi
done

echo "agy-pulse uninstall complete"
