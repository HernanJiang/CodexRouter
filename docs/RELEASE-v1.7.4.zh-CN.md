# CodexRouter v1.7.4

发布日期：2026-08-16

这是当前最新可用稳定版。1.7.2 到 1.7.4 的修复已一并纳入：第三方模型长对话、兼容网关、用量展示和模型目录顺序。

## 本版本重点

- 普通生成不再因为历史条数自动做本地压缩。Gemini / Grok 长对话不会再在“上下文已完成本地压缩与恢复”后停住；只有 Codex 明确调用 `/compact` 时才做摘要压缩。
- 启动和自检会重新拉起 Responses 兼容网关（18082），避免热替换或重启后网关消失。
- 延续 1.7.3：用量页同套餐合并、路由区展示渠道用量、Codex 模型列表顺序与 Router 自上而下顺序一致。
- 延续 1.7.2 / 1.7.3：默认中文、DeepSeek 可在 ChatGPT 登录下调用、会话隔离、Kimi 文本函数转写、授权误报拆分。

升级后请完全重启 Codex-Router，并完全重启 Codex。

## 下载

GitHub Release 提供以下产物：

- `Codex-Router-Installer-1.7.4-windows-x64.exe`：Windows 安装版
- `Codex-Router-Portable-1.7.4-windows-x64.zip`：Windows 便携版
- `SHA256SUMS.txt`：产物校验值

当前受支持的运行时仍是 Windows 10/11 x64。macOS / Linux 理论构建可在源码仓库通过 `Theoretical Unix builds` workflow 生成，本稳定版优先发布已验证的 Windows 安装包和便携版。

## 安装说明

Windows 安装包 `Codex-Router-Installer-1.7.4-windows-x64.exe` 会打开 CodexRouter 安装向导：

1. 选择安装位置，默认路径为 `%LOCALAPPDATA%\Programs\CodexRouter\1.7.4`
2. 默认勾选“创建桌面快捷方式（同时加入开始菜单）”
3. 点击“确认安装”后才开始复制文件
4. 安装不会覆盖个人配置和运行数据

便携版仍可解压后直接运行。请完整解压后启动，不要只把 `Codex-Router.exe` 单独移出目录。

## 已知限制

Windows 产物当前未进行代码签名，首次运行时可能出现 SmartScreen 提示。请从项目 GitHub Release 下载，并使用 `SHA256SUMS.txt` 校验文件完整性。

---

# CodexRouter v1.7.4

Release date: 2026-08-16

This is the latest available stable release. It includes the 1.7.2–1.7.4 fixes for third-party long conversations, the compatibility gateway, usage monitoring, and model-catalog order.

## Highlights

- Ordinary generations no longer auto-compact locally just because the history is long. Gemini / Grok conversations no longer stop after “local compaction completed”; summary compaction runs only when Codex explicitly calls `/compact`.
- Startup and self-check relaunch the Responses compatibility gateway on port 18082, so it no longer disappears after a hot replace or restart.
- Continues 1.7.3: same-plan usage cards, channel usage in the routing panel, and Codex model-list order matching the Router list from top to bottom.
- Continues 1.7.2 / 1.7.3: stable Simplified Chinese by default, DeepSeek usable under a ChatGPT login, session isolation, Kimi leaked-text function rewriting, and split auth-error reporting.

Fully restart Codex-Router and Codex after upgrading.

## Downloads

GitHub Release provides these artifacts:

- `Codex-Router-Installer-1.7.4-windows-x64.exe`: Windows installer
- `Codex-Router-Portable-1.7.4-windows-x64.zip`: Windows portable package
- `SHA256SUMS.txt`: artifact checksums

The currently supported runtime remains Windows 10/11 x64. Theoretical macOS / Linux builds can still be produced from the repository via the Theoretical Unix builds workflow. This stable release prioritizes the verified Windows installer and portable package.

## Setup

The Windows installer `Codex-Router-Installer-1.7.4-windows-x64.exe` opens the CodexRouter setup wizard:

1. Choose the install location. The default path is `%LOCALAPPDATA%\Programs\CodexRouter\1.7.4`
2. "Create a desktop shortcut (also add to the Start menu)" is checked by default
3. Files are copied only after you click "Confirm install"
4. Installation does not overwrite personal configuration or runtime data

The portable package can still be extracted and run directly. Extract the complete directory before launching it. Do not move only the GUI executable out of the package.

## Known limitations

Windows artifacts are currently unsigned. SmartScreen may appear on first run. Please download from the project GitHub Release and verify integrity with `SHA256SUMS.txt`.
