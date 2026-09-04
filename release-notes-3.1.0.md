# CodexRouter v3.1.0

关闭窗口会停掉旧便携版留下的 Host 和端口。Muse 超长 MCP 工具名不再 400。

## 修复

- 退出时清掉同名 `codex-router-host.exe` / `cli-proxy-api.exe` 占用的 18080 / 28080 段端口。
- 启动时接管旧便携版 Host，不再双开。
- Muse / Meta 按命名空间拼接缩短超过 64 字符的工具名。
