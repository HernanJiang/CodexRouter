# Own-Router 全模型验收报告

## 验收结论

- 验收日期：2026 年 7 月 31 日
- Codex CLI：`0.146.0-alpha.3.1`
- `Own-Router` 默认模型：`deepseek-v4-flash`
- `Own-Router` 当前状态：已修复并保存，但未设为当前 Provider
- 当前 Provider：`Chiral-DXH`
- ChatGPT 登录状态：修复前后 `auth.json` 哈希不变

`Own-Router` 已通过同版本 Codex 的严格配置加载和真实任务创建测试。7 个模型全部成功，模型目录声明的 27 个思考档位组合全部收到完整的 `response.completed` 终止事件。

## 修复内容

1. 使用非保留 Provider ID `sub2api`，不再创建或保存 `[model_providers.openai]`。
2. 默认模型改为 `deepseek-v4-flash`。
3. 设置 `supports_websockets=false`，使当前模型统一使用已验证的 HTTP Responses 流。
4. 清除已被当前 Codex 版本移除的 `disable_response_storage` 配置字段。
5. CC Switch 同步只更新 `Own-Router` 保存内容，不自动切换当前 Provider。
6. 同步前备份 CC Switch 数据库，且不修改 ChatGPT 登录对象。

## 全模型结果

| 模型 | 已测试思考档位 | 结果 |
| --- | --- | --- |
| `gpt-5.6-sol` | `low`、`medium`、`high`、`xhigh`、`max`、`ultra` | 全部通过 |
| `gpt-5.6-terra` | `low`、`medium`、`high`、`xhigh`、`max`、`ultra` | 全部通过 |
| `gpt-5.6-luna` | `low`、`medium`、`high`、`xhigh`、`max` | 全部通过 |
| `kimi-for-coding` | 不声明通用思考档位 | 通过 |
| `kimi-for-coding-highspeed` | 不声明通用思考档位 | 通过 |
| `grok-4.5` | `minimal`、`low`、`medium`、`high`、`xhigh` | 全部通过 |
| `deepseek-v4-flash` | `minimal`、`low`、`medium`、`high`、`xhigh` | 全部通过 |

## 验证层级

### Router 流式验证

7 个模型均通过 `127.0.0.1:18081/v1/responses` 发出真实流式请求。所有请求均返回 HTTP 200，并收到 `response.completed`。

### Codex 严格配置验证

从当前 Windows Codex 安装包复制同版本 CLI，在隔离的临时 `CODEX_HOME` 中加载 CC Switch 保存的 `Own-Router` 配置。每个模型均通过 `--strict-config` 创建真实、临时、不保存会话的 Codex 任务，结果均为：

- 进程退出码为 0；
- 收到 `turn.completed`；
- 模型回复 `OK`；
- 未触发工具调用。

### 传输验证

全模型测试期间，Sub2API 日志只记录 `POST /v1/responses`，状态均为 200；对应时间窗口的 WebSocket 入口计数为 0。因此不会再出现因账号不支持 Responses WebSocket v2 而连续重连 5 次的问题。

## 已确认根因

1. 旧 `Own-Router` 保存副本包含 `[model_providers.openai]`。`openai` 是 Codex 保留的内置 Provider ID，不能被自定义配置覆盖。
2. 内置 Provider 会尝试 Responses WebSocket v2，但当前 OpenRouter、Kimi 和部分回退账号只支持 HTTP Responses 或 Chat Completions 转换，导致握手后找不到可用 WSv2 账号。
3. 保存副本继承了过期字段 `disable_response_storage`，同版本 Codex 的严格配置模式会直接拒绝加载。

## 人工界面复验

Windows 自动化规则禁止代理直接操控 Codex 桌面客户端，因此右下角菜单需要人工完成最后一次界面复验：

1. 完全退出并重新打开 Codex。
2. 在 CC Switch 中手动切换到 `Own-Router`。
3. 新建临时任务，依次选择任一模型及其思考档位后发送短消息。

底层严格配置、模型选择、思考档位、路由和流式终止事件均已自动验收通过。
