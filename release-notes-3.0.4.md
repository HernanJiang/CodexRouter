# CodexRouter v3.0.4

## Why this release

Long Grok agent turns failed with `stream disconnected before completion: Incomplete response returned, reason: max_output_tokens`. The gateway used leftover auto-compact input budget as the output cap. Once the transcript was large (multi-MB `/v1/responses` bodies), that leftover became 1 token.

## Fix

- Keep at least the compact reserve (5% of the window; 128k for Grok) for `max_output_tokens`.
- Never lower the value Codex already sent.
- Count screenshots / base64 blobs as a small image cost, not millions of tokens.
- A card-level max output still wins.

## Diagnostics

- `CR-STR-0011` `request.max_output` in the gateway log.
