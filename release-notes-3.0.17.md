# CodexRouter v3.0.17

Windows x64 portable only. See CHANGELOG.md.

## Why this release

After resetting quota in the official consoles (chatgpt.com / grok / google), the Router quota monitor briefly showed a red full-page banner: "The usage query temporarily failed. The last successful data is retained; retry shortly." The same banner appeared for every platform. A transient query failure right after a reset was replacing the live, still-valid snapshot with a scary red error.

## Fix

- When a failed usage refresh happens while a successful snapshot is already on screen, the app no longer shows a red "query failed" banner. It renders a soft yellow notice "Usage refresh failed (last good data kept)" and keeps displaying the last good data; the background refresh retries automatically.
- A red failure banner is only shown when there is no previous data at all (first query fails), with the specific cause.
- Added `Palette.warning` and a testable `usage_error_for_ui` helper (snapshot present -> `RETRY-KEEP:` prefix, rendered yellow).

## Verification

- Full test suite green: lib 151 / GUI 464 / host 7, including the new `usage_error_degrades_to_retry_keep_when_a_snapshot_is_on_screen` case.
