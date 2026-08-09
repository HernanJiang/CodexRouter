# Volcengine Ark Coding Plan quota/usage query

研究范围：火山引擎 Ark Coding Plan 的个人版和企业版周/月额度查询。本文只记录火山引擎官方文档、官方 `volcengine/ark-cli` 的固定提交/发布二进制，以及官方 Go SDK 的请求和签名实现；没有修改生产代码，也没有记录任何真实凭据。

## 结论摘要

- **个人版**：调用 `GetCodingPlanUsage`。它返回当前账号的 quota 快照，官方 CLI 展示为 `session`、`weekly`、`monthly` 三个窗口。
- **企业版/席位版**：单席位调用 `GetSeatInfoUsage`；管理员批量查询调用 `ListSeatInfoUsages`。这两组接口返回周/月用量字段，但结构不同于个人版。
- **鉴权**：这是火山引擎 OpenTOP 控制面接口，使用 Volcengine Signature V4（静态 AK/SK，或 SSO 派生的临时 STS 凭据）。临时 STS 还必须带 `X-Security-Token`。**不能**用推理 API Key 的 `Authorization: Bearer ...` 查询该接口。
- **个人版重置时间**：`QuotaUsage[].ResetTimestamp` 是服务端提供的 Unix epoch 秒；每个窗口独立返回，因此客户端不能硬编码“每周某天/每月某日”。官方 CLI 将它转换为 RFC3339 `UTC+08:00`。
- **个人版精度边界**：官方 Coding Plan 响应只暴露 `Percent`，没有绝对 `used` 和 `total`。若界面需要“剩余百分比”，客户端只能计算 `100 - Percent`，并应标注这是派生值。

## 1. 个人版：GetCodingPlanUsage

### Endpoint 与方法

官方 CLI 文档把它记为逻辑 Action path：`/open/GetCodingPlanUsage`。实际公网 OpenTOP 请求由官方 CLI 构造成：

```text
POST https://open.volcengineapi.com/?Action=GetCodingPlanUsage&Version=2024-01-01
Content-Type: application/json
```

这里的 `/open/GetCodingPlanUsage` 是 OpenAPI/Action 的逻辑标识，不应直接拼成 `https://open.volcengineapi.com/open/GetCodingPlanUsage` 作为最终请求 URL。官方 Go SDK 的 Query 协议实现也说明 `Action` 和 `Version` 是公共请求参数；官方 CLI v1.0.13 的请求构造字符串进一步确认了公网 URL 模板 `%s/?Action=%s&Version=%s`、`POST` 和生产版本 `2024-01-01`。[1][4][5]

### 请求体

普通个人版查询使用空 JSON 对象：

```json
{}
```

官方 CLI 恢复出的请求类型为：

```json
{
  "SeatID": "optional-seat-id",
  "IgnoreSeatStatus": true
}
```

两个字段均为可选；个人版正常查询不需要发送它们。不要把 `ARK_API_KEY` 或任何推理 API Key 放入这里或作为控制面 Bearer 凭据。[1]

### 原始响应字段

官方 CLI v1.0.13 二进制中的第一方类型元数据恢复为以下结构：

```json
{
  "Result": {
    "Status": "...",
    "UpdateTimestamp": 0,
    "QuotaUsage": [
      {
        "Level": "session",
        "Percent": 0,
        "ResetTimestamp": 0
      },
      {
        "Level": "weekly",
        "Percent": 0,
        "ResetTimestamp": 0
      },
      {
        "Level": "monthly",
        "Percent": 0,
        "ResetTimestamp": 0
      }
    ]
  }
}
```

字段语义：

| 原始字段 | 类型 | 语义和单位 |
|---|---:|---|
| `Result.Status` | string | 后端状态；官方 CLI 类型中为可选字段。 |
| `Result.UpdateTimestamp` | int64 | 这次快照更新时间；官方 CLI 将其作为 epoch **毫秒**透传。 |
| `Result.QuotaUsage[]` | array | 各 quota 窗口。 |
| `QuotaUsage[].Level` | string | 窗口标识；官方 CLI 对个人 Coding Plan 稳定呈现 `session`、`weekly`、`monthly`。 |
| `QuotaUsage[].Percent` | number | 已使用百分比，通常为 `0-100`；不是剩余百分比。 |
| `QuotaUsage[].ResetTimestamp` | int64 | 对应窗口下次重置时间，Unix epoch **秒**。 |

官方 CLI 的对外规范明确写出：Coding Plan 后端只返回 `Percent`，`used`/`total` 不存在；并将 Coding Plan 的重置时间从后端秒转换为统一的 RFC3339 `UTC+08:00` 输出。[1]

### 周/月读取示例

客户端应按 `Level` 匹配，而不是按数组下标匹配：

```text
weekly  -> QuotaUsage[i].Percent + QuotaUsage[i].ResetTimestamp
monthly -> QuotaUsage[i].Percent + QuotaUsage[i].ResetTimestamp
```

建议保存原始秒值，并在展示层转换：

```text
reset_at_rfc3339 = Unix(ResetTimestamp, 0).In(UTC+08:00)
remaining_percent = 100 - Percent
```

`remaining_percent` 是客户端计算值，不是火山引擎接口返回字段。

## 2. 企业版/席位版

### 单席位

官方 CLI 将企业 Coding Plan 的单席位 Action 记为 `GetSeatInfoUsage`，逻辑 path 为 `/open/GetSeatInfoUsage`。实际公网请求遵循同一个 OpenTOP 模板：

```text
POST https://open.volcengineapi.com/?Action=GetSeatInfoUsage&Version=2024-01-01
Content-Type: application/json
```

请求字段：

```json
{
  "SeatID": "optional-seat-id",
  "ProjectName": "optional-project",
  "Scene": "optional-scene"
}
```

官方 CLI 恢复的响应类型为：

```json
{
  "Result": {
    "SeatID": "...",
    "ProjectName": "...",
    "UserID": "...",
    "UserName": "...",
    "MonthlySubscribeMilestone": 0,
    "MonthlyResetMilestone": 0,
    "ShortTermUsage": 0,
    "WeeklyUsage": 0,
    "MonthlyUsage": 0
  }
}
```

`ShortTermUsage`、`WeeklyUsage`、`MonthlyUsage` 是企业版的用量数值；官方 CLI 将 Coding Plan 企业版视为百分比视图，不把它当作个人版的 `Percent + ResetTimestamp` 数组。[1]

**重置时间边界：**该官方恢复类型只显式暴露 `MonthlyResetMilestone`，没有 `WeeklyResetMilestone`。公开的官方 CLI 文档没有为这两个 milestone 写出可独立核验的时间单位，也没有给出单独的周重置字段。因此，企业版不能从这组字段可靠推导出“下一次周重置时间”；实现不应把个人版 `ResetTimestamp` 的秒规则套到它上面。若需要企业版完整的窗口/重置展示，应以实际返回的批量 usage 接口字段为准，并保留“官方公开契约未说明”的状态。

### 管理员批量席位

批量查询 Action 为 `ListSeatInfoUsages`，逻辑 path 为 `/open/ListSeatInfoUsages`：

```text
POST https://open.volcengineapi.com/?Action=ListSeatInfoUsages&Version=2024-01-01
Content-Type: application/json
```

请求类型：

```json
{
  "SeatIDs": ["seat-001", "seat-002"],
  "Scene": "optional-scene",
  "ProjectName": "optional-project"
}
```

其中 `SeatIDs` 是批量接口的必需字段。响应顶层为 `Result.Data[]` 与 `Result.Total`，`Data[]` 项目使用与上面企业版单席位响应相同的 Coding Plan usage 字段。官方 CLI 文档注明：企业版席位用量只暴露百分比。[1]

## 3. 鉴权与数据平面边界

### 控制面：本研究涉及的接口

使用 Volcengine Signature V4：

```text
Authorization: HMAC-SHA256 Credential=<AK>/<date>/<region>/<service>/request,
               SignedHeaders=<...>, Signature=<signature>
X-Date: <UTC timestamp>
X-Content-Sha256: <body sha256>
```

如果是临时 STS 凭据，还要发送：

```text
X-Security-Token: <redacted-session-token>
```

官方 CLI 的认证说明区分了三种控制面状态：SSO 登录后使用派生的临时 STS；显式 STS 身份直接使用临时凭据；没有 STS 时才使用静态 AK/SK。官方 SDK 的 `Sign4` 实现展示了 `HMAC-SHA256`、`Authorization`、`X-Date`、`X-Content-Sha256` 和 `X-Security-Token` 的实际签名行为。[2][5]

### 数据面：不要混用

推理 API Key 的形式是：

```text
Authorization: Bearer <redacted-ark-api-key>
```

它用于 Coding Plan 的推理 Base URL，例如官方 Coding Plan 集成文档中的 `/api/coding` 或 `/api/coding/v3`，不是 `GetCodingPlanUsage` 这类 OpenTOP 控制面 Action。官方 CLI 也明确把“控制面 usage”等接口与“数据面 runtime Bearer API Key”分开。[2][3]

因此，对用量查询失败的判断应优先检查：

1. 是否使用了 Volcengine 控制面 Signature V4，而不是 Bearer API Key。
2. SSO/STS 是否仍有效，临时凭据是否带 `X-Security-Token`。
3. `Action`、`Version=2024-01-01` 和 JSON 请求体是否正确。
4. 当前控制面身份是否对应期望的账号、项目和席位。

## 4. 官方行为验证记录

以下验证没有使用真实凭据：

- 对 `POST https://open.volcengineapi.com/open/GetCodingPlanUsage` 发送无签名请求时，服务端返回“缺少 `Action` 参数”；这支持“`/open/GetCodingPlanUsage` 是逻辑 Action path，而不是最终公网 path”的判断。
- 对根路径携带 `Action=GetCodingPlanUsage&Version=2024-01-01` 的无签名请求，服务端进入凭据校验并返回 `InvalidCredential`/`InvalidAccessKey` 类错误；这确认了 Action、版本和 OpenTOP 控制面鉴权链路可达，而不是控制台专用前端接口。
- 官方 Ark CLI v1.0.13 Windows x64 二进制 SHA-256：`EBF8A1BDECDBF96D5A5EB97C78699741B61C5DCEF1FA5B8CDAA9901F68F95629`。研究中仅从该第一方发布二进制恢复类型元数据和请求构造常量，未恢复或记录任何真实密钥。

## 来源

1. [Volcengine `ark-cli` 固定提交：`arkcli-usage-plan.md`](https://github.com/volcengine/ark-cli/blob/daa24759793f6db7c888bf5bdb61990e0c8e249b/skills/arkcli-usage/references/arkcli-usage-plan.md)；[同提交的 usage skill](https://github.com/volcengine/ark-cli/blob/daa24759793f6db7c888bf5bdb61990e0c8e249b/skills/arkcli-usage/SKILL.md)。官方 CLI 命令、Action、个人/企业版窗口、字段语义、单位和逻辑 path 均以此为准。
2. [Volcengine `ark-cli` 固定提交：`auth-modes.md`](https://github.com/volcengine/ark-cli/blob/daa24759793f6db7c888bf5bdb61990e0c8e249b/skills/arkcli-auth/references/auth-modes.md)。官方 CLI 对控制面 STS/AK-SK 签名与数据面 Bearer API Key 的边界说明。
3. [火山引擎官方：Coding Plan 接入 Codex](https://www.volcengine.com/docs/82379/2556056)。官方产品接入文档，确认 Coding Plan 的推理 Base URL 与 API Key 属于数据面配置；不将其当作控制面 quota API。
4. [Volcengine Go SDK v1.2.43：Query 协议构造](https://github.com/volcengine/volcengine-go-sdk/blob/v1.2.43/volcengine/volcenginequery/build.go)。官方 SDK 将 `Operation.Name` 放入 `Action`，将 `ClientInfo.APIVersion` 放入 `Version`，并按 JSON 请求体处理 POST。
5. [Volcengine Go SDK v1.2.43：Signature V4](https://github.com/volcengine/volcengine-go-sdk/blob/v1.2.43/volcengine/base/sign.go)；[SDK signer wiring](https://github.com/volcengine/volcengine-go-sdk/blob/v1.2.43/volcengine/signer/volc/volc.go)。官方 SDK 的 HMAC-SHA256、Authorization、X-Date、X-Content-Sha256 和临时 `X-Security-Token` 实现。

## 可信度和未决项

- **已确认**：个人版 Action、企业版 Action、请求方法、OpenTOP URL 模板、生产 API version、请求字段、个人版原始响应字段、个人版重置时间单位、鉴权平面边界。
- **第一方实现观察**：`MonthlySubscribeMilestone`/`MonthlyResetMilestone` 的具体时间单位，以及企业版是否有可单独查询的周重置时间，在当前公开官方 CLI 文档和恢复的官方类型中没有充分说明；本文没有猜测。
- **未做的事情**：没有调用真实账号、没有写入生产代码、没有上传凭据或修改火山引擎资源。
