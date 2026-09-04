# CodexRouter v3.0.14

Windows x64 portable only. See CHANGELOG.md.

## Why this release

3.0.13 split every OAuth account into its own CLIProxy prefix. Old conversations whose continuation is bound to a dead account then received `503 auth_unavailable: no auth available (providers=xai, model=cr_r10a56_xai/grok-4.6)`. The Host pool-switch check only recognized quota / rate-limit / drained-pool errors, not `auth_unavailable`, so it forwarded the 503 instead of rotating to the next account. New conversations pick a live account and work; old ones stay pinned to the dead pool.

Cross-thread agent history (a forked sub-agent carrying the parent's function responses) also produced orphaned `functionResponse` parts when routing moved to an Antigravity / Gemini pool, and Gemini rejected the request with `400 invalid Gemini function call history`.

## Fix

- Pool failover now also triggers on `auth_unavailable` (no auth available) as long as another usable account pool exists, so old conversations rotate automatically. When every pool is unavailable the error is kept and a clear `request.pool_unavailable` event is emitted.
- A desktop notification now pops when the account pool switches (one pop per pool), and a warning dialog appears when every account is unavailable.
- Gemini / Antigravity requests prune orphaned `functionResponse` parts that have no matching `functionCall` in the same input (cross-thread history), keeping the payload as plain text so the conversation can continue.
