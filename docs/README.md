# agy-pulse 维护者文档索引

> 本目录包含面向开发者与维护者的技术文档与完整参考手册，与 `v2.0.0` 原生 Rust 单二进制代码库严格对齐。

## 文档列表

| 文档 | 内容 |
|---|---|
| [../GLOSSARY.md](../GLOSSARY.md) | 领域术语表（状态栏、网页看板、单窗对话、任务树、账本、上下文压缩、模型配额） |
| [rust-v2-architecture.md](rust-v2-architecture.md) | **Rust v2.0.0 重写共识归档与深模块架构蓝图** |
| [adr/0001-rust-single-binary-v2.md](adr/0001-rust-single-binary-v2.md) | ADR-0001：采用单一原生 Rust 二进制重构本地运行时（v2.0.0） |
| [adr/0002-remove-cloud-quota-requests.md](adr/0002-remove-cloud-quota-requests.md) | ADR-0002：移除向反重力/Google 服务端主动发请求的云端配额模块 |
| [adr/0003-deep-module-single-crate.md](adr/0003-deep-module-single-crate.md) | ADR-0003：采用高内聚深模块与纯数据契约实现模块解耦 |
| [reference.md](reference.md) | 完整参考手册（English）：CLI 参数全表、账本与看板机制、配置项全表、环境变量与目录树 |
| [reference_zh.md](reference_zh.md) | 完整参考手册（中文）：CLI 参数全表、账本与看板机制、配置项全表、环境变量与目录树 |
| [architecture.md](architecture.md) | 系统架构（English）：深模块依赖图、数据流与 v2.0.0 核心升级 |
| [architecture_zh.md](architecture_zh.md) | 系统架构与数据流（中文）：深模块职责、执行流程与核心亮点 |
| [module-reference.md](module-reference.md) | 各 Rust 深模块的公开接口、职责边界与行为契约 |
| [logs/CHANGELOG.md](logs/CHANGELOG.md) | 版本变更日志（`v2.0.0` 及历史版本） |
| [../README.md](../README.md) | 项目英文主页（定位、预览、快速开始、常用命令与致谢） |
| [../README_zh.md](../README_zh.md) | 项目中文主页（定位、预览、快速开始、常用命令与致谢） |
| [../skills/pulse-config/SKILL.md](../skills/pulse-config/SKILL.md) | Agent 交互式配置技能定义（`pulse-config`） |
| [../LICENSE](../LICENSE) | 标准 MIT 开源许可证 |
| [../NOTICE](../NOTICE) | 开源项目归属与参考致谢声明 |
| [../CONTRIBUTING.md](../CONTRIBUTING.md) | 贡献指南、开发流程与工程约束 |
| [../SECURITY.md](../SECURITY.md) | 安全模型说明与漏洞私密上报指引 |
| [../CODE_OF_CONDUCT.md](../CODE_OF_CONDUCT.md) | 开源社区行为准则（Contributor Covenant v2.1） |

---

## 快速导航

### 我想查阅全量 CLI 参数、配置项与环境变量
→ [reference.md](reference.md) / [reference_zh.md](reference_zh.md) — `stats` / `dashboard` / `install` / `uninstall` / `update` 参数全表、`display.*` 全量字段与环境变量

### 我想了解整体架构与合规边界
→ [architecture.md](architecture.md) / [architecture_zh.md](architecture_zh.md) — 单向无环深模块架构、纯内存计算核与 ADR-0001..0003

### 我要修改某个模块
→ [module-reference.md](module-reference.md) — `domain` / `ingest` / `ledger` / `render` / `config` / `dashboard` / `ops` / `platform` 模块契约

### 安装/卸载/自更新流程
→ `src/ops/`、`scripts/install.sh`、`scripts/install.ps1`、`scripts/deploy-local.sh`、`uninstall.sh`、`uninstall.ps1`

### 发版与变更记录
→ [logs/CHANGELOG.md](logs/CHANGELOG.md) — `v2.0.0` 重大变更与五端发布流水线

---

## 关键不变量（维护时必读）

1. **`plugin.json` 与 `gemini-extension.json` 不得删除** — `agy plugin install` 强制校验。
2. **零场外反重力请求（ADR-0002）** — 严禁读取凭证/钥匙串或向反重力/厂商服务端发起场外请求；**模型配额**仅来自宿主 `stdin`。
3. **单向无环与纯内存核（ADR-0003）** — `domain` 与 `render` 保持零 I/O；严禁跨模块传递 `rusqlite::Connection` 或文件句柄。
4. **库代码零恐慌** — `src/lib.rs` 强制 `#![deny(clippy::unwrap_used, clippy::expect_used)]`，状态栏模式兜底永以 `0` 退出。
5. **本地发布与升级校验** — `agypl update` 与一键安装脚本强制验证 `SHA256SUMS` 与 HTTPS 安全重定向边界。
6. **卸载默认保留账本** — `agypl uninstall` 仅清理自身状态栏注册、二进制与指纹匹配的快捷方式，保留 `agy-pulse-ledger.sqlite`（除非传入 `--purge-ledger`）。

---

## 代码与文档对应表

| 源码目录 / 文件 | 对应文档 |
|---|---|
| `src/lib.rs`, `src/main.rs`, `src/bin/agypl.rs` | [module-reference.md](module-reference.md), [architecture.md](architecture.md) |
| `src/domain/` | [module-reference.md](module-reference.md), [GLOSSARY.md](../GLOSSARY.md) |
| `src/ingest/` | [module-reference.md](module-reference.md), [architecture_zh.md](architecture_zh.md) |
| `src/ledger/` | [module-reference.md](module-reference.md), [reference_zh.md](reference_zh.md) |
| `src/render/` | [module-reference.md](module-reference.md), [architecture_zh.md](architecture_zh.md) |
| `src/config/` | [module-reference.md](module-reference.md), [reference.md](reference.md) |
| `src/dashboard/` | [module-reference.md](module-reference.md), [reference.md](reference.md) |
| `src/ops/` | [module-reference.md](module-reference.md), [reference_zh.md](reference_zh.md) |
| `src/platform/` | [module-reference.md](module-reference.md), [architecture.md](architecture.md) |
| `skills/pulse-config/SKILL.md` | [../skills/pulse-config/SKILL.md](../skills/pulse-config/SKILL.md) |
