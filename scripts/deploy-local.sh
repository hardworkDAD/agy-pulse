#!/usr/bin/env bash
# Deploy unified single-process agy-pulse runtime into ~/.gemini/antigravity-cli/,
# smoothly migrating settings.json from statusline-combined.sh to single-process
# Node agy-pulse.js, and syncing skills/ under ~/.gemini/config/plugins/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AGY_DIR="${AGY_PULSE_DATA_DIR:-$HOME/.gemini/antigravity-cli}"
PULSE_DST="$AGY_DIR/agy-pulse-runtime"
if [ -n "${XDG_CONFIG_HOME:-}" ]; then
  CONFIG_PLUGINS_DIR="$XDG_CONFIG_HOME/gemini/plugins"
else
  CONFIG_PLUGINS_DIR="$HOME/.gemini/config/plugins"
fi
PLUGIN_PULSE_DIR="$CONFIG_PLUGINS_DIR/agy-pulse"

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
  mkdir -p "$dst/runtime/bin" "$dst/runtime/quota" "$dst/runtime/renderer" "$dst/scripts"
  cp -f "$REPO_ROOT/package.json" "$dst/package.json"
  if [ -f "$REPO_ROOT/scripts/diagnose-auth.js" ]; then
    cp -f "$REPO_ROOT/scripts/diagnose-auth.js" "$dst/scripts/diagnose-auth.js"
  fi

  local had_existing_cfg=0
  local cfg_backup=""
  if [ "$SYNC_CONFIG" -eq 0 ] && [ -f "$dst/runtime/agy-pulse.config.json" ]; then
    had_existing_cfg=1
    cfg_backup="$(mktemp)"
    cp -f "$dst/runtime/agy-pulse.config.json" "$cfg_backup"
  fi

  for f in \
    bin/agy-pulse.js \
    bin/agy-pulse.cmd \
    brand.js \
    compaction.js \
    config-wizard.js \
    config.js \
    cost.js \
    daily-cost.js \
    dashboard-ui.js \
    dashboard.js \
    encoding.js \
    ledger.js \
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
    stats.js \
    statusline-installer.js \
    uninstall.js \
    update-checker.js; do
    if [ -f "$REPO_ROOT/runtime/$f" ]; then
      cp -f "$REPO_ROOT/runtime/$f" "$dst/runtime/$f"
    fi
  done

  chmod +x "$dst/runtime/bin/agy-pulse.js"

  if [ "$had_existing_cfg" -eq 1 ]; then
    cp -f "$cfg_backup" "$dst/runtime/agy-pulse.config.json"
    node -e 'require("fs").rmSync(process.argv[1], { force: true })' "$cfg_backup"
  else
    cp -f "$REPO_ROOT/runtime/agy-pulse.config.json" "$dst/runtime/agy-pulse.config.json"
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
  ' "$dst/runtime/agy-pulse.config.json"
}

sync_runtime_tree "$PULSE_DST"

# Hot-session zero-restart deduplication shim: replace legacy statusline-combined.sh
# (if present) with a single-process exec forwarder to agy-pulse.js so running agy
# sessions stop invoking agy-statusline/statusline.sh on the very next refresh.
if [ -e "$AGY_DIR/statusline-combined.sh" ] || [ -L "$AGY_DIR/statusline-combined.sh" ]; then
  node -e 'require("fs").rmSync(process.argv[1], { force: true })' "$AGY_DIR/statusline-combined.sh"
  NODE_EXEC_FALLBACK="$(node -e "process.stdout.write(process.execPath)")"
  cat > "$AGY_DIR/statusline-combined.sh" <<EOF
#!/usr/bin/env bash
exec "\$(_n="\${AGY_PULSE_NODE:-\${AGY_STATUSLINE_NODE:-}}"; if [ -n "\$_n" ] && command -v "\$_n" >/dev/null 2>&1; then printf '%s' "\$_n"; else command -v node 2>/dev/null || printf '%s' '$NODE_EXEC_FALLBACK'; fi)" "\${AGY_PULSE_DATA_DIR:-\$HOME/.gemini/antigravity-cli}/agy-pulse-runtime/runtime/bin/agy-pulse.js" "\$@"
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
      const raw = fs.readFileSync(settingsPath, "utf8");
      if (raw.trim()) {
        try {
          settings = JSON.parse(raw);
          if (!settings || typeof settings !== "object" || Array.isArray(settings)) {
            settings = {};
          }
        } catch {
          if (!createIfMissing) return;
          try { fs.copyFileSync(settingsPath, `${settingsPath}.bak`); } catch {}
          settings = {};
        }
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

# Clean stale plugin artifacts, legacy agy-hud files, dirty speed-cache entries,
# stale update-status state, and sync pulse-config skill under plugins dir.
node -e '
  const fs = require("fs");
  const path = require("path");
  const agyDir = process.argv[1];
  const pulseDst = process.argv[2];
  const pluginPulseDir = process.argv[3];

  const stalePluginPaths = [
    path.join(pluginPulseDir, "hooks.json"),
    path.join(pluginPulseDir, "mcp_config.json"),
    path.join(pluginPulseDir, "agents"),
    path.join(pluginPulseDir, "commands"),
    path.join(pluginPulseDir, "rules"),
    path.join(pluginPulseDir, "extensions"),
    path.join(pluginPulseDir, "skills", "hud-config"),
  ];
  for (const p of stalePluginPaths) {
    try { fs.rmSync(p, { recursive: true, force: true }); } catch {}
  }

  const legacyHudTargets = [
    path.join(agyDir, "agy-hud.config.json"),
    path.join(agyDir, "bin", "agy-hud.js"),
    path.join(agyDir, "bin", "agy-hud.cmd"),
    path.join(agyDir, "agy-hud-quota-cache.json"),
    path.join(agyDir, "agy-hud-update-status.json"),
    path.join(agyDir, "agy-hud-runtime"),
    path.join(pulseDst, "runtime", "agy-hud.config.json"),
    path.join(pulseDst, "runtime", "bin", "agy-hud.js"),
    path.join(pulseDst, "runtime", "bin", "agy-hud.cmd"),
  ];
  for (const p of legacyHudTargets) {
    try { fs.rmSync(p, { recursive: true, force: true }); } catch {}
  }

  const speedCacheDir = path.join(agyDir, "speed-cache");
  if (fs.existsSync(speedCacheDir)) {
    for (const name of fs.readdirSync(speedCacheDir)) {
      if (!name.endsWith(".json")) continue;
      const fullPath = path.join(speedCacheDir, name);
      try {
        const raw = JSON.parse(fs.readFileSync(fullPath, "utf8"));
        const speed = raw && raw.speed;
        if (speed === null || typeof speed !== "number" || !Number.isFinite(speed) || speed > 5000) {
          fs.rmSync(fullPath, { force: true });
        }
      } catch {
        try { fs.rmSync(fullPath, { force: true }); } catch {}
      }
    }
  }

  const updateStatusPath = path.join(agyDir, "agy-pulse-update-status.json");
  if (fs.existsSync(updateStatusPath)) {
    try {
      const pkg = JSON.parse(fs.readFileSync(path.join(pulseDst, "package.json"), "utf8"));
      const installedVersion = pkg?.version || "0.0.0";
      const status = JSON.parse(fs.readFileSync(updateStatusPath, "utf8"));
      const { isNewerVersion } = require(path.join(pulseDst, "runtime", "update-checker.js"));
      if (!status || typeof status !== "object") {
        fs.rmSync(updateStatusPath, { force: true });
      } else if (!status.latestVersion || !isNewerVersion(status.latestVersion, installedVersion)) {
        status.localVersion = installedVersion;
        status.updateAvailable = false;
        fs.writeFileSync(updateStatusPath, JSON.stringify(status, null, 2) + "\n", "utf8");
      }
    } catch {
      try { fs.rmSync(updateStatusPath, { force: true }); } catch {}
    }
  }
' "$AGY_DIR" "$PULSE_DST" "$PLUGIN_PULSE_DIR"
mkdir -p "$PLUGIN_PULSE_DIR/skills/pulse-config"
cp -f "$REPO_ROOT/plugin.json" "$PLUGIN_PULSE_DIR/plugin.json"
cp -f "$REPO_ROOT/gemini-extension.json" "$PLUGIN_PULSE_DIR/gemini-extension.json"
cp -f "$REPO_ROOT/skills/pulse-config/SKILL.md" "$PLUGIN_PULSE_DIR/skills/pulse-config/SKILL.md"

BIN_DIR="${AGY_PULSE_BIN_DIR:-$HOME/.local/bin}"
mkdir -p "$BIN_DIR"
CLI_PRIMARY_SHIM="$BIN_DIR/agypl"
CLI_ALIAS_SHIM="$BIN_DIR/agy-pulse"
node -e '
  const fs = require("fs");
  const path = require("path");
  const pulseDst = process.argv[1];
  const shimPaths = process.argv.slice(2);
  const entryPath = path.join(pulseDst, "runtime", "bin", "agy-pulse.js");
  const body = [
    "#!/bin/sh",
    "# agy-pulse CLI shim",
    `exec "${process.execPath}" "${entryPath}" "$@"`,
    "",
  ].join("\n");
  for (const shimPath of shimPaths) {
    try { fs.rmSync(shimPath, { force: true }); } catch {}
    fs.writeFileSync(shimPath, body, { mode: 0o755 });
    fs.chmodSync(shimPath, 0o755);
  }
' "$PULSE_DST" "$CLI_PRIMARY_SHIM" "$CLI_ALIAS_SHIM"

echo "Deployed agy-pulse single-process runtime from $REPO_ROOT -> $AGY_DIR"
echo "Registered global CLI commands: $CLI_PRIMARY_SHIM (alias: $CLI_ALIAS_SHIM)"
case ":${PATH:-}:" in
  *":$BIN_DIR:"*) ;;
  *)
    echo "Hint: $BIN_DIR is not in your PATH. Add it to your shell profile:"
    echo "  export PATH=\"$BIN_DIR:\$PATH\""
    ;;
esac
