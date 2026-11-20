#!/usr/bin/env bash
# One-command installer for agy-pulse. Does:
#   1. agy plugin install (or upgrade) from local repo or AGY_PULSE_REPO_URL
#   2. bootstrap runtime + configure settings.json statusLine
#
# Run from a normal shell — NOT from inside an active `agy` session
# (agy rewrites settings.json from in-memory state on exit).
#
# Env:
#   AGY_PULSE_REPO      owner/repo slug (e.g. hardworkDAD/agy-pulse)
#   AGY_PULSE_REPO_RAW  override raw GitHub base URL (forks / mirrors)
#   AGY_PULSE_REPO_URL  override git URL for `agy plugin install`
set -euo pipefail

DEFAULT_REPO_SLUG="hardworkDAD/agy-pulse"

if [ -n "${BASH_SOURCE[0]:-}" ]; then
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
else
  SCRIPT_DIR=""
  REPO_ROOT=""
fi

REPO_SLUG="${AGY_PULSE_REPO:-}"
REPO_RAW="${AGY_PULSE_REPO_RAW:-}"
REPO_URL="${AGY_PULSE_REPO_URL:-}"
HAS_EXPLICIT_REPO=0
if [ -n "$REPO_SLUG" ] || [ -n "$REPO_RAW" ] || [ -n "$REPO_URL" ]; then
  HAS_EXPLICIT_REPO=1
fi

IS_LOCAL_REPO=0
if [ -n "$REPO_ROOT" ] && [ -f "$REPO_ROOT/plugin.json" ] && [ -f "$REPO_ROOT/scripts/bootstrap.js" ]; then
  IS_LOCAL_REPO=1
fi

if [ "$HAS_EXPLICIT_REPO" -eq 0 ] && [ "$IS_LOCAL_REPO" -eq 0 ]; then
  REPO_SLUG="$DEFAULT_REPO_SLUG"
fi

if [ -z "$REPO_RAW" ] && [ -n "$REPO_SLUG" ]; then
  REPO_RAW="https://raw.githubusercontent.com/$REPO_SLUG/main"
fi
if [ -z "$REPO_URL" ] && [ -n "$REPO_SLUG" ]; then
  REPO_URL="https://github.com/$REPO_SLUG.git"
fi
if [ "$HAS_EXPLICIT_REPO" -eq 1 ] && [ "$IS_LOCAL_REPO" -eq 0 ]; then
  [ -n "$REPO_RAW" ] || REPO_RAW="https://raw.githubusercontent.com/$DEFAULT_REPO_SLUG/main"
  [ -n "$REPO_URL" ] || REPO_URL="https://github.com/$DEFAULT_REPO_SLUG.git"
fi

if ! command -v agy >/dev/null 2>&1; then
  echo "agy-pulse: agy CLI not on PATH. Install it first:" >&2
  echo "  curl -fsSL https://antigravity.google/cli/install.sh | bash" >&2
  exit 1
fi
if ! command -v node >/dev/null 2>&1; then
  echo "agy-pulse: node is required but not on PATH." >&2
  exit 1
fi

# Step 1: stage the plugin. Uninstall first so stale files don't linger.
echo "==> Removing any existing agy-pulse plugin install..."
agy plugin uninstall agy-pulse >/dev/null 2>&1 || true

if [ "$HAS_EXPLICIT_REPO" -eq 1 ] && [ -n "$REPO_URL" ]; then
  echo "==> Installing plugin from $REPO_URL..."
  agy plugin install "$REPO_URL"
elif [ "$IS_LOCAL_REPO" -eq 1 ]; then
  echo "==> Installing plugin from local repository $REPO_ROOT..."
  agy plugin install "$REPO_ROOT"
else
  echo "==> Installing plugin from $REPO_URL..."
  agy plugin install "$REPO_URL"
fi

# Step 2: install runtime + configure statusLine
echo
echo "==> Bootstrapping runtime + statusLine config..."
if [ "$HAS_EXPLICIT_REPO" -eq 0 ] && [ "$IS_LOCAL_REPO" -eq 1 ] && [ -f "$REPO_ROOT/runtime/bin/agy-pulse.js" ]; then
  node "$REPO_ROOT/scripts/bootstrap.js"
elif [ -z "$REPO_RAW" ] && [ "$IS_LOCAL_REPO" -eq 1 ] && [ -f "$REPO_ROOT/runtime/bin/agy-pulse.js" ]; then
  node "$REPO_ROOT/scripts/bootstrap.js"
else
  if ! command -v curl >/dev/null 2>&1; then
    echo "agy-pulse: curl is required but not on PATH." >&2
    exit 1
  fi
  AGY_PULSE_REPO_RAW="$REPO_RAW" node <(curl -fsSL "$REPO_RAW/scripts/bootstrap.js")
fi

cat <<DONE

==> agy-pulse installed.
    Open a new agy session to see the agy-pulse status line.
    Repair anytime by re-running this script.
DONE
