# CodexRouter v3.0.5

## Why this release

Continuing an Antigravity / Gemini Codex thread replayed CLIProxyAPI `cpa-gemini-responses-carrier-v1` thought blobs and Codex `previous_response_id` values that Google no longer has. Upstream returned HTTP 404 `Requested entity was not found.` Thread `01a02cf4` had 323 carriers in one POST.

## Fix

- Strip Gemini server continuation handles and thought carriers on the way in.
- Retry 404 only when the body is a missing Google entity, not when the Router has no model route.
