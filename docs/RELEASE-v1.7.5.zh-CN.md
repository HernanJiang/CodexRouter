# CodexRouter v1.7.5

## 重点改动

- 后台自检与用量刷新改为每 3 分钟一次。
- ChatGPT 保留手动子 Agent 能力，不再默认绑定 Luna。
- 修复托盘和后台唤起后的极小空白窗口、字体缺失和轻量布局误触发。
- ChatGPT Responses 请求与 SSE 流保持原样转发，默认请求和流重试统一为 3 次。
- DeepSeek、Kimi 等官方接口不再使用自动代理；DeepSeek 上游限流不再被本地 503 掩盖。
- Kimi、DeepSeek 的身份提示、`exec_command` 工具兼容和报告命名继续生效。
- Grok 多账号在首账号额度耗尽（402/429）后立即切换；跨账号继续当前会话时自动规范化不兼容历史项，避免健康账号返回 422/本地 502。
- Gemini 等第三方模型长任务遇到 429 或上游 SSE 异常断开时会继续重试，不再在回答或工具执行中途静默结束。
- 火山方舟 Coding Plan 与 Agent Plan 现在共享同一组控制面 AK/SK 和实时额度池；修复 Agent Plan 表单保存无效及额度请求载荷签名不一致。
- ChatGPT OAuth 额度耗尽时，未选中的历史 OAuth 记录不再绕过 Router 组继续抢占请求，匹配的第三方按量 API 可立即接管。
- 用量面板改用真实业务 provider 分组：Kimi、Volcengine 与普通 API 中转站分开显示；同一中转域名下的不同 API 路径不再错误合并。
- 侧边对话恢复 Agent 工具能力：ChatGPT 使用原生 v2，DeepSeek、Kimi、Gemini、Grok、Claude、火山等第三方模型使用兼容 v1 协议；用户自定义 `[agents]` 不会再被 Router Apply 删除。
- 本地 Responses 网关不再把浏览器 `/v1` 导航透传到 Sub2API 登录页，避免每轮对话结束弹出 `18082/login?redirect=/v1`；管理后台继续使用 18080。
- ChatGPT 模型的 Fast 选项重新显示；功能是否可见与当前是否启用 Fast 分离，不会因普通模式写入 `fast_mode=false` 而隐藏入口。
- 恢复 ChatGPT 登录契约，并将支持 Fast 的默认模型直接设为 Fast 模式。

## 发布文件

- `Codex-Router-Installer-1.7.5-windows-x64.exe`
- `Codex-Router-Portable-1.7.5-windows-x64.zip`

升级后请完全重启 CodexRouter 和 Codex。
