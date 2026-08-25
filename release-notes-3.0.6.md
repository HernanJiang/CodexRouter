# CodexRouter v3.0.6

## Why this release

Desktop showed `stream disconnected before completion: error sending request for url (http://127.0.0.1:28085/v1/responses)` on Gemini and then on every model. The gateway treated Antigravity `503 auth_unavailable` as a rate limit and slept 5s/25s/125s before sending any HTTP headers. Codex timed out, retried, and flooded 28085 with TIME_WAIT connections.

## Fix

- Missing provider auth is not retried as 429.
- Silent pre-content retry wait is capped at 8 seconds.
- Incomplete-turn auto-continue applies only to Grok.
