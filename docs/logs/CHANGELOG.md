# Changelog

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
- **Native Installer, Safe Uninstaller & 5-Platform Release Pipeline (`src/ops/`, `scripts/`, `.github/workflows/`)**:
  - Added `agypl install`, `agypl uninstall [--purge-ledger]`, and `agypl update` (HTTPS-enforced with `https -> http` downgrade rejection and `SHA256SUMS` verification).
  - Rewrote `scripts/install.sh`, `scripts/install.ps1`, `scripts/deploy-local.sh`, `uninstall.sh`, `uninstall.ps1`, and `release.sh` with zero Node.js dependency.
  - Added `.github/workflows/ci.yml` (Linux/macOS/Windows `clippy` + `test`) and `.github/workflows/release.yml` packaging all 5 official targets (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`) with unified `SHA256SUMS`.
  - Added `tests/t01`–`tests/t10` black-box integration and golden parity test suites.

## [1.0.0] - 2026-11-18

### Added
- **Unified `agy-pulse` Single-Process Architecture**: Merged the legacy HUD and `agy-statusline` into a single zero-dependency Node.js runtime (`runtime/bin/agy-pulse.js`).
- **Protobuf & SQLite Compaction Limit + 30-DB Model Perf**: Added `gen_metadata` / `executor_metadata` decoding, smooth 5-level context monitor, incremental transcript scanning, permanent telemetry ledger (`stats` / `dashboard`), standalone app-window launcher, and desktop shortcuts.
