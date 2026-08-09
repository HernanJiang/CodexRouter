# Codex-Router v1.2.18 验证记录

验证日期：2026-08-05

## 变更

- composite route 允许同一公开模型按平台保存多个候选 route。
- 候选按匹配强度、endpoint、前缀长度、priority 和 ID 稳定排序。
- OpenAI-compatible handler 支持 `grok` 与 `openai` 平台间的选择前回退。
- 跨平台回退仅发生在尚未选择任何账号且返回 `ErrNoAvailableAccounts` 时。
- 账号已选中、上游已接收请求或下游已输出后，绝不切换平台重放。
- Router 同步按“公开模型 + 目标平台”维护 composite routes；OAuth 优先级为 1，API fallback 优先级为 100。
- API fallback 与 OAuth 配对后共享 OAuth 公开模型 ID，同时保留账号自身 upstream model mapping。
- composite route 唯一索引加入 `target_platform`。

## 真实验证

- Grok OAuth account 4 正常路径完成两轮 Responses 工具调用及 continuation。
- 将 account 4 临时设为不可调度后，`grok-4.5` 自动选择 OpenRouter account 5。
- 两轮 fallback 请求均使用 `/v1/responses`，完成 SSE 和工具结果往返。
- fallback usage 保持公开模型 `grok-4.5`，OpenRouter 账号在转发边界映射到 `x-ai/grok-4.5`。
- account 4 在测试 `finally` 中恢复为 active、schedulable。
- 数据库同时保存 `grok-4.5 -> grok` priority 1 和 `grok-4.5 -> openai` priority 100。

## 自动验证

- Go 全部 package 编译通过：`go test ./... -run '^$'`。
- clean commit + cumulative patch 的针对性 Go 测试连续三轮通过。
- Rust：135 passed，1 ignored。
- Clippy：`--all-targets --locked -- -D warnings` 通过。
- PowerShell：OAuth routing、model catalog、Codex integration 通过。

## 源码可重现性

- Sub2API 基线：`99c8e4bf7564823bafbab369acab6539e734c1bb`。
- 累计 patch：`licenses/sub2api-0.1.168-codex-router.3.patch`。
- patch 已在独立干净 checkout 上通过 `git apply --check`、实际 apply 和全 package 编译。
- 旧 `.2` patch 无法应用到其声明的 v0.1.170 基线，不再打包或作为 provenance 依据。

## 最终发布包

- Stage：`D:\Work\Tools\Codex-Router-Releases\Codex-Router-Portable-1.2.18-windows-x64-20260805-175354-038`
- ZIP：`D:\Work\Tools\Codex-Router-Releases\Codex-Router-Portable-1.2.18-windows-x64-20260805-175354-038.zip`
- ZIP SHA-256：`571A062FBDFC6E723C1BA4866AFAEB26E644B9D138683C7C5E679ED9E2260B8F`
- 文件数：1130。
- Sub2API SHA-256：`F5D8B0A8E54CCB408ACF77CE0F0A937C9AA7BD9E2A2E90D56D0FDA4FF61B4AD7`。
- 独立 Stage 验证、全树秘密扫描、PostgreSQL payload 和 VC Runtime 检查通过。
