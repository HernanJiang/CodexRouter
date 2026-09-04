# CodexRouter v3.0.18

Codex 额度用完后自动切到已配置的第三方 API。CLI 热推失败不再把全部号池停掉。

## 修复

- Host 在 CLI 管理口配置热推失败时，继续按凭证发布可用号池，避免 `503 no schedulable credential in pool`。
- ChatGPT 5 小时窗口用尽时暂时离开号池，同名第三方 API 立即接手；窗口恢复后自动回归。
- ChatGPT 恢复只改本地调度状态，不经过 CLIProxyAPI 刷新 token。
