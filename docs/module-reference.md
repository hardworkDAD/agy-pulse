# agy-pulse Module Reference

> All paths are relative to the runtime install root:  
> `~/.gemini/antigravity-cli/agy-pulse-runtime/`

---

## `runtime/brand.js` — Brand & Repository Constants

Exports:
- `BRAND_NAME = 'agy-pulse'`
- `PACKAGE_VERSION = '1.0.0'`
- `CONFIG_FILENAMES = ['agy-pulse.config.json']`
- `RUNTIME_DIR_NAME = 'agy-pulse-runtime'`
- `resolveRepoSlug(config, env = process.env)` — resolves `owner/repo` from `env.AGY_PULSE_REPO || config?.repository || null` (never falls back to an upstream repository).
- `getRawGitHubUrl(repoSlug, branch = 'main', filePath = 'package.json')` — returns raw GitHub URL or `null`.

---

## `runtime/bin/agy-pulse.js` — Entrypoint

**Role:** Registered as `statusLine.command` in `settings.json`. Launched by `agy` once per agent step.

- `--stats` → delegates to `stats.js::runStatsCli(process.argv.slice(2))` and exits with its returned `exitCode`.
- `--dashboard` → delegates to `dashboard.js::startDashboardCli(process.argv.slice(2))` to start the local `127.0.0.1` web dashboard.
- `--help` / `-h` → outputs CLI subcommand help (`buildCliHelpText()`) and exits `0`.
- `--config` → delegates to `config-wizard.js::startWizard()`.
- `--diagnose` → delegates to `scripts/diagnose-auth.js::buildAuthDiagnostic()`, outputs JSON diagnostic report, and exits `0`.
- `--update` → delegates to `handleSelfUpdate()` using `loadConfig(process.cwd())`, `resolveRepoSlug()`, `http`/`https` download with safe redirects (max 5, no `https -> http` downgrade), derives `AGY_PULSE_REPO_RAW` from query/subpath `AGY_PULSE_BOOTSTRAP_URL`, awaits `bootstrap.js --cleanup-self` exit code, and forwards real exit status.
- Default stdin mode:
  - 1500 ms hard timeout fallback.
  - Resolves `targetCwd = agyData?.workspace?.current_dir || agyData?.cwd || process.cwd()`.
  - Runs `Promise.all([getSessionState(transcriptPath, targetCwd), loadConfig(targetCwd), getQuota({ fast: true }), getCachedTier()])`.
  - Exits cleanly with no `stdout` when `config?.enabled === false` (still triggers `maybeCheckUpdates(targetCwd)`).
  - Pre-warms `daily-cost.json` via `getCostTotals(...)` whenever a real session identifier or existing transcript exists, attaching `state.costTotals`.
  - Reads `agy-pulse-update-status.json` via `readUpdateInfo()`, filtering out stale versions where `!isNewerVersion(status.latestVersion, installedVersion)`.
  - Merges live `agyData.quota` via `parseAgyQuota` + per-provider window merge (`mergeProviderQuota`) + `enrichModelsWithQuotaSummary`, preserving non-enumerable `unavailableReason`.
  - Calls `renderStatusLine(...)`, writes to `stdout`, silently triggers 15s-throttled `ledger.js::syncSingleConversationToLedger(convId, { childConversationIds })` inside `try/catch` via lazy `require('../ledger.js')` when a session ID (`agyData?.conversation_id` or `/brain/<uuid>/`) is present, atomically updates `agy-pulse-update-status.json`, and spawns `update-checker.js <targetCwd>` once per 24h.
  - `detectLiveTerminalColumns()` preserves exact 80-column widths (`cols > 82 ? cols - 3 : cols`) so standard 80-col terminals keep the bottom `◈ <Model>` badge visible.
  - Errors are logged safely to `agy-pulse-error.log` (symlink-checked, mode `0o600`) and process exits `0`.

---

## `runtime/compaction.js` — Protobuf Wire, SQLite Compaction Limit & 30-DB Model Perf

- `decodeVarint(buf, offset = 0)` — uses `BigInt` shifts to prevent 32-bit signed overflow.
- `pbGetSub(buf, fieldNum)`, `pbGetAllSub(buf, fieldNum)`, `pbGetVarint(buf, fieldNum)`, `pbGetString(buf, fieldNum)`, `pbGetDurationSec(buf, fieldNum)`.
- `extractCompactionLimitFromGenMetadata(buf)` — traverses `f1 -> f9 -> f10 -> f4` (`[16000, 10000000]`).
- `extractCompactionLimitFromExecutorMetadata(buf)` — traverses `f10 -> f2 -> f5` (`[16000, 10000000]`).
- `extractGenMetadataTelemetry(buf)` / `extractModelPerfFromGenMetadata(buf)` — extracts per-generation `inputTokens` (`f1.f4.f2`), `outputTokens` (`f1.f4.f3`), `cacheReadTokens` (`f1.f4.f5`), `ttftSec` (`f1.f11`), `streamSec` (`f1.f12`), compact model key + full `stepModel` (`f21 || f19 || f20[model_enum]`), and session lineage (`f20[parent_cascade_id]`, `f20[root_cascade_id]`).
- `querySqliteCompactionLimit(dbPath)`, `querySqliteModelTelemetry(dbPath, options)` & `querySqliteModelPerf(dbPath, options)` — opens read-only via `node:sqlite` `DatabaseSync` with temp-copy/`sqlite3` CLI fallback; filters `<0.05s` streaming noise and `>600s` TTFT outliers while keeping per-model token & USD ledgers for every generation step.
- `resolveServerCompactionLimit(options)` — caches limits keyed by `${dbSize}:${dbMtimeMs}:${walSize}:${walMtimeMs}` in `agy-pulse-compaction-cache.json` (`_db_limits`, capped at 24), with cold-start fallback across the 5 newest `conversations/*.db` files and final `512000` default.
- `resolveModelPerfStats(options)` / `resolveSessionCostTree(options)` — aggregates pure streaming weighted average speed (`∑outputTokens / ∑streamingDuration`), arithmetic average TTFT, and `lastUpdatedMs` (merged with `options.currentSessionModelLastActiveMs`) across the 30 most recent `conversations/*.db` files (always including `activeDbPath` if present), plus per-model cost/token breakdowns across the entire session tree (main + active/killed `/swarm` & subagent DBs matched via `childConversationIds` and `parent_cascade_id`/`root_cascade_id`), caching `schemaVer: 2` per-DB stats in `_db_perf` (capped at 64) and `_persisted_child_speeds` in `agy-pulse-compaction-cache.json`.

---

## `runtime/parser.js` — Incremental Session State

`getSessionState(transcriptPath, cwd, options)` performs incremental byte-offset scanning cached at `transcript-cache/<sha256>.json` (`TRANSCRIPT_CACHE_VERSION = 9`):
- Returns `branch: string | null` (`null` for non-Git directories).
- Tracks `compactionCount` (`type === 'CHECKPOINT'`), `totalInputTokens`, `totalOutputTokens`, `totalCacheReadTokens`, `totalCacheCreationTokens`, `lastTurnUsage`, `turnHistoryMaxByMode: { inclusivePeak, exclusivePeak }`, `serverCompactionLimit`, `modelPerf`, `modelLastActiveMs`, and `costBreakdown` (`{ totalCostUsd, mainCostUsd, childrenCostUsd, childSessionCount, byModel, source }`, preferring SQLite session cost trees with transcript catch-up and pure-transcript fallback).
- Handles nested `context_window` as well as top-level `PLANNER_RESPONSE` token fields.
- Closes `PLANNER_RESPONSE` durations cleanly on `SYSTEM_MESSAGE`, invalid timestamps, or `USER_INPUT`, marking interrupted open turns (`interrupted: true`) so `speed.js` never uses stale `now - startMs` spans.
- Tracks all registered `/swarm` workers and `invoke_subagent` child sessions (`childAgents`, persisting across `killed`, `SYSTEM_MESSAGE` stop, `USER_INPUT` turns, and idle gaps), tail-reading up to 64 KB of sibling transcripts via `readChildTailTelemetry()` / `resolveChildModelSpeeds()` to aggregate per-model deduplicated rolling speeds (`childModelSpeeds`, capped at top 3 recent models, persisted in `transcript-cache` `v9` and `_persisted_child_speeds` when child logs are cleaned up, with fallback to `modelPerf[model].avgSpeed` when `modelHint` is present) and pruning models idle beyond `MAX_MODEL_IDLE_TURNS` (30 turns) or `MAX_MODEL_IDLE_MS` (5 minutes).

---

## `runtime/renderer.js`, `runtime/speed.js` & `runtime/renderer/` — Status Line & Context Monitor

### `runtime/renderer.js`
- `formatCycleModeSegment(cycleMode, { bold, reset })` — formats `agyData.cycle_mode` with theme-independent fixed semantic ANSI colors (`auto` → yellow, `accept-edits` → green, `plan` → bright blue `\x1b[94m`, `bestn` → `best-of-n` magenta, unknown non-empty → sanitized 24-char cyan) and preserves the badge immediately after `planSeg` on Line 1 ahead of narrow-terminal degradation steps when `display.showCycleMode !== false`.

### `runtime/speed.js`
- `getOutputSpeed(agyData, rootDir, state, now)` — single-turn output `tok/s` calculator with 500ms–120s active-turn latching.
- `formatTtft(ttftSec, lang)` — formats TTFT as `<N>ms` when `<1s` and `<N.N>s` when `>=1s` (`ttft` in `en`, `首字` in `zh`).
- `formatChildModelSpeeds(childModelSpeeds, options)` — formats child/non-main model badges, enriching with `modelPerf[model]` (`avgSpeed` + `avgTtftSec`) when available (`⚡ Hydro: X.X tok/s (首字 Y.Ys)` / `[sub] Hydro: X.X tok/s (ttft Y.Ys)`).
- `renderer.js` Line 1 prioritizes `state.modelPerf[mainModelKey].avgSpeed` (falling back to single-turn `outputSpeed`), appends `(${formatTtft(avgTtftSec, lang)})`, merges `state.childModelSpeeds` with non-main `state.modelPerf` entries (sorted by `lastUpdatedMs` descending, capped at `MAX_CHILD_MODELS_DISPLAY = 3`) when `showSpeed` is enabled, and hides both main and child model speeds/TTFT when idle beyond `modelSpeedExpireTurns` (default `30`) or `modelSpeedExpireMinutes` (default `5`).

### `runtime/renderer/context-monitor.js`
- `formatMonitorTokens(n)` — base-1000 uppercase `K`/`M` formatter.
- `buildSmoothBar(pct, width = 12, unicode = true)` — 5-level smooth bar (`█▓▒░·` in UTF-8, `#=-.` in ASCII).
- `renderModelCacheLine({ state, agyData, config, colors, unicode, lang })` — renders the dedicated per-model session cache hit line (`⟳ 累计缓存 · Carbon 86% (命中 38.8M / 总入 45.2M) · ...` / `⟳ Session Cache · ...`) immediately above `renderContextMonitorLine`, preferring `state.costBreakdown.byModel` with current-turn fallback and 6-step responsive width degradation.
- `renderContextMonitorLine({ state, agyData, config, colors, unicode, lang, sessionCostUsd })`:
  - Renders `◈ <Model (Effort)> · Ctx <bar> <pct>% (<active>/<limit>) · Compacted×N(peak X) · Cache X% (Y) · Out Z · Cost $W · Compaction imminent` (localized in `en` and `zh`).
  - Falls back to `state.lastTurnUsage` when `current_usage` is missing, `{}`, or all zero.
  - Normalizes cache-inclusive vs cache-exclusive inputs via `modelIncludesCacheInInput` and picks `inclusivePeak` / `exclusivePeak` accordingly.

### `runtime/renderer/format.js`
- `TIER_ABBREVS` includes `XHigh: 'XH'`, `Thinking: 'Th'`, `High: 'H'`, `Medium: 'M'`, `Low: 'L'`.
- `ABBREVIATION_RULES` & `COMPACT_NAME_RULES` cover 2026 models (`Gemini 4 Carbon/Hydrogen/Argon`, `Gemini 3.8 Flash`, `Orchestrator (Carbon/Argon)`, `Claude Opus/Sonnet 4.6`, `GPT-OSS 120B`) without family collisions.
- `modelIncludesCacheInInput(nameOrId)` covers `claude|anthropic|sonnet|opus|haiku|gemini|carbon|hydrogen|argon|orchestrator|gpt|deepseek|o1-|o3-|o4-`.
- `calculateTurnCacheMetrics(usage, rawModelName)` uses `rawModelName` and `modelIncludesCacheInInput` to avoid misclassifying exclusive models.
- `getDisplayWidth` & `truncateToDisplayWidth` align wide CJK/emoji characters cleanly.

### `runtime/renderer/quota-render.js`
- `renderProviderQuotaTable`: renders `--` for missing windows instead of fake `100%`, skips `~now` for elapsed `resetTime`, and scales divider width dynamically with `columnWidth`.

---

## `runtime/config.js` & `runtime/config-wizard.js`

- `FULL_DEFAULT_CONFIG`: enables `display.showContextMonitor: true`, `display.showModelCache: true`, `showCurrentDir: true`, `showCycleMode: true`, `showSpeed: true`, and keeps `display.unicode` unset for automatic `encoding.js` detection.
- `loadConfig(cwd)` resolves `<cwd>/agy-pulse.config.json` → bundled config and deep-merges `theme`, `display`, `display.cacheHitThresholds`, `icons`, and `thresholds`.
- `saveConfig(config, isGlobal, cwd)` writes atomically via temp file + `renameSync`.
- `config-wizard.js` supports `j`/`k`/`↑`/`↓`, `1-9`/`0`, `s` (save), `q`/`Esc` (cancel), `quotaStyle` (`table`/`compact`/`models`/`none`), `breadcrumbCount` (`0`/`1`/`2`/`3`/`5`), `language` (`auto`/`en`/`zh`), `showContextMonitor`, `showModelCache`, `showCycleMode`, `showModel`, `showContextBar`, `showTurnCacheHitRate`, `showImageQuota`, scope-isolated saving via `buildConfigForScopeSave`, and small-height terminal viewport scrolling (`▲`/`▼`).

---

## `runtime/paths.js`, `runtime/statusline-installer.js`, `runtime/uninstall.js`, `runtime/update-checker.js`

- `paths.js`: accepts `(env, homeDir)` overrides, honors `AGY_PULSE_DATA_DIR` for CLI roots (`getAntigravityRoots`), and provides `getAntigravityIdeRoots(env, homeDir)` (honoring `AGY_PULSE_IDE_DATA_DIR`, returning `[]` when `AGY_PULSE_DATA_DIR` is set alone for sandbox isolation, and defaulting to `~/.gemini/antigravity` plus XDG/AppData fallbacks).
- `statusline-installer.js`: configures `settings.json` to invoke `agy-pulse.js` (or `.cmd` on Windows with `%LocalAppData%\Programs\nodejs\node.exe` & `%ProgramFiles%\nodejs\node.exe` fallbacks), backs up corrupt `settings.json` to `.bak`, preserves `enabled` and `stack_with_default`, and generates `AGY_PULSE_NODE` / `AGY_STATUSLINE_NODE` aware `statusline-combined.sh` shims.
- `uninstall.js`: cleans `agy-pulse-runtime`, primary & secondary `settings.json` `statusLine` entries, `plugin_data/agy-pulse`, `statusline-combined.sh`, caches, tmp mirrors/artifacts (`agy-pulse-db-*`, `agy-pulse-bootstrap-*`), and Windows shims. Preserves permanent telemetry ledgers (`agy-pulse-ledger.sqlite` / `agy-pulse-ledger.json`) by default unless `--purge-ledger` / `--purge` (`options.purgeLedger`) is passed.
- `update-checker.js`: SemVer 2.0 prerelease comparison (`compareSemver` / `isNewerVersion`), `http`/`https` safe redirects (max 5), `options.cwd || process.argv[2] || process.cwd()` config lookup, request timeout destruction, stale `updateAvailable` cleanup on check errors, and `resolveRepoSlug` integration.
- `scripts/bootstrap.js`: atomic runtime replacement (including `ledger.js`, `stats.js`, `dashboard.js`, `dashboard-ui.js`, and `scripts/diagnose-auth.js`) preserving saved global `agy-pulse.config.json` (unless `--reset-config`), `AGY_PULSE_DATA_DIR` support, `{ roots }` scoped `refreshQuotaCache`, stale `agy-pulse-update-status.json` & `skills/hud-config` cleanup, and `--cleanup-self` temp cleanup.

---

## `runtime/ledger.js`, `runtime/stats.js`, `runtime/dashboard.js` & `runtime/dashboard-ui.js` — Permanent Telemetry Ledger, CLI Stats & Local Web Dashboard

- **`runtime/ledger.js`**:
  - Stores permanent per-step telemetry and session lineage (`schema_version = '2'` with `source` column `'cli' | 'ide'` and idempotent v1→v2 SQLite/JSON migration) in `<DATA_DIR>/agy-pulse-ledger.sqlite` via built-in `node:sqlite` `DatabaseSync` (`WAL` mode, `0o700` dir / `0o600` file, automatic fallback to `agy-pulse-ledger.json` when `node:sqlite` is unavailable).
  - Read-only combined scanning of Antigravity CLI (`getAntigravityRoots`) and Antigravity IDE (`getAntigravityIdeRoots`) `conversations/*.db` via `resolveLedgerSourceRoots` (`?immutable=1` when no non-empty `-wal`, or temp-copying `.db` + `-wal` + `-shm` when `-wal` is active), extracting step timestamps (`steps.metadata` tag 1), workspace/branch/createdAt (`trajectory_metadata_blob`), and per-generation telemetry (`gen_metadata`), reusing `compaction.js` and `cost.js`. `queryLedger` supports `filter.source` (`'all' | 'cli' | 'ide'`) and returns `meta.source`, `meta.availableSources`, `bySource`, and per-session `source` (`formatLedgerCsv(..., 'sessions')` includes `source` after `rootConversationId`).
  - Exports `DEFAULT_LEDGER_FILENAME`, `FALLBACK_LEDGER_FILENAME`, `SYNC_THROTTLE_MS`, `normalizeSource`, `normalizeSourceFilter`, `resolveLedgerPath`, `resolveLedgerSourceRoots`, `resolveTimeRange`, `syncSingleConversationToLedger` (15s throttled, CLI-only), `syncAllConversationsToLedger`, `queryLedger`, `formatLedgerCsv`, `extractStepTimestampMs`, `extractTrajectoryMeta`, and `extractStepIndicesFromGenMetadata`.
- **`runtime/stats.js`**:
  - Implements `--stats` (`parseStatsArgs`, `renderStatsText`, `runStatsCli`) with range/granularity/source (`--source <all|cli|ide>`)/model/workspace/subagent filters, bilingual ANSI summary tables (`bySource` distribution + `[CLI]`/`[IDE]` session tags) + ASCII timeseries bars, and `--json` / `--csv` output.
- **`runtime/dashboard.js` & `runtime/dashboard-ui.js`**:
  - `dashboard.js` (`DEFAULT_DASHBOARD_PORT = 3789`, `parseDashboardArgs`, `createDashboardServer`, `startDashboardCli`) binds strictly to `127.0.0.1` (auto-falling back to port `0` on `EADDRINUSE`), validates `Host` and `Origin` headers, parses `source` (`all | cli | ide`) in `parseOverviewFilter`, and serves `GET /`, `GET /api/overview`, `POST /api/sync`, `GET /api/export.csv`, and `GET /api/export.json`.
  - `dashboard-ui.js` renders a zero-CDN bilingual dark/light SPA with date range presets & custom date pickers, `#source-pills` (`All / CLI / IDE`) filter, `#source-summary-bar` breakdown, `hour`/`day`/`week` granularity, model/workspace/subagent filters, 6 KPI cards, 2 interactive native SVG charts, a sortable per-model breakdown table, and a searchable/expandable `/swarm` session tree table with `CLI` / `IDE` source badges.
