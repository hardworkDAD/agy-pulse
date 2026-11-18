# agy-pulse Module Reference

> All paths are relative to the runtime install root:  
> `~/.gemini/antigravity-cli/agy-pulse-runtime/` (legacy alias: `agy-hud-runtime/`)

---

## `runtime/brand.js` — Brand & Repository Constants

Exports:
- `BRAND_NAME = 'agy-pulse'`
- `LEGACY_BRAND_NAME = 'agy-hud'`
- `PACKAGE_VERSION = '1.0.0'`
- `CONFIG_FILENAMES = ['agy-pulse.config.json', 'agy-hud.config.json']`
- `RUNTIME_DIR_NAME = 'agy-pulse-runtime'`
- `LEGACY_RUNTIME_DIR_NAME = 'agy-hud-runtime'`
- `resolveRepoSlug(config, env = process.env)` — resolves `owner/repo` from `env.AGY_PULSE_REPO || env.AGY_HUD_REPO || config?.repository || null` (never falls back to upstream `icebear0828/agy-hud`).
- `getRawGitHubUrl(repoSlug, branch = 'main', filePath = 'package.json')` — returns raw GitHub URL or `null`.

---

## `runtime/bin/agy-pulse.js` (alias `runtime/bin/agy-hud.js`) — Entrypoint

**Role:** Registered as `statusLine.command` in `settings.json`. Launched by `agy` once per agent step.

- `--config` → delegates to `config-wizard.js::startWizard()`.
- `--update` → delegates to `handleSelfUpdate()` using `resolveRepoSlug()` (exits with guidance if unconfigured).
- Default stdin mode:
  - 1500 ms hard timeout fallback.
  - Resolves `targetCwd = agyData?.workspace?.current_dir || agyData?.cwd || process.cwd()`.
  - Runs `Promise.all([getSessionState(transcriptPath, targetCwd), loadConfig(targetCwd), getQuota({ fast: true }), getCachedTier()])`.
  - Merges live `agyData.quota` via `parseAgyQuota` + `enrichModelsWithQuotaSummary`.
  - Calls `renderHUD(...)`, writes to `stdout`, spawns background `update-checker.js` once per 24h.
  - Errors are logged to `agy-pulse-error.log` (mode `0o600`) and process exits `0`.

---

## `runtime/compaction.js` — Protobuf Wire & SQLite Compaction Limit

- `decodeVarint(buf, offset = 0)` — uses `BigInt` shifts to prevent 32-bit signed overflow.
- `pbGetSub(buf, fieldNum)`, `pbGetAllSub(buf, fieldNum)`, `pbGetVarint(buf, fieldNum)`.
- `extractCompactionLimitFromGenMetadata(buf)` — traverses `f1 -> f9 -> f10 -> f4` (`[16000, 10000000]`).
- `extractCompactionLimitFromExecutorMetadata(buf)` — traverses `f10 -> f2 -> f5` (`[16000, 10000000]`).
- `querySqliteCompactionLimit(dbPath)` — opens read-only via `node:sqlite` `DatabaseSync` with copy/`sqlite3` CLI fallback.
- `resolveServerCompactionLimit(options)` — caches limits keyed by `${dbSize}:${dbMtimeMs}:${walSize}:${walMtimeMs}` in `agy-pulse-compaction-cache.json`, with cold-start fallback across the 5 newest `conversations/*.db` files and final `512000` default.

---

## `runtime/parser.js` — Incremental Session State

`getSessionState(transcriptPath, cwd)` performs incremental byte-offset scanning cached at `transcript-cache/<sha256>.json`:
- Returns `branch: string | null` (`null` for non-Git directories).
- Tracks `compactionCount` (`type === 'CHECKPOINT'`), `totalOutputTokens`, `lastTurnUsage`, `turnHistoryMaxByMode: { inclusivePeak, exclusivePeak }`, and `serverCompactionLimit`.
- Handles nested `context_window` as well as top-level `PLANNER_RESPONSE` token fields.
- Closes `PLANNER_RESPONSE` durations cleanly on `SYSTEM_MESSAGE`, invalid timestamps, or `USER_INPUT`.

---

## `runtime/renderer.js` & `runtime/renderer/` — HUD & Context Monitor

### `runtime/renderer/context-monitor.js`
- `formatMonitorTokens(n)` — base-1000 uppercase `K`/`M` formatter.
- `buildSmoothBar(pct, width = 12, unicode = true)` — 5-level smooth bar (`█▓▒░·` in UTF-8, `#=-.` in ASCII).
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

- `FULL_DEFAULT_CONFIG`: enables `display.showContextMonitor: true`, `showCurrentDir: true`, `showSpeed: true`, and keeps `display.unicode` unset for automatic `encoding.js` detection.
- `loadConfig(cwd)` resolves `<cwd>/agy-pulse.config.json` → `<cwd>/agy-hud.config.json` → bundled config and deep-merges `theme`, `display`, `thresholds`.
- `saveConfig(config, isGlobal, cwd)` writes atomically via temp file + `renameSync`.
- `config-wizard.js` supports `j`/`k`/`↑`/`↓`, `1-9`/`0`, `s` (save), `q`/`Esc` (cancel), `quotaStyle` (`table`/`compact`/`models`), `language` (`auto`/`en`/`zh`), `showContextMonitor`, `showModel`, `showContextBar`, `showTurnCacheHitRate`, and `showImageQuota`.

---

## `runtime/paths.js`, `runtime/statusline-installer.js`, `runtime/uninstall.js`, `runtime/update-checker.js`

- `paths.js`: accepts `(env, homeDir)` overrides and honors `AGY_PULSE_DATA_DIR` and `AGY_HUD_DATA_DIR`.
- `statusline-installer.js`: configures `settings.json` to invoke `agy-pulse.js` (or `.cmd` on Windows), preserving `enabled` and `stack_with_default`.
- `uninstall.js`: cleans both `agy-pulse-runtime` and `agy-hud-runtime`, `statusline-combined.sh`, `agy-statusline`, caches, tmp mirrors, and Windows shims.
- `update-checker.js`: semver comparison (`isNewerVersion`), request timeout销毁, and `resolveRepoSlug` integration.
