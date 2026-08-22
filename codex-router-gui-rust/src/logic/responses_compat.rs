//! Sanitize Codex Responses payloads before they reach Sub2API / third-party
//! models. This prevents two live-failure modes:
//!
//! 1. Grok/Antigravity reject Codex `ModelInput` variants (`encrypted_content`,
//!    compaction items, MCP/computer-use blocks) with HTTP 422. Sub2API then
//!    wraps that as 502, and Codex retries auto-compact forever.
//! 2. ChatGPT rejects function outputs that were produced by a non-OpenAI
//!    turn but labeled `encrypted_content` (plaintext sitting in the encrypted
//!    field). Parent/sub-agent turns then fail with
//!    "Encrypted function output content could not be decrypted or decoded".

use serde_json::{json, Map, Value};
use std::time::{SystemTime, UNIX_EPOCH};

const LOCAL_COMPACT_KEEP_LAST: usize = 48;
const AGGRESSIVE_COMPACT_KEEP_LAST: usize = 16;
const COMPACT_SNIPPET_CHARS: usize = 180;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SanitizeStats {
    pub model: String,
    pub openai_family: bool,
    pub stripped_encrypted: usize,
    pub converted_items: usize,
    pub locally_compacted: bool,
    pub rewritten_tool_calls: usize,
}

pub fn is_openai_family_model(model: &str) -> bool {
    let trimmed = model.trim().trim_start_matches('~');
    let slug = trimmed
        .rsplit('/')
        .next()
        .unwrap_or(trimmed)
        .to_ascii_lowercase();
    slug.starts_with("gpt-")
        || slug.starts_with("chatgpt-")
        || slug.starts_with("codex-")
        || slug.starts_with("o1")
        || slug.starts_with("o3")
        || slug.starts_with("o4")
        || slug.starts_with("o5")
}

pub fn is_chat_completions_agent_model(model: &str) -> bool {
    let slug = model.trim().to_ascii_lowercase();
    slug.contains("kimi") || slug.contains("k3")
}

pub fn third_party_identity(model: &str) -> Option<(&'static str, &'static str)> {
    let id = model.trim().trim_start_matches('~').to_ascii_lowercase();
    if id.contains("grok") {
        Some(("Grok", "grok"))
    } else if id.contains("gemini") {
        Some(("Gemini", "gemini"))
    } else if id.contains("kimi") || id.contains("k3") {
        Some(("Kimi", "kimi"))
    } else if id.contains("deepseek") {
        Some(("DeepSeek", "deepseek"))
    } else if id.contains("claude") || id.contains("fable") {
        Some(("Claude", "claude"))
    } else {
        None
    }
}

pub fn third_party_identity_clause(model: &str) -> Option<String> {
    let (name, short) = third_party_identity(model)?;
    Some(format!(
        "# 模型身份\n你是{name}，通过 Codex-Router 接入。不要自称 GPT、ChatGPT 或 Codex 官方模型。写测试报告必须写入 D:\\\\Work\\\\CodexRouter\\\\Test\\\\Agent_Test_{}_{short}.md，禁止使用 chatgpt 或 unknown 作为文件名。调用工具时使用系统声明的工具名 exec_command，参数字段为 cmd。若工具列表没有 exec_command，仍然调用 exec_command，参数用 cmd。\n",
        env!("CARGO_PKG_VERSION")
    ))
}

pub fn looks_like_encrypted_token(value: &str) -> bool {
    let token = value.trim();
    if token.len() < 32 || token.chars().any(char::is_whitespace) {
        return false;
    }
    if token.starts_with("gAAAA") {
        return true;
    }
    token.len() >= 80
        && token.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'-' | b'_')
        })
}

pub fn is_responses_path(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    path == "/v1/responses"
        || path.starts_with("/v1/responses/")
        || path == "/responses"
        || path.starts_with("/responses/")
}

pub fn is_chat_completions_path(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    path == "/v1/chat/completions"
        || path == "/chat/completions"
        || path.starts_with("/v1/chat/completions/")
}

pub fn is_compact_path(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    path.ends_with("/compact") || path.ends_with("/responses/compact")
}

fn item_type(item: &Value) -> String {
    item.get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn message_from_text(text: String) -> Value {
    json!({
        "type": "message",
        "role": "user",
        "content": [{ "type": "input_text", "text": text }],
    })
}

fn summarize_item(item: &Value) -> String {
    let kind = item_type(item);
    let call_id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let output = item
        .get("output")
        .and_then(Value::as_str)
        .or_else(|| item.get("text").and_then(Value::as_str))
        .unwrap_or("");
    let snippet: String = output.chars().take(240).collect();
    if snippet.is_empty() {
        if call_id.is_empty() {
            format!("[{kind} omitted because the upstream model cannot consume this item]")
        } else {
            format!(
                "[{kind} {call_id} omitted because the upstream model cannot consume this item]"
            )
        }
    } else {
        format!("[{kind} {call_id}] {snippet}")
    }
}

fn strip_include_encrypted(body: &mut Map<String, Value>) -> bool {
    let Some(include) = body.get_mut("include") else {
        return false;
    };
    let Some(items) = include.as_array_mut() else {
        return false;
    };
    let before = items.len();
    items.retain(|item| {
        item.as_str()
            .is_none_or(|value| !value.eq_ignore_ascii_case("reasoning.encrypted_content"))
    });
    let changed = items.len() != before;
    if items.is_empty() {
        body.remove("include");
    }
    changed
}

fn sanitize_function_output(item: &mut Map<String, Value>, openai_family: bool) -> usize {
    let Some(encrypted) = item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return 0;
    };
    let valid = looks_like_encrypted_token(&encrypted);
    if openai_family && valid {
        return 0;
    }
    let has_output = item
        .get("output")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    if !has_output {
        let replacement = if valid {
            "[tool output unavailable after switching away from the encrypting model]".to_owned()
        } else {
            encrypted
        };
        item.insert("output".to_owned(), Value::String(replacement));
    }
    item.remove("encrypted_content");
    1
}

fn sanitize_reasoning(item: &mut Map<String, Value>, openai_family: bool) -> usize {
    let Some(encrypted) = item.get("encrypted_content").and_then(Value::as_str) else {
        return 0;
    };
    if openai_family && looks_like_encrypted_token(encrypted) {
        return 0;
    }
    item.remove("encrypted_content");
    1
}

fn sanitize_content_parts(item: &mut Map<String, Value>) -> usize {
    let Some(content) = item.get_mut("content").and_then(Value::as_array_mut) else {
        return 0;
    };
    let mut converted = 0;
    for part in content.iter_mut() {
        let Some(part) = part.as_object_mut() else {
            continue;
        };
        let kind = part
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(
            kind.as_str(),
            "" | "input_text"
                | "output_text"
                | "text"
                | "input_image"
                | "output_image"
                | "image_url"
        ) {
            continue;
        }
        let text = part
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("[{kind} omitted]"));
        part.clear();
        part.insert("type".to_owned(), Value::String("input_text".to_owned()));
        part.insert("text".to_owned(), Value::String(text));
        converted += 1;
    }
    converted
}

/// Convert Codex multi-agent v2 `agent_message` into a regular user message
/// before third-party sanitization. Codex stores the delegated task text in an
/// `encrypted_content` content part; for non-OpenAI providers it is plaintext
/// compatibility content, not an OpenAI encrypted reasoning token.
fn convert_agent_message(item: &mut Map<String, Value>) -> bool {
    if item_type(&Value::Object(item.clone())) != "agent_message" {
        return false;
    }
    item.insert("type".to_owned(), Value::String("message".to_owned()));
    item.insert("role".to_owned(), Value::String("user".to_owned()));
    if let Some(parts) = item.get_mut("content").and_then(Value::as_array_mut) {
        for part in parts.iter_mut().filter_map(Value::as_object_mut) {
            if part.get("type").and_then(Value::as_str) != Some("encrypted_content") {
                continue;
            }
            let task = part
                .get("encrypted_content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            part.clear();
            part.insert("type".to_owned(), Value::String("input_text".to_owned()));
            part.insert("text".to_owned(), Value::String(task));
        }
    }
    true
}

fn remove_image_content_parts(item: &mut Map<String, Value>) -> usize {
    const NOTICE: &str = "[Image input omitted because the selected upstream route rejected image input. Inform the user that the image was not read, then continue the remaining task using the available text and tool history.]";
    let Some(content) = item.get_mut("content").and_then(Value::as_array_mut) else {
        return 0;
    };
    let mut removed = 0;
    let mut notice_present = content
        .iter()
        .any(|part| part.get("text").and_then(Value::as_str) == Some(NOTICE));
    let mut replacement = Vec::with_capacity(content.len());
    for part in content.drain(..) {
        let kind = part
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(kind.as_str(), "input_image" | "output_image" | "image_url") {
            removed += 1;
            if !notice_present {
                replacement.push(json!({"type": "input_text", "text": NOTICE}));
                notice_present = true;
            }
        } else {
            replacement.push(part);
        }
    }
    *content = replacement;
    removed
}

pub fn sanitize_responses_request_without_images(path: &str, body: &mut Value) -> SanitizeStats {
    let mut stats = sanitize_responses_request(path, body);
    if stats.openai_family {
        return stats;
    }
    if let Some(items) = body.get_mut("input").and_then(Value::as_array_mut) {
        for item in items.iter_mut().filter_map(Value::as_object_mut) {
            stats.converted_items += remove_image_content_parts(item);
        }
    }
    stats
}

fn sanitize_grok_item(mut item: Map<String, Value>) -> (Value, usize) {
    let kind = item_type(&Value::Object(item.clone()));
    match kind.as_str() {
        "message" => {
            let converted = sanitize_content_parts(&mut item);
            (Value::Object(item), converted)
        }
        "function_call" => {
            let mut converted = 0;
            if let Some(arguments) = item.get_mut("arguments") {
                if !arguments.is_string() {
                    *arguments = Value::String(arguments.to_string());
                    converted += 1;
                }
            }
            (Value::Object(item), converted)
        }
        "function_call_output" => {
            let mut converted = 0;
            if let Some(output) = item.get_mut("output") {
                if !output.is_string() {
                    *output = Value::String(output.to_string());
                    converted += 1;
                }
            }
            (Value::Object(item), converted)
        }
        _ => (message_from_text(summarize_item(&Value::Object(item))), 1),
    }
}

fn unsupported_third_party_item(kind: &str) -> bool {
    matches!(
        kind,
        "item_reference"
            | "compaction"
            | "compact"
            | "mcp_call"
            | "mcp_list_tools"
            | "mcp_approval_request"
            | "custom_tool_call"
            | "custom_tool_call_output"
            | "computer_call"
            | "computer_call_output"
            | "web_search_call"
            | "web_search_call_output"
            | "file_search_call"
            | "code_interpreter_call"
            | "image_generation_call"
    )
}

fn item_role(item: &Value) -> String {
    item.get("role")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn item_text(item: &Value) -> String {
    if let Some(text) = item.get("output").and_then(Value::as_str) {
        return text.to_owned();
    }
    if let Some(text) = item.get("text").and_then(Value::as_str) {
        return text.to_owned();
    }
    item.get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

fn is_function_output_item(item: &Value) -> bool {
    matches!(
        item_type(item).as_str(),
        "function_call_output"
            | "custom_tool_call_output"
            | "computer_call_output"
            | "local_shell_call_output"
            | "apply_patch_call_output"
    )
}

fn is_function_call_item(item: &Value) -> bool {
    matches!(
        item_type(item).as_str(),
        "function_call" | "custom_tool_call" | "local_shell_call" | "apply_patch_call"
    )
}

fn item_call_id(item: &Value) -> String {
    item.get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn shell_cmd_from(map: &Map<String, Value>) -> String {
    if let Some(cmd) = map.get("cmd").and_then(Value::as_str) {
        return cmd.to_owned();
    }
    if let Some(command) = map.get("command") {
        if let Some(text) = command.as_str() {
            return text.to_owned();
        }
        if let Some(parts) = command.as_array() {
            return parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    if let Some(action) = map.get("action") {
        if let Some(cmd) = action.get("cmd").and_then(Value::as_str) {
            return cmd.to_owned();
        }
        if let Some(command) = action.get("command") {
            if let Some(text) = command.as_str() {
                return text.to_owned();
            }
            if let Some(parts) = command.as_array() {
                return parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ");
            }
        }
    }
    if let Some(arguments) = map.get("arguments").and_then(Value::as_str) {
        if let Ok(parsed) = serde_json::from_str::<Value>(arguments) {
            if let Some(cmd) = parsed.get("cmd").and_then(Value::as_str) {
                return cmd.to_owned();
            }
        }
    }
    String::new()
}

fn convert_shell_protocol_item(map: &mut Map<String, Value>) -> bool {
    match item_type(&Value::Object(map.clone())).as_str() {
        "local_shell_call" => {
            let call_id = item_call_id(&Value::Object(map.clone()));
            let cmd = shell_cmd_from(map);
            map.insert("type".to_owned(), Value::String("function_call".to_owned()));
            map.insert("name".to_owned(), Value::String("exec_command".to_owned()));
            if !call_id.is_empty() {
                map.insert("call_id".to_owned(), Value::String(call_id));
            }
            let needs_args = map
                .get("arguments")
                .and_then(Value::as_str)
                .map(str::is_empty)
                .unwrap_or(true);
            if needs_args {
                map.insert(
                    "arguments".to_owned(),
                    Value::String(json!({ "cmd": cmd }).to_string()),
                );
            }
            true
        }
        "local_shell_call_output" => {
            map.insert(
                "type".to_owned(),
                Value::String("function_call_output".to_owned()),
            );
            match map.get("output") {
                Some(output) if output.is_string() => {}
                Some(output) => {
                    map.insert("output".to_owned(), Value::String(output.to_string()));
                }
                None => {
                    map.insert("output".to_owned(), Value::String(String::new()));
                }
            }
            true
        }
        "apply_patch_call" => {
            map.insert("type".to_owned(), Value::String("function_call".to_owned()));
            if map
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .is_empty()
            {
                map.insert("name".to_owned(), Value::String("apply_patch".to_owned()));
            }
            true
        }
        "apply_patch_call_output" => {
            map.insert(
                "type".to_owned(),
                Value::String("function_call_output".to_owned()),
            );
            true
        }
        _ => false,
    }
}

fn synthetic_tool_output(call_id: &str) -> Value {
    json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": "[tool result missing from history; continue the user task]",
    })
}

fn chat_tool_call_ids(message: &Value) -> Vec<String> {
    message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .filter_map(|call| call.get("id").and_then(Value::as_str))
                .filter(|id| !id.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn ensure_chat_tool_messages(body: &mut Map<String, Value>) -> usize {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return 0;
    };
    let mut inserted = 0;
    let mut index = 0;
    let mut pending = Vec::new();
    while index < messages.len() {
        let role = messages[index]
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("");
        if role == "assistant" {
            let ids = chat_tool_call_ids(&messages[index]);
            if !ids.is_empty() {
                pending = ids;
            }
            index += 1;
            continue;
        }
        if role == "tool" {
            let id = messages[index]
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            pending.retain(|seen| seen != &id);
            index += 1;
            continue;
        }
        if !pending.is_empty() {
            for call_id in pending.drain(..) {
                messages.insert(
                    index,
                    json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": "[tool result missing from history; continue the user task]",
                    }),
                );
                inserted += 1;
                index += 1;
            }
        }
        index += 1;
    }
    for call_id in pending {
        messages.push(json!({
            "role": "tool",
            "tool_call_id": call_id,
            "content": "[tool result missing from history; continue the user task]",
        }));
        inserted += 1;
    }
    inserted
}

fn ensure_tool_call_outputs(items: &mut Vec<Value>) -> usize {
    let mut inserted = 0;
    let mut index = 0;
    let mut pending = Vec::new();
    while index < items.len() {
        if is_function_call_item(&items[index]) {
            let call_id = item_call_id(&items[index]);
            if !call_id.is_empty() {
                pending.push(call_id);
            }
            index += 1;
            continue;
        }
        if is_function_output_item(&items[index]) {
            let call_id = item_call_id(&items[index]);
            pending.retain(|seen| seen != &call_id);
            index += 1;
            continue;
        }
        if !pending.is_empty() {
            for call_id in pending.drain(..) {
                items.insert(index, synthetic_tool_output(&call_id));
                inserted += 1;
                index += 1;
            }
        }
        index += 1;
    }
    for call_id in pending {
        items.push(synthetic_tool_output(&call_id));
        inserted += 1;
    }
    inserted
}

fn compact_split_at(items: &[Value], keep_last: usize) -> usize {
    if items.len() <= keep_last {
        return 0;
    }
    let mut split_at = items.len() - keep_last;
    while split_at > 0 && is_function_output_item(&items[split_at]) {
        split_at -= 1;
    }
    split_at
}

fn snippet(text: &str, limit: usize) -> String {
    let trimmed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.chars().count() <= limit {
        trimmed
    } else {
        format!(
            "{}…",
            trimmed
                .chars()
                .take(limit.saturating_sub(1))
                .collect::<String>()
        )
    }
}

fn extract_workspace_hint(text: &str) -> Option<String> {
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("cwd")
            || lower.contains("working directory")
            || lower.contains("current directory")
            || lower.contains("工作目录")
        {
            let hint = snippet(line.trim(), 160);
            if !hint.is_empty() {
                return Some(hint);
            }
        }
    }
    None
}

fn summarize_dropped_items(items: &[Value]) -> String {
    let mut user_goals = Vec::new();
    let mut tools = Vec::new();
    let mut workspace = None;
    for item in items {
        let kind = item_type(item);
        let text = item_text(item);
        if workspace.is_none() {
            workspace = extract_workspace_hint(&text);
        }
        if item_role(item) == "user" && !text.trim().is_empty() {
            if user_goals.len() < 3 {
                user_goals.push(snippet(&text, COMPACT_SNIPPET_CHARS));
            }
        } else if matches!(
            kind.as_str(),
            "function_call" | "custom_tool_call" | "computer_call" | "local_shell_call"
        ) {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(kind.as_str());
            if tools.len() < 8 && !tools.iter().any(|seen: &String| seen == name) {
                tools.push(name.to_owned());
            }
        }
    }
    let mut lines = vec![format!(
        "【本地压缩摘要】已折叠 {} 条较早记录。这不是任务结束，请立即继续完成压缩前未完成的用户任务。",
        items.len()
    )];
    if !user_goals.is_empty() {
        lines.push(format!("未完成任务：{}", user_goals.join(" / ")));
    }
    if !tools.is_empty() {
        lines.push(format!("已执行工具：{}", tools.join(", ")));
    }
    if let Some(workspace) = workspace {
        lines.push(format!("工作目录提示：{workspace}"));
    }
    lines.push(
        "只使用当前对话工作目录。不要读取 CodexRouter / Codex-Router 源码目录，除非它就是当前 cwd。默认用简体中文继续回复。"
            .to_owned(),
    );
    lines.join("\n")
}

fn compact_input(items: &mut Vec<Value>, keep_last: usize) -> bool {
    if items.len() <= keep_last + 1 {
        return false;
    }
    let split_at = compact_split_at(items, keep_last);
    if split_at == 0 {
        return false;
    }
    let kept = items.split_off(split_at);
    let notice = message_from_text(summarize_dropped_items(items));
    items.clear();
    items.push(notice);
    items.extend(kept);
    true
}

fn looks_like_compact_request(path: &str, body: &Value) -> bool {
    is_compact_path(path)
        || body
            .get("input")
            .and_then(Value::as_array)
            .is_some_and(|items| items.iter().any(|item| item_type(item) == "compaction"))
}

fn is_local_compact_id(value: &str) -> bool {
    value.trim().starts_with("cmp_local_")
}

fn input_looks_like_post_compact_replay(items: &[Value]) -> bool {
    items.iter().any(|item| {
        let kind = item_type(item);
        if matches!(kind.as_str(), "compaction" | "compact") {
            return true;
        }
        let text = item_text(item);
        text.contains("【本地压缩摘要】")
            || text.contains(
                "Another language model started to solve this problem and produced a summary",
            )
    })
}

fn strip_unusable_third_party_continuation(object: &mut Map<String, Value>) -> bool {
    let previous = object
        .get("previous_response_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let conversation = object
        .get("conversation_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let replay = object
        .get("input")
        .and_then(Value::as_array)
        .is_some_and(|items| input_looks_like_post_compact_replay(items));
    if !is_local_compact_id(previous) && !is_local_compact_id(conversation) && !replay {
        return false;
    }
    object.remove("previous_response_id");
    object.remove("conversation_id");
    object.remove("conversation");
    true
}

pub fn sanitize_responses_request(path: &str, body: &mut Value) -> SanitizeStats {
    let mut stats = SanitizeStats {
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        ..SanitizeStats::default()
    };
    stats.openai_family = is_openai_family_model(&stats.model);
    let grok = stats.model.to_ascii_lowercase().contains("grok");
    let Some(object) = body.as_object_mut() else {
        return stats;
    };

    if !stats.openai_family && strip_include_encrypted(object) {
        stats.stripped_encrypted += 1;
    }
    if !stats.openai_family && strip_unusable_third_party_continuation(object) {
        stats.converted_items += 1;
    }
    if !stats.openai_family {
        stats.rewritten_tool_calls += inject_third_party_identity(object);
        stats.rewritten_tool_calls += normalize_request_tools(object);
        if is_chat_completions_agent_model(&stats.model) {
            stats.rewritten_tool_calls += convert_native_shell_tools(object);
            stats.rewritten_tool_calls += simplify_chat_agent_tools(object);
        } else if tools_are_degraded(object) {
            stats.rewritten_tool_calls += simplify_chat_agent_tools(object);
        }
    }
    let compact_request = looks_like_compact_request(path, &Value::Object(object.clone()));

    if let Some(items) = object.get_mut("input").and_then(Value::as_array_mut) {
        let mut replacement = Vec::with_capacity(items.len());
        for item in items.drain(..) {
            let Some(mut map) = item.as_object().cloned() else {
                replacement.push(item);
                continue;
            };
            stats.stripped_encrypted += sanitize_function_output(&mut map, stats.openai_family);
            stats.stripped_encrypted += sanitize_reasoning(&mut map, stats.openai_family);
            if !stats.openai_family {
                if convert_agent_message(&mut map) {
                    stats.converted_items += 1;
                }
                if convert_shell_protocol_item(&mut map) {
                    stats.converted_items += 1;
                }
                if grok {
                    let (item, converted) = sanitize_grok_item(map);
                    stats.converted_items += converted;
                    replacement.push(item);
                    continue;
                }
                stats.converted_items += sanitize_content_parts(&mut map);
                let kind = item_type(&Value::Object(map.clone()));
                if unsupported_third_party_item(&kind) {
                    replacement.push(message_from_text(summarize_item(&Value::Object(map))));
                    stats.converted_items += 1;
                    continue;
                }
                let rewritten = rewrite_leaked_tools_in_item(&mut map);
                stats.rewritten_tool_calls += rewritten.len();
                replacement.push(Value::Object(map));
                replacement.extend(rewritten);
                continue;
            }
            replacement.push(Value::Object(map));
        }
        if !stats.openai_family && compact_request {
            stats.locally_compacted = compact_input(&mut replacement, LOCAL_COMPACT_KEEP_LAST);
        }
        if !stats.openai_family {
            stats.rewritten_tool_calls += ensure_tool_call_outputs(&mut replacement);
        }
        object.insert("input".to_owned(), Value::Array(replacement));
    }
    if !stats.openai_family {
        stats.rewritten_tool_calls += ensure_chat_tool_messages(object);
    }

    stats
}

pub fn sanitize_responses_request_aggressive(path: &str, body: &mut Value) -> SanitizeStats {
    let mut stats = sanitize_responses_request(path, body);
    if stats.openai_family {
        return stats;
    }
    if let Some(items) = body.get_mut("input").and_then(Value::as_array_mut) {
        if compact_input(items, AGGRESSIVE_COMPACT_KEEP_LAST) {
            stats.locally_compacted = true;
        }
    }
    stats
}

pub fn synthetic_compact_response(model: &str, compacted_input: &[Value]) -> Value {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let summary = compacted_input
        .iter()
        .filter_map(|item| {
            item.get("content")
                .and_then(Value::as_array)
                .and_then(|parts| {
                    parts.iter().find_map(|part| {
                        part.get("text").and_then(Value::as_str).map(str::to_owned)
                    })
                })
                .or_else(|| {
                    item.get("output")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
        })
        .take(6)
        .collect::<Vec<_>>()
        .join("\n");
    let reminder = continue_after_local_compact_instructions();
    let mut output = compacted_input.to_vec();
    output.push(message_from_text(reminder.to_owned()));
    json!({
        "id": format!("cmp_local_{now}"),
        "object": "response.compaction",
        "created_at": now,
        "model": model,
        "output": output,
        "status": "completed",
        "text": format!("{summary}\n{reminder}"),
    })
}

const LEAKED_TOOL_MARKERS: &[&str] = &[
    "functions__exec",
    "functions.exec",
    "functions__shell",
    "functions.shell",
    "<|tool_call|>",
    "<|tool_calls_section_begin|>",
];

fn find_leaked_tool_start(text: &str) -> Option<usize> {
    LEAKED_TOOL_MARKERS
        .iter()
        .filter_map(|marker| text.find(marker))
        .min()
}

fn skip_leaked_prefix(text: &str) -> Option<usize> {
    for marker in LEAKED_TOOL_MARKERS {
        if let Some(start) = text.find(marker) {
            let mut index = start + marker.len();
            let bytes = text.as_bytes();
            if index < bytes.len() && bytes[index] == b':' {
                index += 1;
                while index < bytes.len() && bytes[index].is_ascii_digit() {
                    index += 1;
                }
            }
            return Some(index);
        }
    }
    None
}

fn take_balanced_json(text: &str) -> Option<(Value, usize)> {
    let start = text.find('{')?;
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in text[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let end = start + offset + 1;
                    let value = serde_json::from_str(&text[start..end]).ok()?;
                    return Some((value, end));
                }
            }
            _ => {}
        }
    }
    None
}

pub fn remap_tool_name(name: &str) -> String {
    let trimmed = name
        .trim()
        .trim_start_matches("functions__")
        .trim_start_matches("functions.");
    match trimmed {
        "exec" | "shell" | "bash" | "command" | "_command" | "exec_command" => {
            "exec_command".to_owned()
        }
        other => other.to_owned(),
    }
}

fn remap_tool_name_value(value: &mut Value) -> bool {
    let Some(name) = value.as_str().map(str::to_owned) else {
        return false;
    };
    let remapped = remap_tool_name(&name);
    if remapped == name {
        return false;
    }
    *value = Value::String(remapped);
    true
}

fn normalize_request_tools(body: &mut Map<String, Value>) -> usize {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return 0;
    };
    let mut changed = 0;
    for tool in tools.iter_mut() {
        let Some(tool) = tool.as_object_mut() else {
            continue;
        };
        if let Some(name) = tool.get_mut("name") {
            if remap_tool_name_value(name) {
                changed += 1;
            }
        }
        if let Some(function) = tool.get_mut("function").and_then(Value::as_object_mut) {
            if let Some(name) = function.get_mut("name") {
                if remap_tool_name_value(name) {
                    changed += 1;
                }
            }
        }
    }
    changed
}

fn tool_declared_name(tool: &Value) -> String {
    tool.get("name")
        .and_then(Value::as_str)
        .or_else(|| tool.pointer("/function/name").and_then(Value::as_str))
        .unwrap_or("")
        .to_owned()
}

fn force_tool_name(tool: &mut Value, name: &str) {
    if let Some(object) = tool.as_object_mut() {
        object.insert("name".to_owned(), Value::String(name.to_owned()));
        if let Some(function) = object.get_mut("function").and_then(Value::as_object_mut) {
            function.insert("name".to_owned(), Value::String(name.to_owned()));
        }
    }
}

fn exec_command_function_tool() -> Value {
    json!({
        "type": "function",
        "name": "exec_command",
        "description": "Run a shell command in the current working directory.",
        "parameters": {
            "type": "object",
            "properties": {
                "cmd": { "type": "string", "description": "Command to run" },
                "workdir": { "type": "string" }
            },
            "required": ["cmd"]
        }
    })
}

fn inject_third_party_identity(body: &mut Map<String, Value>) -> usize {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let Some(clause) = third_party_identity_clause(&model) else {
        return 0;
    };
    let current = body
        .get("instructions")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if current.contains("# 模型身份") {
        return 0;
    }
    body.insert(
        "instructions".to_owned(),
        Value::String(format!("{clause}\n{current}")),
    );
    1
}

fn convert_native_shell_tools(body: &mut Map<String, Value>) -> usize {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return 0;
    };
    let mut changed = 0;
    for tool in tools.iter_mut() {
        let typ = tool.get("type").and_then(Value::as_str).unwrap_or("");
        let mapped = remap_tool_name(&tool_declared_name(tool));
        if matches!(typ, "local_shell" | "shell" | "shell_command")
            || (mapped == "exec_command" && typ != "function")
        {
            *tool = exec_command_function_tool();
            changed += 1;
        }
    }
    changed
}

fn tools_are_degraded(body: &Map<String, Value>) -> bool {
    let Some(tools) = body.get("tools").and_then(Value::as_array) else {
        return false;
    };
    if tools.is_empty() {
        return true;
    }
    tools.iter().all(|tool| {
        let typ = tool.get("type").and_then(Value::as_str).unwrap_or("");
        if matches!(typ, "local_shell" | "shell" | "shell_command") {
            return false;
        }
        matches!(
            remap_tool_name(&tool_declared_name(tool)).as_str(),
            "wait" | "request_user_input" | ""
        )
    })
}

fn simplify_chat_agent_tools(body: &mut Map<String, Value>) -> usize {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        body.insert("tools".to_owned(), json!([exec_command_function_tool()]));
        return 1;
    };
    let original_len = tools.len();
    let mut kept = Vec::new();
    for tool in tools.iter() {
        let mapped = remap_tool_name(&tool_declared_name(tool));
        if matches!(mapped.as_str(), "exec_command" | "shell_command" | "shell") {
            let mut kept_tool = tool.clone();
            force_tool_name(&mut kept_tool, "exec_command");
            kept.push(kept_tool);
        }
    }
    if kept.is_empty() {
        kept.push(exec_command_function_tool());
    }
    let changed = usize::from(kept.len() != original_len);
    *tools = kept;
    changed
}

pub fn rewrite_tool_names_in_json(value: &mut Value) -> usize {
    let mut changed = 0;
    match value {
        Value::Object(map) => {
            if let Some(name) = map.get_mut("name") {
                if remap_tool_name_value(name) {
                    changed += 1;
                }
            }
            for child in map.values_mut() {
                changed += rewrite_tool_names_in_json(child);
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                changed += rewrite_tool_names_in_json(item);
            }
        }
        _ => {}
    }
    changed
}

fn normalize_leaked_tool_call(raw: &Value, index: usize) -> Value {
    let name = raw
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("exec_command");
    let mapped_name = match name {
        "exec" | "functions__exec" | "shell" | "bash" | "command" => "exec_command",
        other => other,
    };
    let command = raw
        .get("cmd")
        .or_else(|| raw.get("command"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let arguments = if mapped_name == "exec_command" {
        let mut payload = serde_json::Map::new();
        payload.insert("cmd".to_owned(), Value::String(command.to_owned()));
        if let Some(timeout) = raw.get("timeout") {
            payload.insert("timeout".to_owned(), timeout.clone());
        }
        Value::Object(payload).to_string()
    } else {
        raw.to_string()
    };
    json!({
        "type": "function_call",
        "name": mapped_name,
        "call_id": format!("call_compat_{index}"),
        "arguments": arguments,
    })
}

pub fn extract_leaked_tool_calls(text: &str) -> (String, Vec<Value>) {
    let mut remaining = String::new();
    let mut tools = Vec::new();
    let mut rest = text;
    while let Some(start) = find_leaked_tool_start(rest) {
        remaining.push_str(&rest[..start]);
        let Some(prefix_end) = skip_leaked_prefix(&rest[start..]) else {
            remaining.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let after_prefix = &rest[start + prefix_end..];
        if let Some((raw, consumed)) = take_balanced_json(after_prefix) {
            tools.push(normalize_leaked_tool_call(&raw, tools.len() + 1));
            rest = &after_prefix[consumed..];
        } else {
            rest = after_prefix.trim_start();
        }
    }
    remaining.push_str(rest);
    (remaining, tools)
}

fn rewrite_text_value(value: &mut Value, tools: &mut Vec<Value>) -> bool {
    let Some(text) = value.as_str().map(str::to_owned) else {
        return false;
    };
    if find_leaked_tool_start(&text).is_none() {
        return false;
    }
    let (cleaned, extracted) = extract_leaked_tool_calls(&text);
    *value = Value::String(cleaned);
    tools.extend(extracted);
    true
}

fn rewrite_leaked_tools_in_item(item: &mut Map<String, Value>) -> Vec<Value> {
    let kind = item_type(&Value::Object(item.clone()));
    if is_function_call_item(&Value::Object(item.clone()))
        || is_function_output_item(&Value::Object(item.clone()))
        || matches!(
            kind.as_str(),
            "local_shell_call" | "local_shell_call_output"
        )
    {
        return Vec::new();
    }
    let mut tools = Vec::new();
    if let Some(content) = item.get_mut("content").and_then(Value::as_array_mut) {
        for part in content.iter_mut() {
            if let Some(part) = part.as_object_mut() {
                if let Some(text) = part.get_mut("text") {
                    rewrite_text_value(text, &mut tools);
                }
            }
        }
    }
    if let Some(text) = item.get_mut("text") {
        rewrite_text_value(text, &mut tools);
    }
    if let Some(output) = item.get_mut("output") {
        rewrite_text_value(output, &mut tools);
    }
    tools
}

pub fn rewrite_provider_json(body: &mut Value) -> usize {
    let mut extracted = Vec::new();
    if let Some(output) = body.get_mut("output").and_then(Value::as_array_mut) {
        for item in output.iter_mut() {
            if let Some(map) = item.as_object_mut() {
                extracted.extend(rewrite_leaked_tools_in_item(map));
            }
        }
        output.extend(extracted.iter().cloned());
    }
    if let Some(choices) = body.get_mut("choices").and_then(Value::as_array_mut) {
        for choice in choices.iter_mut() {
            if let Some(message) = choice.get_mut("message").and_then(Value::as_object_mut) {
                if let Some(content) = message.get_mut("content") {
                    let mut tools = Vec::new();
                    rewrite_text_value(content, &mut tools);
                    if !tools.is_empty() {
                        let tool_calls = tools
                            .iter()
                            .map(|tool| {
                                json!({
                                    "id": tool.get("call_id").cloned().unwrap_or(Value::String("call_compat".to_owned())),
                                    "type": "function",
                                    "function": {
                                        "name": tool.get("name").cloned().unwrap_or(Value::String("exec_command".to_owned())),
                                        "arguments": tool.get("arguments").cloned().unwrap_or(Value::String("{}".to_owned())),
                                    }
                                })
                            })
                            .collect::<Vec<_>>();
                        message.insert("tool_calls".to_owned(), Value::Array(tool_calls));
                        extracted.extend(tools);
                    }
                }
            }
        }
    }
    extracted.len()
}

#[allow(dead_code)]
pub fn rewrite_sse_text(chunk: &str) -> String {
    let mut rewritten = String::with_capacity(chunk.len());
    let mut rest = chunk;
    while let Some(end) = rest.find("\n\n") {
        let (event, tail) = rest.split_at(end + 2);
        rewritten.push_str(&rewrite_sse_event(event));
        rest = tail;
    }
    rewritten.push_str(&rewrite_sse_event(rest));
    rewritten
}

#[allow(dead_code)]
fn rewrite_sse_event(event: &str) -> String {
    let has_leak = find_leaked_tool_start(event).is_some();
    let has_think = event.to_ascii_lowercase().contains("<think>")
        || event.to_ascii_lowercase().contains("<thinking>");
    if !has_leak && !has_think {
        return event.to_owned();
    }
    let mut out = String::new();
    let mut extracted_tools = Vec::new();
    for line in event.split_inclusive('\n') {
        if let Some(data) = line.strip_prefix("data:") {
            let trimmed = data.trim();
            if let Ok(mut json) = serde_json::from_str::<Value>(trimmed) {
                rewrite_provider_json(&mut json);
                rewrite_tool_names_in_json(&mut json);
                strip_think_tags_from_value(&mut json);
                if let Some(delta) = json.pointer_mut("/delta") {
                    rewrite_text_value(delta, &mut extracted_tools);
                }
                if let Some(text) = json.pointer_mut("/item/content/0/text") {
                    rewrite_text_value(text, &mut extracted_tools);
                }
                out.push_str("data: ");
                out.push_str(&json.to_string());
                if line.ends_with('\n') {
                    out.push('\n');
                }
                continue;
            }
            let (mut cleaned, tools) = extract_leaked_tool_calls(data);
            cleaned = strip_think_tags(&cleaned);
            extracted_tools.extend(tools);
            out.push_str("data:");
            out.push_str(&cleaned);
            if line.ends_with('\n') && !cleaned.ends_with('\n') {
                out.push('\n');
            }
        } else {
            out.push_str(line);
        }
    }
    for (index, tool) in extracted_tools.into_iter().enumerate() {
        let output_index = index as u64;
        out.push_str("data: ");
        out.push_str(
            &json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": tool.clone(),
            })
            .to_string(),
        );
        out.push_str("\n\ndata: ");
        out.push_str(
            &json!({
                "type": "response.output_item.done",
                "output_index": output_index,
                "item": tool,
            })
            .to_string(),
        );
        out.push_str("\n\n");
    }
    out
}

pub fn continue_after_local_compact_instructions() -> &'static str {
    "压缩已完成。请立即继续执行压缩前未完成的用户任务，不要停止。默认使用简体中文。只使用当前对话工作目录，不要读取 CodexRouter 源码目录，除非它就是当前 cwd。"
}

pub fn strip_think_tags(text: &str) -> String {
    // Remove provider reasoning blocks case-insensitively.
    // Unclosed tags at the end of a truncated provider stream still drop
    // the remainder. Tags wrapped in Markdown inline/fenced code are left
    // intact so a documentation example cannot swallow a completed answer.
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    while pos < text.len() {
        let rem_lower = &lower[pos..];
        let think_pos = rem_lower.find("<think>");
        let thinking_pos = rem_lower.find("<thinking>");
        let (rel, open_len, close_tag) = match (think_pos, thinking_pos) {
            (Some(a), Some(b)) => {
                if a < b {
                    (a, 7, "</think>")
                } else {
                    (b, 10, "</thinking>")
                }
            }
            (Some(a), None) => (a, 7, "</think>"),
            (None, Some(b)) => (b, 10, "</thinking>"),
            (None, None) => {
                out.push_str(&text[pos..]);
                break;
            }
        };
        let abs_start = pos + rel;
        let after_open = abs_start + open_len;
        if think_tag_is_literal(text, abs_start, open_len) {
            out.push_str(&text[pos..after_open.min(text.len())]);
            pos = after_open.min(text.len());
            continue;
        }
        out.push_str(&text[pos..abs_start]);
        if after_open > text.len() {
            break;
        }
        let after_lower = &lower[after_open..];
        if let Some(end_rel) = after_lower.find(close_tag) {
            pos = after_open + end_rel + close_tag.len();
        } else {
            // No closing tag: truncated provider stream, drop remainder.
            break;
        }
    }
    out
}

fn think_tag_is_literal(text: &str, tag_start: usize, open_len: usize) -> bool {
    let bytes = text.as_bytes();
    if tag_start > 0 && bytes[tag_start - 1] == b'`' {
        return true;
    }
    let after = tag_start + open_len;
    if after < bytes.len() && bytes[after] == b'`' {
        return true;
    }
    inside_fenced_code_block(text, tag_start)
}

fn inside_fenced_code_block(text: &str, index: usize) -> bool {
    let mut in_fence = false;
    for line in text[..index].split('\n') {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
    }
    in_fence
}

pub fn strip_think_tags_from_value(value: &mut Value) -> bool {
    match value {
        Value::String(s) => {
            let stripped = strip_think_tags(s);
            if stripped != *s {
                *s = stripped;
                true
            } else {
                false
            }
        }
        Value::Array(arr) => {
            let mut changed = false;
            for v in arr.iter_mut() {
                if strip_think_tags_from_value(v) {
                    changed = true;
                }
            }
            changed
        }
        Value::Object(map) => {
            let mut changed = false;
            for v in map.values_mut() {
                if strip_think_tags_from_value(v) {
                    changed = true;
                }
            }
            changed
        }
        _ => false,
    }
}

pub fn should_retry_after_upstream_error(status: u16, body: &str) -> bool {
    if !matches!(status, 400 | 422 | 500 | 502 | 503) {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    lower.contains("modelinput")
        || lower.contains("encrypted function output")
        || lower.contains("encrypted content")
        || lower.contains("invalid_encrypted_content")
        || lower.contains("could not be decrypted")
        || lower.contains("invalid-argument")
        || lower.contains("invalid_argument")
        || lower.contains("previous_response")
        || is_unsupported_image_error(body)
}

pub fn is_unsupported_image_error(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("does not support image input") || lower.contains("unsupported image input")
}

pub fn rewrite_poisoned_upstream_status(status: u16, body: &str) -> u16 {
    if should_retry_after_upstream_error(status, body) && matches!(status, 500 | 502 | 503) {
        400
    } else {
        rewrite_exhausted_account_status(status, body)
    }
}

pub fn rewrite_exhausted_account_status(status: u16, body: &str) -> u16 {
    if is_exhausted_account_status(status, body) {
        429
    } else {
        status
    }
}

/// Sub2API reports an account pool drained by upstream rate limiting as 503.
/// Detecting it here lets the gateway retry it like a literal 429 instead of
/// passing it straight through and ending the conversation.
pub fn is_exhausted_account_status(status: u16, body: &str) -> bool {
    if status != 503 {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    lower.contains("no available accounts")
        || lower.contains("auth_unavailable")
        || lower.contains("no auth available")
        || lower.contains("too many requests")
        || lower.contains("rate_limit")
        || lower.contains("rate limit")
        || lower.contains("service temporarily unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_family_detection_covers_chatgpt_slugs() {
        assert!(is_openai_family_model("gpt-5.6-sol"));
        assert!(is_openai_family_model("~gpt-5.6-luna"));
        assert!(is_openai_family_model("openai/codex-mini-latest"));
        assert!(!is_openai_family_model("grok-4.6"));
        assert!(!is_openai_family_model("claude-fable-5"));
        assert!(!is_openai_family_model("gemini-3.1-pro-high"));
    }

    #[test]
    fn plaintext_is_not_treated_as_encrypted_token() {
        assert!(looks_like_encrypted_token(
            "gAAAAABntotallyopaqueencryptedpayloadfromopenai000000000000"
        ));
        assert!(!looks_like_encrypted_token(
            "Child finished. The repository now contains the requested patch."
        ));
        assert!(!looks_like_encrypted_token("short"));
    }

    #[test]
    fn grok_request_strips_encrypted_reasoning_and_unsupported_items() {
        let mut body = json!({
            "model": "grok-4.6",
            "include": ["reasoning.encrypted_content", "file_search_call.results"],
            "input": [
                {
                    "type": "reasoning",
                    "encrypted_content": "gAAAAABnotforgrok0000000000000000000000000",
                    "summary": [{"type": "summary_text", "text": "planned the edit"}]
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "encrypted_content": "the tool printed hello"
                },
                {
                    "type": "mcp_call",
                    "id": "mcp_1",
                    "output": "browser snapshot"
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "continue"}]
                }
            ]
        });
        let stats = sanitize_responses_request("/v1/responses", &mut body);
        assert!(!stats.openai_family);
        assert!(stats.stripped_encrypted >= 2);
        assert!(stats.converted_items >= 1);
        assert_eq!(body["include"], json!(["file_search_call.results"]));
        assert!(body["input"][0].get("encrypted_content").is_none());
        assert_eq!(body["input"][0]["type"], "message");
        assert_eq!(body["input"][1]["output"], "the tool printed hello");
        assert_eq!(body["input"][2]["type"], "message");
        assert_eq!(body["input"][3]["content"][0]["text"], "continue");
    }

    #[test]
    fn grok_request_canonicalizes_cross_account_history() {
        let mut body = json!({
            "model": "grok-4.6",
            "input": [
                {"type":"reasoning","summary":[{"type":"summary_text","text":"plan"}]},
                {"type":"function_call","name":"exec_command","call_id":"c1","arguments":{"cmd":"pwd"}},
                {"type":"function_call_output","call_id":"c1","output":{"stdout":"ok"}},
                {"type":"unknown_future_item","text":"continue safely"}
            ]
        });

        let stats = sanitize_responses_request("/v1/responses", &mut body);
        assert!(stats.converted_items >= 4);
        assert_eq!(body["input"][0]["type"], "message");
        assert!(body["input"][1]["arguments"].is_string());
        assert!(body["input"][2]["output"].is_string());
        assert_eq!(body["input"][3]["type"], "message");
    }

    #[test]
    fn gemini_keeps_failed_tool_output_paired_for_continuation() {
        let mut body = json!({
            "model": "gemini-3.1-pro-high",
            "input": [
                {"type":"function_call","name":"exec_command","call_id":"call_harnss","arguments":"{\"cmd\":\"gh api repos/OpenSource03/harnss/readme\"}"},
                {"type":"function_call_output","call_id":"call_harnss","output":"ParserError: Missing ')' in method call. Exit code 1."}
            ]
        });
        sanitize_responses_request("/v1/responses", &mut body);
        assert_eq!(body["input"][0]["call_id"], "call_harnss");
        assert_eq!(body["input"][1]["call_id"], "call_harnss");
        assert!(body["input"][1]["output"]
            .as_str()
            .unwrap()
            .contains("ParserError"));
    }

    #[test]
    fn grok_image_fallback_preserves_text_and_tool_history() {
        let mut body = json!({
            "model": "grok-4.6",
            "input": [
                {"type":"function_call","name":"exec_command","call_id":"c1","arguments":"{\"cmd\":\"pwd\"}"},
                {"type":"function_call_output","call_id":"c1","output":"ok"},
                {"type":"message","role":"user","content":[
                    {"type":"input_text","text":"before"},
                    {"type":"input_image","image_url":"data:image/png;base64,secret"},
                    {"type":"input_text","text":"after"}
                ]}
            ]
        });
        let stats = sanitize_responses_request_without_images("/v1/responses", &mut body);
        assert_eq!(body["input"][0]["call_id"], "c1");
        assert_eq!(body["input"][1]["call_id"], "c1");
        let serialized = body.to_string();
        assert!(!serialized.contains("data:image"));
        assert!(serialized.contains("before"));
        assert!(serialized.contains("after"));
        assert!(serialized.contains("image was not read"));
        assert!(stats.converted_items >= 1);
        let once = body.clone();
        sanitize_responses_request_without_images("/v1/responses", &mut body);
        assert_eq!(body, once);
        assert!(is_unsupported_image_error(
            "Cannot read image.png (this model does not support image input). Inform the user."
        ));
    }

    #[test]
    fn chatgpt_request_repairs_plaintext_labeled_as_encrypted() {
        let mut body = json!({
            "model": "gpt-5.6-luna",
            "input": [
                {
                    "type": "function_call_output",
                    "call_id": "call_agent",
                    "encrypted_content": "SPAWNED=1 ERRORS=none"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_keep",
                    "encrypted_content": "gAAAAABvalidopenaiencryptedfunctionoutput000000"
                }
            ]
        });
        let stats = sanitize_responses_request("/v1/responses", &mut body);
        assert!(stats.openai_family);
        assert_eq!(stats.stripped_encrypted, 1);
        assert_eq!(body["input"][0]["output"], "SPAWNED=1 ERRORS=none");
        assert!(body["input"][0].get("encrypted_content").is_none());
        assert_eq!(
            body["input"][1]["encrypted_content"],
            "gAAAAABvalidopenaiencryptedfunctionoutput000000"
        );
    }

    #[test]
    fn regular_third_party_history_is_not_auto_compacted() {
        let mut input = Vec::new();
        for index in 0..120 {
            input.push(json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": format!("turn {index} cwd=D:/demo")}]
            }));
        }
        let mut body = json!({ "model": "grok-4.6", "input": input });
        let stats = sanitize_responses_request("/v1/responses", &mut body);
        assert!(!stats.locally_compacted);
        assert_eq!(body["input"].as_array().unwrap().len(), 120);
    }

    #[test]
    fn explicit_compact_path_summarizes_third_party_history() {
        let mut input = Vec::new();
        for index in 0..120 {
            input.push(json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": format!("turn {index} cwd=D:/demo")}]
            }));
        }
        let mut body = json!({ "model": "grok-4.6", "input": input });
        let stats = sanitize_responses_request("/v1/responses/compact", &mut body);
        assert!(stats.locally_compacted);
        let items = body["input"].as_array().unwrap();
        assert!(items.len() <= LOCAL_COMPACT_KEEP_LAST + 1);
        let notice = items[0]["content"][0]["text"].as_str().unwrap();
        assert!(notice.contains("本地压缩摘要"));
        assert!(notice.contains("这不是任务结束"));
        assert!(notice.contains("未完成任务"));
    }

    #[test]
    fn grok_reconnect_after_local_compact_drops_synthetic_continuation() {
        let mut body = json!({
            "model": "grok-4.6",
            "previous_response_id": "cmp_local_1710000000",
            "conversation_id": "cmp_local_1710000000",
            "conversation": {"id": "cmp_local_1710000000"},
            "input": [
                {"type":"message","role":"user","content":[{"type":"input_text","text":"继续"}]}
            ]
        });
        sanitize_responses_request("/v1/responses", &mut body);
        assert!(body.get("previous_response_id").is_none());
        assert!(body.get("conversation_id").is_none());
        assert!(body.get("conversation").is_none());
        assert_eq!(body["input"][0]["role"], "user");
    }

    #[test]
    fn grok_post_compact_transcript_drops_real_previous_response() {
        let mut body = json!({
            "model": "grok-4.6",
            "previous_response_id": "resp_grok_real",
            "input": [
                {"type":"message","role":"user","content":[{"type":"input_text","text":"【本地压缩摘要】已折叠 80 条较早记录。"}]},
                {"type":"message","role":"user","content":[{"type":"input_text","text":"继续"}]}
            ]
        });
        sanitize_responses_request("/v1/responses", &mut body);
        assert!(body.get("previous_response_id").is_none());
        assert_eq!(body["input"].as_array().unwrap().len(), 2);
        assert!(body["input"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("本地压缩摘要"));
    }

    #[test]
    fn grok_keeps_same_thread_previous_response_without_compact() {
        let mut body = json!({
            "model": "grok-4.6",
            "previous_response_id": "resp_grok_real",
            "input": [
                {"type":"message","role":"user","content":[{"type":"input_text","text":"下一问"}]}
            ]
        });
        sanitize_responses_request("/v1/responses", &mut body);
        assert_eq!(body["previous_response_id"], "resp_grok_real");
    }

    #[test]
    fn compact_keeps_function_output_paired_with_its_call() {
        let mut items = vec![
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"old"}]}),
        ];
        for index in 0..20 {
            items.push(
                json!({"type":"function_call","name":"exec_command","call_id":format!("c{index}")}),
            );
            items.push(
                json!({"type":"function_call_output","call_id":format!("c{index}"),"output":"ok"}),
            );
        }
        assert!(compact_input(&mut items, 5));
        assert!(!is_function_output_item(&items[1]));
    }

    #[test]
    fn kimi_leaked_exec_is_converted_to_exec_command() {
        let (cleaned, tools) = extract_leaked_tool_calls(
            "Wait, I keep doing wait!\nfunctions__exec:8{\"command\": \"Get-Location\", \"timeout\": 30000}\n",
        );
        assert!(!cleaned.contains("functions__exec"));
        assert_eq!(tools.len(), 1);
        assert_eq!(remap_tool_name("functions__exec_command"), "exec_command");
        assert_eq!(remap_tool_name("functions__exec"), "exec_command");
        assert_eq!(remap_tool_name("_command"), "exec_command");
        assert_eq!(tools[0]["name"], "exec_command");
        assert!(tools[0]["arguments"].as_str().unwrap().contains("\"cmd\""));
        assert!(tools[0]["arguments"]
            .as_str()
            .unwrap()
            .contains("Get-Location"));
    }

    #[test]
    fn kimi_request_keeps_only_exec_command_tool() {
        let mut body = json!({
            "model": "kimi-for-coding",
            "tools": [
                {"type":"function","name":"functions__wait","parameters":{"type":"object","properties":{}}},
                {"type":"function","name":"functions__request_user_input","parameters":{"type":"object","properties":{}}},
                {"type":"function","name":"functions__exec","parameters":{"type":"object","properties":{"command":{"type":"string"}}}}
            ]
        });
        let stats = sanitize_responses_request("/v1/responses", &mut body);
        assert!(stats.rewritten_tool_calls >= 1);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "exec_command");
        assert!(body["instructions"]
            .as_str()
            .unwrap()
            .starts_with("# 模型身份"));
        assert!(body["instructions"].as_str().unwrap().contains("你是Kimi"));
        assert!(body["instructions"]
            .as_str()
            .unwrap()
            .contains(&format!("Agent_Test_{}_kimi.md", env!("CARGO_PKG_VERSION"))));
    }

    #[test]
    fn kimi_local_shell_becomes_exec_command_function() {
        let mut body = json!({
            "model": "k3-256k",
            "instructions": "You are Codex, an agent based on GPT-5.",
            "tools": [{"type":"local_shell"}]
        });
        sanitize_responses_request("/v1/responses", &mut body);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "exec_command");
        assert!(body["instructions"].as_str().unwrap().contains("你是Kimi"));
        assert!(!body["instructions"]
            .as_str()
            .unwrap()
            .starts_with("You are Codex"));
    }

    #[test]
    fn provider_json_rewrites_chat_completion_tool_leak() {
        let mut body = json!({
            "choices": [{
                "message": {
                    "content": "functions__exec:1{\"command\":\"pwd\"}"
                }
            }]
        });
        assert_eq!(rewrite_provider_json(&mut body), 1);
        assert_eq!(
            body["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "exec_command"
        );
        let sse = rewrite_sse_text(
            "data: {\"delta\":\"functions__exec:1{\\\"command\\\":\\\"pwd\\\"}\"}\n\n",
        );
        assert!(!sse.contains("functions__exec"));
        assert!(sse.contains("response.output_item.added"));
        assert!(sse.contains("response.output_item.done"));
        assert!(sse.contains("exec_command"));
        assert!(sse.contains("\\\"cmd\\\":\\\"pwd\\\""));
    }

    #[test]
    fn local_shell_history_becomes_paired_exec_command() {
        let mut body = json!({
            "model": "grok-4.6",
            "input": [
                {
                    "type": "local_shell_call",
                    "call_id": "exec_command:0",
                    "action": {"type": "exec", "command": ["Get-Location"]}
                },
                {
                    "type": "local_shell_call_output",
                    "call_id": "exec_command:0",
                    "output": "D:\\temp"
                }
            ]
        });
        sanitize_responses_request("/v1/responses", &mut body);
        assert_eq!(body["input"][0]["type"], "function_call");
        assert_eq!(body["input"][0]["name"], "exec_command");
        assert_eq!(body["input"][0]["call_id"], "exec_command:0");
        assert!(body["input"][0]["arguments"]
            .as_str()
            .unwrap()
            .contains("Get-Location"));
        assert_eq!(body["input"][1]["type"], "function_call_output");
        assert_eq!(body["input"][1]["call_id"], "exec_command:0");
        assert_eq!(body["input"][1]["output"], "D:\\temp");
    }

    #[test]
    fn unpaired_exec_command_call_gets_synthetic_output_before_next_user() {
        let mut body = json!({
            "model": "k3-256k",
            "input": [
                {"type":"function_call","name":"exec_command","call_id":"exec_command:0","arguments":"{\"cmd\":\"ls\"}"},
                {"type":"message","role":"user","content":[{"type":"input_text","text":"继续"}]}
            ]
        });
        sanitize_responses_request("/v1/responses", &mut body);
        let items = body["input"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["call_id"], "exec_command:0");
        assert_eq!(items[1]["type"], "function_call_output");
        assert_eq!(items[1]["call_id"], "exec_command:0");
        assert_eq!(items[2]["role"], "user");
    }

    #[test]
    fn chat_tool_calls_without_tool_message_are_paired() {
        let mut body = json!({
            "model": "glm-latest",
            "messages": [
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "exec_command:0",
                        "type": "function",
                        "function": {"name": "exec_command", "arguments": "{\"cmd\":\"ls\"}"}
                    }]
                },
                {"role": "user", "content": "继续"}
            ]
        });
        sanitize_responses_request("/v1/chat/completions", &mut body);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[1]["tool_call_id"], "exec_command:0");
        assert_eq!(messages[2]["role"], "user");
    }

    #[test]
    fn leaked_tool_text_inside_function_output_does_not_create_unpaired_call() {
        let mut body = json!({
            "model": "kimi-for-coding",
            "input": [
                {"type":"function_call","name":"exec_command","call_id":"exec_command:0","arguments":"{\"cmd\":\"ls\"}"},
                {"type":"function_call_output","call_id":"exec_command:0","output":"functions__exec:8{\"command\":\"pwd\"}"}
            ]
        });
        sanitize_responses_request("/v1/responses", &mut body);
        let items = body["input"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["type"], "function_call");
        assert_eq!(items[1]["type"], "function_call_output");
        assert_eq!(items[1]["call_id"], "exec_command:0");
    }

    #[test]
    fn regular_responses_do_not_compact_just_because_instructions_mention_summary() {
        let mut body = json!({
            "model": "gemini-3.7-flash",
            "instructions": "summarize the conversation so far if needed, then continue the task",
            "input": [
                {"type":"message","role":"user","content":[{"type":"input_text","text":"continue"}]}
            ]
        });
        let stats = sanitize_responses_request("/v1/responses", &mut body);
        assert!(!stats.locally_compacted);
    }

    #[test]
    fn think_tags_are_stripped_from_deepseek_text() {
        assert_eq!(
            strip_think_tags("hello <think>internal reasoning</think> world"),
            "hello  world"
        );
        assert_eq!(
            strip_think_tags("start <THINK>Using direct collaboration tool</THINK> end"),
            "start  end"
        );
        assert_eq!(strip_think_tags("prefix <think>truncated"), "prefix ");
        assert_eq!(strip_think_tags("no tags here"), "no tags here");
        let mut v = json!({"delta": "<think>secret</think> hello"});
        assert!(strip_think_tags_from_value(&mut v));
        assert_eq!(v["delta"], " hello");
        let inline = format!(
            "SSE rewrite only on leaked tool or `{tag}` then more",
            tag = "<think>"
        );
        assert_eq!(strip_think_tags(&inline), inline);
        let fenced = format!("before\n```\n{tag} raw\n```\nafter", tag = "<think>");
        assert_eq!(strip_think_tags(&fenced), fenced);
        let table = format!(
            "| Router empty delta | no. `{tag}` then more |",
            tag = "<think>"
        );
        assert_eq!(strip_think_tags(&table), table);
    }

    #[test]
    fn deepseek_v2_agent_message_preserves_delegated_task() {
        let task = "调用 exec_command 运行 Get-Date，然后逐字返回结果。";
        let mut body = json!({
            "model": "deepseek-v4-flash",
            "instructions": "instruction sentinel",
            "previous_response_id": "resp_parent",
            "input": [{
                "type": "agent_message",
                "id": "amsg_1",
                "author": "/root",
                "recipient": "/root/worker",
                "content": [
                    {"type":"input_text","text":"Message Type: NEW_TASK\nTask name: /root/worker\nPayload:\n"},
                    {"type":"encrypted_content","encrypted_content":task}
                ],
                "internal_chat_message_metadata_passthrough": {"turn_id":"turn_child"}
            }]
        });
        let stats = sanitize_responses_request("/v1/responses", &mut body);
        assert!(stats.converted_items >= 1);
        let item = &body["input"][0];
        assert_eq!(item["type"], "message");
        assert_eq!(item["role"], "user");
        assert_eq!(item["id"], "amsg_1");
        assert_eq!(item["author"], "/root");
        assert_eq!(item["recipient"], "/root/worker");
        assert_eq!(
            item["internal_chat_message_metadata_passthrough"]["turn_id"],
            "turn_child"
        );
        assert_eq!(item["content"][1]["type"], "input_text");
        assert_eq!(item["content"][1]["text"], task);
        assert!(item["content"][1].get("encrypted_content").is_none());
        let encoded = body.to_string();
        assert!(!encoded.contains("[agent_message"));
        assert!(!encoded.contains("[encrypted_content omitted]"));
        assert!(body["instructions"]
            .as_str()
            .unwrap()
            .contains("instruction sentinel"));
    }

    #[test]
    fn third_party_agent_message_matrix_keeps_task() {
        for model in ["deepseek-v4-flash", "kimi-for-coding", "grok-4.6"] {
            let mut body = json!({
                "model": model,
                "input": [{
                    "type":"agent_message",
                    "author":"/root",
                    "recipient":"/root/child",
                    "content":[
                        {"type":"input_text","text":"Message Type: NEW_TASK\nPayload:\n"},
                        {"type":"encrypted_content","encrypted_content":"TASK-SENTINEL"}
                    ]
                }]
            });
            sanitize_responses_request("/v1/responses", &mut body);
            assert_eq!(body["input"][0]["type"], "message", "{model}");
            assert_eq!(body["input"][0]["role"], "user", "{model}");
            assert_eq!(
                body["input"][0]["content"][1]["text"], "TASK-SENTINEL",
                "{model}"
            );
        }
    }

    #[test]
    fn modelinput_502_is_rewritten_to_client_error() {
        assert!(should_retry_after_upstream_error(
            400,
            r#"{"code":"invalid-argument","type":"error"}"#
        ));
        assert!(should_retry_after_upstream_error(
            422,
            r#"{"error":"data did not match any variant of untagged enum ModelInput"}"#
        ));
        assert_eq!(
            rewrite_poisoned_upstream_status(
                502,
                "Encrypted function output content could not be decrypted or decoded."
            ),
            400
        );
        assert_eq!(
            rewrite_poisoned_upstream_status(502, "Our servers are currently overloaded."),
            502
        );
        assert_eq!(
            rewrite_poisoned_upstream_status(503, "no available accounts"),
            429
        );
        assert_eq!(
            rewrite_poisoned_upstream_status(503, "Service temporarily unavailable"),
            429
        );
        assert!(is_exhausted_account_status(
            503,
            "auth_unavailable: no auth available (providers=openai-compatible-cr_r1_openai)"
        ));
    }
}
