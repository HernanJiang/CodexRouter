# CodexRouter v3.0.22

Muse / Meta 的 64 字符工具名限制覆盖 MCP 命名空间展开。3.0.21 只截了顶层名字，CLIProxy 拼 `namespace__child` 之后仍然超长。

## 修复

- 按 CLIProxy 规则缩短 namespace 子工具名，回包还原。
