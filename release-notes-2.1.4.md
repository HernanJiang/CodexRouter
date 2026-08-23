# CodexRouter v2.1.4

## Changes

- Every manual or scheduled self-check refreshes the live quota of each selected OAuth subscription account. Recovered accounts are returned to the matching routing pool immediately; stale Grok billing cache is display-only.
- Self-check repairs lost user-layer and system-layer Codex Router bindings, keeps the current gateway port and retry settings, and respects the sticky official escape mode.
- The overwrite dialog restores the binding after three cumulative foreground-focused seconds, pauses in tray/minimized/unfocused mode, and restarts Codex after the repair completes.
- Cancelled Codex requests stop upstream reconnects immediately. Each turn shares one retry budget with a 180-second cumulative backoff cap, and interrupted SSE requests always receive a terminal failure event so the conversation can be released.
- Default output sizing uses the remaining compact budget and model limits when the user has not configured a fixed maximum. The product compaction default remains 95%.
- The default release target is the Windows x64 portable package. The per-user installer is included in this release as an explicit additional asset.

## Assets

- `Codex-Router-Portable-2.1.4-windows-x64.zip`
- `Codex-Router-Setup-2.1.4.exe`
