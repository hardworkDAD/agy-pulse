# agy-pulse

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)
![Rust Native](https://img.shields.io/badge/Runtime-Native%20Rust-orange.svg)
![Zero Runtime Dependencies](https://img.shields.io/badge/Dependencies-Zero%20Runtime-brightgreen.svg)

> **Native single-binary Rust statusline, real-time context & compaction monitor, unified SQLite WAL telemetry ledger (`stats` / `dashboard`), and host-stdin quota display for Antigravity CLI (`agy`).**

[简体中文](./README_zh.md) · [Reference Manual](./docs/reference.md)

## Preview

```text
my-project │ Google AI Pro │ plan │ out: 42.6 tok/s (ttft 0.8s)
  ───────────────────────────────────────────────────────────────────
  Google 5h   [█████░]  87% ~22m   │ Claude 5h   [██████] 100% ~4h59m
  Google week [████░░]  74% ~4d3h  │ Claude week [██████] 100% ~6d23h
⟳ Session Cache · Carbon 82% (hit 160K · write 4K / in 196K)
◈ Gemini 4 Carbon (High) · Ctx ████▓······· 38.4% (196.6K/512K) · Compacted×1(peak 480K) · Cache 82% (160K) · Out 12.4K · Cost $0.12
```

Includes a built-in local Web Dashboard (`agypl dashboard`) and 3 provider quota layout modes (`table`, `compact`, `none`) with automatic ASCII fallback on non-UTF-8 terminals; see [Reference Manual](./docs/reference.md#display-modes-displayquotastyle).

## Features

- **Native Single-Binary Rust (~4 ms Cold Start, Zero Node.js)**: Compiled into a standalone native binary (`agy-pulse` / `agypl`) with bundled SQLite — zero Node.js, Python, or external runtime dependencies, and a strict 1.5 s stdin timeout guard.
- **Strict Compliance (Zero Off-Host Antigravity Requests)**: Never extracts OAuth tokens, system keychains, or credentials, and never sends off-host requests to Antigravity or model provider backends. All **Quota** and tier data is consumed directly from the host's `stdin` JSON payload.
- **Context & Server Compaction Monitor**: Lock-free read-only SQLite + Protobuf decoding of server **Compaction** ceilings (`?immutable=1` / WAL temp-copy), `CHECKPOINT` peak retention, per-model cumulative cache hit breakdowns, and `Compaction imminent` alerts at 85%+.
- **Unified Pricing & Cascade Accounting**: Single authoritative billing formula (including system prompt `f4.f1` tokens, cache tiers, and >200k long-context rates) shared identically across the **Statusline** and **Dashboard**, aggregating full **Cascade** trees (main **Conversation** + subagent descendants).
- **Unified SQLite WAL Ledger & Persistent Keepalive Web Dashboard**: Bundled SQLite WAL **Ledger** (`agy-pulse-ledger.sqlite`) merges Antigravity CLI and IDE telemetry; `agypl dashboard open` launches a standalone `127.0.0.1` Web **Dashboard** that stays online across browser tab hiding/minimization and exits cleanly only after all open pages close.
- **Multi-Device Object Sync (Zero Conflict, Privacy-First)**: Aggregates telemetry across machines by **`Account` → `Device` → `Source` (`cli` / `ide`) → `Cascade`** and shares each **Account**'s latest **Quota Snapshot** across devices. Uploads only sanitized metadata and monthly compressed chunks — **zero conversation text, zero local `workspace` paths, and zero Git branch names**. The **Statusline** hot path stays strictly local-only and triggers throttled background pushes (`--sync-bg`), while the **Dashboard** refreshes asynchronously in the background.

## Multi-Device Sync Remotes

Supported **Sync Remote** backends (one active at a time, configured in global `0600` `~/.gemini/antigravity-cli/agy-pulse-sync.json`):

- **WebDAV** (`webdav`) — Nextcloud, Jianguoyun, Alist, Nginx/Caddy WebDAV
- **S3-Compatible Object Storage** (`s3`) — Cloudflare R2, AWS S3, Backblaze B2, MinIO, Aliyun OSS (pure-Rust SigV4, zero OpenSSL)
- **Cloudflare Worker** (`cf-worker`) — R2-first with free-tier KV fallback; see [`deploy/cloudflare-worker/README.md`](./deploy/cloudflare-worker/README.md)
- **Local Sync Directory** (`local-dir`) — Syncthing, iCloud Drive, Dropbox, OneDrive

Quick start:

```bash
agy-pulse sync init                   # interactive setup & connectivity probe
agy-pulse sync                        # manual pull + push
agy-pulse account assign alice@example.com   # assign local unassigned records
```

## Quick Start

Run in a normal shell (not inside an active `agy` session, since `agy` overwrites `settings.json` on exit):

```bash
# macOS / Linux prebuilt binary install (x86_64 & aarch64)
curl -fsSL https://raw.githubusercontent.com/hardworkDAD/agy-pulse/main/scripts/install.sh | bash
```

```powershell
# Windows x64 PowerShell prebuilt binary install
irm https://raw.githubusercontent.com/hardworkDAD/agy-pulse/main/scripts/install.ps1 | iex
```

Or build from source with Cargo (`cargo install --git https://github.com/hardworkDAD/agy-pulse && agy-pulse install`), or from a local checkout:

```bash
bash scripts/deploy-local.sh
```

Official prebuilt archives are published for 5 platforms (`linux-x86_64` musl static, `linux-aarch64` musl static, `macos-x86_64`, `macos-aarch64`, `windows-x86_64`) with `SHA256SUMS` verification. For custom forks or mirrors (`AGY_PULSE_REPO` / `AGY_PULSE_RELEASE_BASE_URL`), see [Reference Manual](./docs/reference.md#installation-details--custom-forks).

Launch the offline Web **Dashboard** without keeping a terminal open:

```bash
agypl dashboard install-shortcut --desktop   # register OS app launcher & optional Desktop icon
agypl dashboard open                         # start background server & open standalone window
```

## Commands

Both `agypl` and `agy-pulse` expose identical subcommands:

| Command | Aliases | Description |
|---|---|---|
| `agypl dashboard [action] [options]` | `dash`, `ui`, `--dashboard` | Manage offline local Web Dashboard (`127.0.0.1:3789`, standalone window & shortcut) |
| `agypl stats [range] [options]` | `report`, `--stats` | Print CLI & Antigravity IDE telemetry summary in terminal |
| `agypl sync [init\|status\|prune-device]` | — | Synchronize multi-device **Ledger** chunks and **Quota Snapshots** with active **Sync Remote** |
| `agypl account assign <email> [options]` | — | Reassign local `unassigned` **Conversations** and steps to `<email>` |
| `agypl config` | `--config` | Launch interactive terminal configuration wizard with live Statusline preview |
| `agypl install [--bin-dir <dir>]` | `--install` | Deploy `agy-pulse` & `agypl` binaries and register `statusLine` in host `settings.json` |
| `agypl uninstall [--purge-ledger]` | `--uninstall` | Safely remove `statusLine`, binaries, shortcuts & caches (preserves Ledger by default) |
| `agypl update [--force]` | `--update` | Check GitHub Releases, verify `SHA256SUMS`, and atomically self-update |
| `agypl help` | `--help`, `-h` | Show CLI usage and version (`--version`, `-V`) |
| `agy-pulse` *(stdin)* | — | Read `agy` JSON from `stdin` (1.5 s timeout) and render Statusline |

See [Reference Manual](./docs/reference.md#cli-commands--options) for all `stats` flags, `dashboard` actions/options, and installer flags.

## Configuration

Run `agypl config` for the interactive wizard, create `./agy-pulse.config.json` (workspace), or edit `~/.gemini/antigravity-cli/agy-pulse-runtime/agy-pulse.config.json` (global):

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
    "showCost": false,
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

See [Reference Manual](./docs/reference.md#configuration-reference) for all configuration fields (`display.dashboardPort`, `display.dashboardIdleTimeoutSec`, etc.) and environment variables (`AGY_PULSE_DASHBOARD_PORT`, `AGY_PULSE_DASHBOARD_IDLE_SEC`, `AGY_PULSE_NO_DASHBOARD_AUTOSTART`, etc.).

## Uninstall

```bash
bash uninstall.sh        # macOS / Linux
```

```powershell
.\uninstall.ps1          # Windows PowerShell
```

Or run `agypl uninstall` directly. Your permanent telemetry **Ledger** (`agy-pulse-ledger.sqlite`) is **preserved by default**; pass `--purge-ledger` to remove it as well.

## Documentation

- [Reference Manual (English)](./docs/reference.md) · [参考手册 (中文)](./docs/reference_zh.md) — Full CLI options, config fields, env vars & directory tree
- [Architecture (English)](./docs/architecture.md) · [系统架构 (中文)](./docs/architecture_zh.md) — Deep-module Rust architecture, data flow & ADRs
- [Module Reference](./docs/module-reference.md) — Module APIs, contracts & behavioral invariants
- [Changelog](./docs/logs/CHANGELOG.md) · [Docs Index](./docs/README.md) — Release history & maintainer index

## References & Acknowledgments

| Project | Repository | Referenced Concepts |
|---|---|---|
| `zzusec/agy-statusline` | https://github.com/zzusec/agy-statusline | Real-time context window progress bar, Protobuf server compaction ceiling decoding, cost monitoring, uppercase `K`/`M` formatting |
| `icebear0828/agy-hud` | https://github.com/icebear0828/agy-hud | Multi-line Antigravity CLI HUD layout, quota panel, session telemetry concepts |
| `elviscjn-cjn/antigravity-context-viewer` | https://github.com/elviscjn-cjn/antigravity-context-viewer | `gen_metadata` Protobuf `timeToFirstToken` / `streamingDuration` decoding, per-model streaming speed stats, model pricing alignment |

All copyrights and licenses of the referenced projects above belong to their respective original authors. `agy-pulse` is open-source software licensed under the MIT License; see [NOTICE](./NOTICE) for full attribution details.

## Contributing & License

Run static checks and tests with:

```bash
cargo clippy --all-targets -- -D warnings
cargo test
```

Issues and pull requests are welcome — please see [CONTRIBUTING.md](./CONTRIBUTING.md), [CODE_OF_CONDUCT.md](./CODE_OF_CONDUCT.md), and [SECURITY.md](./SECURITY.md).

Licensed under the [MIT License](./LICENSE). See [NOTICE](./NOTICE) for third-party attribution and reference details.
