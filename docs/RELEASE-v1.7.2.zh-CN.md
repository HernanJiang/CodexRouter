# CodexRouter v1.7.2

修复第三方模型自动压缩死循环、子 Agent 加密函数输出解密失败，以及随后全模型 502 把 Codex 卡死的问题。

## 用户可见变化

- 使用 Grok 等第三方模型时，自动压缩不再因为上游 `ModelInput` 422 而一直显示“正在自动压缩上下文”。
- 子 Agent 使用 ChatGPT 以外模型，或同一对话在第三方模型与 ChatGPT 之间切换时，不再因为明文被当成加密函数输出而断流。
- 这类协议失败不再被包装成 Router 级 `502 Bad Gateway`，避免主对话卡住、新对话也全部失败。
- 升级后请完全重启 Codex-Router，并完全重启 Codex。

## 安装包

- `Codex-Router-Installer-1.7.2-windows-x64.exe`：Windows 安装版
- `Codex-Router-Portable-1.7.2-windows-x64.zip`：Windows 便携版
- `CodexRouter-1.7.2-linux-x64-theoretical.tar.gz`：Linux 理论构建
- `CodexRouter-1.7.2-macos-arm64-theoretical.tar.gz`：macOS Apple Silicon 理论构建
- `CodexRouter-1.7.2-macos-x64-theoretical.tar.gz`：macOS Intel 理论构建

---

# CodexRouter v1.7.2

Fixes infinite auto-compaction on third-party models, encrypted function-output decrypt failures in mixed/sub-agent turns, and the follow-on router-wide 502 that froze Codex.

## User-visible changes

- Auto-compaction on Grok and other third-party models no longer loops on upstream `ModelInput` 422 errors.
- Sub-agents on non-ChatGPT models, or threads that switch between those models and ChatGPT, no longer fail with `Encrypted function output content could not be decrypted or decoded`.
- Those protocol failures are no longer returned as a router-wide `502 Bad Gateway`, so a bad turn cannot stall every new conversation.
- Fully restart Codex-Router and Codex after upgrading.

## Packages

- `Codex-Router-Installer-1.7.2-windows-x64.exe`: Windows installer
- `Codex-Router-Portable-1.7.2-windows-x64.zip`: Windows portable package
- `CodexRouter-1.7.2-linux-x64-theoretical.tar.gz`: Linux theoretical build
- `CodexRouter-1.7.2-macos-arm64-theoretical.tar.gz`: macOS Apple Silicon theoretical build
- `CodexRouter-1.7.2-macos-x64-theoretical.tar.gz`: macOS Intel theoretical build
