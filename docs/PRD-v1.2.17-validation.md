# Codex-Router v1.2.17 PRD 验证记录

验证日期：2026-08-05

基线：v1.2.14 PRD、v1.2.16 已存档发布包、v1.2.17 最终发布包。

## 总结

本轮没有将所有功能宣告为通过。已修复并真实验证升级数据保留、Codex 本地目录绑定、5 小时 OAuth 恢复去重、OpenCode 产品退出、用量端点凭据边界、同名 GUI 进程所有权和应用数据库就绪检查。Sub2API 错误分类、continuation 所有权、Transport 复用及长时间资源测试仍有明确限制。

最终发布包：

`D:\Work\Tools\Codex-Router-Releases\Codex-Router-Portable-1.2.17-windows-x64-20260805-164000-879.zip`

SHA-256：`26A72164D9D37B6F1A903A6DE56EB718FBA2D23090991A853D48CAD213CD3714`

旧包存档：

`D:\Work\Tools\Codex-Router-Releases\Archived-Model-Packages\Codex-Router-Portable-1.2.16-windows-x64-20260805-013749-068.zip`

## 功能矩阵

| ID | 最终状态 | 主要证据与限制 |
|---|---|---|
| F01 | 已实现但尚未验证 | 最终 ZIP 可直接组装并通过 Stage 校验；稳定 UserData 与 portable state 代码存在。中文长路径和全新 Windows 用户仍未在独立 Windows 账户验证。 |
| F02 | 已完整实现并实际通过 | PostgreSQL、Redis、Sub2API 均由最终包运行且只监听回环；PID/路径所有权通过能力检查；重复 Apply 时 Sub2API PID 不变。端口冲突时拒绝接管其他包进程。 |
| F03 | 已实现但尚未验证 | 条款版本、滚动和主动同意代码及 Rust 测试通过；本轮未自动重走全新用户滚动交互。 |
| F04 | 已完整实现并实际通过 | Chiral API 渠道保存、应用、模型目录、Credential 引用和真实请求通过；无 Key 返回 401，未知模型返回 400；重启后保持。 |
| F05 | 部分实现 | catalog 图片能力和 80% 上下文配置测试通过；真实图片传输、阈值压缩及压缩后工具顺序未在当前上游执行。 |
| F06 | 已实现但尚未验证 | reasoning/Fast catalog 与配置单测通过；本轮真实请求使用 medium，未逐个产生 low/high/xhigh/max/ultra 外部调用。 |
| F07 | 部分实现 | OpenAI OAuth 账号发现、状态、套餐和模型读取通过；Grok OAuth 浏览器登录、账号发现和 13 个模型读取通过；Token 未进入 Router JSON。Claude、Gemini、Antigravity 尚无当前授权。 |
| F08 | 部分实现 | OpenAI OAuth account 1 先收到真实 429，日志记录 failover，API account 2 随后成功返回；未选 OAuth 时只使用 API。Grok OAuth 正常请求与工具 continuation 命中 account 4，但 OAuth 不可调度时没有跨平台回退到 OpenRouter，而是返回 503；多 API B/C 顺序也因仅配置一个 API 渠道未验证。 |
| F09 | 已完整实现并实际通过 | 真实 reset_at 持久到 2026-08-08；恢复脚本返回约 241014 秒、probed=0；未知 reset 改为 18000 秒，观察文件含 lastProbeAt/nextProbeAt，测试通过。 |
| F10 | 实现存在缺陷 | 无 Key 401、未知模型 400、OAuth 429 分类和回退通过；`input: 42` 普通无效请求被预编译 Sub2API 包装为 502，未满足普通 400 不切换要求。 |
| F11 | 已实现但尚未验证 | 现有源码测试覆盖输出前失败与输出后不回放；真实 SSE 11 个事件并完成。工具调用后断流和客户端取消的最终二进制假上游矩阵未执行。 |
| F12 | 部分实现 | `.2.patch` 含跨 provider 清理与 owner 测试，但当前 Go 源码与补丁无法干净对应，发布二进制也没有可重现构建证明。不得判完整通过。 |
| F13 | 已完整实现并实际通过 | 重复 Apply 约 11.8 秒，Sub2API PID 31424 保持不变；模型和默认模型热同步。代理改变与活动流保护仍主要由脚本测试覆盖。 |
| F14 | 无法在当前环境验证 | 启动环境设置有界连接池；Python 代理/适配器 13 项测试通过。缺少 Go 工具链，无法对最终 Sub2API Transport/client cache 做连接复用、TTL 和泄漏实测。 |
| F15 | 部分实现 | API 网关在 OAuth 429 后立即回退，GitHub/OAuth 查询未阻塞真实请求；完整 ready 时间修改前后对比与后台并发计数未采集。 |
| F16 | 部分实现 | 命名配置创建、应用、restore point 与原子写测试通过；删除配置和凭据清理路径仍不完整。 |
| F17 | 已实现但尚未验证 | Codex 无关设置保留，DPAPI 测试通过，完全重启后 provider/catalog 正确；未使用另一 Windows 用户验证 DPAPI 不可解密。 |
| F18 | 已完整实现并实际通过 | Apply 前后 OpenCode 文件 SHA-256 始终为 `1632DD...FC1892`；GUI/README/发布包入口已移除；最终 Stage 不含 OpenCode 安装脚本。 |
| F19 | 部分实现 | 用量字段和缓存逻辑存在；ZenMux 凭据外发风险已用严格 HTTPS 主机白名单修复并负面测试。账号全局统计按配置归属问题尚未完全解决。 |
| F20 | 部分实现 | auto/direct/HTTP/HTTPS/SOCKS 测试通过；PAC/WPAD 未实际解析；认证代理管理链路仍有限制。 |
| F21 | 部分实现 | 自适应代理拒绝显式上游响应、无代理直连测试通过；预编译补丁仍把部分代理认证文本视为可直连条件，缺少真实 407 端到端测试。 |
| F22 | 已实现但尚未验证 | 托盘暂停逻辑、健康阈值及退出测试通过；未完成连续数小时 CPU/内存/句柄增长测试。 |
| F23 | 部分实现 | Credential Manager、DPAPI、回环鉴权、无 Key 401、秘密扫描和 ACL 单测通过；运行 config.toml 按 Codex 约束含本地 bearer；锁继承仍由可伪造环境标记表示。 |
| F24 | 部分实现 | 版本比较、官方 GitHub URL和大小检查存在；客户端仍未消费独立签名 SHA-256 manifest，下载失败仍通过结构化 JSON 而非可靠非零退出码。 |
| F25 | 已完整实现并实际通过 | 最终 Stage 1130 文件；manifest/archive 哈希一致；秘密扫描干净；无 UserData/OpenCode；PostgreSQL 初始化/查询/停止通过；CRT 6 文件、14.44.35211.0、x64、签名和两处哈希通过。 |

## 实际测试

- Rust：135 passed，1 ignored（需要显式 packaged OAuth 环境）。
- Clippy：`--all-targets -D warnings` 通过。
- Python：17 passed。
- 代理协议：13 passed。
- PowerShell：OAuth routing、Codex integration、catalog、usage parsing、Credential Store、OpenAI policy、managed proxy、proxy discovery、Router Base URI 均通过。
- 最终 Stage：manifest、archive、秘密扫描、PostgreSQL payload、VC Runtime payload 均通过。
- Codex 完全重启：`provider=codex_router`、`requires_openai_auth=false`、catalog 仅 Router 模型、CLI 真实返回 `OK`。
- API Responses：真实返回 `OK`，约 6193 ms。
- SSE：11 个事件，明确完成，约 7780 ms。
- OAuth 429：account 1 真实 429，切换 account 2 后成功；恢复脚本未提前探测。
- Grok OAuth：浏览器登录成功，account 4 状态 active/schedulable，发现 `grok-4.5` 等 13 个模型；两轮真实 Responses 工具调用及 continuation 均命中 account 4 并完成。
- Grok/OpenRouter 回退：将 account 4 通过官方管理接口临时设为不可调度后，`grok-4.5` 返回 503，未选择 OpenRouter API。账号已在 `finally` 中恢复为可调度，测试路由配置随后还原。
- OpenCode：Apply 前后文件哈希不变。

## 性能与资源快照

- PostgreSQL payload 冷初始化：2624 ms；启动：529 ms；查询：508 ms；智能停止：456 ms。
- 重复 Apply：约 11755 ms；Sub2API PID 不变。
- 运行快照：Sub2API 约 94.0 MiB、398 handles、21 threads；Redis 约 16.9 MiB；PostgreSQL 总约 240 MiB。
- 该快照不是数小时泄漏测试，不能证明长期资源不增长。

## 阻塞与风险

- 本机没有 Go 工具链；`.2.patch` 无法干净应用到当前 `source/backend`，无法验证或重建最终 Sub2API 二进制。
- 普通无效 Responses 请求仍可能被包装为 502，F10 未通过。
- Transport 缓存、HTTP Client 复用、response body/goroutine 泄漏缺少最终 Go 二进制测试。
- 更新包缺少客户端独立可信哈希/签名链。
- 当前 composite route 每个公开模型只解析为一个目标平台；`grok` OAuth 耗尽后不能继续调度 `openai` 平台的 OpenRouter 同模型账号。不得宣称 Grok OAuth 到 OpenRouter 自动回退。
- PAC/WPAD、认证代理 407、长时间托盘资源、其他 OAuth 平台和多 API B/C 顺序尚未完成外部验证。
