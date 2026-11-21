# agy-pulse

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)
![Zero Dependencies](https://img.shields.io/badge/Dependencies-Zero-brightgreen.svg)
![Node.js >= 20](https://img.shields.io/badge/Node.js-%3E%3D20-339933.svg)

> **All-in-one single-process status line, real-time context & compaction monitor, permanent telemetry ledger (`stats` / `dashboard`), and cloud quota tracker for Antigravity CLI (`agy`).**

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

Includes a built-in offline Web Dashboard (`agypl dashboard`) and 4 quota layout modes (`table`, `models`, `compact`, `none`) with automatic ASCII fallback on non-UTF-8 terminals; see [Reference Manual](./docs/reference.md#display-modes-displayquotastyle).

## Features

- **Zero-Dependency Single Process**: Pure Node.js (>= 20) runtime with zero external npm dependencies and non-blocking SWR quota caching.
- **Context & Server Compaction Monitor**: Native SQLite Protobuf compaction-limit decoding, turn-by-turn cache efficiency, and `Compaction imminent` alerts at 85%+.
- **Cloud Quota, Cost & Streaming Speed**: Tracks provider/model quotas, unified session pricing, weighted `tok/s` + TTFT latency, and active `/swarm` subagent speeds.
- **Permanent Ledger & One-Click Dashboard**: Built-in `node:sqlite` ledger merges Antigravity CLI & IDE telemetry; open a standalone offline Web Dashboard without a terminal via `agypl dashboard open`.
- **Privacy-First & Cross-Platform**: All telemetry stays local (`127.0.0.1` loopback, read-only DB scans); works natively on macOS, Linux, and Windows.

## Quick Start

Run in a normal shell (not inside an active `agy` session, since `agy` overwrites `settings.json` on exit):

```bash
# macOS / Linux remote one-command install
curl -fsSL https://raw.githubusercontent.com/hardworkDAD/agy-pulse/main/scripts/install.sh | bash
```

```powershell
# Windows PowerShell remote one-command install
irm https://raw.githubusercontent.com/hardworkDAD/agy-pulse/main/scripts/install.ps1 | iex
```

From a local checkout, run `bash scripts/deploy-local.sh` (or `bash scripts/install.sh` / `.\scripts\install.ps1`). For custom forks or mirrors (`AGY_PULSE_REPO`), see [Reference Manual](./docs/reference.md#installation-details--custom-forks).

Launch the offline Web Dashboard without keeping a terminal open:

```bash
agypl dashboard install-shortcut --desktop   # register OS app launcher & optional Desktop icon
agypl dashboard open                         # start background server & open standalone window
```

## Commands

Both `agypl` and `agy-pulse` expose identical subcommands:

| Command | Aliases | Description |
|---|---|---|
| `agypl dashboard [action] [options]` | `dash`, `ui`, `--dashboard` | Manage offline Web dashboard (`127.0.0.1:3789`, standalone app & shortcut) |
| `agypl stats [range] [options]` | `report`, `--stats` | Print CLI & Antigravity IDE telemetry summary in terminal |
| `agypl config` | `--config` | Launch interactive terminal configuration wizard |
| `agypl diagnose` | `--diagnose` | Output JSON authentication & quota cache diagnostics |
| `agypl update` | `--update` | Download and install latest runtime from configured repository |
| `agypl help` | `--help`, `-h` | Show CLI usage and version |
| `agy-pulse` *(stdin)* | — | Read `agy` JSON from `stdin` (1.5s timeout) and render status line |

See [Reference Manual](./docs/reference.md#cli-commands--options) for all 16 `stats` flags, 14 `dashboard` actions/options, and installer flags.

## Configuration

Run `agypl config` for the interactive wizard, or create `./agy-pulse.config.json` (workspace) / edit `~/.gemini/antigravity-cli/agy-pulse-runtime/runtime/agy-pulse.config.json` (global):

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

See [Reference Manual](./docs/reference.md#configuration-reference) for all configuration fields and 24 environment variables.

## Uninstall

```bash
bash uninstall.sh        # macOS / Linux
```

```powershell
.\uninstall.ps1          # Windows PowerShell
```

Your permanent telemetry ledger (`agy-pulse-ledger.sqlite` / `.json`) is **preserved by default**; pass `--purge-ledger` to `node runtime/uninstall.js` to remove it as well.

## Documentation

- [Reference Manual (English)](./docs/reference.md) · [参考手册 (中文)](./docs/reference_zh.md) — Full CLI options, config fields, env vars & directory tree
- [Architecture (English)](./docs/architecture.md) · [系统架构 (中文)](./docs/architecture_zh.md) — Two-layer design, data flow & module dependencies
- [Module Reference](./docs/module-reference.md) — Module APIs, signatures & behavioral contracts
- [Changelog](./docs/logs/CHANGELOG.md) · [Docs Index](./docs/README.md) — Release history & maintainer index

## References & Acknowledgments

| Project | Repository | Referenced Concepts |
|---|---|---|
| `zzusec/agy-statusline` | https://github.com/zzusec/agy-statusline | Real-time context window progress bar, Protobuf server compaction ceiling decoding, cost monitoring, uppercase `K`/`M` formatting |
| `icebear0828/agy-hud` | https://github.com/icebear0828/agy-hud | Multi-line Antigravity CLI HUD layout, cloud quota panel, session telemetry concepts |
| `elviscjn-cjn/antigravity-context-viewer` | https://github.com/elviscjn-cjn/antigravity-context-viewer | `gen_metadata` Protobuf `timeToFirstToken` / `streamingDuration` decoding, per-model streaming speed stats, model pricing alignment |

All copyrights and licenses of the referenced projects above belong to their respective original authors. `agy-pulse` is open-source software licensed under the MIT License; see [NOTICE](./NOTICE) for full attribution details.

## Contributing & License

Run the test suite with `npm test`. Issues and pull requests are welcome — please see [CONTRIBUTING.md](./CONTRIBUTING.md), [CODE_OF_CONDUCT.md](./CODE_OF_CONDUCT.md), and [SECURITY.md](./SECURITY.md).

Licensed under the [MIT License](./LICENSE). See [NOTICE](./NOTICE) for third-party attribution and reference details.
