# agy-pulse

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)

> **Antigravity CLI (`agy`) 全能单进程状态栏、实时上下文与压缩上限监控、永久遥测账本（`stats` / `dashboard`）、云端配额追踪插件。**
>
> 零外部 npm 依赖 · 纯 Node.js 单进程引擎 · 内置 `node:sqlite` 永久账本 · 本地 Web 看板与终端报表 · 原生 SQLite Protobuf 压缩上限解码 · 字节偏移增量日志扫描 · 中英双语 (`zh` / `en`)

[English](./README.md)

---

## 显示效果

`agy-pulse` 将精简信息栏、云端配额表、分模型累计缓存行，以及实时上下文与压缩监控行原生融合进同一个高速 Node.js 进程：

```text
my-project │ Google AI Pro │ plan │ 输出: 42.6 tok/s (首字 0.8s)
  ───────────────────────────────────────────────────────────────────
  Google 5h   [█████░]  87% ~22m   │ Claude 5h   [██████] 100% ~4h59m
  Google week [████░░]  74% ~4d3h  │ Claude week [██████] 100% ~6d23h
⟳ 累计缓存 · Carbon 82% (命中 160K · 写入 4K / 总入 196K)
◈ Gemini 4 Carbon (High) · 上下文 ████▓······· 38.4% (196.6K/512K) · 压缩×1(峰480K) · 缓存 82% (160K) · 输出 12.4K · 费用 $0.12
```

当上下文占用达到 **85%+**（默认值）时，底栏进度条自动变为红色并在末尾追加 **`即将触发压缩`**；在非 UTF-8 终端（如 Windows `cp936`）下自动降级为纯 ASCII 字符（`*`、`#`/`=`/`-`/`.`、`x`、`|`）。

### 配额显示模式（`display.quotaStyle`）
- **`table`**（默认）：双列按厂商聚合配额窗（`Google 5h/week`、`Claude 5h/week` 及可选 `Argon`），配合底部累计缓存行与实时上下文监控行。缺失单侧配额窗口时显示 `--` 而非虚假 `100%`。
- **`models`**：双列逐模型配额详情（支持自定义 `columnWidth`，窄屏自动收敛列宽并在 `< 56` 列时切为单栏）。
- **`compact`**：按 Provider 分组的迷你进度条（`Google:`、`Anthropic:`，OpenAI 模型归并显示在 `Anthropic` 下），全面覆盖 2026 模型且家族缩写零碰撞（`Carbon`、`Hydro`、`Argon`、`Neon`、`Orch-C`、`Orch-A`、`Orch-N` 等）。
- **`none`**：完全关闭配额面板（含加载中/不可用占位框）。

---

## 安装与部署

在普通终端（**不要**在已打开的 `agy` 会话内，因为 `agy` 退出时会用内存状态覆写 `settings.json`）执行：

### 一键远程安装（`hardworkDAD/agy-pulse`）

**macOS / Linux**：
```bash
curl -fsSL https://raw.githubusercontent.com/hardworkDAD/agy-pulse/main/scripts/install.sh | bash
```

**Windows PowerShell**：
```powershell
irm https://raw.githubusercontent.com/hardworkDAD/agy-pulse/main/scripts/install.ps1 | iex
```

### 从本地克隆仓库安装

**macOS / Linux**：
```bash
bash scripts/install.sh
```

**Windows PowerShell**：
```powershell
.\scripts\install.ps1
```

### 从自建 Fork / 镜像源安装

在执行安装命令前覆盖 `AGY_PULSE_REPO`（或 `AGY_PULSE_REPO_RAW` / `AGY_PULSE_REPO_URL`）：

**macOS / Linux**：
```bash
export AGY_PULSE_REPO="yourname/agy-pulse"
curl -fsSL "https://raw.githubusercontent.com/$AGY_PULSE_REPO/main/scripts/install.sh" | bash
```

**Windows PowerShell**：
```powershell
$env:AGY_PULSE_REPO = 'yourname/agy-pulse'
irm "https://raw.githubusercontent.com/$env:AGY_PULSE_REPO/main/scripts/install.ps1" | iex
```

安装脚本会自动完成：
1. 注册声明式插件（`agy plugin install`）。
2. 将纯 Node.js 运行时原子部署至 `~/.gemini/antigravity-cli/agy-pulse-runtime/`（升级时自动保留并深度合并已保存的全局 `agy-pulse.config.json`，向 `scripts/bootstrap.js` 传入 `--reset-config` 可重置）。
3. 将 `~/.gemini/antigravity-cli/settings.json` 的 `statusLine.command` 指向 `runtime/bin/agy-pulse.js`（Windows 下指向 `.cmd` shim），同时保留用户原有的 `enabled` / `stack_with_default` 等字段。
4. 在 `~/.local/bin/`（Windows 为 `%LOCALAPPDATA%\agy\bin\`，可用 `AGY_PULSE_BIN_DIR` 覆盖）自动注册全局终端命令 **`agypl`**（主命令）与 **`agy-pulse`**（兼容别名）。

---

## CLI 命令与参数

`agypl` 与 `agy-pulse` 支持完全一致的子命令与标志：

| 命令 / 调用方式 | 兼容别名 | 用途说明 |
|---|---|---|
| `agypl stats [range] [options]` | `report`, `--stats` | 输出 CLI 与 Antigravity IDE 终端遥测速查报表 |
| `agypl dashboard [options]` | `dash`, `ui`, `--dashboard` | 启动本地离线 Web 看板（默认 `127.0.0.1:3789`） |
| `agypl config` | `--config` | 打开交互式终端配置向导 |
| `agypl diagnose` | `--diagnose` | 输出 JSON 认证与配额缓存诊断信息 |
| `agypl update` | `--update` | 从配置仓库在线检查并自更新运行时 |
| `agypl help` | `--help`, `-h` | 显示 CLI 帮助与当前版本号 |
| `agy-pulse` *(无参数)* | — | 从 `stdin` 读取 `agy` JSON（1.5s 超时）并渲染状态栏 |

### `agypl stats` 参数全表

| 参数 | 可选值 / 格式 | 默认值 | 说明 |
|---|---|---|---|
| 位置时间范围 | `today \| 24h \| 7d \| 30d \| month \| all` | `7d` | 快捷位置参数（如 `agypl stats today`） |
| `--range <val>` | `today \| 24h \| 7d \| 30d \| month \| all \| custom` | `7d` | 显式指定统计时间窗口 |
| `--today`, `--24h`, `--7d`, `--30d`, `--month`, `--all` | 开关标志 | — | 各预设时间范围快捷参数 |
| `--from <date>` / `--to <date>` | `YYYY-MM-DD` 或毫秒时间戳 | *不限* | 自定义起止时间（未显式指定 `--range` 时自动切为 `custom`） |
| `--granularity <val>` / `--by <val>` | `auto \| hour \| day \| week` | `auto` | 时间序列聚合粒度 |
| `--source <val>` | `all \| cli \| ide` | `all` | 按来源筛选（`Antigravity CLI`、`Antigravity IDE` 或全部） |
| `--model <val>` | 模型名或 `all` | `all` | 按模型名称或家族过滤 |
| `--workspace <val>` / `--ws <val>` | 路径、工作区名或 `all` | `all` | 按工作区过滤 |
| `--no-subagents` / `--include-subagents` | 开关标志 | 包含 | 排除或包含 `/swarm` Worker 与子代理会话 |
| `--format <val>` | `table \| json \| csv` | `table` | 输出格式 |
| `--json` | 开关标志 | — | `--format json` 简写 |
| `--csv[=kind]` | `timeseries \| models \| sessions` | `timeseries` | `--format csv` 简写，可选导出表类型 |
| `--csv-kind <val>` / `--kind <val>` | `timeseries \| models \| sessions` | `timeseries` | 指定 CSV 导出数据集 |
| `--lang <val>` | `auto \| en \| zh` | `auto` | 报表输出语言 |
| `--no-sync` | 开关标志 | `false` | 跳过只读 DB 补扫，直接查询现有永久账本 |
| `-h`, `--help` | 开关标志 | — | 显示 `stats` 帮助说明 |

### `agypl dashboard` 参数全表

| 参数 | 可选值 / 格式 | 默认值 | 说明 |
|---|---|---|---|
| `--port <num>`, `-p <num>` | `0`–`65535` | `3789` | 监听回环端口（`0` 随机分配；端口冲突时自动递增回退） |
| `--no-open` / `--open` | 开关标志 | 自动打开 | 禁止或允许启动后自动打开系统浏览器 |
| `--lang <val>`, `--zh`, `--en` | `auto \| en \| zh` | `auto` | 指定 Web 看板初始语言 |
| `--no-sync` | 开关标志 | `false` | 跳过启动时 DB 补扫，直接展示现有账本 |

### 安装 / 部署 / 卸载脚本参数

| 脚本 | 参数 | 说明 |
|---|---|---|
| `node scripts/bootstrap.js` | `--reset-config` | 安装或升级时重置已保存的全局 `agy-pulse.config.json` 为默认值 |
| `node scripts/bootstrap.js` | `--cleanup-self` | 运行结束后自动删除位于系统临时目录 `os.tmpdir()` 内的自身副本 |
| `node scripts/bootstrap.js` | `--delay-start` | 延迟 1 秒启动安装（供自更新进程交接使用） |
| `bash scripts/deploy-local.sh` | `--sync-config` | 本地部署时使用仓库默认配置覆盖全局 `agy-pulse.config.json` |
| `node runtime/uninstall.js` | `--purge-ledger`, `--purge` | 卸载时一并清除永久遥测账本（`agy-pulse-ledger.sqlite` / `.json`） |

### 永久遥测账本与本地 Web 后台（`stats` / `dashboard`）
- **零常驻进程采集（CLI + Antigravity IDE 合并统计）**：状态栏运行时仅对当前活跃 CLI 会话静默执行 15s 节流增量归档（优先使用内置 `node:sqlite` 写入 `agy-pulse-ledger.sqlite`，无 `node:sqlite` 时自动降级 `agy-pulse-ledger.json`）；运行 `stats` 或 `dashboard` 时自动合并只读补扫 Antigravity CLI（`~/.gemini/antigravity-cli/conversations/*.db`）与 Antigravity IDE（`~/.gemini/antigravity/conversations/*.db`）的全部会话库（`?immutable=1` + 活跃 `-wal` 临时拷贝，绝不写入原库，也不影响终端状态栏聚焦当前 CLI）。
- **历史永久保留**：已归档的步级遥测与会话谱系树永久保存于本地账本中，即使后续源 `conversations/*.db` 被清理也不会丢失历史统计。
- **纯离线本地 Web 看板（`dashboard`）**：严格绑定 `127.0.0.1` 并校验 `Host` / `Origin`，内置深/浅色与中/英文一键切换、`All / CLI / IDE` 来源切换与来源费用占比汇总、快捷区间 + 自定义起止日期、小时/天/周粒度、模型/工作区/子代理筛选、6 张 KPI 卡、2 张纯原生交互 SVG 图表、可排序模型分析表、可搜索可展开且带 `CLI` / `IDE` 徽标的 `/swarm` 蜂群树会话明细及 CSV/JSON 导出。

---

## 卸载

```bash
bash uninstall.sh
```

Windows PowerShell：
```powershell
.\uninstall.ps1
```

卸载脚本会清理 `agy-pulse` 的运行时目录、插件目录、缓存、错误日志、Windows shim，并清除 `settings.json` 中的 `statusLine`（保留 `.bak` 备份）。默认**保留**永久历史遥测账本（`agy-pulse-ledger.sqlite` / `agy-pulse-ledger.json`）；如需一并清理，可在执行 `node runtime/uninstall.js` 时传入 `--purge-ledger`（或 `--purge`）。

---

## 配置

可选。您可以在工作区根目录创建 `agy-pulse.config.json` 覆盖默认配置。

### 配置文件路径与合并优先级

1. **工作区局部配置**（最高优先级）：`./agy-pulse.config.json`
2. **全局配置**：`~/.gemini/antigravity-cli/agy-pulse-runtime/runtime/agy-pulse.config.json`（或 `$AGY_PULSE_GLOBAL_CONFIG_DIR/agy-pulse.config.json`）
3. **内置默认值**（`FULL_DEFAULT_CONFIG`，最低优先级）

`runtime/config.js` 会深度合并 `theme`（当覆写为字符串预设/单色名时整体替换）、`display`、`display.cacheHitThresholds`、`icons` 与 `thresholds`。

```json
{
  "enabled": true,
  "theme": {
    "primary": "green",
    "secondary": "gray",
    "warning": "yellow",
    "critical": "red"
  },
  "display": {
    "quotaStyle": "table",
    "showContextMonitor": true,
    "showModelCache": true,
    "showCurrentDir": true,
    "showCycleMode": true,
    "showSpeed": true,
    "showModel": false,
    "showTokenBar": false,
    "showContextBar": false,
    "showTurnCacheHitRate": false,
    "showImageQuota": false,
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

### 顶层配置参数表

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `enabled` | `boolean` | `true` | 总开关；设为 `false` 时状态栏静默退出不输出 |
| `language` | `"auto" \| "en" \| "zh"` | `"auto"` | 界面语言（`"auto"` 自动读取 `LC_ALL` / `LC_CTYPE` / `LANG`） |
| `repository` | `string \| { url?, slug? }` | `package.json#repository`（`https://github.com/hardworkDAD/agy-pulse`） | 用于 `agypl update` 与后台版本检查的 `"owner/repo"` 或 GitHub URL；省略或留空时自动回退至 `package.json#repository` |
| `theme` | `object \| string` | `{ primary: "green", secondary: "gray", warning: "yellow", critical: "red" }` | 支持 4 键配色对象（`green`/`gray`/`yellow`/`red`/`blue`/`magenta`/`cyan`）、4 个字符串主题预设（`"Emerald Green"`、`"Ocean Blue"`、`"Cyberpunk Magenta"`、`"Amber Gold"`），或单色名字符串（覆写 `primary`） |
| `thresholds` | `{ warning, critical }` | `{ warning: 0.7, critical: 0.9 }` | 配额警告/告警比例（`0`–`1`）。底部上下文监控条在保持默认 `0.7 / 0.9` 时实际使用 `65%` 黄色 / `85%` 红色；自定义 `warning` 或 `critical` 时同步以 `warning * 100%` / `critical * 100%` 作用于上下文监控条 |
| `icons` | `object` | *未设置* | 自定义图标覆写（`branch`、`plan`、`step`、`task`、`token`、`ctx`、`model`），最长截断至 8 字符，支持浅层合并 |
| `endpoints` | `string[]` | *内置默认地址* | 高级可选项：覆盖云端配额接口 URL 列表（优先级低于 `AGY_PULSE_ENDPOINTS`） |
| `interestingModels` | `string[]` | *自动探测* | 高级可选项：指定配额表纳入的模型 ID 列表（优先级低于 `AGY_PULSE_INTERESTING_MODELS`） |

### `display.*` 全量参数表

> 说明：若直接调用 `renderStatusLine()` 且未传配置，`showGitBranch`、`showTokenBar`、`showContextBar`、`showBreadcrumbs`、`showModel` 因采用 `!== false` 判断会表现为 `true`；但内置默认配置（`FULL_DEFAULT_CONFIG` 与 `runtime/agy-pulse.config.json`）均显式将其设为下表所示的 `false`。

| 字段 | 类型 | 默认值 | 所属区域 | 说明 |
|---|---|---|---|---|
| `display.quotaStyle` | `"table" \| "models" \| "compact" \| "none"` | `"table"` | 配额面板 | 配额面板显示模式（`"none"` 完全隐藏面板） |
| `display.showContextMonitor` | `boolean` | `true` | 底部监控行 | 是否显示底部实时上下文与压缩监控行 |
| `display.showModelCache` | `boolean` | `true` | 累计缓存行 | 是否在上下文监控行上方显示分模型累计缓存命中行（`showContextMonitor` 为 `true` 时默认开启） |
| `display.showCurrentDir` | `boolean` | `true` | 第 1 行 | 是否显示当前工作区目录名 |
| `display.showCycleMode` | `boolean` | `true` | 第 1 行 | 是否显示 `agy` 交互模式徽章（`auto`、`accept-edits`、`plan`、`best-of-n`，由 `Shift+Tab` 切换） |
| `display.showSpeed` | `boolean` | `true` | 第 1 行 | 是否显示近 30 非空库流式加权平均输出速率与平均首字延迟（`输出: X.X tok/s (首字 Y.Ys)`）及最多 3 个活跃子模型 / `/swarm` 速率 |
| `display.modelSpeedExpireMinutes` | `number` | `5` | 第 1 行 | 主/子模型超过指定闲置分钟数后自动隐藏速度与首字 |
| `display.modelSpeedExpireTurns` | `number` | `30` | 第 1 行 | 主/子模型超过指定闲置轮次后自动隐藏速度与首字 |
| `display.showModel` | `boolean` | `false` | 第 1 行 | 是否在顶栏重复显示模型名（默认关闭，底栏已展示 `◈ <Model (Effort)>`） |
| `display.showTokenBar` | `boolean` | `false` | 第 2 行 | 是否显示经典 Token 明细栏 |
| `display.showContextBar` | `boolean` | `false` | 第 2 行 | 是否显示经典上下文进度条 |
| `display.showTurnCacheHitRate` | `boolean` | `false` | 第 2 行 | 是否显示单轮缓存命中率徽章 |
| `display.cacheHitThresholds` | `{ excellent?, partial? }` | `{ excellent: 80, partial: 50 }` | 第 2 行 | 单轮缓存命中率配色阈值（`>= excellent` 绿色加粗，`>= partial` 黄色，`> 0` 暗黄，`0` 暗灰；支持浅层合并） |
| `display.showImageQuota` | `boolean` | `false` | 第 2 行 | 是否显示生图配额进度条 |
| `display.showBreadcrumbs` | `boolean` | `false` | 第 3 行 | 是否显示最近修改文件路径面包屑 |
| `display.breadcrumbCount` | `number` | `3` | 第 3 行 | 面包屑最多显示文件数（配置向导循环 `0, 1, 2, 3, 5`） |
| `display.showGitBranch` | `boolean` | `false` | 第 1 行 | 是否显示当前 Git 分支（非 Git 目录自动隐藏） |
| `display.showUsername` | `boolean` | `false` | 第 1 行 | 是否显示解析的登录邮箱或自定义 `display.username` |
| `display.username` | `string` | *未设置* | 第 1 行 | `showUsername` 开启时优先显示的自定义用户名 |
| `display.showCost` | `boolean` | `false` | 第 1 行 | 是否在顶栏显示当前会话美元费用（底栏监控默认已含） |
| `display.showDailyCost` | `boolean` | `false` | 第 1 行 | 是否显示今日累计费用（`daily-cost.json`） |
| `display.showWeeklyCost` | `boolean` | `false` | 第 1 行 | 是否显示近 7 日滚动费用 |
| `display.showRoutedCost` | `boolean` | `false` | 费用计算 | 针对 Bedrock（`anthropic.*` / `us.anthropic.*` / `eu.anthropic.*`）与 Vertex（`claude-*@*`）路由模型：`false` 忽略 CLI 原生费用改用本地 token 费率估算；`true` 采信路由上报的原生费用 |
| `display.useNerdFonts` | `boolean` | `false` | 图标 | 是否启用 Nerd Fonts 图标 |
| `display.unicode` | `boolean` | *省略（自动检测）* | 字符集 | 省略时自动检测终端编码；`true` 强制 Unicode，`false` 强制纯 ASCII |
| `display.columnWidth` | `number` | `40` | 配额面板 | `table` / `models` 布局每列最大宽度 |
| `display.terminalWidth` | `number` | *未设置（自动）* | 响应式布局 | 显式终端列宽上限；优先级：`agyData.terminal_width > display.terminalWidth > COLUMNS > TTY` |

---

## 环境变量

### 路径与安装目录
| 环境变量 | 用途 |
|---|---|
| `AGY_PULSE_DATA_DIR` | 自定义 Antigravity CLI 数据目录（默认 `~/.gemini/antigravity-cli`） |
| `AGY_PULSE_IDE_DATA_DIR` | 自定义 Antigravity IDE 数据目录（默认 `~/.gemini/antigravity`） |
| `AGY_PULSE_GLOBAL_CONFIG_DIR` | 覆盖全局 `agy-pulse.config.json` 所在目录（默认 `runtime/`） |
| `AGY_PULSE_BIN_DIR` | 自定义全局 CLI 命令注册目录（Unix 默认 `~/.local/bin`，Windows 默认 `%LOCALAPPDATA%\agy\bin`） |
| `AGY_PULSE_NODE` | 为旧版 `statusline-combined.sh` 垫片指定自定义 Node 可执行文件路径 |
| `AGY_PULSE_PROFILE_PATH` | 覆盖 `scripts/configure-utf8.ps1` 写入的 PowerShell Profile 路径 |
| `AGY_BIN` | 为 `scripts/diagnose-auth.js` 与 `scripts/verify-display.js` 指定自定义 `agy` 可执行文件路径 |

### 仓库绑定、自更新与引导安装
| 环境变量 | 用途 |
|---|---|
| `AGY_PULSE_REPO` | 自定义 `owner/repo` 仓库（用于远程安装、bootstrap 与 `--update`，默认 `hardworkDAD/agy-pulse`） |
| `AGY_PULSE_REPO_RAW` | 自定义 GitHub Raw 镜像地址 |
| `AGY_PULSE_REPO_URL` | 自定义 `agy plugin install` 使用的 Git 克隆地址 |
| `AGY_PULSE_BOOTSTRAP_URL` | 自定义 `agypl update` 下载的 `bootstrap.js` 地址 |
| `AGY_PULSE_UPDATE_URL` | 自定义后台版本检查请求的 `package.json` 地址 |
| `AGY_PULSE_SETUP_SOURCE_DIR` | 指定 `scripts/bootstrap.js` 使用的本地源码目录 |
| `AGY_PULSE_SETUP_SOURCE_BASE` | 指定 `scripts/bootstrap.js` 使用的远端源码基础 URL |

### 配额、显示、语言与运行时
| 环境变量 | 用途 |
|---|---|
| `AGY_PULSE_ENDPOINTS` | 逗号分隔的云端配额接口端点列表 |
| `AGY_PULSE_INTERESTING_MODELS` | 逗号分隔的关注模型 ID 列表 |
| `AGY_PULSE_FORCE_ASCII` | 设为 `1` 强制使用 ASCII 字符 |
| `AGY_PULSE_FORCE_UNICODE` | 设为 `1` 强制使用 Unicode 字符 |
| `AGY_PULSE_NO_OPEN` | 设为 `1` 时阻止 `agypl dashboard` 自动打开浏览器 |
| `NODE_USE_ENV_PROXY` | 后台配额刷新子进程默认注入 `'1'`，允许透传系统/环境代理（可显式覆盖） |
| `COLUMNS` | 未传入 `agyData.terminal_width` 且未配置 `display.terminalWidth` 时的终端列宽兜底 |
| `NO_COLOR` | 禁用 `agypl stats` 终端 ANSI 彩色输出 |
| `LC_ALL` / `LC_CTYPE` / `LANG` | 用于自动检测终端 UTF-8 支持，以及在 `language: "auto"` 时判定 `zh`/`en` |

---

## 本地开发与旧版迁移

如需在本地开发运行时，或将旧版双进程 `statusline-combined.sh` 平滑迁移至单进程 `agy-pulse.js`（默认保留现有配置，传入 `--sync-config` 可重置覆盖）：

```bash
bash scripts/deploy-local.sh
```

`deploy-local.sh` 会同步运行时文件至 `~/.gemini/antigravity-cli/agy-pulse-runtime/`，将残留的 `statusline-combined.sh` 升级为单进程 `exec` 转发垫片，更新主/次级 `settings.json`（JSON 损坏时自动备份 `.bak`），清理旧版插件残留并同步 `skills/pulse-config/SKILL.md`。

---

## 目录结构

```
agy-pulse/
├── LICENSE                            # MIT 许可证与致谢
├── package.json                       # 包清单、仓库绑定与测试/CLI 脚本
├── plugin.json                        # {"name":"agy-pulse"}
├── gemini-extension.json              # agy 插件清单元数据 (v1.0.0)
├── runtime/
│   ├── brand.js                       # 品牌常量与远端仓库解析
│   ├── compaction.js                  # 纯 JS Protobuf 解码器、SQLite 压缩上限、近 30 库性能与会话树分模型成本缓存
│   ├── bin/
│   │   └── agy-pulse.js / .cmd        # statusLine 与 CLI 入口
│   ├── config-wizard.js               # 交互式配置向导 (--config)
│   ├── config.js                      # 深度合并配置加载器与原子写入
│   ├── cost.js                        # 对标 ACV 费率表（含 >200k 阶梯价）与统一会话总费用计算
│   ├── daily-cost.js                  # 并发安全的今日/七日滚动费用账本
│   ├── dashboard.js                   # 本地 127.0.0.1 HTTP 遥测后台服务 (--dashboard)
│   ├── dashboard-ui.js                # 零外链中英双语深浅色原生 SVG Web 看板
│   ├── encoding.js                    # 终端 UTF-8 / 代码页自动检测
│   ├── ledger.js                      # 永久 node:sqlite/JSON 遥测账本与全量只读 DB 归档扫描
│   ├── parser.js                      # 字节偏移增量 transcript 扫描与工作区元数据
│   ├── paths.js                       # 跨平台数据根目录与安全可执行文件定位
│   ├── quota.js                       # SWR 配额主协调器
│   ├── quota/                         # token · cache · cloud · models
│   ├── renderer.js                    # 多行状态栏渲染器
│   ├── renderer/                      # context-monitor · format · lang · quota-render
│   ├── speed.js                       # 输出 tok/s 与首字延迟 TTFT 格式化（支持首轮/尾轮锁存）
│   ├── stats.js                       # CLI 遥测报表、ASCII 趋势图与 CSV/JSON 导出 (--stats)
│   ├── statusline-installer.js        # settings.json 写入与 Windows .cmd/.bat shim
│   ├── uninstall.js                   # agy-pulse 统一清理器
│   └── update-checker.js              # Semver 安全的后台版本更新检查
├── scripts/
│   ├── install.sh / install.ps1       # 一键安装脚本
│   ├── bootstrap.sh / bootstrap.js    # 原子化运行时部署工具
│   ├── deploy-local.sh                # 本地部署与旧版双进程配置平滑迁移脚本
│   ├── configure-utf8.ps1             # Windows UTF-8 环境助手
│   ├── verify-display.js              # E2E 渲染验证器
│   └── diagnose-auth.js               # 认证与缓存诊断工具
├── skills/pulse-config/SKILL.md       # Agent 交互式配置技能
├── docs/                              # 中英架构与模块参考文档
└── tests/unit/                        # 基于 node:test 的单元测试套件
```

---

## 致谢

`agy-pulse` 在设计理念与功能实现上参考并致谢以下开源项目：
- [`icebear0828/agy-hud`](https://github.com/icebear0828/agy-hud) — Antigravity CLI 多行状态栏 HUD 与云端配额追踪思路
- [`zzusec/agy-statusline`](https://github.com/zzusec/agy-statusline) — 实时上下文窗口、Protobuf 服务端压缩上限解码与费用监控思路
- [`elviscjn-cjn/antigravity-context-viewer`](https://github.com/elviscjn-cjn/antigravity-context-viewer) — `gen_metadata` Protobuf `timeToFirstToken` / `streamingDuration` 解码与分模型生成性能统计思路

---

## License

[MIT](./LICENSE)
