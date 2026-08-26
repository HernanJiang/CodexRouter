# CodexRouter v3.0.8

Windows x64 portable only. See CHANGELOG.md.

## Why this release

Grok 4.6 would finish a Debugger Verdict, then print 「任务已完成」 and Codex would treat that as the turn summary. 3.0.3 auto-continue mistook 「下一步：Coder 做 T02」 in a long report for a premature stop, and the nudge taught Grok to reply 「任务已完成」 if the work was done.

Codex Desktop also hid reasoning pickers for Antigravity Opus 4.6 Thinking (catalog only advertised `medium`). Grok 4.6 was missing official `xhigh`.

## Fix

- Do not auto-continue a substantial answer (over ~200 characters) or a write-up that already looks finished (`Verdict`, 「任务已完成」, …). Short 「我先对照…」 / 「接下来…」 stops still continue.
- The nudge now says not to reply 「任务已完成」; it no longer teaches that phrase as a stop token.
- Reasoning menus: Grok 4.6 `low` / `medium` / `high` / `xhigh`; Claude 4.6 Thinking `low` / `medium` / `high` / `max`; Claude 5 / 4.7+ keep `xhigh`; GLM-5.2 `high` / `max`.
