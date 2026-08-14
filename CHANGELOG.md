# Codex-Router 更新记录

本文件记录面向用户的重要变化。完整技术细节以对应版本的源码和 GitHub Release 为准。

## 待发布（仓库主线）

### 变更

- 仓库授权从“源码可见、禁止商用”改为 GNU Affero 通用公共许可证 v3.0（AGPL-3.0）：原创部分允许商业使用，向他人分发或以网络服务形式提供修改版时须按 AGPL 公开对应源码；Sub2API 等第三方组件继续遵循各自上游许可证。
- 应用内使用条款同步更新为新条款版本，升级后会要求重新阅读并确认。
- 新增 `.gitattributes`，将 Windows 启动/构建/发布脚本从 GitHub 语言统计中排除，仓库语言栏不再以 PowerShell 为主。

### 说明

- 许可证变更只涉及仓库主线，已发布的 1.6.11 安装包仍包含旧版专有许可条款；重新构建发布后会随新版本生效。

## 1.6.11 - 2026-08-14

### 修复

- OAuth 账号页的账号与实时模型目录改由 Rust 原生读取 Sub2API 管理接口，不再依赖最终发布包中已移除的 `Get-OAuthAccounts.ps1`，修复 Grok、Antigravity 与 ChatGPT 实际可调用但界面误显示“当前没有实时声明可用模型”或“模型目录暂时无法读取”的问题。
- 同时兼容账号稳定目录接口的 `data` 模型对象数组与实时同步接口的 `data.models` 模型 ID 数组；ChatGPT 不支持上游同步时会自动使用稳定目录，Grok 与 Antigravity 则在同步后读取最新账号目录。
- 单次目录刷新失败时保留该账号上一次成功模型列表并显示可重试状态。手动刷新和每 10 分钟统一自检都会更新可选目录，但不会自动把订阅模型加入当前配置。
- 新安装且尚未完成配置时，默认打开五阶段引导的第一页；已有有效配置的用户仍直接进入控制台。
- 特别适配 Google Antigravity OAuth，可使用具备资格的 Google AI Pro 账号提供的 Antigravity 订阅额度；实际可用模型和额度以账号实时声明及 Google 上游状态为准。

## 1.6.10 - 2026-08-14

### 修复

- 修复 Codex 保持 ChatGPT 登录时，外部程序覆写 `config.toml` 后第三方模型请求绕过本机 Router、继而提示“该模型不支持 ChatGPT 账号”的问题。启动、手动刷新和每 10 分钟自检会精确核验并原子恢复 Router provider、本机 bearer、模型目录与当前模型；ChatGPT 认证文件、用户配置和当前客户端进程保持不变，不需要重启 Codex。
- 修复 Antigravity 账号实时模型映射较稀疏时，`gemini-3.1-pro-high` 等兼容名称被原样发送并在 Codex 中返回 `Upstream request failed` 的问题。Gemini 3.1 Pro 系列现在稳定规范到账号可执行的 `gemini-pro-agent`，同时保留用户明确设置的自定义映射。
- 为 `gemini-3.7-flash` 的公开名称及 Low、Medium、High 执行档补充非零计费回退。动态价格仍优先；尚无独立价卡时临时沿用现有 Flash 价格，避免响应成功后记录 `pricing not found` 并产生错误日志。
- OAuth 账号模型目录刷新失败时保留上次成功结果，不再把账号卡片误显示为空；刷新成功后只展示该账号实时声明且可用的模型。

### 界面

- OAuth 账号卡片直接展示账号实时声明的可用模型，已加入模型仍可右键按账号删除；目录刷新不会自动把全部订阅模型写入当前配置。
- 模型编辑页新增“添加订阅账号模型”，点击后进入 OAuth 账号页并触发自检刷新。底部操作顺序统一为“取消、添加订阅账号模型、保存模型”，订阅模型仍需用户逐个明确添加。

## 1.6.9 - 2026-08-14

### 修复

- 修复 Codex Responses 同时使用网页搜索与函数工具时，Antigravity 上游返回 `Upstream request failed` 的问题。由于 Antigravity `cloudcode-pa` 不接受服务端搜索与函数声明并存，混合请求只在 Antigravity 边界保留可执行函数并移除服务端搜索；Google AI Studio 的混合工具行为保持不变。
- 兼容 Antigravity `gemini-3.7-flash`：对外统一模型名映射到账号实时声明的默认 Medium 档 `gemini-3.7-flash-medium`，并把 High、Medium、Low 三个分档归并为一个用户可选模型。
- Antigravity 按真实用量与重置签名归并额度池并隐藏内部模型条目；同一渠道端点、同一真实 Key 的 API 与 Coding Plan 模型也归并为一个额度池，包括 Kimi for Coding 与 Kimi K3。
- Grok OAuth 默认模型映射与账号调度加入上游实时声明的 `grok-4.6`，修复模型已在订阅账号中可用、但 Codex 请求仍被“账号不支持该模型”拦截的问题。
- OAuth 账号模型刷新改为调用实时上游同步接口。手动刷新与每 10 分钟自检会更新可选目录，但仍不会自动把新模型加入当前配置；只有用户明确点击添加并保存后才参与路由。

## 1.6.8 - 2026-08-13

### 修复

- 修复 Antigravity 账号可正常读取额度和模型、但 Gemini / Claude 真实生成统一返回 `RESOURCE_EXHAUSTED`，随后显示无可用账号的问题。生成请求现在优先使用经两个真实账号验证可用的 non-sandbox daily 端点，生产端点作为有限回退；OAuth、额度查询和模型目录端点保持不变，sandbox 仅在显式诊断配置下启用。
- 修复 Antigravity 同一模型绑定多个 OAuth 账号时，首个账号被额度隔离后健康重复账号无法接管，导致 Gemini 3.1 Pro High、Claude Fable 5 等模型返回无可用账号的问题。健康账号现在会继续提供共享模型并生成对应复合路由。
- OAuth 账号模型列表改为只使用该账号实时模型接口返回的数据，移除静态服务商建议和可绕过账号能力的手动补填入口，避免展示订阅实际不能调用的模型。
- 统一自检会在用户打开实时用量、刷新 OAuth 或后台每 10 分钟运行时，顺带刷新各账号实时模型目录。发现的新模型只进入可选列表，不会自动加入配置或恢复用户已删除的模型。
- 已加入的 OAuth 模型支持通过右键菜单按账号精确删除；保留其他账号的同名模型，并在最后一个 OAuth 同名模型删除后清理对应 fallback 选择。

### 界面

- 陶土主题的“当前配置 OAuth”按钮改用与“保存并应用”相同的咖啡色，修复误显示为绿色的问题。

## 1.6.7 - 2026-08-13

### 修复

- 修复 Grok、Gemini / Antigravity 的实时用量已经恢复正常，但 OAuth 账号卡片仍显示历史 `class=request_failure` 并误报“本地服务未启动”的问题。同一账号最新用量状态为 `healthy / active` 时会清除已被成功探测证伪的旧临时错误；真实 401、403、429、封禁、额度耗尽和非活动状态仍完整保留。
- 账号级临时网络或上游探测错误不再复用 OAuth 账号清单加载失败文案，而是明确提示该账号的上游探测暂时失败并由自检系统自动重试。

### 界面

- 默认控制台左右两列底边对齐，概览卡与右侧模型区域和活动日志使用同一可用高度，消除开始页面底部不齐和无效空白。
- 路由模型卡的模型名称行与额度、路由、视觉标签行之间进一步收紧，默认窗口可完整查看 3 个模型条目，同时保留活动日志区域。
- 顶部代理、更新、语言和主题控件恢复白色底色与深色文字；语言当前项保留深蓝选中态，中英文和紧凑窗口均不发生文字覆盖。

## 1.6.6 - 2026-08-13

### 修复

- 修复从 1.6.4 / 1.6.5 升级后仍保留约 50 个旧版自动导入 OAuth 模型的问题。迁移只删除具备完整旧目录指纹且未被用户选择的批量 OAuth 条目，保留 API 渠道、用户手动添加的 OAuth 模型、改名模型和小规模既有选择；刷新账号或“保存并应用”不会再次补回已删除模型。
- Antigravity 不再把共享同一服务端额度窗口的每个模型显示成独立额度池。使用比例、重置时间和用量字段完全相同的模型合并为一条“Antigravity 共享额度”；真正拥有不同窗口的模型仍分别显示。

### 界面

- 控制台的实时用量、常见渠道、配置分组和当前配置 OAuth 操作统一靠右排列，充分使用标题右侧空间。
- 顶部横栏恢复为较深雾蓝色；代理、更新、语言和主题控件使用一致的深蓝底与白字。中英文切换改为两个等宽分段，文字不再与切换轨道重合；默认窗口下的五阶段教学滑块扩展到可用宽度。
- 路由配置卡片进一步压缩垂直空白，默认窗口可同时查看约 2.5 个模型。活动日志拥有固定完整区域并始终保留在窗口底部，不再与底边或签名重叠。
- 新增只读 UI 审计截图入口，用隔离数据直接捕获中文、英文、紧凑控制台和用量页面，不读取或修改正式用户配置。

## 1.6.5 - 2026-08-13

### 修复

- 修复手动或后台用量自检读取 OAuth 账号模型目录，并把约 50 个订阅模型自动加入当前配置的问题。用量自检现在只查询额度、健康和 fallback，不读取或修改模型目录。
- OAuth 页面刷新只更新账号可用模型列表，只有用户逐个点击“＋ 模型”后才加入当前配置。用户删除模型并点击“保存并应用”后，该模型不会再被后台刷新自动补回；`grok-4.6` 继续保留为手动建议项。

### 界面

- 默认窗口客户区调整为约 `1064 × 820`，与紧凑控制台目标尺寸一致，同时保留窗口缩放和高 DPI 自适应。
- 顶部“控制台 5/5”改为五阶段新手教学进度滑块，依次显示项目、登录、首个模型、网络代理和完成，可返回已完成阶段。
- 控制台标题右侧保留实时用量统计、常见渠道快速配置、切换配置分组和当前配置 OAuth；“保存并应用”与“＋ 添加新模型”移入路由配置卡片标题栏，让查看导航与配置写操作分区更清楚。
- OAuth 渠道、独立/兜底状态和托管说明合并为一个路由标签。模型列表会使用活动日志上方的全部剩余高度，默认视图可同时查看约 3 个模型；长模型名会省略显示，不会覆盖默认、编辑、删除和排序操作。
- 默认窗口下的英文顶部栏自动使用紧凑标签，修复 `NETWORK PROXY` 等长文本与五阶段教学滑块重叠的问题。

## 1.6.4 - 2026-08-13

### 修复

- 修复 OAuth 恢复观察中的过期时间被压缩为 1 秒，导致后台恢复探测和“额度状态已同步”日志约每 5 秒反复出现的问题。自动恢复最短间隔固定为 10 分钟，未知状态最长 5 小时的安全恢复上限保持不变。
- 每次手动或后台用量自检都会读取已选 OAuth 订阅账号实际可用的模型目录；发现上游新增模型时，按账号自动且幂等地加入当前 Router 配置和活动隔离配置。新增 `grok-4.6` 手动建议项；后台模型发现只热同步 Router，不改写 Codex 配置，也不重启客户端。
- OAuth 恢复观察现在只保留当前配置中实际启用了同模型 API fallback 的账号。已移除、未选中或不再具备 fallback 的陈旧账号不会继续触发后台探测。
- 区分观察文件更新时间与真实账号路由变化；只有账号实际隔离或恢复时才同步本机实时路由，不再因普通探测结果重复刷新路由或再次查询用量。
- 后台 OAuth 恢复会在用量查询、OAuth 账号列表读取、路由同步、配置应用、OAuth 登录或服务健康恢复占用管理接口时静默顺延，避免 OAuth 页面因并发管理请求暂时显示“无法读取 OAuth 账号”。手动刷新仍立即执行统一自检。
- OAuth 恢复和 fallback 切换继续只更新本机 Router 后端，不改写当前 Codex 配置，不关闭或重启 Codex / ChatGPT，不中断正在进行的任务。

## 1.6.3 - 2026-08-13

### 修复

- 恢复 1.5.2 已验证的 Codex 登录契约：应用 Router 后继续使用原有 ChatGPT 登录，provider 保持 `Codex-Router`，并写入 `requires_openai_auth = true`；不再伪装成 `OpenAI` 第三方 API 登录，也不会因此丢失 Router 自定义模型目录。
- OAuth 额度耗尽时改为只在本机 Router 后端热切换到同名 API fallback，并弹窗提示额度变化；不关闭或重启 Codex / ChatGPT，不中断正在进行的任务。额度恢复后同样热切回 OAuth，只有完整手动“应用配置”才可能需要重启客户端。
- 默认每 10 分钟运行一次统一自检，实时查询会确认 OAuth 额度、fallback 可用性和配置绑定；未知额度状态最长隔离 5 小时，确认恢复后自动重新接入。
- OAuth 与 API fallback 默认共用同一个模型展示名称，不追加 `(OAuth)` 后缀；自动接续关闭时仍通过稳定的独立模型 ID 手动选择额度来源。
- 新增登录与自定义模型目录联合回归测试，覆盖任意模型 ID、自定义显示名、稳定 `model_catalog_json` 路径和不写入全局 `forced_login_method`。
- 已保存配置的每一项新增“删除”按钮和二次确认，可删除配置快照及其隔离 API 凭据；当前正在使用的配置必须先切换或初始化后才能删除，OAuth 账号和当前 Codex 配置不会被改动。
- 修复 Grok 添加第二个账号时授权链接被 Windows Explorer 当成本地路径处理、继而打开“文档”目录的问题。OpenAI、Claude、Gemini、Antigravity 和 Grok 的 OAuth 入口现在统一通过系统默认 HTTPS 处理器打开，并保留长 URL 及多账号登录参数。
- 修复 Grok 与 Antigravity 实时额度查询已成功，但 OAuth 账号卡片仍显示历史 `class=request_failure` 的问题。成功查询会清除由 OAuth 刷新、网络请求或临时认证探测产生的残留错误和临时不可调度状态；真实 403、封禁和限流状态不会被误恢复。
- Antigravity 用量查询统一通过受锁 Token Provider 读取凭据；上游 401 会绕过旧缓存、刷新 token 并仅重试一次，成功后清除强制刷新标记，避免持续使用已失效 token。
- Antigravity 额度展示以本次实时返回的模型目录为准，不再因本机旧模型名与上游目录漂移而误判为空；后端结构化认证、权限、限流和网络错误也会显示为可执行提示，不再泄漏原始 `class=` 文本。
- 修复后台自检发现 Codex 配置漂移时仍调用“轻量绑定修复”的问题。自检现在只读检查并提醒，不再自动写入 `config.toml` 或模型目录。
- 修复后台 OAuth 账号自动发现触发完整 Apply、关闭并重启 Codex 的问题。后台发现账号只同步本机 Router 实时路由；只有用户明确点击“保存并应用”或切换配置才会进入需要重启客户端的完整流程。

## 1.6.2 - 2026-08-11

### 修复

- 修复 Router 的“OAuth + API 上游”选择被错误映射成 Codex 全局 `forced_login_method = "chatgpt"` 的问题。1.6.2 继续保留 `OpenAI` 展示名、本机 Router Key 和 `requires_openai_auth = false`，但不再限制或切换 Codex 自己的登录方式，并在应用 Router 配置时清理旧版本写入的限制。
- 修复“恢复 Codex 默认配置”会把非 ChatGPT 形式或未来版本格式的 `auth.json` 当作无效文件删除的问题。Router 现在只判断是否存在可恢复的 ChatGPT 文件登录，不再删除任何由 Codex 自己管理的认证文件。
- 本地发布验收统一使用独立的 `CODEX_HOME` 和 `CODEX_ROUTER_USER_DATA_ROOT`，并在结束前确认真实默认 `config.toml` / `auth.json` 的存在状态、长度和时间戳均未变化，防止测试路径再次触碰用户配置。

## 1.6.1 - 2026-08-11

### 修复

- 修复磁盘中存在旧配置分组、但 `activeProfileId` 为空时隔离保护被绕过的问题。启用 `generateIsolation` 后，所有手动和自动应用入口都必须绑定到一个真实存在的活动或待切换分组，否则会停止写入默认 Codex 配置并引导用户重新选择。
- 修复有效 ChatGPT 登录在 Codex Desktop 中显示为“Codex-Router 登录”的问题。`chatgpt_oauth` 模式继续使用稳定的 `codex_router` provider ID 和本机 Router Key，但展示名改为 `OpenAI`；API-only 模式仍保留 `Codex-Router` 展示名。
- 兼容配置应用和 Codex 集成安装脚本会根据软件内 `authMode` 传递 ChatGPT 登录标记，不再把 GUI 已写入的 OAuth 展示身份覆盖回 API-only 身份。
- Router provider 的所有权和账号状态判断优先使用稳定 ID `codex_router`，旧 `Codex-Router` 名称仅保留为历史 `custom` / `sub2api` 配置的迁移兼容条件。

## 1.6.0 - 2026-08-11

### 修复

- 修复 Codex Desktop 应用配置后提示“无法自动重启”，导致新 `model_catalog_json` 未在冷启动时加载的问题。程序会先发送窗口关闭请求，超时后才按子进程到父进程顺序处理仍存活的已验证进程；已自然退出的 Electron 子进程不再被当作失败。
- 修复 Microsoft Store/MSIX 版 Codex 通过 `WindowsApps` 内部 EXE 直接重启失败的问题；1.6.0 改用官方 AUMID 和 `shell:AppsFolder` 激活，并确认新进程实际启动后才报告成功。
- 修复 `chatgpt_oauth` 配置下文件型 ChatGPT 登录意外缺失后只剩第三方 provider 状态的问题。应用配置会从当前 Windows 用户 DPAPI 可解密、内容校验有效的最近快照恢复缺失登录，已有认证文件保持不变。
- `chatgpt_oauth` 模式显式写入 `forced_login_method = "chatgpt"`；Router provider 仍使用本机 Key 且保持 `requires_openai_auth = false`，避免第三方模型重新被 ChatGPT 模型白名单拦截。
- 修复安装器外壳继承 IExpress 的 `Microsoft Corporation / Internet Explorer / 11.00` 文件元数据；构建时会复制已验收主程序的版本资源，并强制校验为 `Hernan_JIANG / Codex-Router / 1.6.0`。

### 验证

- 新增 MSIX AUMID 启动计划、父子关闭顺序、已退出进程容错、ChatGPT 登录快照恢复和登录模式区分回归测试。
- 使用隔离 `CODEX_HOME` 由当前 Codex CLI 加载真实生成的模型目录，确认 13 个模型可见且配置指向最新目录；验收过程未关闭或重启当前 Codex、ChatGPT 或 Codex-Router。
- Rust 共发现 215 项测试：默认执行 213 项全部通过、0 项失败，2 项需要显式真实环境的测试保持忽略；格式检查、静态检查和 Clippy 严格检查通过。
- 最终本地发布验收 17/17 通过；便携目录与 ZIP 的 1103 个文件逐项哈希一致，隔离安装和原生服务生命周期通过，运行包不包含 PowerShell 文件。

## 1.5.8 - 2026-08-11

Windows 原生运行时迁移的统一发布版本。迁移期间不生成阶段性 LAB、RELEASE、便携包或安装包，只在全部目标通过后构建一次最终验收产物。

### 变更

- 修复订阅额度耗尽后只显示“自动路由”提示、但 OAuth 账号仍保持可调度而继续接收请求的问题；实时额度确认耗尽且存在同模型 API 备用渠道时会立即停用该 OAuth 调度，额度恢复后再自动启用，不需要重启 Router。
- 修复 Codex 配置被覆盖或仍带 `requires_openai_auth = true` 时不会触发自愈的问题；Router provider 现在始终绕过 ChatGPT 模型白名单，运行中的 Codex 不会阻止配置恢复，仅 Windows sandbox 安装助手运行时短暂延期。
- 用量刷新、OAuth 恢复、备用路由维护和 Codex 配置覆盖检测默认统一为每 15 分钟执行一次；失败的恢复探测也会在 15 分钟后重新调度。

- PostgreSQL、Redis 和 Sub2API 的初始化、启动、认证就绪、状态读取、健康恢复和停止流程迁移到 Rust；Redis 启动必须通过带密码的 `PONG`，Sub2API 必须同时通过 `/health`、数据库管理员和依赖就绪检查。
- 生命周期锁、进程路径所有权、回环监听器、活动连接保护、代理网络指纹和已验证进程树终止统一由 Rust 执行，继续保留 `ROUTER_LIFECYCLE_BUSY`、`ROUTER_LIFECYCLE_DEFERRED` 等稳定错误契约。
- GUI 完整退出、手动关闭 Router、连续健康失败恢复和 OAuth 账号读取后的本机修复不再调用 `Start-Router.ps1`、`Stop-Router.ps1` 或 `Ensure-RouterHealthy.ps1`。
- 配置应用、Sub2API 管理接口同步、OAuth 登录与恢复、凭据复制、模型目录和 Codex TOML 写入已切换为 Rust 原生实现；重复应用不会因管理接口隐藏完整 Key 而创建重复记录。
- 关闭自动接续时，同名 OAuth 与 API 模型使用独立且稳定的公开模型 ID；将 API 渠道设为默认时，Codex 不会误选同名 OAuth 模型。
- Kimi `k3-256k` 渠道在应用时加入正确 Router 分组，保留对应模型映射并使用 Coding Plan 支持的 chat-completions 传输策略。
- 更新器使用官方 GitHub 地址、SHA-256 和发布清单进行校验，提供实时下载进度，并由独立 Rust 助手完成原子覆盖、失败回滚和自动重启。
- 安装器改为调用受限的 Rust 安装 CLI，校验便携 ZIP 后安装到当前用户目录并通过 Windows Shell Link API 创建快捷方式，不再携带或执行安装 PowerShell 脚本。
- 原生安装覆盖在既有安装校验失败时会保留原目录并清除同卷 staging，不再留下 `.codex-router-update-*` 临时目录。
- 最终便携包和安装载荷不包含 `.ps1`、`.psm1` 或 `.psd1`；PowerShell 只保留在源码仓库用于 Windows 构建、发布和开发测试。
- 项目目录识别改为检查 Sub2API、PostgreSQL 和 Redis 本机运行组件，不再强制要求 `Start-Router.ps1`。

### 验证

- Rust 全量测试通过：205 项通过、0 项失败、2 项需要显式真实环境的测试保持忽略；格式检查、静态检查和 Clippy 严格检查通过。
- 原生安装 CLI、安装覆盖时的用户文件保留和 Windows `.lnk` 创建已在隔离临时目录通过验收。
- 新增隔离生命周期验收：使用独立用户数据目录和三个随机回环端口完成启动、深度状态、重复启动幂等和强制停止；测试前后当前会话 Router 的 PID、路径和响应状态保持不变，且无测试进程或临时目录残留。

## 1.5.7 - 2026-08-10

阶段 4 源码里程碑。本中间版本未生成便携包或安装包。

### 变更

- GUI 直接通过 Rust 写入 Windows 凭据管理器，并在提交成功后清除 API、火山引擎和代理明文。
- 稳定用户数据、配置、模型目录、备份和运行态路径继续由 Rust `user_data` 模块统一管理。
- 手动代理、环境变量、Windows Internet Settings、当前用户 WinHTTP 与系统 WinHTTP 代理改由 Rust 归一化；每个上游目标的绕过与直连回退策略只计算一次，且不持久化代理密码。
- 开机自启改用 Rust 维护当前用户 `Run` 注册表项，同时清理旧启动快捷方式和分钟级健康任务，仍以轻量 `--background` 模式启动。
- GUI 不再为上述操作调用 `CredentialStore.psm1`、`ProxyDiscovery.psm1`、`Register-Autostart.ps1` 或 `Unregister-Autostart.ps1`；兼容脚本暂留给下一阶段迁移的生命周期入口。

## 1.5.6 - 2026-08-10

### 修复

- 每次手动或后台用量查询都会顺带执行账号恢复探查。被误置为错误或不可调度的已选 API/OAuth 账号，仅在实时额度成功、额度未耗尽且凭据未被拒绝时恢复；缓存额度、401/403 凭据拒绝和真实额度耗尽不会误触发。
- OAuth、Grok、Gemini 等账号级额度读取继续使用有边界重试；某个提供商失败不会阻塞其余用量卡片，也不会向界面泄漏原始 `class=request_failure`。
- 默认每小时在后台刷新用量并执行恢复维护，轻量托盘模式同样保持该调度。

### 改进

- OAuth 账号优先级更新已从 `Set-OAuthAccountPriority.ps1` 迁移为 Rust 直接调用本机管理接口。
- OAuth/API 基础优先级和同模型 API 渠道有效优先级计算已迁移到 Rust；PowerShell 模块仅保留兼容转发入口。
- 发布包不再包含 `Set-OAuthAccountPriority.ps1`，继续减少最终用户运行路径中的 PowerShell 文件。
- 便携包新增不依赖 PowerShell 的 `Start-Codex-Router.cmd` 启动壳；EXE 版本资源和安装器信息统一标注发布者 `Hernan_JIANG`。SmartScreen 的受信任发布者仍需同名代码签名证书。

### 发布物

- `Codex-Router-Portable-1.5.6-windows-x64.zip`
- `Codex-Router-Installer-1.5.6-windows-x64.exe`

## 1.5.5 - 2026-08-10

### 修复

- 修复 OAuth 账号列表读取成功后，单账号 `/stats`、`/usage` 或 OpenAI `/quota` 瞬时失败仍直接显示 `class=request_failure` 的问题；账号级查询现在执行有边界的本地重试。
- 修复 Kimi `k3-256k` 请求超过 256K 上下文时，上游 401 被误判为 Key 失效并永久禁用账号的问题；请求仍会返回上下文限制，但有效账号保持可调度。
- 配置应用会恢复已确认有效但曾被误置为错误或不可调度的受管 API 账号，避免模型目录最终返回“无可用账号”。
- 每次用量查询都会同时请求 OAuth 恢复探查；已配置并启用 Router 时，即使窗口位于轻量托盘模式，用量与恢复维护也会默认每小时在后台执行一次。
- Kimi Coding Plan 若因 `k3-256k` 上下文限制被旧版本误禁用，仅在本次官方额度实时查询成功后自动恢复；无效 Key、401/403 凭据拒绝、缓存额度和其他渠道不会被误恢复。
- 修复 Rust 配置迁移时旧 `127.0.0.1:15721` 临时代理残留的问题，同时保留用户自己的第三方 provider。

### 改进

- 将 Codex `config.toml` 的结构化生成、校验、权限设置保留、备份轮换与原子写入迁移到 Rust；PowerShell 仅保留兼容调用包装。
- 配置写入 CLI 在 GUI 单实例检查前运行，Codex-Router 已开启时也能应用配置，不会触发第二个 GUI 实例。
- 兼容包装通过标准输入传递本机 Router Key，不再把 Key 放入进程命令行。

### 验证

- Kimi 上下文限制 401 与普通认证 401 的 Go 回归测试通过；普通认证失败仍保持原有禁用行为。
- Rust 回归测试覆盖一小时托盘调度、查询绑定 OAuth 探查，以及 Kimi 恢复与拒绝误恢复边界。
- Rust 配置单元测试和 PowerShell 端到端配置验收通过，覆盖幂等、权限保留、Fast 模式、旧 provider 清理与自定义 provider 保留。

### 发布物

- `Codex-Router-Portable-1.5.5-windows-x64.zip`
- `Codex-Router-Installer-1.5.5-windows-x64.exe`

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
