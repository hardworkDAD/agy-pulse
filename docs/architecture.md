# agy-pulse Architecture

> **Version:** 2.0.0  
> **Last updated:** aligned to single-binary native Rust architecture ([ADR-0001](./adr/0001-rust-single-binary-v2.md), [ADR-0002](./adr/0002-remove-cloud-quota-requests.md), [ADR-0003](./adr/0003-deep-module-single-crate.md), [ADR-0004](./adr/0004-multi-device-object-sync.md), [ADR-0005](./adr/0005-dashboard-local-config-management.md))

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
│   Single Cargo crate (src/lib.rs) with 9 cohesive deep modules:          │
│     domain · render · ingest · ledger · sync · config · dashboard · ops │
│     + platform (OS isolation)                                           │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Deep-Module Dependency Graph

```
src/main.rs / src/bin/agypl.rs  ──> agy_pulse::run_cli()
  ├── ops        (installer, uninstall, `account assign`, GitHub updater)
  │     ├── ledger
  │     ├── dashboard (stop daemon / remove shortcuts on uninstall)
  │     └── platform
  ├── dashboard  (127.0.0.1 HTTP server, SSE keepalive, launcher, embedded SPA, config_api)
  │     ├── sync   (non-blocking async pull/push, raw config snapshot & probe)
  │     ├── ledger
  │     ├── config (global config snapshot, unknown-key deep merge & live preview)
  │     ├── render
  │     └── platform
  ├── sync       (SyncRemote trait, wire format v1, 4 backends, --sync-bg & CLI)
  │     ├── ledger
  │     ├── domain
  │     └── platform
  ├── config     (deep-merged config, atomic 0600 save, interactive raw-TTY wizard)
  │     ├── render
  │     └── platform
  ├── ledger     (bundled SQLite WAL Schema v4, CLI/IDE backfill, stats CLI, CSV/JSON)
  │     ├── sync   (read active SyncConfig for syncStatus overview)
  │     ├── ingest
  │     ├── domain
  │     ├── config
  │     ├── render
  │     └── platform
  ├── ingest     (HostReader: stdin, incremental transcript, lock-free .db + Protobuf)
  │     ├── domain
  │     └── platform
  ├── render     (100% pure in-memory Statusline layout, quota table, width & i18n)
  │     └── domain
  └── domain     (100% pure in-memory types, unified pricing/accounting, quota, sync types)
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
5. **Page-Lifecycle Dashboard Keepalive & Async Remote Pull (`dashboard`)**:
   - Replaces v1's 10-minute background-tab suicide timer with `/api/keepalive` SSE streams + `/api/ping` leases + `pagehide` `sendBeacon` `/api/disconnect` with reload grace, keeping the local server alive while minimized or backgrounded and exiting cleanly once all pages close.
   - Follows a stale-while-revalidate pattern for multi-device data: `GET /api/overview` immediately serves local SQLite data (local + previously pulled remote records), while the frontend triggers non-blocking `POST /api/sync-remote` (30 s server throttle, `?force=1` / `{"force":true}` bypass) to pull remote **Devices** and refresh the view when new chunks or **Quota Snapshots** arrive.
6. **Multi-Device Object Sync & Ledger Schema v4 (`sync` + `ledger`, [ADR-0004](./adr/0004-multi-device-object-sync.md))**:
   - **Unified `SyncRemote` contract**: `probe`, `list`, `get`, `put`, and `delete` implemented across `local_dir`, `webdav` (`ureq`), `s3` (pure-Rust HMAC-SHA256 SigV4, zero OpenSSL), and `cf_worker` (`deploy/cloudflare-worker/`).
   - **Single-writer-per-device wire format v1**: Each **Device** writes exclusively under `<prefix>/accounts/<account_key>/devices/<device_id>/` (`manifest.json` + UTC monthly `chunks/YYYY-MM.jsonl.gz` with SHA-256 skip), eliminating multi-machine write conflicts and omitting conversation text, `workspace`, and `branch`.
   - **Ledger Schema v4**: Adds `account_id` and `device_id` columns to `ingested_dbs` and `telemetry_steps` (with `idx_steps_device_ts` and `idx_steps_account_device_ts`), introduces `sync_devices`, `quota_snapshots`, and `sync_chunk_state`, stores sync metadata in `ledger_meta` (`local_device_id`, `last_observed_email`, `last_sync_push_ms`, `last_sync_pull_ms`, `last_sync_error`, `last_sync_remote_fingerprint`), and keeps `rolling_cost_cursors` strictly local.
   - **Hot-path isolation & `--sync-bg`**: **Statusline** queries strictly filter `device_id = local_device_id`, optionally spawning a detached `agy-pulse --sync-bg` push guarded by `agy-pulse-sync.lock`, 15 s HTTP timeouts, and a 45 s hard watchdog. Global sync config (`agy-pulse-sync.json`, `0600`) is strictly isolated from workspace `agy-pulse.config.json`.
7. **Local Dashboard Config Management & Write-Only Credential Contract (`dashboard::config_api`, [ADR-0005](./adr/0005-dashboard-local-config-management.md))**:
   - **Zero-login loopback + CSRF & strict same-origin**: `DashboardServer::start` generates a 32-byte (64-hex) process-scoped CSRF token injected into `dashboard.html` on `GET /`. Every `/api/config/*` endpoint validates `X-Agy-Pulse-Token` in constant time, enforces strict same-origin (`Sec-Fetch-Site: same-origin` / loopback `Origin`), and requires `application/json` on mutating requests.
   - **Single source of truth & unknown-key preservation**: The Settings drawer reads and writes only the global `agy-pulse.config.json` (never touching workspace directories) and global `0600` `agy-pulse-sync.json`, using raw disk snapshots without writing environment overrides to disk, preserving unknown fields (`columnWidth`, custom keys), and reusing Rust `render_statusline` (`config::render_statusline_preview`) for live **Statusline** previews.
   - **Write-only credentials & optimistic concurrency**: `GET`/`PUT /api/config/sync` never echo plaintext secrets (`password`, `token`, `accessKey`, `secretKey`); `PUT` uses `keep` / `set` / `clear` directives (inheriting `keep` only within the same remote type) and `POST /api/config/sync/test` sanitizes errors via `redact_sync_error_message`. Both config files use per-resource `Mutex` locks and SHA-256 `version` checks (`409 Conflict` on mismatch), and `PUT`/`DELETE` immediately refresh `LedgerRuntime.sync_options` identity hints.
8. **Panic-Free Library Gate**:
   - `src/lib.rs` enforces `#![deny(clippy::unwrap_used, clippy::expect_used)]`; `run_cli()` wraps the Statusline pipeline in `catch_unwind`, logs errors safely to `agy-pulse-error.log` (symlink-checked `0600`), and exits `0`.
