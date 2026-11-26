# agy-pulse Reference Manual

[简体中文](./reference_zh.md) · [Back to README](../README.md) · [Docs Index](./README.md)

Complete technical reference for `agy-pulse` v2.0.0 CLI commands, flags, unified SQLite WAL **Ledger**, local Web **Dashboard**, configuration schema, environment variables, custom forks, local development, and repository layout.

---

## Table of Contents

- [Display Modes (`display.quotaStyle`)](#display-modes-displayquotastyle)
- [Installation Details & Custom Forks](#installation-details--custom-forks)
- [CLI Commands & Options](#cli-commands--options)
  - [Primary Commands](#primary-commands)
  - [`agypl stats` Options](#agypl-stats-options)
  - [`agypl dashboard` Actions & Options](#agypl-dashboard-actions--options)
  - [`agypl sync` Subcommands & Options](#agypl-sync-subcommands--options)
  - [`agypl account` Subcommands & Options](#agypl-account-subcommands--options)
  - [`agypl install` / `uninstall` / `update` Options](#agypl-install--uninstall--update-options)
- [Permanent Telemetry Ledger & Web Dashboard (`stats` / `dashboard`)](#permanent-telemetry-ledger--web-dashboard-stats--dashboard)
  - [Dashboard HTTP API](#dashboard-http-api)
- [Uninstall Details](#uninstall-details)
- [Configuration Reference](#configuration-reference)
  - [Config Paths & Merge Precedence](#config-paths--merge-precedence)
  - [Full Default Configuration Example](#full-default-configuration-example)
  - [Top-Level Configuration Fields](#top-level-configuration-fields)
  - [`display.*` Configuration Fields](#display-configuration-fields)
- [Multi-Device Sync Configuration (`agy-pulse-sync.json`)](#multi-device-sync-configuration-agy-pulse-syncjson)
  - [Top-Level Sync Fields](#top-level-sync-fields)
  - [Sync Remote Backends & Examples](#sync-remote-backends--examples)
- [Environment Variables](#environment-variables)
- [Development & Legacy v1 Migration](#development--legacy-v1-migration)
- [File Structure](#file-structure)

---

## Display Modes (`display.quotaStyle`)

All provider **Quota** data is parsed strictly from the host `stdin` JSON payload (zero off-host Antigravity cloud requests per ADR-0002):

- **`table`** (default): 2-column provider window overview (`Google 5h/week`, `Claude 5h/week`, and optional `Argon`) + bottom session cache & real-time **Compaction** monitor. Missing quota windows render `--` instead of a misleading `100%`. Legacy `v1` `"models"` values smoothly fall back to `"table"`.
- **`compact`**: Single-line provider critical-window percentages (`Google: X% │ Claude: Y% │ Argon: Z%`).
- **`none`**: Disables the quota panel completely.

---

## Installation Details & Custom Forks

Run in a normal shell (**NOT** inside an active `agy` session, since `agy` overwrites `settings.json` from in-memory state on exit):

### What the installer does

1. Detects OS and CPU architecture (`linux-x86_64`, `linux-aarch64`, `macos-x86_64`, `macos-aarch64`, or `windows-x86_64`), downloads the matching prebuilt archive and `SHA256SUMS`, and verifies the SHA-256 checksum.
2. Installs native binaries **`agy-pulse`** and **`agypl`** to `~/.local/bin/` (Unix) or `%LOCALAPPDATA%\agy-pulse\bin\` (Windows; override via `AGY_PULSE_BIN_DIR`).
3. Runs `agy-pulse install` to configure `statusLine.command` in `~/.gemini/antigravity-cli/settings.json` (preserving existing `enabled` / `stack_with_default` flags and backing up corrupt JSON to `.bak`), and cleans stale `v1` plugin artifacts.
4. When `agy` is on `PATH`, runs `agy plugin install` to register static plugin manifests (`plugin.json`, `gemini-extension.json`, `skills/pulse-config/SKILL.md`).

### Prebuilt install (macOS / Linux / Windows)

```bash
curl -fsSL https://raw.githubusercontent.com/hardworkDAD/agy-pulse/main/scripts/install.sh | bash
```

```powershell
irm https://raw.githubusercontent.com/hardworkDAD/agy-pulse/main/scripts/install.ps1 | iex
```

### Source install (`cargo install` or local checkout)

```bash
cargo install --git https://github.com/hardworkDAD/agy-pulse
agy-pulse install
```

Or from a local git clone:

```bash
bash scripts/deploy-local.sh
```

### Custom fork or mirror

Override `AGY_PULSE_REPO` or `AGY_PULSE_RELEASE_BASE_URL` before running the installer:

```bash
export AGY_PULSE_REPO="yourname/agy-pulse"
curl -fsSL "https://raw.githubusercontent.com/$AGY_PULSE_REPO/main/scripts/install.sh" | bash
```

```powershell
$env:AGY_PULSE_REPO = 'yourname/agy-pulse'
irm "https://raw.githubusercontent.com/$env:AGY_PULSE_REPO/main/scripts/install.ps1" | iex
```

---

## CLI Commands & Options

Both `agypl` and `agy-pulse` expose identical subcommands and flags.

### Primary Commands

| Command / Invocation | Aliases | Description |
|---|---|---|
| `agypl stats [range] [options]` | `report`, `--stats` | Print CLI & Antigravity IDE telemetry summary in terminal |
| `agypl dashboard [action] [options]` | `dash`, `ui`, `--dashboard` | Start or manage offline local Web **Dashboard** (`127.0.0.1:3789`, standalone app window & desktop icon) |
| `agypl sync [subcommand] [options]` | — | Synchronize **Ledger** chunks and **Quota Snapshots** with the active **Sync Remote** |
| `agypl account assign <email> [options]` | — | Reassign local `unassigned` **Conversations** and steps to `<email>` |
| `agypl config` | `--config` | Launch interactive terminal configuration wizard with live **Statusline** preview |
| `agypl install [--bin-dir <dir>]` | `--install` | Deploy `agy-pulse` & `agypl` binaries and register `statusLine` in host `settings.json` |
| `agypl uninstall [--purge-ledger]` | `--uninstall` | Safely remove `statusLine`, binaries, shortcuts & caches (preserves **Ledger** by default) |
| `agypl update [--force]` | `--update` | Check GitHub Releases, verify `SHA256SUMS`, and atomically self-update |
| `agypl help` | `--help`, `-h` | Show CLI usage (`--version` / `-V` / `-v` / `version` prints version) |
| `agy-pulse` *(no args)* | — | Read `agy` JSON from `stdin` (1500 ms timeout) and render **Statusline** |

### `agypl stats` Options

| Option | Values / Format | Default | Description |
|---|---|---|---|
| Positional range | `today \| 24h \| 7d \| 30d \| month \| all` | `7d` | Time window shorthand (e.g. `agypl stats today`) |
| `--range <val>` | `today \| 24h \| 7d \| 30d \| month \| all \| custom` | `7d` | Explicit time range |
| `--today`, `--24h`, `--7d`, `--30d`, `--month`, `--all` | flag | — | Shortcut flags for preset ranges |
| `--from <date>` / `--to <date>` | `YYYY-MM-DD` or epoch ms | *unset* | Custom range bounds (auto-selects `custom` if `--range` is omitted) |
| `--granularity <val>` / `--by <val>` | `auto \| hour \| day \| week` | `auto` | Time series aggregation bucket |
| `--source <val>` | `all \| cli \| ide` | `all` | Filter by source (`cli`, `ide`, or both) |
| `--model <val>` | model name or `all` | `all` | Filter by model name/family |
| `--workspace <val>` / `--ws <val>` | path, name, or `all` | `all` | Filter by workspace |
| `--no-subagents` / `--include-subagents` | flag | include | Exclude or include `/swarm` workers & subagent **Conversations** |
| `--format <val>` | `table \| json \| csv` | `table` | Output format |
| `--json` | flag | — | Shorthand for `--format json` |
| `--csv[=kind]` | `timeseries \| models \| sessions` | `timeseries` | Shorthand for `--format csv` with optional export kind |
| `--csv-kind <val>` / `--kind <val>` | `timeseries \| models \| sessions` | `timeseries` | Select CSV export dataset |
| `--lang <val>` | `auto \| en \| zh` | `auto` | Output language |
| `--no-sync` | flag | `false` | Skip read-only DB backfill and query existing **Ledger** directly |
| `-h`, `--help` | flag | — | Show `stats` help |

### `agypl dashboard` Actions & Options

| Action / Option | Values / Format | Default | Description |
|---|---|---|---|
| *(no action)* | foreground server | `serve` | Run local **Dashboard** in foreground (`127.0.0.1:3789`) and open browser |
| `open` | sub-action | — | Reuse or silently start background server and open a standalone app window |
| `stop` | sub-action | — | Stop the running background **Dashboard** instance cleanly |
| `status` | sub-action | — | Show background **Dashboard** status (`url`, `pid`) and desktop shortcut state |
| `install-shortcut` (`install-app`) | sub-action | — | Install a cross-platform desktop/app-launcher shortcut for terminal-free opening |
| `uninstall-shortcut` (`uninstall-app`) | sub-action | — | Remove only `agy-pulse`-owned desktop shortcuts (fingerprint-verified) |
| `--port <num>`, `-p <num>` | `0`–`65535` | `3789` | Bind loopback port (`0` picks random port; auto-falls back on conflict) |
| `--no-open` / `--open` | flag | open | Disable or enable auto-opening the browser/window |
| `--app` / `--browser` | flag | `--app` for `open`, `--browser` for `serve` | Open in a standalone Chromium app window (`--app=<url>`) or default browser |
| `--background` | flag | `false` | Run server in background daemon mode with page-lifecycle auto-exit |
| `--idle-timeout <sec>` | non-negative integer | `600` (`open`/`--background`), `0` (`serve`) | Startup/orphan idle timeout when no page client is connected |
| `--desktop` | flag | `false` | Also place a shortcut on `~/Desktop` during `install-shortcut` |
| `--lang <val>`, `--zh`, `--en` | `auto \| en \| zh` | `auto` | Initial **Dashboard** UI language |
| `--no-sync` | flag | `false` | Skip startup DB backfill and serve existing **Ledger** immediately |

### `agypl sync` Subcommands & Options

| Invocation / Option | Values / Format | Default | Description |
|---|---|---|---|
| `agypl sync` | bidirectional sync | pull then push | Pull remote **Device** chunks/manifests then push local changes |
| `agypl sync --push` | flag | `false` | Push local data to **Sync Remote** only (cannot combine with `--pull`) |
| `agypl sync --pull` | flag | `false` | Pull remote **Device** data into local **Ledger** only (cannot combine with `--push`) |
| `agypl sync --dry-run` | flag | `false` | Preview chunks and manifests without modifying remote or local **Ledger** |
| `agypl sync --no-local-scan` | flag | `false` | Skip scanning local `conversations/*.db` before syncing |
| `agypl sync status` | subcommand | — | Display local `device_id`, `device_name`, last observed email, redacted remote target, push/pull timestamps, errors, and known **Devices** |
| `agypl sync init [options]` | subcommand | interactive | Configure `~/.gemini/antigravity-cli/agy-pulse-sync.json` (`0600`) and run `probe()` unless `--skip-probe` is passed |
| `--type <backend>` | `local-dir \| webdav \| s3 \| cf-worker` | *prompted* | Select **Sync Remote** backend type (`init`) |
| `--path <dir>` | directory path | *prompted* | Local directory path for `local-dir` (`init`) |
| `--url <url>` | HTTPS URL (or loopback HTTP) | *prompted* | Remote endpoint URL for `webdav` or `cf-worker` (`init`) |
| `--endpoint <url>` | HTTPS URL | *prompted* | S3-compatible endpoint URL for `s3` (`init`) |
| `--bucket <name>` | string | *prompted* | S3 bucket name for `s3` (`init`) |
| `--region <region>` | string | `auto` | S3 signing region (`init`) |
| `--username <user>` | string | *unset* | WebDAV Basic Auth username (`init`) |
| `--password <pass>` | string | *unset* | WebDAV Basic Auth password (`init`) |
| `--token <token>` | string | *unset* | Bearer token for `webdav` or `cf-worker` (`init`) |
| `--access-key <ak>` | string | *unset* | S3 access key ID (`init`) |
| `--secret-key <sk>` | string | *unset* | S3 secret access key (`init`) |
| `--path-style <true\|false>` | boolean | `true` | Use path-style S3 URLs (`init`) |
| `--allow-insecure-http` | flag (or `=true\|false`) | `false` | Permit non-loopback `http://` URLs (`init`) |
| `--device-name <name>` | string | OS hostname | Custom display name for this **Device** (`init`) |
| `--default-account <email>` | normalized email | *unset* | Fallback **Account** email for IDE or historical records (`init`) |
| `--prefix <prefix>` | key prefix | `agy-pulse-sync` | Remote object key prefix (`init`) |
| `--push-interval <sec>` | integer `>= 1` | `300` | Background push throttle interval in seconds (`init`) |
| `--skip-probe` | flag | `false` | Save config without running a live `probe()` connectivity check (`init`) |
| `agypl sync prune-device <device_id>` | subcommand | — | Delete remote objects and local cached rows for `<device_id>` |
| `--force` | flag | `false` | Allow pruning the current local **Device** (`prune-device`) |
| `--dry-run` | flag | `false` | List objects that would be deleted without modifying remote or local **Ledger** (`prune-device`) |

### `agypl account` Subcommands & Options

| Invocation / Option | Values / Format | Default | Description |
|---|---|---|---|
| `agypl account assign <email>` | subcommand | — | Reassign local `unassigned` **Conversations** and steps to normalized `<email>` (never modifies already-assigned or remote records) |
| `--all-unassigned` | flag | default | Assign all local `unassigned` **Conversations** (cannot combine with `--workspace`) |
| `--workspace <substr>` | substring | *unset* | Only assign local `unassigned` **Conversations** whose `workspace` contains `<substr>` |
| `--dry-run` | flag | `false` | Preview matching **Conversation** and step counts without modifying SQLite |
| `-h`, `--help` | flag | — | Show `account` or `assign` usage help |

### `agypl install` / `uninstall` / `update` Options

| Command | Flag | Description |
|---|---|---|
| `agypl install` | `--bin-dir <dir>` | Override binary deployment directory (default `~/.local/bin` or `%LOCALAPPDATA%\agy-pulse\bin`) |
| `agypl uninstall` | `--purge-ledger`, `--purge` | Also remove `agy-pulse-ledger.sqlite` (preserved by default) |
| `agypl uninstall` | `--bin-dir <dir>` | Also clean binaries from custom bin directory |
| `agypl update` | `--force` | Re-download and replace binary even if remote version matches local version |
| `bash scripts/deploy-local.sh` | `--offline`, `--bin-dir=<dir>` | Build release binaries locally (`cargo build --release`) and deploy |

---

## Permanent Telemetry Ledger & Web Dashboard (`stats` / `dashboard`)

- **Unified SQLite WAL Ledger (`agy-pulse-ledger.sqlite`)**: Built-in `rusqlite` (`bundled` SQLite with WAL mode and `busy_timeout`) stores all CLI and IDE **Conversation** steps, **Cascade** lineage, and monotonic rolling daily/7-day cost state (`0600` file permissions).
- **Lock-free dual-source scanning**: `agy-pulse` incrementally syncs the active CLI **Conversation** during **Statusline** runs and scans both `~/.gemini/antigravity-cli/conversations/*.db` and `~/.gemini/antigravity/conversations/*.db` read-only (`?immutable=1` or temp-copied active WAL) during `stats` and `dashboard`.
- **Persistent Keepalive Dashboard & Built-in Settings Drawer**: Binds strictly to `127.0.0.1`, validates `Host`/`Origin` + CSP headers, and uses `/api/keepalive` SSE streams + `/api/ping` + `pagehide` `/api/disconnect` beacons so minimized or backgrounded browser tabs stay connected indefinitely and exit only after all pages close. The top-bar **Settings** drawer lets you visually edit global **Pulse Config** (with live Rust **Statusline** preview and all advanced fields) and **Sync Config** (supporting all 4 **Sync Remote** backends, write-only credentials, live connection tests, and config removal), sharing the same underlying config files as `agypl config` and `agypl sync init`.

### Dashboard HTTP API

All endpoints validate `Host` (and `Origin` on `POST` routes) to restrict access to loopback browser clients:

- `GET /api/overview`: Query **Ledger** overview JSON. Supports `range`, `from`, `to`, `granularity`, `source`, `model`, `workspace`, `includeSubagents`, plus multi-device parameters:
  - `account`: `all` (default) | `<email>` | `unassigned`
  - `device`: `all` (default) | `<device_id>` | `local`
  - Response includes `meta.account`, `meta.device`, `meta.localDeviceId`, `meta.availableAccounts`, `meta.availableDevices`, `byAccount`, `byDevice`, `accountQuotas`, `syncStatus`, and `sessions[].accountId` / `deviceId` / `deviceName`.
- `GET /api/export.json` & `GET /api/export.csv`: Accept the same `account` and `device` filter parameters alongside existing range/source/model/workspace filters (`export.csv` also accepts `kind=timeseries|models|sessions`).
- `POST /api/sync-remote`: Non-blocking remote synchronization trigger (runs `pull` + `push` against the configured **Sync Remote** with a 30 s server-side throttle). Pass `?force=1` (or `?force=true`) or JSON body `{"force": true}` to bypass the throttle. Returns `{ ok, enabled, changed, throttled, inProgress, pulledChunks, pushedChunks, stepsImported, error?, syncStatus }`.

#### Local Configuration Endpoints (`/api/config/*`)

Security boundaries enforced on **every** `/api/config/*` route:
- **Loopback + Process CSRF Token**: Bound strictly to `127.0.0.1`; `GET /` injects a 32-byte (64-hex) process-scoped CSRF token into the embedded page, and every `/api/config/*` request must supply it in `X-Agy-Pulse-Token` (constant-time verification; `403 Forbidden` otherwise).
- **Strict Same-Origin & JSON Content-Type**: Rejects non-`same-origin` `Sec-Fetch-Site` or non-loopback `Origin` headers (`403 Forbidden`), and requires `Content-Type: application/json` on `PUT`/`POST`/`DELETE` requests (`400 Bad Request` otherwise).
- **Write-Only Credentials & Optimistic Concurrency**: Sync secrets are never echoed in responses and environment overrides are never written back to disk; `PUT`/`DELETE` requests require `expectedVersion` (`sha256` hex or `"absent"`) checked under per-file mutex locks (`409 Conflict` with `currentVersion` on mismatch).

Routes:
- `GET /api/config/pulse`: Read raw global `agy-pulse.config.json` (never reads workspace files or persists env overrides), `version`, `defaults`, `themePresets`, `allowedColors`, `envOverrides` metadata, and active `runtime` port/idle timeout.
- `PUT /api/config/pulse`: Validate and atomically write global **Pulse Config** while deep-merging and preserving unknown keys (such as `columnWidth` or user custom keys) and removing cleared optional fields. Returns updated `version`, `config`, `envOverrides`, `restartRequired`, and `resolvedLanguage`.
- `POST /api/config/pulse/preview`: Render live ANSI **Statusline** preview for a draft `config` (and optional `terminalCols`) directly via the Rust `render_statusline` engine without writing to disk.
- `GET /api/config/sync`: Read raw global `0600` `agy-pulse-sync.json` mapped to camelCase DTO (`localDeviceId`, `envOverrides` var names, and `secrets.*` `{ configured, overriddenByEnv }` metadata — zero plaintext secrets).
- `PUT /api/config/sync`: Validate and atomically write `0600` `agy-pulse-sync.json` using write-only credential directives (`{ "mode": "keep" | "set" | "clear" }`; `"keep"` inherits from disk only when remote type is unchanged), and immediately refresh `DashboardServer` runtime identity hints (`defaultAccount` / `deviceName`).
- `POST /api/config/sync/test`: Probe a draft **Sync Remote** (`local-dir`, `webdav`, `s3`, `cf-worker`) in memory without writing to disk (merging `"keep"` disk secrets and active env overrides); any error message is sanitized via `redact_sync_error_message`.
- `DELETE /api/config/sync`: Remove `agy-pulse-sync.json` under `expectedVersion` optimistic locking and clear runtime identity hints without touching `agy-pulse-ledger.sqlite` or remote objects.

---

## Uninstall Details

```bash
bash uninstall.sh        # macOS / Linux
```

```powershell
.\uninstall.ps1          # Windows PowerShell
```

Or run `agypl uninstall` directly. Removes `agy-pulse` & `agypl` binaries, `settings.json` `statusLine` (saving `.bak`), fingerprint-verified desktop shortcuts, caches, logs, and plugin registrations. Your permanent telemetry **Ledger** (`agy-pulse-ledger.sqlite`) is **preserved by default**; pass `--purge-ledger` to remove it too.

---

## Configuration Reference

### Config Paths & Merge Precedence

1. **Environment overrides**: `AGY_PULSE_LANGUAGE`, `AGY_PULSE_DASHBOARD_PORT`, `AGY_PULSE_DASHBOARD_IDLE_SEC`, `AGY_PULSE_FORCE_ASCII` / `AGY_PULSE_FORCE_UNICODE` (displayed as override badges in the **Dashboard** Settings drawer and never persisted back to disk)
2. **Workspace local config** (highest file priority): `./agy-pulse.config.json` (takes runtime precedence over global config for **Statusline**; never modified by the Web **Dashboard**)
3. **Global config**: `~/.gemini/antigravity-cli/agy-pulse-runtime/agy-pulse.config.json` (also discovers legacy `agy-pulse-runtime/runtime/agy-pulse.config.json`, or `$AGY_PULSE_GLOBAL_CONFIG_DIR/agy-pulse.config.json`; edited by both `agypl config` and the Web **Dashboard** Settings drawer)
4. **Built-in defaults** (`DisplayConfig::builtin_defaults()`, lowest priority)

Unknown or retired `v1` keys (`endpoints`, `interestingModels`, `cloudQuota`, `repository`) are silently ignored during deserialization, and `"quotaStyle": "models"` smoothly maps to `"table"`.

### Full Default Configuration Example

```json
{
  "enabled": true,
  "language": "auto",
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
    "showDashboardUrl": true,
    "showModel": false,
    "showTokenBar": false,
    "showContextBar": false,
    "showTurnCacheHitRate": false,
    "showBreadcrumbs": false,
    "showGitBranch": false,
    "showUsername": false,
    "showCost": false,
    "showDailyCost": false,
    "showWeeklyCost": false,
    "showRoutedCost": false,
    "breadcrumbCount": 3,
    "useNerdFonts": false,
    "columnWidth": 40
  },
  "thresholds": {
    "warning": 0.7,
    "critical": 0.9
  }
}
```

### Top-Level Configuration Fields

| Field | Type | Default | Description |
|---|---|---|---|
| `enabled` | `boolean` | `true` | Master switch; when `false`, Statusline exits silently with no output |
| `language` | `"auto" \| "en" \| "zh"` | `"auto"` | UI language (`"auto"` inspects `LC_ALL` / `LC_CTYPE` / `LANG`) |
| `theme` | `object \| string` | `{ primary: "green", secondary: "gray", warning: "yellow", critical: "red" }` | 4-key object, preset (`"Emerald Green"`, `"Ocean Blue"`, `"Cyberpunk Magenta"`, `"Amber Gold"`), or single color name overriding `primary` |
| `thresholds` | `{ warning, critical }` | `{ warning: 0.7, critical: 0.9 }` | Quota warning/critical ratios (`0`–`1`); context monitor uses `65%`/`85%` at defaults or `warning*100%`/`critical*100%` when customized |
| `icons` | `object` | *unset* | Optional icon overrides (`branch`, `plan`, `step`, `task`, `token`, `ctx`, `model`, max 8 chars) |

### `display.*` Configuration Fields

| Field | Type | Default | Area | Description |
|---|---|---|---|---|
| `display.quotaStyle` | `"table" \| "compact" \| "none"` | `"table"` | Quota panel | Provider quota layout (`"models"` falls back to `"table"`) |
| `display.showContextMonitor` | `boolean` | `true` | Bottom bar | Real-time context & **Compaction** monitor line |
| `display.showModelCache` | `boolean` | `true` | Session Cache | Per-model cumulative cache hit breakdown line |
| `display.showCurrentDir` | `boolean` | `true` | Line 1 | Current working directory basename |
| `display.showCycleMode` | `boolean` | `true` | Line 1 | Interactive cycle mode badge (`auto`, `accept-edits`, `plan`, `best-of-n`) |
| `display.showSpeed` | `boolean` | `true` | Line 1 | Main weighted output `tok/s` + TTFT and up to 3 active child model speeds |
| `display.showDashboardUrl` | `boolean` | `true` | Bottom line | Show local Web Dashboard URL (`http://127.0.0.1:3789`) on its own dedicated bottom line, silently auto-starting and keeping the background dashboard alive while `agy` is active |
| `display.dashboardPort` | `number` | `3789` | Dashboard | Preferred loopback port when status line auto-starts the background dashboard (overridden by `AGY_PULSE_DASHBOARD_PORT`) |
| `display.dashboardIdleTimeoutSec` | `number` | `1800` | Dashboard | Idle auto-exit timeout in seconds for the status-line auto-started dashboard (`0` keeps it running without idle exit; overridden by `AGY_PULSE_DASHBOARD_IDLE_SEC`) |
| `display.modelSpeedExpireMinutes` | `number` | `5` | Line 1 | Hide main/child model speed & TTFT after N idle minutes |
| `display.modelSpeedExpireTurns` | `number` | `30` | Line 1 | Hide main/child model speed & TTFT after N idle turns |
| `display.showModel` | `boolean` | `false` | Line 1 | Active model name on Line 1 |
| `display.showTokenBar` | `boolean` | `false` | Line 2 | Token breakdown bar (`in`, `out`, `cache`) |
| `display.showContextBar` | `boolean` | `false` | Line 2 | Classic context progress bar |
| `display.showTurnCacheHitRate` | `boolean` | `false` | Line 2 | Per-turn cache hit rate badge |
| `display.cacheHitThresholds` | `{ excellent?, partial? }` | `{ excellent: 80, partial: 50 }` | Line 2 | Turn cache hit color thresholds |
| `display.showBreadcrumbs` | `boolean` | `false` | Line 3 | Workspace memory file / breadcrumbs |
| `display.breadcrumbCount` | `number` | `3` | Line 3 | Max breadcrumb items (`0, 1, 2, 3, 5`) |
| `display.showGitBranch` | `boolean` | `false` | Line 1 | Current Git branch (hidden outside Git repos) |
| `display.showUsername` | `boolean` | `false` | Line 1 | Host stdin `username` / `email` or custom `display.username` |
| `display.username` | `string` | *unset* | Line 1 | Custom username override when `showUsername` is `true` (overrides host `stdin.username` / `stdin.email` and OS username) |
| `display.showCost` | `boolean` | `false` | Line 1 | Current **Cascade** USD total |
| `display.showDailyCost` | `boolean` | `false` | Line 1 | Today's cumulative USD cost from **Ledger** |
| `display.showWeeklyCost` | `boolean` | `false` | Line 1 | 7-day rolling USD cost from **Ledger** |
| `display.showRoutedCost` | `boolean` | `false` | Cost calc | Trust native routed cost for Bedrock/Vertex models when `true` |
| `display.useNerdFonts` | `boolean` | `false` | Icons | Use Nerd Font glyphs |
| `display.unicode` | `boolean` | *omitted (auto)* | Charset | Omit for auto-detect; `true` forces Unicode; `false` forces ASCII |
| `display.columnWidth` | `number` | `40` | Quota panel | Column width hint |
| `display.terminalWidth` | `number` | *unset (auto)* | Layout | Explicit terminal width cap (`terminal_width > display.terminalWidth > COLUMNS > TTY`) |

---

## Multi-Device Sync Configuration (`agy-pulse-sync.json`)

Multi-device sync configuration is stored exclusively in the global private file `~/.gemini/antigravity-cli/agy-pulse-sync.json` (or `$AGY_PULSE_SYNC_CONFIG`), created atomically with `0700` parent directory and `0600` file permissions on Unix. Manage it via `agypl sync init` or the local Web **Dashboard** top-bar **Settings** drawer (Sync Config tab, with write-only credential inputs, active `AGY_PULSE_*` env override indicators, live connection probe, and safe removal). **It is never read from or written to workspace `agy-pulse.config.json`**, preventing credentials from being committed to Git repositories.

### Top-Level Sync Fields

| Field | Type | Default | Description |
|---|---|---|---|
| `enabled` | `boolean` | `false` | Enable automatic background `--sync-bg` push and **Dashboard** `/api/sync-remote` sync |
| `deviceId` | `string` (optional) | *auto-managed* | Immutable local **Device** ID synchronized with `ledger_meta.local_device_id` |
| `deviceName` | `string` (optional) | OS hostname | Display name shown in `sync status` and **Dashboard** (mutable without splitting history) |
| `defaultAccount` | `string` (optional) | *unset* | Normalized fallback **Account** email when host `stdin.email` is unavailable (e.g. IDE or historical records) |
| `pushIntervalSec` | `number` | `300` | Minimum interval in seconds (`>= 1`) between automatic Statusline `--sync-bg` pushes |
| `prefix` | `string` | `"agy-pulse-sync"` | Remote object key root prefix (`/` trimmed) |
| `remote` | `object` | `{ "type": "local-dir", "path": "" }` | Single active **Sync Remote** backend configuration (see below) |

### Sync Remote Backends & Examples

1. **Local Sync Directory (`local-dir`)** — Syncthing, iCloud Drive, Dropbox, OneDrive:
   - Fields: `type` (`"local-dir"`), `path` (`string`).
   ```json
   {
     "enabled": true,
     "deviceName": "lab-ubuntu",
     "defaultAccount": "alice@example.com",
     "pushIntervalSec": 300,
     "prefix": "agy-pulse-sync",
     "remote": {
       "type": "local-dir",
       "path": "/home/alice/Sync/agy-pulse"
     }
   }
   ```

2. **WebDAV (`webdav`, alias `web-dav`)** — Nextcloud, Jianguoyun, Alist, Caddy/Nginx WebDAV:
   - Fields: `type` (`"webdav"`), `url` (`string`), `username` (`string?`), `password` (`string?`, overridden by `AGY_PULSE_SYNC_PASSWORD`), `token` (`string?`, overridden by `AGY_PULSE_SYNC_TOKEN`), `allow_insecure_http` (`boolean`, default `false`).
   ```json
   {
     "enabled": true,
     "deviceName": "macbook-pro",
     "defaultAccount": "alice@example.com",
     "pushIntervalSec": 300,
     "prefix": "agy-pulse-sync",
     "remote": {
       "type": "webdav",
       "url": "https://dav.example.com/remote.php/dav/files/alice/agy-pulse",
       "username": "alice",
       "password": "",
       "allow_insecure_http": false
     }
   }
   ```

3. **S3-Compatible Object Storage (`s3`)** — Cloudflare R2, AWS S3, Backblaze B2, MinIO, Aliyun OSS:
   - Fields: `type` (`"s3"`), `endpoint` (`string`), `region` (`string`, default `"auto"`), `bucket` (`string`), `access_key` (`string?`, overridden by `AGY_PULSE_S3_ACCESS_KEY`), `secret_key` (`string?`, overridden by `AGY_PULSE_S3_SECRET_KEY`), `path_style` (`boolean`, default `true`), `allow_insecure_http` (`boolean`, default `false`).
   ```json
   {
     "enabled": true,
     "deviceName": "workstation",
     "defaultAccount": "alice@example.com",
     "pushIntervalSec": 300,
     "prefix": "agy-pulse-sync",
     "remote": {
       "type": "s3",
       "endpoint": "https://<account-id>.r2.cloudflarestorage.com",
       "region": "auto",
       "bucket": "agy-pulse-sync",
       "access_key": "",
       "secret_key": "",
       "path_style": true,
       "allow_insecure_http": false
     }
   }
   ```

4. **Cloudflare Worker (`cf-worker`)** — R2-first with free-tier KV fallback (see [`deploy/cloudflare-worker/README.md`](../deploy/cloudflare-worker/README.md)):
   - Fields: `type` (`"cf-worker"`), `url` (`string`), `token` (`string?`, overridden by `AGY_PULSE_SYNC_TOKEN`), `allow_insecure_http` (`boolean`, default `false`).
   ```json
   {
     "enabled": true,
     "deviceName": "laptop",
     "defaultAccount": "alice@example.com",
     "pushIntervalSec": 300,
     "prefix": "agy-pulse-sync",
     "remote": {
       "type": "cf-worker",
       "url": "https://agy-pulse-sync.example.workers.dev",
       "token": "",
       "allow_insecure_http": false
     }
   }
   ```

---

## Environment Variables

| Variable | Purpose |
|---|---|
| `AGY_PULSE_DATA_DIR` | Override Antigravity CLI data directory (`~/.gemini/antigravity-cli` by default) |
| `AGY_PULSE_IDE_DATA_DIR` | Override Antigravity IDE data directory (`~/.gemini/antigravity`; empty when `AGY_PULSE_DATA_DIR` is set alone for sandbox isolation) |
| `AGY_PULSE_GLOBAL_CONFIG_DIR` | Override global `agy-pulse.config.json` directory |
| `AGY_PULSE_BIN_DIR` | Override binary install directory (`~/.local/bin` on Unix, `%LOCALAPPDATA%\agy-pulse\bin` on Windows) |
| `AGY_PULSE_REPO` | Override `owner/repo` slug for install and `agypl update` (default `hardworkDAD/agy-pulse`) |
| `AGY_PULSE_REPO_URL` | Custom Git clone URL for `agy plugin install` in `install.sh` / `install.ps1` |
| `AGY_PULSE_RELEASE_BASE_URL` | Override GitHub Release download base URL (mirrors or local `http://127.0.0.1` test server) |
| `AGY_PULSE_UPDATE_URL` | Override update check metadata JSON URL |
| `AGY_PULSE_DISABLE_UPDATE_CHECK` | Set `1` to disable 24h background update checks on Statusline runs |
| `AGY_PULSE_FORCE_ASCII` | Set `1` to force ASCII fallback |
| `AGY_PULSE_FORCE_UNICODE` | Set `1` to force Unicode output |
| `AGY_PULSE_LANGUAGE` | Force UI language (`en`, `zh`, `auto`) |
| `AGY_PULSE_NO_OPEN` | Set `1` to prevent `agypl dashboard` from opening a browser |
| `AGY_PULSE_NO_DASHBOARD_AUTOSTART` | Set `1` to prevent Statusline rendering from auto-starting the background dashboard (still displays URL if already running) |
| `AGY_PULSE_DASHBOARD_PORT` | Override preferred loopback port for status-line dashboard auto-start (default `3789`) |
| `AGY_PULSE_DASHBOARD_IDLE_SEC` | Override idle auto-exit timeout in seconds for `agypl dashboard open` / `--background` (default `600`) and status-line auto-start (default `1800`; `0` disables idle exit) |
| `AGY_PULSE_DASHBOARD_CLOSE_GRACE_MS` | Override close-grace window in milliseconds after the last page disconnects |
| `AGY_PULSE_SYNC_CONFIG` | Override path to private `agy-pulse-sync.json` (never reads workspace directories) |
| `AGY_PULSE_SYNC_TOKEN` | Override Bearer token for `webdav` / `cf-worker` sync remotes without storing in JSON |
| `AGY_PULSE_SYNC_PASSWORD` | Override Basic Auth password for `webdav` sync remotes without storing in JSON |
| `AGY_PULSE_S3_ACCESS_KEY` | Override `s3` access key ID without storing in JSON |
| `AGY_PULSE_S3_SECRET_KEY` | Override `s3` secret access key without storing in JSON |
| `AGY_PULSE_NO_SYNC_AUTOSTART` | Set `1` / `true` / `yes` to disable automatic background `--sync-bg` push from Statusline runs |
| `AGY_PULSE_PROFILE_PATH` | Override PowerShell profile path for `scripts/configure-utf8.ps1` |
| `COLUMNS` | Fallback terminal column width |
| `NO_COLOR` | Disable ANSI colors in `agypl stats` output |
| `LC_ALL` / `LC_CTYPE` / `LANG` | Auto-detect terminal UTF-8 support and `zh`/`en` language |

---

## Development & Legacy v1 Migration

Upgrading from `v1.0.0` (Node.js) automatically preserves your saved `agy-pulse.config.json` and replaces `settings.json` `statusLine.command` with the native `agy-pulse` binary. Historical usage is rebuilt directly by scanning CLI and IDE `conversations/*.db` archives into `agy-pulse-ledger.sqlite`.

```bash
cargo clippy --all-targets -- -D warnings
cargo test
bash scripts/deploy-local.sh
```

---

## File Structure

```
agy-pulse/
├── Cargo.toml                         # Rust Edition 2024 crate & bundled dependencies
├── Cargo.lock                         # Locked dependency graph
├── GLOSSARY.md                        # Canonical domain terminology
├── LICENSE                            # Standard MIT License
├── NOTICE                             # Open-source attribution & reference notices
├── plugin.json                        # {"name":"agy-pulse"}
├── gemini-extension.json              # agy plugin manifest (v2.0.0)
├── release.sh                         # Official local & tag release helper
├── uninstall.sh / uninstall.ps1       # Zero-Node cross-platform uninstallers
├── .github/workflows/
│   ├── ci.yml                         # 3-OS Clippy + release build + full test suite
│   └── release.yml                    # 5-target prebuilt archives + unified SHA256SUMS
├── src/
│   ├── lib.rs                         # #![deny(clippy::unwrap_used, clippy::expect_used)] + run_cli()
│   ├── main.rs                        # Primary `agy-pulse` binary entrypoint
│   ├── bin/agypl.rs                   # Short `agypl` binary entrypoint
│   ├── domain/                        # Pure in-memory models, pricing, accounting, quota
│   ├── ingest/                        # HostReader: stdin, transcript, lock-free .db, protobuf, workspace
│   ├── ledger/                        # Unified SQLite WAL Ledger, sync, query & `stats`
│   ├── render/                        # Pure in-memory layout, context bar, quota table, width, i18n
│   ├── config/                        # Deep-merge config loader & interactive raw-TTY wizard
│   ├── dashboard/                     # 127.0.0.1 HTTP server, SSE keepalive, launcher & embedded SPA
│   ├── sync/                          # Multi-device object sync engine, 4 remotes & `sync` CLI
│   ├── ops/                           # Native installer, uninstaller, `account assign` & updater
│   └── platform/                      # Unix / Windows OS abstraction boundary
├── deploy/
│   └── cloudflare-worker/             # Zero-dependency Cloudflare Worker (R2 / KV) sync template
├── scripts/
│   ├── install.sh / install.ps1       # Prebuilt release archive installers with SHA-256 check
│   ├── deploy-local.sh                # Local Cargo release build + deploy + smoke check
│   └── configure-utf8.ps1             # Windows UTF-8 PowerShell & Git helper
├── skills/pulse-config/SKILL.md       # Agent config skill
├── docs/                              # Architecture, ADRs, module & reference docs (en / zh)
└── tests/
    └── t01..t15_*.rs                  # Black-box integration, parity, sync & web config test suites
```
