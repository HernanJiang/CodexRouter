# CodexRouter v1.7.3

用量页与路由区展示优化，并修复授权误报、多模型中断、DeepSeek 白名单、同步覆盖和中文输出不稳定。

## 用户可见变化

- 同一套餐的多个账号合并到一张用量卡片；页面更紧凑。
- 路由配置右下方直接看到套餐进度或按量 Token / 缓存命中。
- Codex 模型列表顺序与 Router 自上而下顺序一致。
- Grok / Gemini / Kimi 长对话不再轻易因本地压缩或文本函数标记中断。
- ChatGPT 账号可以选用 deepseek-v4-flash 等第三方模型。
- 默认稳定中文回复，会话之间不再串读 CodexRouter 目录。

升级后请完全重启 Codex-Router，并完全重启 Codex。

## 安装包

- `Codex-Router-Installer-1.7.3-windows-x64.exe`：Windows 安装版
- `Codex-Router-Portable-1.7.3-windows-x64.zip`：Windows 便携版
