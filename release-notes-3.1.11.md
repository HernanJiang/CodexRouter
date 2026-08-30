# CodexRouter v3.1.11

第三方模型切换、思考链展示和 Codex App 线程工具现在可以正常用了。

## 3.1.11

- Muse Spark 不再因 Gmail 等 MCP 工具的递归 `$ref` schema 报 `Recursive JSON schemas are not currently supported`。
- Grok 切换后的笼统 `invalid-argument` 同源：同一套递归 schema 会断环后再送给上游。
- Muse 会改成当前模型身份，不再沿用上一轮的「你是 Grok」。

## 自 3.1.4 以来的相关修复

- 切换到 Grok 时不再把 Codex 线程 ID 当成 Grok 会话 ID（`prompt_cache_key` / `client_metadata` / `X-Request-Id`）。
- Muse 重复 `function_call_output`（`call_compat_1`）会配对去重。
- Gemini 去掉 protobuf 不认识的 `userAgent` / `requestType` / `requestId`。
- Gemini / Muse 的思考会实时转成 Codex reasoning summary，工具调用紧随其后。
- Gemini 3.7 Flash 可以创建和发送 Codex App 子线程；`projectId` 会按当前工作目录修正，成功回执原样保留。
- ChatGPT 官方思考不再重复输出，compact 后不再重放旧历史。
- 额度恢复通知不再反复弹窗。
- 关闭窗口后的 Host / 端口约定：打包后不擅自停掉正在运行的实例。
