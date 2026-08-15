# Codex-Router v1.7.0

发布日期：2026-08-14

## 本版本重点

- Codex 模型目录使用 Router 配置中的非空模型别名作为 `display_name`，内部路由仍使用 Model ID。比如 Model ID 为 `grok-4.6` 时，界面显示配置的 `Grok-4.6`。
- 请求重试和 Streaming 重连默认次数统一为 `5`，避免不同配置加载路径继续使用旧的 `2`。
- 增加可重复的 TTFT 分段测量脚本 `scripts/Test-TTFT.ps1`，用于区分上游首 Token 等待和本地 Streaming 开销。
- 清理已确认无运行时依赖的旧 `cc-switch-0.4.3` 归档和废弃 Python GUI，实现与当前 Rust GUI、安装脚本和构建流程保持一致。

## TTFT 测量结论

本版本按 Codex 客户端、Router、上游模型、首个 SSE/语义事件和客户端接收链路逐段测量。Router 首 Token 测得 `0.929–4.401s`，Router 向客户端转发首语义事件的额外开销为 `7–80ms`；一次 Codex CLI 首语义事件测得 `2.591s`。没有发现 Router 本地 Streaming、同步等待或阻塞造成十几秒延迟，主要等待来自上游模型服务的首 Token。

## 验证结果

- Acceptance：`18 passed / 0 failed`
- Rust：`261 passed / 0 failed / 2 ignored`
- `cargo fmt`、`cargo check --locked`、`cargo clippy --locked --all-targets -- -D warnings`
- Python 测试、7 组 PowerShell 集成测试、生产构建
- PostgreSQL、Redis、Sub2API 生命周期和真实 Streaming 测试
- 四路并发流量测试：`4/4` 成功，墙钟 `8.35s`，CPU 增量 `0.078s`，峰值工作集 `102.4 MiB`，句柄净增 `2`
- Installer `/Q` 实际安装成功，耗时约 `3.10s`；Portable 和 Installer GUI 启动耗时约 `255ms / 218ms`
- Clean Source、安装器载荷、Portable 压缩包和实际安装目录均通过敏感信息扫描

## 下载与安装

GitHub Release 提供以下 Windows x64 产物：

- `Codex-Router-Installer-1.7.0-windows-x64.exe`：安装版
- `Codex-Router-Portable-1.7.0-windows-x64.zip`：便携版，解压后直接运行
- `SHA256SUMS.txt`：产物校验值

首次运行请根据界面向导填写自己的 Router、上游服务和 Codex 配置。公开发布包不包含实验环境、个人 UserData、API Key、Token、Cookie、Session 或本机路径。

## 已知限制

Windows 产物当前未进行代码签名，首次运行时可能出现 SmartScreen 提示。请从项目 GitHub Release 下载，并使用 `SHA256SUMS.txt` 校验文件完整性。
