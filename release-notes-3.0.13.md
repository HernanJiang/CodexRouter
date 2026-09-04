# CodexRouter v3.0.13

Windows x64 portable only. See CHANGELOG.md.

## Why this release

Gemini / Antigravity quota exhaustion returned `429 Resource has been exhausted (e.g. check quota.)`. Multiple OAuth accounts shared one CLIProxy prefix (`cr_r13_antigravity`), so Google cooldown applied to every credential. The Host had no next pool. The gateway retried that 429 as a normal rate limit (5s/25s/125s) until Codex reported `exceeded retry limit`.

## Fix

- Each OAuth account gets its own prefix (`cr_r13a52_antigravity`, …) and P1/P2/P3 pool. Quota exhaustion fails over immediately.
- The gateway does not retry quota / cooldown 429s on the same account.
