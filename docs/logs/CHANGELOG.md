# Changelog

## [1.0.0] - 2026-11-18

### Added
- **Unified `agy-pulse` Single-Process Architecture**: Merged `agy-hud` and `agy-statusline` into a single zero-dependency Node.js runtime (`runtime/bin/agy-pulse.js`), eliminating Python + Bash + `mktemp` subprocess glue.
- **`runtime/brand.js`**: Centralized branding (`agy-pulse`, `1.0.0`), config filenames (`agy-pulse.config.json` / `agy-hud.config.json`), and safe `resolveRepoSlug` without upstream `icebear0828/agy-hud` hardcoding.
- **`runtime/compaction.js`**: Pure-JS Protobuf wire decoder (with `BigInt` varint shifting) and SQLite read-only `CheckpointConfig.max_context_tokens` extractor (`gen_metadata` `f1.f9.f10.f4` & `executor_metadata` `f10.f2.f5`), with `size:mtimeMs` WAL-safe caching and cold-start recent-DB fallback.
- **`runtime/renderer/context-monitor.js`**: Native 5-level smooth context & compaction progress bar (`█▓▒░·` / `#=-.`), compaction counter & historical peak (`Compacted×N(peak X)` / `压缩×N(峰X)`), cache hit rate, output tokens, cost, and `>=85%` imminent compaction alert in both `zh` and `en`.
- **2026 Model Coverage**: Full support for `gemini-4-carbon-*`, `gemini-4-hydrogen-*` (`XHigh`), `gemini-4-argon-*`, `gemini-3.8-flash-*`, `gemini-pro-orchestrator(-argon)`, `claude-opus-4-6-thinking`, `claude-sonnet-4-6`, and `gpt-oss-120b-medium` across quota discovery, pricing, abbreviations, and collision-free compact names.
- **Comprehensive Unit Tests**: Added `compaction.test.mjs`, `renderer.context-monitor.test.mjs`, `update-checker.test.mjs`, and `config-wizard.test.mjs`, plus expanded regression suites.

### Fixed
- **Cache-Inclusive Double-Counting & Empty `current_usage` Fallback**: Normalized `promptCtx` and `turnHistoryMaxByMode` (`inclusivePeak` / `exclusivePeak`) and fell back to `state.lastTurnUsage` when `current_usage` is `{}` or all zeros.
- **Incremental Transcript Scanning & Timing**: Added byte-offset incremental caching in `parser.js`, fixed `PLANNER_RESPONSE` duration closing on `SYSTEM_MESSAGE` / `USER_INPUT`, prevented 429 false positives via structured error inspection + `transcript_full.jsonl` lookup, and returned `null` branch in non-Git directories.
- **Speed Calculation Edge Cases**: Fixed first-turn plain-text reply `null` speed and relaxed token alignment tolerance in `speed.js`.
- **Auth & Quota Cache Stability**: Prevented Linux keyring hot-path blocking on `fast: true`, prevented stale token mirror shadowing on account switch, prevented failed background refreshes from overwriting valid quota cache with `[]`, and used `base64url` for JWT decoding.
- **Cost & Ledger Accuracy**: Fixed first-turn baseline omission, context-compaction monotonic cursor regression, concurrent multi-session writes, and offline 7-day rolling weekly cost fallback in `daily-cost.js`.
- **Renderer, Config, Installer, & Updater**: Passed `targetCwd` to `loadConfig()`, deep-merged `FULL_DEFAULT_CONFIG`, omitted hardcoded `display.unicode` to enable auto-detection, rendered `--` for missing quota windows instead of fake `100%`, preserved `enabled` / `stack_with_default` in `settings.json`, added semver comparison and request timeouts in `update-checker.js`, and expanded `config-wizard.js` and `uninstall.js`.
