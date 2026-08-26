# CodexRouter v3.0.10

Windows x64 portable only. See CHANGELOG.md.

## Why this release

Grok returned HTTP 400 `{"code":"invalid-argument"}` on both the long thread `01a038a7` and the original `01a02c3a` retry. Item types were already valid. CLIProxy flattened Codex `namespace` tools and forwarded `mcp__codex_app__automation_update` with a root `oneOf` + `$defs`/`$ref` schema that xAI rejects. The gateway also set `max_output_tokens` to the leftover compact budget (297543 on the live dump).

## Fix

- Simplify Grok tool schemas before CLIProxy: replace `automation_update` (including `mcp__codex_app__*` and flattened names) with an empty object schema; strip other root `oneOf`/`$defs`. Keep `web_search` and ordinary functions.
- Drop Grok `text.verbosity` and `reasoning.summary`; keep `reasoning.effort`.
- Hard-cap Grok `max_output_tokens` at 128000.
