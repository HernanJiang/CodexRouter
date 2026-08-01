# Codex 统一模型路由最终需求汇总

## 1. 文档说明

- 文档日期：2026 年 7 月 31 日
- 项目目录：`D:\Work\Tools\Codex-Router`
- 主要使用入口：Codex 桌面端
- CC Switch 配置名称：`Own-Router`
- 原始需求来源：`C:\Users\Haona\Documents\Codex\2026-07-31\llm-light-cc-switch-https-api\outputs\Codex统一模型路由需求说明.md`

本文档合并此前提出的全部需求，并以较晚确认的要求覆盖较早的冲突项。本文档是后续修改、排错和验收的最终需求基线，不保存或展示任何 API Key、OAuth Token、密码或本地访问密钥。

## 2. 最终目标

在 Codex 桌面端中融合多个模型和多个上游，同时保持以下体验：

1. 用户在 Codex 原生模型菜单中手动选择模型。
2. 本地 Router 在后台选择账号或上游，不擅自替换用户选择的模型。
3. GPT 优先使用 ChatGPT Plus，必要时自动回退到 GPT API 中转。
4. Kimi 优先使用主 Key，必要时自动回退到备用 Key。
5. Grok 4.5 和 DeepSeek V4 Flash 通过 OpenRouter 调用。
6. 支持 Fast 的模型显示真实可用的 Fast 选项，不支持的模型不显示。
7. 切换到 `Own-Router` 后仍保持现有 ChatGPT 登录状态和远端控制能力。
8. 所有对话应能正常完成普通和流式响应，不得持续重连后失败。
9. 切换配置或重启 Codex 不得删除本地任务和聊天记录。

## 3. Codex 配置基线

Codex 用户级配置必须满足：

```toml
model_provider = "sub2api"
model = "deepseek-v4-flash"
model_catalog_json = 'D:\Work\Tools\Codex-Router\config\models.json'

[model_providers.sub2api]
name = "Codex Unified Router"
wire_api = "responses"
base_url = "http://127.0.0.1:18081/v1"
requires_openai_auth = true
supports_websockets = false

[features]
fast_mode = true
```

额外约束：

- 不设置全局 `service_tier`。
- Fast 由当前模型目录动态决定，不强制应用到所有模型。
- `sub2api` 是非保留的自定义 provider ID，不得创建 `[model_providers.openai]` 覆盖 Codex 内置 provider。
- `requires_openai_auth=true` 保留 ChatGPT 登录态和远端控制，`18081` 只把模型请求导向本地认证适配器。
- `supports_websockets=false` 让当前 7 个模型使用已验证的 HTTP Responses 流，避免无 WSv2 账号时反复重连。
- 默认模型为 `deepseek-v4-flash`。
- 模型目录发生变化后，需要完全退出并重新打开 Codex 才能可靠刷新菜单。

## 4. 最终模型范围

模型菜单只保留经过实际请求验证的 7 个模型：

| Codex 模型 ID | 显示名称 | 实际上游 | 上游模型映射 |
| --- | --- | --- | --- |
| `gpt-5.6-sol` | GPT-5.6-Sol | ChatGPT Plus，失败后 430123 | 同名模型 |
| `gpt-5.6-terra` | GPT-5.6-Terra | ChatGPT Plus，失败后 430123 | 同名模型 |
| `gpt-5.6-luna` | GPT-5.6-Luna | ChatGPT Plus，失败后 430123 | 同名模型 |
| `kimi-for-coding` | Kimi for Coding | Kimi Coding Plan | 同名模型 |
| `kimi-for-coding-highspeed` | Kimi for Coding HighSpeed | Kimi Coding Plan | 同名模型 |
| `grok-4.5` | Grok 4.5 | OpenRouter | `x-ai/grok-4.5` |
| `deepseek-v4-flash` | DeepSeek V4 Flash | OpenRouter | `deepseek/deepseek-v4-flash` |

模型目录要求：

- `/v1/models` 只能返回上述 7 个模型。
- 不显示 GPT-5.5、旧 GPT 模型、DeepSeek V4 Pro 或其他未验证模型。
- 模型名称、Codex ID 和上游映射必须一致。
- Grok、DeepSeek 和 Kimi 不得继承 GPT 模板中的升级或弃用提示。
- 不得出现“GPT-5.4 即将弃用并切换到 GPT-5.6”等与当前模型无关的文字。
- 不得为了丰富菜单而添加无法完成真实请求的模型。

## 5. 模型选择与上下文

- 模型由用户在 Codex 输入框下方的原生菜单中选择。
- Router 不得自动把选中的模型替换成另一个模型。
- 选择新模型后，下一条消息使用新模型。
- 在同一任务中切换模型时，已有聊天上下文继续保留。
- 不要求为了换模型新建任务或打开多个聊天窗口。
- 旧任务继续对话时，如果保存的是已移除模型，应允许用户重新选择当前 7 个模型之一，不得删除旧任务。

## 6. 思考档位

思考档位必须按模型能力显示：

| 模型 | 可显示档位 |
| --- | --- |
| GPT-5.6 Sol | `low`、`medium`、`high`、`xhigh`、`max`、`ultra` |
| GPT-5.6 Terra | `low`、`medium`、`high`、`xhigh`、`max`、`ultra` |
| GPT-5.6 Luna | `low`、`medium`、`high`、`xhigh`、`max` |
| Grok 4.5 | `minimal`、`low`、`medium`、`high`、`xhigh` |
| DeepSeek V4 Flash | `minimal`、`low`、`medium`、`high`、`xhigh` |
| 两个 Kimi 模型 | 未验证通用思考档位时不显示伪造档位 |

思考档位不仅要出现在菜单中，还必须能够随请求进入 Router，并由 Router 转换为上游能够接受的参数。

## 7. Fast 模式

### 7.1 支持范围

| 模型 | Fast 选项 | 请求映射 |
| --- | --- | --- |
| GPT-5.6 Sol | 显示 | `fast` 映射为 `service_tier=priority` |
| GPT-5.6 Terra | 显示 | `fast` 映射为 `service_tier=priority` |
| GPT-5.6 Luna | 显示 | `fast` 映射为 `service_tier=priority` |
| Kimi for Coding | 不显示 | 无 |
| Kimi for Coding HighSpeed | 不显示 | 它本身是独立高速模型 |
| Grok 4.5 | 不显示 | 未验证 OpenRouter Fast 服务层 |
| DeepSeek V4 Flash | 不显示 | 未验证 OpenRouter Fast 服务层 |

### 7.2 行为要求

- `[features].fast_mode` 必须启用，以允许 Codex 显示目录声明的 Fast 服务层。
- 三个 GPT-5.6 模型必须在目录中声明 `additional_speed_tiers=["fast"]`。
- 三个 GPT-5.6 模型必须声明服务层 ID `priority`，显示名称为 `Fast`。
- 开启 Fast 后，Router 的用量记录必须显示 `service_tier=priority`。
- Fast 请求必须实际由 `ChatGPT Plus OAuth` 通道处理，而不是只改变界面文字。
- 不设置全局 `service_tier`，避免切换到不支持模型时误发 `priority`。
- ChatGPT Fast 会提高 credits 消耗；用户按需手动开启。
- 独立高速模型不得伪装成普通模型的 Fast 开关。

## 8. GPT 路由规则

| 顺序 | 通道 | 角色 | 优先级 |
| --- | --- | --- | --- |
| 1 | ChatGPT Plus OAuth | 首选通道 | 高 |
| 2 | `https://api.430123.xyz/v1` | GPT API 回退通道 | 低 |

行为要求：

1. 用户选择任一 GPT-5.6 模型后，优先使用 ChatGPT Plus OAuth。
2. Plus 可用时持续优先使用 Plus 订阅额度。
3. Plus 额度不足、达到时间窗口、暂不可调度、代理失败或上游传输失败时，自动回退到 430123。
4. 回退必须保持用户选择的模型 ID，不得静默换模型。
5. 回退应尽可能发生在同一个请求中，不要求用户重新选择或重新发送。
6. Plus 恢复后，后续请求自动恢复 Plus 优先级。
7. 普通和 Fast 请求都必须支持流式 Responses。

## 9. Kimi 路由规则

| 顺序 | 通道 | 角色 | 优先级 |
| --- | --- | --- | --- |
| 1 | Kimi Coding Plan 主 Key | 首选 | 10 |
| 2 | Kimi Coding Plan 备用 Key | 回退 | 20 |

行为要求：

- 主 Key 可用时优先使用主 Key。
- 主 Key 额度耗尽、返回 403、受到速率限制或暂不可调度时，自动使用备用 Key。
- 回退时保持当前 Kimi 模型，不要求用户重新发送。
- 主 Key 恢复后重新成为首选。
- HighSpeed 作为独立模型存在，不使用通用 Fast 开关。
- 两个 Key 只能存储在 Windows Credential Manager 中。

## 10. OpenRouter 路由规则

- `grok-4.5` 必须映射到 `x-ai/grok-4.5`。
- `deepseek-v4-flash` 必须映射到 `deepseek/deepseek-v4-flash`。
- 两个模型均通过 `https://openrouter.ai/api/v1` 调用。
- OpenRouter 能直连时不强制依赖 Clash。
- 当前只有一个 OpenRouter 凭据时，不宣称存在第二级 Key 回退。
- OpenRouter 不可用时返回明确错误，不得静默切到其他模型。
- 两个模型必须同时通过普通 Responses 和流式 Responses 验收。

## 11. ChatGPT 登录态与远端控制

- 切换到 `Own-Router` 前后，`auth.json` 必须保持用户当前 ChatGPT 登录对象。
- 安装或同步 Router 不得用 API Key 登录对象覆盖 ChatGPT 登录对象。
- 同步前后应核对 `auth.json` 哈希不变。
- CC Switch 保存的认证对象必须来自当前 `auth.json`，不能保存过期登录副本。
- 本地认证适配器监听 `127.0.0.1:18081`，把 Codex 的 ChatGPT Bearer 请求转换为 Router 本地凭据请求。
- 认证适配器不得在日志中打印 OAuth Token 或本地 API Key。
- Provider 当前禁用 Responses WebSocket；认证适配器仍应正确保留 Upgrade 语义，便于未来启用兼容账号后复验。
- HTTP Responses 请求应避免有问题的长连接复用，防止持续重连。
- 使用 Router 时必须继续保留 Codex 远端控制电脑的能力。

## 12. CC Switch 的 `Own-Router`

`Own-Router` 的职责是保存和恢复整套 Codex Router 配置，而不是日常逐模型切换。

必须满足：

- `Own-Router` 可在 CC Switch 中被启用并显示“使用中”。
- 启用后写入 `model_provider="sub2api"` 和本地 `18081` 入口。
- 保存配置不得包含 `[model_providers.openai]`、`[model_providers.ollama]` 或 `[model_providers.lmstudio]` 等保留 ID 覆盖块。
- 同时恢复默认模型、模型目录路径、认证适配器和 Fast 功能开关。
- 保存的 TOML 必须与当前 Codex `config.toml` 精确一致。
- 保存的认证对象必须与当前 `auth.json` 结构一致。
- 不向 CC Switch 配置写入任何上游 API Key。
- 不覆盖 ChatGPT 登录状态。
- 同步或更新 `Own-Router` 保存内容不得自动切换当前 Provider；用户需要使用时再手动启用。
- 如果 `Own-Router` 已经显示“使用中”，不需要重复点击。
- 模型目录变更后，完全退出并重开 Codex 一次即可加载。

## 13. 本地任务和聊天记录

- 切换 CC Switch Provider 不得删除 Codex 本地任务。
- 修改模型目录、Router 地址或 Fast 设置不得修改聊天正文。
- 重启 Codex 不得清空侧栏任务或聊天历史。
- 本地任务数据库、会话索引和 `sessions` 文件必须与 Router 配置分离。
- 打开旧任务时，原消息记录应继续保留。
- Router 只影响旧任务之后的新请求，不重写历史消息。
- 如果任务列表异常为空，应先检查本地数据库、会话索引、会话文件和侧栏状态，不得直接删除或重建用户数据。
- 修复任务索引时必须保留原始会话文件，并在修改前创建备份。

## 14. 本地服务和端口

| 组件 | 地址 | 用途 |
| --- | --- | --- |
| Adaptive Proxy | `127.0.0.1:17897` | Clash/直连自适应代理 |
| PostgreSQL | `127.0.0.1:15432` | Sub2API 数据库 |
| Redis | `127.0.0.1:16379` | Sub2API 状态和缓存 |
| Sub2API | `127.0.0.1:18080` | 统一模型路由入口 |
| Codex Auth Adapter | `127.0.0.1:18081` | 保留 ChatGPT 登录态的 Codex 入口 |

要求：

- 所有服务只监听 `127.0.0.1`。
- 不向局域网或公网暴露管理页面、数据库或代理。
- `/health` 必须返回成功。
- 重复执行启动脚本不得产生重复进程。
- 任一组件启动失败时应给出明确错误。
- Windows 当前用户的启动目录中必须存在 Router 开机启动项。

## 15. Clash 与网络

- 默认 Clash HTTP 端口为 `127.0.0.1:7897`。
- Clash 可用时，ChatGPT Plus 优先通过 Clash。
- Clash 不可用时，自适应代理尝试 IPv4 直连 ChatGPT。
- 当前网络不能直连 ChatGPT 时，GPT 请求自动回退到 430123。
- Kimi、OpenRouter 和 430123 能直连时直接连接，不跟随 ChatGPT 一起失败。
- 用户开关 Clash 后，不需要修改 Codex、CC Switch 或 Router 配置。
- 代理端口关闭或 Clash 重启时，Router 服务不得退出。
- 实际运行的 `17897` 代理必须能访问 ChatGPT、Kimi、OpenRouter 和 430123。

## 16. 自动恢复

- 上游账号定期探测，目标周期为每小时整点一次：`0 * * * *`。
- 暂停或冷却账号探测成功后自动恢复。
- Plus 恢复后重新成为 GPT 首选。
- Kimi 主 Key 恢复后重新成为 Kimi 首选。
- 优先使用上游返回的额度重置时间；没有准确时间时依赖定时探测。
- 用户不需要为了恢复账号手动登录管理页面解除暂停。

## 17. 稳定性与错误处理

- 普通 Responses 和流式 Responses 都必须可用。
- 流式请求必须收到完整结束事件。
- 不得持续显示“正在重新连接”后最终失败。
- 单次上游错误不得导致 Sub2API、认证适配器或代理进程退出。
- OpenRouter 不可用时返回明确上游错误。
- 不支持的模型、思考档位或服务层不得伪装为可用。
- 回退上游不能提供同一模型时，不得静默换模型。
- 管理页面和日志用于诊断，但不得成为日常对话的前置步骤。

## 18. 密钥与安全

- 所有上游 Key、本地 API Key 和管理凭据存储在 Windows Credential Manager。
- 项目脚本、模型目录、Codex 配置和 CC Switch 配置不得包含明文上游 Key。
- 日志不得输出完整 Key、OAuth Token、密码或认证 JSON。
- 凭据通过本地命令式认证读取。
- 更换 Key 时使用专用配置脚本，不需要重装 Router。
- 曾在聊天中直接粘贴过的 Key 应视为已暴露并进行轮换。
- 修改 `auth.json`、CC Switch 数据库或任务索引前必须创建备份。

## 19. 可观测性

状态检查应能确认：

- 五个 Router 组件是否运行；
- `/health` 是否通过；
- 各上游账号是否 active、可调度；
- Plus 是否绑定自适应代理；
- 最近请求实际命中的账号；
- 请求模型和上游模型是否一致；
- Fast 请求是否记录为 `service_tier=priority`；
- 发生回退的原因和预计恢复时间；
- 每小时探测计划是否启用。

## 20. 验收标准

### 20.1 配置与登录

- 手动切换到 `Own-Router` 后 `is_current=1`；仅同步保存内容时不得改变当前 Provider。
- CC Switch 中保存的 TOML 与当前 `config.toml` 精确一致。
- CC Switch 中保存的认证对象与当前 `auth.json` 一致。
- 安装、同步和切换前后 `auth.json` 哈希不变。
- Codex 远端控制仍可用。

### 20.2 模型目录

- `/v1/models` 恰好返回 7 个模型。
- Grok 4.5 和 DeepSeek V4 Flash 均存在且映射正确。
- 不存在 GPT-5.5、DeepSeek V4 Pro、旧模型或错误升级提示。
- 三个 GPT 显示 Fast；其余四个模型不显示 Fast。

### 20.3 请求

- 三个 GPT-5.6 模型普通和流式请求成功。
- 三个 GPT 的 Fast 流式请求成功，并记录 `service_tier=priority`。
- GPT 请求在 Plus 可用时命中 `ChatGPT Plus OAuth`。
- Grok 4.5 普通和流式请求成功。
- DeepSeek V4 Flash 普通和流式请求成功。
- Kimi 主/备用 Key 回退按优先级工作。
- GPT Plus/430123 回退在不改变模型的情况下工作。

### 20.4 运行状态

- `17897`、`15432`、`16379`、`18080`、`18081` 均在监听。
- `/health` 返回成功。
- 实际代理能够连接四个上游域名。
- 开机启动项存在并指向统一启动脚本。
- 认证适配器单元测试全部通过。

### 20.5 任务记录

- 重启 Codex 前后任务数量和旧消息保持不变。
- 打开旧任务后历史消息可见。
- 在旧任务中选择当前模型后能够继续发送新消息。

## 21. 日常使用流程

1. 确认 CC Switch 中 `Own-Router` 显示“使用中”；已使用中时不要重复点击。
2. Router 由 Windows 启动项自动运行。
3. 模型目录刚发生变化时，完全退出并重新打开 Codex 一次。
4. 在 Codex 原生菜单中选择模型。
5. 选择该模型实际支持的思考档位。
6. 使用三个 GPT 模型时，按需开启或关闭 Fast。
7. 直接发送消息。
8. GPT Plus/API 回退和 Kimi 主/备用 Key 回退由 Router 自动完成。
9. 只有诊断问题时才打开 Sub2API 管理页面。

## 22. 禁止事项

- 不得要求用户日常反复打开 CC Switch 换模型。
- 不得把 CC Switch Local Route 作为日常对话前提。
- 不得自动更换用户选择的模型。
- 不得在未经验证的模型上显示思考档位或 Fast。
- 不得通过全局 `service_tier` 把 Fast 强加到所有模型。
- 不得把 Kimi HighSpeed 伪装成普通 Kimi 的 Fast。
- 不得保存明文密钥。
- 不得暴露本地服务到局域网或公网。
- 不得为了修复侧栏而删除原始任务数据库或会话文件。
- 不得让 Router 配置覆盖 ChatGPT 登录状态。

## 23. 历史要求变更

下列早期目标已被后续确认替代，不再作为当前实现要求：

| 早期要求 | 最终要求 |
| --- | --- |
| 模型菜单目标为 23 个模型 | 只保留实际验证的 7 个模型 |
| 包含 GPT-5.2、5.3、5.4、5.5 等旧系列 | 只保留 GPT-5.6 Sol、Terra、Luna |
| OpenRouter 包含 DeepSeek V 或 DeepSeek V4 Pro | 改为 DeepSeek V4 Flash |
| 多个旧模型支持 Fast | 仅三个 GPT-5.6 模型显示并实测 Fast |
| 因响应显示 `service_tier=default` 而隐藏全部 Fast | 以 Router 用量记录为准，三个 GPT 实际转发 `priority` |

## 24. 最终定义

本项目的最终行为定义为：

> 用户在 Codex 原生菜单中选择 7 个已验证模型之一。本地 Router 在不改变模型的前提下自动选择上游账号：GPT 优先使用 ChatGPT Plus，失败后回退到 430123；Kimi 优先使用主 Key，失败后使用备用 Key；Grok 4.5 和 DeepSeek V4 Flash 通过 OpenRouter。只有 GPT-5.6 Sol、Terra、Luna 显示并实际使用 Fast priority 服务层。启用 `Own-Router` 后，Codex 保持 ChatGPT 登录态、远端控制和本地聊天记录，所有普通和流式对话均应稳定完成。
