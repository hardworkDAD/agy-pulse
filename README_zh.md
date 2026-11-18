# agy-pulse

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](#license)

> **Antigravity CLI (`agy`) 全能单进程状态栏 HUD、实时上下文与压缩上限监控、云端配额追踪插件。**
>
> 零外部 npm 依赖 · 纯 Node.js 单进程引擎 · 原生 SQLite Protobuf 压缩上限解码 · 字节偏移增量日志扫描 · 中英双语 (`zh` / `en`)

[English](./README.md)

---

## 显示效果

`agy-pulse` 将经典 `agy-hud` 精简信息栏与云端配额表，以及 `agy-statusline` 的实时上下文与压缩监控行原生融合进同一个高速 Node.js 进程：

```
my-project │ Google AI Pro │ 输出: 42.6 tok/s
1 MEMORY.md │ 4 rules │ 2 MCPs │ 3 hooks
  ─────────────────────────────────────────────────────────────────────────────────
  Google 5h   [█████░]  87% ~22m   │ Claude 5h   [██████] 100% ~4h59m
  Google week [████░░]  74% ~4d3h  │ Claude week [██████] 100% ~6d23h
◈ Gemini 4 Carbon (High) · 上下文 ████▓······· 38.4% (196.6K/512K) · 压缩×1(峰480K) · 缓存 82% (160K) · 输出 12.4K · 费用 $0.12
```

当上下文占用达到 **85%+** 时，底栏进度条自动变为红色并在末尾追加 **`即将触发压缩`**；在非 UTF-8 终端（如 Windows `cp936`）下自动降级为纯 ASCII 字符（`*`、`#`/`=`/`-`/`.`、`x`、`|`）。

### 配额显示模式（`display.quotaStyle`）
- **`table`**（默认）：双列按厂商聚合配额窗（`Google 5h/week`、`Claude 5h/week` 及可选 `Argon`），配合底部实时上下文监控行。缺失单侧配额窗口时显示 `--` 而非虚假 `100%`。
- **`models`**：双列逐模型配额详情（支持自定义 `columnWidth`）。
- **`compact`**：按 Provider 分组的迷你进度条（`Google:`、`Anthropic:`、`OpenAI:`），全面覆盖 2026 模型且家族缩写零碰撞（`Carbon`、`Hydro`、`Argon`、`Orch-C`、`Orch-A`、`G3.8F` 等）。

---

## 安装与部署

在普通终端（**不要**在已打开的 `agy` 会话内）执行：

### 从本地克隆仓库安装 / 平滑迁移

**macOS / Linux 一键本地部署（自动从旧版 `statusline-combined.sh` 平滑切换至单进程 `agy-pulse.js`）**：
```bash
bash scripts/deploy-local.sh
```

**或完整执行插件注册 + 运行时安装**：
```bash
bash scripts/install.sh
```

### 从自建 GitHub 仓库 / Fork 安装

通过 `AGY_PULSE_REPO` 指定你自己的 `owner/repo`：

**macOS / Linux**：
```bash
AGY_PULSE_REPO="yourname/agy-pulse" \
  bash <(curl -fsSL "https://raw.githubusercontent.com/$AGY_PULSE_REPO/main/scripts/install.sh")
```

**Windows PowerShell**：
```powershell
$env:AGY_PULSE_REPO = 'yourname/agy-pulse'
irm "https://raw.githubusercontent.com/$env:AGY_PULSE_REPO/main/scripts/install.ps1" | iex
```

安装脚本会自动完成：
1. 注册声明式插件（`agy plugin install`）。
2. 将纯 Node.js 运行时原子部署至 `~/.gemini/antigravity-cli/agy-pulse-runtime/`。
3. 将 `~/.gemini/antigravity-cli/settings.json` 的 `statusLine.command` 指向 `runtime/bin/agy-pulse.js`（Windows 下指向 `.cmd` shim），同时保留用户原有的 `enabled` / `stack_with_default` 等字段。

---

## 验证与交互配置

```bash
# 检查 settings.json 中的 statusLine 配置
cat ~/.gemini/antigravity-cli/settings.json | grep statusLine -A3

# 直接手动调用验证输出
node ~/.gemini/antigravity-cli/agy-pulse-runtime/runtime/bin/agy-pulse.js

# 打开交互式配置向导（支持 ↑/↓、j/k、空格/回车切换、s 保存、q 退出）
node ~/.gemini/antigravity-cli/agy-pulse-runtime/runtime/bin/agy-pulse.js --config

# Token 与配额缓存诊断
node scripts/diagnose-auth.js
```

---

## 卸载

```bash
bash uninstall.sh
```

Windows PowerShell：
```powershell
.\uninstall.ps1
```

卸载脚本会同时清理 `agy-pulse` 与旧版 `agy-hud` 的运行时目录、插件目录、缓存、错误日志、Windows shim，并清除 `settings.json` 中的 `statusLine`（保留 `.bak` 备份）。

---

## 配置（可选）

您可以在工作区根目录创建 `agy-pulse.config.json`（向下兼容 `agy-hud.config.json`）覆盖默认配置；若未创建，则使用内置 `runtime/agy-hud.config.json` 并与 `FULL_DEFAULT_CONFIG` 深度合并：

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

### 配置项说明
- **`theme`**：HUD 前景配色（`primary`、`secondary`、`warning`、`critical`），支持标准 ANSI 颜色（`green`、`gray`、`yellow`、`red`、`blue`、`magenta`、`cyan`）。
- **`display`**：
  - `showContextMonitor`：是否显示底部实时上下文与压缩监控行（默认 `true`）。
  - `quotaStyle`：配额样式，可选 `"table"`（默认）、`"models"` 或 `"compact"`。
  - `showCurrentDir`：是否显示当前工作区目录名（默认 `true`）。
  - `showSpeed`：是否在顶栏显示输出速率（`输出: X.X tok/s`，默认 `true`）。
  - `showModel`：是否在第一行显示模型名（默认 `false`，避免与底栏 `◈ <Model (Effort)>` 重复）。
  - `showTokenBar` / `showContextBar`：可选开启第二行经典 Token 明细与上下文条。
  - `showTurnCacheHitRate`：是否在第二行显示单轮缓存命中率徽章。
  - `showImageQuota`：是否在第二行显示生图配额进度条。
  - `showBreadcrumbs` / `breadcrumbCount`：是否在第三行显示最近修改的文件路径。
  - `showGitBranch`：是否在第一行显示 Git 分支（非 Git 目录自动隐藏，不再伪造 `main`）。
  - `showUsername` / `username`：是否显示登录邮箱或自定义用户名。
  - `showCost` / `showDailyCost` / `showWeeklyCost`：是否在第一行显示当前会话、今日及本周费用。
  - `useNerdFonts`：是否启用 Nerd Fonts 图标。
  - `columnWidth`：配额表格每列最大宽度（默认 `40`）。
- **`thresholds`**：配额警戒阈值（`0.7` 黄色预警、`0.9` 红色告警）。
- **`language`**：语言设置，可选 `"auto"`（根据 `LC_ALL`/`LC_CTYPE`/`LANG` 自动判定）、`"en"` 或 `"zh"`。

---

## 环境变量（兼容旧 `AGY_HUD_*` 前缀）

| 主环境变量 | 兼容别名 | 用途 |
|---|---|---|
| `AGY_PULSE_DATA_DIR` | `AGY_HUD_DATA_DIR` | 自定义 Antigravity CLI 数据目录 |
| `AGY_PULSE_REPO` | `AGY_HUD_REPO` | 自定义 `owner/repo` 仓库（用于自更新与远程安装） |
| `AGY_PULSE_REPO_RAW` | `AGY_HUD_REPO_RAW` | 自定义 GitHub Raw 镜像地址 |
| `AGY_PULSE_REPO_URL` | `AGY_HUD_REPO_URL` | 自定义 Git 克隆地址 |
| `AGY_PULSE_UPDATE_URL` | `AGY_HUD_UPDATE_URL` | 自定义版本检查 `package.json` 地址 |
| `AGY_HUD_FORCE_ASCII` | — | 设为 `1` 强制使用 ASCII 字符 |
| `AGY_HUD_FORCE_UNICODE` | — | 设为 `1` 强制使用 Unicode 字符 |

---

## 目录结构

```
agy-pulse/
├── plugin.json                        # {"name":"agy-pulse"}
├── gemini-extension.json              # agy 插件清单元数据 (v1.0.0)
├── runtime/
│   ├── brand.js                       # 品牌常量与远端仓库解析
│   ├── compaction.js                  # 纯 JS Protobuf 解码器与 SQLite 压缩上限缓存
│   ├── bin/
│   │   ├── agy-pulse.js / .cmd        # 主 statusLine 入口
│   │   └── agy-hud.js / .cmd          # 向下兼容入口别名
│   ├── config-wizard.js               # 交互式配置向导 (--config)
│   ├── config.js                      # 深度合并配置加载器与原子写入
│   ├── cost.js                        # 2026 模型费率表与会话费用计算
│   ├── daily-cost.js                  # 并发安全的今日/七日滚动费用账本
│   ├── encoding.js                    # 终端 UTF-8 / 代码页自动检测
│   ├── parser.js                      # 字节偏移增量 transcript 扫描与工作区元数据
│   ├── paths.js                       # 跨平台数据根目录与安全可执行文件定位
│   ├── quota.js                       # SWR 配额主协调器
│   ├── quota/                         # token · cache · cloud · models
│   ├── renderer.js                    # 多行 HUD 渲染主协调器
│   ├── renderer/                      # context-monitor · format · lang · quota-render
│   ├── speed.js                       # 输出 tok/s 速率计算（支持首轮/尾轮锁存）
│   ├── statusline-installer.js        # settings.json 写入与 Windows .cmd/.bat shim
│   ├── uninstall.js                   # agy-pulse 与旧版 agy-hud 统一清理器
│   └── update-checker.js              # Semver 安全的后台版本更新检查
├── scripts/
│   ├── install.sh / install.ps1       # 一键安装脚本
│   ├── bootstrap.sh / bootstrap.js    # 原子化运行时部署工具
│   ├── deploy-local.sh                # 本地部署与旧版双进程配置平滑迁移脚本
│   ├── configure-utf8.ps1             # Windows UTF-8 环境助手
│   ├── verify-display.js              # E2E 渲染验证器
│   └── diagnose-auth.js               # 认证与缓存诊断工具
├── skills/hud-config/SKILL.md         # Agent 交互式配置技能
├── docs/                              # 中英架构与模块参考文档
└── tests/unit/                        # 基于 node:test 的单元测试套件
```

---

## License

MIT
