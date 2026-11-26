---
name: pulse-config
description: Configure or customize agy-pulse status line settings (enabled, theme, icons, quota display mode, context monitor, thresholds, language) / 当用户想要配置或自定义 agy-pulse 状态栏的显示设置（开关、主题、图标、面板模式、上下文监控、阈值、语言等）时触发。
---

# Interactive `agy-pulse` Statusline Configuration / 交互式配置 `agy-pulse` 状态栏

When the user asks to configure, customize, toggle, or style the `agy-pulse` status line, follow this workflow / 当用户要求修改、设置、美化或配置 `agy-pulse` 状态栏时，请按以下流程操作：

## 1. Gather Preferences / 发起交互选择

Use `ask_question` (or ask concisely) to confirm scope and desired options / 使用 `ask_question` 工具确认配置范围与偏好：

- **Scope / 保存范围**：
  - Local workspace config (`./agy-pulse.config.json`) / 局部项目配置
  - Global default config (`~/.gemini/antigravity-cli/agy-pulse-runtime/agy-pulse.config.json`, or `$AGY_PULSE_GLOBAL_CONFIG_DIR/agy-pulse.config.json`) / 全局默认配置
- **Master Switch / 总开关 (`enabled`)**：`true` (enabled / 启用) | `false` (disabled / 禁用，CLI 静默退出不输出)
- **Theme / 颜色主题 (`theme`)**：
  - Preset / 预设字符串 (`"Emerald Green"`, `"Ocean Blue"`, `"Cyberpunk Magenta"`, `"Amber Gold"`) or single color name (`"green"`, `"blue"`, etc.)
  - Or custom object `{ "primary", "secondary", "warning", "critical" }` with colors: `"green"`, `"gray"`, `"yellow"`, `"red"`, `"blue"`, `"magenta"`, `"cyan"`
- **Quota Display Mode / 配额显示模式 (`display.quotaStyle`)**：
  - `"table"` (default — 2-column provider overview: Google & Claude 5h/week + optional Argon / 双列厂商概览)
  - `"compact"` (single-line provider summary: Google, Claude & Argon / 单行厂商配额摘要)
  - `"none"` (hide quota panel completely / 完全关闭配额面板)
- **Icons & Encoding / 图标与字符集**：
  - `display.useNerdFonts`: `false` (default) | `true`
  - `display.unicode`: omit for auto-detection via terminal encoding (recommended) / 省略即自动检测（推荐），`true` 强制 Unicode，`false` 强制纯 ASCII
- **Language / 界面语言 (`language`)**：`"auto"` | `"en"` | `"zh"`

## 2. Exact JSON Schema & Valid Values / 精确 JSON 路径与合法值

```json
{
  "enabled": true,
  "language": "auto",
  "theme": {
    "primary": "green",
    "secondary": "gray",
    "warning": "yellow",
    "critical": "red"
  },
  "display": {
    "quotaStyle": "table",
    "useNerdFonts": false,
    "showContextMonitor": true,
    "showModelCache": true,
    "showCurrentDir": true,
    "showCycleMode": true,
    "showSpeed": true,
    "showDashboardUrl": true,
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
    "showRoutedCost": false,
    "breadcrumbCount": 3,
    "columnWidth": 40
  },
  "thresholds": {
    "warning": 0.7,
    "critical": 0.9
  }
}
```

### Field Reference / 字段清单

| JSON Path | Type / Valid Values | Default | Description / 说明 |
|---|---|---|---|
| `enabled` | `boolean` | `true` | Enable or disable status line output / 是否启用状态栏输出 |
| `language` | `"auto" \| "en" \| "zh"` | `"auto"` | UI language (`auto` detects `LC_ALL`/`LC_CTYPE`/`LANG`, overridden by `AGY_PULSE_LANGUAGE`) / 界面语言 |
| `theme` | `object \| string` | `{ primary: "green", secondary: "gray", warning: "yellow", critical: "red" }` | Foreground colors (`green`, `gray`, `yellow`, `red`, `blue`, `magenta`, `cyan`), preset string (`"Emerald Green"`, `"Ocean Blue"`, `"Cyberpunk Magenta"`, `"Amber Gold"`), or single color name |
| `display.quotaStyle` | `"table" \| "compact" \| "none"` | `"table"` | Provider quota panel layout (`"none"` hides quota panel; legacy `"models"` falls back to `"table"`) / 厂商配额面板布局 |
| `display.useNerdFonts` | `boolean` | `false` | Enable Nerd Font glyphs / 启用 Nerd Fonts 图标 |
| `display.unicode` | `boolean` (optional) | *omitted (auto)* | Omit for auto-detect; `true` forces Unicode, `false` forces ASCII / 省略自动检测；`true` 强制 Unicode，`false` 强制 ASCII |
| `display.showContextMonitor` | `boolean` | `true` | Bottom real-time context & compaction bar / 底部实时上下文与压缩监控行 |
| `display.showModelCache` | `boolean` | `true` | Per-model cache hit breakdown line above context monitor (active by default when `showContextMonitor` is `true`) / 底部区分模型缓存命中明细行 |
| `display.showCurrentDir` | `boolean` | `true` | Current directory basename on Line 1 / 状态栏首行当前目录名 |
| `display.showCycleMode` | `boolean` | `true` | Interactive cycle mode badge (`auto` / `accept-edits` / `plan` / `best-of-n`) on Line 1 / 状态栏首行交互模式徽章 |
| `display.showSpeed` | `boolean` | `true` | Output speed (`tok/s`) & TTFT on Line 1 / 状态栏首行输出速率与首字延迟 |
| `display.showDashboardUrl` | `boolean` | `true` | Show local Web Dashboard URL on its own dedicated bottom line and silently auto-start & keep it alive while `agy` is active (disable auto-start with `AGY_PULSE_NO_DASHBOARD_AUTOSTART=1`) / 在状态栏最底部单开一行显示本地网页看板网址并静默自动拉起保活（设 `AGY_PULSE_NO_DASHBOARD_AUTOSTART=1` 可禁止自动拉起） |
| `display.dashboardPort` | `number` (optional) | `3789` | Preferred loopback port for status-line auto-started dashboard (overridden by `AGY_PULSE_DASHBOARD_PORT`) / 状态栏自动拉起网页看板的首选端口（优先级低于 `AGY_PULSE_DASHBOARD_PORT`） |
| `display.dashboardIdleTimeoutSec` | `number` (optional) | `1800` | Idle auto-exit timeout in seconds for status-line auto-started dashboard (`0` disables idle exit; overridden by `AGY_PULSE_DASHBOARD_IDLE_SEC`) / 状态栏自动拉起网页看板的空闲退出秒数（`0` 常驻不自动退出；优先级低于 `AGY_PULSE_DASHBOARD_IDLE_SEC`） |
| `display.modelSpeedExpireMinutes` | `number` (optional) | `5` | Auto-hide model speed & TTFT after N idle minutes / 超过指定分钟未调用自动隐藏对应模型速度与首字 |
| `display.modelSpeedExpireTurns` | `number` (optional) | `30` | Auto-hide main/child model speed & TTFT after N idle turns / 超过指定轮次未调用自动隐藏对应模型速度与首字 |
| `display.showModel` | `boolean` | `false` | Model name on Line 1 / 状态栏首行模型名 |
| `display.showTokenBar` | `boolean` | `false` | Line 2 token breakdown bar / 第二行 Token 明细条 |
| `display.showContextBar` | `boolean` | `false` | Line 2 classic context progress bar / 第二行经典上下文条 |
| `display.showTurnCacheHitRate` | `boolean` | `false` | Line 2 per-turn cache hit rate badge / 第二行单轮缓存命中率 |
| `display.cacheHitThresholds` | `object` (`{ excellent?, partial? }`) | `{ excellent: 80, partial: 50 }` | Custom turn cache hit color thresholds (`>= excellent` bold green, `>= partial` yellow; shallow-merged) / 单轮缓存命中率颜色阈值（浅合并） |
| `display.showBreadcrumbs` | `boolean` | `false` | Line 3 workspace memory / recently touched files / 第三行工作区记忆或最近文件面包屑 |
| `display.breadcrumbCount` | `number` | `3` | Max breadcrumb files shown (wizard cycles `0, 1, 2, 3, 5`) / 面包屑最多显示文件数 |
| `display.showGitBranch` | `boolean` | `false` | Git branch on Line 1 (hidden in non-Git dirs) / 状态栏首行 Git 分支 |
| `display.showUsername` | `boolean` | `false` | Host stdin `username` / `email` or `display.username` / 显示宿主 `stdin.username` / `stdin.email` 或自定义用户名 |
| `display.username` | `string` | *unset* | Custom username override when `showUsername` is `true` (overrides host `stdin.username` / `stdin.email` and OS username) / 自定义用户名覆写（优先于宿主 `stdin.username` / `stdin.email` 与系统用户名） |
| `display.showCost` | `boolean` | `false` | Cascade USD cost / 当前任务树总费用 |
| `display.showDailyCost` | `boolean` | `false` | Today's USD cost / 今日费用 |
| `display.showWeeklyCost` | `boolean` | `false` | 7-day rolling USD cost / 近 7 日费用 |
| `display.showRoutedCost` | `boolean` | `false` | Trust CLI native cost for Bedrock (`anthropic.*` / `us.anthropic.*` / `eu.anthropic.*`) & Vertex (`claude-*@*`) routed models instead of token-rate estimation / Bedrock/Vertex 路由模型是否采信 CLI 原生费用（默认 `false` 改用本地费率估算） |
| `display.columnWidth` | `number` | `40` | Max column width hint / 配额表列宽参考 |
| `display.terminalWidth` | `number` (optional) | *unset (auto)* | Explicit terminal column width cap (`agyData.terminal_width > display.terminalWidth > COLUMNS > TTY`) / 显式终端列宽上限 |
| `icons` | `object` (optional) | *unset* | Custom icon overrides (`branch`, `plan`, `step`, `task`, `token`, `ctx`, `model`, max 8 chars, shallow-merged) / 自定义图标覆写（浅合并） |
| `thresholds.warning` | `number` (`0`–`1`) | `0.7` | Quota warning threshold (`0.7` yellow; bottom context monitor uses `65%` at default `0.7` or `warning*100%` if customized) / 配额黄色预警阈值（自定义时同步作用于底栏上下文监控） |
| `thresholds.critical` | `number` (`0`–`1`) | `0.9` | Quota critical threshold (`0.9` red; bottom context monitor uses `85%` at default `0.9` or `critical*100%` if customized) / 配额红色告警阈值（自定义时同步作用于底栏上下文监控） |

## 3. Write Config / 写入配置文件

Read the existing target file (if any), deep-merge user selections while preserving unrelated keys, and write back to:
- **Local / 局部路径**: `./agy-pulse.config.json`
- **Global / 全局路径**: `~/.gemini/antigravity-cli/agy-pulse-runtime/agy-pulse.config.json` (if a legacy `~/.gemini/antigravity-cli/agy-pulse-runtime/runtime/agy-pulse.config.json` exists, it is still discovered automatically)

Alternatively, the user can run the native `agypl` (or `agy-pulse`) command directly in the terminal / 用户也可在终端直接运行原生命令 `agypl`（或 `agy-pulse`）打开交互向导、遥测看板或执行安装升级：
```bash
agypl config                              # or --config
agypl stats [today|24h|7d|30d|month|all]  # or report / --stats
agypl dashboard                           # or dash / ui / --dashboard
agypl install                             # or --install
agypl uninstall [--purge-ledger]          # or --uninstall
agypl update                              # or --update
agypl help                                # or --help / -h
```
