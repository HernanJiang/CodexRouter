<p align="center">
  <a href="README.md"><strong>English README</strong></a>
</p>

<p align="center">
  <img src="assets/release/codex-router-logo.png" alt="Codex-Router Logo" width="128">
</p>

<h1 align="center">Codex-Router</h1>

<p align="center"><strong>一个入口，连接你的全部模型、订阅账号与 API 渠道。</strong></p>

<p align="center">
  <img src="assets/release/codex-router-banner.png" alt="Codex-Router 项目横幅" width="100%">
</p>

<p align="center">
  <img src="https://img.shields.io/badge/version-1.6.11-0969da" alt="版本 1.6.11">
  <img src="https://img.shields.io/badge/platform-Windows%2010%20%2F%2011-0078d4" alt="Windows 10/11">
  <img src="https://img.shields.io/badge/architecture-x64-555555" alt="x64">
  <img src="https://img.shields.io/badge/runtime-portable-2ea44f" alt="便携运行">
</p>

<p align="center">
  <a href="#总体概览">总体概览</a> ·
  <a href="#模型路由">模型路由</a> ·
  <a href="#用量监控">用量监控</a> ·
  <a href="#下载与首次启动">下载</a> ·
  <a href="#安全与条款">安全</a>
</p>

Codex-Router 保留 Codex 原有工作方式，同时通过一个模型菜单接入不同服务商、OAuth 订阅账号与第三方 API 渠道。启用自动接续时，同名 OAuth 与 API 渠道合并为一个公开模型并优先使用订阅额度；关闭后则显示稳定分开的 API 与 OAuth 路由，使用相同的模型展示名称，由用户手动选择额度来源。

## 总体概览

<p align="center">
  <img src="assets/release/promotion.png" alt="Codex-Router 总体功能图" width="100%">
</p>

Codex-Router 是基于 Sub2API 和 Rust 桌面控制台的 Windows 本地路由器。便携包内置 PostgreSQL、Redis、Sub2API、Router 运行时以及 app-local VC++ Runtime，服务默认只监听本机回环地址。

### 适合解决的问题

- 在 Codex 中继续原有工作，通过一个模型菜单切换服务商和账号。
- 优先使用订阅额度，订阅限额或故障时自动切换到 API 渠道继续工作。
- 在一个聚合面板中查看 OAuth 账号、Coding Plan、Token 用量、额度窗口、余额、重置时间和 API 使用统计。
- 为不同模型独立设置上下文、压缩、多模态和思考强度。
- 进入托盘后台运行，尽量降低后台资源占用。

## 模型路由

### 一个菜单，多个后端渠道

Codex-Router 可以将 OAuth 和 API 渠道合并为 Codex 可见的模型目录。启用自动接续时，同名模型只公开一次，后端优先级和容错链仍然独立保存；关闭自动接续时，API 与 OAuth 路由使用稳定的不同 ID 同时出现，但保持相同的模型展示名称，不再添加 `(OAuth)` 后缀，可直接通过模型选择控制额度来源。

<p align="center">
  <img src="assets/release/screenshot-codex.png" alt="Codex 模型菜单中的多模型切换" width="900">
</p>

### 同一上下文窗口无缝切换

直接在 Codex 模型菜单中切换模型，并继续使用同一上下文窗口。对话、工作目录和任务状态仍保留在 Codex 中，Router 只切换实际使用的后端渠道。你可以从订阅模型切换到 API 渠道，也可以在多个服务商之间切换，无需重新打开一个工作流。

### OAuth 订阅与 API 混合路由

- 支持 Sub2API 当前提供的 OpenAI/ChatGPT、Anthropic/Claude、Google Gemini、Google Antigravity 与 xAI/Grok 登录入口。
- 登录后可查看账号套餐、状态、可用额度、重置时间以及平台实际发现的模型。
- 每次手动或后台自检都会刷新各 OAuth 账号实时声明的可用模型列表。界面只展示该账号实际返回的模型，发现过程不会自动导入，只有用户逐个点击“＋ 模型”后才会加入。
- 已加入的 OAuth 模型可通过右键菜单从当前配置删除。“保存并应用”会尊重删除结果，不会由模型发现重新补回。
- 每套路由配置独立保存 OAuth 账号选择，只有用户已手动加入并启用的模型才参与当前路由。
- 开启自动接续时，同名模型优先使用订阅额度，限额或故障时转入较低优先级的 API Key 渠道；关闭时不自动转接，由模型列表中的独立条目决定额度来源。
- 上游提供重置时间时按实际时间恢复；无法取得重置时间时执行低频保底探测，成功后自动回切。
- OAuth 令牌由 Sub2API 管理，不写入 Codex-Router 配置文件，也不提供明文导出。

### 按模型适配的控制项

不同模型可以独立设置默认上下文窗口、自动压缩阈值、图片输入能力和思考强度。主流模型的思考档位、上下文默认值、压缩比例和多模态开关都做了适配，点击即可使用推荐设置。尚未做内置适配的模型仍保留可编辑的手动设置窗口，不会被强制套用一组通用参数。

<p align="center">
  <img src="assets/release/feature-thinking-intensity.png" alt="按模型适配的思考强度菜单" width="620">
</p>

模型目录会对公开 ID 去重，同时保留后端冗余。GPT-5.6 Sol/Terra、Luna、Claude、Gemini、Grok、Kimi、GLM 等已配置模型可以保留各自服务商准确的思考和多模态行为。

## 用量监控

### 独立的多账号聚合监控平台

用量监控是 Codex-Router 的独立核心能力，不只是 OAuth 登录页面附带展示的一行状态。它专门聚合多个 OAuth 账号、API 渠道和 Coding Plan 的实时状态，集中展示：

- 订阅额度窗口和重置倒计时；
- 5 小时、每日、每周、每月 Coding Plan 额度；
- 配置控制面凭据后读取火山方舟 Coding Plan 周额度和月额度；
- Kimi、Grok、Z.ai/GLM、MiniMax、MiMo、OpenRouter、DeepSeek、ZenMux 等已支持渠道的用量；
- Token 总量、请求数、模型级用量、估算费用、余额和服务商错误状态；
- 服务商暂时拒绝或延迟查询时，使用有时效边界的最近一次成功数据，避免整块监控面板失效。

用量刷新现在采用有界并发和单任务截止时间，每个服务商独立完成或失败。Grok、Kimi 或某个 API 渠道变慢时只会独立超时，其他卡片仍可先返回；嵌套结构、比例字段和服务商专用字段也会统一归一化。

用量页面将 OAuth 订阅卡片与 API 用量卡片同时保留，按卡片内容动态填充为独立列，减少不同账号额度窗口数量不一致造成的大面积空白。

## 托盘与性能优化

Codex-Router 可以在 Windows 登录后直接进入轻量托盘模式，不启动额外守护进程。托盘模式暂停日志跟随、界面刷新和高频用量更新，保留每 60 秒一次的原生健康检查、连续失败后的无窗口恢复，以及每 10 分钟一次的统一自检。

1.6.11 同时保留了内存和后台任务优化。空闲托盘状态下的 CPU、磁盘和网络活动设计为几乎可以忽略；下图展示了测试环境中 Codex-Router 进程处于 0% CPU、0 Mbps 网络占用的空闲状态。

<p align="center">
  <img src="assets/release/usage-performance.png" alt="Codex-Router 空闲资源占用" width="900">
</p>

### 1.6.11 更新重点

- OAuth 账号页现在通过 Rust 原生 Sub2API 管理客户端读取账号和实时模型目录。无 PowerShell 的最终发布包不再依赖已移除的 `Get-OAuthAccounts.ps1`，实际可用的 Grok、Antigravity 与 ChatGPT 账号不会再被误显示为没有声明模型或目录无法读取。
- 同时支持 Sub2API 的两种真实响应：稳定账号目录返回的 `data` 模型对象数组，以及上游同步返回的 `data.models` 模型 ID 数组。ChatGPT 不支持同步时使用稳定目录；Grok 与 Antigravity 会先同步上游再读取当前账号目录。
- 瞬时刷新失败会保留上次成功模型列表并提示重试。手动刷新与每 10 分钟统一自检只更新可选目录，不会批量把订阅模型加入当前配置。
- 新安装默认打开五阶段引导的第一页；已经完成有效配置的用户仍直接进入控制台。
- 特别适配 Google Antigravity OAuth，可使用具备资格的 Google AI Pro 账号提供的 Antigravity 订阅额度；实际可用模型和额度以账号实时声明及 Google 上游状态为准。

### 1.6.10 变更回顾

- 保持 ChatGPT 登录时也能直接使用 DeepSeek、Kimi、Claude、Gemini、Grok 等第三方模型。若其他程序覆写 Codex 配置，启动、手动刷新和每 10 分钟自检会恢复本机 Router provider、bearer 和模型目录，同时保留当前模型、ChatGPT 认证与用户设置，不关闭或重启 Codex。
- OAuth 账号卡片会展示账号实时声明的可用模型；刷新失败时保留上次成功目录并提示重试，不会把账号误显示为空，也不会自动把全部订阅模型加入当前配置。
- 模型编辑页新增“添加订阅账号模型”，会进入 OAuth 账号页并触发自检刷新。底部按钮按“取消、添加订阅账号模型、保存模型”排列，订阅模型仍由用户逐个明确添加。
- Antigravity 的稀疏账号映射现在会把 Gemini 3.1 Pro 兼容名称稳定规范到可执行的 `gemini-pro-agent`，修复 Codex 中的 `Upstream request failed`，同时保留明确的自定义映射。
- `gemini-3.7-flash` 及 Low、Medium、High 执行档在动态价格缺失时使用非零 Flash 计费回退，不再在成功响应后产生 `pricing not found` 日志。

### 1.6.9 更新重点

- Antigravity Responses 同时使用网页搜索与函数工具时，不再在输出前返回上游失败。由于 Antigravity 的 `cloudcode-pa` 端点拒绝这种组合，Router 只在 Antigravity 边界保留可执行函数并移除服务端搜索；Google AI Studio 请求仍原样保留两类工具。
- 对外统一的 `gemini-3.7-flash` 会映射到账号实时声明的 `gemini-3.7-flash-medium`，与 Antigravity 官方默认的 Medium 档一致。账号返回的 High、Medium、Low 三个分档会归并为一个用户可选模型，不再误显示成三个无关模型。
- Antigravity 中使用相同上游用量和重置签名的条目只按真实额度池显示一次，并隐藏内部 `chat_*`、`tab_*` 条目。同一渠道端点且使用同一真实 Key 的 API 或 Coding Plan 模型也合并为一个额度展示，包括 Kimi for Coding 与 Kimi K3。
- Grok OAuth 默认账号映射与调度器正式识别订阅账号实时声明的 `grok-4.6`。
- OAuth 模型刷新改用账号实时上游同步接口。手动刷新与每 10 分钟自检只更新可选目录，仍需用户明确点击添加才会进入配置。

### 1.6.8 更新重点

- Antigravity 的 Gemini 3.1 Pro High、Claude Fable 5 等共享模型在某个重复 OAuth 账号被隔离、另一个已选账号健康时，会由健康账号接管路由，不再因首个账号不可调度而出现没有可用后端。
- 每次手动刷新或每 10 分钟自检都会刷新各 OAuth 账号的实时模型目录。已移除静态服务商建议和手动绕过入口，界面不会再展示账号未声明的模型；上游新开放模型会自动出现在可选列表，但不会自动加入配置。
- 已加入的 OAuth 模型支持在标签上右键删除，删除精确到账号；另一个账号的同名模型会保留，只有最后一个 OAuth 同名模型删除后才清理对应 fallback 选择。
- 陶土主题的“当前配置 OAuth”按钮改为与“保存并应用”一致的咖啡色，不再显示为绿色。

### 1.6.7 变更回顾

- Grok 与 Gemini / Antigravity 的最新用量结果为 `healthy / active` 时，会清除账号卡片中已被成功探测证伪的历史 `request_failure`。真实认证、权限、限流、禁用和额度耗尽状态仍会保留并给出对应操作提示。
- 单个账号的临时上游探测失败不再误报为整个本地 OAuth 服务不可用；提示会明确指出受影响账号，并说明自检系统将自动重试。
- 控制台概览卡与右侧模型区加活动日志的底边对齐；模型名称与路由标签间距进一步收紧，默认窗口可完整显示 3 个模型。代理、更新、语言和主题控件恢复白色底色，当前语言仍使用深蓝选中态。

### 1.6.6 变更回顾

- 从 1.6.4 / 1.6.5 升级时，只移除从未由用户选择的旧版批量 OAuth 目录条目；API 渠道、手动加入的 OAuth 模型、改名模型和小规模既有选择均会保留。刷新账号与“保存并应用”都不会补回用户已删除的目录模型。
- 控制台导航操作统一靠右；顶部横栏使用较深雾蓝色，中英文切换改为两个互不覆盖的等宽分段，五阶段教学滑块会利用代理控件左侧的可用空间。
- 路由模型卡进一步压缩，默认窗口可同时查看约 2.5 个模型；活动日志获得完整固定区域，不再与窗口底部或署名重叠。
- Antigravity 中服务端额度签名相同的模型合并为一条共享额度；使用比例或重置窗口不同的模型仍分别展示。

### 1.6.5 变更回顾

- 用量自检不再读取或导入 OAuth 模型目录。刷新 OAuth 页面只展示账号可用模型，必须由用户逐个点击“＋ 模型”才会加入；“保存并应用”尊重用户删除结果，不会再次补回几十个订阅模型。
- 默认窗口改为紧凑控制台尺寸；原“控制台 5/5”替换为五阶段新手教学进度滑块。
- 实时用量统计、常见渠道快速配置、切换配置分组和当前配置 OAuth 集中在控制台标题右侧；“保存并应用”与“＋ 添加新模型”位于路由配置卡片标题栏，查看导航与配置写操作分区显示。
- 模型卡将 OAuth 渠道、独立/兜底状态和托管说明合并为一个路由标签。模型列表使用活动日志上方的全部剩余高度，默认同时显示约 3 个模型；长模型名会省略显示，不会覆盖右侧操作按钮。
- 默认窗口下的英文顶部栏自动使用紧凑标签，避免长英文按钮与五阶段教学滑块重叠。

### 1.6.4 变更回顾

- OAuth 恢复观察中的过期时间不再触发 1 秒重试循环。自动恢复探测最短间隔为 10 分钟，未知状态最长 5 小时的安全恢复上限保持不变。
- 新增 OAuth 账号恢复、fallback 同步和 Grok 4.6 手动建议项；后续 1.6.5 已禁止用量自检自动导入账号模型目录。
- 账号不再处于“已选择 OAuth 且存在同模型 API fallback”的配置时，对应陈旧恢复观察会自动移除。维护观察文件不再被误判成真实路由变化，也不会重复触发路由同步和第二次用量查询。
- 用量查询、OAuth 账号读取、路由同步、配置应用、OAuth 登录或服务健康恢复正在使用本地管理接口时，后台恢复会静默顺延，避免管理请求争用导致 OAuth 页面暂时显示“无法读取 OAuth 账号”；手动刷新仍会立即启动统一自检。
- OAuth 恢复与 fallback 切换仍然只热更新本机 Router 后端，不改写当前 Codex 配置，不关闭或重启 Codex / ChatGPT，也不会中断当前任务。

### 1.6.3 变更回顾

- 恢复 1.5.2 已验证的 Codex 登录行为：应用 Router 后继续使用原有 ChatGPT 登录，provider 显示为 `Codex-Router` 并保持 `requires_openai_auth = true`；本机 Router Key 仍只用于本地转发，自定义模型目录会在同一登录界面中加载。
- 已保存配置现在同时提供“直接应用”和“删除”；删除前会二次确认，并且只删除对应配置快照与隔离 API 凭据，不会删除 OAuth 账号或改动当前 Codex 配置。当前正在使用的配置必须先切换或初始化后才能删除。
- “恢复 Codex 默认配置”只移除 Router 自己写入的路由字段，不会删除非 ChatGPT 认证文件或未来版本的未知认证格式。
- OpenAI、Claude、Gemini、Antigravity 和 Grok 的 OAuth 授权链接统一交给 Windows 默认 HTTPS 浏览器打开。添加 Grok 等平台的第二个账号时会保留账号选择参数和完整长 URL，不会再误开“文档”目录。

- 启用配置隔离后，手动保存、OAuth 登录后的自动同步和 Router 模式启用都必须绑定到一个真实存在的配置分组。后台自检只读检查配置绑定并提醒，不会自动覆写 Codex `config.toml` 或模型目录，避免后台动作触发客户端重载。
- `chatgpt_oauth` 模式继续使用稳定的 `codex_router` provider ID 和 `Codex-Router` 展示名；兼容应用和安装脚本也会写入与 1.5.2 一致的账号契约，不会再次覆盖成第三方 API 身份。
- 应用新配置后会先优雅关闭 Codex Desktop；只有超时未退出的已验证进程才会按子进程到父进程顺序终止，已经自然退出的 Electron 子进程不会再导致整次重启误报失败。
- Microsoft Store/MSIX 版 Codex 使用官方 AUMID 通过 `shell:AppsFolder` 重新激活，不再直接执行受保护的 `WindowsApps` 内部 EXE；重新打开后会加载最新模型目录。
- `chatgpt_oauth` 模式明确保留 ChatGPT 登录方式；若文件型登录状态意外缺失，只会从当前 Windows 用户可解密且校验有效的最近快照恢复，绝不覆盖已有认证文件。Router 请求仍使用本机 Key，不会把 ChatGPT Token 当作 Router Key。

- 用量查询、本机 Sub2API 读取、服务商响应归一化、有界并发和最近成功额度缓存已迁移到 Rust，刷新用量时不再启动 PowerShell 进程。
- Kimi、Grok、Z.ai/GLM、MiniMax、ZenMux、火山方舟、MiMo、OpenRouter 和 DeepSeek 继续独立刷新；单个服务商失败不会使整个面板失败。
- 用量查询直接按软件配置中的引用读取 Windows Credential Manager，Key、Token、Cookie 和账号身份不会进入日志或测试夹具。
- 模型目录生成、路由规划和 OAuth/API 合并拆分逻辑已迁移到 Rust；Codex 看到的模型列表由 GUI 直接生成，不再依赖 PowerShell 脚本。
- 开启自动连续时，匹配的 OAuth 和 API 渠道共用同一个公开模型 ID 并优先使用订阅额度；关闭时，API 与 OAuth 路由使用稳定的不同 ID 分开显示，但保持相同的模型展示名称，方便手动选择额度来源。
- 修复了旧目录生成器导致的重复 OAuth 账号条目和不稳定的公开模型 ID。
- Codex TOML 生成、校验、权限设置保留、备份轮换和原子写入已迁移到 Rust；关闭自动接续时，同名 API 默认模型会写入稳定的独立公开 ID，不会误落到 OAuth 渠道。
- OAuth 单账号用量请求会在显示 `class=request_failure` 前对瞬时故障执行有边界重试。
- Grok 与 Antigravity 实时额度成功后只清理由探测产生的可恢复历史错误。Antigravity 额度查询使用当前 Token Provider，遇到 401 时刷新并仅重试一次；即使本机模型名已经过期，也会按上游本次返回的实时模型目录展示额度。
- Kimi `k3-256k` 的上下文限制响应不再被误判为 Key 失效，也不会永久禁用有效的 Coding Plan 账号。
- 每次手动或后台用量查询都会顺带检查已选账号的恢复状态；只有实时额度查询成功、额度尚未耗尽且凭据未被拒绝时才重新启用，避免误禁用长期残留，也不会用缓存额度误恢复失效账号。
- 默认每 10 分钟在后台运行统一自检，并执行 OAuth 健康检查、用量查询、账号恢复和备用路由维护，轻量托盘模式也不会停用。实时查询确认 OAuth 额度已耗尽且存在同模型 API 备用渠道时，会立即把该 OAuth 账号设为不可调度；只有后续实时额度确认恢复后才会重新启用；未知状态最长隔离 5 小时。后台发现 Codex 配置被外部覆盖时只提醒，不自动写文件。
- OAuth 到 API fallback、恢复以及后台发现新 OAuth 账号后的同步都是本机 Router 的实时后端路由变更，不会关闭或重启 Codex / ChatGPT，因此当前任务和对话不会中断。Codex-Router 只弹窗提醒额度变化；只有用户明确点击完整“保存并应用”或切换配置时才可能需要重启 Codex。OAuth 与 API 默认共用同一个模型展示名称，不追加 `(OAuth)` 后缀。
- OAuth 账号优先级更新、账号恢复、OAuth 登录、配置应用以及 PostgreSQL、Redis、Sub2API 生命周期均由 Rust 原生执行；Redis 就绪必须通过带密码的 `PONG`。
- 更新器会校验官方 GitHub 地址、SHA-256 和发布清单，显示实时下载进度；下载完成后由独立 Rust 助手执行原子替换、失败回滚和自动重启。
- 本机 Router Key 在管理接口隐藏完整值时仍按受管名称和分组幂等识别，重复应用不会生成重复 Key 记录。
- 便携包根目录保留不依赖 PowerShell 的 `Start-Codex-Router.cmd` 启动壳；EXE 发布者元数据统一为 `Hernan_JIANG`。Windows SmartScreen 的受信任发布者仍需要同名代码签名证书。
- 1.6.11 便携包和安装载荷不包含 `.ps1`、`.psm1` 或 `.psd1`。PowerShell 仅保留在 GitHub 源码仓库中用于 Windows 构建、发布、兼容和开发测试。

## 下载与首次启动

前往 [GitHub Releases](https://github.com/HernanJiang/Codex-Router/releases/tag/v1.6.11) 下载 Windows x64 版本：

`Codex-Router-Portable-1.6.11-windows-x64.zip`

同时提供可选的用户级安装器：`Codex-Router-Installer-1.6.11-windows-x64.exe`。它使用 Rust 原生安装逻辑，把同一份已验收运行时安装到 `%LOCALAPPDATA%\Programs\Codex-Router\1.6.11`，不需要管理员权限。

对于 `Upstream request failed` 这类尚未向客户端输出内容的瞬时流错误，Router 默认允许同一账号最多重试 5 次，每次间隔 1.5 秒。已经开始输出模型内容后不会重复回放请求。

这是一个解压即用的便携包，不要求预装 Python、Node.js、Rust、PostgreSQL、Redis 或独立 VC++ Runtime。请完整解压后启动，不要只把 `Codex-Router.exe` 单独移出目录。

第一次打开会直接进入全流程引导的第一页，按项目、登录、模型、网络和部署步骤带你完成配置，减少第一次使用的学习成本，不需要另找一份安装手册。

<p align="center">
  <img src="assets/release/first-run-guide.png" alt="Codex-Router 首次启动全流程引导" width="100%">
</p>

### 快速开始

1. 解压完整便携包并打开 `Codex-Router.exe`；若 Windows 对未签名 EXE 显示 SmartScreen 提示，也可使用同目录的 `Start-Codex-Router.cmd` 启动壳。
2. 按首次启动引导添加第一个 API 渠道或登录 OAuth 订阅。
3. 将需要使用的模型加入当前路由配置。
4. 完整阅读内置的使用与分发条款，滚动到底并由你本人确认。
5. 应用配置，Router 会初始化本地服务并更新 Codex Provider 配置。
6. 回到 Codex，在同一上下文窗口内从模型菜单切换模型。

当前版本支持 Windows 10/11 x64，不支持 ARM64；本 Windows 版本不包含 macOS 构建产物。

## 安全与条款

- API Key、代理密码和本地 Router Key 通过 Windows Credential Manager 保存。
- OAuth Token 始终由 Sub2API 管理，不复制到 Router 配置文件。
- 发布包排除用户配置、日志、数据库、OAuth 状态、备份和开发机路径。
- 本地运行服务默认绑定 `127.0.0.1`，不建议将管理接口暴露到远程网络。
- 完整条款请查看 [中文条款](TERMS.zh-CN.md) 和 [English Terms](TERMS.en.md)。
- Codex-Router 原创部分采用 GNU Affero 通用公共许可证 v3.0（AGPL-3.0）开源授权；Sub2API 和其他第三方组件继续遵循各自的上游许可证和声明。

官方仓库：<https://github.com/HernanJiang/Codex-Router>
