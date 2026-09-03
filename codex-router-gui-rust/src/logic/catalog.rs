use crate::config::{ModelConfig, RouterConfig};
use crate::logic::{
    canonical_route_model_id, is_eligible_oauth_api_fallback, model_identity, model_route_policy,
    recommended_model_display_name, resolve_context_window, resolve_multimodal, resolve_reasoning,
    same_model_identity, slugify, ModelRoutePolicy,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Path;

/// Simple instructions for non-OpenAI OAuth models that cannot consume Codex's
/// native multi-agent protocol metadata.
const RESTRICTED_OAUTH_INSTRUCTIONS: &str = "你是编程助手。请完整遵循用户指令。\n默认使用简体中文回复，不要在中英文之间无序切换。仅在用户明确要求其他语言，或必须引用英文标识符、代码、命令、路径、日志时使用英文。\n以当前工作目录为准完成任务。若 cwd 是 CodexRouter 项目或其子目录，可以按需要读取该项目文件（包括 Test/）。若 cwd 不是 CodexRouter，不要去扫描 D:\\\\Work\\\\CodexRouter 源码树。不要把其他对话的内容混入本轮。\n调用工具时必须使用系统提供的结构化 tool_calls。执行命令的工具名是 exec_command，参数字段是 cmd。禁止在正文中输出 functions__exec、functions.exec 或任何纯文本函数标记。\n不要输出协议元数据、控制 JSON、reasoning 信封或 {\"type\":\"reasoning_text\"} 这类内部标签。工具成功后必须继续完成用户任务，不要把单次命令成功当成对话结束。\nCodex 把「只有正文、没有 function_call」当成任务结束。任务完成前每一轮都必须发出结构化工具调用，不要只写计划或「接下来」然后停。";

const LANGUAGE_AND_ISOLATION_CLAUSE: &str = "\n\n# 输出语言与会话隔离\n默认使用简体中文回复。不要在中英文之间无序切换。仅当用户明确要求其他语言，或必须引用英文标识符、代码、命令、路径、日志时，才使用英文。\n以当前工作目录为准完成任务。若 cwd 是 CodexRouter 项目或其子目录，可以按需要读取该项目文件（包括 Test/）。若 cwd 不是 CodexRouter，不要去扫描 D:\\\\Work\\\\CodexRouter 源码树。不要把其他对话的内容混入本轮。\n调用工具时必须使用系统提供的结构化 tool_calls。执行命令的工具名是 exec_command，参数字段是 cmd。禁止在正文中输出 functions__exec、functions.exec 或任何纯文本函数标记。\n工具成功后必须继续完成用户任务，不要把单次命令成功当成对话结束。\nCodex 把「只有正文、没有 function_call」当成任务结束。任务完成前每一轮都必须发出结构化工具调用，不要只写计划或「接下来」然后停。";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRoute {
    pub index: usize,
    /// The full model configuration used by this route. Secrets are skipped
    /// during serialization.
    pub model: ModelConfig,
    pub source: String,
    pub canonical_model_id: String,
    pub identity_key: String,
    pub identity_display_candidate: String,
    pub public_model_id: String,
    pub request_model_ids: Vec<String>,
    pub include_in_catalog: bool,
    pub join_router: bool,
    pub is_oauth_fallback: bool,
    pub is_merged_oauth_route: bool,
    pub served_by: String,
}

#[derive(Clone, Debug)]
struct RouteDescriptor {
    index: usize,
    model: ModelConfig,
    model_id: String,
    source: String,
    selected: bool,
    canonical_model_id: String,
    identity_key: String,
    identity_display_candidate: String,
    discovered: bool,
}

fn model_source(model: &ModelConfig) -> String {
    let source = model.source.trim().to_ascii_lowercase();
    if source.is_empty() {
        "apikey".to_owned()
    } else {
        source
    }
}

fn is_oauth(model: &ModelConfig) -> bool {
    model_source(model) == "oauth"
}

fn canonical_public_model_id(model_id: &str) -> String {
    let identity = model_identity(model_id);
    if identity.provider.starts_with("unknown") {
        model_id.trim().to_owned()
    } else {
        identity.real_id
    }
}

fn split_public_model_id(model: &ModelConfig) -> String {
    let canonical = canonical_route_model_id(&model.model);
    let slug = slugify(&canonical);
    let seed = format!(
        "{}\n{}\n{}",
        model.model.trim().to_ascii_lowercase(),
        model
            .base_url
            .trim()
            .trim_end_matches('/')
            .to_ascii_lowercase(),
        model.credential_name.trim().to_ascii_lowercase(),
    );
    let digest = Sha256::digest(seed.as_bytes());
    let token = digest[..6]
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>();
    format!("{}--api-{}", slug, token)
}

fn oauth_platform_for_catalog(model: &ModelConfig) -> String {
    if !model.oauth_platform.trim().is_empty() {
        return model.oauth_platform.trim().to_ascii_lowercase();
    }
    let id = model.model.trim().to_ascii_lowercase();
    if id.contains("gemini") {
        return "gemini".to_owned();
    }
    if id.contains("grok") {
        return "grok".to_owned();
    }
    if id.contains("claude") {
        return "anthropic".to_owned();
    }
    if id.starts_with("gpt-")
        || (id.starts_with('o')
            && id
                .chars()
                .nth(1)
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false))
        || id.starts_with("codex-")
    {
        return "openai".to_owned();
    }
    String::new()
}

fn is_restricted_oauth_model(model: &ModelConfig) -> bool {
    if !is_oauth(model) {
        return false;
    }
    matches!(
        oauth_platform_for_catalog(model).as_str(),
        "antigravity"
            | "gemini"
            | "google"
            | "google_one"
            | "grok"
            | "xai"
            | "x-ai"
            | "claude"
            | "anthropic"
    )
}

fn is_openai_catalog_model(model: &ModelConfig) -> bool {
    matches!(
        oauth_platform_for_catalog(model).as_str(),
        "openai" | "chatgpt"
    ) && is_oauth(model)
}

/// Build the route plan that determines which model entries appear in the Codex
/// catalog and which channels participate in the Sub2API routing.
pub fn build_route_plan(cfg: &RouterConfig) -> Vec<ModelRoute> {
    let mut descriptors = Vec::new();
    for (index, model) in cfg.models.iter().enumerate() {
        let model_id = model.model.trim().to_string();
        if model_id.is_empty() {
            panic!("Model entry #{} has an empty model ID.", index + 1);
        }
        let source = model_source(model);
        let selected = !is_oauth(model)
            || cfg.oauth_account_ids.is_none()
            || cfg
                .oauth_account_ids
                .as_ref()
                .unwrap()
                .contains(&model.oauth_account_id);
        let identity = model_identity(&model_id);
        descriptors.push(RouteDescriptor {
            index,
            model: model.clone(),
            model_id,
            source,
            selected,
            canonical_model_id: canonical_route_model_id(&model.model),
            identity_key: format!("{}:{}", identity.provider, identity.real_id),
            identity_display_candidate: identity.display_candidate,
            discovered: false,
        });
    }

    let selected_oauth: Vec<RouteDescriptor> = descriptors
        .iter()
        .filter(|d| d.source == "oauth" && d.selected)
        .cloned()
        .collect();
    let fallback_enabled = cfg.oauth_fallback.enabled;

    let mut catalog_ids = HashSet::new();
    descriptors
        .iter()
        .map(|d| {
            let policy_allows_fallback = fallback_enabled
                && model_route_policy(cfg, &d.model_id) != ModelRoutePolicy::SubscriptionOnly;
            let matching_oauth: Vec<RouteDescriptor> = selected_oauth
                .iter()
                .filter(|o| same_model_identity(&o.model_id, &d.model_id))
                .cloned()
                .collect();
            let is_oauth = d.source == "oauth";
            let matching_api_fallbacks: Vec<RouteDescriptor> = if is_oauth && policy_allows_fallback {
                descriptors
                    .iter()
                    .filter(|x| {
                        x.source != "oauth"
                            && same_model_identity(&x.model_id, &d.model_id)
                            && is_eligible_oauth_api_fallback(cfg, &d.model, &x.model)
                    })
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };

            let mut is_fallback = false;
            let mut join_router = d.selected;
            let mut public_model_id = canonical_public_model_id(&d.model_id);
            let mut request_model_ids = vec![public_model_id.clone(), d.model_id.clone()];
            request_model_ids.sort();
            request_model_ids.dedup();
            let include_in_catalog;
            let mut is_merged_oauth_route = false;

            if is_oauth {
                include_in_catalog =
                    d.selected && catalog_ids.insert(public_model_id.to_ascii_lowercase());
            } else if policy_allows_fallback && !matching_oauth.is_empty() {
                is_fallback = matching_oauth
                    .iter()
                    .any(|oauth| is_eligible_oauth_api_fallback(cfg, &oauth.model, &d.model));
                join_router = is_fallback;
                public_model_id = canonical_public_model_id(&matching_oauth[0].model_id);
                let has_explicit_matching_oauth = matching_oauth.iter().any(|o| !o.discovered);
                if has_explicit_matching_oauth {
                    include_in_catalog = false;
                } else {
                    include_in_catalog =
                        catalog_ids.insert(public_model_id.to_ascii_lowercase());
                }
                if is_fallback {
                    request_model_ids = matching_oauth
                        .iter()
                        .map(|o| o.model_id.clone())
                        .collect::<HashSet<_>>()
                        .into_iter()
                        .collect();
                }
            } else if !policy_allows_fallback {
                let same_count = descriptors
                    .iter()
                    .filter(|x| x.selected && same_model_identity(&x.model_id, &d.model_id))
                    .count();
                if same_count > 1 {
                    public_model_id = split_public_model_id(&d.model);
                }
                include_in_catalog = catalog_ids.insert(public_model_id.to_ascii_lowercase());
            } else {
                include_in_catalog = catalog_ids.insert(public_model_id.to_ascii_lowercase());
            }

            if is_oauth && policy_allows_fallback && !matching_api_fallbacks.is_empty() {
                is_merged_oauth_route = true;
            }

            let served_by = if is_oauth { "oauth" } else { "api" };

            ModelRoute {
                index: d.index,
                model: d.model.clone(),
                source: d.source.clone(),
                canonical_model_id: d.canonical_model_id.clone(),
                identity_key: d.identity_key.clone(),
                identity_display_candidate: d.identity_display_candidate.clone(),
                public_model_id,
                request_model_ids,
                include_in_catalog,
                join_router,
                is_oauth_fallback: is_fallback,
                is_merged_oauth_route,
                served_by: served_by.to_owned(),
            }
        })
        .collect()
}

fn route_display_name(route: &ModelRoute) -> String {
    if !route.model.alias.trim().is_empty() {
        return route.model.alias.trim().to_owned();
    }
    recommended_model_display_name(&route.model.model)
}

fn load_catalog_template(router_root: &Path) -> Value {
    let candidates = [
        router_root.join("config").join("models.json"),
        router_root
            .join("config")
            .join("model-catalog.example.json"),
    ];
    for path in candidates {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(doc) = serde_json::from_str::<Value>(&content) {
                let models = doc
                    .get("models")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_else(|| doc.as_array().cloned().unwrap_or_default());
                if let Some(model) = models.iter().find(|m| {
                    m.get("base_instructions")
                        .and_then(Value::as_str)
                        .map(|s| !s.is_empty())
                        .unwrap_or(false)
                        && m.get("model_messages").is_some()
                }) {
                    return model.clone();
                }
            }
        }
    }
    fallback_model_template()
}

fn fallback_model_template() -> Value {
    json!({
        "base_instructions": format!(
            "你是 Codex，编程助手。在用户当前工作目录中工作，遵循用户指令，保留无关改动，并验证已完成的工作。{LANGUAGE_AND_ISOLATION_CLAUSE}"
        ),
        "model_messages": {
            "instructions_template": format!(
                "你是 Codex，编程助手。在用户当前工作目录中工作，遵循用户指令，保留无关改动，并验证已完成的工作。{LANGUAGE_AND_ISOLATION_CLAUSE}"
            ),
            "instructions_variables": null,
            "approvals": null,
            "auto_review": null,
            "permissions": null,
        },
    })
}

fn model_identity_clause(model: &ModelConfig) -> Option<String> {
    if let Some(clause) = crate::logic::responses_compat::third_party_identity_clause(&model.model) {
        return Some(clause);
    }
    if crate::logic::responses_compat::is_openai_family_model(&model.model) {
        return None;
    }
    Some(
        "# 模型身份\n你是当前路由模型，通过 Codex-Router 接入。不要自称 GPT、ChatGPT 或 Codex 官方模型。调用工具时使用系统声明的工具名 exec_command，参数字段为 cmd。\n任务完成前每一轮必须发出结构化 function_call；只写计划或「接下来」而不调用工具会被 Codex 当成收工。\n"
            .to_owned(),
    )
}

fn apply_model_identity(entry: &mut Value, model: &ModelConfig) {
    let Some(clause) = model_identity_clause(model) else {
        return;
    };
    let prompt = format!(
        "{clause}\n{RESTRICTED_OAUTH_INSTRUCTIONS}"
    );
    entry["base_instructions"] = Value::String(prompt.clone());
    if let Some(model_messages) = entry.get_mut("model_messages") {
        model_messages["instructions_template"] = Value::String(prompt);
    }
}

fn apply_language_and_isolation(entry: &mut Value) {
    if let Some(base) = entry.get("base_instructions").and_then(Value::as_str) {
        if !base.contains("默认使用简体中文") {
            entry["base_instructions"] = Value::String(format!("{base}{LANGUAGE_AND_ISOLATION_CLAUSE}"));
        }
    }
    if let Some(template) = entry
        .pointer_mut("/model_messages/instructions_template")
        .and_then(|value| value.as_str().map(str::to_owned))
    {
        if !template.contains("默认使用简体中文") {
            if let Some(slot) = entry.pointer_mut("/model_messages/instructions_template") {
                *slot = Value::String(format!("{template}{LANGUAGE_AND_ISOLATION_CLAUSE}"));
            }
        }
    }
}

#[allow(dead_code)]
pub fn catalog_requires_chatgpt_allowlist(cfg: &RouterConfig) -> bool {
    let plan = build_route_plan(cfg);
    plan.iter()
        .filter(|route| route.include_in_catalog)
        .all(|route| is_openai_catalog_model(&route.model))
        && plan.iter().any(|route| route.include_in_catalog)
}

pub fn build_model_catalog_with_root(cfg: &RouterConfig, router_root: &Path) -> Vec<Value> {
    let template = load_catalog_template(router_root);
    let route_plan = build_route_plan(cfg);
    let visible: Vec<&ModelRoute> = route_plan.iter().filter(|r| r.include_in_catalog).collect();
    let mut entries = Vec::new();
    for (index, route) in visible.iter().enumerate() {
        let model = &route.model;
        let reasoning = resolve_reasoning(model, None);
        let reasoning_levels: Vec<Value> = reasoning
            .levels
            .iter()
            .map(|effort| json!({"effort": effort, "description": format!("{} reasoning level", effort)}))
            .collect();
        let supports_images = resolve_multimodal(model);
        let context_window = resolve_context_window(model);
        let compact_percent = super::clamp_auto_compact_percent(model.auto_compact_percent);
        // Codex Desktop shows `context_window * effective_context_window_percent / 100`
        // as the composer denominator. Keep auto_compact_token_limit on the same
        // percent window so Grok 500k @ 95% becomes 475k, not a stale 400k cache.
        let auto_compact_token_limit = super::resolve_auto_compact_token_limit(model);
        let display_name = route_display_name(route);

        let mut entry = template.clone();
        apply_language_and_isolation(&mut entry);
        apply_model_identity(&mut entry, model);
        entry["slug"] = Value::String(route.public_model_id.clone());
        entry["display_name"] = Value::String(display_name);
        entry["description"] = Value::String(format!("Codex-Router model #{}", index + 1));
        entry["default_reasoning_level"] = Value::String(reasoning.default_level.clone());
        entry["supported_reasoning_levels"] = Value::Array(reasoning_levels);
        entry["input_modalities"] = Value::Array(if supports_images {
            vec![json!("text"), json!("image")]
        } else {
            vec![json!("text")]
        });
        entry["supports_image_detail_original"] = Value::Bool(supports_images);
        entry["supports_vision"] = Value::Bool(supports_images);
        entry["context_window"] = context_window.into();
        entry["max_context_window"] = context_window.into();
        entry["effective_context_window_percent"] = compact_percent.into();
        entry["auto_compact_token_limit"] = auto_compact_token_limit.into();
        entry["shell_type"] = Value::String("shell_command".to_owned());
        entry["visibility"] = Value::String("list".to_owned());
        entry["supported_in_api"] = Value::Bool(true);
        entry["priority"] = (index as i64 + 1).into();
        entry["display_order"] = (index as i64 + 1).into();
        entry["additional_speed_tiers"] = Value::Array(if reasoning.supports_fast {
            vec![json!("fast")]
        } else {
            vec![]
        });
        entry["service_tiers"] = Value::Array(if reasoning.supports_fast {
            vec![json!({
                "id": "priority",
                "name": "Fast",
                "description": "1.5x speed, increased usage",
            })]
        } else {
            vec![]
        });
        entry["availability_nux"] = Value::Null;
        entry["upgrade"] = Value::Null;
        entry["include_skills_usage_instructions"] = Value::Bool(false);
        // ChatGPT 原生 reasoning 事件在 summary=auto 时会在 Desktop 里留一块空思考区。
        // 第三方模型（Gemini / GLM / DeepSeek 等）必须开 summary，否则思考增量被客户端丢掉，只剩最终结果。
        entry["default_reasoning_summary"] = Value::String(if is_openai_catalog_model(model) {
            "none".to_owned()
        } else {
            "auto".to_owned()
        });
        entry["support_verbosity"] = Value::Bool(true);
        entry["default_verbosity"] = Value::String("low".to_owned());
        if is_openai_catalog_model(model) {
            entry["apply_patch_tool_type"] = Value::String("freeform".to_owned());
        } else if let Some(obj) = entry.as_object_mut() {
            obj.remove("apply_patch_tool_type");
        }
        let truncation_limit = (context_window / 2).clamp(64_000, 400_000);
        entry["truncation_policy"] = json!({"mode": "tokens", "limit": truncation_limit});
        entry["supports_parallel_tool_calls"] = Value::Bool(true);
        entry["experimental_supported_tools"] = Value::Array(vec![]);
        entry["comp_hash"] = entry
            .get("comp_hash")
            .cloned()
            .unwrap_or_else(|| Value::String("codex-router-v1".to_owned()));

        if is_restricted_oauth_model(model) || !is_openai_catalog_model(model) {
            apply_model_identity(&mut entry, model);
            entry["use_responses_lite"] = Value::Bool(false);
            entry["multi_agent_version"] = Value::String("v1".to_owned());
            entry["tool_mode"] = Value::String("default".to_owned());
            entry["supports_search_tool"] = Value::Bool(false);
            entry["web_search_tool_type"] = Value::Null;
        } else {
            // Official ChatGPT OAuth still gets web search.
            // Do not copy `code_mode_only` / `use_responses_lite` / v2
            // collaboration from the official catalog template. Through
            // Router/CLIProxy those make Desktop expose `collaboration.spawn_agent`
            // while the model emits empty JSON `{}`, which Desktop rejects as
            // missing field `message`. Keep JSON `exec_command` and v1 agents.
            entry["supports_search_tool"] = Value::Bool(true);
            entry["web_search_tool_type"] = Value::String("text_and_image".to_owned());
            entry["use_responses_lite"] = Value::Bool(false);
            entry["multi_agent_version"] = Value::String("v1".to_owned());
            entry["tool_mode"] = Value::String("default".to_owned());
        }

        // Remove the optional web_search_tool_type field when it is explicitly
        // null, because Codex rejects an explicit JSON null for that typed field.
        if entry
            .get("web_search_tool_type")
            .map(|v| v.is_null())
            .unwrap_or(false)
        {
            if let Some(obj) = entry.as_object_mut() {
                obj.remove("web_search_tool_type");
            }
        }

        entries.push(entry);
    }
    entries
}

#[allow(dead_code)]
pub fn build_model_catalog(cfg: &RouterConfig) -> Vec<Value> {
    build_model_catalog_with_root(cfg, Path::new(""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ModelConfig, OAuthFallback};

    #[test]
    fn merged_oauth_hides_api_fallback_from_catalog() {
        let mut cfg = crate::config::RouterConfig {
            oauth_account_ids: Some(vec![1]),
            oauth_fallback: OAuthFallback {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };
        cfg.models.push(ModelConfig {
            model: "gpt-5.6-sol".to_owned(),
            source: "oauth".to_owned(),
            oauth_account_id: 1,
            ..Default::default()
        });
        cfg.models.push(ModelConfig {
            model: "gpt-5.6-sol".to_owned(),
            source: "apikey".to_owned(),
            base_url: "https://api.example.com/v1".to_owned(),
            credential_name: "key".to_owned(),
            ..Default::default()
        });

        let plan = build_route_plan(&cfg);
        let catalog = build_model_catalog(&cfg);
        let visible: Vec<&ModelRoute> = plan.iter().filter(|r| r.include_in_catalog).collect();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].source, "oauth");
        assert!(visible[0].is_merged_oauth_route);
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0]["slug"], "gpt-5.6-sol");
        assert_eq!(catalog[0]["display_name"], "ChatGPT-5.6-Sol");
    }

    #[test]
    fn split_mode_exposes_distinct_route_ids_with_one_display_name() {
        let mut cfg = crate::config::RouterConfig {
            oauth_account_ids: Some(vec![1]),
            oauth_fallback: OAuthFallback {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        };
        cfg.models.push(ModelConfig {
            model: "gpt-5.6-sol".to_owned(),
            source: "oauth".to_owned(),
            oauth_account_id: 1,
            ..Default::default()
        });
        cfg.models.push(ModelConfig {
            model: "gpt-5.6-sol".to_owned(),
            source: "apikey".to_owned(),
            base_url: "https://api.example.com/v1".to_owned(),
            credential_name: "key".to_owned(),
            ..Default::default()
        });

        let plan = build_route_plan(&cfg);
        let catalog = build_model_catalog(&cfg);
        let visible: Vec<&ModelRoute> = plan.iter().filter(|r| r.include_in_catalog).collect();
        assert_eq!(visible.len(), 2);
        assert_eq!(
            catalog
                .iter()
                .filter(|entry| entry["display_name"] == "ChatGPT-5.6-Sol")
                .count(),
            2
        );
        let api = catalog
            .iter()
            .find(|entry| {
                entry["display_name"] == "ChatGPT-5.6-Sol"
                    && entry["slug"]
                        .as_str()
                        .is_some_and(|slug| slug.contains("--api-"))
            })
            .unwrap();
        assert!(api["slug"].as_str().unwrap().contains("--api-"));
    }

    #[test]
    fn subscription_only_policy_keeps_oauth_and_api_as_separate_routes() {
        let mut cfg = crate::config::RouterConfig {
            oauth_account_ids: Some(vec![1]),
            oauth_fallback: OAuthFallback {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };
        cfg.models.push(ModelConfig {
            model: "grok-4.5".to_owned(),
            source: "oauth".to_owned(),
            oauth_account_id: 1,
            ..Default::default()
        });
        cfg.models.push(ModelConfig {
            model: "x-ai/grok-4.5".to_owned(),
            source: "apikey".to_owned(),
            base_url: "https://api.example.com/v1".to_owned(),
            credential_name: "key".to_owned(),
            ..Default::default()
        });
        crate::logic::set_model_route_policy(
            &mut cfg,
            "grok-4.5",
            crate::logic::ModelRoutePolicy::SubscriptionOnly,
        );

        let plan = build_route_plan(&cfg);
        let oauth = plan.iter().find(|route| route.source == "oauth").unwrap();
        let api = plan.iter().find(|route| route.source != "oauth").unwrap();
        assert!(!oauth.is_merged_oauth_route);
        assert!(!api.is_oauth_fallback);
    }

    #[test]
    fn split_public_model_id_is_stable_for_same_inputs() {
        let model = ModelConfig {
            model: "gpt-5.6-sol".to_owned(),
            base_url: "https://api.example.com/v1".to_owned(),
            credential_name: "key".to_owned(),
            ..Default::default()
        };
        assert_eq!(split_public_model_id(&model), split_public_model_id(&model));
    }

    #[test]
    fn configured_display_name_is_emitted_without_changing_model_id() {
        let cfg = crate::config::RouterConfig {
            models: vec![ModelConfig {
                model: "vendor/model-id".to_owned(),
                alias: "Configured Display Name".to_owned(),
                alias_customized: Some(false),
                ..Default::default()
            }],
            ..Default::default()
        };

        let catalog = build_model_catalog(&cfg);

        assert_eq!(catalog[0]["slug"], "vendor/model-id");
        assert_eq!(catalog[0]["display_name"], "Configured Display Name");
    }

    #[test]
    fn grok_oauth_uses_compatible_multi_agent_protocol() {
        let cfg = crate::config::RouterConfig {
            models: vec![ModelConfig {
                model: "grok-4.6".to_owned(),
                source: "oauth".to_owned(),
                oauth_platform: "grok".to_owned(),
                oauth_account_id: 3,
                ..Default::default()
            }],
            oauth_account_ids: Some(vec![3]),
            ..Default::default()
        };
        let catalog = build_model_catalog(&cfg);
        assert_eq!(catalog[0]["use_responses_lite"], false);
        assert_eq!(catalog[0]["multi_agent_version"], "v1");
        assert_eq!(catalog[0]["shell_type"], "shell_command");
    }

    #[test]
    fn mixed_catalog_keeps_chatgpt_multi_agent_without_default_luna() {
        let cfg = crate::config::RouterConfig {
            models: vec![
                ModelConfig {
                    model: "gpt-5.6-sol".to_owned(),
                    source: "oauth".to_owned(),
                    oauth_platform: "openai".to_owned(),
                    oauth_account_id: 1,
                    ..Default::default()
                },
                ModelConfig {
                    model: "grok-4.6".to_owned(),
                    source: "oauth".to_owned(),
                    oauth_platform: "grok".to_owned(),
                    oauth_account_id: 3,
                    ..Default::default()
                },
            ],
            oauth_account_ids: Some(vec![1, 3]),
            ..Default::default()
        };
        let catalog = build_model_catalog(&cfg);
        let chatgpt = catalog
            .iter()
            .find(|entry| entry["slug"] == "gpt-5.6-sol")
            .unwrap();
        assert_eq!(chatgpt["multi_agent_version"], "v1");
        assert_eq!(chatgpt["tool_mode"], "default");
        assert_eq!(chatgpt["use_responses_lite"], false);
        assert_eq!(chatgpt["shell_type"], "shell_command");
        assert_eq!(chatgpt["apply_patch_tool_type"], "freeform");
    }

    #[test]
    fn catalog_order_and_priority_follow_config_vec() {
        let cfg = crate::config::RouterConfig {
            models: vec![
                ModelConfig {
                    model: "grok-4.6".to_owned(),
                    source: "apikey".to_owned(),
                    priority: 80,
                    ..Default::default()
                },
                ModelConfig {
                    model: "gpt-5.6-sol".to_owned(),
                    source: "apikey".to_owned(),
                    priority: 1,
                    ..Default::default()
                },
                ModelConfig {
                    model: "deepseek-v4-flash".to_owned(),
                    source: "apikey".to_owned(),
                    priority: 10,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let catalog = build_model_catalog(&cfg);
        assert_eq!(catalog[0]["slug"], "grok-4.6");
        assert_eq!(catalog[1]["slug"], "gpt-5.6-sol");
        assert_eq!(catalog[2]["slug"], "deepseek-v4-flash");
        assert_eq!(catalog[0]["priority"], 1);
        assert_eq!(catalog[1]["priority"], 2);
        assert_eq!(catalog[2]["priority"], 3);
        assert!(
            catalog[0]["truncation_policy"]["limit"]
                .as_i64()
                .unwrap()
                >= 64_000
        );
        assert_eq!(catalog[0]["display_order"], 1);
        assert!(catalog[0]["base_instructions"]
            .as_str()
            .unwrap()
            .contains("默认使用简体中文"));
        assert!(catalog[0]["base_instructions"]
            .as_str()
            .unwrap()
            .contains("你是Grok"));
        assert!(!catalog[0]["base_instructions"]
            .as_str()
            .unwrap()
            .contains("GPT-5"));
        assert!(catalog[0].get("apply_patch_tool_type").is_none());
        let chatgpt_api = catalog
            .iter()
            .find(|entry| entry["slug"] == "gpt-5.6-sol")
            .unwrap();
        assert_eq!(chatgpt_api["supports_search_tool"], false);
        assert_eq!(chatgpt_api["multi_agent_version"], "v1");
        assert!(chatgpt_api["additional_speed_tiers"]
            .as_array()
            .is_some_and(|tiers| tiers.iter().any(|tier| tier == "fast")));
        assert!(!catalog_requires_chatgpt_allowlist(&cfg));
    }

    #[test]
    fn glm53_flash_catalog_exposes_low_high_and_streams_reasoning() {
        let cfg = crate::config::RouterConfig {
            models: vec![ModelConfig {
                model: "z-ai/glm-5.3-flash".to_owned(),
                source: "apikey".to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let catalog = build_model_catalog(&cfg);
        assert_eq!(catalog[0]["slug"], "z-ai/glm-5.3-flash");
        assert_eq!(catalog[0]["default_reasoning_level"], "high");
        assert_eq!(catalog[0]["default_reasoning_summary"], "auto");
        let levels: Vec<&str> = catalog[0]["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|level| level["effort"].as_str())
            .collect();
        assert_eq!(levels, vec!["low", "high", "max"]);
    }

    #[test]
    fn gemini_catalog_asks_desktop_to_show_reasoning_summaries() {
        let cfg = crate::config::RouterConfig {
            models: vec![
                ModelConfig {
                    model: "gemini-3.8-flash".to_owned(),
                    source: "apikey".to_owned(),
                    ..Default::default()
                },
                ModelConfig {
                    model: "gpt-5.6-terra".to_owned(),
                    source: "oauth".to_owned(),
                    oauth_platform: "openai".to_owned(),
                    oauth_account_id: 1,
                    ..Default::default()
                },
            ],
            oauth_account_ids: Some(vec![1]),
            ..Default::default()
        };
        let catalog = build_model_catalog(&cfg);
        let gemini = catalog
            .iter()
            .find(|entry| entry["slug"] == "gemini-3.8-flash")
            .unwrap();
        let chatgpt = catalog
            .iter()
            .find(|entry| entry["slug"] == "gpt-5.6-terra")
            .unwrap();
        assert_eq!(gemini["default_reasoning_summary"], "auto");
        assert_eq!(chatgpt["default_reasoning_summary"], "none");
    }

    #[test]
    fn case_variants_share_one_catalog_row_in_config_order() {
        let cfg = crate::config::RouterConfig {
            models: vec![
                ModelConfig {
                    model: "grok-4.6".to_owned(),
                    ..Default::default()
                },
                ModelConfig {
                    model: "DeepSeek-V4-Flash".to_owned(),
                    base_url: "https://school.example/v1".to_owned(),
                    credential_name: "school".to_owned(),
                    ..Default::default()
                },
                ModelConfig {
                    model: "deepseek-v4-flash".to_owned(),
                    base_url: "https://plan.example/v1".to_owned(),
                    credential_name: "plan".to_owned(),
                    ..Default::default()
                },
                ModelConfig {
                    model: "gpt-5.6-sol".to_owned(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let plan = build_route_plan(&cfg);
        let catalog = build_model_catalog(&cfg);
        assert_eq!(catalog.len(), 3);
        assert_eq!(catalog[0]["slug"], "grok-4.6");
        assert_eq!(catalog[1]["slug"], "deepseek-v4-flash");
        assert_eq!(catalog[2]["slug"], "gpt-5.6-sol");
        let deepseek_routes = plan
            .iter()
            .filter(|route| route.canonical_model_id == "deepseek-v4-flash")
            .collect::<Vec<_>>();
        assert_eq!(deepseek_routes.len(), 2);
        assert!(deepseek_routes[0].include_in_catalog);
        assert!(!deepseek_routes[1].include_in_catalog);
        assert!(deepseek_routes.iter().all(|route| {
            route.public_model_id == "deepseek-v4-flash"
                && route
                    .request_model_ids
                    .iter()
                    .any(|id| id == "deepseek-v4-flash")
        }));
    }

    #[test]
    fn official_search_stays_on_chatgpt_oauth_only() {
        let oauth_cfg = crate::config::RouterConfig {
            models: vec![ModelConfig {
                model: "gpt-5.6-sol".to_owned(),
                source: "oauth".to_owned(),
                oauth_platform: "openai".to_owned(),
                oauth_account_id: 1,
                ..Default::default()
            }],
            oauth_account_ids: Some(vec![1]),
            ..Default::default()
        };
        let relay_cfg = crate::config::RouterConfig {
            models: vec![ModelConfig {
                model: "openai/gpt-5.6-sol".to_owned(),
                source: "apikey".to_owned(),
                base_url: "https://openrouter.ai/api/v1".to_owned(),
                credential_name: "or".to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let gemini = ModelConfig {
            model: "gemini-3.1-pro-high".to_owned(),
            source: "oauth".to_owned(),
            oauth_account_id: 2,
            ..Default::default()
        };
        let oauth = &build_model_catalog(&oauth_cfg)[0];
        let relay = &build_model_catalog(&relay_cfg)[0];
        assert_eq!(oauth["supports_search_tool"], true);
        assert_eq!(oauth["multi_agent_version"], "v1");
        assert_eq!(oauth["tool_mode"], "default");
        assert_eq!(oauth["use_responses_lite"], false);
        assert_eq!(relay["supports_search_tool"], false);
        assert_eq!(relay["multi_agent_version"], "v1");
        assert_eq!(oauth_platform_for_catalog(&gemini), "gemini");
    }

    #[test]
    fn kimi_and_deepseek_drop_gpt_persona_and_use_default_tools() {
        let cfg = crate::config::RouterConfig {
            models: vec![
                ModelConfig {
                    model: "kimi-for-coding".to_owned(),
                    source: "apikey".to_owned(),
                    ..Default::default()
                },
                ModelConfig {
                    model: "deepseek-v4-flash".to_owned(),
                    source: "apikey".to_owned(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let catalog = build_model_catalog(&cfg);
        let kimi = catalog
            .iter()
            .find(|entry| entry["slug"] == "kimi-for-coding")
            .unwrap();
        let deepseek = catalog
            .iter()
            .find(|entry| entry["slug"] == "deepseek-v4-flash")
            .unwrap();
        for entry in [kimi, deepseek] {
            let instructions = entry["base_instructions"].as_str().unwrap();
            assert!(instructions.starts_with("# 模型身份"));
            assert!(!instructions.contains("GPT-5"));
            assert_eq!(entry["use_responses_lite"], false);
            assert_eq!(entry["tool_mode"], "default");
            assert_eq!(entry["multi_agent_version"], "v1");
            assert_eq!(entry["shell_type"], "shell_command");
            assert!(entry.get("apply_patch_tool_type").is_none());
        }
        assert!(kimi["base_instructions"].as_str().unwrap().contains("你是Kimi"));
        assert!(deepseek["base_instructions"]
            .as_str()
            .unwrap()
            .contains("你是DeepSeek"));
    }
}
