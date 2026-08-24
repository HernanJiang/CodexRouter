# CodexRouter v3.0.2

## Why this release exists

Codex Desktop 26.818 refreshes ChatGPT OAuth when the active provider has `requires_openai_auth = true`. The account heartbeat (`getAuthStatus`) and the online model list (`list_models`) can call `Refreshing token` at the same time. OpenAI rotates refresh tokens strictly: using the same ticket twice at once invalidates the whole family (`HTTP 401 refresh_token_invalidated`). Desktop then opens `account/login/start`.

That is a Desktop client behavior. Router forwarding still works. The ChatGPT session file (`auth.json`) is not deleted.

Older Router builds wrote `name = "OpenAI"` and `requires_openai_auth = true` so the Desktop corner showed a ChatGPT account while traffic still used the local Router bearer. On 26.818 that combination triggers the concurrent refresh. Writing `name = "OpenAI"` with `requires_openai_auth = false` makes Desktop show a signed-out OpenAI account.

3.0.2 therefore keeps one legal identity for the local provider:

- `name = "Codex-Router"`
- `requires_openai_auth = false`
- requests still use `experimental_bearer_token` on the local gateway

The Codex-Router label is expected. ChatGPT tokens stay in Desktop's own `auth.json`. ChatGPT quota in Router is observational; third-party API fallback remains.

3.0.1 only rewrote the user-layer `~/.codex/config.toml`. The machine-wide `%ProgramData%\OpenAI\Codex\config.toml` could still require OpenAI auth. When Desktop strips the user provider (`model = "first"`), routing falls back to that system file and the login loop returns. 3.0.2 repairs both layers, recreates a missing system file from a legal user file, and no longer deletes the system binding on Router exit (only "restore official Codex" removes it).

## Changes

- Keep Desktop from refreshing ChatGPT tokens for the local Router provider.
- Repair illegal or missing user/system provider identity on Host start; skip identical writes afterwards.
- Do not delete the system-layer Router binding when exiting Router.
- Shield upstream 401 as 503 so Desktop does not treat it as a logout.
- Emit `CR-DSK-0001` … `CR-DSK-0015` in `router-events.jsonl` (config writes, replica writes, heartbeats with mtimes, identity repair). No tokens or account identifiers are logged.

## Assets

- `Codex-Router-Portable-3.0.2-windows-x64.zip`
