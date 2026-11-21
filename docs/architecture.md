# agy-pulse Architecture

> **Version:** 2.0.0  
> **Last updated:** aligned to single-binary native Rust architecture (ADR-0001, ADR-0002, ADR-0003)

## Overview

`agy-pulse` is a single-binary native Rust **Statusline**, **Compaction** monitor, unified **Ledger** CLI (`stats`), and local Web **Dashboard** (`dashboard`) for the [Antigravity CLI](https://antigravity.google) (`agy`).

`agy` invokes the registered `statusLine.command` once per agent step, piping a JSON payload on `stdin`. `agy-pulse` must render ANSI/ASCII output to `stdout` under a **1500 ms** hard timeout and always exit `0`. In release builds on Linux x86_64, cold-start latency is ~4 ms with zero Node.js, Python, or external SQLite CLI dependencies.

---

## Two-Layer Plugin + Native Binary Design

```
┌─────────────────────────────────────────────────────────────────────────┐
│  HOST PLUGIN MANIFESTS (staged by `agy plugin install`)                 │
│   plugin.json · gemini-extension.json (2.0.0) · skills/pulse-config/    │
└────────────────────────────────────┬────────────────────────────────────┘
                                     │  scripts/install.sh (.ps1) / deploy-local.sh
┌────────────────────────────────────▼────────────────────────────────────┐
│  NATIVE BINARIES (~/.local/bin/ or %LOCALAPPDATA%\agy-pulse\bin\)       │
│   agy-pulse (statusLine.command & CLI) · agypl (CLI alias)              │
│   Single Cargo crate (src/lib.rs) with 8 acyclic deep modules:          │
│     domain · render · ingest · ledger · config · dashboard · ops        │
│     + platform (OS isolation)                                           │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Acyclic Deep-Module Dependency Graph

```
src/main.rs / src/bin/agypl.rs  ──> agy_pulse::run_cli()
  ├── ops        (installer, uninstall, GitHub Release updater)
  │     ├── config
  │     ├── dashboard (stop daemon / remove shortcuts on uninstall)
  │     └── platform
  ├── dashboard  (127.0.0.1 HTTP server, SSE keepalive, launcher, embedded SPA)
  │     ├── ledger
  │     ├── ingest
  │     ├── config
  │     └── platform
  ├── config     (deep-merged config, atomic 0600 save, interactive raw-TTY wizard)
  │     ├── render
  │     └── platform
  ├── ledger     (bundled SQLite WAL ledger, CLI/IDE backfill, stats CLI, CSV/JSON)
  │     ├── ingest
  │     ├── domain
  │     └── platform
  ├── ingest     (HostReader: stdin, incremental transcript, lock-free .db + Protobuf)
  │     ├── domain
  │     └── platform
  ├── render     (100% pure in-memory Statusline layout, quota table, width & i18n)
  │     └── domain
  └── domain     (100% pure in-memory types, unified pricing/accounting, quota windows)
```

---

## Key Architectural Guarantees in v2.0.0

1. **Zero Off-Host Antigravity Requests (ADR-0002)**:
   - All cloud quota HTTP clients, keychain/keyring/Credential Manager extractors, `oauth_creds.json` / `google_accounts.json` readers, and `diagnose` subcommands have been physically removed.
   - Provider **Quota** and tier displays consume only the `stdin` JSON payload supplied by `agy`. Only `ops::updater` performs outbound HTTPS requests (strictly to GitHub Releases with `https -> http` redirect downgrade rejection and `SHA256SUMS` verification).
2. **Pure In-Memory Kernel (`domain` & `render`)**:
   - `src/domain/` and `src/render/` perform zero filesystem I/O, zero network calls, zero subprocesses, and zero implicit clock reads; timestamps (`observed_at_ms`) and `TerminalCaps` are passed explicitly as values.
3. **Lock-Free Read-Only Host DB Ingestion (`ingest`)**:
   - Reads `<root>/conversations/*.db` without interfering with active `agy` writers: static databases open with `?immutable=1`; databases with a non-empty `-wal` or active write state copy `.db` + `-wal` + `-shm` into an isolated temp directory before opening read-only.
   - Zero-copy Protobuf wire decoding (`proto_wire.rs`) extracts `CheckpointConfig.max_context_tokens` (`gen_metadata` `f1.f9.f10.f4`, `executor_metadata` `f10.f2.f5`), `systemPromptTokens` (`f4.f1`), input/output/cache tokens, TTFT (`f1.f11`), streaming duration (`f1.f12`), and **Cascade** parent/root lineage (`parent_cascade_id`, `root_cascade_id`).
4. **Unified Pricing & Single SQLite WAL Ledger (`domain::accounting` + `ledger`)**:
   - Eliminates v1's split cost formulas and `daily-cost.json` spin-lock file. Every **Statusline** and **Dashboard** figure uses the same `calculate_token_cost` + `f4.f1` system prompt accounting.
   - `agy-pulse-ledger.sqlite` runs on bundled SQLite (`rusqlite`) with `WAL` mode and `busy_timeout`, reconstructing historical **Ledger** data directly from CLI (`~/.gemini/antigravity-cli`) and IDE (`~/.gemini/antigravity`) conversation databases.
5. **Page-Lifecycle Dashboard Keepalive (`dashboard`)**:
   - Replaces v1's 10-minute background-tab suicide timer with `/api/keepalive` SSE streams + `/api/ping` leases + `pagehide` `sendBeacon` `/api/disconnect` with reload grace, keeping the local server alive while minimized or backgrounded and exiting cleanly once all pages close.
6. **Panic-Free Library Gate**:
   - `src/lib.rs` enforces `#![deny(clippy::unwrap_used, clippy::expect_used)]`; `run_cli()` wraps the Statusline pipeline in `catch_unwind`, logs errors safely to `agy-pulse-error.log` (symlink-checked `0600`), and exits `0`.
