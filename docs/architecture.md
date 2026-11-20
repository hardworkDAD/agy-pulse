# agy-pulse Architecture

> **Version:** 1.0.0  
> **Last updated:** aligned to single-process Node.js `agy-pulse` architecture

## Overview

`agy-pulse` is an all-in-one single-process statusline plugin for the [Antigravity CLI](https://antigravity.google) (`agy`).
It unifies multi-line workspace and quota status line rendering with real-time server compaction ceiling decoding, incremental transcript scanning, token speed, and cost tracking — with zero external npm dependencies and no Python/Bash subprocess glue.

`agy` invokes the registered `statusLine.command` once per agent step, piping a JSON payload on stdin. `runtime/bin/agy-pulse.js` must write rendered ANSI output to stdout and exit within ~1.5 s.

---

## Two-Layer Design

```
┌─────────────────────────────────────────────────────────────────────┐
│  PLUGIN LAYER  (staged by `agy plugin install`, lives in plugins/)  │
│   plugin.json · gemini-extension.json · skills/pulse-config/        │
└─────────────────────────────────┬───────────────────────────────────┘
                                  │  bootstrap.js / deploy-local.sh
┌─────────────────────────────────▼───────────────────────────────────┐
│  RUNTIME LAYER  (~/.gemini/antigravity-cli/agy-pulse-runtime/)      │
│   bin/agy-pulse.js ← statusLine.command                            │
│   brand.js · compaction.js · parser.js · speed.js · cost.js         │
│   daily-cost.js · config.js · encoding.js · paths.js · uninstall.js │
│   quota.js ──> quota/ (token · cache · cloud · models)              │
│   renderer.js ──> renderer/ (context-monitor · format · lang ·      │
│                              quota-render)                          │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Module Dependency Graph

```
bin/agy-pulse.js
  ├── brand.js           (BRAND_NAME, resolveRepoSlug, getRawGitHubUrl)
  ├── parser.js          (getSessionState, parseAgyInput, parseAgyQuota)
  │     ├── compaction.js (resolveServerCompactionLimit, resolveModelPerfStats, Protobuf wire decoder)
  │     └── paths.js      (resolveSafeExecutable, resolveAntigravityPath)
  ├── renderer.js        (renderStatusLine)
  │     ├── renderer/context-monitor.js (renderContextMonitorLine)
  │     ├── renderer/format.js
  │     ├── renderer/lang.js
  │     ├── renderer/quota-render.js
  │     ├── encoding.js  (supportsUnicode)
  │     ├── cost.js      (getSessionCostUsd, formatUsd)
  │     ├── daily-cost.js (getCostTotals)
  │     └── speed.js     (getOutputSpeed, formatTtft, formatChildModelSpeeds)
  ├── config.js          (loadConfig — deep merge with FULL_DEFAULT_CONFIG)
  ├── quota.js           (getQuota, getCachedTier)
  │     └── quota/ (token · cache · cloud · models)
  └── paths.js
```

---

## Key Architectural Upgrades in v1.0.0

1. **Native Server Compaction Limit & 30-DB Model Perf (`compaction.js`)**:
   - Reads `gen_metadata` (`f1.f9.f10.f4`) and `executor_metadata` (`f10.f2.f5`) from `<root>/conversations/<conversation_id>.db` via `node:sqlite` (`DatabaseSync`, read-only) with temp-copy and `sqlite3` CLI fallbacks.
   - Scans `gen_metadata` across the most recent 30 `conversations/*.db` databases (`f1.f4.f3` output tokens, `f1.f11` TTFT `Duration`, `f1.f12` `streamingDuration`, `f21`/`f19`/`f20[model_enum]`) to compute per-model pure streaming weighted average speed (`∑outputTokens / ∑streamingDuration`) and arithmetic average TTFT.
   - Uses `BigInt` varint shifting to prevent 32-bit JS signed overflow.
   - Caches limits (`_db_limits`, max 24) and per-DB sufficient statistics (`_db_perf`, max 36) keyed by `${dbSize}:${dbMtimeMs}:${walSize}:${walMtimeMs}` in `agy-pulse-compaction-cache.json`.
2. **Incremental Byte-Offset Transcript Scanning (`parser.js`)**:
   - Tracks `{ ino, offset, steps, compactionCount, totalOutputTokens, turnHistoryMaxByMode, lastTurnUsage }` in `transcript-cache/<sha256>.json`, avoiding full-file `JSON.parse` on long sessions.
3. **Cache-Inclusive / Exclusive Peak & Hit-Rate Normalization (`context-monitor.js`, `format.js`)**:
   - Maintains both `inclusivePeak` and `exclusivePeak` in `turnHistoryMaxByMode` and uses `modelIncludesCacheInInput` + `inp >= cacheTotal` checks so cache tokens are never double-counted.
4. **Fast-Path Auth & Lock-Safe Ledgers (`quota/`, `daily-cost.js`)**:
   - `getQuota({ fast: true })` skips blocking Linux keyring / Windows Credential Manager subprocesses and falls back cleanly on failed background refreshes.
   - `daily-cost.js` merges concurrent session updates before atomic rename.
5. **Independent Branding with Zero Upstream Coupling (`brand.js`)**:
   - `resolveRepoSlug` defaults to `null` unless `AGY_PULSE_REPO` / `config.repository` is explicitly configured.
   - All identifiers use the `agy-pulse` prefix: `agy-pulse.js`, `agy-pulse.config.json`, `AGY_PULSE_*` env vars, and `agy-pulse-runtime/`.
