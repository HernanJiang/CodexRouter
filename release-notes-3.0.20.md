# CodexRouter v3.0.20

GLM-5.3-Flash 的 `max` 仍可用。第三方模型会把思考过程交给 Codex 显示，不再只丢最终结果。

## 修复

- Flash 目录提供 `low` / `high` / `max`；`max` 不改写。非法档位按智谱 Coding Plan 映射，避免 400。
- Gemini / GLM / DeepSeek 等第三方模型默认打开思考摘要；网关把 `summary=none` 改成 `auto`。
- GLM-5.3 输出上限改为官方 128K。
