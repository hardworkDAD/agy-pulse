# agy-pulse 完整参考手册

[English](./reference.md) · [返回主页](../README_zh.md) · [文档索引](./README.md)

本文档提供 `agy-pulse` v2.0.0 完整的 CLI 命令与参数全表、统一 SQLite WAL **账本**与本地**网页看板**机制、全量配置项、环境变量、自建 Fork 安装、本地开发与完整源码目录结构。

---

## 目录

- [配额显示模式（`display.quotaStyle`）](#配额显示模式displayquotastyle)
- [安装细节与自建 Fork / 镜像源](#安装细节与自建-fork--镜像源)
- [CLI 命令与参数全表](#cli-命令与参数全表)
  - [主命令速查](#主命令速查)
  - [`agypl stats` 参数全表](#agypl-stats-参数全表)
  - [`agypl dashboard` 子命令与参数全表](#agypl-dashboard-子命令与参数全表)
  - [`agypl sync` 子命令与参数全表](#agypl-sync-子命令与参数全表)
  - [`agypl account` 子命令与参数全表](#agypl-account-子命令与参数全表)
  - [`agypl install` / `uninstall` / `update` 参数](#agypl-install--uninstall--update-参数)
- [永久遥测账本与本地网页看板（`stats` / `dashboard`）](#永久遥测账本与本地网页看板stats--dashboard)
  - [网页看板 HTTP API](#网页看板-http-api)
- [卸载机制说明](#卸载机制说明)
- [完整配置项参考](#完整配置项参考)
  - [配置文件路径与合并优先级](#配置文件路径与合并优先级)
  - [完整默认配置示例](#完整默认配置示例)
  - [顶层配置参数表](#顶层配置参数表)
  - [`display.*` 全量参数表](#display-全量参数表)
- [多机同步私有配置（`agy-pulse-sync.json`）](#多机同步私有配置agy-pulse-syncjson)
  - [顶层同步配置字段表](#顶层同步配置字段表)
  - [4 种同步远端配置说明与示例](#4-种同步远端配置说明与示例)
- [环境变量全表](#环境变量全表)
- [本地开发与 v1 平滑升级](#本地开发与-v1-平滑升级)
- [完整目录结构](#完整目录结构)

---

## 配额显示模式（`display.quotaStyle`）

**模型配额**数据严格且唯一地来自宿主通过 `stdin` 合法传入的 JSON（遵循 ADR-0002，零场外反重力云端请求）：

- **`table`**（默认）：双列按厂商聚合配额窗（`Google 5h/week`、`Claude 5h/week` 及可选 `Argon`），配合底部累计缓存行与实时**上下文压缩**监控行。缺失单侧配额窗口时显示 `--` 而非虚假 `100%`。旧版配置残留 `"models"` 时自动平滑回退为 `"table"`。
- **`compact`**：单行紧凑厂商临界窗口剩余比例摘要（`Google: X% │ Claude: Y% │ Argon: Z%`）。
- **`none`**：完全关闭配额面板。

---

## 安装细节与自建 Fork / 镜像源

在普通终端（**不要**在已打开的 `agy` 会话内，因为 `agy` 退出时会用内存状态覆写 `settings.json`）执行。

### 安装脚本自动完成的工作

1. 自动识别当前操作系统与 CPU 架构（`linux-x86_64`、`linux-aarch64`、`macos-x86_64`、`macos-aarch64` 或 `windows-x86_64`），下载对应预编译压缩包与 `SHA256SUMS` 并执行 SHA-256 强制校验。
2. 将原生二进制 **`agy-pulse`** 与 **`agypl`** 部署至 `~/.local/bin/`（Windows 为 `%LOCALAPPDATA%\agy-pulse\bin\`，可通过 `AGY_PULSE_BIN_DIR` 覆盖）。
3. 调用 `agy-pulse install` 将 `~/.gemini/antigravity-cli/settings.json` 的 `statusLine.command` 指向原生 `agy-pulse` 二进制（保留原有 `enabled` / `stack_with_default` 字段，JSON 损坏时自动备份 `.bak`），并清理旧版 `v1` 插件残留。
4. 若本机装有 `agy`，同步调用 `agy plugin install` 注册静态插件声明（`plugin.json`、`gemini-extension.json`、`skills/pulse-config/SKILL.md`）。

### 一键预编译安装

```bash
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/hardworkDAD/agy-pulse/main/scripts/install.sh | bash
```

```powershell
# Windows x64 PowerShell
irm https://raw.githubusercontent.com/hardworkDAD/agy-pulse/main/scripts/install.ps1 | iex
```

### 从源码安装（`cargo install` 或本地克隆）

```bash
cargo install --git https://github.com/hardworkDAD/agy-pulse
agy-pulse install
```

本地克隆仓库执行：

```bash
bash scripts/deploy-local.sh
```

### 从自建 Fork / 镜像源安装

在执行安装命令前覆盖 `AGY_PULSE_REPO` 或 `AGY_PULSE_RELEASE_BASE_URL`：

```bash
export AGY_PULSE_REPO="yourname/agy-pulse"
curl -fsSL "https://raw.githubusercontent.com/$AGY_PULSE_REPO/main/scripts/install.sh" | bash
```

```powershell
$env:AGY_PULSE_REPO = 'yourname/agy-pulse'
irm "https://raw.githubusercontent.com/$env:AGY_PULSE_REPO/main/scripts/install.ps1" | iex
```

---

## CLI 命令与参数全表

`agypl` 与 `agy-pulse` 支持完全一致的子命令与标志：

### 主命令速查

| 命令 / 调用方式 | 兼容别名 | 用途说明 |
|---|---|---|
| `agypl stats [range] [options]` | `report`, `--stats` | 输出 CLI 与 Antigravity IDE 终端遥测速查报表 |
| `agypl dashboard [action] [options]` | `dash`, `ui`, `--dashboard` | 启动或管理本地离线**网页看板**（默认 `127.0.0.1:3789`，支持桌面图标与长效保活） |
| `agypl sync [subcommand] [options]` | — | 与已激活的**同步远端**同步**账本**分片与**配额快照** |
| `agypl account assign <email> [options]` | — | 将本机 `unassigned` 存量**单窗对话**与步骤归属到指定邮箱 |
| `agypl config` | `--config` | 打开交互式终端配置向导（支持实时**状态栏**预览与原子写盘） |
| `agypl install [--bin-dir <dir>]` | `--install` | 部署原生 `agy-pulse` / `agypl` 二进制并向宿主 `settings.json` 注册 `statusLine` |
| `agypl uninstall [--purge-ledger]` | `--uninstall` | 安全移除 `statusLine`、二进制、快捷方式与缓存（默认保留**账本**） |
| `agypl update [--force]` | `--update` | 检查 GitHub Releases 最新版本，校验 `SHA256SUMS` 并原子自替换 |
| `agypl help` | `--help`, `-h` | 显示 CLI 帮助与版本号（`--version` / `-V` / `-v` / `version`） |
| `agy-pulse` *(无参数)* | — | 从 `stdin` 读取 `agy` JSON（1500 ms 超时）并渲染**状态栏** |

### `agypl stats` 参数全表

| 参数 | 可选值 / 格式 | 默认值 | 说明 |
|---|---|---|---|
| 位置时间范围 | `today \| 24h \| 7d \| 30d \| month \| all` | `7d` | 快捷位置参数（如 `agypl stats today`） |
| `--range <val>` | `today \| 24h \| 7d \| 30d \| month \| all \| custom` | `7d` | 显式指定统计时间窗口 |
| `--today`, `--24h`, `--7d`, `--30d`, `--month`, `--all` | 开关标志 | — | 各预设时间范围快捷参数 |
| `--from <date>` / `--to <date>` | `YYYY-MM-DD` 或毫秒时间戳 | *不限* | 自定义起止时间（未显式指定 `--range` 时自动切为 `custom`） |
| `--granularity <val>` / `--by <val>` | `auto \| hour \| day \| week` | `auto` | 时间序列聚合粒度 |
| `--source <val>` | `all \| cli \| ide` | `all` | 按来源筛选（`cli`、`ide` 或全部） |
| `--model <val>` | 模型名或 `all` | `all` | 按模型名称或家族过滤 |
| `--workspace <val>` / `--ws <val>` | 路径、工作区名或 `all` | `all` | 按工作区过滤 |
| `--no-subagents` / `--include-subagents` | 开关标志 | 包含 | 排除或包含 `/swarm` Worker 与子代理**单窗对话** |
| `--format <val>` | `table \| json \| csv` | `table` | 输出格式 |
| `--json` | 开关标志 | — | `--format json` 简写 |
| `--csv[=kind]` | `timeseries \| models \| sessions` | `timeseries` | `--format csv` 简写，可选导出表类型 |
| `--csv-kind <val>` / `--kind <val>` | `timeseries \| models \| sessions` | `timeseries` | 指定 CSV 导出数据集 |
| `--lang <val>` | `auto \| en \| zh` | `auto` | 报表输出语言 |
| `--no-sync` | 开关标志 | `false` | 跳过只读 DB 补扫，直接查询现有永久**账本** |
| `-h`, `--help` | 开关标志 | — | 显示 `stats` 帮助说明 |

### `agypl dashboard` 子命令与参数全表

| 子命令 / 参数 | 可选值 / 格式 | 默认值 | 说明 |
|---|---|---|---|
| *(无子命令)* | 前台服务 | `serve` | 前台启动本地**网页看板**（`127.0.0.1:3789`）并打开浏览器 |
| `open` | 子动作 | — | 复用或静默拉起后台服务，并以独立 App 窗口打开 |
| `stop` | 子动作 | — | 优雅停止正在运行的后台服务并清理状态文件 |
| `status` | 子动作 | — | 查看后台服务运行状态（`url`、`pid`）及桌面快捷方式安装状态 |
| `install-shortcut`（`install-app`） | 子动作 | — | 安装跨平台桌面/应用启动器图标，支持免终端直接点开 |
| `uninstall-shortcut`（`uninstall-app`） | 子动作 | — | 仅卸载带 `agy-pulse` 指纹标记的桌面快捷方式 |
| `--port <num>`, `-p <num>` | `0`–`65535` | `3789` | 监听回环端口（`0` 随机分配；端口冲突时自动递增回退） |
| `--no-open` / `--open` | 开关标志 | 自动打开 | 禁止或允许启动后自动打开窗口/浏览器 |
| `--app` / `--browser` | 开关标志 | `open` 默认 `--app`，`serve` 默认 `--browser` | 使用 Chromium 系独立无地址栏 App 窗口或系统默认浏览器打开 |
| `--background` | 开关标志 | `false` | 启用后台守护模式与页面生命周期自动退出 |
| `--idle-timeout <sec>` | 非负整数（秒） | `600`（`open`/`--background`），`0`（`serve`） | 无页面连接时的启动/孤儿超时秒数 |
| `--desktop` | 开关标志 | `false` | 执行 `install-shortcut` 时同步在桌面生成快捷方式副本 |
| `--lang <val>`, `--zh`, `--en` | `auto \| en \| zh` | `auto` | 指定**网页看板**初始语言 |
| `--no-sync` | 开关标志 | `false` | 跳过启动时 DB 补扫，直接展示现有**账本** |

### `agypl sync` 子命令与参数全表

| 命令 / 参数 | 可选值 / 格式 | 默认值 | 说明 |
|---|---|---|---|
| `agypl sync` | 双向同步 | 先 Pull 后 Push | 拉取远端其他**设备**分片与**配额快照**，再推送本机增量变更 |
| `agypl sync --push` | 开关标志 | `false` | 仅推送本机数据至**同步远端**（不可与 `--pull` 同用） |
| `agypl sync --pull` | 开关标志 | `false` | 仅拉取远端**设备**数据至本机**账本**（不可与 `--push` 同用） |
| `agypl sync --dry-run` | 开关标志 | `false` | 仅预览待上传/下载分片与清单，不实际修改远端或本机**账本** |
| `agypl sync --no-local-scan` | 开关标志 | `false` | 同步前跳过扫描本机 `conversations/*.db` |
| `agypl sync status` | 子命令 | — | 输出本机 `device_id`、`device_name`、最近观测邮箱、脱敏远端目标、Push/Pull 时间、错误及已知**设备** |
| `agypl sync init [options]` | 子命令 | 交互引导 | 初始化 `~/.gemini/antigravity-cli/agy-pulse-sync.json`（`0600`）并执行 `probe()` 自检（传 `--skip-probe` 可跳过） |
| `--type <backend>` | `local-dir \| webdav \| s3 \| cf-worker` | *交互选择* | 指定**同步远端**后端类型（`init`） |
| `--path <dir>` | 目录路径 | *交互输入* | `local-dir` 本地同步目录路径（`init`） |
| `--url <url>` | HTTPS URL（或回环 HTTP） | *交互输入* | `webdav` 或 `cf-worker` 远端 URL（`init`） |
| `--endpoint <url>` | HTTPS URL | *交互输入* | `s3` 兼容对象存储 Endpoint URL（`init`） |
| `--bucket <name>` | 字符串 | *交互输入* | `s3` 存储桶名称（`init`） |
| `--region <region>` | 字符串 | `auto` | `s3` 签名区域（`init`） |
| `--username <user>` | 字符串 | *未设置* | WebDAV Basic Auth 用户名（`init`） |
| `--password <pass>` | 字符串 | *未设置* | WebDAV Basic Auth 密码（`init`） |
| `--token <token>` | 字符串 | *未设置* | `webdav` 或 `cf-worker` Bearer 令牌（`init`） |
| `--access-key <ak>` | 字符串 | *未设置* | `s3` Access Key ID（`init`） |
| `--secret-key <sk>` | 字符串 | *未设置* | `s3` Secret Access Key（`init`） |
| `--path-style <true\|false>` | 布尔值 | `true` | `s3` 是否使用 Path-Style 寻址（`init`） |
| `--allow-insecure-http` | 开关标志（或 `=true\|false`） | `false` | 允许非回环 `http://` 地址（用于内网 WebDAV/MinIO，`init`） |
| `--device-name <name>` | 字符串 | 操作系统主机名 | 自定义本机展示名称（`init`） |
| `--default-account <email>` | 规范化邮箱 | *未设置* | 当 IDE 或历史记录无邮箱时的默认回退**账号**（`init`） |
| `--prefix <prefix>` | 对象前缀 | `agy-pulse-sync` | 远端对象键根前缀（`init`） |
| `--push-interval <sec>` | 整数 `>= 1` | `300` | 状态栏后台自动 Push 最小节流间隔秒数（`init`） |
| `--skip-probe` | 开关标志 | `false` | 保存配置前跳过 `probe()` 连通性检测（`init`） |
| `agypl sync prune-device <device_id>` | 子命令 | — | 删除指定 `<device_id>` 的远端对象与本机 SQLite 缓存行 |
| `--force` | 开关标志 | `false` | 允许清理当前本机**设备**在远端的数据（`prune-device`） |
| `--dry-run` | 开关标志 | `false` | 仅预览将被删除的对象清单而不实际删除（`prune-device`） |

### `agypl account` 子命令与参数全表

| 命令 / 参数 | 可选值 / 格式 | 默认值 | 说明 |
|---|---|---|---|
| `agypl account assign <email>` | 子命令 | — | 将本机 `unassigned` **单窗对话**与步骤重新归属到规范化 `<email>`（不改动已归属或远端**设备**记录） |
| `--all-unassigned` | 开关标志 | 默认行为 | 归属本机全部 `unassigned` **单窗对话**（不可与 `--workspace` 联用） |
| `--workspace <substr>` | 子串 | *未设置* | 仅归属 `workspace` 包含 `<substr>` 的本机 `unassigned` **单窗对话** |
| `--dry-run` | 开关标志 | `false` | 仅预览匹配的**单窗对话**与步骤数量，不写入数据库 |
| `-h`, `--help` | 开关标志 | — | 显示 `account` 或 `assign` 帮助说明 |

### `agypl install` / `uninstall` / `update` 参数

| 命令 | 参数 | 说明 |
|---|---|---|
| `agypl install` | `--bin-dir <dir>` | 覆盖二进制安装目录（默认 `~/.local/bin` 或 `%LOCALAPPDATA%\agy-pulse\bin`） |
| `agypl uninstall` | `--purge-ledger`, `--purge` | 一并删除永久遥测**账本** `agy-pulse-ledger.sqlite`（默认保留） |
| `agypl uninstall` | `--bin-dir <dir>` | 指定待清理的自定义二进制目录 |
| `agypl update` | `--force` | 即使远端版本未高于本地也强制校验并重新安装 |
| `bash scripts/deploy-local.sh` | `--offline`, `--bin-dir=<dir>` | 本地源码编译 Release（`cargo build --release`）并完成部署与冒烟自检 |

---

## 永久遥测账本与本地网页看板（`stats` / `dashboard`）

- **统一 SQLite WAL 账本（`agy-pulse-ledger.sqlite`）**：内嵌静态编译 `rusqlite`（启用 WAL 模式与忙等待超时），集中保存 CLI 与 IDE 双源**单窗对话**步级遥测、**任务树（Cascade）**父子血缘以及防**上下文压缩**倒扣的今日/7日滚动花费（文件权限 `0600`）。
- **无锁双源采集**：**状态栏**热路径增量同步当前活跃**单窗对话**；执行 `stats` 或 `dashboard` 时自动以只读模式（`?immutable=1` 或活跃 WAL 临时副本）扫描 `~/.gemini/antigravity-cli/conversations/*.db` 与 `~/.gemini/antigravity/conversations/*.db`。
- **长效在线网页看板与内置「设置」抽屉**：严格绑定 `127.0.0.1` 并校验 `Host` / `Origin` 与 CSP 头；前端页面通过 `/api/keepalive` SSE 长连接 + `/api/ping` 租约 + `pagehide` `sendBeacon` `/api/disconnect` 维持在线状态，窗口最小化或切至后台标签页永不断连，仅在所有打开页面真正关闭并度过刷新宽限窗口后优雅退出。顶栏内置「设置」抽屉，可图形化管理全局**偏好配置**（含实时 Rust **状态栏**预览与全部高级选项）与全局 `0600` **同步配置**（支持全部 4 种**同步远端**初始化、只写凭据编辑、实时连通性测试与安全移除），与 `agypl config` / `agypl sync init` 共用同一底层配置文件。

### 网页看板 HTTP API

所有接口均校验 `Host`（`POST` 路由同时校验 `Origin`），仅允许本机回环浏览器访问：

- `GET /api/overview`：查询**账本**聚合概览 JSON。支持 `range`、`from`、`to`、`granularity`、`source`、`model`、`workspace`、`includeSubagents` 及以下多机筛选参数：
  - `account`：`all`（默认）| `<email>` | `unassigned`
  - `device`：`all`（默认）| `<device_id>` | `local`
  - 返回结构包含 `meta.account`、`meta.device`、`meta.localDeviceId`、`meta.availableAccounts`、`meta.availableDevices`、`byAccount`、`byDevice`、`accountQuotas`、`syncStatus` 以及 `sessions[].accountId` / `deviceId` / `deviceName`。
- `GET /api/export.json` 与 `GET /api/export.csv`：同样支持 `account` 与 `device` 过滤参数（`export.csv` 另支持 `kind=timeseries|models|sessions`）。
- `POST /api/sync-remote`：非阻塞按需触发远端双向同步（执行 `pull` + `push`），服务端默认带 30 秒防连点节流。传入查询参数 `?force=1`（或 `?force=true`）或 JSON 请求体 `{"force": true}` 可绕过节流立即执行；返回 `{ ok, enabled, changed, throttled, inProgress, pulledChunks, pushedChunks, stepsImported, error?, syncStatus }`。

#### 本地配置管理接口（`/api/config/*`）

全部 `/api/config/*` 路由强制执行以下安全边界：
- **仅本机回环 + 进程级 CSRF Token**：严格绑定 `127.0.0.1`；服务端启动生成 32 字节（64 位十六进制）随机 Token 并注入 `GET /` 页面，所有 `/api/config/*` 请求必须携带 `X-Agy-Pulse-Token` 请求头（恒定时间比对，缺失或不匹配返回 `403 Forbidden`）。
- **严格同源与 JSON Content-Type**：若存在 `Sec-Fetch-Site` 须为 `same-origin`，若存在 `Origin` 须为同端口回环来源（否则 `403`）；`PUT`/`POST`/`DELETE` 请求体须为 `Content-Type: application/json`（否则 `400`）。
- **凭据只写不回显与乐观并发锁**：**同步配置**凭据绝不经 HTTP 回显明文，环境变量覆盖项绝不反写落盘；`PUT`/`DELETE` 须传 `expectedVersion`（文件 SHA-256 摘要或 `"absent"`），在独立 `Mutex` 锁内比对不一致时返回 `409 Conflict`（附最新 `currentVersion`）。

接口清单：
- `GET /api/config/pulse`：读取全局 `agy-pulse.config.json` 原始快照（绝不读取工作区文件、不将环境变量写回配置），返回 `version`、`defaults`、`themePresets`、`allowedColors`、`envOverrides` 元数据及当前 `runtime` 端口与空闲超时。
- `PUT /api/config/pulse`：校验并原子保存全局**偏好配置**（深合并并保留 `columnWidth` 等未知字段与用户自定义键，置空可选字段自动移除），返回新 `version`、`config`、`envOverrides`、`restartRequired` 与 `resolvedLanguage`。
- `POST /api/config/pulse/preview`：以草稿 `config`（及可选 `terminalCols`）直接调用 Rust `render_statusline` 引擎输出实时 ANSI **状态栏**预览（零落盘）。
- `GET /api/config/sync`：读取全局 `0600` `agy-pulse-sync.json` 并转为 camelCase 脱敏 DTO（含 `localDeviceId`、`envOverrides` 变量名及 `secrets.*` `{ configured, overriddenByEnv }` 元数据，零明文密钥）。
- `PUT /api/config/sync`：校验并原子写入 `0600` `agy-pulse-sync.json`，凭据字段使用只写指令（`{ "mode": "keep" | "set" | "clear" }`，仅在远端类型未变时继承磁盘原密钥），保存后立即刷新看板运行时身份提示（`defaultAccount` / `deviceName`）。
- `POST /api/config/sync/test`：在内存中合并草稿、同类型 `"keep"` 磁盘密钥与环境变量覆盖，执行**同步远端**实时连通性探测（零落盘）；任何异常均经 `redact_sync_error_message` 脱敏后返回。
- `DELETE /api/config/sync`：按 `expectedVersion` 乐观锁删除 `agy-pulse-sync.json` 并清空运行时身份提示，绝不触碰 SQLite **账本**或远端数据。

---

## 卸载机制说明

```bash
bash uninstall.sh        # macOS / Linux
```

```powershell
.\uninstall.ps1          # Windows PowerShell
```

或直接执行 `agypl uninstall`。将安全移除 `agy-pulse` 与 `agypl` 二进制、`settings.json` 中的 `statusLine`（保留 `.bak` 备份）、指纹校验的桌面快捷方式、各类缓存日志以及宿主插件目录；永久**账本**（`agy-pulse-ledger.sqlite`）**默认保留**，传入 `--purge-ledger` 可一并删除。

---

## 完整配置项参考

### 配置文件路径与合并优先级

1. **环境变量覆盖**：`AGY_PULSE_LANGUAGE`、`AGY_PULSE_DASHBOARD_PORT`、`AGY_PULSE_DASHBOARD_IDLE_SEC`、`AGY_PULSE_FORCE_ASCII` / `AGY_PULSE_FORCE_UNICODE`（在**网页看板**「设置」抽屉中仅作生效来源徽章提示，绝不反写落盘）
2. **工作区局部配置**（最高文件优先级）：`./agy-pulse.config.json`（**状态栏**运行期仍优先于全局配置；**网页看板**设置抽屉绝不触碰或修改任何工作区文件）
3. **全局配置**：`~/.gemini/antigravity-cli/agy-pulse-runtime/agy-pulse.config.json`（兼容自动发现旧路径 `agy-pulse-runtime/runtime/agy-pulse.config.json`，或 `$AGY_PULSE_GLOBAL_CONFIG_DIR/agy-pulse.config.json`；`agypl config` 与**网页看板**「设置」抽屉共同读写此文件）
4. **内置默认值**（`DisplayConfig::builtin_defaults()`，最低优先级）

反序列化对旧版已退役字段（`endpoints`、`interestingModels`、`cloudQuota`、`repository`）做静默忽略；`"quotaStyle": "models"` 自动平滑回退为 `"table"`。

### 完整默认配置示例

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
    "useNerdFonts": false,
    "columnWidth": 40
  },
  "thresholds": {
    "warning": 0.7,
    "critical": 0.9
  }
}
```

### 顶层配置参数表

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `enabled` | `boolean` | `true` | 总开关；设为 `false` 时状态栏静默退出不输出 |
| `language` | `"auto" \| "en" \| "zh"` | `"auto"` | 界面语言（`"auto"` 自动读取 `LC_ALL` / `LC_CTYPE` / `LANG`） |
| `theme` | `object \| string` | `{ primary: "green", secondary: "gray", warning: "yellow", critical: "red" }` | 4 键配色对象、预设字符串（`"Emerald Green"`、`"Ocean Blue"`、`"Cyberpunk Magenta"`、`"Amber Gold"`）或单色名 |
| `thresholds` | `{ warning, critical }` | `{ warning: 0.7, critical: 0.9 }` | 配额警告/告警比例（`0`–`1`）；底栏上下文监控默认 `65%`/`85%`，自定义时同步按比例生效 |
| `icons` | `object` | *未设置* | 自定义图标覆写（`branch`、`plan`、`step`、`task`、`token`、`ctx`、`model`，最长 8 字符） |

### `display.*` 全量参数表

| 字段 | 类型 | 默认值 | 所属区域 | 说明 |
|---|---|---|---|---|
| `display.quotaStyle` | `"table" \| "compact" \| "none"` | `"table"` | 配额面板 | 厂商配额展示布局（`"models"` 自动回退 `"table"`） |
| `display.showContextMonitor` | `boolean` | `true` | 底部监控行 | 是否显示底部实时上下文与压缩监控行 |
| `display.showModelCache` | `boolean` | `true` | 累计缓存行 | 是否显示分模型累计缓存命中行 |
| `display.showCurrentDir` | `boolean` | `true` | 第 1 行 | 是否显示当前工作区目录名 |
| `display.showCycleMode` | `boolean` | `true` | 第 1 行 | 是否显示 `agy` 交互模式徽章（`auto`、`accept-edits`、`plan`、`best-of-n`） |
| `display.showSpeed` | `boolean` | `true` | 第 1 行 | 是否显示主模型流式加权均速与 TTFT，及最多 3 个活跃子模型速率 |
| `display.showDashboardUrl` | `boolean` | `true` | 最末行 | 是否在状态栏最底部单开一行显示本地网页看板网址（如 `http://127.0.0.1:3789`），并在未运行时后台静默自动拉起与活跃续期 |
| `display.dashboardPort` | `number` | `3789` | 网页看板 | 状态栏自动拉起网页看板时的首选本机端口（优先级低于 `AGY_PULSE_DASHBOARD_PORT`） |
| `display.dashboardIdleTimeoutSec` | `number` | `1800` | 网页看板 | 状态栏自动拉起网页看板时的空闲自动退出秒数（默认 `1800` 秒即 30 分钟；设为 `0` 常驻不自动退出；优先级低于 `AGY_PULSE_DASHBOARD_IDLE_SEC`） |
| `display.modelSpeedExpireMinutes` | `number` | `5` | 第 1 行 | 超过 N 分钟未调用自动隐藏速度与首字 |
| `display.modelSpeedExpireTurns` | `number` | `30` | 第 1 行 | 超过 N 轮未调用自动隐藏速度与首字 |
| `display.showModel` | `boolean` | `false` | 第 1 行 | 是否在状态栏首行显示模型名 |
| `display.showTokenBar` | `boolean` | `false` | 第 2 行 | 是否显示 Token 明细栏 |
| `display.showContextBar` | `boolean` | `false` | 第 2 行 | 是否显示经典上下文进度条 |
| `display.showTurnCacheHitRate` | `boolean` | `false` | 第 2 行 | 是否显示单轮缓存命中率徽章 |
| `display.cacheHitThresholds` | `{ excellent?, partial? }` | `{ excellent: 80, partial: 50 }` | 第 2 行 | 单轮缓存命中率颜色阈值 |
| `display.showBreadcrumbs` | `boolean` | `false` | 第 3 行 | 是否显示工作区记忆文件或最近修改面包屑 |
| `display.breadcrumbCount` | `number` | `3` | 第 3 行 | 面包屑最多显示条目数 |
| `display.showGitBranch` | `boolean` | `false` | 第 1 行 | 是否显示当前 Git 分支（非 Git 目录自动隐藏） |
| `display.showUsername` | `boolean` | `false` | 第 1 行 | 是否显示宿主 `stdin.username` / `stdin.email` 或自定义 `display.username`（自动读取宿主登录账号） |
| `display.username` | `string` | *未设置* | 第 1 行 | 自定义用户名覆写（开启 `showUsername` 时优先于宿主 `stdin.username` / `stdin.email` 与系统用户名） |
| `display.showCost` | `boolean` | `false` | 第 1 行 | 是否在状态栏首行显示当前**任务树**汇总费用 |
| `display.showDailyCost` | `boolean` | `false` | 第 1 行 | 是否显示今日累计费用（来自**账本**） |
| `display.showWeeklyCost` | `boolean` | `false` | 第 1 行 | 是否显示近 7 日滚动费用（来自**账本**） |
| `display.showRoutedCost` | `boolean` | `false` | 费用计算 | Bedrock/Vertex 路由模型是否采信 CLI 原生费用 |
| `display.useNerdFonts` | `boolean` | `false` | 图标 | 是否启用 Nerd Fonts 图标 |
| `display.unicode` | `boolean` | *省略（自动）* | 字符集 | 省略自动检测；`true` 强制 Unicode，`false` 强制纯 ASCII |
| `display.columnWidth` | `number` | `40` | 配额面板 | 列宽参考值 |
| `display.terminalWidth` | `number` | *未设置（自动）* | 响应式布局 | 显式终端列宽上限（`terminal_width > display.terminalWidth > COLUMNS > TTY`） |

---

## 多机同步私有配置（`agy-pulse-sync.json`）

多机同步配置专门存放在用户全局私有文件 `~/.gemini/antigravity-cli/agy-pulse-sync.json`（或 `$AGY_PULSE_SYNC_CONFIG`），在 Unix 下强制以目录 `0700`、文件 `0600` 权限原子落盘。除 `agypl sync init` 外，也可直接在本地**网页看板**顶栏「设置」抽屉的「同步配置」标签页中图形化管理（支持凭据只写不回显、`AGY_PULSE_*` 环境变量覆盖提示、实时连通性测试与安全移除）。**严禁放入工作区 `agy-pulse.config.json`**（程序也绝不会从任何工作区目录读取同步配置），以防密钥随项目提交至 Git 仓库。

### 顶层同步配置字段表

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `enabled` | `boolean` | `false` | 是否启用状态栏后台 `--sync-bg` 推送与**网页看板** `/api/sync-remote` 同步 |
| `deviceId` | `string`（可选） | *自动管理* | 不可变的本机**设备**唯一标识，与 `ledger_meta.local_device_id` 保持一致 |
| `deviceName` | `string`（可选） | 操作系统主机名 | 展示在 `sync status` 与**网页看板**中的**设备**名称（可随时修改而不分裂历史记录） |
| `defaultAccount` | `string`（可选） | *未设置* | 当无法从宿主 `stdin.email` 获取邮箱时（如 IDE 或历史记录）的回退默认**账号** |
| `pushIntervalSec` | `number` | `300` | 状态栏自动拉起后台 `--sync-bg` 推送的最小间隔秒数（`>= 1`） |
| `prefix` | `string` | `"agy-pulse-sync"` | 远端对象存储根前缀（自动去除首尾 `/`） |
| `remote` | `object` | `{ "type": "local-dir", "path": "" }` | 当前唯一激活的**同步远端**配置（见下方 4 种后端） |

### 4 种同步远端配置说明与示例

1. **本地同步目录（`local-dir`）** —— Syncthing、iCloud Drive、Dropbox、OneDrive：
   - 字段：`type`（`"local-dir"`）、`path`（`string`）。
   ```json
   {
     "enabled": true,
     "deviceName": "lab-ubuntu",
     "defaultAccount": "alice@example.com",
     "pushIntervalSec": 300,
     "prefix": "agy-pulse-sync",
     "remote": {
       "type": "local-dir",
       "path": "/home/alice/Sync/agy-pulse"
     }
   }
   ```

2. **WebDAV（`webdav`，兼容别名 `web-dav`）** —— 坚果云、Nextcloud、Alist、Caddy/Nginx WebDAV：
   - 字段：`type`（`"webdav"`）、`url`（`string`）、`username`（`string?`）、`password`（`string?`，可被 `AGY_PULSE_SYNC_PASSWORD` 覆盖）、`token`（`string?`，可被 `AGY_PULSE_SYNC_TOKEN` 覆盖）、`allow_insecure_http`（`boolean`，默认 `false`）。
   ```json
   {
     "enabled": true,
     "deviceName": "macbook-pro",
     "defaultAccount": "alice@example.com",
     "pushIntervalSec": 300,
     "prefix": "agy-pulse-sync",
     "remote": {
       "type": "webdav",
       "url": "https://dav.example.com/remote.php/dav/files/alice/agy-pulse",
       "username": "alice",
       "password": "",
       "allow_insecure_http": false
     }
   }
   ```

3. **S3 兼容对象存储（`s3`）** —— Cloudflare R2、AWS S3、Backblaze B2、MinIO、阿里云 OSS：
   - 字段：`type`（`"s3"`）、`endpoint`（`string`）、`region`（`string`，默认 `"auto"`）、`bucket`（`string`）、`access_key`（`string?`，可被 `AGY_PULSE_S3_ACCESS_KEY` 覆盖）、`secret_key`（`string?`，可被 `AGY_PULSE_S3_SECRET_KEY` 覆盖）、`path_style`（`boolean`，默认 `true`）、`allow_insecure_http`（`boolean`，默认 `false`）。
   ```json
   {
     "enabled": true,
     "deviceName": "workstation",
     "defaultAccount": "alice@example.com",
     "pushIntervalSec": 300,
     "prefix": "agy-pulse-sync",
     "remote": {
       "type": "s3",
       "endpoint": "https://<account-id>.r2.cloudflarestorage.com",
       "region": "auto",
       "bucket": "agy-pulse-sync",
       "access_key": "",
       "secret_key": "",
       "path_style": true,
       "allow_insecure_http": false
     }
   }
   ```

4. **Cloudflare Worker（`cf-worker`）** —— R2 优先 + 免费 KV 自动回退（部署见 [`deploy/cloudflare-worker/README.md`](../deploy/cloudflare-worker/README.md)）：
   - 字段：`type`（`"cf-worker"`）、`url`（`string`）、`token`（`string?`，可被 `AGY_PULSE_SYNC_TOKEN` 覆盖）、`allow_insecure_http`（`boolean`，默认 `false`）。
   ```json
   {
     "enabled": true,
     "deviceName": "laptop",
     "defaultAccount": "alice@example.com",
     "pushIntervalSec": 300,
     "prefix": "agy-pulse-sync",
     "remote": {
       "type": "cf-worker",
       "url": "https://agy-pulse-sync.example.workers.dev",
       "token": "",
       "allow_insecure_http": false
     }
   }
   ```

---

## 环境变量全表

| 环境变量 | 用途 |
|---|---|
| `AGY_PULSE_DATA_DIR` | 自定义 Antigravity CLI 数据目录（默认 `~/.gemini/antigravity-cli`） |
| `AGY_PULSE_IDE_DATA_DIR` | 自定义 Antigravity IDE 数据目录（默认 `~/.gemini/antigravity`；仅设 `AGY_PULSE_DATA_DIR` 时置空以隔离测试） |
| `AGY_PULSE_GLOBAL_CONFIG_DIR` | 覆盖全局 `agy-pulse.config.json` 所在目录 |
| `AGY_PULSE_BIN_DIR` | 自定义二进制安装目录（Unix 默认 `~/.local/bin`，Windows 默认 `%LOCALAPPDATA%\agy-pulse\bin`） |
| `AGY_PULSE_REPO` | 自定义 `owner/repo` 仓库（默认 `hardworkDAD/agy-pulse`） |
| `AGY_PULSE_REPO_URL` | 自定义 `agy plugin install` 仓库克隆地址 |
| `AGY_PULSE_RELEASE_BASE_URL` | 自定义 GitHub Release 下载源基础地址（镜像或本地 `http://127.0.0.1` 测试服务） |
| `AGY_PULSE_UPDATE_URL` | 自定义版本元数据 JSON URL |
| `AGY_PULSE_DISABLE_UPDATE_CHECK` | 设为 `1` 禁用状态栏路径下的 24h 后台更新检查 |
| `AGY_PULSE_FORCE_ASCII` | 设为 `1` 强制使用纯 ASCII 字符 |
| `AGY_PULSE_FORCE_UNICODE` | 设为 `1` 强制使用 Unicode 字符 |
| `AGY_PULSE_LANGUAGE` | 强制界面语言（`en` / `zh` / `auto`） |
| `AGY_PULSE_NO_OPEN` | 设为 `1` 阻止 `agypl dashboard` 自动打开浏览器 |
| `AGY_PULSE_NO_DASHBOARD_AUTOSTART` | 设为 `1` 时禁止状态栏自动拉起后台网页看板（若看板已在运行仍会显示其网址） |
| `AGY_PULSE_DASHBOARD_PORT` | 覆盖状态栏自动拉起网页看板时的首选监听端口（默认 `3789`） |
| `AGY_PULSE_DASHBOARD_IDLE_SEC` | 覆盖 `agypl dashboard open` / `--background`（默认 `600`）及状态栏自动拉起（默认 `1800`）的空闲自动退出秒数（`0` 表示不自动退出） |
| `AGY_PULSE_DASHBOARD_CLOSE_GRACE_MS` | 覆盖最后一个页面关闭后的优雅退出宽限毫秒数 |
| `AGY_PULSE_SYNC_CONFIG` | 自定义私有同步配置文件 `agy-pulse-sync.json` 路径（绝不读取工作区目录） |
| `AGY_PULSE_SYNC_TOKEN` | 覆盖 `webdav` / `cf-worker` 的 Bearer 令牌（避免明文落盘） |
| `AGY_PULSE_SYNC_PASSWORD` | 覆盖 `webdav` 的 Basic Auth 密码（避免明文落盘） |
| `AGY_PULSE_S3_ACCESS_KEY` | 覆盖 `s3` Access Key ID（避免明文落盘） |
| `AGY_PULSE_S3_SECRET_KEY` | 覆盖 `s3` Secret Access Key（避免明文落盘） |
| `AGY_PULSE_NO_SYNC_AUTOSTART` | 设为 `1` / `true` / `yes` 时禁止状态栏自动拉起后台 `--sync-bg` 推送 |
| `AGY_PULSE_PROFILE_PATH` | 覆盖 `scripts/configure-utf8.ps1` 的 PowerShell Profile 路径 |
| `COLUMNS` | 终端列宽兜底值 |
| `NO_COLOR` | 禁用 `agypl stats` ANSI 彩色输出 |
| `LC_ALL` / `LC_CTYPE` / `LANG` | 用于自动检测终端 UTF-8 能力与 `zh`/`en` 语言 |

---

## 本地开发与 v1 平滑升级

从旧版 `v1.0.0`（Node.js）升级时，现有 `agy-pulse.config.json` 自动保留并兼容生效，`settings.json` 的 `statusLine.command` 自动切换为原生 `agy-pulse` 二进制；历史消费与今日/7日花费通过重扫 CLI 与 IDE 原始 `conversations/*.db` 自动重建至 `agy-pulse-ledger.sqlite`。

```bash
cargo clippy --all-targets -- -D warnings
cargo test
bash scripts/deploy-local.sh
```

---

## 完整目录结构

```
agy-pulse/
├── Cargo.toml                         # Rust Edition 2024 包清单与静态内嵌依赖
├── Cargo.lock                         # 锁定依赖树
├── GLOSSARY.md                        # 权威领域术语表
├── LICENSE                            # MIT 开源许可证
├── NOTICE                             # 开源项目归属与致谢声明
├── plugin.json                        # {"name":"agy-pulse"}
├── gemini-extension.json              # agy 插件清单元数据 (v2.0.0)
├── release.sh                         # 校验、本地打包与 Tag 发版脚本
├── uninstall.sh / uninstall.ps1       # 零 Node 跨端卸载脚本
├── .github/workflows/
│   ├── ci.yml                         # Linux / macOS / Windows Clippy + Release 编译 + 全量测试
│   └── release.yml                    # 五大官方平台交叉/原生打包与统一 SHA256SUMS 发布
├── src/
│   ├── lib.rs                         # 禁 panic 门禁与统一 CLI 分发入口 run_cli()
│   ├── main.rs                        # 主入口 `agy-pulse`
│   ├── bin/agypl.rs                   # 简写入口 `agypl`
│   ├── domain/                        # 纯内存领域模型、统一计费定价与配额计算
│   ├── ingest/                        # HostReader：stdin、增量日志、无锁 .db、Protobuf 与工作区元数据
│   ├── ledger/                        # 统一 SQLite WAL 账本、双源同步、滚动花费与 stats 报表
│   ├── render/                        # 纯内存状态栏排版、进度条、配额表、宽字符截断与双语
│   ├── config/                        # 深度合并配置加载器与原始终端交互式向导
│   ├── dashboard/                     # 127.0.0.1 本地看板服务、SSE 页面保活、启动器与内嵌前端
│   ├── sync/                          # 多机对象同步引擎、4 种同步远端与 `sync` CLI 子命令
│   ├── ops/                           # 原生安装器、安全卸载器、`account assign` 与自更新器
│   └── platform/                      # Unix / Windows 系统底层差异封装
├── deploy/
│   └── cloudflare-worker/             # 零构建依赖 Cloudflare Worker（R2 / KV）同步模板
├── scripts/
│   ├── install.sh / install.ps1       # 预编译包一键下载与 SHA-256 校验安装脚本
│   ├── deploy-local.sh                # 本地 Cargo Release 编译部署与冒烟自检
│   └── configure-utf8.ps1             # Windows UTF-8 环境助手
├── skills/pulse-config/SKILL.md       # Agent 交互式配置技能
├── docs/                              # 中英文架构、ADR、模块与参考文档
└── tests/
    └── t01..t15_*.rs                  # 黑盒集成测试、双端黄金对拍、多机同步与网页配置 E2E 套件
```
