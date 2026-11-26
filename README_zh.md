# agy-pulse

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)
![Rust 原生单二进制](https://img.shields.io/badge/Runtime-Native%20Rust-orange.svg)
![零运行时依赖](https://img.shields.io/badge/Dependencies-Zero%20Runtime-brightgreen.svg)

> **Antigravity CLI (`agy`) 原生单二进制 Rust 状态栏、实时上下文与压缩上限监控、统一 SQLite WAL 遥测账本（`stats` / `dashboard`）与合规模型配额面板。**

[English](./README.md) · [完整参考手册](./docs/reference_zh.md)

## 显示效果

```text
my-project │ Google AI Pro │ plan │ 输出: 42.6 tok/s (首字 0.8s)
  ───────────────────────────────────────────────────────────────────
  Google 5h   [█████░]  87% ~22m   │ Claude 5h   [██████] 100% ~4h59m
  Google week [████░░]  74% ~4d3h  │ Claude week [██████] 100% ~6d23h
⟳ 累计缓存 · Carbon 82% (命中 160K · 写入 4K / 总入 196K)
◈ Gemini 4 Carbon (High) · 上下文 ████▓······· 38.4% (196.6K/512K) · 压缩×1(峰480K) · 缓存 82% (160K) · 输出 12.4K · 费用 $0.12
```

内置纯离线本地**网页看板**（`agypl dashboard`）及 3 种**模型配额**显示模式（`table`、`compact`、`none`），非 UTF-8 终端自动降级纯 ASCII；详见[完整参考手册](./docs/reference_zh.md#配额显示模式displayquotastyle)。

## 核心特性

- **原生单二进制 Rust（~4ms 冷启动、零 Node 依赖）**：编译为单一静态原生可执行文件（`agy-pulse` / `agypl`），内嵌 SQLite，彻底告别 Node.js、Python 或系统命令行工具依赖，具备 1.5s 硬超时兜底。
- **零场外反重力请求，严守合规红线**：物理移除系统钥匙串/OAuth 令牌提取与云端配额请求，运行期绝不向反重力或模型厂商服务端发起任何场外请求；**模型配额**与套餐等级 100% 消费宿主 `stdin` 合法传入的数据。
- **实时上下文与上下文压缩监控**：无锁只读扫描宿主 SQLite `.db` 与 Protobuf 服务端**上下文压缩**上限（`?immutable=1` / 活跃 WAL 临时副本），`CHECKPOINT` 压缩后保留历史峰值水位，支持分模型累计缓存命中行与 `>=85%` `即将触发压缩` 红色告警。
- **统一计费口径与任务树归拢**：**状态栏**与**网页看板**共享全局唯一字数归一化与费率公式（完整计入 `f4.f1` 系统提示词、缓存读写与 >200k 长上下文阶梯定价），自动归拢**任务树（Cascade）**内主**单窗对话**与全部子代理明细。
- **统一 SQLite WAL 账本与长效在线网页看板**：内嵌 SQLite WAL **账本**（`agy-pulse-ledger.sqlite`）合并归档 CLI 与 Antigravity IDE 遥测；`agypl dashboard open` 启动严格绑定 `127.0.0.1` 的独立小窗**网页看板**，窗口最小化或切至后台标签页始终保持在线不断连，仅在所有打开页面真正关闭后自动退出回收。
- **多机对象同步（零写冲突、隐私最小化）**：按 **`账号` → `设备` → `来源端`（`cli` / `ide`）→ `任务树`** 跨多台**设备**聚合消费统计，并共享各**账号**最新**配额快照**。同步载荷**零对话正文、零本机 `workspace` 目录路径、零 Git 分支名**；**状态栏**热路径严格仅查询本机数据并在后台节流静默 Push（`--sync-bg`），**网页看板**秒开本地**账本**后异步拉取远端更新。

## 多机同步远端

支持以下 4 种**同步远端**（同一时刻激活 1 个，配置与密钥仅存全局 `0600` 私有文件 `~/.gemini/antigravity-cli/agy-pulse-sync.json`）：

- **WebDAV**（`webdav`）：坚果云、Nextcloud、Alist、Caddy/Nginx WebDAV 等
- **S3 兼容对象存储**（`s3`）：Cloudflare R2、AWS S3、Backblaze B2、MinIO、阿里云 OSS（纯 Rust SigV4 签名，零 OpenSSL 依赖）
- **Cloudflare Worker**（`cf-worker`）：内置 R2 优先 + 免费 KV 自动回退模板，部署指南见 [`deploy/cloudflare-worker/README.md`](./deploy/cloudflare-worker/README.md)
- **本地同步目录**（`local-dir`）：Syncthing、iCloud Drive、Dropbox、OneDrive 等本地同步文件夹

快速上手：

```bash
agy-pulse sync init                   # 交互式配置远端并执行连通性自检
agy-pulse sync                        # 手动执行双向同步（先 Pull 后 Push）
agy-pulse account assign alice@example.com   # 将本机未归属存量记录收编到指定账号
```

## 快速开始

在普通终端执行（**不要**在已打开的 `agy` 会话内执行，因 `agy` 退出时会覆写 `settings.json`）：

```bash
# macOS / Linux 一键预编译安装（支持 x86_64 与 aarch64）
curl -fsSL https://raw.githubusercontent.com/hardworkDAD/agy-pulse/main/scripts/install.sh | bash
```

```powershell
# Windows x64 PowerShell 一键预编译安装
irm https://raw.githubusercontent.com/hardworkDAD/agy-pulse/main/scripts/install.ps1 | iex
```

也可通过 Cargo 从源码安装（`cargo install --git https://github.com/hardworkDAD/agy-pulse && agy-pulse install`），或在本地克隆仓库执行：

```bash
bash scripts/deploy-local.sh
```

官方 GitHub Releases 覆盖五大平台预编译产物（`linux-x86_64` musl 静态版、`linux-aarch64` musl 静态版、`macos-x86_64`、`macos-aarch64`、`windows-x86_64`），附带统一 `SHA256SUMS` 校验。自建 Fork 或镜像源（`AGY_PULSE_REPO` / `AGY_PULSE_RELEASE_BASE_URL`）请见[完整参考手册](./docs/reference_zh.md#安装细节与自建-fork--镜像源)。

免终端一键安装桌面图标并直开本地**网页看板**：

```bash
agypl dashboard install-shortcut --desktop   # 安装系统应用启动器图标（可选同步放桌面）
agypl dashboard open                         # 静默拉起后台并用独立 App 窗口打开
```

## 常用命令

`agypl` 与 `agy-pulse` 支持完全一致的子命令：

| 命令 | 兼容别名 | 用途说明 |
|---|---|---|
| `agypl dashboard [action] [options]` | `dash`, `ui`, `--dashboard` | 启动或管理本地离线**网页看板**（`127.0.0.1:3789`，支持桌面图标、长效保活与独立窗口） |
| `agypl stats [range] [options]` | `report`, `--stats` | 输出 CLI 与 Antigravity IDE 终端遥测速查报表 |
| `agypl sync [init\|status\|prune-device]` | — | 与已配置的**同步远端**同步多机**账本**分片与**配额快照** |
| `agypl account assign <email> [options]` | — | 将本机 `unassigned` 存量**单窗对话**与步骤归属到指定邮箱 |
| `agypl config` | `--config` | 打开交互式终端配置向导（支持实时**状态栏**预览与原子保存） |
| `agypl install [--bin-dir <dir>]` | `--install` | 部署原生 `agy-pulse` / `agypl` 二进制并向宿主 `settings.json` 注册 `statusLine` |
| `agypl uninstall [--purge-ledger]` | `--uninstall` | 安全移除 `statusLine` 注册、二进制、快捷方式与缓存（默认保留**账本**） |
| `agypl update [--force]` | `--update` | 检查 GitHub Releases 最新版本，校验 `SHA256SUMS` 并原子自更新 |
| `agypl help` | `--help`, `-h` | 显示 CLI 帮助与当前版本号（`--version`, `-V`） |
| `agy-pulse` *(stdin)* | — | 从 `stdin` 读取 `agy` JSON（1.5s 超时）并渲染**状态栏** |

`stats` 全部参数、`dashboard` 全部子动作与参数及脚本选项见[完整参考手册](./docs/reference_zh.md#cli-命令与参数全表)。

## 配置

运行 `agypl config` 打开交互向导，或在工作区创建 `./agy-pulse.config.json` / 编辑全局 `~/.gemini/antigravity-cli/agy-pulse-runtime/agy-pulse.config.json`：

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
    "showCost": false,
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

全部配置项（如 `display.dashboardPort`、`display.dashboardIdleTimeoutSec` 等）与环境变量（如 `AGY_PULSE_DASHBOARD_PORT`、`AGY_PULSE_DASHBOARD_IDLE_SEC`、`AGY_PULSE_NO_DASHBOARD_AUTOSTART` 等）详情请参阅[完整参考手册](./docs/reference_zh.md#完整配置项参考)。

## 卸载

```bash
bash uninstall.sh        # macOS / Linux
```

```powershell
.\uninstall.ps1          # Windows PowerShell
```

或直接执行 `agypl uninstall`。默认**保留**永久遥测**账本**（`agy-pulse-ledger.sqlite`）；如需一并删除，可传入 `--purge-ledger`。

## 文档导航

- [完整参考手册 (中文)](./docs/reference_zh.md) · [Reference Manual (English)](./docs/reference.md) — 全量 CLI 参数、配置项、环境变量与目录树
- [系统架构 (中文)](./docs/architecture_zh.md) · [Architecture (English)](./docs/architecture.md) — Rust 深模块架构、数据流与 ADR 决策记录
- [模块参考](./docs/module-reference.md) — 各模块公开接口、不变量与行为契约
- [变更日志](./docs/logs/CHANGELOG.md) · [文档索引](./docs/README.md) — `v2.0.0` 变更明细与维护者索引

## 参考项目与致谢

| 项目 | 仓库地址 | 参考与启发维度 |
|---|---|---|
| `zzusec/agy-statusline` | https://github.com/zzusec/agy-statusline | 实时上下文窗口进度条、Protobuf 服务端压缩上限解码、费用监控与大写 `K`/`M` 格式化风格 |
| `icebear0828/agy-hud` | https://github.com/icebear0828/agy-hud | Antigravity CLI 多行布局、配额面板与会话遥测思路 |
| `elviscjn-cjn/antigravity-context-viewer` | https://github.com/elviscjn-cjn/antigravity-context-viewer | `gen_metadata` Protobuf `timeToFirstToken` / `streamingDuration` 解码、分模型流式速率统计与模型费率对齐参考 |

以上项目的版权与许可均归原项目作者所有；`agy-pulse` 基于 MIT 协议完全开源，详细归属与致谢说明见 [NOTICE](./NOTICE)。

## 参与贡献与开源协议

运行静态检查与测试套件：

```bash
cargo clippy --all-targets -- -D warnings
cargo test
```

欢迎提交 Issue 与 Pull Request——请参阅 [贡献指南](./CONTRIBUTING.md)、[行为准则](./CODE_OF_CONDUCT.md) 与 [安全策略](./SECURITY.md)。

本项目采用 [MIT License](./LICENSE) 开源；第三方项目归属与致谢声明见 [NOTICE](./NOTICE)。
