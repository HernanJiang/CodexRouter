<p align="center">
  <img src="assets/release/codex-router-logo.png" alt="CodexRouter Logo" width="128">
</p>

<h1 align="center">CodexRouter</h1>

<p align="center"><strong>一个入口，连接你的全部模型、订阅账号与 API 渠道。</strong></p>

<p align="center">
  <img src="assets/release/codex-router-banner.png" alt="CodexRouter 项目横幅" width="100%">
</p>

<p align="center">
  <img src="https://img.shields.io/badge/version-3.1.11-0969da" alt="版本 3.1.11">
  <img src="https://img.shields.io/badge/platform-Windows%20%2F%20macOS%20%2F%20Linux-0078d4" alt="Windows / macOS / Linux">
  <img src="https://img.shields.io/badge/architecture-x64-555555" alt="x64">
  <img src="https://img.shields.io/badge/default%20build-portable-2ea44f" alt="默认构建便携版">
</p>

<p align="center">
  <a href="README.md"><strong>English README</strong></a>
</p>

<p align="center">
  <a href="#总体概览">总体概览</a> ·
  <a href="#模型路由">模型路由</a> ·
  <a href="#用量监控">用量监控</a> ·
  <a href="#下载与首次启动">下载</a> ·
  <a href="#安全与条款">安全</a>
</p>

CodexRouter 保留 Codex 原有工作方式，同时通过一个模型菜单接入不同服务商、OAuth 订阅账号与第三方 API 渠道。启用自动接续时，同名 OAuth 与 API 渠道合并为一个公开模型并优先使用订阅额度；关闭后则显示稳定分开的 API 与 OAuth 路由，使用相同的模型展示名称，由用户手动选择额度来源。

版本更新记录请查看 [GitHub Releases](https://github.com/HernanJiang/CodexRouter/releases)。

## 总体概览

<p align="center">
  <img src="assets/release/promotion.png" alt="CodexRouter 总体功能图" width="100%">
</p>

CodexRouter 是基于 CLIProxyAPI、Router Host 兼容层和 Rust 桌面控制台的 Windows 本地路由器。Windows x64 发布版提供自包含便携包，内置 CLIProxyAPI、Router Host、嵌入式 SQLite 状态层、Router 运行时、Gemini CLI 插件以及 app-local VC++ Runtime，服务默认只监听本机回环地址。

### 适合解决的问题

- 在 Codex 中继续原有工作，通过一个模型菜单切换服务商和账号。
- 优先使用订阅额度，订阅限额或故障时自动切换到 API 渠道继续工作。
- 在一个聚合面板中查看 OAuth 账号、Coding Plan、Token 用量、额度窗口、余额、重置时间和 API 使用统计。
- 为不同模型独立设置上下文、压缩、多模态和思考强度。
- 进入托盘后台运行，尽量降低后台资源占用。

## 模型路由

### 一个菜单，多个后端渠道

CodexRouter 可以将 OAuth 和 API 渠道合并为 Codex 可见的模型目录。启用自动接续时，同名模型只公开一次，后端优先级和容错链仍然独立保存；关闭自动接续时，API 与 OAuth 路由使用稳定的不同 ID 同时出现，但保持相同的模型展示名称，不再添加 `(OAuth)` 后缀，可直接通过模型选择控制额度来源。

<p align="center">
  <img src="assets/release/screenshot-codex.png" alt="Codex 模型菜单中的多模型切换" width="900">
</p>

### 同一上下文窗口无缝切换

直接在 Codex 模型菜单中切换模型，并继续使用同一上下文窗口。对话、工作目录和任务状态仍保留在 Codex 中，Router 只切换实际使用的后端渠道。你可以从订阅模型切换到 API 渠道，也可以在多个服务商之间切换，无需重新打开一个工作流。

### OAuth 订阅与 API 混合路由

- 支持 OpenAI/ChatGPT、Anthropic/Claude、Google Gemini、Google Antigravity 与 xAI/Grok 的 OAuth 登录入口。
- 登录后可查看账号套餐、状态、可用额度、重置时间以及平台实际发现的模型。
- 每次手动或后台自检都会刷新各 OAuth 账号实时声明的可用模型列表，并逐个检测当前已选订阅账号的实时额度。界面只展示该账号实际返回的模型，发现过程不会自动导入，只有用户逐个点击“＋ 模型”后才会加入。
- 已加入的 OAuth 模型可通过右键菜单从当前配置删除。“保存并应用”会尊重删除结果，不会由模型发现重新补回。
- 每套路由配置独立保存 OAuth 账号选择，只有用户已手动加入并启用的模型才参与当前路由。
- 开启自动接续时，同名模型优先使用订阅额度，限额或故障时转入较低优先级的 API Key 渠道；关闭时不自动转接，由模型列表中的独立条目决定额度来源。
- 上游提供重置时间时按实际时间恢复；无法取得可靠实时额度时执行账号级保底探测，成功后自动重新加入号池。Grok 的陈旧 billing 缓存只用于展示，必须由实时额度或当前模型的最小生成确认恢复。
- Codex 配置被外部更新后，自检会核对用户层与系统层绑定、当前本机网关端口和重试设置。两层都丢失时显示三按钮覆写窗；窗口保持前台焦点累计 3 秒会自动写回并重启 Codex，最小化、托盘或失焦时暂停，不会抢焦点。“恢复默认”会进入粘性官方模式，自检不会再次绑回 Router，直到用户主动重新开启转发。
- OAuth 令牌由 CLIProxyAPI 管理，不写入 CodexRouter 配置文件，也不提供明文导出。
- Codex Desktop 26.818 在 `requires_openai_auth = true` 时会并发刷新 ChatGPT refresh token，导致登录循环。3.0.2 起本地 Router provider 固定为 `Codex-Router` + `requires_openai_auth = false`：转发仍走本机网关，ChatGPT token 留在 Desktop 的 `auth.json` 中不由 Router 刷新。左下角显示 Codex-Router 是预期行为，不是登录丢失。详情见 [CHANGELOG](CHANGELOG.md)。
- 3.0.3：Grok 等第三方模型若在工具回合里只写「接下来…」不发 `function_call`，Codex 会当成任务结束。网关会扣住 `response.completed` 并自动续跑最多两次。登录身份不变。
- 3.0.4：长对话不再把 Grok 的 `max_output_tokens` 压成 1（那会触发 `Incomplete response returned, reason: max_output_tokens`）。输出保底为窗口 5% / Grok 128k。
- 3.0.5：Antigravity/Gemini 续跑旧线程时剥掉过期 thought carrier 和 `previous_response_id`，避免 Google 404 `Requested entity was not found`。
- 3.0.6：不再把 `no auth available` 空等 125 秒；那会让 Desktop 报 `error sending request` 并拖死所有模型。自动续跑仅限 Grok。
- 3.0.7：Antigravity 网页授权成功后，不再因为 `www.googleapis.com` userinfo TLS 超时丢掉 token（`CR-OAU-0008` / `exchange-code`）。ChatGPT 官方额度用尽时按优先级立刻改走中转，不再把整条 Sol 线路一起冷却。
- 3.0.8：Grok 写完整份 Verdict 后不再复读「任务已完成」；长篇报告里的「下一步」不再触发自动续跑。推理档位：Grok 4.6 增加 `xhigh`，Claude 4.6 Thinking 为 low/medium/high/max，GLM-5.2 为 high/max。
- 3.0.9：Grok 402 Payment Required 立刻换下一个账号/池。Grok 登录改由 Host 在 `127.0.0.1:56121/callback` 做 PKCE 换 token，授权页能真正拿到 xAI 登录。
- 3.0.10：Grok 400 `invalid-argument` 是 Codex Desktop 的 `mcp__codex_app__automation_update` schema（根上 `oneOf`/`$ref`）再加上 `max_output_tokens` 超过 128k。进 CLIProxy 前先把该工具 schema 换成空 object，Grok 输出硬上限 128k。
- 3.0.11：Claude Opus 最高思考档不再 400 `max_tokens` must be greater than `thinking.budget_tokens`。网关把输出抬到高于 CLIProxy 的 128k max budget。
- 3.0.12：Claude Opus/Sonnet 4.6+ 目录上下文改为 1M（95% 压缩点 950k），不再用未知模型的 128k，Desktop 也就不会只显示约 122k。
- 3.0.13：Gemini/Antigravity 额度 429 立刻换下一个 OAuth 账号，不再把整池冷却后再重试到 Codex 报 exceeded retry limit。
- 3.0.18：ChatGPT 5 小时额度用尽后立刻切到已配置的同名第三方 API。CLI 热推失败不再把全部号池停掉（避免 `503 no schedulable credential in pool`）。
- 3.1.0：关闭应用会清掉旧便携版留下的 Host 端口；Muse 超长 MCP 工具名不再 400。
- 3.0.22：Muse / Meta 按 CLIProxy 的 MCP 命名空间拼接缩短工具名，避免 400。
- 3.0.21：Muse / Meta 等第三方模型缩短超过 64 字符的 MCP 工具名，避免 400。
- 3.0.20：GLM-5.3-Flash 保留 `max`；Gemini / GLM 等第三方模型会把思考过程显示出来，不再只丢最终结果。
- 3.0.19：Windows 凭据按 UserData 命名空间隔离，避免和 CraftStation 抢 `CodexRouter/*` 钥匙。

### 按模型适配的控制项

不同模型可以独立设置默认上下文窗口、自动压缩阈值、图片输入能力和思考强度。主流模型的思考档位、上下文默认值、压缩比例和多模态开关都做了适配，点击即可使用推荐设置。尚未做内置适配的模型仍保留可编辑的手动设置窗口，不会被强制套用一组通用参数。

<p align="center">
  <img src="assets/release/feature-thinking-intensity.png" alt="按模型适配的思考强度菜单" width="620">
</p>

模型目录会对公开 ID 去重，同时保留后端冗余。GPT-5.6 Sol/Terra、Luna、Claude、Gemini、Grok、Kimi、GLM 等已配置模型可以保留各自服务商准确的思考和多模态行为。

## 用量监控

### 独立的多账号聚合监控平台

用量监控是 CodexRouter 的独立核心能力，不只是 OAuth 登录页面附带展示的一行状态。它专门聚合多个 OAuth 账号、API 渠道和 Coding Plan 的实时状态，集中展示：

- 订阅额度窗口和重置倒计时；
- 5 小时、每日、每周、每月 Coding Plan 额度；
- 配置控制面凭据后读取火山方舟 Coding Plan 周额度和月额度；
- Kimi、Grok、Z.ai/GLM、MiniMax、MiMo、OpenRouter、DeepSeek、ZenMux 等已支持渠道的用量；
- Token 总量、请求数、模型级用量、估算费用、余额和服务商错误状态；
- 服务商暂时拒绝或延迟查询时，使用有时效边界的最近一次成功数据，避免整块监控面板失效。

用量刷新现在采用有界并发和单任务截止时间，每个服务商独立完成或失败。Grok、Kimi 或某个 API 渠道变慢时只会独立超时，其他卡片仍可先返回；嵌套结构、比例字段和服务商专用字段也会统一归一化。

用量页面将 OAuth 订阅卡片与 API 用量卡片同时保留，按卡片内容动态填充为独立列，减少不同账号额度窗口数量不一致造成的大面积空白。

## 托盘与性能优化

CodexRouter 可以在 Windows 登录后直接进入轻量托盘模式，不启动额外守护进程。托盘模式暂停日志跟随、界面刷新和高频用量更新，保留每 60 秒一次的原生健康检查、连续失败后的无窗口恢复，以及每 3 分钟一次的统一自检。

当前运行时同时保留了内存和后台任务优化。空闲托盘状态下的 CPU、磁盘和网络活动设计为几乎可以忽略；下图展示了测试环境中 CodexRouter 进程处于 0% CPU、0 Mbps 网络占用的空闲状态。

<p align="center">
  <img src="assets/release/usage-performance.png" alt="CodexRouter 空闲资源占用" width="900">
</p>

## 下载与首次启动

前往 [GitHub Releases](https://github.com/HernanJiang/CodexRouter/releases/tag/v2.1.14) 下载 Windows x64 版本：

`Codex-Router-Portable-2.1.14-windows-x64.zip`

默认发布与本地交付只构建便携版。用户级 installer 仍保留为可选构建目标，仅在明确需要安装器时单独生成。

macOS / Linux 理论构建仍可通过仓库 workflow 从源码生成，但未在真实机器上测试。当前受支持的运行时仍是 Windows 10/11 x64。

断网、429 或瞬时上游错误默认重试 3 次，退避为 5s / 25s / 125s。每个任务累计等待最多 180 秒；即使自定义为 32 次，也不会进入 625 秒或单步 1 小时等待。Codex 取消任务后 Router 会立即停止该请求的重连；已开始输出后若无法安全续跑，会发送终止事件释放当前任务。

这是一个解压即用的便携包，不要求预装 Python、Node.js、Rust 或独立 VC++ Runtime。请完整解压后启动，不要只把 `Codex-Router.exe` 单独移出目录。

第一次打开会直接进入全流程引导的第一页，按项目、登录、模型、网络和部署步骤带你完成配置，减少第一次使用的学习成本，不需要另找一份安装手册。

<p align="center">
  <img src="assets/release/first-run-guide.png" alt="CodexRouter 首次启动全流程引导" width="100%">
</p>

### 快速开始

1. 解压完整便携包并打开 `Codex-Router.exe`；若 Windows 对未签名 EXE 显示 SmartScreen 提示，也可使用同目录的 `Start-Codex-Router.cmd` 启动壳。
2. 按首次启动引导添加第一个 API 渠道或登录 OAuth 订阅。
3. 将需要使用的模型加入当前路由配置。
4. 完整阅读内置的使用与分发条款，滚动到底并由你本人确认。
5. 应用配置，Router 会初始化本地服务并更新 Codex Provider 配置。
6. 回到 Codex，在同一上下文窗口内从模型菜单切换模型。

当前受支持的运行时是 Windows 10/11 x64，不支持 Windows ARM64。本版本中的 macOS 和 Linux 仍为理论目标，不包含在已发布的 Windows 包中。

## 安全与条款

- API Key、代理密码和本地 Router Key 通过 Windows Credential Manager 保存。
- OAuth Token 始终由 CLIProxyAPI 管理，不复制到 Router 配置文件。
- 发布包排除用户配置、日志、数据库、OAuth 状态、备份和开发机路径。
- 本地运行服务默认绑定 `127.0.0.1`，不建议将管理接口暴露到远程网络。
- 完整条款请查看 [中文条款](TERMS.zh-CN.md) 和 [English Terms](TERMS.en.md)。
- CodexRouter 原创部分仅授权个人、非商业使用；CLIProxyAPI、Gemini CLI 插件和其他第三方组件继续遵循各自的上游许可证和声明。

官方仓库：<https://github.com/HernanJiang/CodexRouter>

macOS 和 Linux 仍为理论目标，未经过实际测试，欢迎更多用户参与共同构建。
