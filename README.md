# agy-pulse

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](#license)

> **All-in-one single-process statusline HUD, real-time context & compaction monitor, and cloud quota tracker for Antigravity CLI (`agy`).**
>
> Zero external npm dependencies · Pure Node.js runtime · Native SQLite Protobuf compaction limit decoding · Incremental transcript scanning · Bilingual (`en` / `zh`)

[简体中文](./README_zh.md)

---

## What it looks like

`agy-pulse` unifies the classic `agy-hud` multi-line dashboard & cloud quota table with `agy-statusline`'s real-time context & compaction monitor into a single fast Node.js process:

```
my-project │ Google AI Pro │ out: 42.6 tok/s
1 MEMORY.md │ 4 rules │ 2 MCPs │ 3 hooks
  ─────────────────────────────────────────────────────────────────────────────────
  Google 5h   [█████░]  87% ~22m   │ Claude 5h   [██████] 100% ~4h59m
  Google week [████░░]  74% ~4d3h  │ Claude week [██████] 100% ~6d23h
◈ Gemini 4 Carbon (High) · Ctx ████▓······· 38.4% (196.6K/512K) · Compacted×1(peak 480K) · Cache 82% (160K) · Out 12.4K · Cost $0.12
```

When context usage reaches **85%+**, the bottom bar switches to red and appends **`Compaction imminent`**. On non-UTF-8 terminals (e.g. Windows `cp936`), it automatically degrades cleanly to ASCII (`*`, `#`/`=`/`-`/`.`, `x`, `|`).

### Display modes (`display.quotaStyle`)
- **`table`** (default): 2-column provider window overview (`Google 5h/week`, `Claude 5h/week`, and optional `Argon`) + bottom real-time context monitor. Missing quota windows render `--` instead of a misleading `100%`.
- **`models`**: 2-column per-model quota breakdown with custom `columnWidth`.
- **`compact`**: Provider-grouped mini bars (`Google:`, `Anthropic:`, `OpenAI:`) with zero family-name collisions across 2026 models (`Carbon`, `Hydro`, `Argon`, `Orch-C`, `Orch-A`, `G3.8F`, etc.).

---

## Install

Run in a normal shell (NOT inside an active `agy` session):

### Local / self-hosted checkout

**macOS / Linux**:
```bash
bash scripts/install.sh
```

**Or deploy runtime & migrate an existing `agy-hud` / `statusline-combined.sh` setup**:
```bash
bash scripts/deploy-local.sh
```

### Remote GitHub repository / fork

Set `AGY_PULSE_REPO` to your own `owner/repo` slug:

**macOS / Linux**:
```bash
AGY_PULSE_REPO="yourname/agy-pulse" \
  bash <(curl -fsSL "https://raw.githubusercontent.com/$AGY_PULSE_REPO/main/scripts/install.sh")
```

**Windows PowerShell**:
```powershell
$env:AGY_PULSE_REPO = 'yourname/agy-pulse'
irm "https://raw.githubusercontent.com/$env:AGY_PULSE_REPO/main/scripts/install.ps1" | iex
```

This installer:
1. Stages the declarative plugin (`agy plugin install`).
2. Installs the pure Node.js runtime to `~/.gemini/antigravity-cli/agy-pulse-runtime/`.
3. Configures `statusLine.command` in `~/.gemini/antigravity-cli/settings.json` to invoke `runtime/bin/agy-pulse.js` (or `.cmd` shim on Windows), preserving existing `enabled` / `stack_with_default` flags.

---

## Verify & Configure

```bash
# Inspect settings.json
cat ~/.gemini/antigravity-cli/settings.json | grep statusLine -A3

# Direct invocation
node ~/.gemini/antigravity-cli/agy-pulse-runtime/runtime/bin/agy-pulse.js

# Launch interactive configuration wizard
node ~/.gemini/antigravity-cli/agy-pulse-runtime/runtime/bin/agy-pulse.js --config

# Auth & token diagnostics
node scripts/diagnose-auth.js
```

---

## Uninstall

```bash
bash uninstall.sh
```

Windows PowerShell:
```powershell
.\uninstall.ps1
```

Removes both `agy-pulse` and legacy `agy-hud` runtime directories, plugin installs, caches, logs, Windows shims, and clears `settings.json` `statusLine` (saving a `.bak` backup).

---

## Configuration

Optional. Create `agy-pulse.config.json` (or legacy `agy-hud.config.json`) in your workspace root to override defaults. Without it, `agy-pulse` uses `runtime/agy-hud.config.json` deep-merged with `FULL_DEFAULT_CONFIG`:

```json
{
  "theme": {
    "primary": "green",
    "secondary": "gray",
    "warning": "yellow",
    "critical": "red"
  },
  "display": {
    "quotaStyle": "table",
    "showContextMonitor": true,
    "showCurrentDir": true,
    "showSpeed": true,
    "showModel": false,
    "showTokenBar": false,
    "showContextBar": false,
    "showTurnCacheHitRate": false,
    "showBreadcrumbs": false,
    "showGitBranch": false,
    "showUsername": false,
    "showCost": false,
    "showDailyCost": false,
    "showWeeklyCost": false,
    "breadcrumbCount": 3,
    "useNerdFonts": false,
    "columnWidth": 40
  },
  "thresholds": {
    "warning": 0.7,
    "critical": 0.9
  },
  "language": "auto"
}
```

### Configuration fields
- **`theme`**: Foreground colors (`primary`, `secondary`, `warning`, `critical`). Supports `green`, `gray`, `yellow`, `red`, `blue`, `magenta`, `cyan`.
- **`display`**:
  - `showContextMonitor`: Show the bottom real-time context & compaction monitor bar (`true` by default).
  - `quotaStyle`: `"table"` (default), `"models"`, or `"compact"`.
  - `showCurrentDir`: Show current working directory basename (`true` by default).
  - `showSpeed`: Show token output generation speed (`out: X.X tok/s`, `true` by default).
  - `showModel`: Show model name on Line 1 (`false` by default since Line bottom shows `◈ <Model (Effort)>`).
  - `showTokenBar` / `showContextBar`: Optional classic Line 2 token & context bars.
  - `showTurnCacheHitRate`: Show per-turn cache hit rate badge on Line 2.
  - `showImageQuota`: Show image generation quota bar on Line 2.
  - `showBreadcrumbs` / `breadcrumbCount`: Show recent file breadcrumbs on Line 3.
  - `showGitBranch`: Show current Git branch on Line 1 (omitted automatically in non-Git directories).
  - `showUsername` / `username`: Show resolved login email or custom username.
  - `showCost` / `showDailyCost` / `showWeeklyCost`: Show session, daily, and weekly USD totals.
  - `useNerdFonts`: Use Nerd Fonts icons.
  - `columnWidth`: Max column width in quota table/models layout (default `40`).
- **`thresholds`**: Quota warning (`0.7`) and critical (`0.9`) ratios.
- **`language`**: `"auto"` (detects `LC_ALL`/`LC_CTYPE`/`LANG`), `"en"`, or `"zh"`.

---

## Environment Variables (with `AGY_HUD_*` backward compatibility)

| Primary | Legacy Alias | Purpose |
|---|---|---|
| `AGY_PULSE_DATA_DIR` | `AGY_HUD_DATA_DIR` | Override Antigravity CLI data directory |
| `AGY_PULSE_REPO` | `AGY_HUD_REPO` | `owner/repo` slug for self-update / remote install |
| `AGY_PULSE_REPO_RAW` | `AGY_HUD_REPO_RAW` | Custom raw GitHub base URL |
| `AGY_PULSE_REPO_URL` | `AGY_HUD_REPO_URL` | Custom Git clone URL for `agy plugin install` |
| `AGY_PULSE_UPDATE_URL` | `AGY_HUD_UPDATE_URL` | Custom `package.json` URL for update checker |
| `AGY_HUD_FORCE_ASCII` | — | Set `1` to force ASCII fallback |
| `AGY_HUD_FORCE_UNICODE` | — | Set `1` to force Unicode output |

---

## File structure

```
agy-pulse/
├── plugin.json                        # {"name":"agy-pulse"}
├── gemini-extension.json              # agy remote-install manifest (v1.0.0)
├── runtime/
│   ├── brand.js                       # Brand constants & repository slug resolver
│   ├── compaction.js                  # Pure-JS Protobuf wire decoder & SQLite compaction limit reader
│   ├── bin/
│   │   ├── agy-pulse.js / .cmd        # Primary statusLine entrypoint
│   │   └── agy-hud.js / .cmd          # Backward-compatible entrypoint alias
│   ├── config-wizard.js               # Interactive terminal config wizard (--config)
│   ├── config.js                      # Deep-merging config loader & atomic writer
│   ├── cost.js                        # Model pricing table (2026 models) & session cost
│   ├── daily-cost.js                  # Lock-safe daily & 7-day rolling cost ledger
│   ├── encoding.js                    # Terminal UTF-8 / codepage detector
│   ├── parser.js                      # Incremental byte-offset transcript scanner & metadata
│   ├── paths.js                       # Cross-platform root & safe executable resolver
│   ├── quota.js                       # SWR quota orchestrator
│   ├── quota/                         # token · cache · cloud · models
│   ├── renderer.js                    # Multi-line HUD orchestrator
│   ├── renderer/                      # context-monitor · format · lang · quota-render
│   ├── speed.js                       # Output tok/s calculator with active-turn latching
│   ├── statusline-installer.js        # settings.json updater & Windows .cmd/.bat shims
│   ├── uninstall.js                   # Full uninstaller for agy-pulse & legacy agy-hud
│   └── update-checker.js              # Semver-safe background update checker
├── scripts/
│   ├── install.sh / install.ps1       # One-command installers
│   ├── bootstrap.sh / bootstrap.js    # Atomic runtime installer
│   ├── deploy-local.sh                # Local deploy & legacy combined-script migrator
│   ├── configure-utf8.ps1             # Windows UTF-8 helper
│   ├── verify-display.js              # E2E verifier
│   └── diagnose-auth.js               # Auth & token cache diagnostic
├── skills/hud-config/SKILL.md         # Agent config skill
├── docs/                              # Architecture & module reference (en / zh)
└── tests/unit/                        # Modular unit tests (node --test)
```

---

## License

MIT
