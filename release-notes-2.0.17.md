# CodexRouter v2.0.17

## Fixes

- Grok answers no longer get cut off when documenting reasoning tags inside Markdown inline/fenced code. Real provider think blocks and truncated streams are still stripped.
- OAuth ChatGPT through Router no longer copies official `code_mode_only` / `use_responses_lite` / v2 collaboration. Commands stay on JSON `exec_command`. Sub-agents use v1 so Desktop is not hit with empty `spawn_agent {}` (`missing field message`). Web search is kept.

## Packages

- `Codex-Router-Portable-2.0.17-windows-x64.zip`
- `Codex-Router-Setup-2.0.17.exe`

SHA256 values are in the release assets after upload.
