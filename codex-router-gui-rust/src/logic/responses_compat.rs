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
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

const LOCAL_COMPACT_KEEP_LAST: usize = 48;
const AGGRESSIVE_COMPACT_KEEP_LAST: usize = 16;
const COMPACT_SNIPPET_CHARS: usize = 180;
const TOOL_OUTPUT_PRUNE_CHARS: usize = 2_000;
const RECENT_KEEP_CHARS: usize = 32_000;
const COMPACT_MIN_KEEP_ITEMS: usize = 8;
/// OpenAI-compatible Chat Completions / Meta Muse reject function `name` > 64.
const OPENAI_COMPAT_TOOL_NAME_MAX: usize = 64;

thread_local! {
    static TOOL_NAME_RESTORE: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

/// Clears shortened-tool-name aliases when a gateway request thread exits.
pub struct ToolNameRestoreGuard;

impl Drop for ToolNameRestoreGuard {
    fn drop(&mut self) {
        clear_tool_name_restore();
    }
}

fn clear_tool_name_restore() {
    TOOL_NAME_RESTORE.with(|slot| slot.borrow_mut().clear());
}

fn set_tool_name_restore(map: HashMap<String, String>) {
    TOOL_NAME_RESTORE.with(|slot| *slot.borrow_mut() = map);
}

fn tool_name_restore_active() -> bool {
    TOOL_NAME_RESTORE.with(|slot| !slot.borrow().is_empty())
}

fn restore_short_tool_name(name: &str) -> Option<String> {
    TOOL_NAME_RESTORE.with(|slot| slot.borrow().get(name).cloned())
}

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
        "# 模型身份\n你是{name}，通过 Codex-Router 接入。不要自称 GPT、ChatGPT 或 Codex 官方模型。写测试报告必须写入 D:\\\\Work\\\\CodexRouter\\\\Test\\\\Agent_Test_{}_{short}.md，禁止使用 chatgpt 或 unknown 作为文件名。调用工具时使用系统声明的工具名 exec_command，参数字段为 cmd。若工具列表没有 exec_command，仍然调用 exec_command，参数用 cmd。\n任务完成前每一轮必须发出结构化 function_call；只写计划或「接下来」而不调用工具会被 Codex 当成收工。\n",
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

pub fn is_xai_family_model(model: &str) -> bool {
    let slug = model
        .trim()
        .trim_start_matches('~')
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    slug.contains("grok") || slug.starts_with("grok-")
}

pub fn is_gemini_family_model(model: &str) -> bool {
    let slug = model
        .trim()
        .trim_start_matches('~')
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    slug.contains("gemini")
}

pub fn is_claude_family_model(model: &str) -> bool {
    let slug = model
        .trim()
        .trim_start_matches('~')
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    slug.contains("claude") || slug.contains("fable") || slug.contains("mythos")
}

fn looks_like_xai_compaction_blob(value: &str) -> bool {
    let token = value.trim();
    !token.is_empty()
        && !token.starts_with("gAAAA")
        && token.len() >= 24
        && !token.chars().any(char::is_whitespace)
}

pub fn prepare_xai_official_compact_request(body: &mut Value) {
    prepare_official_compact_request(body);
}

pub fn prepare_official_compact_request(body: &mut Value) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    for key in [
        "stream",
        "tools",
        "tool_choice",
        "parallel_tool_calls",
        "temperature",
        "top_p",
        "top_k",
        "stop",
        "max_output_tokens",
        "max_tokens",
        "include",
        "reasoning",
        "text",
        "store",
        "previous_response_id",
        "conversation",
        "conversation_id",
    ] {
        object.remove(key);
    }
}

fn is_official_xai_compaction_item(item: &Map<String, Value>) -> bool {
    matches!(
        item_type(&Value::Object(item.clone())).as_str(),
        "compaction" | "compact"
    ) && item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .is_some_and(looks_like_xai_compaction_blob)
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
    if is_official_xai_compaction_item(item) {
        return 0;
    }
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
    if is_official_xai_compaction_item(item) {
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
        "compaction" | "compact" if is_official_xai_compaction_item(&item) => {
            (Value::Object(item), 0)
        }
        _ => (message_from_text(summarize_item(&Value::Object(item))), 1),
    }
}

fn grok_safe_function_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": true
    })
}

fn is_grok_hang_schema_tool(name: &str, namespace: &str) -> bool {
    let name = name.trim();
    let lower = name.to_ascii_lowercase();
    if lower == "automation_update" || lower.ends_with("__automation_update") {
        return true;
    }
    let ns = namespace.trim().to_ascii_lowercase();
    (ns == "codex_app" || ns == "mcp__codex_app" || ns.ends_with("codex_app"))
        && lower == "automation_update"
}

fn grok_root_schema_rejected(params: &Value) -> bool {
    let Some(object) = params.as_object() else {
        return !params.is_null();
    };
    object.contains_key("oneOf")
        || object.contains_key("anyOf")
        || object.contains_key("allOf")
        || object.contains_key("$ref")
        || object.contains_key("$defs")
        || object.contains_key("definitions")
}

fn simplify_grok_function_parameters(params: &mut Value, force_safe: bool) -> bool {
    if force_safe {
        *params = grok_safe_function_parameters();
        return true;
    }
    if !grok_root_schema_rejected(params) {
        return false;
    }
    if let Some(object) = params.as_object_mut() {
        if object.get("type").and_then(Value::as_str) == Some("object")
            && object.contains_key("properties")
        {
            object.remove("oneOf");
            object.remove("anyOf");
            object.remove("allOf");
            object.remove("$ref");
            object.remove("$defs");
            object.remove("definitions");
            object.remove("not");
            return true;
        }
    }
    *params = grok_safe_function_parameters();
    true
}

fn grok_function_parameters_mut(tool: &mut Map<String, Value>) -> Option<&mut Value> {
    if tool.contains_key("parameters") {
        return tool.get_mut("parameters");
    }
    tool.get_mut("function")
        .and_then(Value::as_object_mut)
        .and_then(|function| function.get_mut("parameters"))
}

fn sanitize_grok_tool_list(tools: &mut [Value], namespace: &str) -> usize {
    let mut changed = 0;
    for tool in tools.iter_mut() {
        let Some(object) = tool.as_object_mut() else {
            continue;
        };
        let typ = object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("function")
            .to_owned();
        if typ == "namespace" {
            let child_ns = object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if let Some(children) = object.get_mut("tools").and_then(Value::as_array_mut) {
                changed += sanitize_grok_tool_list(children, &child_ns);
            }
            continue;
        }
        if typ != "function" && typ != "custom" {
            continue;
        }
        let name = tool_declared_name(&Value::Object(object.clone()));
        let force_safe = is_grok_hang_schema_tool(&name, namespace);
        if let Some(params) = grok_function_parameters_mut(object) {
            if simplify_grok_function_parameters(params, force_safe) {
                changed += 1;
            }
        } else if force_safe {
            object.insert("parameters".to_owned(), grok_safe_function_parameters());
            changed += 1;
        }
    }
    changed
}

fn sanitize_grok_request(body: &mut Map<String, Value>) -> usize {
    let mut changed = 0;
    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
        changed += sanitize_grok_tool_list(tools, "");
    }
    let drop_text = body
        .get_mut("text")
        .and_then(Value::as_object_mut)
        .map(|text| {
            let removed = text.remove("verbosity").is_some();
            (removed, text.is_empty())
        });
    if let Some((removed, empty)) = drop_text {
        if removed {
            changed += 1;
        }
        if empty {
            body.remove("text");
        }
    }
    if let Some(reasoning) = body.get_mut("reasoning").and_then(Value::as_object_mut) {
        if reasoning.remove("summary").is_some() {
            changed += 1;
        }
        if reasoning.remove("generate_summary").is_some() {
            changed += 1;
        }
    }
    changed
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

/// Gemini / Antigravity reject a `functionResponse` whose call id has no
/// matching `functionCall` earlier in the same request. Cross-thread agent
/// history (a forked sub-agent carrying the parent's function responses
/// without the originating calls) produces exactly this orphaned output, and
/// the upstream answers 400 `invalid Gemini function call history`. Dropping
/// the unmatched output -- keeping its payload as a plain text message so the
/// context survives -- lets the conversation continue on the new pool.
fn prune_unmatched_tool_outputs(items: &mut Vec<Value>) -> usize {
    let mut pruned = 0;
    let mut seen_calls: std::collections::HashSet<String> = Default::default();
    let mut output = Vec::with_capacity(items.len());
    for item in items.drain(..) {
        let call_id = item_call_id(&item);
        if is_function_call_item(&item) {
            if !call_id.is_empty() {
                seen_calls.insert(call_id);
            }
            output.push(item);
            continue;
        }
        if is_function_output_item(&item) && !call_id.is_empty() && !seen_calls.contains(&call_id) {
            output.push(message_from_text(summarize_item(&item)));
            pruned += 1;
            continue;
        }
        output.push(item);
    }
    *items = output;
    pruned
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

fn item_char_len(item: &Value) -> usize {
    item_text(item)
        .chars()
        .count()
        .max(item.to_string().len().min(64))
}

fn compact_split_at_chars(items: &[Value], keep_chars: usize) -> usize {
    if items.is_empty() {
        return 0;
    }
    let mut used = 0usize;
    let mut split_at = items.len();
    while split_at > 0 {
        let idx = split_at - 1;
        let size = item_char_len(&items[idx]).max(1);
        if used >= keep_chars && split_at < items.len() {
            break;
        }
        if used + size > keep_chars && split_at < items.len() && used > 0 {
            break;
        }
        used = used.saturating_add(size);
        split_at -= 1;
    }
    while split_at > 0 && is_function_output_item(&items[split_at]) {
        split_at -= 1;
    }
    split_at
}

fn compact_keep_split(items: &[Value], keep_last: usize) -> usize {
    if items.len() <= COMPACT_MIN_KEEP_ITEMS {
        return 0;
    }
    let max_keep = keep_last.max(COMPACT_MIN_KEEP_ITEMS);
    let split_keep_max = compact_split_at(items, COMPACT_MIN_KEEP_ITEMS);
    let split_keep_min = compact_split_at(items, max_keep);
    let split_chars = compact_split_at_chars(items, RECENT_KEEP_CHARS);
    split_chars.clamp(split_keep_min, split_keep_max)
}

fn truncate_text_value(value: &mut Value, limit: usize) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    if text.chars().count() <= limit {
        return false;
    }
    let truncated: String = text.chars().take(limit.saturating_sub(1)).collect();
    *value = Value::String(format!("{truncated}…"));
    true
}

fn prune_item_output(item: &mut Value, limit: usize) -> bool {
    let Some(map) = item.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    if let Some(output) = map.get_mut("output") {
        changed |= truncate_text_value(output, limit);
    }
    if let Some(text) = map.get_mut("text") {
        changed |= truncate_text_value(text, limit);
    }
    if let Some(content) = map.get_mut("content") {
        match content {
            Value::String(text) => {
                if text.chars().count() > limit {
                    let truncated: String = text.chars().take(limit.saturating_sub(1)).collect();
                    *text = format!("{truncated}…");
                    changed = true;
                }
            }
            Value::Array(parts) => {
                for part in parts {
                    if let Some(part_text) = part.get_mut("text") {
                        changed |= truncate_text_value(part_text, limit);
                    }
                }
            }
            _ => {}
        }
    }
    changed
}

fn prune_old_tool_outputs(items: &mut [Value]) -> bool {
    if items.len() <= COMPACT_MIN_KEEP_ITEMS {
        return false;
    }
    let split_at = compact_split_at_chars(items, RECENT_KEEP_CHARS);
    if split_at == 0 {
        return false;
    }
    let mut changed = false;
    for item in items.iter_mut().take(split_at) {
        let kind = item_type(item);
        let role = item_role(item);
        if is_function_output_item(item)
            || matches!(kind.as_str(), "tool" | "function")
            || role == "tool"
        {
            changed |= prune_item_output(item, TOOL_OUTPUT_PRUNE_CHARS);
        }
    }
    changed
}

fn summarize_dropped_items(items: &[Value]) -> String {
    let mut user_goals = Vec::new();
    let mut tools = Vec::new();
    let mut files = Vec::new();
    let mut workspace = None;
    for item in items {
        let kind = item_type(item);
        let text = item_text(item);
        if workspace.is_none() {
            workspace = extract_workspace_hint(&text);
        }
        for token in text.split_whitespace() {
            let candidate = token.trim_matches(|c: char| "\"'`[],".contains(c));
            if (candidate.contains('\\') || candidate.contains('/'))
                && candidate.chars().count() > 4
                && files.len() < 8
                && !files.iter().any(|seen: &String| seen == candidate)
            {
                files.push(candidate.to_owned());
            }
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
    let goal = if user_goals.is_empty() {
        "继续完成压缩前未完成的用户任务。".to_owned()
    } else {
        user_goals.join(" / ")
    };
    let pending = user_goals.last().cloned().unwrap_or_else(|| goal.clone());
    let mut lines = vec![
        format!(
            "【本地压缩摘要】已折叠 {} 条较早记录。这不是新任务，也不是任务结束；用户没有要求停止。不要写交接、不要空回复收工，立即从压缩前未完成的那一步继续。",
            items.len()
        ),
        "## Goal".to_owned(),
        goal,
        "## Instructions / Constraints".to_owned(),
        "用户没有换任务。不要输出空回复，不要写 handoff，不要等待新指令。只使用当前对话工作目录；不要读取 CodexRouter / Codex-Router 源码目录，除非它就是当前 cwd。默认用简体中文继续。".to_owned(),
        "## Discoveries".to_owned(),
        if tools.is_empty() {
            "较早回合已折叠；以近文原文和当前工作目录为准。".to_owned()
        } else {
            format!("已执行工具：{}", tools.join(", "))
        },
        "## Files".to_owned(),
        if files.is_empty() {
            workspace
                .clone()
                .unwrap_or_else(|| "以当前对话工作目录为准。".to_owned())
        } else {
            files.join("\n")
        },
        "## Pending".to_owned(),
        pending,
        "## Current work".to_owned(),
        "立刻从摘要里未完成的那一步继续执行，直到原任务完成。".to_owned(),
    ];
    if let Some(workspace) = workspace {
        lines.push(format!("工作目录提示：{workspace}"));
    }
    lines.join("\n")
}

fn compact_input(items: &mut Vec<Value>, keep_last: usize) -> bool {
    if items.len() <= COMPACT_MIN_KEEP_ITEMS {
        return false;
    }
    let split_at = compact_keep_split(items, keep_last);
    if split_at == 0 {
        return false;
    }
    let kept = items.split_off(split_at);
    let notice = message_from_text(summarize_dropped_items(items));
    items.clear();
    items.push(notice);
    items.extend(kept);
    let _ = prune_old_tool_outputs(items);
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
            || text.contains("Here is the summary produced by the other language model")
    })
}

fn rewrite_codex_compact_handoff_text(text: &str) -> Option<String> {
    let marker = "Another language model started to solve this problem and produced a summary";
    if !text.contains(marker)
        && !text.contains("Here is the summary produced by the other language model")
    {
        return None;
    }
    let summary = text
        .split("use the information in this summary to assist with your own analysis:")
        .nth(1)
        .or_else(|| {
            text.split("Here is the summary produced by the other language model")
                .nth(1)
        })
        .unwrap_or(text)
        .trim();
    Some(format!(
        "【自动压缩后续跑】用户没有停止，也没有换任务。下面只是压缩后的工作摘要，不是交接、不是收工指令。不要输出空回复，不要写 handoff，不要等待新指令。立刻从摘要里未完成的那一步继续执行，直到原任务完成。\n\n{}",
        summary
    ))
}

fn rewrite_codex_compact_handoff(map: &mut Map<String, Value>) -> bool {
    let mut changed = false;
    if let Some(parts) = map.get_mut("content").and_then(Value::as_array_mut) {
        for part in parts {
            let Some(part_map) = part.as_object_mut() else {
                continue;
            };
            let Some(text) = part_map.get("text").and_then(Value::as_str) else {
                continue;
            };
            if let Some(rewritten) = rewrite_codex_compact_handoff_text(text) {
                part_map.insert("text".to_owned(), Value::String(rewritten));
                changed = true;
            }
        }
    }
    if let Some(text) = map.get("text").and_then(Value::as_str) {
        if let Some(rewritten) = rewrite_codex_compact_handoff_text(text) {
            map.insert("text".to_owned(), Value::String(rewritten));
            changed = true;
        }
    }
    changed
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

const GEMINI_CARRIER_MARKER: &str = "cpa-gemini-responses-carrier";

fn looks_like_gemini_thought_carrier(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.contains(GEMINI_CARRIER_MARKER) || trimmed.starts_with("cpa-gemini-")
}

/// Gemini/Antigravity 404 `Requested entity was not found` happens when Codex
/// replays a two-day-old thought carrier or a Codex `previous_response_id`
/// that Google no longer has. Drop those server-side handles; the transcript
/// in `input` is enough to continue.
fn strip_gemini_server_continuation(object: &mut Map<String, Value>) -> bool {
    let mut changed = false;
    for key in ["previous_response_id", "conversation_id"] {
        if object.remove(key).is_some() {
            changed = true;
        }
    }
    if object.remove("conversation").is_some() {
        changed = true;
    }
    changed
}

fn sanitize_gemini_item(item: &mut Map<String, Value>) -> usize {
    let mut stripped = 0;
    let keys: Vec<String> = item.keys().cloned().collect();
    for key in keys {
        if !matches!(
            key.as_str(),
            "encrypted_content" | "output" | "signature" | "thought_signature"
        ) {
            continue;
        }
        let Some(text) = item.get(&key).and_then(Value::as_str) else {
            continue;
        };
        if looks_like_gemini_thought_carrier(text) {
            item.remove(&key);
            stripped += 1;
        }
    }
    stripped
}

pub fn sanitize_responses_request(path: &str, body: &mut Value) -> SanitizeStats {
    clear_tool_name_restore();
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
    let gemini = is_gemini_family_model(&stats.model);
    let Some(object) = body.as_object_mut() else {
        return stats;
    };

    if !stats.openai_family && strip_include_encrypted(object) {
        stats.stripped_encrypted += 1;
    }
    if gemini && strip_gemini_server_continuation(object) {
        stats.converted_items += 1;
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
        if grok {
            stats.rewritten_tool_calls += sanitize_grok_request(object);
        }
        stats.rewritten_tool_calls += clamp_openai_compat_tool_names(object);
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
            if gemini {
                stats.stripped_encrypted += sanitize_gemini_item(&mut map);
            }
            if !stats.openai_family {
                if convert_agent_message(&mut map) {
                    stats.converted_items += 1;
                }
                if convert_shell_protocol_item(&mut map) {
                    stats.converted_items += 1;
                }
                if grok {
                    if rewrite_codex_compact_handoff(&mut map) {
                        stats.converted_items += 1;
                    }
                    let (item, converted) = sanitize_grok_item(map);
                    stats.converted_items += converted;
                    replacement.push(item);
                    continue;
                }
                if rewrite_codex_compact_handoff(&mut map) {
                    stats.converted_items += 1;
                }
                stats.converted_items += sanitize_content_parts(&mut map);
                let kind = item_type(&Value::Object(map.clone()));
                if unsupported_third_party_item(&kind)
                    && !(is_xai_family_model(&stats.model) && is_official_xai_compaction_item(&map))
                {
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
        if !stats.openai_family && prune_old_tool_outputs(&mut replacement) {
            stats.converted_items += 1;
        }
        if !stats.openai_family && compact_request {
            stats.locally_compacted = compact_input(&mut replacement, LOCAL_COMPACT_KEEP_LAST);
        }
        if !stats.openai_family {
            stats.rewritten_tool_calls += ensure_tool_call_outputs(&mut replacement);
        }
        if gemini {
            stats.rewritten_tool_calls += prune_unmatched_tool_outputs(&mut replacement);
        }
        object.insert("input".to_owned(), Value::Array(replacement));
    }
    if !stats.openai_family {
        stats.rewritten_tool_calls += ensure_chat_tool_messages(object);
        if let Some(messages) = object.get_mut("messages").and_then(Value::as_array_mut) {
            if prune_old_tool_outputs(messages) {
                stats.converted_items += 1;
            }
        }
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

#[cfg(test)]
pub fn shorten_openai_compat_tool_name(name: &str) -> String {
    shorten_openai_compat_tool_name_unique(name, &HashSet::new())
}

fn shorten_openai_compat_tool_name_unique(name: &str, used: &HashSet<String>) -> String {
    let name = remap_tool_name(name);
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= OPENAI_COMPAT_TOOL_NAME_MAX {
        return name;
    }
    let digest = Sha256::digest(name.as_bytes());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut take = 8;
    loop {
        let suffix = format!("_{}", &hex[..take]);
        let keep = OPENAI_COMPAT_TOOL_NAME_MAX.saturating_sub(suffix.len());
        let prefix: String = chars.iter().take(keep).collect();
        let candidate = format!("{prefix}{suffix}");
        if !used.contains(&candidate) {
            return candidate;
        }
        take = (take + 2).min(hex.len());
        if take >= hex.len() {
            return candidate;
        }
    }
}

fn remap_tool_name_value(value: &mut Value) -> bool {
    let Some(name) = value.as_str().map(str::to_owned) else {
        return false;
    };
    let mut remapped = remap_tool_name(&name);
    if let Some(original) = restore_short_tool_name(&remapped) {
        remapped = original;
    }
    if remapped == name {
        return false;
    }
    *value = Value::String(remapped);
    true
}

fn set_json_string_name(value: &mut Value, name: &str) -> bool {
    if value.as_str() == Some(name) {
        return false;
    }
    if value.as_str().is_none() {
        return false;
    }
    *value = Value::String(name.to_owned());
    true
}

fn annotate_shortened_tool_description(tool: &mut Map<String, Value>, original: &str, short: &str) {
    if original == short {
        return;
    }
    let note = format!("Original Codex tool name: {original}. ");
    let insert_note = |desc: &mut Value| {
        if let Some(text) = desc.as_str() {
            if text.contains(original) {
                return;
            }
            *desc = Value::String(format!("{note}{text}"));
        }
    };
    if let Some(desc) = tool.get_mut("description") {
        insert_note(desc);
    }
    if let Some(function) = tool.get_mut("function").and_then(Value::as_object_mut) {
        if let Some(desc) = function.get_mut("description") {
            insert_note(desc);
        }
    }
}

fn qualify_namespace_tool_name(namespace: &str, child: &str) -> String {
    let child = child.trim();
    let namespace = namespace.trim();
    if child.is_empty() {
        return String::new();
    }
    if namespace.is_empty() || child.starts_with("mcp__") || child.starts_with(namespace) {
        return child.to_owned();
    }
    if namespace.ends_with("__") {
        format!("{namespace}{child}")
    } else {
        format!("{namespace}__{child}")
    }
}

fn shorten_to_char_len(name: &str, max: usize, used: &HashSet<String>) -> String {
    let chars: Vec<char> = name.chars().collect();
    if max == 0 {
        return String::new();
    }
    if chars.len() <= max && !used.contains(name) {
        return name.to_owned();
    }
    let digest = Sha256::digest(name.as_bytes());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut take = 8.min(hex.len());
    loop {
        let suffix = format!("_{}", &hex[..take]);
        if suffix.len() >= max {
            let clipped: String = suffix.chars().take(max).collect();
            if !used.contains(&clipped) {
                return clipped;
            }
        }
        let keep = max.saturating_sub(suffix.len());
        let prefix: String = chars.iter().take(keep).collect();
        let candidate = format!("{prefix}{suffix}");
        if !used.contains(&candidate) {
            return candidate;
        }
        if take >= hex.len() {
            return candidate;
        }
        take = (take + 2).min(hex.len());
    }
}

fn local_name_for_qualified_chat(
    namespace: &str,
    original_local: &str,
    short_chat: &str,
) -> String {
    if namespace.is_empty()
        || original_local.starts_with("mcp__")
        || original_local.starts_with(namespace)
    {
        return short_chat.to_owned();
    }
    let prefix = if namespace.ends_with("__") {
        namespace.to_owned()
    } else {
        format!("{namespace}__")
    };
    short_chat
        .strip_prefix(&prefix)
        .unwrap_or(short_chat)
        .to_owned()
}

fn clamp_declared_tool_name(
    tool: &mut Map<String, Value>,
    namespace: &str,
    used: &mut HashSet<String>,
    forward: &mut HashMap<String, String>,
    restore: &mut HashMap<String, String>,
) -> usize {
    let original_local = tool_declared_name(&Value::Object(tool.clone()));
    if original_local.is_empty() {
        return 0;
    }
    let chat = qualify_namespace_tool_name(namespace, &original_local);
    let short_chat = if let Some(existing) = forward.get(&chat) {
        existing.clone()
    } else if chat.chars().count() <= OPENAI_COMPAT_TOOL_NAME_MAX {
        used.insert(chat.clone());
        forward.insert(chat.clone(), chat.clone());
        chat.clone()
    } else {
        let prefix = if namespace.is_empty()
            || original_local.starts_with("mcp__")
            || original_local.starts_with(namespace)
        {
            String::new()
        } else if namespace.ends_with("__") {
            namespace.to_owned()
        } else {
            format!("{namespace}__")
        };
        let max_local = OPENAI_COMPAT_TOOL_NAME_MAX.saturating_sub(prefix.chars().count());
        let short = if prefix.is_empty() || max_local < 12 {
            shorten_openai_compat_tool_name_unique(&chat, used)
        } else {
            let local_short = shorten_to_char_len(&original_local, max_local, used);
            format!("{prefix}{local_short}")
        };
        used.insert(short.clone());
        forward.insert(chat.clone(), short.clone());
        restore.insert(short.clone(), original_local.clone());
        if short != original_local {
            restore.insert(
                local_name_for_qualified_chat(namespace, &original_local, &short),
                original_local.clone(),
            );
        }
        short
    };
    let new_local = local_name_for_qualified_chat(namespace, &original_local, &short_chat);
    let mut changed = 0;
    if let Some(name) = tool.get_mut("name") {
        if set_json_string_name(name, &new_local) {
            changed += 1;
        }
    }
    if let Some(function) = tool.get_mut("function").and_then(Value::as_object_mut) {
        if let Some(name) = function.get_mut("name") {
            if set_json_string_name(name, &new_local) {
                changed += 1;
            }
        }
    }
    annotate_shortened_tool_description(tool, &original_local, &new_local);
    changed
}

fn clamp_tool_list(
    tools: &mut [Value],
    namespace: &str,
    used: &mut HashSet<String>,
    forward: &mut HashMap<String, String>,
    restore: &mut HashMap<String, String>,
) -> usize {
    let mut changed = 0;
    for tool in tools.iter_mut() {
        let Some(object) = tool.as_object_mut() else {
            continue;
        };
        let typ = object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("function")
            .to_owned();
        if typ == "namespace" {
            let child_ns = object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if let Some(children) = object.get_mut("tools").and_then(Value::as_array_mut) {
                changed += clamp_tool_list(children, &child_ns, used, forward, restore);
            }
            continue;
        }
        if typ != "function" && typ != "custom" && !typ.is_empty() {
            continue;
        }
        changed += clamp_declared_tool_name(object, namespace, used, forward, restore);
    }
    changed
}

fn clamp_openai_compat_tool_names(body: &mut Map<String, Value>) -> usize {
    let mut used = HashSet::new();
    let mut forward = HashMap::new();
    let mut restore = HashMap::new();
    let mut changed = 0;
    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
        changed += clamp_tool_list(tools, "", &mut used, &mut forward, &mut restore);
    }
    if let Some(items) = body.get_mut("input").and_then(Value::as_array_mut) {
        for item in items.iter_mut() {
            let Some(item) = item.as_object_mut() else {
                continue;
            };
            let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
            if kind == "additional_tools" {
                if let Some(tools) = item.get_mut("tools").and_then(Value::as_array_mut) {
                    changed += clamp_tool_list(tools, "", &mut used, &mut forward, &mut restore);
                }
                continue;
            }
            if kind != "function_call" && kind != "custom_tool_call" {
                continue;
            }
            let namespace = item
                .get("namespace")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            changed +=
                clamp_declared_tool_name(item, &namespace, &mut used, &mut forward, &mut restore);
        }
    }
    set_tool_name_restore(restore);
    changed
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

/// Carries `<think>` / `<thinking>` scan state across SSE events so Spark
/// (and similar Chat Completions models) stream reasoning instead of
/// buffering until `</think>`.
#[derive(Debug, Default)]
pub struct SseThinkState {
    /// ChatGPT / Grok already emit native reasoning events. Do not rewrite
    /// their SSE (a stray `<` or `encrypted_content` substring would stall
    /// or strip the stream).
    pub disabled: bool,
    scan: ThinkScan,
    reasoning_open: bool,
    item_id: String,
    output_index: u64,
    next_item: u64,
}

impl SseThinkState {
    pub fn disabled() -> Self {
        Self {
            disabled: true,
            ..Self::default()
        }
    }

    fn needs_scan(&self) -> bool {
        !self.disabled && (self.scan.is_active() || self.reasoning_open)
    }
}

#[derive(Debug, Default)]
struct ThinkScan {
    hold: String,
    inside: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum ThinkPiece {
    Visible(String),
    Think(String),
}

enum OpenThink {
    Found { rel: usize, open_len: usize },
    HoldPrefix(usize),
    None,
}

enum CloseThink {
    Found { rel: usize, close_len: usize },
    HoldPrefix(usize),
    None,
}

fn is_open_think_prefix(rest_lower: &str) -> bool {
    // Require `<think` so a lone `<` / `<t` / HTML `<th` is not held across
    // ChatGPT/Gemini token boundaries (that looked like "the model stopped").
    rest_lower.len() >= 6
        && ("<think>".starts_with(rest_lower) || "<thinking>".starts_with(rest_lower))
}

fn is_close_think_prefix(rest_lower: &str) -> bool {
    rest_lower.len() >= 7
        && ("</think>".starts_with(rest_lower) || "</thinking>".starts_with(rest_lower))
}

fn find_open_think(s: &str) -> OpenThink {
    let lower = s.to_ascii_lowercase();
    let mut search_from = 0;
    while search_from < lower.len() {
        let Some(off) = lower[search_from..].find('<') else {
            return OpenThink::None;
        };
        let rel = search_from + off;
        let rest = &lower[rel..];
        if rest.starts_with("<thinking>") {
            return OpenThink::Found {
                rel,
                open_len: "<thinking>".len(),
            };
        }
        if rest.starts_with("<think>") {
            return OpenThink::Found {
                rel,
                open_len: "<think>".len(),
            };
        }
        if is_open_think_prefix(rest) && rel + rest.len() == lower.len() {
            return OpenThink::HoldPrefix(rel);
        }
        search_from = rel + 1;
    }
    OpenThink::None
}

fn find_close_think(s: &str) -> CloseThink {
    let lower = s.to_ascii_lowercase();
    let mut search_from = 0;
    while search_from < lower.len() {
        let Some(off) = lower[search_from..].find('<') else {
            return CloseThink::None;
        };
        let rel = search_from + off;
        let rest = &lower[rel..];
        if rest.starts_with("</thinking>") {
            return CloseThink::Found {
                rel,
                close_len: "</thinking>".len(),
            };
        }
        if rest.starts_with("</think>") {
            return CloseThink::Found {
                rel,
                close_len: "</think>".len(),
            };
        }
        if is_close_think_prefix(rest) && rel + rest.len() == lower.len() {
            return CloseThink::HoldPrefix(rel);
        }
        search_from = rel + 1;
    }
    CloseThink::None
}

impl ThinkScan {
    fn is_active(&self) -> bool {
        self.inside || !self.hold.is_empty()
    }

    fn push(&mut self, incoming: &str) -> Vec<ThinkPiece> {
        if incoming.is_empty() && self.hold.is_empty() {
            return Vec::new();
        }
        let mut src = std::mem::take(&mut self.hold);
        src.push_str(incoming);
        let mut pieces = Vec::new();
        let mut i = 0;
        while i < src.len() {
            if !src.is_char_boundary(i) {
                i += 1;
                continue;
            }
            if !self.inside {
                match find_open_think(&src[i..]) {
                    OpenThink::Found { rel, open_len } => {
                        let abs = i + rel;
                        if abs > i {
                            pieces.push(ThinkPiece::Visible(src[i..abs].to_owned()));
                        }
                        self.inside = true;
                        i = abs + open_len;
                    }
                    OpenThink::HoldPrefix(rel) => {
                        let abs = i + rel;
                        if abs > i {
                            pieces.push(ThinkPiece::Visible(src[i..abs].to_owned()));
                        }
                        self.hold = src[abs..].to_owned();
                        return pieces;
                    }
                    OpenThink::None => {
                        pieces.push(ThinkPiece::Visible(src[i..].to_owned()));
                        return pieces;
                    }
                }
            } else {
                match find_close_think(&src[i..]) {
                    CloseThink::Found { rel, close_len } => {
                        let abs = i + rel;
                        if abs > i {
                            pieces.push(ThinkPiece::Think(src[i..abs].to_owned()));
                        }
                        self.inside = false;
                        i = abs + close_len;
                    }
                    CloseThink::HoldPrefix(rel) => {
                        let abs = i + rel;
                        if abs > i {
                            pieces.push(ThinkPiece::Think(src[i..abs].to_owned()));
                        }
                        self.hold = src[abs..].to_owned();
                        return pieces;
                    }
                    CloseThink::None => {
                        pieces.push(ThinkPiece::Think(src[i..].to_owned()));
                        return pieces;
                    }
                }
            }
        }
        pieces
    }

    fn flush(&mut self) -> Vec<ThinkPiece> {
        let hold = std::mem::take(&mut self.hold);
        if hold.is_empty() {
            return Vec::new();
        }
        if self.inside {
            vec![ThinkPiece::Think(hold)]
        } else {
            vec![ThinkPiece::Visible(hold)]
        }
    }
}

fn sse_data_line(value: &Value) -> String {
    format!("data: {value}\n\n")
}

fn ensure_reasoning_open(state: &mut SseThinkState, out: &mut String) {
    if state.reasoning_open {
        return;
    }
    state.next_item = state.next_item.saturating_add(1);
    state.item_id = format!("rs_cr_think_{}", state.next_item);
    state.output_index = 0;
    state.reasoning_open = true;
    out.push_str(&sse_data_line(&json!({
        "type": "response.output_item.added",
        "output_index": state.output_index,
        "item": {
            "id": state.item_id,
            "type": "reasoning",
            "status": "in_progress",
            "summary": []
        }
    })));
    out.push_str(&sse_data_line(&json!({
        "type": "response.reasoning_summary_part.added",
        "item_id": state.item_id,
        "output_index": state.output_index,
        "summary_index": 0,
        "part": { "type": "summary_text", "text": "" }
    })));
}

fn emit_think_delta(state: &mut SseThinkState, out: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    ensure_reasoning_open(state, out);
    out.push_str(&sse_data_line(&json!({
        "type": "response.reasoning_summary_text.delta",
        "item_id": state.item_id,
        "output_index": state.output_index,
        "summary_index": 0,
        "delta": text
    })));
}

fn close_reasoning(state: &mut SseThinkState, out: &mut String) {
    if !state.reasoning_open {
        return;
    }
    out.push_str(&sse_data_line(&json!({
        "type": "response.reasoning_summary_text.done",
        "item_id": state.item_id,
        "output_index": state.output_index,
        "summary_index": 0,
        "text": ""
    })));
    out.push_str(&sse_data_line(&json!({
        "type": "response.reasoning_summary_part.done",
        "item_id": state.item_id,
        "output_index": state.output_index,
        "summary_index": 0,
        "part": { "type": "summary_text", "text": "" }
    })));
    out.push_str(&sse_data_line(&json!({
        "type": "response.output_item.done",
        "output_index": state.output_index,
        "item": {
            "id": state.item_id,
            "type": "reasoning",
            "status": "completed",
            "summary": []
        }
    })));
    state.reasoning_open = false;
}

fn emit_think_pieces(
    state: &mut SseThinkState,
    pieces: Vec<ThinkPiece>,
    original_text_event: Option<&Value>,
    out: &mut String,
) {
    for piece in pieces {
        match piece {
            ThinkPiece::Think(text) if text.is_empty() => {}
            ThinkPiece::Think(text) => emit_think_delta(state, out, &text),
            ThinkPiece::Visible(text) if text.is_empty() => {}
            ThinkPiece::Visible(text) => {
                close_reasoning(state, out);
                if let Some(orig) = original_text_event {
                    let mut ev = orig.clone();
                    ev["delta"] = Value::String(text);
                    out.push_str(&sse_data_line(&ev));
                }
            }
        }
    }
}

fn flush_think_state(state: &mut SseThinkState, out: &mut String) {
    let pieces = state.scan.flush();
    emit_think_pieces(state, pieces, None, out);
    close_reasoning(state, out);
}

#[allow(dead_code)]
pub fn flush_sse_think(state: &mut SseThinkState) -> String {
    let mut out = String::new();
    flush_think_state(state, &mut out);
    out
}

fn event_kind(json: &Value) -> &str {
    json.get("type").and_then(Value::as_str).unwrap_or("")
}

fn is_output_text_delta(kind: &str) -> bool {
    kind == "response.output_text.delta"
}

fn is_think_boundary(kind: &str) -> bool {
    matches!(
        kind,
        "response.output_item.added"
            | "response.function_call_arguments.delta"
            | "response.function_call_arguments.done"
            | "response.output_text.done"
            | "response.content_part.done"
            | "response.output_item.done"
            | "response.completed"
            | "response.failed"
            | "response.incomplete"
    )
}

fn event_may_need_think_rewrite(state: &SseThinkState, event: &str) -> bool {
    if state.disabled {
        return false;
    }
    if state.needs_scan() {
        return true;
    }
    let lower = event.to_ascii_lowercase();
    if !lower.contains("<think") {
        return false;
    }
    event.contains("output_text.delta") || event.contains("output_text.done")
}

#[allow(dead_code)]
pub fn rewrite_sse_text(chunk: &str) -> String {
    let mut state = SseThinkState::default();
    rewrite_sse_text_with(&mut state, chunk)
}

#[allow(dead_code)]
pub fn rewrite_sse_text_with(state: &mut SseThinkState, chunk: &str) -> String {
    let mut rewritten = String::with_capacity(chunk.len());
    let mut rest = chunk;
    while let Some(end) = rest.find("\n\n") {
        let (event, tail) = rest.split_at(end + 2);
        rewritten.push_str(&rewrite_sse_event(state, event));
        rest = tail;
    }
    rewritten.push_str(&rewrite_sse_event(state, rest));
    rewritten
}

#[allow(dead_code)]
fn rewrite_sse_event(state: &mut SseThinkState, event: &str) -> String {
    if event.trim().is_empty() {
        return event.to_owned();
    }
    let has_leak = find_leaked_tool_start(event).is_some();
    let restore = tool_name_restore_active();
    let think = event_may_need_think_rewrite(state, event);
    if !has_leak && !restore && !think {
        return event.to_owned();
    }
    let mut out = String::new();
    let mut extracted_tools = Vec::new();
    let mut converted_text_delta = false;
    for line in event.split_inclusive('\n') {
        if let Some(data) = line.strip_prefix("data:") {
            let trimmed = data.trim();
            if let Ok(mut json) = serde_json::from_str::<Value>(trimmed) {
                if restore {
                    rewrite_tool_names_in_json(&mut json);
                }
                if has_leak {
                    rewrite_provider_json(&mut json);
                }
                let kind = event_kind(&json).to_owned();
                if think && is_output_text_delta(&kind) {
                    if let Some(delta) = json.get("delta").and_then(Value::as_str) {
                        let pieces = state.scan.push(delta);
                        let mut visible_tools = Vec::new();
                        let mut mapped = Vec::new();
                        for piece in pieces {
                            match piece {
                                ThinkPiece::Visible(text) => {
                                    let mut value = Value::String(text);
                                    if has_leak {
                                        rewrite_text_value(&mut value, &mut visible_tools);
                                    }
                                    if let Some(cleaned) = value.as_str() {
                                        if !cleaned.is_empty() {
                                            mapped.push(ThinkPiece::Visible(cleaned.to_owned()));
                                        }
                                    }
                                }
                                think_piece => mapped.push(think_piece),
                            }
                        }
                        extracted_tools.extend(visible_tools);
                        emit_think_pieces(state, mapped, Some(&json), &mut out);
                        converted_text_delta = true;
                        continue;
                    }
                }
                if think && is_think_boundary(&kind) {
                    if kind == "response.output_text.done" {
                        if let Some(text) = json.get("text").and_then(Value::as_str) {
                            if !state.needs_scan()
                                && (text.to_ascii_lowercase().contains("<think>")
                                    || text.to_ascii_lowercase().contains("<thinking>"))
                            {
                                let pieces = state.scan.push(text);
                                emit_think_pieces(state, pieces, None, &mut out);
                            }
                        }
                    }
                    flush_think_state(state, &mut out);
                }
                if has_leak {
                    if let Some(delta) = json.pointer_mut("/delta") {
                        rewrite_text_value(delta, &mut extracted_tools);
                    }
                    if let Some(text) = json.pointer_mut("/item/content/0/text") {
                        rewrite_text_value(text, &mut extracted_tools);
                    }
                }
                if restore || has_leak {
                    out.push_str("data: ");
                    out.push_str(&json.to_string());
                    if line.ends_with('\n') {
                        out.push('\n');
                    }
                } else {
                    out.push_str(line);
                }
                continue;
            }
            if has_leak {
                let (cleaned, tools) = extract_leaked_tool_calls(data);
                extracted_tools.extend(tools);
                out.push_str("data:");
                out.push_str(&cleaned);
                if line.ends_with('\n') && !cleaned.ends_with('\n') {
                    out.push('\n');
                }
            } else {
                out.push_str(line);
            }
        } else if converted_text_delta {
            continue;
        } else {
            out.push_str(line);
        }
    }
    if converted_text_delta && !out.ends_with("\n\n") && !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
    }
    for (index, tool) in extracted_tools.into_iter().enumerate() {
        let output_index = index as u64;
        out.push_str(&sse_data_line(&json!({
            "type": "response.output_item.added",
            "output_index": output_index,
            "item": tool.clone(),
        })));
        out.push_str(&sse_data_line(&json!({
            "type": "response.output_item.done",
            "output_index": output_index,
            "item": tool,
        })));
    }
    out
}

pub fn continue_after_local_compact_instructions() -> &'static str {
    "这不是新任务，也不是任务结束。上下文压缩只是把较早记录折成摘要，用户没有要求停止。不要输出交接文档、不要总结后收工、不要等待新指令。立即从压缩前未完成的那一步继续执行，直到用户原任务真正完成。默认使用简体中文。只使用当前对话工作目录，不要读取 CodexRouter 源码目录，除非它就是当前 cwd。"
}

/// Marker Codex Desktop never writes. The gateway uses it to recognise an
/// automatic "keep going" nudge it injected after Grok (or another
/// third-party model) ended an agent turn with commentary and no tool call.
pub const INCOMPLETE_TURN_CONTINUE_MARKER: &str = "【自动续跑】";

pub fn continue_after_premature_stop_instructions() -> &'static str {
    "【自动续跑】这不是任务结束。你刚才只写了说明，没有发出结构化 function_call。Codex 会把「纯文字、无工具调用」当成收工。用户没有要求停止。立刻调用系统工具继续执行未完成的步骤，不要再只写计划、不要写「接下来」然后停。不要回复「任务已完成」。"
}

fn is_continue_nudge_item(item: &Value) -> bool {
    item_text(item).contains(INCOMPLETE_TURN_CONTINUE_MARKER)
}

pub fn request_has_agent_tools(body: &Value) -> bool {
    body.get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty())
}

fn item_is_assistant_message(item: &Value) -> bool {
    item_role(item) == "assistant"
}

fn item_is_chat_tool_result(item: &Value) -> bool {
    matches!(item_role(item).as_str(), "tool" | "function")
}

/// True when the latest real input is a tool result: Codex just fed function
/// outputs back and is waiting for the model to call more tools or finish.
pub fn request_is_mid_agent_turn(body: &Value) -> bool {
    if let Some(items) = body.get("input").and_then(Value::as_array) {
        for item in items.iter().rev() {
            if is_continue_nudge_item(item) || item_is_assistant_message(item) {
                continue;
            }
            return is_function_output_item(item);
        }
        return false;
    }
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for item in messages.iter().rev() {
            if is_continue_nudge_item(item) || item_is_assistant_message(item) {
                continue;
            }
            return item_is_chat_tool_result(item);
        }
    }
    false
}

const INCOMPLETE_STOP_INTENTS: &[&str] = &[
    "接下来",
    "我先",
    "我改用",
    "让我",
    "我来",
    "下一步",
    "先看",
    "先读",
    "先打开",
    "再看一下",
    "看图工具",
    "view_image",
    "i'll ",
    "i will ",
    "let me ",
    "i am going to",
    "i'm going to",
    "next i'll",
    "now look",
    "now check",
    "now read",
    "now open",
];

const FINISHED_TURN_MARKERS: &[&str] = &[
    "任务已完成",
    "任务完成",
    "已完成全部",
    "全部完成",
    "修复完成",
    "验收通过",
    "verdict",
    "that's all",
    "task is complete",
    "task complete",
    "all done",
];

/// Short "我先对照…" / "接下来…" commentary is a premature stop. A 1000-char
/// Debugger report that happens to say "下一步：Coder 做 T02" is not.
const SUBSTANTIAL_ANSWER_CHARS: usize = 200;

pub fn assistant_text_looks_like_incomplete_stop(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    INCOMPLETE_STOP_INTENTS.iter().any(|marker| {
        if marker.bytes().all(|byte| byte.is_ascii()) {
            lower.contains(marker)
        } else {
            trimmed.contains(marker)
        }
    })
}

pub fn assistant_text_looks_finished(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    FINISHED_TURN_MARKERS.iter().any(|marker| {
        if marker.bytes().all(|byte| byte.is_ascii()) {
            lower.contains(marker)
        } else {
            trimmed.contains(marker)
        }
    })
}

/// Codex treats an assistant message with no `function_call` as `task_complete`.
/// Grok often writes "我先对照…" / "接下来…" after tool results and stops.
/// Hold that completed event and retry with a continuation nudge when this
/// returns true.
pub fn should_continue_incomplete_agent_turn(
    model: &str,
    request: &Value,
    streamed_text: &str,
    had_function_call: bool,
) -> bool {
    if had_function_call || is_openai_family_model(model) || !is_xai_family_model(model) {
        return false;
    }
    if !extract_leaked_tool_calls(streamed_text).1.is_empty() {
        return false;
    }
    let trimmed = streamed_text.trim();
    let chars = trimmed.chars().count();
    // A finished write-up wins even if it also contains "下一步" / "接下来".
    if assistant_text_looks_finished(streamed_text) {
        return false;
    }
    // Long answers are reports, not the short plan-only stops this retry exists for.
    if chars > SUBSTANTIAL_ANSWER_CHARS {
        return false;
    }
    let incomplete = assistant_text_looks_like_incomplete_stop(streamed_text);
    if request_is_mid_agent_turn(request) {
        return incomplete || trimmed.is_empty() || chars <= 80;
    }
    request_has_agent_tools(request) && incomplete
}

/// Append this turn's assistant text plus a keep-going user nudge so the
/// follow-up POST is a full replay (third-party `previous_response_id` is
/// not usable across the gateway).
pub fn append_incomplete_turn_continuation(body: &mut Value, assistant_text: &str) -> bool {
    let Some(object) = body.as_object_mut() else {
        return false;
    };
    object.remove("previous_response_id");
    let nudge = continue_after_premature_stop_instructions();
    let assistant = assistant_text.trim();
    if let Some(items) = object.get_mut("input").and_then(Value::as_array_mut) {
        if !assistant.is_empty() {
            items.push(json!({
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": assistant }],
            }));
        }
        items.push(json!({
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": nudge }],
        }));
        return true;
    }
    if let Some(messages) = object.get_mut("messages").and_then(Value::as_array_mut) {
        if !assistant.is_empty() {
            messages.push(json!({
                "role": "assistant",
                "content": assistant,
            }));
        }
        messages.push(json!({
            "role": "user",
            "content": nudge,
        }));
        return true;
    }
    let mut items = Vec::new();
    if !assistant.is_empty() {
        items.push(json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": assistant }],
        }));
    }
    items.push(json!({
        "type": "message",
        "role": "user",
        "content": [{ "type": "input_text", "text": nudge }],
    }));
    object.insert("input".to_owned(), Value::Array(items));
    true
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

pub fn is_missing_upstream_entity_error(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("requested entity was not found")
        || lower.contains("entity was not found")
        || (lower.contains("not_found") && lower.contains("entity"))
}

pub fn should_retry_after_upstream_error(status: u16, body: &str) -> bool {
    if status == 404 {
        return is_missing_upstream_entity_error(body);
    }
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
    if status == 401 {
        // Codex Desktop with requires_openai_auth=true treats a 401 from the
        // local provider as "ChatGPT login died" and bounces to the login
        // page. Router requests use an independent bearer; an upstream
        // ChatGPT 401 must never leak through as an authentication failure.
        return 503;
    }
    if should_retry_after_upstream_error(status, body) && matches!(status, 500 | 502 | 503) {
        400
    } else {
        rewrite_exhausted_account_status(status, body)
    }
}

/// Replace an upstream 401 envelope with a non-auth error so Desktop cannot
/// escalate it into a forced ChatGPT login. Other statuses are returned
/// unchanged (body copied).
pub fn shield_desktop_auth_failure(status: u16, body: &[u8]) -> (u16, Vec<u8>) {
    if status != 401 {
        return (status, body.to_vec());
    }
    let payload = serde_json::json!({
        "error": {
            "message": "model temporarily unavailable",
            "type": "codex_router_error",
            "param": null,
            "code": "CR-UP-0011"
        }
    });
    (
        503,
        serde_json::to_vec(&payload).unwrap_or_else(|_| {
            br#"{"error":{"message":"model temporarily unavailable","type":"codex_router_error","code":"CR-UP-0011"}}"#
                .to_vec()
        }),
    )
}

pub fn rewrite_exhausted_account_status(status: u16, body: &str) -> u16 {
    if is_exhausted_account_status(status, body) {
        429
    } else {
        status
    }
}

/// Hard "this provider has no usable credential". Waiting a 5s/25s/125s
/// rate-limit ladder will not mint an OAuth token, and Codex disconnects
/// with "error sending request" while the gateway is still silent.
pub fn is_missing_provider_auth(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("auth_unavailable") || lower.contains("no auth available")
}

/// Sub2API reports an account pool drained by upstream rate limiting as 503.
/// Detecting it here lets the gateway retry it like a literal 429 instead of
/// passing it straight through and ending the conversation.
pub fn is_exhausted_account_status(status: u16, body: &str) -> bool {
    if status != 503 {
        return false;
    }
    if is_missing_provider_auth(body) {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    lower.contains("no available accounts")
        || lower.contains("too many requests")
        || lower.contains("rate_limit")
        || lower.contains("rate limit")
        || lower.contains("service temporarily unavailable")
        || lower.contains("model temporarily unavailable")
}

/// Official-subscription 429/cooldown that should fail over to the next
/// Router pool (API relay) instead of parking the whole public model.
/// Grok SuperGrok quota arrives as HTTP 402 `Payment Required`.
#[allow(dead_code)] // consumed by the host data plane; the GUI crate shares this module.
pub fn is_pool_failover_error(status: u16, body: &str) -> bool {
    if status == 402 {
        return true;
    }
    if !matches!(status, 429 | 503) {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    is_quota_exhausted_error(status, body)
        || lower.contains("rate_limit")
        || lower.contains("rate limit")
        || lower.contains("too many requests")
        || is_exhausted_account_status(status, body)
}

/// Quota / subscription exhaustion. The gateway must not 5s/25s/125s retry
/// these: Host should switch pools, and retrying the same account burns the
/// Codex retry budget until "exceeded retry limit".
pub fn is_quota_exhausted_error(status: u16, body: &str) -> bool {
    if status == 402 {
        return true;
    }
    if !matches!(status, 429 | 503) {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    lower.contains("quota")
        || lower.contains("usage_limit")
        || lower.contains("usage limit")
        || lower.contains("resource has been exhausted")
        || lower.contains("resource_exhausted")
        || lower.contains("cooling down")
        || lower.contains("model_cooldown")
        || lower.contains("payment_required")
        || lower.contains("payment required")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_auth_shield_rewrites_401_to_non_auth_503() {
        let (status, body) = shield_desktop_auth_failure(
            401,
            br#"{"error":{"message":"unauthenticated","type":"invalid_request_error","code":"invalid_api_key"}}"#,
        );
        assert_eq!(status, 503);
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["error"]["type"], "codex_router_error");
        assert_eq!(payload["error"]["code"], "CR-UP-0011");
        assert_eq!(payload["error"]["message"], "model temporarily unavailable");
        assert!(!String::from_utf8_lossy(&body)
            .to_ascii_lowercase()
            .contains("unauthenticated"));
        assert!(!String::from_utf8_lossy(&body).contains("401"));
        assert_eq!(
            rewrite_poisoned_upstream_status(401, "unauthenticated"),
            503
        );
        let (ok_status, ok_body) = shield_desktop_auth_failure(429, b"rate limit");
        assert_eq!(ok_status, 429);
        assert_eq!(ok_body, b"rate limit");
    }

    #[test]
    fn openai_family_detection_covers_chatgpt_slugs() {
        assert!(is_openai_family_model("gpt-5.6-sol"));
        assert!(is_openai_family_model("~gpt-5.6-luna"));
        assert!(is_openai_family_model("openai/codex-mini-latest"));
        assert!(!is_openai_family_model("grok-4.6"));
        assert!(!is_openai_family_model("claude-fable-5"));
        assert!(is_claude_family_model(
            "cr_r16_antigravity/claude-opus-4.6-thinking"
        ));
        assert!(is_claude_family_model("claude-fable-5"));
        assert!(!is_claude_family_model("grok-4.6"));
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
    fn grok_official_compaction_blob_is_preserved() {
        let mut body = json!({
            "model": "grok-4.6",
            "stream": true,
            "tools": [{"type":"function","name":"exec_command"}],
            "input": [
                {
                    "type": "compaction",
                    "encrypted_content": "xai-compact-blob-abcdefghijklmnopqrstuvwxyz012345"
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
        assert_eq!(body["input"][0]["type"], "compaction");
        assert_eq!(
            body["input"][0]["encrypted_content"],
            "xai-compact-blob-abcdefghijklmnopqrstuvwxyz012345"
        );
        prepare_xai_official_compact_request(&mut body);
        assert!(body.get("stream").is_none());
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn chatgpt_official_compact_request_drops_generation_controls() {
        let mut body = json!({
            "model": "gpt-5.6-sol",
            "stream": true,
            "tools": [{"type":"function","name":"exec_command"}],
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "include": ["reasoning.encrypted_content"],
            "max_output_tokens": 128000,
            "reasoning": {"effort": "high"},
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "compact this"}]
                }
            ]
        });
        prepare_official_compact_request(&mut body);
        assert_eq!(body["model"], "gpt-5.6-sol");
        assert!(body.get("stream").is_none());
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
        assert!(body.get("parallel_tool_calls").is_none());
        assert!(body.get("include").is_none());
        assert!(body.get("max_output_tokens").is_none());
        assert!(body.get("reasoning").is_none());
        assert_eq!(body["input"][0]["type"], "message");
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
    fn grok_request_simplifies_codex_app_automation_schema() {
        let mut body = json!({
            "model": "cr_r10_xai/grok-4.6",
            "reasoning": {"effort": "high", "summary": "detailed"},
            "text": {"verbosity": "low"},
            "tools": [
                {
                    "type": "function",
                    "name": "exec_command",
                    "parameters": {
                        "type": "object",
                        "properties": {"cmd": {"type": "string"}}
                    }
                },
                {
                    "type": "namespace",
                    "name": "mcp__codex_app",
                    "tools": [{
                        "type": "function",
                        "name": "automation_update",
                        "parameters": {
                            "type": "object",
                            "oneOf": [{"type": "object"}, {"type": "null"}],
                            "$defs": {"__schema0": {"type": "object"}},
                            "properties": {"mode": {"type": "string"}}
                        }
                    }]
                },
                {
                    "type": "function",
                    "name": "mcp__codex_app__automation_update",
                    "parameters": {
                        "type": "object",
                        "oneOf": [{"type": "object"}, {"type": "null"}],
                        "$defs": {"x": {"$ref": "#/$defs/x"}},
                        "properties": {"mode": {"type": "string"}}
                    }
                },
                {"type": "web_search"}
            ],
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "hi"}]
            }]
        });
        let stats = sanitize_responses_request("/v1/responses", &mut body);
        assert!(stats.rewritten_tool_calls >= 1);
        let safe = json!({
            "type": "object",
            "properties": {},
            "additionalProperties": true
        });
        assert_eq!(body["tools"][1]["tools"][0]["parameters"], safe);
        assert_eq!(body["tools"][2]["parameters"], safe);
        assert_eq!(body["tools"][0]["name"], "exec_command");
        assert_eq!(
            body["tools"][0]["parameters"]["properties"]["cmd"]["type"],
            "string"
        );
        assert_eq!(body["tools"][3]["type"], "web_search");
        assert_eq!(body["reasoning"]["effort"], "high");
        assert!(body["reasoning"].get("summary").is_none());
        assert!(body.get("text").is_none() || body["text"].get("verbosity").is_none());
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
    fn third_party_old_tool_outputs_are_pruned_without_full_compact() {
        let mut input = vec![json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "fix the build"}]
        })];
        for index in 0..12 {
            input.push(json!({
                "type": "function_call",
                "name": "exec_command",
                "call_id": format!("c{index}"),
                "arguments": "{\"cmd\":\"cargo test\"}"
            }));
            input.push(json!({
                "type": "function_call_output",
                "call_id": format!("c{index}"),
                "output": "x".repeat(8_000)
            }));
        }
        input.push(json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "continue"}]
        }));
        let original_len = input.len();
        let mut body = json!({ "model": "grok-4.6", "input": input });
        let stats = sanitize_responses_request("/v1/responses", &mut body);
        assert!(!stats.locally_compacted);
        let items = body["input"].as_array().unwrap();
        assert_eq!(items.len(), original_len);
        let old_output = items[2]["output"].as_str().unwrap();
        assert!(old_output.chars().count() <= TOOL_OUTPUT_PRUNE_CHARS);
        assert!(old_output.ends_with('…'));
        let latest_output = items[items.len() - 2]["output"].as_str().unwrap();
        assert_eq!(latest_output.len(), 8_000);
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
        assert!(notice.contains("这不是新任务"));
        assert!(notice.contains("## Goal"));
        assert!(notice.contains("## Pending"));
        assert!(notice.contains("turn 0"));
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
    fn grok_codex_compact_handoff_is_rewritten_into_continuation() {
        let mut body = json!({
            "model": "grok-4.6",
            "input": [
                {
                    "type":"message",
                    "role":"user",
                    "content":[{"type":"input_text","text":"Another language model started to solve this problem and produced a summary of its thinking process. You also have access to the state of the tools that were used by that language model. Use this to build on the work that has already been done and avoid duplicating work. Here is the summary produced by the other language model, use the information in this summary to assist with your own analysis:\n# Handoff\n还差把优先级统一成账号 priority。"}]
                }
            ]
        });
        let stats = sanitize_responses_request("/v1/responses", &mut body);
        assert!(stats.converted_items >= 1);
        let text = body["input"][0]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("自动压缩后续跑"));
        assert!(text.contains("不要输出空回复"));
        assert!(text.contains("优先级统一成账号 priority"));
        assert!(!text.contains("Another language model started"));
    }

    fn grok_mid_agent_body() -> Value {
        json!({
            "model": "grok-4.6",
            "tools": [{"type":"function","name":"exec_command"}],
            "input": [
                {"type":"message","role":"user","content":[{"type":"input_text","text":"修登录循环"}]},
                {"type":"function_call","name":"exec_command","call_id":"c1","arguments":"{\"cmd\":\"ls\"}"},
                {"type":"function_call_output","call_id":"c1","output":"ok"}
            ],
            "previous_response_id": "resp_grok_real",
        })
    }

    #[test]
    fn grok_mid_agent_commentary_without_tools_is_continued() {
        let body = grok_mid_agent_body();
        assert!(request_is_mid_agent_turn(&body));
        assert!(assistant_text_looks_like_incomplete_stop(
            "我先对照 0.2.15 截图"
        ));
        assert!(should_continue_incomplete_agent_turn(
            "grok-4.6",
            &body,
            "我先对照 0.2.15 截图",
            false,
        ));
        assert!(should_continue_incomplete_agent_turn(
            "grok-4.6",
            &body,
            "我改用看图工具",
            false,
        ));
        assert!(should_continue_incomplete_agent_turn(
            "grok-4.6", &body, "\n", false,
        ));
        assert!(!should_continue_incomplete_agent_turn(
            "grok-4.6",
            &body,
            "我先对照 0.2.15 截图",
            true,
        ));
        assert!(!should_continue_incomplete_agent_turn(
            "gpt-5.6-sol",
            &body,
            "我先对照 0.2.15 截图",
            false,
        ));
        assert!(!should_continue_incomplete_agent_turn(
            "gemini-3.7-flash",
            &body,
            "我先对照 0.2.15 截图",
            false,
        ));
        assert!(!should_continue_incomplete_agent_turn(
            "deepseek-v4-flash",
            &body,
            "\n",
            false,
        ));
        assert!(!should_continue_incomplete_agent_turn(
            "grok-4.6",
            &body,
            "任务已完成。登录循环已按 3.0.2 修好。",
            false,
        ));
    }

    #[test]
    fn grok_plain_answer_without_tools_is_not_continued() {
        let body = json!({
            "model": "grok-4.6",
            "input": [
                {"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}
            ]
        });
        assert!(!should_continue_incomplete_agent_turn(
            "grok-4.6",
            &body,
            "hello world",
            false,
        ));
        let with_tools = json!({
            "model": "grok-4.6",
            "tools": [{"type":"function","name":"exec_command"}],
            "input": [
                {"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}
            ]
        });
        assert!(should_continue_incomplete_agent_turn(
            "grok-4.6",
            &with_tools,
            "我先看一下仓库结构",
            false,
        ));
        assert!(!should_continue_incomplete_agent_turn(
            "grok-4.6",
            &with_tools,
            "hello world",
            false,
        ));
    }

    #[test]
    fn incomplete_turn_continuation_appends_nudge_and_drops_previous_response() {
        let mut body = grok_mid_agent_body();
        assert!(append_incomplete_turn_continuation(
            &mut body,
            "我先对照 0.2.15 截图",
        ));
        assert!(body.get("previous_response_id").is_none());
        let items = body["input"].as_array().unwrap();
        assert_eq!(items[items.len() - 2]["role"], "assistant");
        assert!(items[items.len() - 2]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("我先对照"));
        let nudge = items[items.len() - 1]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(nudge.contains(INCOMPLETE_TURN_CONTINUE_MARKER));
        assert!(nudge.contains("不是任务结束"));
        assert!(nudge.contains("不要回复「任务已完成」"));
        assert!(!nudge.contains("若任务其实已经做完"));
        assert!(request_is_mid_agent_turn(&body));
        assert!(should_continue_incomplete_agent_turn(
            "grok-4.6",
            &body,
            "接下来再看图",
            false,
        ));
    }

    #[test]
    fn grok_long_verdict_with_next_step_is_not_continued() {
        let body = grok_mid_agent_body();
        let verdict = "**Verdict：T01 ACCEPT，Feature 不能 PASS。**\n\nCoder 报告只交付了 v0.3/T01。Debugger 不能把整 Feature 关掉。T02–T09 还没做。\n\n## 独立核过什么\n\n- 对照 manager T01：Interface + fake app-server\n- 独立跑：3 files / 22 tests passed\n- CodeGraph 已 sync\n\n## 文档\n\n- debugger_0.3.0.md\n- PROJECT_STATUS.md\n\n下一步：Coder 做 v0.3/T02，直接连官方 codex app-server。整 Feature 在 T09 cutover 前都不会 Feature PASS。";
        assert!(verdict.chars().count() > 200);
        assert!(assistant_text_looks_like_incomplete_stop(verdict));
        assert!(assistant_text_looks_finished(verdict));
        assert!(!should_continue_incomplete_agent_turn(
            "grok-4.6", &body, verdict, false,
        ));
        let fail_report = "**Verdict：`FAIL`。** v0.3.0 Feature 不能 Closeout。\n\n独立验收看到的是 mock JSON-RPC，不是官方 app-server。没有真实 round-trip，不能 Feature PASS。\n\n阻断 Findings：方法未锁到官方协议；model/list 失败时静默返回内置 GPT 列表。\n\n下一步：Coder 做 Fix Cycle v0.3.1。先对齐官方 V2 协议，再给一份非 synthetic 的真实握手证据。";
        assert!(fail_report.chars().count() > 200);
        assert!(!should_continue_incomplete_agent_turn(
            "grok-4.6",
            &body,
            fail_report,
            false,
        ));
        assert!(should_continue_incomplete_agent_turn(
            "grok-4.6",
            &body,
            "下一步我打开那个文件",
            false,
        ));
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
    fn gemini_antigravity_drops_stale_server_handles_and_thought_carriers() {
        let carrier = "cpa-gemini-responses-carrier-v1:next:function:RXRFT0NzNE9BUkZOTWc4aVFsaVY1TjZGTDBuMEc5b3g0QVRRS21GMFRhaXNhdjVPSlZVaDhob3FsblN6WWIwZFo1WUh6RmdTK2RTOVFoVGZONldBUjVqLzUwUWMvRlF4SVBCSDh3VXZLYUNCMWFlOGJ2bVpzajNnVFFWMnZaMVQrSkpzU3VjTFRJODAraExzUy8rSnBIMTVnZmZCZW03QWYxNFQ3NFNucndqbTluMkdSVWJtM2dpbnJjUEEwZUpIK3VGRGtLUWFmNDFPZHRzVFJtcU1uQ1RMcDVqMnoxN2JRN0MxWkROOGlvUWRHb2gyREtpRlpubWRHN3ZFYUNZU0tkcDNrR05JS0dUMFpJYWhMUHJibmdNUE5zbUpaZmZaZitFbVNqcllnYjZXbnJOaGwybmlHY3J4dlpkeGkwdUgrRHUrazVWVnJNL3JxRHpsMVZTbUF3WHFlcmd3WGlOakRCY3QzQ214L0habnF5L0lqQnc2NDNKUm5rKysyd080enZnUmNYMkkyOE5IdGVGclFsdUFaN2YySGo4d0lHemg3UlhaNFFiWTVxMS96anNwOEYyVUtKZGI0TTNZcEIxbUlabFUvTVNUbnM5OE1ldStYa1BnRWxRT1dyanhzM2tveUg1YWJYQ0w5aGdJT0d6Q0RMamo5UUx5YXRwMUN2MU";
        let mut body = json!({
            "model": "gemini-3.7-flash",
            "previous_response_id": "resp_8HuKas2mGpnG0-kPus-UwAM",
            "conversation_id": "conv_gemini_stale",
            "conversation": {"id": "conv_gemini_stale"},
            "input": [
                {
                    "type": "reasoning",
                    "id": "rs_resp_8HuKas2mGpnG0-kPus-UwAM_0",
                    "encrypted_content": carrier,
                    "output": carrier,
                    "summary": [{"type":"summary_text","text":"Analyzing HPC"}]
                },
                {
                    "type": "function_call",
                    "name": "exec_command",
                    "call_id": "c1",
                    "arguments": "{\"cmd\":\"ls\"}",
                    "thought_signature": carrier
                }
            ]
        });
        let stats = sanitize_responses_request("/v1/responses", &mut body);
        assert!(stats.converted_items >= 1 || stats.stripped_encrypted >= 1);
        assert!(body.get("previous_response_id").is_none());
        assert!(body.get("conversation_id").is_none());
        assert!(body.get("conversation").is_none());
        let reasoning = &body["input"][0];
        assert!(reasoning.get("encrypted_content").is_none());
        assert!(reasoning.get("output").is_none());
        assert!(reasoning["summary"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Analyzing HPC"));
        assert!(body["input"][1].get("thought_signature").is_none());
        assert_eq!(body["input"][1]["name"], "exec_command");

        let mut pooled = json!({
            "model": "cr_r13_antigravity/gemini-3.7-flash",
            "previous_response_id": "resp_stale",
            "input": [{
                "type": "reasoning",
                "output": carrier
            }]
        });
        sanitize_responses_request("/v1/responses", &mut pooled);
        assert!(pooled.get("previous_response_id").is_none());
        assert!(pooled["input"][0].get("output").is_none());
    }

    #[test]
    fn gemini_entity_not_found_is_retryable_without_retrying_missing_routes() {
        assert!(is_missing_upstream_entity_error(
            r#"{"error":{"message":"Requested entity was not found.","status":"NOT_FOUND"}}"#
        ));
        assert!(should_retry_after_upstream_error(
            404,
            r#"{"error":{"message":"Requested entity was not found.","status":"NOT_FOUND"}}"#
        ));
        assert!(!should_retry_after_upstream_error(
            404,
            r#"{"error":{"code":"CR-RTE-0001","message":"no route for model gemini-3.7-flash"}}"#
        ));
    }

    #[test]
    fn chatgpt_weekly_cooldown_fails_over_to_the_next_pool() {
        assert!(is_pool_failover_error(
            429,
            r#"{"error":{"code":"model_cooldown","message":"All credentials for model cr_r1_openai/gpt-5.6-sol are cooling down","reset_seconds":480804}}"#
        ));
        assert!(is_pool_failover_error(
            429,
            r#"{"error":{"message":"usage_limit_reached"}}"#
        ));
        assert!(!is_pool_failover_error(
            400,
            r#"{"error":{"message":"invalid_request"}}"#
        ));
        assert!(!is_pool_failover_error(200, "ok"));
        assert!(is_pool_failover_error(
            402,
            r#"{"error":{"message":"Payment Required","type":"invalid_request_error"}}"#
        ));
        assert!(is_pool_failover_error(
            429,
            r#"{"error":{"code":"429","message":"Resource has been exhausted (e.g. check quota.)"}}"#
        ));
        assert!(is_quota_exhausted_error(
            429,
            r#"{"error":{"code":"429","message":"Resource has been exhausted (e.g. check quota.)"}}"#
        ));
        assert!(is_quota_exhausted_error(
            429,
            r#"{"error":{"code":"model_cooldown","message":"All credentials for model cr_r13_antigravity/gemini-3.7-flash are cooling down"}}"#
        ));
        assert!(!is_quota_exhausted_error(429, r#"{"error":"rate limit"}"#));
    }

    #[test]
    fn orphaned_function_response_is_pruned_to_text() {
        // Cross-thread agent history: a forked sub-agent carried a function
        // response whose originating call is not in this request. Gemini
        // rejects that with "invalid Gemini function call history".
        let mut items = vec![
            json!({"type":"function_call","name":"exec_command","call_id":"call-a"}),
            json!({"type":"function_call_output","call_id":"call-a","output":"ok"}),
            json!({"type":"function_call_output","call_id":"call-orphan","output":"stale cross-thread"}),
        ];
        let pruned = prune_unmatched_tool_outputs(&mut items);
        assert_eq!(pruned, 1);
        assert!(items
            .iter()
            .any(|item| item.get("call_id").and_then(Value::as_str) == Some("call-a")));
        assert!(!items
            .iter()
            .any(|item| item.get("call_id").and_then(Value::as_str) == Some("call-orphan")));
        // The orphaned output survives as a plain text message so context is
        // not silently lost.
        assert!(items
            .iter()
            .any(|item| item.get("type").and_then(Value::as_str) == Some("message")));
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
    fn muse_clamps_80_char_mcp_tool_name_and_restores_it_on_the_way_back() {
        let long =
            "mcp__openai_api_key_local_confirmation__confirm_openai_api_key_local_destination";
        assert_eq!(long.chars().count(), 80);
        let short = shorten_openai_compat_tool_name(long);
        assert_eq!(short.chars().count(), 64);
        assert_ne!(short, long);

        let mut body = json!({
            "model": "meta/muse-spark-1.2-contributor",
            "tools": [{
                "type": "function",
                "name": long,
                "description": "Confirm the local API key destination.",
                "parameters": {"type":"object","properties":{}}
            }],
            "input": [{
                "type": "function_call",
                "name": long,
                "call_id": "call_1",
                "arguments": "{}"
            }]
        });
        let stats = sanitize_responses_request("/v1/responses", &mut body);
        assert!(stats.rewritten_tool_calls >= 1);
        let sent = body["tools"][0]["name"].as_str().unwrap();
        assert_eq!(sent.chars().count(), 64);
        assert_eq!(body["input"][0]["name"], sent);
        assert!(body["tools"][0]["description"]
            .as_str()
            .unwrap()
            .contains(long));

        let sse = rewrite_sse_text(&format!(
            "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"name\":\"{sent}\"}}}}\n\n"
        ));
        assert!(sse.contains(long));
        assert!(!sse.contains(&format!("\"name\":\"{sent}\"")));
        clear_tool_name_restore();
    }

    #[test]
    fn muse_clamps_namespaced_mcp_child_before_cliproxy_expands_it() {
        let namespace = "mcp__openai_api_key_local_confirmation";
        let local = "confirm_openai_api_key_local_destination";
        let expanded = format!("{namespace}__{local}");
        assert_eq!(expanded.chars().count(), 80);

        let mut body = json!({
            "model": "meta/muse-spark-1.2-contributor",
            "tools": [{
                "type": "namespace",
                "name": namespace,
                "tools": [{
                    "type": "function",
                    "name": local,
                    "description": "Confirm the local API key destination.",
                    "parameters": {"type":"object","properties":{}}
                }]
            }],
            "input": [{
                "type": "function_call",
                "name": local,
                "namespace": namespace,
                "call_id": "call_1",
                "arguments": "{}"
            }]
        });
        sanitize_responses_request("/v1/responses", &mut body);
        let sent_local = body["tools"][0]["tools"][0]["name"].as_str().unwrap();
        let sent_history = body["input"][0]["name"].as_str().unwrap();
        let qualified = format!("{namespace}__{sent_local}");
        assert_eq!(sent_local, sent_history);
        assert!(qualified.chars().count() <= 64, "qualified={qualified}");
        assert_ne!(sent_local, local);

        let sse = rewrite_sse_text(&format!(
            "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"name\":\"{sent_local}\",\"namespace\":\"{namespace}\"}}}}\n\n"
        ));
        assert!(sse.contains(local));
        clear_tool_name_restore();
    }

    #[test]
    fn openai_family_keeps_long_tool_names() {
        let long =
            "mcp__openai_api_key_local_confirmation__confirm_openai_api_key_local_destination";
        let mut body = json!({
            "model": "gpt-5.6-terra",
            "tools": [{"type":"function","name": long}]
        });
        sanitize_responses_request("/v1/responses", &mut body);
        assert_eq!(body["tools"][0]["name"], long);
        clear_tool_name_restore();
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
    fn spark_think_tags_become_reasoning_summary_deltas() {
        let sse = rewrite_sse_text(
            "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"m1\",\"delta\":\"<think>plan the edit</think>I'll run ls\"}\n\n",
        );
        assert!(sse.contains("response.reasoning_summary_text.delta"));
        assert!(sse.contains("plan the edit"));
        assert!(sse.contains("I'll run ls"));
        assert!(!sse.contains("<think>"));
        assert!(sse.contains("response.output_text.delta"));
        let think_pos = sse.find("plan the edit").unwrap();
        let vis_pos = sse.find("I'll run ls").unwrap();
        assert!(think_pos < vis_pos);
    }

    #[test]
    fn spark_think_tags_stream_across_sse_chunks() {
        let mut state = SseThinkState::default();
        let first = rewrite_sse_text_with(
            &mut state,
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"<think\"}\n\n",
        );
        assert!(!first.contains("reasoning_summary_text.delta"));

        let second = rewrite_sse_text_with(
            &mut state,
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\">read the file first\"}\n\n",
        );
        assert!(second.contains("response.reasoning_summary_text.delta"));
        assert!(second.contains("read the file first"));
        assert!(!second.contains("\"type\":\"response.output_text.delta\""));

        let third = rewrite_sse_text_with(
            &mut state,
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"</think>now exec\"}\n\n",
        );
        assert!(third.contains("now exec"));
        assert!(third.contains("response.output_text.delta"));
        assert!(third.contains("response.output_item.done"));
    }

    #[test]
    fn unclosed_think_is_flushed_on_completed() {
        let mut state = SseThinkState::default();
        let mid = rewrite_sse_text_with(
            &mut state,
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"<think>still thinking\"}\n\n",
        );
        assert!(mid.contains("still thinking"));
        assert!(mid.contains("reasoning_summary_text.delta"));
        let done = rewrite_sse_text_with(
            &mut state,
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\"}}\n\n",
        );
        assert!(done.contains("response.completed"));
        assert!(done.contains("response.output_item.done"));
        assert!(done.contains("\"type\":\"reasoning\""));
    }

    #[test]
    fn less_than_in_output_text_is_not_held_as_think_tag() {
        let sse = rewrite_sse_text(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"if a < b {\"}\n\n",
        );
        assert!(sse.contains("if a < b {"));
        assert!(!sse.contains("reasoning_summary"));
    }

    #[test]
    fn reasoning_encrypted_content_is_not_stripped() {
        let raw = "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"reasoning\",\"encrypted_content\":\"abc<think>def\"}}\n\n";
        let sse = rewrite_sse_text(raw);
        assert!(sse.contains("abc<think>def"));
        assert!(!sse.contains("reasoning_summary_text.delta"));
    }

    #[test]
    fn openai_family_think_rewrite_is_disabled() {
        let mut state = SseThinkState::disabled();
        let raw = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"<think>secret</think>hello\"}\n\n";
        let sse = rewrite_sse_text_with(&mut state, raw);
        assert_eq!(sse, raw);
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
        assert!(should_retry_after_upstream_error(
            404,
            "Requested entity was not found."
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
        assert!(!is_exhausted_account_status(
            503,
            "auth_unavailable: no auth available (providers=antigravity, model=cr_r13_antigravity/gemini-3.7-flash)"
        ));
        assert!(is_missing_provider_auth(
            "auth_unavailable: no auth available (providers=antigravity, model=cr_r13_antigravity/gemini-3.7-flash)"
        ));
        assert!(is_exhausted_account_status(503, "no available accounts"));
    }
}
