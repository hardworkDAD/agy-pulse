#!/usr/bin/env bash
# Combined Antigravity CLI status line.
#   - agy-hud        : quota table, workspace/model context, speed, project info
#   - agy-statusline : real server-pushed compaction ceiling, compaction count,
#                      cache hit %, output tokens
# Both renderers read the same statusLine JSON from stdin once; we fan it out
# to a temp file and run them in parallel.

set -u

AGY_DIR="${HOME}/.gemini/antigravity-cli"
HUD_JS="${AGY_DIR}/agy-hud-runtime/runtime/bin/agy-hud.js"
SL_PY="${AGY_DIR}/agy-statusline/statusline.sh"
NODE_BIN="${AGY_STATUSLINE_NODE:-/usr/bin/node}"
PY_BIN="${AGY_STATUSLINE_PYTHON:-/usr/bin/python3}"

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/agy-statusline.XXXXXX")" || exit 0
cleanup() { rm -rf "$tmpdir"; }
trap cleanup EXIT INT TERM

cat > "$tmpdir/stdin.json"

hud_pid=""
sl_pid=""
if [ -s "$tmpdir/stdin.json" ]; then
  "$NODE_BIN" "$HUD_JS" < "$tmpdir/stdin.json" > "$tmpdir/hud.out" 2>/dev/null &
  hud_pid=$!
  "$PY_BIN" "$SL_PY" < "$tmpdir/stdin.json" > "$tmpdir/sl.out" 2>/dev/null &
  sl_pid=$!
  [ -n "$hud_pid" ] && wait "$hud_pid" 2>/dev/null || true
  [ -n "$sl_pid" ] && wait "$sl_pid" 2>/dev/null || true
fi

ends_with_newline() {
  [ -s "$1" ] && [ "$(tail -c 1 "$1" | od -An -t u1 | tr -d ' \n')" = "10" ]
}

# agy-hud block first (it renders multiple lines), then the agy-statusline
# monitor line underneath.
if [ -s "$tmpdir/hud.out" ]; then
  cat "$tmpdir/hud.out"
  ends_with_newline "$tmpdir/hud.out" || printf '\n'
fi

if [ -s "$tmpdir/sl.out" ]; then
  cat "$tmpdir/sl.out"
  ends_with_newline "$tmpdir/sl.out" || printf '\n'
fi
