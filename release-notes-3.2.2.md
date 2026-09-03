# CodexRouter 3.2.2

Gemini 3.8 Flash 已进入 Antigravity 可选模型。保存并应用时，缺一把 API 钥匙不再把整份配置回滚。

## 更新

- Antigravity 订阅若声明 Gemini 3.8 Flash 的 High / Medium / Low 档，界面只显示一个「Gemini 3.8 Flash」，默认走 Medium。
- Google Gemini 兼容 API 渠道预设改为 `gemini-3.8-flash`。思考档为 low / medium / high，默认 medium。

## 修复

- 某个 API 渠道的 Windows 凭据名对不上时，会按邻近编号找回已保存的 Key；仍然找不到则跳过该渠道，其余模型和 Codex 绑定继续写入。
