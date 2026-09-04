# CodexRouter v3.0.11

Windows x64 portable only. See CHANGELOG.md.

## Why this release

Claude Opus 4.6 Thinking at max effort returned HTTP 400: ``max_tokens` must be greater than `thinking.budget_tokens``. CLIProxy 7.2.135 maps `reasoning.effort=max` to a 128000 thinking budget. The gateway wrote `max_output_tokens` from the leftover compact budget (113874 on the live thread). High/xhigh budgets are smaller, so only max failed. Anthropic requires a strict greater-than.

## Fix

After injecting the Claude output cap, if `max_output_tokens` is not greater than that effort's thinking budget, raise it to budget + 4096 (max → 132096) so there is room for the visible answer.
