# CodexRouter v3.0.9

Windows x64 portable only. See CHANGELOG.md.

## Why this release

Grok returned HTTP 402 `Payment Required` when SuperGrok quota ran out. Router only failed over on 429/503, so Desktop saw a payment error. Grok login also could not finish: CLIProxy's `xai-auth-url` did not use the registered loopback `http://127.0.0.1:56121/callback`, so the UI sat in pending and never stored xAI tokens.

## Fix

- Treat 402 as quota exhaustion and switch to the next Grok/relay pool immediately. If no pool remains, return 429 `usage_limit` instead of Payment Required.
- Host owns Grok PKCE login: authorize URL pins `127.0.0.1:56121/callback`, Host exchanges the code on `auth.x.ai`, and writes `xai-{email}.json`.
