# CodexRouter v3.1.3

本版本修复启动时重复弹出额度切换提示的问题，并修正第三方模型 Responses 流中思考内容的兼容处理。

## 修复

- 启动后的第一份用量快照只作为基线，不再把已经耗尽的订阅账号当作刚刚耗尽并弹窗。
- 启动回放中的 pool failover / pool unavailable 日志不再重复触发账号切换或额度提示。
- 只有运行过程中检测到额度从可用变为耗尽时才提示一次；恢复后可再次提示重新加入。
- Spark 等以 `<think>` / `<thinking>` 标签输出思考的模型，跨 SSE 分块转写为 Codex reasoning 事件。
- ChatGPT / Grok 原生 reasoning 流保持原样，避免把普通 `<` 或加密 reasoning 内容误识别为标签。
- GLM-5.3 / GLM-5.3-Flash 的 reasoning 档位、输出上限和目录元数据按官方接口约束适配。
- Router 凭据按便携包 UserData 根目录隔离，兼容读取旧的 Windows Credential Manager 条目。

## 发布物

- Windows x64 便携版：`Codex-Router-Portable-3.1.3-windows-x64.zip`
- 支持 Windows 10/11 x64；便携包不包含用户配置、日志、数据库、OAuth 状态或 API Key。
