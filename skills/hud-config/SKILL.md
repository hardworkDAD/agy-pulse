---
name: hud-config
description: 当用户想要配置或自定义 agy-pulse（兼容 agy-hud）状态栏的显示设置（上下文监控、主题、图标、面板模式、语言等）时触发。
---

# 交互式配置 agy-pulse 状态栏

当用户要求修改、设置、美化或配置 `agy-pulse` / `agy-hud` 状态栏时，请遵循以下流程：

1. **发起交互选择题**：
   使用 `ask_question` 工具对用户发起提问，包含以下选项让用户进行单选：
   - **保存范围**：局部项目配置 (Local) 还是 全局默认配置 (Global)
   - **颜色主题**：翡翠绿 (Emerald Green)、深海蓝 (Ocean Blue)、赛博朋克 (Cyberpunk Magenta)、琥珀金 (Amber Gold)
   - **显示模式**：表格模式 (Table - 详尽多模型对比)、紧凑模式 (Compact - 单行迷你图) 还是 独立模型表 (Models)
   - **底部实时上下文与压缩监控行 (`showContextMonitor`)**：开启 (推荐) 还是 关闭
   - **图标字体**：自动检测 UTF-8 (推荐)、Nerd Fonts、Unicode Emojis 还是 纯 ASCII (兼容低端终端)
   - **界面语言 (`language`)**：自动 (`auto`)、简体中文 (`zh`) 还是 英文 (`en`)

2. **写入配置文件**：
   根据用户的选择，读取当前配置（或新建配置），并使用文件修改工具将配置保存到对应的配置文件中（优先写入 `agy-pulse.config.json`，若旧项目已有 `agy-hud.config.json` 也可就地更新）：
   - 局部路径：当前项目根目录的 `./agy-pulse.config.json`（兼容 `./agy-hud.config.json`）
   - 全局路径：用户主目录下的 `~/.gemini/antigravity-cli/agy-pulse-runtime/runtime/agy-hud.config.json`（若存在旧目录 `~/.gemini/antigravity-cli/agy-hud-runtime/runtime/agy-hud.config.json` 建议同步更新或提示运行部署脚本）
   - 若用户希望使用终端交互向导，也可提示运行 `node ~/.gemini/antigravity-cli/agy-pulse-runtime/runtime/bin/agy-pulse.js --config`。
