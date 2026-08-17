# CodexRouter v1.7.8

正式版修复：协议与额度分层、禁止订阅/Coding Plan 静默切 PAYG、429 六档阶梯重试。

## 包名

- `Codex-Router-Installer-1.7.8-windows-x64.exe`
- `Codex-Router-Portable-1.7.8-windows-x64.zip`

## 关键变化

- 额度来源与接口协议严格分离。
- 订阅 / Coding Plan 禁止自动切同厂商 PAYG。
- 火山 Coding Plan 使用 Responses；Kimi Coding Plan 继续 Chat 转换。
- 429 / 断网六档阶梯重试：2s / 10s / 30s / 1min / 3min / 5min。
