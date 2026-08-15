use crate::config::{ModelConfig, RouterConfig};
use crate::logic::{
    canonical_route_model_id, is_fallback_channel_selected, model_identity,
    recommended_model_display_name, resolve_context_window, resolve_multimodal, resolve_reasoning,
    same_model_identity, slugify,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Path;

/// Simple instructions for non-OpenAI OAuth models that cannot consume Codex's
/// native multi-agent protocol metadata.
const RESTRICTED_OAUTH_INSTRUCTIONS: &str = "You are a coding assistant. Follow the user's instructions carefully and completely.\nPrefer plain language and standard markdown. Never emit protocol metadata, control JSON,\nreasoning envelopes, or internal tags such as {\"type\":\"reasoning_text\"} in user-visible output.\nPut only the final answer and necessary tool calls in the response stream.";

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
        return "antigravity".to_owned();
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
        "antigravity" | "gemini" | "google" | "google_one" | "grok" | "xai" | "x-ai"
    )
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
            let matching_oauth: Vec<RouteDescriptor> = selected_oauth
                .iter()
                .filter(|o| same_model_identity(&o.model_id, &d.model_id))
                .cloned()
                .collect();
            let is_oauth = d.source == "oauth";
            let matching_api_fallbacks: Vec<RouteDescriptor> = if is_oauth && fallback_enabled {
                descriptors
                    .iter()
                    .filter(|x| {
                        x.source != "oauth"
                            && same_model_identity(&x.model_id, &d.model_id)
                            && is_fallback_channel_selected(cfg, &x.model)
                    })
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };

            let mut is_fallback = false;
            let mut join_router = d.selected;
            let mut public_model_id = d.model_id.clone();
            let mut request_model_ids = vec![d.model_id.clone()];
            let include_in_catalog;
            let mut is_merged_oauth_route = false;

            if is_oauth {
                include_in_catalog = d.selected && catalog_ids.insert(public_model_id.clone());
            } else if fallback_enabled && !matching_oauth.is_empty() {
                is_fallback = is_fallback_channel_selected(cfg, &d.model);
                join_router = is_fallback;
                public_model_id = matching_oauth[0].model_id.clone();
                let has_explicit_matching_oauth = matching_oauth.iter().any(|o| !o.discovered);
                if has_explicit_matching_oauth {
                    include_in_catalog = false;
                } else {
                    include_in_catalog = catalog_ids.insert(public_model_id.clone());
                }
                if is_fallback {
                    request_model_ids = matching_oauth
                        .iter()
                        .map(|o| o.model_id.clone())
                        .collect::<HashSet<_>>()
                        .into_iter()
                        .collect();
                }
            } else if !fallback_enabled {
                let same_count = descriptors
                    .iter()
                    .filter(|x| x.selected && same_model_identity(&x.model_id, &d.model_id))
                    .count();
                if same_count > 1 {
                    public_model_id = split_public_model_id(&d.model);
                }
                include_in_catalog = catalog_ids.insert(public_model_id.clone());
            } else {
                include_in_catalog = catalog_ids.insert(public_model_id.clone());
            }

            if is_oauth && fallback_enabled && !matching_api_fallbacks.is_empty() {
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
        "base_instructions": "You are Codex, a coding agent. Work in the user's workspace, follow the user's instructions, preserve unrelated changes, and verify completed work.",
        "model_messages": {
            "instructions_template": "You are Codex, a coding agent. Work in the user's workspace, follow the user's instructions, preserve unrelated changes, and verify completed work.",
            "instructions_variables": null,
            "approvals": null,
            "auto_review": null,
            "permissions": null,
        },
    })
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
        let compact_percent = model.auto_compact_percent.clamp(60, 90);
        let auto_compact_token_limit = context_window * compact_percent as i64 / 100;
        let display_name = route_display_name(route);

        let mut entry = template.clone();
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
        entry["priority"] = model.priority.into();
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
        entry["default_reasoning_summary"] = Value::String("none".to_owned());
        entry["support_verbosity"] = Value::Bool(true);
        entry["default_verbosity"] = Value::String("low".to_owned());
        entry["apply_patch_tool_type"] = Value::String("freeform".to_owned());
        entry["truncation_policy"] = json!({"mode": "tokens", "limit": 10_000});
        entry["supports_parallel_tool_calls"] = Value::Bool(true);
        entry["experimental_supported_tools"] = Value::Array(vec![]);
        entry["comp_hash"] = entry
            .get("comp_hash")
            .cloned()
            .unwrap_or_else(|| Value::String("codex-router-v1".to_owned()));

        if is_restricted_oauth_model(model) {
            let simple = RESTRICTED_OAUTH_INSTRUCTIONS;
            entry["base_instructions"] = Value::String(simple.to_owned());
            if let Some(model_messages) = entry.get_mut("model_messages") {
                model_messages["instructions_template"] = Value::String(simple.to_owned());
            }
            entry["use_responses_lite"] = Value::Bool(false);
            entry["multi_agent_version"] = Value::Null;
            entry["tool_mode"] = Value::String("default".to_owned());
            entry["supports_search_tool"] = Value::Bool(false);
            entry["web_search_tool_type"] = Value::Null;
        } else {
            entry["supports_search_tool"] = Value::Bool(true);
            entry["web_search_tool_type"] = Value::String("text_and_image".to_owned());
            entry["use_responses_lite"] = entry
                .get("use_responses_lite")
                .cloned()
                .unwrap_or(Value::Bool(true));
            entry["multi_agent_version"] = entry
                .get("multi_agent_version")
                .cloned()
                .unwrap_or(Value::String("v2".to_owned()));
            entry["tool_mode"] = entry
                .get("tool_mode")
                .cloned()
                .unwrap_or(Value::String("code_mode_only".to_owned()));
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
}
