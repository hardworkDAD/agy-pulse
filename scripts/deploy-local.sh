#!/usr/bin/env bash
# Build native Rust agy-pulse release binaries locally, deploy them into
# AGY_PULSE_BIN_DIR (default ~/.local/bin), register host settings.json,
# sync plugin manifest + skills/pulse-config/SKILL.md, and run an E2E smoke check.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${AGY_PULSE_BIN_DIR:-$HOME/.local/bin}"
if [ -n "${XDG_CONFIG_HOME:-}" ]; then
  CONFIG_PLUGINS_DIR="$XDG_CONFIG_HOME/gemini/plugins"
else
  CONFIG_PLUGINS_DIR="$HOME/.gemini/config/plugins"
fi
PLUGIN_PULSE_DIR="$CONFIG_PLUGINS_DIR/agy-pulse"

CARGO_ARGS=(build --release)
for arg in "$@"; do
  case "$arg" in
    --offline)
      CARGO_ARGS+=(--offline)
      ;;
    --bin-dir=*)
      BIN_DIR="${arg#--bin-dir=}"
      ;;
    *)
      echo "Unknown option: $arg" >&2
      echo "Usage: $0 [--offline] [--bin-dir=<dir>]" >&2
      exit 1
      ;;
  esac
done

if ! command -v cargo >/dev/null 2>&1; then
  PATH="${PATH:-}"
  CARGO_ENV="${CARGO_HOME:-$HOME/.cargo}/env"
  CARGO_BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
  if [ -f "$CARGO_ENV" ]; then
    # shellcheck disable=SC1090
    . "$CARGO_ENV"
  fi
  if ! command -v cargo >/dev/null 2>&1 && [ -x "$CARGO_BIN_DIR/cargo" ]; then
    export PATH="$CARGO_BIN_DIR:$PATH"
  fi
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "agy-pulse: cargo not found in PATH or ${CARGO_HOME:-$HOME/.cargo}/bin. Install Rust (https://rustup.rs) or run 'source \"\$HOME/.cargo/env\"'." >&2
  exit 1
fi

echo "==> Building release binaries (cargo ${CARGO_ARGS[*]})..."
cargo "${CARGO_ARGS[@]}" --manifest-path "$REPO_ROOT/Cargo.toml"

RELEASE_PRIMARY="$REPO_ROOT/target/release/agy-pulse"
RELEASE_ALIAS="$REPO_ROOT/target/release/agypl"
if [ ! -x "$RELEASE_PRIMARY" ] || [ ! -x "$RELEASE_ALIAS" ]; then
  echo "agy-pulse: release binaries not found under $REPO_ROOT/target/release" >&2
  exit 1
fi

echo "==> Installing binaries & registering host statusLine -> $BIN_DIR..."
"$RELEASE_PRIMARY" install --bin-dir "$BIN_DIR"

echo "==> Syncing plugin manifest & pulse-config skill -> $PLUGIN_PULSE_DIR..."
mkdir -p "$PLUGIN_PULSE_DIR/skills/pulse-config"
cp -f "$REPO_ROOT/plugin.json" "$PLUGIN_PULSE_DIR/plugin.json"
cp -f "$REPO_ROOT/gemini-extension.json" "$PLUGIN_PULSE_DIR/gemini-extension.json"
cp -f "$REPO_ROOT/skills/pulse-config/SKILL.md" "$PLUGIN_PULSE_DIR/skills/pulse-config/SKILL.md"

echo "==> Running end-to-end smoke self-check..."
VERSION_OUT="$("$BIN_DIR/agy-pulse" --version)"
"$BIN_DIR/agypl" --help >/dev/null
SMOKE_OUT="$(
  printf '{"model":{"id":"gemini-3-flash-agent","display_name":"Gemini 3 Flash"},"workspace":{"current_dir":"%s"}}' "$REPO_ROOT" \
    | AGY_PULSE_DISABLE_UPDATE_CHECK=1 "$BIN_DIR/agy-pulse"
)"
if [ -z "$SMOKE_OUT" ]; then
  echo "agy-pulse: smoke check produced empty statusline output" >&2
  exit 1
fi

echo "Deployed $VERSION_OUT -> $BIN_DIR (agy-pulse, agypl)"
echo "Synced plugin manifest -> $PLUGIN_PULSE_DIR"
case ":${PATH:-}:" in
  *":$BIN_DIR:"*) ;;
  *)
    echo "Hint: $BIN_DIR is not in your PATH. Add it to your shell profile:"
    echo "  export PATH=\"$BIN_DIR:\$PATH\""
    ;;
esac
