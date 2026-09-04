# CodexRouter v3.0.15

Windows x64 portable only. See CHANGELOG.md.

## Why this release

ChatGPT moved from a plain weekly cap to a 5-hour rolling window plus a weekly cap. The Router's quota check treated "any window at 100%" as exhausted, so a full 5-hour window (while the weekly quota still had headroom) parked the account and kept routing on API. After the 5-hour window reset the account did not come back to the pool.

## Fix

- Quota judgement now separates short and long windows: when a weekly / seven-day window exists, only a full weekly window counts as a genuinely drained subscription. A full 5-hour window is a short-lived rate limit; the account stays in the pool and requests rotate across accounts instead of falling back to API. Platforms without a long window keep the previous any-window behaviour.
- New popup when an account recovers: "quota recovered, account rejoined the pool" (once per recovery cycle).
- The existing "left the pool -> API fallback" popup keeps working and now only fires on a genuine weekly-cap drain.
