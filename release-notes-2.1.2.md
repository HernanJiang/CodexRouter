# CodexRouter v2.1.2

## Changes

- Subscriptions with remaining quota re-enter the account pool at the card P value. OAuth copies write CLI `priority`/`weight`. Hidden tiers such as 2100 fold back to 1–999, so a live ChatGPT/Grok subscription is no longer skipped for a lower-priority API.
- Multiple subscriptions of the same model schedule as P1 / P2 / P3 instead of equal-weight rotation. Three Grok accounts drain the higher-priority account first.
- ChatGPT OAuth only attaches to mapped GPT routes. It no longer joins Kimi / GLM / DeepSeek OpenAI pools.
- Coding Plan keys that share a Base URL each get their own 5-hour / weekly quota bar. Identical keys still merge.

## Package

- `Codex-Router-Portable-2.1.2-windows-x64.zip`
- `Codex-Router-Setup-2.1.2.exe`
- ZIP SHA256 `da5eded8768e02a3ea82c1ca54f1e0dda6a77527bdf140ce5272fd98e6a24476`
- EXE SHA256 `2ab1d433ddaad467d90d299931cf67a8f7cf881100a528177f81043b5ec2ed01`
- Host SHA256 `be3cb8c973ffe417422e317058301e2fdff4caf11adb64cb2906ee30c089560c`
- Installer SHA256 cb802a3497b079d4eb2682b747d2a638e1787762856d58df6ec53b21f73650b6

Save and Apply so CLI recompiles the account pool. Existing running instances are not overwritten by this GitHub upload.
