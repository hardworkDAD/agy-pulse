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
    "showBreadcrumbs": false,
    "showGitBranch": false,
    "showUsername": false,
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
| `display.showContextMonitor` | `boolean` | `true` | Bottom real-time context & compaction bar (model, progress bar, turn cache, output & cost) / 底部实时上下文与压缩监控行（含模型名、进度条、单轮缓存、输出量与单窗费用） |
| `display.showModelCache` | `boolean` | `true` | Session model cache line showing cumulative cache hit rate per model (independent toggle, default `true`) / 底部模型累计缓存行（按模型汇总的累计缓存命中率，独立开关默认开启） |
| `display.showCurrentDir` | `boolean` | `true` | Current directory basename on Line 1 / 状态栏首行当前目录名 |
| `display.showCycleMode` | `boolean` | `true` | Interactive cycle mode badge (`auto` / `accept-edits` / `plan` / `best-of-n`) on Line 1 / 状态栏首行交互模式徽章 |
| `display.showSpeed` | `boolean` | `true` | Output speed (`tok/s`) & TTFT on Line 1 / 状态栏首行输出速率与首字延迟 |
| `display.showDashboardUrl` | `boolean` | `true` | Show local Web Dashboard URL on its own dedicated bottom line and silently auto-start & keep it alive while `agy` is active (disable auto-start with `AGY_PULSE_NO_DASHBOARD_AUTOSTART=1`) / 在状态栏最底部单开一行显示本地网页看板网址并静默自动拉起保活（设 `AGY_PULSE_NO_DASHBOARD_AUTOSTART=1` 可禁止自动拉起） |
| `display.dashboardPort` | `number` (optional) | `3789` | Preferred loopback port for status-line auto-started dashboard (overridden by `AGY_PULSE_DASHBOARD_PORT`) / 状态栏自动拉起网页看板的首选端口（优先级低于 `AGY_PULSE_DASHBOARD_PORT`） |
| `display.dashboardIdleTimeoutSec` | `number` (optional) | `1800` | Idle auto-exit timeout in seconds for status-line auto-started dashboard (`0` disables idle exit; overridden by `AGY_PULSE_DASHBOARD_IDLE_SEC`) / 状态栏自动拉起网页看板的空闲退出秒数（`0` 常驻不自动退出；优先级低于 `AGY_PULSE_DASHBOARD_IDLE_SEC`） |
| `display.modelSpeedExpireMinutes` | `number` (optional) | `5` | Auto-hide model speed & TTFT after N idle minutes / 超过指定分钟未调用自动隐藏对应模型速度与首字 |
| `display.modelSpeedExpireTurns` | `number` (optional) | `30` | Auto-hide main/child model speed & TTFT after N idle turns / 超过指定轮次未调用自动隐藏对应模型速度与首字 |
| `display.cacheHitThresholds` | `object` (`{ excellent?, partial? }`) | `{ excellent: 80, partial: 50 }` | Color thresholds for turn cache & cumulative model cache lines on the bottom Statusline (`>= excellent` bold primary, `>= partial` yellow, `< partial` dim; shallow-merged) / 状态栏底栏单轮缓存与模型累计缓存行颜色阈值（浅合并） |
| `display.showBreadcrumbs` | `boolean` | `false` | Line 3 workspace memory / recently touched files / 第三行工作区记忆或最近文件面包屑 |
| `display.breadcrumbCount` | `number` | `3` | Max breadcrumb files shown (wizard cycles `0, 1, 2, 3, 5`) / 面包屑最多显示文件数 |
| `display.showGitBranch` | `boolean` | `false` | Git branch on Line 1 (hidden in non-Git dirs) / 状态栏首行 Git 分支 |
| `display.showUsername` | `boolean` | `false` | Host stdin `username` / `email` or `display.username` / 显示宿主 `stdin.username` / `stdin.email` 或自定义用户名 |
| `display.username` | `string` | *unset* | Custom username override when `showUsername` is `true` (overrides host `stdin.username` / `stdin.email` and OS username) / 自定义用户名覆写（优先于宿主 `stdin.username` / `stdin.email` 与系统用户名） |
| `display.showDailyCost` | `boolean` | `false` | Today's USD cost / 今日费用 |
| `display.showWeeklyCost` | `boolean` | `false` | 7-day rolling USD cost / 近 7 日费用 |
| `display.showRoutedCost` | `boolean` | `false` | Trust CLI native cost for Bedrock (`anthropic.*` / `us.anthropic.*` / `eu.anthropic.*`) & Vertex (`claude-*@*`) routed models instead of token-rate estimation / Bedrock/Vertex 路由模型是否采信 CLI 原生费用（默认 `false` 改用本地费率估算） |
| `display.columnWidth` | `number` | `40` | Max column width hint / 配额表列宽参考 |
| `display.terminalWidth` | `number` (optional) | *unset (auto)* | Explicit terminal column width cap (`agyData.terminal_width > display.terminalWidth > COLUMNS > TTY`) / 显式终端列宽上限 |
| `icons` | `object` (optional) | *unset* | Custom icon overrides (`branch`, `plan`, `step`, `task`, `model`, max 8 chars, shallow-merged; Dashboard Settings exposes `branch`, while retired `token`/`ctx` are silently ignored) / 自定义图标覆写（浅合并，看板仅开放 `branch`，旧版 `token`/`ctx` 已退役并静默忽略） |
| `thresholds.warning` | `number` (`0`–`1`) | `0.7` | Quota warning threshold (`0.7` yellow; bottom context monitor uses `65%` at default `0.7` or `warning*100%` if customized) / 配额黄色预警阈值（自定义时同步作用于底栏上下文监控） |
| `thresholds.critical` | `number` (`0`–`1`) | `0.9` | Quota critical threshold (`0.9` red; bottom context monitor uses `85%` at default `0.9` or `critical*100%` if customized) / 配额红色告警阈值（自定义时同步作用于底栏上下文监控） |

> **Backward Compatibility / 向后兼容**：Retired keys (`display.showModel`, `display.showTokenBar`, `display.showContextBar`, `display.showTurnCacheHitRate`, `display.showCost`, `icons.token`, `icons.ctx`) in legacy config files are silently ignored during deserialization without errors. / 旧配置文件中残留的已退役字段在反序列化时会被向后兼容地静默忽略，不会报错。

## 3. Write Config / 写入配置文件

Read the existing target file (if any), deep-merge user selections while preserving unrelated keys, and write back to:
- **Local / 局部路径**: `./agy-pulse.config.json`
- **Global / 全局路径**: `~/.gemini/antigravity-cli/agy-pulse-runtime/agy-pulse.config.json` (if a legacy `~/.gemini/antigravity-cli/agy-pulse-runtime/runtime/agy-pulse.config.json` exists, it is still discovered automatically)

Alternatively, the user can open the local Web **Dashboard** (`agypl dashboard open`) and click the top-bar **Settings** panel to visually manage both global **Pulse Config** (with live **Statusline** preview) and **Sync Config**, or run the native `agypl` (or `agy-pulse`) commands directly in the terminal / 用户也可直接打开本地**网页看板**（`agypl dashboard open`）顶栏「设置」面板可视化管理全局**偏好配置**（含实时**状态栏**预览）与**同步配置**，或在终端直接运行原生命令 `agypl`（或 `agy-pulse`）打开交互向导、遥测看板或执行安装升级：
```bash
agypl config                              # or --config
agypl stats [today|24h|7d|30d|month|all]  # or report / --stats
agypl dashboard                           # or dash / ui / --dashboard
agypl sync [init|status|prune-device]     # multi-device object sync / 多机同步
agypl account assign <email> [options]    # assign local unassigned records / 存量记录账号归属
agypl install                             # or --install
agypl uninstall [--purge-ledger]          # or --uninstall
agypl update                              # or --update
agypl help                                # or --help / -h
```

## 4. Multi-Device Sync & Account Assignment Guide / 多机同步与账号归属配置引导

When the user asks to set up multi-device sync, connect a **Sync Remote** (`local-dir`, `webdav`, `s3`, or `cf-worker`), inspect sync state, or assign historical `unassigned` records to an **Account**, guide them to either open the local Web **Dashboard** (`agypl dashboard open`) top-bar **Settings** panel (Sync Config tab, supporting live connection tests and write-only credentials) or run the dedicated CLI commands instead of editing `agy-pulse.config.json`:

当用户需要配置多机同步、连接**同步远端**（`local-dir`、`webdav`、`s3`、`cf-worker`）、查看同步状态或将本机历史未归属（`unassigned`）记录归入指定**账号**时，请引导用户打开本地**网页看板**（`agypl dashboard open`）顶栏「设置」面板可视化配置（支持实时测试连接与凭据只写不回显），或使用以下 CLI 命令，绝不要编辑 `agy-pulse.config.json`：

```bash
agy-pulse sync init                        # interactive setup & probe / 交互式配置远端并执行连通性自检
agy-pulse sync status                      # show device ID, redacted remote & devices / 查看本机标识、脱敏远端与已知设备
agy-pulse sync                             # manual pull + push / 手动执行双向同步
agy-pulse account assign alice@example.com # assign local unassigned records / 收编本机未归属记录（支持 --workspace / --dry-run）
```

> **CRITICAL Security Boundary / 安全红线**：
> - Multi-device sync configuration and credentials (`token`, `password`, `access_key`, `secret_key`) MUST ONLY reside in the global `0600` private file `~/.gemini/antigravity-cli/agy-pulse-sync.json` (or `$AGY_PULSE_SYNC_CONFIG`), or be injected via environment variables (`AGY_PULSE_SYNC_TOKEN`, `AGY_PULSE_SYNC_PASSWORD`, `AGY_PULSE_S3_ACCESS_KEY`, `AGY_PULSE_S3_SECRET_KEY`).
> - **NEVER** write sync configuration, remote URLs, or credentials into workspace `./agy-pulse.config.json` (or global `agy-pulse.config.json`), so secrets can never be accidentally committed to Git.
> - 多机同步配置与密钥（`token`、`password`、`access_key`、`secret_key`）**只能**保存在全局 `0600` 私有文件 `~/.gemini/antigravity-cli/agy-pulse-sync.json`（或 `$AGY_PULSE_SYNC_CONFIG`），或通过环境变量注入；**绝不允许**写入工作区 `./agy-pulse.config.json`（或全局 `agy-pulse.config.json`），防止密钥随项目提交进 Git 仓库。
