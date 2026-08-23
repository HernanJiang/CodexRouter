# CodexRouter v2.0.19

## Changes

- Default auto-compact is now the official 95%. The slider still covers 60–95. Catalog `auto_compact_token_limit` equals the full `context_window`, so Codex sees Grok as 500k instead of 80% of that window.
- Codex Grok compact handoff text (“Another language model started…”) is rewritten into a continue-running instruction, so compact is not treated as a new task / handoff / empty completion.
- If an API key is valid but the typed model ID is missing from `/models`, Router shows a scrollable upstream model list. Clicking an item adds it. `gpt-5.6` still canonicalizes to `gpt-5.6-sol`.
- Scheduling uses account priority only. Dragging model cards sets account P (01→P1, 02→P2). Apply writes the same P to every slot of that OAuth account. Subscription-page P edits write back to those slots too.
- Auto-fill of Grok slots is kept: new Grok accounts still attach to existing 4.5/4.6 cards, including empty catalogs.
- This release ships the portable package only. No installer.

## Package

- `Codex-Router-Portable-2.0.19-windows-x64.zip`
- ZIP SHA256 `577e1fce88a49ffa45f3bfcd024ac25491f1c52e06fd26727c1c6a425a37abba`
- EXE SHA256 `7e9267f369df129217c2d3fa22668a29822badc7ec55ebe70bb4ab10333edb03`
- Host SHA256 `bd79a933e1cc89d9b3650179f81f7ff4dc92191896ad6772795eff2598717021`

Existing model cards already saved at 80% are not rewritten automatically. Grok official `/responses/compact` is still not enabled.
