# Codex-Router 更新记录

本文件记录面向用户的重要变化。完整技术细节以对应版本的源码和 GitHub Release 为准。

## 1.5.4 - 2026-08-10

### 改进

- 将模型目录生成、路由规划和 OAuth/API 合并拆分逻辑迁移到 Rust，`Build-ModelCatalog.ps1` 不再参与发布包。
- 开启自动连续时，匹配的 OAuth 和 API 渠道共用同一个公开模型 ID 并优先使用订阅额度；关闭时，API 模型与带 `(OAuth)` 后缀的订阅模型分开显示，允许手动选择额度来源。
- 修复旧目录生成器导致的重复 OAuth 账号条目和不稳定公开模型 ID。

### 验证

- Rust 完整测试、格式检查、静态检查和 Clippy 严格检查通过。
- 使用真实本机配置验证模型目录：OAuth/API 合并与拆分模式均生成正确列表，未出现重复账号。

### 发布物

- `Codex-Router-Portable-1.5.4-windows-x64.zip`
- `Codex-Router-Installer-1.5.4-windows-x64.exe`

## 1.5.3 - 2026-08-09

### 改进

- 将用量查询、本机 Sub2API 管理读取、服务商响应归一化、有界并发和最近成功额度缓存迁移到 Rust，不再为每次刷新启动 PowerShell 进程。
- Kimi、Grok、Z.ai/GLM、MiniMax、ZenMux、火山方舟、MiMo、OpenRouter 和 DeepSeek 继续使用各自的官方配额或余额接口，并保持单渠道失败不影响整个面板。
- 用量查询直接按配置中的凭据引用读取 Windows Credential Manager；错误信息只保留安全分类，不输出 Key、Token、Cookie 或账号身份。
- 便携包和安装包不再包含 `Get-UsageMonitor.ps1`，减少运行时 PowerShell 文件和后台进程开销。

### 验证

- Rust 完整测试、格式检查、静态检查和 Clippy 严格检查通过。
- 使用现有本机数据进行只读真实查询，成功返回 3 个订阅账号和 5 个 API 渠道；Kimi 返回可读额度窗口，订阅记录未出现 `class=request_failure`。

### 发布物

- `Codex-Router-Portable-1.5.3-windows-x64.zip`
- `Codex-Router-Installer-1.5.3-windows-x64.exe`

## 1.5.2 - 2026-08-09

### 修复

- 修复活动 Codex 连接存在时“保存并应用”被错误延后的问题。配置可进行非破坏性初始化，只有确实需要代理切换或服务重启时才执行连接保护。
- 修复 `lifecycle_deferred` 日志被重复归类为 `configuration`、`database` 或 `redis` 的误报。
- 修复未输入新 API Key 时仍显示“已安全保存”的误导提示；现在会显示实际更新的 Key 数量，或明确说明保留现有凭据。
- 修复 Kimi Coding Plan 实时用量错误分类，将 401 凭据拒绝、403 权限不足和限流分别处理，并保留有边界的最近成功额度。
- 修复部分 OAuth 账号刷新后重复显示，以及 Google、Grok 等渠道的短暂请求失败影响整个用量面板的问题。

### 改进

- 关闭自动接续后，OAuth 订阅模型会以 `(OAuth)` 标记与同名第三方 API 模型分开显示，可直接从模型列表选择额度来源。
- 用量刷新采用有界并发、独立截止时间和重试退避，单个服务商变慢或失败不会阻塞其他卡片。
- Kimi 模型编辑器新增“保存新 Key 与模型”状态，并明确提示还需点击“保存并应用”才会覆写 Windows 凭据。
- 加强本机 Router 生命周期恢复、OAuth 路由、安装器和便携发布物的自动化回归覆盖。

### 发布物

- `Codex-Router-Portable-1.5.2-windows-x64.zip`
- `Codex-Router-Installer-1.5.2-windows-x64.exe`
