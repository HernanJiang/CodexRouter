# CodexRouter v2.1.3

## Changes

- Startup / self-check restores a lost Codex Router binding automatically when both the user-layer and system-layer configs are unbound. The overwrite dialog only appears if repair fails. UserData is never overwritten.
- If the user left max output at the model default, the gateway sizes `max_output_tokens` from remaining compact budget instead of Codex's fixed 5% of the full window. User-set limits still win. Gemini is capped at 65536.
- Codex `request_max_retries` / `stream_max_retries` still follow the subscription slider (default 3, 5s x5).

## Package

- `Codex-Router-Portable-2.1.3-windows-x64.zip`
- `Codex-Router-Setup-2.1.3.exe`
