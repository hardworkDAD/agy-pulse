# Statusline 统一总仓库说明

本仓库整合了当前本地实际使用的组合式 `statusLine` 全套组件：

- `runtime/`、`scripts/`、`skills/`、`tests/`：`agy-hud` 主体（额度表、工作区信息、输出速率 `输出: X tok/s` 等），已包含：
  - `renderer.js`：`showModel` / `showContextBar` 可组合显示开关与空行过滤；
  - `parser.js` + `speed.js`：修复 `PLANNER_RESPONSE` 耗时区间反置（原本误用上一步工具耗时）与 `total_output_tokens` 累计值未做差导致的虚高 `tok/s` 问题。
- `agy-statusline/statusline.sh`：Python 版实时上下文上限、压缩次数与单轮缓存命中率监控。
- `statusline-combined.sh`：入口组合脚本，并发调用 `agy-hud` 与 `agy-statusline`。
- `combined/agy-hud.config.json`：组合模式下的推荐配置模板（关闭重复的 Model/Context，开启 `showSpeed`）。
- `scripts/deploy-local.sh`：将仓库改动一键同步至 `~/.gemini/antigravity-cli/` 运行目录。

## 常用命令

```bash
# 运行全部单元测试
npm test

# 将代码同步到本地运行目录（保留已有 ~/.gemini/antigravity-cli/agy-hud-runtime/runtime/agy-hud.config.json）
./scripts/deploy-local.sh

# 如需连同 combined/agy-hud.config.json 一起覆盖同步：
./scripts/deploy-local.sh --sync-config
```
