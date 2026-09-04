# CodexRouter v3.0.16

Windows x64 portable only. See CHANGELOG.md.

## Why this release

OpenAI (ChatGPT) accounts always showed "The provider did not expose readable 5-hour, weekly, or monthly quota windows". Root cause: the Host's usage query for Desktop-owned OpenAI accounts returned the cache directly and never called `chatgpt.com/backend-api/wham/usage` (a legacy `desktop_openai_auth_owner` shortcut), and that cache never refreshed, so the dashboard had no windows.

Live verification confirmed the wham endpoint itself works: a real Plus account returns 200 with `rate_limit.primary_window` (5-hour, 48%) and `rate_limit.secondary_window` (weekly, 23%).

## Fix

- OpenAI OAuth quota now performs a live read-only wham probe using Desktop's current access token (never the CLIProxyAPI `$TOKEN$` refresh path, so the token family is not revoked). Success writes the cache and returns `five_hour` / `seven_day` / `monthly` windows; failure falls back to cache with an `error_code` (e.g. `auth_unavailable`) and does not arm a re-auth cooldown.
- Scheduling stays observational: account health / pool rotation still belongs to the existing isolation-recovery path. This release only restores readable quota windows.
- Follows token-monitor's query-layer / adapter-layer pattern: every provider (grok, antigravity, kimi, openai) now has its own live query, parser and cache fallback. OpenAI was the missing one.

## Verification

- Real wham call: `200 OK`, primary_window (18000s -> fiveHour) = 48%, secondary_window (604800s -> weekly) = 23%.
- End-to-end test with a mocked wham server resolves `five_hour.used_percent=48`, `seven_day.used_percent=23`, `source=upstream`.
- Full test suite green: lib 151 / GUI 463 / host 7.
