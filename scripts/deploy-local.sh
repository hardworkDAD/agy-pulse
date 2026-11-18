#!/usr/bin/env bash
# Deploy unified single-process agy-pulse runtime into ~/.gemini/antigravity-cli/
# (and mirror to legacy agy-hud-runtime), smoothly migrating settings.json from
# statusline-combined.sh / agy-hud.js to single-process Node agy-pulse.js, and
# syncing skills/ under ~/.gemini/config/plugins/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AGY_DIR="${AGY_PULSE_DATA_DIR:-${AGY_HUD_DATA_DIR:-$HOME/.gemini/antigravity-cli}}"
PULSE_DST="$AGY_DIR/agy-pulse-runtime"
LEGACY_DST="$AGY_DIR/agy-hud-runtime"
CONFIG_PLUGINS_DIR="${XDG_CONFIG_HOME:-$HOME/.gemini/config}/plugins"
if [ "$CONFIG_PLUGINS_DIR" = "$HOME/.gemini/config/plugins" ]; then
  PLUGIN_PULSE_DIR="$HOME/.gemini/config/plugins/agy-pulse"
  PLUGIN_LEGACY_DIR="$HOME/.gemini/config/plugins/agy-hud"
else
  PLUGIN_PULSE_DIR="$CONFIG_PLUGINS_DIR/agy-pulse"
  PLUGIN_LEGACY_DIR="$CONFIG_PLUGINS_DIR/agy-hud"
fi

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

sync_runtime_tree() {
  local dst="$1"
  mkdir -p "$dst/runtime/bin" "$dst/runtime/quota" "$dst/runtime/renderer"
  cp -f "$REPO_ROOT/package.json" "$dst/package.json"

  local had_existing_cfg=0
  local cfg_backup=""
  if [ "$SYNC_CONFIG" -eq 0 ] && [ -f "$dst/runtime/agy-hud.config.json" ]; then
    had_existing_cfg=1
    cfg_backup="$(mktemp)"
    cp -f "$dst/runtime/agy-hud.config.json" "$cfg_backup"
  fi

  for f in \
    bin/agy-pulse.js \
    bin/agy-pulse.cmd \
    bin/agy-hud.js \
    bin/agy-hud.cmd \
    brand.js \
    compaction.js \
    config-wizard.js \
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
    renderer/context-monitor.js \
    renderer/format.js \
    renderer/lang.js \
    renderer/quota-render.js \
    speed.js \
    statusline-installer.js \
    uninstall.js \
    update-checker.js; do
    if [ -f "$REPO_ROOT/runtime/$f" ]; then
      cp -f "$REPO_ROOT/runtime/$f" "$dst/runtime/$f"
    fi
  done

  chmod +x "$dst/runtime/bin/agy-pulse.js" "$dst/runtime/bin/agy-hud.js"

  if [ "$had_existing_cfg" -eq 1 ]; then
    cp -f "$cfg_backup" "$dst/runtime/agy-hud.config.json"
    rm -f "$cfg_backup"
  else
    cp -f "$REPO_ROOT/runtime/agy-hud.config.json" "$dst/runtime/agy-hud.config.json"
  fi

  # Ensure showContextMonitor defaults to true during migration while preserving user customizations
  node -e '
    const fs = require("fs");
    const p = process.argv[1];
    try {
      const cfg = JSON.parse(fs.readFileSync(p, "utf8"));
      cfg.display = cfg.display || {};
      if (cfg.display.showContextMonitor === undefined) {
        cfg.display.showContextMonitor = true;
      }
      fs.writeFileSync(p, JSON.stringify(cfg, null, 2) + "\n", "utf8");
    } catch {}
  ' "$dst/runtime/agy-hud.config.json"
}

sync_runtime_tree "$PULSE_DST"
sync_runtime_tree "$LEGACY_DST"

# Hot-session zero-restart deduplication shim: replace legacy statusline-combined.sh
# (if present) with a single-process exec forwarder to agy-pulse.js so running agy
# sessions stop invoking agy-statusline/statusline.sh on the very next refresh.
if [ -e "$AGY_DIR/statusline-combined.sh" ] || [ -L "$AGY_DIR/statusline-combined.sh" ]; then
  rm -f "$AGY_DIR/statusline-combined.sh"
  cat > "$AGY_DIR/statusline-combined.sh" <<'EOF'
#!/usr/bin/env bash
exec "${AGY_STATUSLINE_NODE:-/usr/bin/node}" "${AGY_PULSE_DATA_DIR:-${AGY_HUD_DATA_DIR:-$HOME/.gemini/antigravity-cli}}/agy-pulse-runtime/runtime/bin/agy-pulse.js" "$@"
EOF
  chmod +x "$AGY_DIR/statusline-combined.sh"
fi

# Smoothly upgrade settings.json (and any secondary Gemini settings.json with an
# existing statusLine) to single-process Node agy-pulse.js while preserving flags.
node -e '
  const fs = require("fs");
  const path = require("path");
  const agyDir = process.argv[1];
  const pulseDst = process.argv[2];
  const homeDir = process.env.HOME || "";
  const primarySettingsPath = path.join(agyDir, "settings.json");
  const entryPath = path.join(pulseDst, "runtime", "bin", "agy-pulse.js");
  const targetCommand = `"${process.execPath}" "${entryPath}"`;

  function upgradeSettings(settingsPath, createIfMissing) {
    if (!createIfMissing && !fs.existsSync(settingsPath)) return;
    let settings = {};
    if (fs.existsSync(settingsPath)) {
      try {
        settings = JSON.parse(fs.readFileSync(settingsPath, "utf8"));
      } catch {
        if (!createIfMissing) return;
        settings = {};
      }
    }
    if (!createIfMissing && (!settings || typeof settings !== "object" || !settings.statusLine)) {
      return;
    }
    const existing = (settings.statusLine && typeof settings.statusLine === "object")
      ? settings.statusLine
      : {};
    settings.statusLine = {
      ...existing,
      type: "command",
      command: targetCommand,
    };
    fs.mkdirSync(path.dirname(settingsPath), { recursive: true });
    fs.writeFileSync(settingsPath, JSON.stringify(settings, null, 2) + "\n", "utf8");
  }

  upgradeSettings(primarySettingsPath, true);

  const secondaryCandidates = homeDir
    ? [
        path.join(homeDir, ".gemini", "config", "settings.json"),
        path.join(homeDir, ".gemini", "settings.json"),
      ]
    : [];
  const primaryResolved = path.resolve(primarySettingsPath);
  for (const candidate of secondaryCandidates) {
    if (path.resolve(candidate) !== primaryResolved) {
      upgradeSettings(candidate, false);
    }
  }
' "$AGY_DIR" "$PULSE_DST"

# Sync plugin skill under ~/.gemini/config/plugins/
mkdir -p "$PLUGIN_PULSE_DIR/skills/hud-config"
cp -f "$REPO_ROOT/plugin.json" "$PLUGIN_PULSE_DIR/plugin.json"
cp -f "$REPO_ROOT/gemini-extension.json" "$PLUGIN_PULSE_DIR/gemini-extension.json"
cp -f "$REPO_ROOT/skills/hud-config/SKILL.md" "$PLUGIN_PULSE_DIR/skills/hud-config/SKILL.md"

if [ -d "$PLUGIN_LEGACY_DIR" ]; then
  mkdir -p "$PLUGIN_LEGACY_DIR/skills/hud-config"
  cp -f "$REPO_ROOT/skills/hud-config/SKILL.md" "$PLUGIN_LEGACY_DIR/skills/hud-config/SKILL.md"
fi

echo "Deployed agy-pulse single-process runtime from $REPO_ROOT -> $AGY_DIR"
