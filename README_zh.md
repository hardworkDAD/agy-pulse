# agy-pulse

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)
![零第三方依赖](https://img.shields.io/badge/Dependencies-Zero-brightgreen.svg)
![Node.js >= 22.5](https://img.shields.io/badge/Node.js-%3E%3D22.5-339933.svg)

> **Antigravity CLI (`agy`) 全能单进程状态栏、实时上下文与压缩上限监控、永久遥测账本（`stats` / `dashboard`）与云端配额追踪插件。**

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

内置纯离线本地 Web 看板（`agypl dashboard`）及 4 种配额显示模式（`table`、`models`、`compact`、`none`），非 UTF-8 终端自动降级纯 ASCII；详见[完整参考手册](./docs/reference_zh.md#配额显示模式displayquotastyle)。

## 核心特性

- **零外部依赖单进程**：纯 Node.js 运行（推荐 >= 22.5 使用内置 `node:sqlite`，Node 20 自动降级为 JSON 账本），无任何第三方 npm 依赖，SWR 配额刷新不阻塞状态栏热路径。
- **实时上下文与压缩上限监控**：原生 SQLite Protobuf 解码服务端压缩上限、分模型累计缓存命中，达到 85%+ 红色告警 `即将触发压缩`。
- **云端配额、费用与流式速率追踪**：聚合厂商/模型配额、统一费率估算、加权 `tok/s` 与首字延迟，以及活跃 `/swarm` 子代理速率。
- **永久 SQLite 账本与免终端桌面看板**：内置 `node:sqlite` 账本合并归档 CLI 与 Antigravity IDE 遥测，支持 `agypl dashboard open` 一键独立窗口直开。
- **纯本地隐私安全与跨平台**：所有数据仅留存本地（`127.0.0.1` 回环监听、只读扫描源库），原生支持 macOS、Linux 与 Windows。

## 快速开始

在普通终端执行（**不要**在已打开的 `agy` 会话内执行，因 `agy` 退出时会覆写 `settings.json`）：

```bash
# macOS / Linux 一键远程安装
curl -fsSL https://raw.githubusercontent.com/hardworkDAD/agy-pulse/main/scripts/install.sh | bash
```

```powershell
# Windows PowerShell 一键远程安装
irm https://raw.githubusercontent.com/hardworkDAD/agy-pulse/main/scripts/install.ps1 | iex
```

本地克隆仓库可运行 `bash scripts/deploy-local.sh`（或 `bash scripts/install.sh` / `.\scripts\install.ps1`）。自建 Fork 或镜像源（`AGY_PULSE_REPO`）请见[完整参考手册](./docs/reference_zh.md#安装细节与自建-fork--镜像源)。

免终端一键安装桌面图标并直开本地 Web 看板：

```bash
agypl dashboard install-shortcut --desktop   # 安装应用启动器图标（可选同步放桌面）
agypl dashboard open                         # 静默拉起后台并用独立 App 窗口打开
```

## 常用命令

`agypl` 与 `agy-pulse` 支持完全一致的子命令：

| 命令 | 兼容别名 | 用途说明 |
|---|---|---|
| `agypl dashboard [action] [options]` | `dash`, `ui`, `--dashboard` | 启动或管理本地离线 Web 看板（`127.0.0.1:3789`，支持桌面图标与独立窗口） |
| `agypl stats [range] [options]` | `report`, `--stats` | 输出 CLI 与 Antigravity IDE 终端遥测速查报表 |
| `agypl config` | `--config` | 打开交互式终端配置向导 |
| `agypl diagnose` | `--diagnose` | 输出 JSON 认证与配额缓存诊断信息 |
| `agypl update` | `--update` | 从配置仓库在线检查并自更新运行时 |
| `agypl help` | `--help`, `-h` | 显示 CLI 帮助与当前版本号 |
| `agy-pulse` *(stdin)* | — | 从 `stdin` 读取 `agy` JSON（1.5s 超时）并渲染状态栏 |

`stats` 全部 16 项参数、`dashboard` 全部 14 项动作与参数及脚本选项见[完整参考手册](./docs/reference_zh.md#cli-命令与参数全表)。

## 配置

运行 `agypl config` 打开交互向导，或在工作区创建 `./agy-pulse.config.json` / 编辑全局 `~/.gemini/antigravity-cli/agy-pulse-runtime/runtime/agy-pulse.config.json`：

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

全部配置项与 24 个环境变量详情请参阅[完整参考手册](./docs/reference_zh.md#完整配置项参考)。

## 卸载

```bash
bash uninstall.sh        # macOS / Linux
```

```powershell
.\uninstall.ps1          # Windows PowerShell
```

默认**保留**永久遥测账本（`agy-pulse-ledger.sqlite` / `.json`）；如需一并删除，可在执行 `node runtime/uninstall.js` 时传入 `--purge-ledger`。

## 文档导航

- [完整参考手册 (中文)](./docs/reference_zh.md) · [Reference Manual (English)](./docs/reference.md) — 全量 CLI 参数、配置项、环境变量与目录树
- [系统架构 (中文)](./docs/architecture_zh.md) · [Architecture (English)](./docs/architecture.md) — 两层设计、数据流与模块依赖图
- [模块参考](./docs/module-reference.md) — 各模块 API、函数签名与契约行为
- [变更日志](./docs/logs/CHANGELOG.md) · [文档索引](./docs/README.md) — 版本历史与维护者索引

## 参考项目与致谢

| 项目 | 仓库地址 | 参考与启发维度 |
|---|---|---|
| `zzusec/agy-statusline` | https://github.com/zzusec/agy-statusline | 实时上下文窗口进度条、Protobuf 服务端压缩上限解码、费用监控与大写 `K`/`M` 格式化风格 |
| `icebear0828/agy-hud` | https://github.com/icebear0828/agy-hud | Antigravity CLI 多行 HUD 布局、云端配额面板与会话遥测思路 |
| `elviscjn-cjn/antigravity-context-viewer` | https://github.com/elviscjn-cjn/antigravity-context-viewer | `gen_metadata` Protobuf `timeToFirstToken` / `streamingDuration` 解码、分模型流式速率统计与模型费率对齐参考 |

以上项目的版权与许可均归原项目作者所有；`agy-pulse` 基于 MIT 协议完全开源，详细归属与致谢说明见 [NOTICE](./NOTICE)。

## 参与贡献与开源协议

运行 `npm test` 验证单元测试，欢迎提交 Issue 与 Pull Request——请参阅 [贡献指南](./CONTRIBUTING.md)、[行为准则](./CODE_OF_CONDUCT.md) 与 [安全策略](./SECURITY.md)。

本项目采用 [MIT License](./LICENSE) 开源；第三方项目归属与致谢声明见 [NOTICE](./NOTICE)。
