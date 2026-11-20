# agy-pulse 维护者文档索引

> 本目录包含面向维护者的技术文档，与 `v1.0.0` 代码库严格对齐。

## 文档列表

| 文档 | 内容 |
|---|---|
| [architecture.md](architecture.md) | 系统架构（English）：两层设计、模块依赖图、v1.0.0 架构升级 |
| [architecture_zh.md](architecture_zh.md) | 系统架构与数据流（中文）：执行流程、核心模块与技术亮点 |
| [module-reference.md](module-reference.md) | 每个模块的 API、函数签名、行为说明与 CLI/环境约定 |
| [logs/CHANGELOG.md](logs/CHANGELOG.md) | 版本变更日志（v1.0.0） |
| [../README.md](../README.md) | 项目英文主页、安装、配置字段与环境变量全表 |
| [../README_zh.md](../README_zh.md) | 项目中文主页、安装、配置字段与环境变量全表 |
| [../skills/pulse-config/SKILL.md](../skills/pulse-config/SKILL.md) | Agent 交互式配置技能定义（`pulse-config`） |
| [../LICENSE](../LICENSE) | MIT 许可证与开源项目致谢 |

---

## 快速导航

### 我想了解整体架构
→ [architecture.md](architecture.md) / [architecture_zh.md](architecture_zh.md) — 两层设计、数据流与模块依赖图

### 我要修改某个模块
→ [module-reference.md](module-reference.md) — 每个文件的函数签名和行为

### 安装/引导/升级流程出了问题
→ [module-reference.md](module-reference.md) 与 [../README_zh.md](../README_zh.md) — `bootstrap.js` 原子部署、保留用户全局配置、`AGY_PULSE_DATA_DIR` 与卸载流程

### quota 显示或认证探测不正确
→ [module-reference.md](module-reference.md) — SWR 缓存、provider/window 合并、`unavailableReason` 与快速路径认证

### agy-pulse 状态栏显示异常（字符乱码、颜色错误、主题预设）
→ [architecture.md](architecture.md) / [module-reference.md](module-reference.md) — `encoding.js` 编码降级、`renderer.js` 与 `context-monitor.js`

### 配置选项与交互向导说明
→ [../skills/pulse-config/SKILL.md](../skills/pulse-config/SKILL.md) / [../README.md](../README.md) — 全字段 Schema、主题预设与默认值

### 发版与变更记录
→ [logs/CHANGELOG.md](logs/CHANGELOG.md) — v1.0.0 变更明细与 E2E 发布门槛

---

## 关键不变量（维护时必读）

1. **`gemini-extension.json` 不得删除** — `agy plugin install` 强制校验
2. **statusline 热路径不得阻塞网络** — quota SWR 必须纯缓存
3. **`writeCache` 与配置写入必须原子写** — `tmp + rename` 防并发截断
4. **单元测试通过 ≠ 完成** — E2E（含 zip 清单校验）才是完成门槛
5. **bootstrap 必须幂等且保留用户已保存全局配置** — 仅 `--reset-config` 时重置
6. **卸载不得删除第三方 shim 或无关插件** — 按路径/内容匹配才清理

---

## 代码与文档对应表

| 源文件 | 对应文档 |
|---|---|
| `runtime/bin/agy-pulse.js` | [module-reference.md](module-reference.md), [architecture.md](architecture.md) |
| `runtime/parser.js` | [module-reference.md](module-reference.md), [architecture_zh.md](architecture_zh.md) |
| `runtime/compaction.js` | [module-reference.md](module-reference.md), [architecture.md](architecture.md) |
| `runtime/quota.js` | [module-reference.md](module-reference.md) |
| `runtime/quota/token.js` | [module-reference.md](module-reference.md) |
| `runtime/quota/cache.js` | [module-reference.md](module-reference.md) |
| `runtime/quota/cloud.js` | [module-reference.md](module-reference.md) |
| `runtime/quota/models.js` | [module-reference.md](module-reference.md) |
| `runtime/renderer.js` | [module-reference.md](module-reference.md), [architecture_zh.md](architecture_zh.md) |
| `runtime/renderer/context-monitor.js` | [module-reference.md](module-reference.md), [architecture_zh.md](architecture_zh.md) |
| `runtime/renderer/format.js` | [module-reference.md](module-reference.md) |
| `runtime/renderer/lang.js` | [module-reference.md](module-reference.md) |
| `runtime/renderer/quota-render.js` | [module-reference.md](module-reference.md) |
| `runtime/config.js` | [module-reference.md](module-reference.md), [../README.md](../README.md) |
| `runtime/config-wizard.js` | [module-reference.md](module-reference.md) |
| `runtime/encoding.js` | [module-reference.md](module-reference.md) |
| `runtime/paths.js` | [module-reference.md](module-reference.md) |
| `runtime/statusline-installer.js` | [module-reference.md](module-reference.md) |
| `runtime/uninstall.js` | [module-reference.md](module-reference.md) |
| `runtime/update-checker.js` | [module-reference.md](module-reference.md) |
| `scripts/bootstrap.js` | [module-reference.md](module-reference.md) |
| `scripts/verify-display.js` | [module-reference.md](module-reference.md) |
| `skills/pulse-config/SKILL.md` | [../skills/pulse-config/SKILL.md](../skills/pulse-config/SKILL.md) |
| `runtime/agy-pulse.config.json` | [../README.md](../README.md) |
