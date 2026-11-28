# Changelog

## [2.1.0] - 2026-11-28

### Added
- **Multi-Device Object Sync & Ledger Schema v4 (`src/sync/`, `src/ledger/`, ADR-0004)**:
  - Added single-writer-per-device monthly gzipped chunk sync (`local-dir`, `webdav`, pure-Rust SigV4 `s3`, and `cf-worker` with `deploy/cloudflare-worker/` template), storing credentials exclusively in global `0600` `agy-pulse-sync.json` (never in workspace config).
  - Added `agypl sync [init|status|prune-device]` and `agypl account assign <email>`, throttled background `--sync-bg` pushes, cross-device **Quota Snapshots**, and Dashboard multi-account/multi-device filters + quota cards + non-blocking `/api/sync-remote`.
- **Zero-Login Dashboard Config API & Standalone `/settings` Page (`src/dashboard/config_api.rs`, `dashboard.html`, ADR-0005)**:
  - Redesigned the embedded Web Dashboard with Bento cards, Spend Distribution donut, 3-column Rankings, Core Trends tabs, toast notifications, and a draggable Floating Glass Dock.
  - Added a dedicated standalone Settings page (`/settings`, `/settings/pulse`, `/settings/sync`) with History API routing, full-width pinnable live ANSI **Statusline** preview rendered directly by the Rust engine, 2×2 Bento config cards, 12 deduplicated statusline toggles (`cacheHitThresholds` wired end-to-end), and a sticky Sync Config sidebar with write-only credentials (`keep`/`set`/`clear`), live connection probe, `409 Conflict` optimistic locking, and safe config deletion.
  - Protected all `/api/config/*` routes with process-scoped 64-hex CSRF tokens (`X-Agy-Pulse-Token`), strict same-origin checks, JSON `Content-Type` enforcement, and `redact_sync_error_message` error sanitization.
- **Thin Statusline Client, Resident Collector & In-Memory WAL Merge (`src/collector/`, `src/ingest/sqlite_ro.rs`, ADR-0006)**:
  - Isolated the hot-path `agy-pulse` statusline as a thin zero-SQLite client backed by a single-instance resident collector (embedded in the background dashboard daemon or standalone `--collector-bg`) with `tmpfs` precomputed snapshots, eliminating Linux D-state 5 s SIGKILL hangs.
  - Enabled `rusqlite` `serialize` to merge committed WAL frames in memory via `sqlite3_deserialize` (zero `/tmp/agy-pulse-db-*` copies and zero fsync storms), deduplicated active DB reads from 3× to 1× per refresh, and added stale temp-dir sweeping.

### Changed & Fixed
- **Performance & Reliability**:
  - Accelerated `>8 MB` two-pass transcript scanning with chunk-level Horspool & `memchr`, bounded statusline SQLite busy retries by wall-clock budget, enforced end-to-end statusline exit deadlines, and coalesced small HTTP responses with graceful half-close on early `400`/`413` rejections.
  - Deduplicated 17 legacy display toggles down to 12 canonical segments and moved the local Dashboard URL onto its own dedicated bottom line.
  - Fixed a collector `worker_busy` release ordering race so immediate sequential statusline invocations never receive spurious `Busy` fallbacks, and added a single retry on idle-shutdown disconnects (`tests/t12`–`tests/t16`).

## [2.0.0] - 2026-11-21

### Breaking Changes & Architecture Overhaul
- **Single Native Static Rust Binary (`agy-pulse` / `agypl`) — Zero Node.js Dependency (ADR-0001)**:
  - Replaced the entire v1 Node.js runtime (`runtime/`), `npm` package manifests (`package.json`, `package-lock.json`), legacy script bootstrappers (`scripts/bootstrap.js`, `scripts/bootstrap.sh`), and JS white-box unit tests (`tests/unit/`) with a single statically linked Rust binary compiled from `Cargo.toml` (`src/main.rs` + `src/bin/agypl.rs`), delivering ~4 ms cold-start latency and zero runtime dependencies.
  - Preserved host-required static plugin declarations (`plugin.json`, `gemini-extension.json` bumped to `2.0.0`, `skills/pulse-config/SKILL.md`) while keeping CLI entrypoints (`agypl` / `agy-pulse`) and `agy-pulse.config.json` backward-compatible.
- **Compliance Hardening — Zero Off-Host Antigravity Requests (ADR-0002)**:
  - Physically removed all off-host cloud quota fetchers, OAuth / keychain / Credential Manager readers, `agypl diagnose` (`scripts/diagnose-auth.js`), and retired config fields (`endpoints`, `interestingModels`, `quotaStyle: "models"`, `cloudQuota`).
  - Provider **Quota** (`table`, `compact`, `none`; legacy `"models"` smoothly falls back to `"table"`) and plan tier displays now consume exclusively the host's `stdin` JSON payload.
- **Deep-Module Single-Crate Architecture (ADR-0003)**:
  - Organized the codebase into 8 cohesive, single-responsibility modules with strict acyclic dependencies: pure in-memory `domain` and `render`, plus `ingest`, `ledger`, `config`, `dashboard`, `ops`, and `platform`.
  - Enforced `#![deny(clippy::unwrap_used, clippy::expect_used)]` across the entire library crate and a 1500 ms hard stdin timeout guard that always degrades safely and exits `0`.

### Added
- **Unified Billing & Cascade Tree Accounting (`src/domain/`, `src/ingest/`)**:
  - Consolidated token normalization and USD pricing into `domain::accounting` & `domain::pricing`, unifying **Statusline** and **Dashboard** numbers and consistently billing `f4.f1` (`systemPromptTokens`), cache read/write tokens, and `>200k` long-context tiers.
  - Lock-free read-only host `.db` ingestion (`?immutable=1` for static DBs and temporary `.db`/`.db-wal`/`.db-shm` copies for active WAL DBs) with zero-copy Protobuf wire decoding and transitive **Cascade** subagent cost trees.
- **Unified SQLite WAL Ledger & `stats` (`src/ledger/`)**:
  - Replaced the separate `daily-cost.json` + spin-lock file and JSON fallback ledger with a single bundled `rusqlite` WAL database (`agy-pulse-ledger.sqlite`, `0600` permissions).
  - Automatic dual-source CLI + IDE conversation `.db` scanning, compaction-safe monotonic rolling daily/7-day cost tracking, and full `agypl stats` (`table`, `--json`, `--csv`) reporting.
- **Interactive Config Wizard (`src/config/wizard.rs`)**:
  - Native cross-platform raw-TTY interactive configuration wizard (`agypl config`) invoking `render::render_statusline` for live preview, scope-isolated global/local saves, atomic writes, and safe non-TTY fallback.
- **Persistent-Keepalive Web Dashboard & Shortcuts (`src/dashboard/`)**:
  - Embedded zero-CDN frontend (`include_str!`) served over `127.0.0.1` with `Host`/`Origin` validation and CSP headers.
  - Redesigned background keepalive around live page instances (`/api/keepalive` SSE + `/api/ping` + `pagehide` `/api/disconnect` beacon with reload grace), keeping the **Dashboard** online even when minimized or backgrounded in a browser tab until all pages close.
  - Preserved single-instance daemon management (`open`, `stop`, `status`), Chromium `--app` window launch, and fingerprint-guarded desktop shortcuts (`install-shortcut` / `uninstall-shortcut`).
  - Added statusline Line 1 local Web Dashboard URL rendering and silent background auto-start/keepalive (`display.showDashboardUrl` default `true`, `display.dashboardPort` default `3789`, `display.dashboardIdleTimeoutSec` default `1800`), coordinated via exclusive spawn lock (`agy-pulse-dashboard.lock`), throttled `/api/ping` renewal + stale-instance respawn, loopback URL sanitization, and env overrides (`AGY_PULSE_DASHBOARD_PORT`, `AGY_PULSE_DASHBOARD_IDLE_SEC`, `AGY_PULSE_NO_DASHBOARD_AUTOSTART=1`).
- **Native Installer, Safe Uninstaller & 5-Platform Release Pipeline (`src/ops/`, `scripts/`, `.github/workflows/`)**:
  - Added `agypl install`, `agypl uninstall [--purge-ledger]`, and `agypl update` (HTTPS-enforced with `https -> http` downgrade rejection and `SHA256SUMS` verification).
  - Rewrote `scripts/install.sh`, `scripts/install.ps1`, `scripts/deploy-local.sh`, `uninstall.sh`, `uninstall.ps1`, and `release.sh` with zero Node.js dependency.
  - Added `.github/workflows/ci.yml` (Linux/macOS/Windows `clippy` + `test`) and `.github/workflows/release.yml` packaging all 5 official targets (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`) with unified `SHA256SUMS`.
  - Added `tests/t01`–`tests/t11` black-box integration and golden parity test suites.

## [1.0.0] - 2026-11-18

### Added
- **Unified `agy-pulse` Single-Process Architecture**: Merged the legacy HUD and `agy-statusline` into a single zero-dependency Node.js runtime (`runtime/bin/agy-pulse.js`).
- **Protobuf & SQLite Compaction Limit + 30-DB Model Perf**: Added `gen_metadata` / `executor_metadata` decoding, smooth 5-level context monitor, incremental transcript scanning, permanent telemetry ledger (`stats` / `dashboard`), standalone app-window launcher, and desktop shortcuts.
