# agy-pulse Module Reference

> **Version:** 2.0.0 — Deep-Module Contracts (`src/lib.rs` enforces `#![deny(clippy::unwrap_used, clippy::expect_used)]`)

---

## `src/lib.rs`, `src/main.rs` & `src/bin/agypl.rs` — CLI Dispatch & Statusline Guard

- `run_cli() -> i32`:
  - Dispatches `--version`/`-V`/`-v`/`version`, `dashboard`/`dash`/`ui`/`--dashboard`, `stats`/`report`/`--stats`, `config`/`--config`, `install`/`--install`, `uninstall`/`--uninstall`, `update`/`--update`, `--check-update-bg`, and `help`/`--help`/`-h`.
  - Default stdin mode executes inside `std::panic::catch_unwind` with a **1500 ms** hard `read_stdin_with_timeout`, logs diagnostics safely to `agy-pulse-error.log` (symlink-checked `0600`), and always exits `0`.

---

## `src/domain/` — Pure In-Memory Domain Kernel (Zero I/O)

- **`model.rs`**: Canonical domain structs aligned to `GLOSSARY.md` (`PulseSnapshot`, `HostIdentity`, `ModelIdentity`, `ContextWindowSnapshot`, `CostSnapshot`, `TokenUsage`, `ProviderQuotaSummary`, `ProviderQuota`, `QuotaWindow`, `ChildModelSpeed`, `TurnCacheMetrics`, `WorkspaceMetadata`).
- **`pricing.rs`**: `lookup_pricing(model)` & `calculate_token_cost(model, tokens, cache_inclusive, is_cumulative)` supporting Claude 4.5/4.6/5.5, Gemini 3.x/4 Carbon/Hydrogen/Argon/Neon, Orchestrator, DeepSeek, OpenAI o-series/GPT, and `>200k` long-context pricing tiers.
- **`accounting.rs`**: Single source of truth for `model_includes_cache_in_input`, `is_usage_cache_inclusive`, `calculate_turn_cache_metrics`, `resolve_cascade_cost_usd`, `compact_model_name`, `resolve_model_display_name`, and `format_usd`.
- **`quota.rs`**: Pure functions `parse_iso8601_to_epoch_ms`, `is_quota_window_expired`, `prune_expired_provider_quota`, `pick_critical_window`, `has_valid_provider_quota`, `format_quota_percent`, `format_reset_duration`, `format_reset_countdown`, and `is_openai_provider_key`.

---

## `src/ingest/` — Lock-Free Read-Only Host Ingestion (`HostReader`)

- **`mod.rs`**: Narrow public facade `HostReader::read_live_pulse(&stdin, &fallback_cwd) -> IngestResult` and `HostReader::read_conversation_db(db_path) -> Option<ConversationDbRecord>` (never leaks `rusqlite::Connection` or raw Protobuf blobs).
- **`stdin.rs`**: Parses heterogeneous host JSON (`model`, `workspace.current_dir`/`cwd`, `plan_tier`, `cycle_mode`, `context_window`, `cost`, `quota` keys `gemini-5h`/`gemini-weekly`/`anthropic-5h`/`claude-5h`/`3p-5h`/`argon*`).
- **`transcript.rs`**: Byte-offset incremental scanning of `transcript.jsonl` (up to 64 MB, `TRANSCRIPT_CACHE_VERSION = 10`) with `transcript_full.jsonl` `truncated_fields` backfill, `CHECKPOINT` compaction detection & `inclusivePeak`/`exclusivePeak` tracking, child/swarm model speed tracking, and `429` image quota exhaustion detection.
- **`sqlite_ro.rs` & `proto_wire.rs` & `db_telemetry.rs`**:
  - Opens static host `.db` files via `?immutable=1` and active `-wal` databases by copying `.db` + `-wal` + `-shm` into an isolated `TempDir`.
  - Decodes `gen_metadata` (`f1.f9.f10.f4` compaction ceiling, `f4.f1` `systemPromptTokens`, `f4.f2` raw input, `f4.f3` output, `f4.f5` cache read, `f1.f11` TTFT, `f1.f12` stream duration, `f20` parent/root lineage) and `executor_metadata` (`f10.f2.f5`), aggregating recent 30-DB weighted speeds and **Cascade** cost trees.
- **`workspace.rs`**: Pure-file `.git/HEAD` & `.git` worktree pointer resolution (timeout-bounded `git` fallback only for reftable), plus offline counts of memory files, rule `.md` files, git hooks, and MCP server entries.

---

## `src/ledger/` — Unified SQLite WAL Ledger & `stats` (`LedgerStore`)

- **`mod.rs` & `schema.rs`**: Manages `agy-pulse-ledger.sqlite` (`rusqlite` bundled + WAL + `busy_timeout`, `0600` file permissions) and dispatches `agypl stats`.
- **`sync.rs`**: Dual-source read-only scanning of CLI (`get_antigravity_roots()`) and IDE (`get_antigravity_ide_roots()`) `conversations/*.db` via `HostReader::read_conversation_db`, plus monotonic compaction-safe `record_rolling_costs()` for `today_usd` and `week_usd`.
- **`query.rs` & `stats_report.rs`**: Multi-dimensional filtering (`today`/`24h`/`7d`/`30d`/`month`/`all`/`custom`, `hour`/`day`/`week`, `all`/`cli`/`ide`, model, workspace, subagent toggle), **Cascade** root aggregation, and ANSI/JSON/CSV output.

---

## `src/render/` — Pure In-Memory Statusline Renderer (Zero I/O)

- **`mod.rs` & `layout.rs`**: Public entrypoint `render_statusline(&PulseSnapshot, &PulseConfig, &TerminalCaps, LanguageSetting) -> String`. Handles 9-step Line 1 narrow-terminal degradation, Line 2 tokens/cache/context bar, Line 3 workspace metadata, and ANSI-safe truncation.
- **`quota_table.rs`**: Renders 2-column provider quota table (`Google 5h/week`, `Claude 5h/week`, optional `Argon`/extras, filtering OpenAI) or `compact` single-line summary.
- **`context_bar.rs`**: Renders `Session Cache` (`render_model_cache_line`) and the 5-level smooth **Compaction** monitor (`render_context_monitor_line`) with 11-step narrow-width degradation.
- **`width.rs` & `i18n.rs`**: `unicode-width` CJK/emoji display width calculation, ANSI-preserving truncation, control-sequence sanitization, theme palettes, and `en`/`zh` copy.

---

## `src/config/` — Configuration & Interactive Raw-TTY Wizard

- **`mod.rs`**: Deep-merges `PulseConfig::default()` → global `agy-pulse.config.json` → workspace `./agy-pulse.config.json` → `AGY_PULSE_LANGUAGE`. Ignores retired `v1` keys and maps legacy `quotaStyle: "models"` to `Table`.
- **`wizard.rs`**: Interactive terminal wizard (`agypl config`) with live `render_statusline` preview, scope-isolated atomic saving, and safe non-TTY fallback.

---

## `src/dashboard/` — Local Web Dashboard, Persistent Keepalive & Shortcuts

- **`server.rs` & `http_wire.rs` & `assets/dashboard.html`**: Binds `127.0.0.1` (`3789` with ephemeral fallback), validates `Host`/`Origin`, emits CSP headers, and serves embedded single-page HTML + `/api/overview`, `/api/sync`, `/api/health`, `/api/keepalive` (SSE), `/api/ping`, `/api/disconnect`, `/api/export.csv`, `/api/export.json`, `/icon.svg`, and `/manifest.webmanifest`.
- **`keepalive.rs`**: Tracks active page instances across SSE connections and ping leases; stays online while minimized/backgrounded and shuts down cleanly after close grace when the last page closes.
- **`launcher.rs`**: Single-instance daemon state (`agy-pulse-dashboard.json`), Chromium `--app=<url>` detection, and fingerprint-guarded desktop shortcuts (`install-shortcut` / `uninstall-shortcut`).

---

## `src/ops/` & `src/platform/` — Operations & OS Isolation

- **`ops/installer.rs`**: Deploys `agy-pulse` & `agypl` into `AGY_PULSE_BIN_DIR` (`~/.local/bin` or `%LOCALAPPDATA%\agy-pulse\bin`), atomically updates `settings.json` `statusLine` (backing up corrupt JSON to `.bak`), and cleans stale `v1` plugin artifacts without ever writing to official `agy` program directories.
- **`ops/uninstall.rs`**: Stops running dashboard daemon, removes fingerprint-matching shortcuts, clears `agy-pulse`-owned `statusLine` entries (protecting foreign statuslines), cleans caches/binaries, and preserves `agy-pulse-ledger.sqlite` unless `--purge-ledger` is passed.
- **`ops/updater.rs`**: Enforces `https://` (loopback `http://` only for tests), forbids `https -> http` redirect downgrades, verifies `SHA256SUMS`, and atomically self-replaces `agy-pulse` and `agypl` (including Windows `.exe.old` swap).
- **`platform/`**: Isolates Unix (`unix.rs`) and Windows (`windows.rs`) terminal raw mode, TTY width/locale probing, detached spawning, process signaling, and atomic binary replacement.
