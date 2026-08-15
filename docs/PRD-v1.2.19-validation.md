# Codex-Router v1.2.19 验证记录

验证日期：2026-08-05

## 模型身份

- 自动合并采用双门槛：系统推导的展示候选一致，且厂商感知的真实模型身份一致。
- 自定义展示名称只用于显示，不作为路由证据。
- 支持 OpenRouter 的 `openai/`、`anthropic/`、`google/`、`x-ai/`、`deepseek/` 和已声明 Moonshot/Kimi 命名空间。
- 支持明确的分隔符别名，例如 `gemini-3-6-flash` 与 `gemini-3.6-flash`、`claude-opus-4-6` 与 `claude-opus-4.6`。
- Fast、High/Low、Highspeed、上下文长度、日期和 Preview 等变体不做模糊合并。
- 未知厂商命名空间相互隔离，不能仅凭叶子 ID 相同合并。

## 渠道顺序

- OAuth 订阅为 tier 0。
- Coding Plan 为 tier 1。
- 第三方 API/中转平台为 tier 2。
- 层级内继续保留用户配置 priority。
- Coding Plan 可由受信任 endpoint 识别，或由 `extra.codex_router_channel_kind=coding_plan` 显式声明。

## GUI

- 模型编辑页在展示名称下显示模型身份判断说明。
- Dashboard 渠道卡片显示同一模型路由或独立路由及身份依据。
- Fallback picker 只显示展示候选和真实 ID 均一致的 API 条目。
- OAuth 首次 429 自动切换时显示一次弹窗，包含订阅显示名和实际下一渠道显示名。
- 通知按 account ID 去重；额度恢复后允许下一次耗尽重新通知。
- 轻量托盘模式只低频监听结构化 OAuth 429 failover，不恢复完整日志开销。
- GUI 启动时忽略历史 failover 尾部，防止过期弹窗。

## 真实验证

- ChatGPT OAuth 到 Chiral：两轮 Responses 工具 continuation 命中 account 2，完成 SSE。
- Grok OAuth 到 OpenRouter：两轮 Responses 工具 continuation 命中 account 5，完成 SSE。
- OpenRouter Gemini `google/gemini-2.5-flash` 连续三轮通过 Responses/Chat 普通请求、SSE 和工具 round-trip。
- OpenRouter Gemini 不支持 `previous_response_id`，稳定返回 400；探针降级到 stateless history continuation 后通过。
- 当前没有 Gemini 或 Antigravity OAuth 账号，因此不宣称 Gemini OAuth 自动回退已真实通过。
- 当前 backend 没有 Gemini 原生到 Responses 的安全 adapter；Gemini OAuth 到 OpenRouter Responses 保持 fail-closed，不跨 handler 重放。

## 稳定性

- 8 个并发 `gpt-5.6-sol` 本地 Responses 请求全部 200/Completed。
- 并发前后 Sub2API PID、句柄、线程保持不变，工作集无增长。
- 连续三次 Apply 均成功，PID 不变，约 8.8-11.8 秒。
- Rust 两轮全量测试：每轮 138 passed，1 ignored。
- Rust Clippy `--all-targets --locked -- -D warnings` 通过。
- Python unittest：17 passed。
- PowerShell OAuth routing、catalog、Codex integration 通过。

## 最终发布包

- Stage：`Codex-Router-Portable-1.2.19-windows-x64-20260805-184714-194`
- ZIP：`Codex-Router-Portable-1.2.19-windows-x64-20260805-184714-194.zip`
- ZIP SHA-256：`314B22CFC06389DF299421A678C045852A2EA0A34D9CF423B9FD41712B6AFD7B`
- Codex-Router.exe SHA-256：`3E8A4E4695A9FC5D437C5A091085E161E248BA7A06420683AA921E50B4E08369`
- Sub2API SHA-256：`F5D8B0A8E54CCB408ACF77CE0F0A937C9AA7BD9E2A2E90D56D0FDA4FF61B4AD7`
- 独立 Stage 验证通过，文件数 1130；全树秘密扫描、PostgreSQL payload 和 VC Runtime 检查通过。
