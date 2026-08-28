# CodexRouter 更新记录

本文件记录面向用户的重要变化。完整技术细节以对应版本的源码和 GitHub Release 为准。

## 3.1.4 - 2026-08-29

### 修复

- 修复 Gemini、Muse 等 Chat Completions 兼容流把 `reasoning_content`、`reasoning`、`thinking` 和 `<think>` 内容误当普通正文输出的问题；现在会实时转换为 Codex Responses reasoning summary delta。
- 修复分块 `tool_calls` 被缓冲到最终事件的问题；工具调用首个分块到达时立即转换为 `response.output_item.added` 和 `response.function_call_arguments.delta`，结束时补齐 `done` 与唯一的 `response.completed`。
- 修复兼容流在网关边界转换后被误判为没有终止事件的问题，避免有效的 Chat Completions `[DONE]` 流被当成断流重试。

说明：Router 只能转发模型实际产生的 reasoning 和工具调用；如果 Gemini/Muse 本身没有生成可执行动作，Router 不会凭空制造 ReAct 步骤。

## 3.1.3 - 2026-08-28

### 原因

每次打开 Codex-Router，已经用尽的订阅和日志里的账号切换会被当成「刚刚发生」，所有额度弹窗再弹一遍。

### 修复

- 启动后的第一份用量快照只作基线，不弹窗。
- 启动回放的 pool failover 日志不再弹出切换/额度对话框。
- 只有运行中额度从可用变成用尽时，才弹一次。

## 3.1.2 - 2026-08-28

### 原因

3.1.1 把 Spark 的 `<think>` 转换套到**所有** Responses SSE 上。ChatGPT 5.6 Sol 等原生模型的 `output_item.added` 被整段重写，流里的 `<` 还会被当成标签前缀攒住，看起来像「也不输出了」。

### 修复

- ChatGPT / Grok 的 SSE 原样转发，不再做 think 标签转换。
- 只有完整的 `<think` 才进入转换；比较运算符 `<`、加密 reasoning 字段不再被剥掉或卡住。

## 3.1.1 - 2026-08-28

### 原因

Spark（`meta/muse-spark`）等模型把思考写在 `<think>` 标签里。网关按块剥标签，未闭合的思考增量被丢掉，Codex 同一条思考条空转，最后才整段砸出结果。

### 修复

- 流式 `<think>` / `<thinking>` 转成 `response.reasoning_summary_text.delta`，跨 SSE 块也能接上。
- 思考结束后再出可见正文或工具调用。不能强迫模型在思考中途交错执行命令。

## 3.1.0 - 2026-08-28

### 原因

关闭 Codex-Router 窗口后，旧便携版 Host 仍占端口。Stop 把「同名但路径不同」的 `codex-router-host.exe` 当成别人的安装，3.0.18 便留在 28083。新版本再 hop 端口，两套 Host 抢同一份 UserData。Muse 的 MCP 工具名超 64 字符 400 也一并收进本版。

### 修复

- 关闭/停止时清掉本机所有 `codex-router-host.exe` / `cli-proxy-api.exe` 在 18080 及 28080 段端口上的监听，不限安装路径。
- 启动时接管同名旧 Host，不再 hop 出第二套。
- Muse / Meta：按 CLIProxy 的 namespace 拼接把工具名压到 64 字符，回包还原。

## 3.0.22 - 2026-08-28

### 原因

3.0.21 只缩短了顶层 `tools[].name`。Codex 实际把 MCP 工具放在 `type=namespace` 里，CLIProxy 转 Chat Completions 时再拼成 `namespace__child`。`mcp__openai_api_key_local_confirmation` + `confirm_openai_api_key_local_destination` 拼出来仍是 80 字符，Meta 继续 400。当时 3.0.18 Host 也还在跑。

### 修复

- 递归处理 namespace / additional_tools，按 CLIProxy 的拼接规则把即将发出的名字压到 64 字符以内。
- 带 `namespace` 的历史 function_call 同步缩短；回包再还原成 Codex 的子工具名。

## 3.0.21 - 2026-08-28

### 原因

`meta/muse-spark-1.2-contributor`（CommandCode / Meta 网关）报 400：`` `name` must be at most 64 characters, got 80 ``。Codex 把 MCP 工具名 `mcp__openai_api_key_local_confirmation__confirm_openai_api_key_local_destination`（80 字符）原样送给 Chat Completions，上游函数名上限是 64。

### 修复

- 第三方模型请求里，超过 64 字符的 tool / function_call 名称改成「前缀 + SHA-256 短哈希」，并在描述里留下原名。
- 上游回包的 function_call 名称再还原成 Codex 的 MCP 全名，桌面端才能对上工具。

## 3.0.20 - 2026-08-28

### 原因

Codex 线程 `01a04464-5d43-7ba0-84bf-56ef552e7bed` 用 `z-ai/glm-5.3-flash` 时，每一轮都显示「正在运行」约 1–2 分钟才发出第一条工具调用。现场：CLIProxy `POST /v1/responses` 耗时 `1m49s` / `44s` / `53s` / `1m4s`，但可见输出只有约 131–297 token、reasoning 0–115 token；同机 DeepSeek 只要 3–10 秒。没有 Router 重试。

GLM-5.3-Flash **强制思考且不能关闭**。官方默认 `reasoning_effort=max`，Codex Desktop 全局 max 也会原样送上去。max 在 Agent 循环里会先想 40–90 秒才出 tool call；Router 目录还没暴露官方的 `low`，输出上限也按剩余压缩预算抬到 16 万+（超过官方 128K）。

### 修复

- GLM-5.3-Flash 目录档位为 `low` / `high` / `max`（`max` 仍在，只是更慢）；非法档位按官方 Coding Plan 映射，避免 400。
- 第三方模型（Gemini / GLM / DeepSeek 等）目录 `default_reasoning_summary` 改为 `auto`，网关把请求里的 `summary=none` 改成 `auto`。否则 Codex 会丢掉思考增量，只显示最终结果。ChatGPT / Grok 请求形状保持原样。
- GLM-5.3 输出硬上限 128K，不再按剩余压缩预算抬到十几万。

## 3.0.19 - 2026-08-27

### 原因

从 3.0.17 切到 3.0.18 后点「保存并应用」报 `配置文件已写入，但本机路由应用失败`。旧 Host 仍占 `28083`，新版本因安装路径不同被当成「另一份 Router」，改去 `28080` 再起一套。两套 Host 抢同一份 `%LOCALAPPDATA%\Codex-Router\UserData`（sqlite、锁、Windows 凭据），CLI `/healthz` 30 秒超时（CR-CLI-0003），热推 CR-CFG-0005，停服务又 `lifecycle_busy`。CraftStation 与 Router 若共用未命名空间的 `CodexRouter/*` 凭据，也会互相覆盖。

### 修复

- 升级接管：UserData 里 `router-host.pid` 仍指向旧便携版 Host 时，视为本实例前驱，停掉后在原端口拉起新 Host，不再换端口双开。
- Windows 凭据改为 `CodexRouter/{UserData指纹}/{name}`；读取时回退旧的 `CodexRouter/{name}`。CraftStation 不再碰到 Router 的 LocalApiKey / AdminPassword。

## 3.0.18 - 2026-08-27

### 原因

Codex（ChatGPT）5 小时额度用完后，对话报 `503 no schedulable credential in pool`（CR-RTE-0002），不会切到已配置的第三方 API。现场 `01a02cf3-7ca0-7f92-9843-848ed2638cb9`：对话前一天已从 ChatGPT 429 切到中继，11:30 CLI 管理口 `PUT /v0/management/config.yaml` 返回 404 后 Host 把**所有**号池（含 API 兜底）标成 `available=false`，随后全部 503。

3.0.15 把「5 小时窗口满」当成短时限流、不踢出号池；ChatGPT 又是 Desktop 持有凭证，隔离和 `recover-state` 都被跳过。只有一个 ChatGPT 账号时无法轮换，请求继续打官方，热推失败后再加上号池全停，第三方 API 永远进不去。

### 修复

- CLI 配置热推失败（404 / 未确认模型）时，仍按凭证编译结果发布路由表，不再把全部号池停泊。YAML 已写盘，前缀缺失由单次请求失败后换池处理。
- ChatGPT 5 小时窗口满（周额度仍有余量）视为暂时耗尽：踢出号池、同名 API 兜底立刻接手；5 小时窗口恢复后重新加入号池。
- ChatGPT 的 `recover-state` 只在本地把账号设回 `schedulable=1`，绝不经 CLIProxyAPI 用 `$TOKEN$` 探测。

## 3.0.17 - 2026-08-27

### 原因

在官方（chatgpt.com / grok / google 等）点击「重置」后回到 Router，用量监控页短暂显示红色整页错误「用量查询暂时失败，已保留上次成功数据，请稍后重试」，各平台都会出现。重置瞬间各平台额度接口短暂抖动、或本地用量查询瞬时失败，`load_usage_snapshot` 返回错误后，红色失败 banner 覆盖了屏幕上仍在展示的上次成功数据，造成误报警。

### 修复

- 用量刷新失败但屏幕已有上次成功数据时，不再用红色「查询失败」整页 banner 覆盖数据，改为柔和的**黄色提示条**「用量刷新失败（已保留上次成功数据）」，数据继续展示，后台自动重试。
- 仅当**没有任何上次数据**（首次查询即失败）时才显示红色失败 banner，并给出具体原因。
- 新增 `Palette.warning` 配色与可测试的 `usage_error_for_ui` 辅助函数（有快照 → `RETRY-KEEP:` 前缀，UI 按黄色渲染）。

## 3.0.16 - 2026-08-27

### 原因

OpenAI（ChatGPT）账号额度总是显示「平台未提供可读取的 5 小时 / 周 / 月额度窗口」。排查确认 `chatgpt.com/backend-api/wham/usage` 本身可用（真实账号返回 200，含 5 小时 primary 窗口与周 secondary 窗口），但 Router 对 Desktop 拥有的 OpenAI 账号在额度查询里直接返回缓存、从不调用 wham（历史 `desktop_openai_auth_owner` 设计），缓存又长期不更新，导致 UI 永远没有窗口。

### 修复

- OpenAI OAuth 账号的额度查询改为**实时只读 wham 探测**：用 Desktop 当前 access_token 直连 `wham/usage`（绝不经过 CLIProxyAPI 的 `$TOKEN$` 刷新路径，避免 token 家族被吊销），成功则写缓存并返回 `five_hour` / `seven_day` / `monthly` 窗口；失败则回退缓存并标注 `error_code`（如 `auth_unavailable`），不触发 re-auth 冷却。
- 调度层面仍保持 observational：账号健康与换池仍由 Desktop / 既有隔离恢复链路负责，本次只恢复"窗口可读"。
- 参考 token-monitor 的「查询层 + 适应层」思路：每个 provider 独立查询、解析、缓存降级（grok / antigravity / kimi 均已如此），OpenAI 是唯一漏掉的，本次补齐。

## 3.0.15 - 2026-08-27

### 原因

ChatGPT 官方从纯周额度改成「5 小时滚动窗口 + 周额度」。Router 的额度判定是「任一窗口 used ≥ 99.999% 即耗尽」，于是 5 小时窗口被用完时（周额度明明还有余量）就把账号误判成额度耗尽，踢出号池并长期优先走 API；5 小时窗口重置后也没有及时回归号池。

### 修复

- 额度判定区分短窗口与长窗口：存在 weekly / seven-day 窗口时，**只有周窗口耗尽才算真耗尽**；5 小时窗口满只是短时限流，账号留在号池，请求由账号池自动轮换（不优先 API）。没有长窗口的平台保持原判定。
- 新增「账号已回归号池」弹窗：订阅账号自检发现额度恢复、重新加入号池时，弹出提示「账号 X 额度已恢复，已重新加入号池」（同一恢复周期只弹一次）。
- 账号离开号池（周额度真耗尽 → API 兜底）的提示弹窗继续生效，且只在真正耗尽时触发。

## 3.0.14 - 2026-08-27

### 原因

3.0.13 把每个 OAuth 账号拆成独立 CLIProxy 前缀后，旧对话（continuation 绑定在旧账号池）在账号失效时收到 `503 auth_unavailable: no auth available (providers=xai, model=cr_r10a56_xai/grok-4.6)`。Host 的池切换判定只认识额度 / 限流 / 池耗尽，不认识 `auth_unavailable`，于是不换号直接把 503 透传给 Codex Desktop——新对话能选到可用账号，旧对话却一直卡在失效账号上报错。

另外，跨线程多 Agent 协同（fork 子代理带上父对话的工具历史）在切到 Antigravity / Gemini 池时，会把没有对应 functionCall 的 functionResponse 一起带上，Gemini 校验失败返回 400 `invalid Gemini function call history`。

### 修复

- Host 池切换判定新增 `auth_unavailable`（no auth available）：只要还有可用的后备账号池就自动换号，旧对话不再 503；所有账号都不可用时才保留错误，并记录明确的 `request.pool_unavailable` 事件。
- 换号（自动切换账号池）现在会弹窗提醒，弹窗同时覆盖"所有账号不可用"的提示（Windows 通知对话框，同一池只提醒一次）。
- Gemini / Antigravity 请求在发送前裁剪无对应 functionCall 的孤立 functionResponse（跨线程历史），保留其文本内容，避免 400 结束对话。

## 3.0.13 - 2026-08-26

### 原因

Gemini / Antigravity 额度用尽返回 `429 Resource has been exhausted (e.g. check quota.)`。三个 OAuth 账号被编进同一个 CLIProxy 前缀 `cr_r13_antigravity`，Google 一冷却就 `All credentials ... are cooling down`。Host 没有下一池可切。网关还把这条 429 当普通限流 5s/25s/125s 重试，Desktop 看到 `exceeded retry limit`。

### 修复

- 每个 OAuth 账号独立前缀（`cr_r13a52_antigravity` …），按账号优先级 P1/P2/P3 分池。额度用尽立刻换下一个账号。
- 网关不再对额度/quota/cooldown 429 做同号重试。

## 3.0.12 - 2026-08-26

### 原因

Codex Desktop 给 Antigravity Claude Opus 4.6 Thinking 显示约 122k 上下文。那是 Router 把 4.6 当成未知模型，套了保守默认 128k，再乘 95% 压缩点（128000×0.95=121600）。官方和 Vertex / Antigravity 的输入窗口是 **1M**；128k 是输出上限，不是上下文。Claude 5 已经按 1M 适配，4.6 / 4.7 / 4.8 漏了。

### 修复

- Claude Opus/Sonnet 4.6–4.8（含 `-thinking`）和 Claude 5 / Fable 5：默认上下文 1,000,000，95% 压缩点 950,000。
- 其余 Claude（如 4.5）：200,000。

## 3.0.11 - 2026-08-26

### 原因

Antigravity Claude Opus 4.6 Thinking 在最高思考档（`reasoning.effort = max`）立刻 400：``max_tokens` must be greater than `thinking.budget_tokens``。CLIProxy 7.2.135 把 `max` 映射成 128000 thinking budget，网关却按剩余压缩预算写入 `max_output_tokens`（现场 113874）。high / xhigh 的 budget 更小所以能过，只有 max 会撞上。Anthropic 要求严格大于，相等也不行。

### 修复

- Claude 族在注入输出上限后：若 `max_output_tokens` 不大于该档 thinking budget，抬到 budget + 4096（max → 132096），给正文留出额度。

## 3.0.10 - 2026-08-26

### 原因

Grok 长线程和压缩后的短重试都返回 HTTP 400 `{"code":"invalid-argument"}`（现场 `01a038a7`、`01a02c3a`）。请求体里的 message / function_call 已经合法，CLIProxy 把 5 个 Codex `namespace` 拍成 43 个 function 之后，把 `mcp__codex_app__automation_update` 原样送给 `cli-chat-proxy.grok.com`。这个工具的 parameters 根上是 `oneOf` + `$defs`/`$ref`，xAI 拒收（和 CLIProxy `#4343` 同类，但 7.2.135 只修了 `codex_app__automation_update`，没修 MCP 前缀名）。同时网关把 `max_output_tokens` 抬到剩余压缩预算（现场 297543），超过 Grok 兼容上限。

### 修复

- Grok 请求在进 CLIProxy 之前：把 `automation_update`（含 `mcp__codex_app__*` / 拍平名）的 schema 换成空 object；去掉其它工具根上的 `oneOf`/`$defs`。`web_search` 和普通 function 保留。
- 去掉 Grok 的 `text.verbosity` 和 `reasoning.summary`，只保留 `reasoning.effort`。
- Grok `max_output_tokens` 硬上限 128000，长对话不再写成 20 万+。

## 3.0.9 - 2026-08-26

### 原因

Grok 上游返回 HTTP 402 `Payment Required`（SuperGrok 额度用尽）。Router 的池故障转移只认 429/503，Desktop 直接看到 402。另外 Grok 登录把 code 交给 CLIProxy 的 `xai-auth-url`，回调 URI 对不上 xAI 注册的 `http://127.0.0.1:56121/callback`，授权页一直 pending、拿不到 token。

### 修复

- 402 视为额度用尽：立刻换下一个 Grok/中转池。没有下一池时改成 429 `usage_limit`，不再把 Payment Required 打给 Desktop。
- Grok 登录改由 Host 自己做 PKCE：授权 URL 固定回调 `http://127.0.0.1:56121/callback`，在 `auth.x.ai` 换 token 并写入 `xai-{email}.json`。不再依赖 CLIProxy 消费一次性 code。

## 3.0.8 - 2026-08-26

### 原因

Grok 4.6 在 Debugger 已经写完整份 `Verdict` 之后，还会再冒出一句「任务已完成」。3.0.3 的自动续跑把报告里的「下一步：Coder 做 T02」当成提前停机，提示词又教模型「若已做完，回复一句任务已完成」。Grok 按口令复读，Codex 把这一轮摘要盖成五个字。

同时，Antigravity `claude-opus-4-6-thinking` 没有推理档位可选；Grok 4.6 官方有 `xhigh`，目录里却只给了 low/medium/high。

### 修复

- 超过约 200 字的长篇结论、或带 `Verdict` / 「任务已完成」的收工正文，不再自动续跑。短的「我先对照…」「接下来…」仍会续。
- 续跑提示改为「不要回复任务已完成」，不再把这五个字当成停止口令。
- 推理档位：Grok 4.6 增加 `xhigh`（4.5 官方仍是三档）；Claude 4.6 Thinking 为 low/medium/high/max；Claude 5 / 4.7+ 保留 xhigh；GLM-5.2 为 high/max。

## 3.0.7 - 2026-08-25

### 原因

浏览器里 Google 已经显示 Antigravity 授权成功，Router 仍报 `CR-OAU-0008` `http=502` `path=/api/v1/admin/antigravity/oauth/exchange-code`。CLIProxyAPI 7.2.135 换完 token 之后**还必须**访问 `www.googleapis.com/oauth2/v2/userinfo` 才能写 auth 文件。现场日志是 `TLS handshake timeout`：`oauth2.googleapis.com/token` 已经成功，userinfo 超时就把内存里的 token 扔掉。账号进不了号池，订阅额度自然读不到。这和订阅本身是否有效无关。

另外 ChatGPT 官方订阅额度用尽后，Desktop 选 `gpt-5.6-sol` 会立刻 429 `model_cooldown`。官方 OAuth 和第三方中转被编进同一个 CLIProxy 前缀，官方周额度冷却会把中转一起停掉，看起来像中转也挂了。

### 修复

- Antigravity 回调改由 Router 占用 Google 注册的 `51121`，不再把 code 交给 CLIProxy 那条「userinfo 失败则整段作废」的路径。
- Host 在同一台已通的 `oauth2.googleapis.com` 上换 token，邮箱优先从 id_token / tokeninfo 取；userinfo 失败仍保存账号。
- 项目 ID 拉取失败不再阻断登录。
- ChatGPT 官方订阅和同名中转拆成两条优先级线路：官方优先；官方 429 / 额度用尽 / 冷却时立刻改走中转，互不连坐。

## 3.0.6 - 2026-08-25

### 原因

`stream disconnected before completion: error sending request for url (http://127.0.0.1:28085/v1/responses)` 会打在所有模型上。3.0.4 估算 token 时按 512 **字节**切片，中文指令（「你是Grok/Gemini…」）正好切在汉字中间，工作线程 panic，连接被 RST。Desktop 报发送失败，然后狂重试。

另外 Antigravity `503 auth_unavailable` 被当成限流空等 5s/25s/125s，第一个字节都没有，加重断连。3.0.3 还把自动续跑套到了所有第三方模型。

### 修复

- 二进制 blob 探测按字符边界取样，中文请求不再 panic。
- `no auth available` 不再当 429 重试，立刻把 503 交回 Desktop。
- 第一个字节到达前，重试等待上限 8 秒。
- 自动续跑只对 Grok/xAI。

## 3.0.5 - 2026-08-25

### 原因

Antigravity / Gemini 续跑旧线程时，Codex 会把 CLIProxyAPI 写入的 `cpa-gemini-responses-carrier-v1` thought 和 `previous_response_id` 原样送回 Google。Google 侧实体过期或换号后返回 HTTP 404 `Requested entity was not found.`，Desktop 显示 `url: http://127.0.0.1:28085/v1/responses`。现场线程 `01a02cf4`：983 条 input 里有 323 条 carrier。

### 修复

- Gemini 族请求去掉 `previous_response_id` / `conversation_id`，并剥掉 reasoning/function_call 上的 Gemini thought carrier（`encrypted_content` 和 `output`）。对话正文仍在 `input` 里，可以继续跑。
- 上游 404 且正文是 `Requested entity was not found` 时按损坏续接重试；`CR-RTE-0001` 这种「没有这条模型路由」的 404 不重试。

## 3.0.4 - 2026-08-25

### 原因

Grok 长任务会出现 `stream disconnected before completion: Incomplete response returned, reason: max_output_tokens`。3.0.x 把「压缩点还剩多少输入额度」当成输出上限。对话一大（现场网关日志有 3.5MB～14MB 的 `/v1/responses`），估算一过头，剩余额度变成 **1**，网关就写入 `max_output_tokens: 1`。Grok 一开口就撞上限，Desktop 显示截断。这和「按上下文自动调节」的本意相反：越到后半段越需要输出，额度却被压没了。

### 修复

- 未配置卡片上限时：输出预算至少保留窗口的 5%（Grok 再保底推荐的 128k），**不会**因为长上下文压成 1。
- 不把 Codex 已带的 `max_output_tokens` 再往下压。
- 截图/base64/加密 blob 不再按字节当 token 估算。
- 卡片上手动填的上限仍然优先。

### 可观测性

- `CR-STR-0011` `request.max_output`：from/to、remaining、reserve、used。

## 3.0.3 - 2026-08-25

### 原因

Codex 的 agent 循环把「助手只写正文、没有 `function_call`」当成 `task_complete`。Grok（以及其他第三方模型）经常在工具结果之后只写「我先对照…」「接下来…」「我改用看图工具」就结束 SSE，HTTP 仍是 200。用户看到的就是「没做完就自己停了」。这不是 Desktop 截断，也不是网关超时。

### 修复

- 第三方 `/v1/responses` 流若在未完成的 agent 回合里以纯文字结束，网关会先扣住 `response.completed`，再最多自动续跑 2 次（写入 `【自动续跑】` 提示，要求立刻发工具调用）。续跑若产生 `function_call`，拼进同一轮再交给 Codex。
- 已写「任务已完成」或普通问答（没有工具、没有「接下来」这类话）不会误续。
- 模型指令补了一句：只写计划不调用工具会被当成收工。登录身份仍是 3.0.2 的 `Codex-Router` + `requires_openai_auth=false`，没有改回去。

### 可观测性

- `CR-STR-0010`：`gateway-requests.jsonl` / 网关日志 `request.incomplete_continue`（model、第几次续跑、已输出字数）。

## 3.0.2 - 2026-08-25

### 原因

Codex Desktop 26.818 在活动 provider 为 `requires_openai_auth = true` 时，会把账号心跳 `getAuthStatus` 和在线模型列表 `list_models` **并发**打到 `auth.openai.com/oauth/token`。OpenAI 对 refresh token 使用严格轮换：同一张票同时用两次即整族作废（HTTP 401 `refresh_token_invalidated`），Desktop 再升级为 `account/login/start`。这是客户端行为，不是 Router 不能转发。

旧写法 `name = "OpenAI"` + `requires_openai_auth = true` 能让左下角显示 ChatGPT，但在 26.818 上会触发上述并发刷新。只改显示名为 `OpenAI`、开关仍为 `false`（2.1.10）会被 Desktop 当成未登录的 OpenAI。因此 3.0.1 起固定 `name = Codex-Router` + `requires_openai_auth = false`：ChatGPT token 仍留在用户 `auth.json`，请求走本地 Router bearer，Desktop 不再为这个 provider 刷新 OAuth。左下角显示 Codex-Router 是预期结果。

3.0.1 只改了用户层 `~/.codex/config.toml`。系统层 `%ProgramData%\OpenAI\Codex\config.toml` 仍可能是 `true`；Desktop 周期性剥掉用户层 provider（`model = "first"`）后会回落到系统层，登录循环复现。3.0.2 把系统层一并修掉。

### 修复

- Host 启动时修补非法身份；用户层被剥则从系统层恢复；系统层缺失则从用户层重建合法绑定。之后字节相同不再写文件。
- CLI `write-codex-config` 同时刷新系统层绑定。
- 退出 Router 不再删除系统层绑定（否则 Desktop 剥掉用户层后又会回到 ChatGPT OAuth 刷新）。只有「恢复官方配置」才移除系统层。
- 账号模式切换不再按模型族把 `requires_openai_auth` 写回 `true`。

### 可观测性

- `CR-DSK-0001` 用户 config 写入、`0002` 跳过、`0003` 副本实写、`0004` 会话心跳（用户/系统 config 与 auth.json mtime、身份、副本跳过/写入计数）、`0005` 额度观察、`0006` 上游 401 屏蔽、`0007/0008` overlay 写/跳过、`0009/0010` 系统层写/跳过、`0011` 非法身份、`0012` 就地修复、`0013` 用户层 provider 被 Desktop 剥掉、`0014` 从系统层一次性恢复、`0015` 系统层缺失时从用户层重建。事件写入 `router-events.jsonl`。

## 3.0.1 - 2026-08-25

### 修复

- 现场 `logs_2.sqlite` 证实：`requires_openai_auth=true` 会让 Codex Desktop `getAuthStatus` 调用 `Refreshing token`；登录成功后 28 秒再次刷新并 401 `refresh_token_invalidated`，随即 `account/login/start`。Router provider 现固定 `name=Codex-Router` + `requires_openai_auth=false`，请求仍走本地 bearer，**不再让 Desktop 刷新 ChatGPT token**。
- ChatGPT 额度改为纯观察缓存，不再直连 `wham/usage`。
- CLI 副本改为原地写入，避免 watcher 把 delete+rename 当成 REMOVE。
- 诊断事件：`CR-DSK-0001` 写 config、`CR-DSK-0002` 跳过写入、`CR-DSK-0003` 副本实写、`CR-DSK-0005` 额度观察、`CR-DSK-0006` 上游 401 屏蔽。

## 3.0.0 - 2026-08-25

### 修复

- 彻底消除 ChatGPT「用到一半跳回登录页」的 Router 侧剩余触发点：3 分钟自检改为只观察、不再写 `config.toml`；Desktop overlay 往返幂等，相同内容不再产生多余备份或文件变更事件。
- 网关和 Host 不再把上游 401 原样回给 Codex Desktop（改成 503「模型暂不可用」），并先按现有阶梯重试以便落到第三方 API。
- ChatGPT 模型目录同步不再触碰 CLI 副本文件；手动 recover/test 也不再把 ChatGPT 送进 CLI `$TOKEN$`；写入 auth 文件时强制去掉 `refresh_token`；残留 `legacy-openai-*.json` 在后端同步时隔离。
- 偶发 `CR-SYS-0001 terminal span was not completed explicitly` 改为请求取消/中断记录，不再伪装成 500 内部错误。
- 放宽 Grok 额度解析（`data`/`credits` 及 JSON 对象 body），减少 `invalid_response`。

### 架构

- 当时仍允许两种身份：`OpenAI` + `requires_openai_auth=true`（ChatGPT 会话外观）或 `Codex-Router` + `requires_openai_auth=false`。**3.0.1 已撤回前者**：Desktop 26.818 在 `true` 时并发刷新 token。
- `config.toml` 写入统一为「字节相同则跳过」；真实写入才备份，并记录 content hash。

## 2.1.14 - 2026-08-24

### 修复

- 回退 ChatGPT 为 Desktop 单端自检：Router 不再对 ChatGPT 做额度直连、计划探测、恢复或周期性同步；ChatGPT 路由和 API fallback 保留，其他 OAuth 号池不变。

## 2.1.13 - 2026-08-24

### 修复

- ChatGPT 额度查询改用 Desktop `auth.json` 当前 access token 直连，ChatGPT 仍参与订阅额度和 API fallback 调度，但 CLIProxyAPI 不再接触 ChatGPT refresh token；其他 OAuth 供应商继续使用 CLIProxyAPI 号池。

## 2.1.12 - 2026-08-24

### 修复

- ChatGPT OAuth 改为由 Codex Desktop 单独持有并刷新 refresh token；Router 只同步 Desktop 当前 access token，不再在 Router 副本中保存 refresh token。

## 2.1.11 - 2026-08-24

### 修复

- 修复 2.1.10 只把 provider 显示名改成 `OpenAI`、却把 `requires_openai_auth` 写成 `false`，导致 Codex 把已登录的 ChatGPT 会话登出、左下角只剩未登录的 OpenAI。ChatGPT 登录现在保持 ChatGPT 会话：`requires_openai_auth=true`、显示名为 `OpenAI`，请求仍走本地 Router bearer/catalog，不覆盖 `auth.json`。
- API Key 登录仍保持 API Key：`requires_openai_auth=false`、显示名为 `Codex-Router`。Router 不会再拿死 refresh token 反复打上游。
- 修复 ChatGPT 远程压缩 v2：`/v1/responses/compact` 不再被注入 `max_output_tokens` / tools / stream，避免官方 compact 返回 2 条普通输出、0 条 compaction item。

## 2.1.10 - 2026-08-24

### 修复

- 拆开 ChatGPT 身份与自动登录弹窗。`codex_router` 请求仍走本地 Router bearer/catalog，`requires_openai_auth` 固定为 `false`，ChatGPT token 失效时 Desktop 不再自动弹出登录窗。
- 已登录 ChatGPT 或 Router 默认 OAuth 会话时，provider 显示名为 `OpenAI`，左下角不再显示成 Codex-Router 登录；API Key 登录仍显示 `Codex-Router`。不覆盖、不删除 `auth.json`。

## 2.1.9 - 2026-08-24

### 修复

- 修复 2.1.8 把 `codex_router` 一律写成 `requires_openai_auth=false`，导致 Codex Desktop 被强制切到 Router 登录、反复弹出“登录 ChatGPT”的问题。请求仍走本地 Router bearer 与 catalog；登录方式按 Codex 现有会话保留：已登录 ChatGPT 保持 ChatGPT，第三方 API Key 登录保持 API Key，不覆盖 `auth.json`。
- API Key 登录仍会清掉残留的 `forced_login_method = "chatgpt"`；ChatGPT 登录不再被改成 Router 登录。

## 2.1.8 - 2026-08-24

### 修复

- 修复 Codex 本地 Router provider 被写成 `requires_openai_auth=true`，导致 Desktop 即使全部模型请求都使用 Router 独立 bearer，仍把 ChatGPT 登录视为强制前置条件；当 `getAuthStatus` 刷新收到 `refresh_token_invalidated` 时会自动打开登录窗口。Router provider 现固定为 `requires_openai_auth=false`，不删除、不覆盖 Codex 自己的 `auth.json`，本地模型与 catalog 继续使用 Router bearer。
- OAuth 模型目录刷新不再访问 disabled、unschedulable、401 冷却或物理 auth 文件缺失的账号，避免隔离后的 `legacy-openai-1.json` 仍被周期性请求 CLI models 端点。

## 2.1.7 - 2026-08-24

### 修复

- 修复 GUI 显示“OAuth 恢复探测完成：healthy=… deferred=… recovered=0”时仍对显式禁用或等待重新认证的 OAuth 账号发起额度/恢复探测，可能重放死 refresh token 并让 Codex 再次弹出 ChatGPT 登录的问题。显式禁用账号现在只读本地统计，GUI 恢复、Host `recover-state` 与定时 scheduler 均不得触达其凭据；401 冷却也不能被恢复端点穿透。
- OAuth `recover-state` 探测失败不再回退为直接设置 schedulable；CLI 同步失败会恢复账号原状态。OpenAI 恢复批次不再重复调用两个指向同一处理器的 quota 端点。

## 2.1.6 - 2026-08-24

### 修复

- 复合路由改为一次事务批量写入并只触发一次 CLIProxyAPI 全量热加载，避免逐条路由连续推送配置导致 `ROUTER_DEPLOY_COMPOSITE_FAILED` / `CR-CFG-0005`。
- 未变化的 OAuth 路由副本不再重复发布；变化时先完整写入唯一临时文件再替换，避免 CLI 文件监控器在 Windows 独占写窗口读取失败。
- OAuth 额度刷新失败保留在原始 JSONL 供诊断，但不再刷活动日志；合法 `control.*` 事件名不再被误显示为 `[REDACTED]`。
- 修复同一 ChatGPT 账号同时登录 Codex 桌面端和 Router OAuth 渠道时，Router 侧过期 refresh token 被每分钟反复重试、导致 OpenAI 吊销整个令牌族并让 Codex 反复弹出"登录 ChatGPT"的问题：OAuth 账号首次 401 后进入 1 小时重认证冷却，冷却期只展示缓存额度、不再触达上游；手动恢复探测仍为 401 时重新进入完整冷却；被禁用/隔离的 OAuth 账号不再发起实时额度调用；定时探测只对 OAuth 账号执行冷却跳过并在 401 时联动冷却。
- OAuth 重新登录成功后按上游账号 ID / 邮箱复用并恢复原账号、替换 CLI auth 文件和稳定身份、解除重认证冷却；旧死令牌文件无人共用时立即删除，同级身份命中多个旧账号时拒绝猜测，避免恢复错账号或遗留死令牌继续刷新。普通账号信息保存不再误解除冷却。

## 2.1.5 - 2026-08-24

### 修复

- 普通用量刷新不再执行 OAuth 账号隔离、恢复或路由重建；额度刷新失败只展示 stale/unknown 状态，不再循环触发 `control.oauth_quota_refresh_failed` 和 fallback 重同步。
- 五小时未知额度保护统一由已有 OAuth 自检执行，只有自检获得真实可用/耗尽证据时才改变调度；网络、权限、认证和无效响应不会被误判为额度耗尽。

## 2.1.4 - 2026-08-23

### 功能

- 软件内点「检查更新」后：发现新版本会自动下载、校验、覆盖当前程序目录并重启，无需再去 GitHub 找包或手动解压。`%LOCALAPPDATA%\Codex-Router\UserData`、已配置模型和 Key 不会被覆盖。
- 官方 GitHub API / 资源失败时（含 403、429 限流）自动改走国内公开镜像（ghfast / ghproxy / kkgithub），不再误报需要 GitHub CLI 登录或 `OAuth token is missing`。

### 修复

- 每次手动或后台自检都会逐个刷新当前已选 OAuth 订阅账号的实时额度；前一轮额度查询尚未结束时，新一轮自检会排队补跑，不再被静默丢弃。额度恢复后立即同步账号状态并重新加入对应号池。
- Grok billing 查询失败时返回的 `source=cache` / `stale=true` 只用于界面保底展示，不再冒充本轮实时额度或刷新成功缓存时间。停用中的 Grok 账号只有在实时额度明确可用，或使用当前模型完成一次最小生成探测后才恢复；仅 `/models` 可访问不再视为额度已恢复。
- Codex 当前任务被取消、Router 掉线或断网时，网关重试现在每 100ms 检查客户端是否仍连接；Codex 已结束请求就立即释放 worker，不再继续跑完整重试梯度。
- 每个 Codex 任务共用一份重试预算，累计退避最多 180 秒。默认 3 次仍完整保留 5s / 25s / 125s；即使滑条设为 32，也不会进入 625 秒和单步 1 小时的等待。连接失败会返回明确 502，已打开 SSE 后则发送 `response.failed` 终止事件，保证当前任务可结束并允许继续发消息。
- 修复 SSE 断流后重试又收到 429/5xx 时把第二份 HTTP 响应写进已打开事件流的问题；现在只保留第一份 HTTP 响应，并用一个合法终止事件收尾。
- 完整并入 2.1.3 稳定性链。Codex 用户层/系统层绑定均丢失时显示三按钮覆写窗；主窗有焦点时累计 3 秒后自动写回当前端口、retries、bearer、catalog 和协议字段并重启 Codex。最小化、托盘、失焦期间倒计时暂停且不抢焦点；“保持现状”仅记住当前文件指纹，新覆写会重新提示。
- “恢复默认”继续作为粘性官方逃生模式：新线程不走 Router，自检不自动绑回；只有用户重新启用转发后才恢复绑定保护。默认压缩仍为 95%，用户自定义百分比保留；未指定最大输出时按剩余压缩预算与模型硬上限动态计算。

### 说明

- 源码版本已升到 2.1.4；默认交付只构建便携版，installer 仅在明确要求时构建。本轮未发布 GitHub。

## 2.1.3 - 2026-08-23

### 修复

- 启动/自检发现用户层和系统层都丢了 Router 绑定时，自动写回标准绑定并记日志，不再先弹「Codex 原生配置已被外部程序覆写」。修失败才弹窗。选择「保持外部配置」时，系统层写入失败也会写入活动日志。UserData 不会被覆盖。
- 用户卡片填了最大输出仍听用户。没填时按当前已用上下文重算：`min(模型硬上限, 离压缩点还剩多少)`。不再用 Codex 按整段窗口 5% 算出的上限。Gemini 硬顶 65536；DeepSeek V4 384000；Kimi 131072；Claude 128000。Grok / GPT / 未知模型没有公开硬上限，就用剩余压缩预算，进度条还剩多少就可以写到接近压缩点。
- Codex `request_max_retries` / `stream_max_retries` 继续跟随订阅页滑条（默认 3，可改 0–32，5s×5），没有压成 1。

### 说明

- 源码版本已升到 2.1.3；本轮未打便携包、未发布 GitHub。
- Gemini 官方输出硬顶仍是 64k，靠近压缩点时仍可能看起来像提前停，这是模型上限而不是 5% 公式。

## 2.1.2 - 2026-08-23

### 修复

- 订阅号有额度时会按卡片优先级重新进号池。ChatGPT / Grok 等 OAuth 副本写入 CLI `priority`/`weight`，旧隐藏档（如 P2 写成 2100）折叠回 1–999，不再被第三方 API 抢先。
- 同一模型的多个订阅号按 P1 / P2 / P3 调度，不再等权轮询。三个 Grok 号会先打高优先级账号。
- ChatGPT OAuth 只挂到已映射的 GPT 路由，不再误进 Kimi / GLM / DeepSeek 的 openai 池。
- Coding Plan 同一 Base URL 下每个 Key 各自显示 5 小时 / 周额度条。相同 Key 仍合并。

### 说明

- 同时提供便携包和用户级 installer。
- 保存并应用后，CLI 号池才会按新的优先级重编译。

## 2.1.1 - 2026-08-22

### 修复

- 订阅页的「上游 429 / 断网自动重试次数」默认 3，可改 0–32；保存并应用后同时写入 Codex `request_max_retries` / `stream_max_retries`。间隔仍是首次 5s，之后每次 ×5（25s / 125s / 625s …，单次封顶 1h）。
- Codex 进度条分母按 `窗口 × 百分比` 写入 catalog（Grok 500k @ 95% = 475k），不再把 `auto_compact_token_limit` 写成完整窗口。etag 随百分比/压缩窗口变化，保存时删除 `~/.codex/model-catalog.codex-router.json` 陈旧 80%/400k 副本。
- Grok 在用户未自定义最大输出时，把 Codex 按剩余百分比算出的 `max_output_tokens`（95% 窗口上约 5%/2 万 token）抬到 128k，避免上下文每过约 5% 就断流。
- 同一 Coding Plan Base URL 的多个 Key 共用一条额度条。两个 Kimi Coding Plan Key 不再各显示一条 5 小时/周额度。
- 网关在工具调用/推理事件之后断流也会续跑，不再当成交接收工。
- 同一订阅池只要任一账号选中了某个模型，其余已选账号默认补上同样的模型槽。三个 Grok 号会合成一张「3 · 独立订阅」卡片。
- 同平台多个 OAuth 账号不再全部写成 P1；Apply / 保存时按账号顺序写成 P1、P2、P3。
- Grok 官方 `/v1/responses/compact` 透传：强制非 stream、保留 xAI compaction blob；失败立刻降级本地 OpenCode 折叠。ChatGPT 仍走官方 compact，其他第三方仍本地折叠。

### 说明

- 只提供便携包，不提供 installer。
- 需要保存并应用后，Codex Desktop 才会读到新的重试次数和 Grok compact 开关。

## 2.1.0 - 2026-08-22

### 功能

- 启动时把 2.1.0 之前配置里的模型卡片自动压缩一次性升到官方 95%，并写回 catalog。Codex 进度条分母按 `窗口 × 百分比` 计算，Grok 从 400k 变为约 475k。2.1.0 之后用户手动下调的百分比会保留。
- ChatGPT 以外的网关压缩改为 OpenCode 式：先截断较早工具输出（约 2000 字），再在 `/compact` 时折叠较早记录并保留近文；摘要使用 Goal / Files / Pending / Current 续跑结构。失败仍降级为本地折叠假成功，避免无限 compact。ChatGPT / OpenAI 家族继续走官方 compact。

### 说明

- 未打开 Grok 官方 `/responses/compact`。
- 只提供便携包，不提供 installer。

## 2.0.19 - 2026-08-22

### 功能

- 自动压缩默认改为官方 95%，滑条 60–95 仍可调。catalog 把 `auto_compact_token_limit` 写成完整 `context_window`，Codex 看到完整窗口（Grok 500k），不再用 80% 当有效窗。
- API Key 有效但填写的模型 ID 不在上游 `/models` 时，弹出可滚动可用列表，点选即添加。`gpt-5.6` 仍 canonical 到 `gpt-5.6-sol`。
- 调度源统一为账号 priority：模型卡片拖拽顺序即账号 P 值（01→P1，02→P2）；Apply 把同一 OAuth 账号全部槽位写成同一个 P；订阅页改 P 也会写回槽位。
- 本版本只提供便携包，不再提供 installer。

### 修复

- Codex 对 Grok 的 compact handoff（“Another language model started…”）改写成续跑指令，避免把摘要当交接收工导致空回复结束。
- 保留自动补槽位：新 Grok 号挂到现有 4.5/4.6 卡片；catalog 为空也补。

### 说明

- 旧配置里已经写成 80% 的模型卡片不会自动改到 95%。
- 未打开 Grok 官方 `/responses/compact`。

## 2.0.18 - 2026-08-22

### 修复

- 修复 ChatGPT Plus 额度恢复后仍走第三方 API：恢复探测不再打 `api.openai.com/v1/models`（OAuth token 会 403），改为 Codex `chatgpt.com/backend-api/codex/models`。live quota 可用时即使探测失败也会重新 `schedulable=1`，订阅优先重新生效。
- 关闭 CLI `session-affinity`。额度恢复后旧线程不再粘在回退 API 上，按当前优先级重新选号。
- 停用的 OAuth 账号在额度未满时不再被 UI/恢复逻辑当成健康号；隔离判定包含 `quotaExhausted` / `cooldown`。

## 2.0.17 - 2026-08-22

### 修复

- 修复 OAuth ChatGPT 在 2.0.16 换入后仍发送空 `spawn_agent {}`，Desktop 报 missing field message / 「后台分工接口当前拒绝了有效任务参数」。实测命令已走 `exec_command`，但 catalog 仍开着 v2 collaboration。现改为 v1 子 Agent，保留 web search 与 JSON `exec_command`。

## 2.0.16 - 2026-08-22

### 修复

- 修复 Grok 解释推理标签时答案被切在行内代码反引号处：`strip_think_tags` 把 Markdown 行内/围栏代码中的开标签当字面量，不再把后文整段丢掉；真推理块和未闭合的真实截断流仍剥离。
- 修复 OAuth ChatGPT 经 Router 后把 `spawn_agent {}` / 无参 `exec` 发给 Desktop，界面提示“后台分工接口当前拒绝了有效任务参数”。官方 catalog 的 `code_mode_only` + `use_responses_lite` 不再抄到 Router 转发路径；OAuth ChatGPT 保留 web search 与 v2 子 Agent，工具改为 JSON `exec_command`（`tool_mode=default`，完整 Responses）。

## 2.0.15 - 2026-08-22

### 修复

- 同一服务商号池共享到该平台已有模型卡片：新授权的 Grok 账号会同时出现在 Grok 4.5 / 4.6 等现有渠道上，卡片显示“3 · 独立订阅”，不再只给默认模型加一条。
- “撤销此订阅”改为先弹出该服务商全部已有账号，再选择要撤销哪一条。
- 过滤活动日志里的 `ledger.record_failed`（request ledger entry already exists）刷屏；Host 对同一 request_id 的重复记账不再记 WARN。
- 保留并收紧上游半截断流重试：Host 合成的 `Upstream stream ended before completion` / `before a terminal event` 仍由网关重试并对账，不把合成 failed 直接交给 Codex。

## 2.0.14 - 2026-08-22

### 修复

- 修复 Grok 等 OAuth：网页已显示“设备已授权”、号池也出现新账号后，界面仍转圈“正在等待当前订阅授权完成”，超时后弹出“授权未完成”。账号进入 Router 即结束等待并弹出“授权已成功”；等待期间轮询失败不再把整次登录打成失败。
- 第三个及后续 OAuth 账号即使模型目录暂时为空，也会立即写入当前路由配置卡片（Grok 默认 grok-4.5），优先级弹窗不再只显示旧的两个号。
- 活动日志不再把 Router 事件压成空的 `class=request_failure` / `class=configuration` 刷屏；配置同步事件不再进活动日志，其余 WARN/ERROR 会带上 `event=` 与错误码。

## 2.0.13 - 2026-08-22

### 修复

- 修复旧 CLI YAML 中 `openai-compatibility` 条目缺少 `openai-capabilities` 时 Router Host 启动即退出，进而导致 `[2/7]`、`class=configuration`、OAuth 账号为 0 和管理会话/用量查询连锁失败。
- 端口隔离改为整体检查 Router Host、派生 CLIProxyAPI 与 Responses Gateway，只有派生端口冲突时也会自动换到完整空闲端口组。
- 完整继承 2.0.12 的历史线程休眠 provider、退出保护和 Router events 启动回放时间过滤修复，并重新构建完整发布包。

## 2.0.12 - 2026-08-22

### 修复

- 新增 API 渠道测试连接失败时弹出“本配置无效，请检查配置”，成功添加时弹出“添加成功”。
- 各平台 OAuth 授权成功后弹出“授权已成功”，不再只写状态栏后继续转圈等待。
- Grok 等网页显示设备已授权、但本机仍显示“正在等待当前订阅授权完成”时，改为同时轮询新账号；账号已进入 Router 即结束等待并同步模型。
- 已有模型列表时，新授权的第三个及后续 OAuth 账号会自动加入当前配置的默认模型，不再只出现在 OAuth 页。
- 修复 Codex 对话框 `stream disconnected before completion: upstream stream ended before a terminal event`。Host 在 CLIProxyAPI SSE 提前结束时会合成 `response.failed`/`CR-UP-0014`；网关把它当成可重试断流，不再转发给 Codex。无内容静默重试，纯文本流对账后只补后缀，tool/reasoning 仍干净收尾。额度等真实业务失败继续转发。

## 2.0.11 - 2026-08-22

### 修复

- 修复 Codex 对话框 `stream disconnected before completion: error sending request for url (http://127.0.0.1:28082/v1/responses)` 与 `error decoding response body`。
- 根因是本机 Responses 网关 listener 非阻塞，Windows 上 accepted 套接字继承非阻塞；大请求写回 SSE 时触发 WSAEWOULDBLOCK (10035)，旧逻辑当成客户端取消并 RST，Codex 解码失败。Kimi 与 ChatGPT 都走同一条 28082 本地链路。
- 网关 accept 后强制阻塞套接字和 TCP_NODELAY；读写遇到 10035/WouldBlock 短睡重试；请求读完后超时从 30s 提到 300s；响应结束 half-close(FIN) 并排空，避免 RST。
- `connection: close` 响应头改为标准 CRLF。
- Router Host 对截断的上游 JSON 响应体返回 502，不再把半截 body 当成成功。

### 说明

- 上游仍是 CLIProxyAPI 7.2.135 + Router Host；`28082` 是 Codex-Router.exe 内嵌网关。
- 升级后请完全重启 CodexRouter，并用新线程验证；旧失败对话框不会自动消失。

## 2.0.10 - 2026-08-22

### 功能

- 模型编辑面板新增最大输出 Token（默认 0 = 不发送，使用上游默认）；网关按请求 model 注入 `max_output_tokens` / `max_tokens`，映射可热更新。

### 修复

- 网关对 OpenAI 家族透传强制 `connection: close`，并增加 `gateway-requests.jsonl` 诊断。
- 纯文本 SSE 中段断流自动重试并对账去重：只补后缀，分叉则干净 `response.failed`。
- 绑定探测同时检查用户层与系统层；系统层在位不再弹“路由绑定已变化”，用户层无效 model（如 `first`）静默修复为默认模型。
- 托盘恢复后中文方框：字体安装失败不再误置成功，2s 节流重试，CJK 候选扩充，交互时强制恢复全量字体。

## 2.0.9 - 2026-08-22

### 修复

- 断网 / 请求错误 / 429 自动重试改为首次 5s、之后每次 ×5（5s / 25s / 125s / …，单步封顶 1h）；默认重试次数 6 → 3，UI 可自定义 0–32。
- 重试覆盖产内容前所有阶段：SSE 首事件前断线静默重试，JSON/错误响应体中途断线整请求重试；已输出内容后断流保持干净 `response.failed`。

## 2.0.8 - 2026-08-21

### 修复

- 同模型不同渠道严格按用户 P 值调度，移除 Relay、Coding Plan、API 类型的隐藏 tier；学校 P10 会先于火山 P20。
- 同一个 API Key可服务多个模型和渠道，不再因 API Key被误用为账号唯一身份而导致第二条渠道写入失败。
- 新增 API 保存前同时验证模型列表和最小生成；若 Responses 不兼容但 Chat Completions 可用，会自动保存正确协议。
- OpenAI 兼容渠道的 `openai-capabilities` 现在完整传递到 CLIProxyAPI配置，并兼容缺少该字段的旧配置。
- Codex公开模型 ID保持 canonical 小写去重，渠道上游模型 ID保留服务端声明的原始大小写；学校 DeepSeek Pro现使用 `DeepSeek-V4-Pro` 和 Chat Completions。
- 归档修复改为离线协调：Codex运行时绝不再修改 `state_5.sqlite`；检测到 Codex连续完全退出后，先做完整性检查和备份，再根据 Codex自己的 `thread/archive` 日志完成停在 active-thread shutdown阶段的归档。
- 离线协调器仅规范化真实文件存在的扩展路径，不会归档用户未请求归档的对话；支持文件已移动但数据库尚未提交的中断恢复，并在失败时回滚文件移动。
- 升级启动时若整个 Codex应用包已经完全退出，会立即执行一次离线协调；若仍有 Codex/ChatGPT主进程或子进程存活则严格跳过，避免内存状态覆盖数据库修复。

## 2.0.7 - 2026-08-21

### 修复

- 修复独立安装向导未初始化中文字体、中文标题和按钮显示为方框的问题；安装向导现在与主界面共用完整 CJK 字体配置。
- 字体发现改为使用 Windows 实际安装目录，不再固定假设系统安装在 `C:\Windows`。
- 修复同名模型优先级拖拽结果只存在于释放当帧、点击保存时恢复旧顺序的问题；拖拽顺序现在跨帧保留，保存会原子校验同名模型集合并同步配置顺序、P 值和订阅/API 路由策略。
- 修复归档兼容问题：Apply 不再无条件重启 Codex，启动时仅规范化普通路径文件确实存在的 `\\?\` rollout 路径，避免 active thread 关闭后因路径不一致而无法继续归档。
- 模型菜单按路由配置卡片的首次出现顺序生成；已知模型 ID 统一为 canonical 小写身份，大小写不同的同一模型只显示一次，但不同 API/订阅渠道仍保留在同一优先级池。
- 同模型不同 API 渠道使用独立、稳定且不含密钥的后端账号身份，避免同名渠道互相覆盖，确保每条 P 值都参与实际调度。
- API 账号唯一身份不再使用 API Key；同一个 Key可同时服务多个模型或渠道，不会因后端唯一约束导致第二条渠道写入失败。
- 移除 Relay/Coding Plan/API 类型的隐藏优先级加权；后端调度严格使用用户设置的 P 值和配置顺序，P10 不再被暗中改成 P2010 后排到 P20 之后。
- 新增 API 渠道在正式添加前异步验证 `/models` 和最小生成请求；DNS/VPN/代理、401/403、模型缺失、限流或上游 5xx 会给出安全提示并阻止保存，不记录 API Key 或响应正文。

## 2.0.6 - 2026-08-21

### 修复

- 修复 DeepSeek/Kimi/Grok 子 Agent 只返回空闲问候：Codex multi-agent v2 的任务正文位于 `agent_message.content[].encrypted_content`，Router 现在将其转换为标准 `message/user + input_text` 并逐字保留任务，不再改成 omitted 占位文本。
- 修复 Apply 后对话无法归档：Router 不再在每次 Apply 后无条件关闭并重启 Codex，避免全部线程被重新加载为 active、`thread/archive` 只完成 shutdown 而不执行归档移动；需要冷重载时由成功弹窗提示手动重开。
- 同名模型优先级弹窗改为单行自适应：只保留左侧拖拽手柄，移除重复上下箭头；显示 `来源类型 · 厂商 · 邮箱/账号或 Base URL`，空间不足时自动换到第二行，悬停显示完整账号 ID/登录账号或完整 URL。
- 优先级保存同步模型 P 值、订阅账号后端优先级和订阅/API 优先策略，确保弹窗顺序就是实际调度顺序。

升级后请完全重启 CodexRouter；需要加载新目录时再手动重启 Codex。

## 2.0.5 - 2026-08-21

### 修复

- 修复使用中突然跳回登录：选择“保持覆写配置”时仍刷新系统层绑定，保留 DACL 的原子写入并重试 sharing violation，避免 Codex Desktop 重写用户配置后路由丢失、鉴权失败导致的登录页；首个健康与 OAuth 恢复探测延后至 60s/300s，避免打开后几秒即触发刷新；ChatGPT 探活改用 `api.openai.com/v1/models` 消除 400 误判；DeepSeek 默认子 Agent 配置写入与 1800s 空闲超时已校正。
- DeepSeek 等模型不再明文透出 `<think>` 思考过程：Router 在 Gateway 与 Host 对流式与非流式响应双向剥离 `<think>` / `<thinking>` 标签（大小写不敏感、支持截断流）。
- 修复子 Agent `spawn_agent` 超时：非 OpenAI 家族模型现在也会写入 `default_subagent_model` 与推理档位，子 Agent 按当前模型正确路由；DeepSeek/Coding Plan 的长推理静默不再在 5 分钟内中断。
- 修复“保存并应用”后无法归档对话：原子写入改用 `ReplaceFileW` 保留文件权限并自动重试占用冲突，避免回滚后 `model_catalog_json` 指向被删除的临时文件。
- 路由配置模型卡片新增【优先级】按钮：当存在多条同名模型（多 API/订阅）时，在“设为默认”与“编辑”之间显示；弹窗支持拖动（≡）或 ↑↓ 调整调用顺序，默认订阅靠前，可自主编排，保存后生效。
- 降低日志噪音：`ChatGPT` 探活改 `api.openai.com`、`account_recovery_probe_failed`/`scheduler.probe_failed` 改 INFO，`think` 标签剥离减少上游 400 误报。

### 说明

- `Gemini` 当前仍不支持 Codex 子 Agent 与部分 web_search：属上游模型/协议限制，不是 Router 缺陷。
- DeepSeek 官方 `<think>` 透出属模型原生行为，已由 Router 统一隐藏；如需保留思考过程可后续提供开关。

升级后请完全重启 CodexRouter 和 Codex。

## 2.0.4 - 2026-08-20

### 修复

- 首次条款弹窗按可视区域自适应，固定标题和操作区，不再遮挡顶部或底部按钮。
- 弹窗统一为主题色标题栏、浅色正文、圆角边框和实体背景，移除半透明毛玻璃弹窗。
- 首次 OAuth 只有在账号、模型注册和 Router 验证完成后才结束，并自动启用第一个可用模型。
- 原生运行时补齐首次管理员凭据初始化，修复干净机器上开发环境历史状态掩盖的 OAuth 失败。
- 完全退出会等待 Gateway、Router Host 和 CLIProxyAPI 退出并验证监听端口释放。
- 显式 UserData 隔离优先于便携状态，运行日志移出发布目录，CLI 端口和认证目录每次启动都会校准。
- VC++ Runtime 同时放置在主程序和 Router Host 目录，保证未安装系统运行库的干净机器也能启动子进程。

升级后请完全重启 CodexRouter 和 Codex。

## 2.0.3 - 2026-08-20

### 修复

- ChatGPT OAuth 回调只由 CodexRouter 监听，避免与 CLI 本地登录服务器争抢固定端口；端口被占用时明确切换为粘贴完整回调 URL。
- 实时用量按提供商合并账号卡；Grok、xAI、x-ai 账号统一使用同一套实时额度查询、凭据索引与错误展示逻辑。
- 当前订阅卡不再重复显示“共享配置”；共享开关仅保留在配置分组切换区。
- 配置加载后同步记录当前软件版本；升级和 Apply 继续保持 Codex 登录状态、聊天目录及模型目录绑定。

升级后请完全重启 CodexRouter 和 Codex。

## 2.0.0 - 2026-08-18

### 变更

- 本地运行时整体迁移：Sub2API + PostgreSQL + Redis 替换为 CLIProxyAPI + CodexRouter Host + 嵌入式 SQLite，安装包体积与内存占用显著下降。
- 启动、停止、状态、修复与开机自启全部改由原生 Router Host 生命周期管理；CLIProxyAPI 进程随 Host 自动退出，不再残留孤儿进程。
- 用量账本、路由状态、OAuth 账号与计划任务迁移到本地 SQLite；旧版本数据在首次启动时自动迁移。
- 日志页改为展示 Router Host / CLIProxyAPI / 结构化事件流；状态页组件同步更名。
- 条款更新至 v1.3：Sub2API 专项条款替换为 CLIProxyAPI（MIT）组件说明，升级后需要重新阅读并确认。

升级后请完全重启 CodexRouter 和 Codex。

## 1.7.10 - 2026-08-17

### 新增

- 路由配置列表将多个 OAuth 账号的同名模型合并为一项；名称后显示可提供该模型的账号数，悬浮提示该数字含义。
- 模型卡片底部额度只展示当前正在使用的账号 / API。

### 修复

- 干净 new 版本首次启动强制进入引导首页，不再因为读到本机已有 UserData 而直接进入控制台。

升级后请完全重启 CodexRouter 和 Codex。

## 1.7.9 - 2026-08-17

### 修复

- 修复别人机器上首次引导第一次 OAuth 无法配置：未完成设置时不再把缺失的 Router 配置文件误报成 `class=configuration`；账号刷新改用当前内存配置，并在后台用这份配置启动本地 Router。
- 条款页“安全登录环境准备失败”继续显示具体阶段，并在冷启动 initdb / Redis / Sub2API 未就绪时自动重试准备。

升级后请完全重启 CodexRouter 和 Codex。

## 1.7.8 - 2026-08-17

### 修复

- 严格区分额度来源与接口协议：`subscription | coding_plan | official_api | relay` 与 `responses | chat_completions | anthropic` 分开保存；不再因厂商名自动切官方协议。
- 订阅与 Coding Plan 默认 `allow_fallback = false`，禁止因 401/429/5xx/断流静默切到同厂商 PAYG。ChatGPT 订阅不会静默切 OpenAI PAYG；Grok 订阅经 sub2api 不会静默切 xAI PAYG；火山 Coding Plan 固定 `https://ark.cn-beijing.volces.com/api/coding/v3` Responses，禁止切 `/api/v3`。
- Kimi Coding Plan 仍按实际 Chat 协议处理，由 Router 做 Responses ↔ Chat 转换；DXH / CIRL 优先直接 Responses。
- Gemini 按实际线路（官方 API / sub2api / DXH / CIRL）决定协议，不再仅凭 `gemini-*` 模型名判断。
- 上游 429 / 无网络最大尝试从 3 次改为 6 次，阶梯等待 `2s → 10s → 30s → 1min → 3min → 5min`；短 `Retry-After` 不能缩短等待，避免 RequestBurstTooFast 瞬间耗尽重试。
- 自检覆写弹窗三个选项改为卡片式说明，并接到真实功能：应用当前设置会保存并写入现有 CodexRouter 配置后自动重启 Codex；保持当前覆写结果不改任何文件；恢复 Codex 默认设置会移除 Router 绑定并自动重启 Codex。

升级后请完全重启 CodexRouter 和 Codex。

## 1.7.7 - 2026-08-17

### 修复

- 修复模型降级兜底仅对 ChatGPT-5.6-Sol 生效的问题：`chatgpt-*` 品牌命名的渠道统一归一到对应 `gpt-*` 模型族，OAuth 与同名 API 渠道的身份配对、手动备用选择和自动隔离接管对所有接入模型生效，切换 Luna 等变体后兜底不再失效。
- 修复 Grok、Gemini 长会话中途被擅自中断：网关改为 30 秒轮询上游流并注入 SSE 保活注释，长时间推理静默不再触发 Codex 空闲超时；上游完全静默的上限放宽到 30 分钟；上游分块帧损坏时以显式终止事件收尾，不再静默断开。
- 修复全新设备首次引导高频弹出“本地 Router 未能稳定启动”：OAuth 准备/登录的生命周期锁等待从 10 秒放宽到 120 秒以容纳冷机 initdb 与并发 Apply；预热重试扩为 4 次（2/5/10 秒阶梯）；Sub2API 就绪检测改为低成本探针优先（health → Redis → PostgreSQL），总预算放宽到 180 秒；健康检查先通过但 Windows TCP 属主表尚未刷新时继续等待而不是直接判失败。
- 修复火山方舟 CodingPlan（DeepSeek V4 Flash）429 限流直接终止对话：网关对 429 与 Sub2API 额度耗尽型 503 做阶梯退避自动重试（2/4/8/16/30 秒，封顶 60 秒），长 `Retry-After` 不再放弃重试而是按封顶值等待；最大重试次数可在 OAuth 设置中配置（默认 8 次），耗尽后才向 Codex 正常抛出限流提示。

### 新增

- 自检识别到 Codex 原生 `config.toml` 被外部程序覆写时，不再静默自动修复，改为弹出交互选择窗口：① 写入 CodexRouter 标准配置；② 保留当前已被覆写的配置（按内容指纹记忆，文件再次变化时重新提示）；③ 恢复 Codex 官方出厂默认配置。
- 应用配置与修复绑定时，自动把 Router 绑定（模型提供方、模型目录、Fast 开关、推理档位菜单）同步写入 Codex 的系统层配置 `%ProgramData%\OpenAI\Codex\config.toml`：Codex Desktop 周期性重写用户 `config.toml` 并丢弃 Router 键时（此前会导致 Grok/Kimi 等非 ChatGPT 模型被直接发往 ChatGPT 后端，报 “model is not supported when using Codex with a ChatGPT account”），新会话仍自动走本地路由，全部已注册模型不受影响。恢复出厂设置、初始化默认配置与关闭 Router 路由时会自动移除该绑定。

升级后请完全重启 CodexRouter 和 Codex。

## 1.7.6 - 2026-08-17

### 修复

- 修复全新电脑首次引导 OAuth 准备失败：预热与正式登录直接使用向导当前的内存配置，不再要求 `%LOCALAPPDATA%` 中预先存在已 Apply 的 Router 配置。
- 修复 Gemini/Antigravity 流式文本工具调用被清理后丢失的问题：泄漏的 `functions__exec` 会转换成结构化 `function_call` 事件，失败的工具输出保留 `call_id` 以继续下一轮生成。
- 修复火山方舟 Coding Plan 429 快速耗尽重试：尊重短 `Retry-After`，无 header 时按 2/4/8 秒退避，长时间限流直接返回；Coding Plan 并发降为 2，Codex 请求层重试降为 1，避免嵌套重试风暴。
- 保留 Grok 4.6 视觉能力；仅当具体上游明确拒绝图片输入时，执行一次文本降级重试，移除图片内容但保留正文、工具历史和 `call_id`，提示模型告知用户图片未读取后继续任务。

升级后请完全重启 CodexRouter 和 Codex。

## 1.7.5 - 2026-08-16

### 修复

- ChatGPT 请求/SSE 原样转发，降低第二轮长时间卡在“正在思考”的概率；默认重试改为 3 次。
- 去掉 Luna 默认子 Agent 绑定，保留 ChatGPT 手动子 Agent 能力。
- 托盘/后台唤起后恢复窗口尺寸与完整字体，最大化不再误入轻量布局。
- DeepSeek 官方接口不再走自动代理；限流后的本地 503 改回 429，避免误当成服务挂掉再狂重试。
- Kimi/DeepSeek 身份与 `exec_command` 工具面继续沿用 1.7.4 后续修复。
- Grok OAuth 账号返回 402/429 时立即触发多账号故障转移；切换账号后规范化跨账号会话历史，避免健康账号因 `ModelInput` 422 被包装成 502。
- 修复 Gemini/第三方模型长任务中途停止：未开始输出的 429 会在网关内重试以等待健康账号接管；SSE 未收到完成事件就断开时不再伪装为正常结束，而是触发 Codex 的流重试。
- 修复火山方舟 Agent Plan 填写控制面 AK/SK 后未保存的问题；Coding Plan 与 Agent Plan 共享同一组 Windows 控制面凭据和额度池，官方额度请求改为签名 `{}` 请求体。
- 修复 ChatGPT OAuth 耗尽后旧的未选中 OAuth 记录仍参与调度、阻止第三方按量 API 兜底接管的问题；未选中记录会退出 Router 组并停止调度，重新选择后仍可按额度恢复。
- 修复 API 用量把内部 OpenAI-compatible 传输类型误当成业务平台的问题；Kimi Coding Plan、Volcengine Coding Plan 与普通 API 中转站不再显示为同一个 `OPENAI` 额度池，不同中转路径也不再误合并。
- 修复侧边对话 Agent 工具被禁用：Router 不再删除用户 `[agents]` 配置；ChatGPT 使用原生 `multi_agent_version=v2`，DeepSeek、Kimi、Gemini、Grok、Claude、火山等第三方模型使用兼容 `v1` Agent 协议并开放 shell 工具。
- 修复对话结束后弹出 `18082/login?redirect=/v1` 网页预览：Responses 网关对浏览器根路径返回本地状态页，管理后台入口使用 18080；ChatGPT 登录契约保持不变。
- 修复 Codex 中 ChatGPT Fast 选项消失：`features.fast_mode` 作为功能可见性开关始终启用，是否实际使用 Fast 仅由 `service_tier="fast"` 控制；绑定检查会自动修复旧的隐藏开关。
- 修复 Fast 默认与登录回归：ChatGPT/Codex provider 恢复 `requires_openai_auth=true`，并对支持 Fast 的默认模型自动写入 `service_tier="fast"`。

### 调整

- 后台自检与用量刷新间隔从 10 分钟改为 3 分钟。

升级后请完全重启 Codex-Router 和 Codex。

## 1.7.4 - 2026-08-16

### 修复

- 普通生成不再因历史条数自动本地压缩，避免 Gemini 中途出现“上下文已完成本地压缩与恢复”后停住；仅 Codex 明确调用 `/compact` 时才做摘要压缩。
- 启动和自检时重新拉起 Responses 兼容网关（18082），避免热替换后网关消失。
- 目录截断上限再提高；catalog 增加 `display_order` 与 `priority` 同步为列表序号。
- 延续 1.7.3：中文默认、DeepSeek 白名单绕过、会话隔离、Kimi 文本函数转写、授权误报拆分。

升级后请完全重启 Codex-Router 和 Codex。

## 1.7.3 - 2026-08-15

### 界面

- 实时用量页提高信息密度，并把同一 coding plan（例如多个 Grok 账号）合并到一张卡片内分区展示。
- 路由配置区块右下方新增渠道用量：套餐显示最小周期进度条，按量渠道显示输入/输出 Token 与缓存命中率。
- 路由列表顺序与写入 Codex 的目录顺序对齐；拖动后立即同步 `model-catalog.json`。

### 修复

- 修复额度页把本地管理会话失效或 403 套餐策略误报成“请前往授权页重新登录”。
- 第三方模型本地压缩改为摘要保留，并提高触发阈值；不再仅因指令里出现 summarize 就压缩。
- Grok 4.6 使用 50 万上下文窗口；目录截断上限从 1 万 token 提高到按窗口计算，缓解 Gemini 中途截断。
- 网关转译 Kimi 等模型泄漏到正文的 `functions__exec` 为标准 `exec_command` 工具调用。
- ChatGPT 登录下可调用 `deepseek-v4-flash` 等第三方模型，不再被官方白名单拦截。
- 近端/远端同步时保留 Codex `[desktop]` 排版键，避免旧远端布局覆盖近端修改。
- Agent 默认稳定使用简体中文，并禁止把 CodexRouter 目录误读进其他会话。

升级后请完全重启 Codex-Router，并完全重启 Codex。

## 1.7.2 - 2026-08-15

### 修复

- 修复第三方模型（如 Grok）自动压缩上下文时一直失败、界面一直显示“正在自动压缩上下文”的问题。Router 现在会在请求到达 Sub2API 之前清掉这些模型无法解析的加密推理、compaction 和 MCP/计算机用途条目，必要时改为本地截断历史。
- 修复子 Agent 使用 ChatGPT 以外模型，或主对话在第三方模型与 ChatGPT 之间切换后，出现 `Encrypted function output content could not be decrypted or decoded` 并断开流的问题。明文被误标成加密内容时会改回普通 tool output；真正的 OpenAI 加密块仍原样保留。
- 修复上述失败被 Sub2API 包装成 `502 Bad Gateway: Upstream request failed` 后，主对话卡住、新对话也全部 502 的问题。协议不兼容错误不再以 502 回传，避免 Codex 把整个本机 Router 当成上游宕机并无限重试。
- 同时存在第三方模型时，ChatGPT 目录改为 multi-agent v1，避免父 Agent 继续使用加密的子 Agent 回传协议。
- Grok / Antigravity OAuth 账号默认关闭 `openai_compact_supported`，不再把官方 compact 协议转发给这些上游。

升级后请完全重启 Codex-Router，并完全重启 Codex，让本机网关和模型目录生效。

## 1.7.1 - 2026-08-15

### 安装与发布

- Windows 安装器改为安装向导：先选择安装位置，默认创建桌面快捷方式，再确认安装，不再一点就装完。
- 公开项目名称和官方仓库地址统一为 `CodexRouter`。
- 额外提供 macOS / Linux 理论构建包。这些包尚未在真实 macOS 或 Linux 机器上测试，欢迎更多用户参与共同构建。
- 修复 Unix 理论构建时 `build.rs` 仍引用 Windows-only `windres` 导致无法编译的问题。
- macOS Intel 理论构建改为在 Apple Silicon runner 上交叉编译，避免 macos-13 队列长时间卡住。

## 1.7.0 - 2026-08-14

### 修复

- Codex 模型目录现在始终使用 Router 配置中非空的模型别名作为 \`display_name\`，内部 \`slug\` 和实际路由 Model ID 保持不变；修复 \`grok-4.6\` 等模型在 Codex UI 中回退显示为原始 ID 的问题。
- Router 生成的 Codex provider 将请求重试与 Streaming 重连默认值统一从 2 调整为 5，并由同一常量写入，避免配置链路中的旧默认覆盖。

### 验证与性能

- 新增可重复的真实 TTFT 分段验收，分别记录客户端响应头、首 SSE、首语义事件、首文本与 Sub2API \`first_token_ms\`，用于区分上游等待和本地 Streaming 开销。
- 发布构建、安装器元数据和安装测试统一从当前构建清单或 Cargo 版本读取版本号，避免散落的历史版本常量进入新产物。

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
