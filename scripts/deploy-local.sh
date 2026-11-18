#!/usr/bin/env bash
# Deploy unified statusline monorepo (agy-hud + agy-statusline + statusline-combined.sh)
# into ~/.gemini/antigravity-cli/ without overwriting an existing runtime agy-hud.config.json
# unless --sync-config is passed.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AGY_DIR="${AGY_HUD_DATA_DIR:-$HOME/.gemini/antigravity-cli}"
RUNTIME_DST="$AGY_DIR/agy-hud-runtime"
SL_DST="$AGY_DIR/agy-statusline"
SYNC_CONFIG=0

for arg in "$@"; do
  case "$arg" in
    --sync-config) SYNC_CONFIG=1 ;;
    *)
      echo "Unknown option: $arg" >&2
      echo "Usage: $0 [--sync-config]" >&2
      exit 1
      ;;
  esac
done

mkdir -p "$RUNTIME_DST/runtime/bin" "$RUNTIME_DST/runtime/quota" "$RUNTIME_DST/runtime/renderer" "$SL_DST"

cp -f "$REPO_ROOT/package.json" "$RUNTIME_DST/package.json"

for f in \
  bin/agy-hud.js \
  config.js \
  cost.js \
  daily-cost.js \
  encoding.js \
  parser.js \
  paths.js \
  quota.js \
  quota/cache.js \
  quota/cloud.js \
  quota/models.js \
  quota/token.js \
  renderer.js \
  renderer/format.js \
  renderer/lang.js \
  renderer/quota-render.js \
  speed.js \
  statusline-installer.js \
  uninstall.js \
  update-checker.js; do
  cp -f "$REPO_ROOT/runtime/$f" "$RUNTIME_DST/runtime/$f"
done

if [ "$SYNC_CONFIG" -eq 1 ] || [ ! -f "$RUNTIME_DST/runtime/agy-hud.config.json" ]; then
  cp -f "$REPO_ROOT/combined/agy-hud.config.json" "$RUNTIME_DST/runtime/agy-hud.config.json"
fi

cp -f "$REPO_ROOT/agy-statusline/statusline.sh" "$SL_DST/statusline.sh"
chmod +x "$SL_DST/statusline.sh"

cp -f "$REPO_ROOT/statusline-combined.sh" "$AGY_DIR/statusline-combined.sh"
chmod +x "$AGY_DIR/statusline-combined.sh"

echo "Deployed unified statusline from $REPO_ROOT -> $AGY_DIR"
