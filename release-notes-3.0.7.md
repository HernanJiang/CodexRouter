# CodexRouter v3.0.7

Windows x64 portable only. This is the first GitHub package after v3.0.2; it also includes 3.0.3–3.0.6. See CHANGELOG.md.

## Why this release

The browser already showed Antigravity authorization success, but Router returned `CR-OAU-0008` on `/api/v1/admin/antigravity/oauth/exchange-code`. CLIProxyAPI 7.2.135 requires `www.googleapis.com/oauth2/v2/userinfo` after the token exchange; a TLS handshake timeout discarded the tokens and never wrote an auth file.

ChatGPT official quota exhaustion also returned 429 `model_cooldown` for `gpt-5.6-sol` without falling back to the configured relay. Official OAuth and the relay shared one CLIProxy prefix, so the weekly cooldown parked both.

## Fix

- Router owns the Google-registered Antigravity callback on port `51121`.
- Host exchanges the code on `oauth2.googleapis.com` and keeps the account if userinfo is down.
- Email comes from id_token or tokeninfo first. Missing project ID does not fail login.
- Official ChatGPT and the same-name relay are separate priority pools. Official is first; a 429 / quota / cooldown fails over to the relay instead of cooling both.
