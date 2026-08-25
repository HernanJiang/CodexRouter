# CodexRouter v3.0.3

## Why this release

Codex Desktop ends an agent turn as soon as the model returns assistant text with no `function_call`. Grok often writes a short plan ("我先对照…", "接下来…", "我改用看图工具") after tool results and stops there. The HTTP status is still 200; the thread looks truncated.

## Fix

The local Responses gateway now holds `response.completed` when a third-party model ends an unfinished agent turn that way, and issues at most two follow-up POSTs with a `【自动续跑】` nudge. If the follow-up emits a `function_call`, those items are spliced into the same Codex turn. Finished answers ("任务已完成") and ordinary chat without tools are not continued.

Login identity is unchanged from 3.0.2: `name = Codex-Router`, `requires_openai_auth = false`.

## Diagnostics

- `CR-STR-0010` `request.incomplete_continue` in the gateway log.
