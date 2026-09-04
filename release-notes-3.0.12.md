# CodexRouter v3.0.12

Windows x64 portable only. See CHANGELOG.md.

## Why this release

Codex Desktop showed ~122k context for Antigravity Claude Opus 4.6 Thinking. That was the Router unknown-model fallback of 128k times the 95% compact point (121600). Official Anthropic, Vertex AI, and Antigravity input context for Opus/Sonnet 4.6 is 1M tokens. 128k is the output cap, not the window. Claude 5 was already mapped to 1M; 4.6/4.7/4.8 were missed.

## Fix

- Claude Opus/Sonnet 4.6–4.8 (including `-thinking`) and Claude 5 / Fable 5 default to a 1,000,000 context window (950,000 at 95% compact).
- Remaining Claude models (for example 4.5) default to 200,000.
