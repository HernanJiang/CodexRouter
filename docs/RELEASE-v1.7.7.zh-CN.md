# CodexRouter v1.7.7

## 重点改动

- 模型降级兜底全局通用：`chatgpt-*` 品牌命名统一归一到 `gpt-*` 模型族，OAuth 与同名 API 渠道的配对、手动备用选择、额度耗尽后的自动隔离接管对 Sol/Luna 等所有接入模型生效。
- Grok、Gemini 长会话不再被中途擅自中断：网关以 30 秒轮询上游流并注入 SSE 保活注释维持 Codex 会话，完全静默上限放宽到 30 分钟；损坏的上游分块帧以显式终止事件收尾，不再静默掉线。
- 全新设备首次引导更稳定：OAuth 准备的生命周期锁等待放宽到 120 秒，预热重试扩为 4 次阶梯退避；Sub2API 就绪检测低成本探针优先、总预算 180 秒，健康检查与 TCP 属主表竞态不再直接判失败。
- 火山方舟 CodingPlan 等上游 429 限流改为阶梯退避自动重试（2/4/8/16/30 秒，封顶 60 秒），额度耗尽型 503 同等对待；最大重试次数可配置（默认 8），耗尽后才正常抛出限流提示。
- 自检新增配置文件覆写检测：Codex 原生配置被外部程序覆写时弹出三选一窗口——写入 CodexRouter 标准配置 / 保留当前配置（按指纹记忆，不再重复提示）/ 恢复 Codex 官方出厂默认配置，取代原先的静默自动修复。
- 应用配置与修复绑定时自动向 Codex 系统层配置 `%ProgramData%\OpenAI\Codex\config.toml` 写入 Router 绑定：即使 Codex Desktop 周期性重写用户 `config.toml` 并丢弃 Router 键（此前非 ChatGPT 模型会被直接发往 ChatGPT 后端并报 “model is not supported”），新会话仍自动走本地路由，Grok/Kimi/Gemini 等全部已注册模型不受影响；恢复出厂或关闭路由时自动移除。

## 发布文件

- `Codex-Router-Installer-1.7.7-windows-x64.exe`
- `Codex-Router-Portable-1.7.7-windows-x64.zip`

升级后请完全重启 CodexRouter 和 Codex。
