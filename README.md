# agy-pulse

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)

> **All-in-one single-process status line, real-time context & compaction monitor, permanent telemetry ledger (`stats` / `dashboard`), and cloud quota tracker for Antigravity CLI (`agy`).**
>
> Zero external npm dependencies · Pure Node.js runtime · Built-in `node:sqlite` permanent ledger · Local Web Dashboard & CLI reports · Native SQLite Protobuf compaction limit decoding · Incremental transcript scanning · Bilingual (`en` / `zh`)

[简体中文](./README_zh.md)

---

## What it looks like

`agy-pulse` combines a multi-line HUD, cloud quota panel, per-model session cache breakdown, and a real-time context & compaction monitor into a single fast Node.js process:

```text
my-project │ Google AI Pro │ plan │ out: 42.6 tok/s (ttft 0.8s)
  ───────────────────────────────────────────────────────────────────
  Google 5h   [█████░]  87% ~22m   │ Claude 5h   [██████] 100% ~4h59m
  Google week [████░░]  74% ~4d3h  │ Claude week [██████] 100% ~6d23h
⟳ Session Cache · Carbon 82% (hit 160K · write 4K / in 196K)
◈ Gemini 4 Carbon (High) · Ctx ████▓······· 38.4% (196.6K/512K) · Compacted×1(peak 480K) · Cache 82% (160K) · Out 12.4K · Cost $0.12
```

When context usage reaches **85%+** (by default), the bottom bar switches to red and appends **`Compaction imminent`**. On non-UTF-8 terminals (e.g. Windows `cp936`), it automatically degrades cleanly to ASCII (`*`, `#`/`=`/`-`/`.`, `x`, `|`).

### Display modes (`display.quotaStyle`)
- **`table`** (default): 2-column provider window overview (`Google 5h/week`, `Claude 5h/week`, and optional `Argon`) + bottom session cache & real-time context monitor. Missing quota windows render `--` instead of a misleading `100%`.
- **`models`**: 2-column per-model quota breakdown with custom `columnWidth` (automatically adapts to narrow terminals and switches to single-column below 56 columns).
- **`compact`**: Provider-grouped mini bars (`Google:`, `Anthropic:`, with OpenAI models grouped under `Anthropic`) and zero family-name collisions across 2026 models (`Carbon`, `Hydro`, `Argon`, `Neon`, `Orch-C`, `Orch-A`, `Orch-N`, etc.).
- **`none`**: Disables the quota panel completely (including loading/unavailable placeholders).

---

## Install

Run in a normal shell (NOT inside an active `agy` session, since `agy` overwrites `settings.json` from in-memory state on exit):

### One-command remote install (`hardworkDAD/agy-pulse`)

**macOS / Linux**:
```bash
curl -fsSL https://raw.githubusercontent.com/hardworkDAD/agy-pulse/main/scripts/install.sh | bash
```

**Windows PowerShell**:
```powershell
irm https://raw.githubusercontent.com/hardworkDAD/agy-pulse/main/scripts/install.ps1 | iex
```

### Local checkout

**macOS / Linux**:
```bash
bash scripts/install.sh
```

**Windows PowerShell**:
```powershell
.\scripts\install.ps1
```

### Custom fork or mirror

Override `AGY_PULSE_REPO` (or `AGY_PULSE_REPO_RAW` / `AGY_PULSE_REPO_URL`) before running the installer:

**macOS / Linux**:
```bash
export AGY_PULSE_REPO="yourname/agy-pulse"
curl -fsSL "https://raw.githubusercontent.com/$AGY_PULSE_REPO/main/scripts/install.sh" | bash
```

**Windows PowerShell**:
```powershell
$env:AGY_PULSE_REPO = 'yourname/agy-pulse'
irm "https://raw.githubusercontent.com/$env:AGY_PULSE_REPO/main/scripts/install.ps1" | iex
```

What the installer does:
1. Stages the declarative plugin (`agy plugin install`).
2. Installs the pure Node.js runtime to `~/.gemini/antigravity-cli/agy-pulse-runtime/` (preserving and deep-merging your saved global `agy-pulse.config.json` across upgrades unless `--reset-config` is passed to `scripts/bootstrap.js`).
3. Configures `statusLine.command` in `~/.gemini/antigravity-cli/settings.json` to invoke `runtime/bin/agy-pulse.js` (or `.cmd` shim on Windows), preserving existing `enabled` / `stack_with_default` flags.
4. Registers global CLI commands **`agypl`** (primary) and **`agy-pulse`** (compatibility alias) in `~/.local/bin/` (or `%LOCALAPPDATA%\agy\bin\` on Windows; override via `AGY_PULSE_BIN_DIR`).

---

## CLI Commands & Options

Both `agypl` and `agy-pulse` expose identical subcommands and flags:

| Command / Invocation | Aliases | Description |
|---|---|---|
| `agypl stats [range] [options]` | `report`, `--stats` | Print CLI & Antigravity IDE telemetry summary in terminal |
| `agypl dashboard [options]` | `dash`, `ui`, `--dashboard` | Start offline local Web dashboard (`127.0.0.1:3789` by default) |
| `agypl config` | `--config` | Launch interactive terminal configuration wizard |
| `agypl diagnose` | `--diagnose` | Output JSON authentication & quota cache diagnostics |
| `agypl update` | `--update` | Download and install latest runtime from configured repository |
| `agypl help` | `--help`, `-h` | Show CLI usage and version |
| `agy-pulse` *(no args)* | — | Read `agy` JSON from `stdin` (1.5s timeout) and render status line |

### `agypl stats` options

| Option | Values / Format | Default | Description |
|---|---|---|---|
| Positional range | `today \| 24h \| 7d \| 30d \| month \| all` | `7d` | Time window shorthand (e.g. `agypl stats today`) |
| `--range <val>` | `today \| 24h \| 7d \| 30d \| month \| all \| custom` | `7d` | Explicit time range |
| `--today`, `--24h`, `--7d`, `--30d`, `--month`, `--all` | flag | — | Shortcut flags for preset ranges |
| `--from <date>` / `--to <date>` | `YYYY-MM-DD` or epoch ms | *unset* | Custom range bounds (auto-selects `custom` if `--range` is omitted) |
| `--granularity <val>` / `--by <val>` | `auto \| hour \| day \| week` | `auto` | Time series aggregation bucket |
| `--source <val>` | `all \| cli \| ide` | `all` | Filter by source (`Antigravity CLI`, `Antigravity IDE`, or both) |
| `--model <val>` | model name or `all` | `all` | Filter by model name/family |
| `--workspace <val>` / `--ws <val>` | path, name, or `all` | `all` | Filter by workspace |
| `--no-subagents` / `--include-subagents` | flag | include | Exclude or include `/swarm` workers & subagent sessions |
| `--format <val>` | `table \| json \| csv` | `table` | Output format |
| `--json` | flag | — | Shorthand for `--format json` |
| `--csv[=kind]` | `timeseries \| models \| sessions` | `timeseries` | Shorthand for `--format csv` with optional export kind |
| `--csv-kind <val>` / `--kind <val>` | `timeseries \| models \| sessions` | `timeseries` | Select CSV export dataset |
| `--lang <val>` | `auto \| en \| zh` | `auto` | Output language |
| `--no-sync` | flag | `false` | Skip read-only DB backfill and query existing ledger directly |
| `-h`, `--help` | flag | — | Show `stats` help |

### `agypl dashboard` options

| Option | Values / Format | Default | Description |
|---|---|---|---|
| `--port <num>`, `-p <num>` | `0`–`65535` | `3789` | Bind loopback port (`0` picks random port; auto-falls back on conflict) |
| `--no-open` / `--open` | flag | open | Disable or enable auto-opening the browser |
| `--lang <val>`, `--zh`, `--en` | `auto \| en \| zh` | `auto` | Initial dashboard UI language |
| `--no-sync` | flag | `false` | Skip startup DB backfill and serve existing ledger immediately |

### Installer / deploy / uninstaller flags

| Script | Flag | Description |
|---|---|---|
| `node scripts/bootstrap.js` | `--reset-config` | Reset saved global `agy-pulse.config.json` to defaults during install/upgrade |
| `node scripts/bootstrap.js` | `--cleanup-self` | Remove temporary `bootstrap.js` copy inside `os.tmpdir()` after execution |
| `node scripts/bootstrap.js` | `--delay-start` | Delay execution by 1s before installing (for self-update handoff) |
| `bash scripts/deploy-local.sh` | `--sync-config` | Overwrite global `agy-pulse.config.json` with repo defaults during local deploy |
| `node runtime/uninstall.js` | `--purge-ledger`, `--purge` | Also delete permanent telemetry ledgers (`agy-pulse-ledger.sqlite` / `.json`) |

### Permanent Telemetry Ledger & Web Dashboard (`stats` / `dashboard`)
- **Zero-daemon collection (CLI + Antigravity IDE)**: While rendering the status line, `agy-pulse` silently performs a 15s-throttled incremental archive for the active CLI session (`agy-pulse-ledger.sqlite` via built-in `node:sqlite`, with automatic `agy-pulse-ledger.json` fallback). Running `stats` or `dashboard` automatically backfills both Antigravity CLI (`~/.gemini/antigravity-cli/conversations/*.db`) and Antigravity IDE (`~/.gemini/antigravity/conversations/*.db`) read-only (`?immutable=1` / WAL-safe temp copy) without touching source databases or affecting the live status bar.
- **Permanent retention**: Recorded steps and session trees stay in your ledger even if old `conversations/*.db` files are pruned later.
- **Offline Local Web Dashboard (`dashboard`)**: Binds strictly to `127.0.0.1` with Host/Origin validation, dark/light theme & `en`/`zh` toggles, `All / CLI / IDE` source filter pills, source cost share summary, preset + custom date range pickers, `hour`/`day`/`week` granularity, model/workspace/subagent filters, 6 KPI cards, interactive native SVG charts (stacked cost bars + cache hit rate & output speed dual-line trend), sortable per-model breakdown, searchable & expandable `/swarm` session tree with `CLI` / `IDE` source badges, and CSV/JSON export.

---

## Uninstall

```bash
bash uninstall.sh
```

Windows PowerShell:
```powershell
.\uninstall.ps1
```

Removes the `agy-pulse` runtime directory, plugin installs, caches, logs, Windows shims, and clears `settings.json` `statusLine` (saving a `.bak` backup). Your permanent telemetry ledger (`agy-pulse-ledger.sqlite` / `agy-pulse-ledger.json`) is **preserved by default**; pass `--purge-ledger` (or `--purge`) to `node runtime/uninstall.js` if you want to remove it too.

---

## Configuration

Optional. Create `agy-pulse.config.json` in your workspace root to override defaults.

### Config paths & merge precedence

1. **Workspace local config** (highest priority): `./agy-pulse.config.json`
2. **Global config**: `~/.gemini/antigravity-cli/agy-pulse-runtime/runtime/agy-pulse.config.json` (or `$AGY_PULSE_GLOBAL_CONFIG_DIR/agy-pulse.config.json`)
3. **Built-in defaults** (`FULL_DEFAULT_CONFIG`, lowest priority)

`runtime/config.js` deep-merges `theme` (unless overridden by a string preset/color), `display`, `display.cacheHitThresholds`, `icons`, and `thresholds`.

```json
{
  "enabled": true,
  "theme": {
    "primary": "green",
    "secondary": "gray",
    "warning": "yellow",
    "critical": "red"
  },
  "display": {
    "quotaStyle": "table",
    "showContextMonitor": true,
    "showModelCache": true,
    "showCurrentDir": true,
    "showCycleMode": true,
    "showSpeed": true,
    "showModel": false,
    "showTokenBar": false,
    "showContextBar": false,
    "showTurnCacheHitRate": false,
    "showImageQuota": false,
    "showBreadcrumbs": false,
    "showGitBranch": false,
    "showUsername": false,
    "showCost": false,
    "showDailyCost": false,
    "showWeeklyCost": false,
    "breadcrumbCount": 3,
    "useNerdFonts": false,
    "columnWidth": 40
  },
  "thresholds": {
    "warning": 0.7,
    "critical": 0.9
  },
  "language": "auto"
}
```

### Top-level configuration fields

| Field | Type | Default | Description |
|---|---|---|---|
| `enabled` | `boolean` | `true` | Master switch; when `false`, status line exits silently with no output |
| `language` | `"auto" \| "en" \| "zh"` | `"auto"` | UI language (`"auto"` inspects `LC_ALL` / `LC_CTYPE` / `LANG`) |
| `repository` | `string \| { url?, slug? }` | `package.json#repository` (`https://github.com/hardworkDAD/agy-pulse`) | GitHub `"owner/repo"` or URL used by `agypl update` and background update checks; falls back to `package.json#repository` when omitted or empty |
| `theme` | `object \| string` | `{ primary: "green", secondary: "gray", warning: "yellow", critical: "red" }` | 4-key object (`green`, `gray`, `yellow`, `red`, `blue`, `magenta`, `cyan`), one of 4 string presets (`"Emerald Green"`, `"Ocean Blue"`, `"Cyberpunk Magenta"`, `"Amber Gold"`), or a single color name string overriding `primary` |
| `thresholds` | `{ warning, critical }` | `{ warning: 0.7, critical: 0.9 }` | Quota warning/critical ratios (`0`–`1`). For the bottom context monitor bar, keeping default `0.7 / 0.9` uses `65%` yellow / `85%` red; setting custom values applies `warning * 100%` / `critical * 100%` to both quota and context monitor |
| `icons` | `object` | *unset* | Optional icon overrides (`branch`, `plan`, `step`, `task`, `token`, `ctx`, `model`), truncated to max 8 chars and shallow-merged |
| `endpoints` | `string[]` | *built-in defaults* | Advanced optional override for cloud quota endpoint URLs (lower priority than `AGY_PULSE_ENDPOINTS`) |
| `interestingModels` | `string[]` | *auto-discovered* | Advanced optional override for model IDs queried in the quota table (lower priority than `AGY_PULSE_INTERESTING_MODELS`) |

### `display.*` configuration fields

> Note: When calling `renderStatusLine()` directly without config, some flags (`showGitBranch`, `showTokenBar`, `showContextBar`, `showBreadcrumbs`, `showModel`) default to `true` via `!== false`, whereas the shipped `FULL_DEFAULT_CONFIG` & `runtime/agy-pulse.config.json` explicitly set them to `false` as shown below.

| Field | Type | Default | Area | Description |
|---|---|---|---|---|
| `display.quotaStyle` | `"table" \| "models" \| "compact" \| "none"` | `"table"` | Quota panel | Quota panel layout (`"none"` hides panel completely) |
| `display.showContextMonitor` | `boolean` | `true` | Bottom bar | Show real-time context window & compaction monitor line |
| `display.showModelCache` | `boolean` | `true` | Session Cache | Show per-model cache hit breakdown line above context monitor (active by default when `showContextMonitor` is `true`) |
| `display.showCurrentDir` | `boolean` | `true` | Line 1 | Show current working directory basename |
| `display.showCycleMode` | `boolean` | `true` | Line 1 | Show interactive `agy` cycle mode badge (`auto`, `accept-edits`, `plan`, `best-of-n`, toggled via `Shift+Tab`) |
| `display.showSpeed` | `boolean` | `true` | Line 1 | Show streaming weighted avg speed (`out: X.X tok/s (ttft Y.Ys)`) across recent 30 non-empty DBs plus up to 3 active child/`/swarm` model speeds |
| `display.modelSpeedExpireMinutes` | `number` | `5` | Line 1 | Hide main/child model speed & TTFT after N idle minutes |
| `display.modelSpeedExpireTurns` | `number` | `30` | Line 1 | Hide main/child model speed & TTFT after N idle turns |
| `display.showModel` | `boolean` | `false` | Line 1 | Show active model name on Line 1 (off by default since bottom bar renders `◈ <Model (Effort)>`) |
| `display.showTokenBar` | `boolean` | `false` | Line 2 | Show classic token breakdown bar |
| `display.showContextBar` | `boolean` | `false` | Line 2 | Show classic context window progress bar |
| `display.showTurnCacheHitRate` | `boolean` | `false` | Line 2 | Show per-turn cache hit rate badge |
| `display.cacheHitThresholds` | `{ excellent?, partial? }` | `{ excellent: 80, partial: 50 }` | Line 2 | Cache hit badge thresholds (`>= excellent` bold green, `>= partial` yellow, `> 0` dim yellow, `0` dim gray; shallow-merged) |
| `display.showImageQuota` | `boolean` | `false` | Line 2 | Show image generation quota bar |
| `display.showBreadcrumbs` | `boolean` | `false` | Line 3 | Show recently touched file breadcrumbs |
| `display.breadcrumbCount` | `number` | `3` | Line 3 | Max breadcrumb files shown (`config` wizard cycles `0, 1, 2, 3, 5`) |
| `display.showGitBranch` | `boolean` | `false` | Line 1 | Show current Git branch (auto-hidden outside Git repositories) |
| `display.showUsername` | `boolean` | `false` | Line 1 | Show resolved login email or custom `display.username` |
| `display.username` | `string` | *unset* | Line 1 | Custom username override when `showUsername` is `true` |
| `display.showCost` | `boolean` | `false` | Line 1 | Show current session USD total (also shown on bottom monitor bar) |
| `display.showDailyCost` | `boolean` | `false` | Line 1 | Show today's USD cost (`daily-cost.json`) |
| `display.showWeeklyCost` | `boolean` | `false` | Line 1 | Show 7-day rolling USD cost |
| `display.showRoutedCost` | `boolean` | `false` | Cost calc | For Bedrock (`anthropic.*` / `us.anthropic.*` / `eu.anthropic.*`) & Vertex (`claude-*@*`) routed models, `false` ignores CLI native cost and estimates from tokens; `true` trusts native routed cost |
| `display.useNerdFonts` | `boolean` | `false` | Icons | Use Nerd Font glyphs |
| `display.unicode` | `boolean` | *omitted (auto)* | Charset | Omit for terminal UTF-8 auto-detection; set `true` to force Unicode or `false` to force ASCII |
| `display.columnWidth` | `number` | `40` | Quota panel | Max column width in `table` / `models` layouts |
| `display.terminalWidth` | `number` | *unset (auto)* | Layout | Explicit column width cap; precedence: `agyData.terminal_width > display.terminalWidth > COLUMNS > TTY` |

---

## Environment Variables

### Paths & Installation
| Variable | Purpose |
|---|---|
| `AGY_PULSE_DATA_DIR` | Override Antigravity CLI data directory (`~/.gemini/antigravity-cli` by default) |
| `AGY_PULSE_IDE_DATA_DIR` | Override Antigravity IDE data directory (`~/.gemini/antigravity` by default) |
| `AGY_PULSE_GLOBAL_CONFIG_DIR` | Override directory containing global `agy-pulse.config.json` (defaults to `runtime/`) |
| `AGY_PULSE_BIN_DIR` | Override global CLI shim directory (`~/.local/bin` on Unix, `%LOCALAPPDATA%\agy\bin` on Windows) |
| `AGY_PULSE_NODE` | Custom Node.js binary path for legacy `statusline-combined.sh` shim |
| `AGY_PULSE_PROFILE_PATH` | Override PowerShell profile path for `scripts/configure-utf8.ps1` |
| `AGY_BIN` | Override `agy` executable path for `scripts/diagnose-auth.js` and `scripts/verify-display.js` |

### Repository, Self-Update & Bootstrap
| Variable | Purpose |
|---|---|
| `AGY_PULSE_REPO` | Override `owner/repo` slug for remote install, bootstrap, and `--update` (default `hardworkDAD/agy-pulse`) |
| `AGY_PULSE_REPO_RAW` | Custom raw GitHub base URL for forks/mirrors |
| `AGY_PULSE_REPO_URL` | Custom Git clone URL for `agy plugin install` |
| `AGY_PULSE_BOOTSTRAP_URL` | Custom `bootstrap.js` URL for `agypl update` |
| `AGY_PULSE_UPDATE_URL` | Custom `package.json` URL for background update checker |
| `AGY_PULSE_SETUP_SOURCE_DIR` | Local source directory override for `scripts/bootstrap.js` |
| `AGY_PULSE_SETUP_SOURCE_BASE` | Fallback raw source base URL for `scripts/bootstrap.js` |

### Quota, Display, Locale & Runtime
| Variable | Purpose |
|---|---|
| `AGY_PULSE_ENDPOINTS` | Comma-separated cloud quota endpoint URLs |
| `AGY_PULSE_INTERESTING_MODELS` | Comma-separated model IDs to include in quota table |
| `AGY_PULSE_FORCE_ASCII` | Set `1` to force ASCII fallback |
| `AGY_PULSE_FORCE_UNICODE` | Set `1` to force Unicode output |
| `AGY_PULSE_NO_OPEN` | Set `1` to prevent `agypl dashboard` from auto-opening a browser |
| `NODE_USE_ENV_PROXY` | Automatically set to `'1'` in background quota refresh subprocess so Node honors `HTTP_PROXY`/`HTTPS_PROXY` (can be overridden) |
| `COLUMNS` | Fallback terminal column width when `agyData.terminal_width` and `display.terminalWidth` are unset |
| `NO_COLOR` | Disable ANSI colors in `agypl stats` output |
| `LC_ALL` / `LC_CTYPE` / `LANG` | Used to auto-detect terminal UTF-8 support and `zh`/`en` language when `language` is `"auto"` |

---

## Development & Legacy Migration

For local runtime development or migrating an older dual-process `statusline-combined.sh` setup to single-process `agy-pulse.js` (preserving existing config unless `--sync-config` is passed):

```bash
bash scripts/deploy-local.sh
```

`deploy-local.sh` updates `~/.gemini/antigravity-cli/agy-pulse-runtime/`, upgrades any legacy `statusline-combined.sh` into a single-process `exec` forwarder, updates `settings.json` (backing up corrupt JSON to `.bak`), cleans stale plugin files, and syncs `skills/pulse-config/SKILL.md`.

---

## File structure

```
agy-pulse/
├── LICENSE                            # MIT License + acknowledgments
├── package.json                       # Package metadata, repository & test/CLI scripts
├── plugin.json                        # {"name":"agy-pulse"}
├── gemini-extension.json              # agy remote-install manifest (v1.0.0)
├── runtime/
│   ├── brand.js                       # Brand constants & repository slug resolver
│   ├── compaction.js                  # Pure-JS Protobuf wire decoder, SQLite compaction limit, 30-DB model perf & session cost tree
│   ├── bin/
│   │   └── agy-pulse.js / .cmd        # statusLine & CLI entrypoint
│   ├── config-wizard.js               # Interactive terminal config wizard (--config)
│   ├── config.js                      # Deep-merging config loader & atomic writer
│   ├── cost.js                        # ACV-aligned model pricing table (>200k tiers) & unified session cost
│   ├── daily-cost.js                  # Lock-safe daily & 7-day rolling cost ledger
│   ├── dashboard.js                   # Local 127.0.0.1 HTTP telemetry dashboard server (--dashboard)
│   ├── dashboard-ui.js                # Zero-CDN bilingual dark/light SVG dashboard SPA
│   ├── encoding.js                    # Terminal UTF-8 / codepage detector
│   ├── ledger.js                      # Permanent node:sqlite/JSON telemetry ledger & read-only DB scanner
│   ├── parser.js                      # Incremental byte-offset transcript scanner & metadata
│   ├── paths.js                       # Cross-platform root & safe executable resolver
│   ├── quota.js                       # SWR quota orchestrator
│   ├── quota/                         # token · cache · cloud · models
│   ├── renderer.js                    # Multi-line status line renderer
│   ├── renderer/                      # context-monitor · format · lang · quota-render
│   ├── speed.js                       # Output tok/s & TTFT formatter with active-turn latching
│   ├── stats.js                       # CLI telemetry table, ASCII trend & CSV/JSON reporter (--stats)
│   ├── statusline-installer.js        # settings.json updater & Windows .cmd/.bat shims
│   ├── uninstall.js                   # Full uninstaller for agy-pulse
│   └── update-checker.js              # Semver-safe background update checker
├── scripts/
│   ├── install.sh / install.ps1       # One-command installers
│   ├── bootstrap.sh / bootstrap.js    # Atomic runtime installer
│   ├── deploy-local.sh                # Local deploy & legacy combined-script migrator
│   ├── configure-utf8.ps1             # Windows UTF-8 helper
│   ├── verify-display.js              # E2E verifier
│   └── diagnose-auth.js               # Auth & token cache diagnostic
├── skills/pulse-config/SKILL.md       # Agent config skill
├── docs/                              # Architecture & module reference (en / zh)
└── tests/unit/                        # Modular unit tests (node --test)
```

---

## Acknowledgments

`agy-pulse` draws inspiration and architectural concepts from:
- [`icebear0828/agy-hud`](https://github.com/icebear0828/agy-hud) — multi-line Antigravity CLI HUD & cloud quota tracking
- [`zzusec/agy-statusline`](https://github.com/zzusec/agy-statusline) — real-time context window, Protobuf compaction ceiling decoding, and cost monitor
- [`elviscjn-cjn/antigravity-context-viewer`](https://github.com/elviscjn-cjn/antigravity-context-viewer) — `gen_metadata` Protobuf `timeToFirstToken` / `streamingDuration` decoding and per-model performance analysis

---

## License

[MIT](./LICENSE)
