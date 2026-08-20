use crate::config::{atomic_write, ModelConfig, ReasoningConfig, RouterConfig};
use anyhow::{bail, Context};
use serde_json::{json, Value};
#[cfg(test)]
use std::collections::VecDeque;
use std::collections::{HashMap, HashSet};
#[cfg(test)]
use std::io::{BufRead, BufReader};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
#[cfg(test)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::Ordering;
#[cfg(test)]
use std::sync::mpsc;
use std::time::{Duration, Instant};
use toml_edit::{DocumentMut, Item};
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
    CRED_TYPE_GENERIC,
};
use windows_sys::Win32::Security::Cryptography::{
    BCryptGenRandom, CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN,
    CRYPT_INTEGER_BLOB,
};
use zeroize::Zeroizing;

pub mod catalog;
pub mod codex_toml;
pub mod deployment;
pub mod oauth;
pub mod responses_compat;
pub mod responses_gateway;
mod usage;
pub use catalog::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OAuthRoutingPriorities {
    pub enabled: bool,
    pub prefer_oauth: bool,
    pub oauth_priority: i32,
    pub api_priority: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelRoutePolicy {
    SubscriptionFirst,
    ApiFirst,
    SubscriptionOnly,
}

impl ModelRoutePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SubscriptionFirst => "subscription_first",
            Self::ApiFirst => "api_first",
            Self::SubscriptionOnly => "subscription_only",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value.trim() {
            "api_first" => Self::ApiFirst,
            "subscription_only" => Self::SubscriptionOnly,
            _ => Self::SubscriptionFirst,
        }
    }
}

pub fn model_identity_key(model_id: &str) -> String {
    let identity = model_identity(model_id);
    format!("{}:{}", identity.provider, identity.real_id)
}

pub fn model_route_policy(cfg: &RouterConfig, model_id: &str) -> ModelRoutePolicy {
    cfg.model_route_policies
        .get(&model_identity_key(model_id))
        .map(|value| ModelRoutePolicy::parse(value))
        .unwrap_or(ModelRoutePolicy::SubscriptionFirst)
}

pub fn set_model_route_policy(cfg: &mut RouterConfig, model_id: &str, policy: ModelRoutePolicy) {
    let key = model_identity_key(model_id);
    match policy {
        ModelRoutePolicy::SubscriptionFirst => {
            cfg.model_route_policies.remove(&key);
        }
        ModelRoutePolicy::ApiFirst | ModelRoutePolicy::SubscriptionOnly => {
            cfg.model_route_policies
                .insert(key, policy.as_str().to_owned());
        }
    }
}

pub fn matching_api_fallback_models<'a>(
    cfg: &'a RouterConfig,
    oauth_model_id: &str,
) -> Vec<&'a ModelConfig> {
    cfg.models
        .iter()
        .filter(|candidate| {
            candidate.source != "oauth" && same_model_identity(&candidate.model, oauth_model_id)
        })
        .collect()
}

pub fn oauth_routing_priorities(
    fallback: Option<&crate::config::OAuthFallback>,
) -> OAuthRoutingPriorities {
    let enabled = fallback.is_some_and(|value| value.enabled);
    if !enabled {
        return OAuthRoutingPriorities {
            enabled: false,
            prefer_oauth: true,
            oauth_priority: 1,
            api_priority: 10,
        };
    }
    let fallback = fallback.expect("enabled fallback exists");
    let official = fallback.official_priority.max(1);
    let api = fallback.fallback_priority.max(1);
    OAuthRoutingPriorities {
        enabled: true,
        prefer_oauth: fallback.prefer_oauth,
        oauth_priority: if fallback.prefer_oauth { official } else { api },
        api_priority: if fallback.prefer_oauth { api } else { official },
    }
}

pub fn model_oauth_routing_priorities(
    cfg: &RouterConfig,
    model_id: &str,
) -> OAuthRoutingPriorities {
    match model_route_policy(cfg, model_id) {
        ModelRoutePolicy::SubscriptionOnly => OAuthRoutingPriorities {
            enabled: false,
            prefer_oauth: true,
            oauth_priority: 1,
            api_priority: 10,
        },
        policy => {
            let fallback = crate::config::OAuthFallback {
                enabled: true,
                prefer_oauth: policy == ModelRoutePolicy::SubscriptionFirst,
                official_priority: cfg.oauth_fallback.official_priority,
                fallback_priority: cfg.oauth_fallback.fallback_priority,
            };
            oauth_routing_priorities(Some(&fallback))
        }
    }
}

pub fn effective_api_priority(
    configured_priority: i32,
    minimum_matching_priority: i32,
    api_base_priority: i32,
    oauth_priority: i32,
    prefer_oauth: bool,
) -> i32 {
    let offset = configured_priority
        .saturating_sub(minimum_matching_priority)
        .max(0);
    let mut effective = api_base_priority.saturating_add(offset).max(1);
    if !prefer_oauth && oauth_priority > 1 {
        effective = effective.min(oauth_priority - 1);
    }
    effective
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReasoningSpec {
    pub levels: Vec<String>,
    pub default_level: String,
    pub supports_fast: bool,
    pub family_zh: &'static str,
    pub family_en: &'static str,
    pub source_zh: &'static str,
    pub source_en: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelPreset {
    pub id: &'static str,
    pub label_zh: &'static str,
    pub label_en: &'static str,
    pub base_url: &'static str,
    pub model: &'static str,
    pub alias: &'static str,
    pub website_url: &'static str,
    pub docs_url: &'static str,
}

const CHANNEL_PRESETS: &[ChannelPreset] = &[
    ChannelPreset {
        id: "openai",
        label_zh: "OpenAI 官方 API",
        label_en: "OpenAI official API",
        base_url: "https://api.openai.com/v1",
        model: "gpt-5.6-sol",
        alias: "ChatGPT-5.6-Sol",
        website_url: "https://platform.openai.com/",
        docs_url: "https://developers.openai.com/api/docs/models/gpt-5.6-sol",
    },
    ChannelPreset {
        id: "anthropic",
        label_zh: "Anthropic / Claude",
        label_en: "Anthropic / Claude",
        base_url: "https://api.anthropic.com/v1",
        model: "claude-opus-5",
        alias: "Claude-Opus-5",
        website_url: "https://console.anthropic.com/",
        docs_url: "https://platform.claude.com/docs/en/about-claude/models/overview",
    },
    ChannelPreset {
        id: "chiral",
        label_zh: "Chiral-API / 可靠聚合中转平台（推荐）",
        label_en: "Chiral-API / Reliable aggregation relay (recommended)",
        base_url: "https://api.430123.xyz/v1",
        model: "gpt-5.6-sol",
        alias: "ChatGPT-5.6-Sol",
        website_url: "https://api.430123.xyz/chiral",
        docs_url: "https://api.430123.xyz/chiral",
    },
    ChannelPreset {
        id: "openrouter",
        label_zh: "OpenRouter",
        label_en: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        model: "openai/gpt-5.6-sol",
        alias: "ChatGPT-5.6-Sol",
        website_url: "https://openrouter.ai/",
        docs_url: "https://openrouter.ai/docs/quickstart",
    },
    ChannelPreset {
        id: "kimi-open",
        label_zh: "Kimi Open Platform 官方 API",
        label_en: "Kimi Open Platform official API",
        base_url: "https://api.moonshot.ai/v1",
        model: "kimi-k3",
        alias: "Kimi-K3",
        website_url: "https://platform.kimi.ai/",
        docs_url: "https://platform.kimi.ai/docs/guide/codex-kimi",
    },
    ChannelPreset {
        id: "kimi",
        label_zh: "Kimi Coding Plan",
        label_en: "Kimi Coding Plan",
        base_url: "https://api.kimi.com/coding/v1",
        model: "kimi-for-coding",
        alias: "Kimi-For-Coding",
        website_url: "https://www.kimi.com/code/console",
        docs_url: "https://www.kimi.com/code/docs/en/third-party-tools/codex.html",
    },
    ChannelPreset {
        id: "ark-coding",
        label_zh: "字节跳动 火山方舟 Coding Plan",
        label_en: "ByteDance Volcengine Ark Coding Plan",
        base_url: "https://ark.cn-beijing.volces.com/api/coding/v3",
        model: "ark-code-latest",
        alias: "Ark-Code-Latest",
        website_url: "https://console.volcengine.com/ark",
        docs_url: "https://www.volcengine.com/docs/82379/2556056",
    },
    ChannelPreset {
        id: "ark-plan",
        label_zh: "字节跳动 火山方舟 Agent Plan",
        label_en: "ByteDance Volcengine Ark Agent Plan",
        base_url: "https://ark.cn-beijing.volces.com/api/plan/v3",
        model: "ark-code-latest",
        alias: "Ark-Code-Latest",
        website_url: "https://console.volcengine.com/ark",
        docs_url: "https://www.volcengine.com/docs/82379/2556056",
    },
    ChannelPreset {
        id: "mimo",
        label_zh: "Xiaomi MiMo Token Plan",
        label_en: "Xiaomi MiMo Token Plan",
        base_url: "https://api.xiaomimimo.com/v1",
        model: "mimo-v2.5-pro",
        alias: "MiMo-V2.5-Pro",
        website_url: "https://platform.xiaomimimo.com/token-plan",
        docs_url: "https://mimo.mi.com/docs/models/mimo-v2-5-pro",
    },
    ChannelPreset {
        id: "deepseek",
        label_zh: "DeepSeek 官方 API",
        label_en: "DeepSeek official API",
        base_url: "https://api.deepseek.com/v1",
        model: "deepseek-v4-pro",
        alias: "DeepSeek-V4-Pro",
        website_url: "https://platform.deepseek.com/",
        docs_url: "https://api-docs.deepseek.com/",
    },
    ChannelPreset {
        id: "gemini",
        label_zh: "Google Gemini 兼容 API",
        label_en: "Google Gemini compatible API",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai/",
        model: "gemini-3.6-flash",
        alias: "Gemini-3.6-Flash",
        website_url: "https://aistudio.google.com/",
        docs_url: "https://ai.google.dev/gemini-api/docs/openai",
    },
];

pub fn channel_presets() -> &'static [ChannelPreset] {
    CHANNEL_PRESETS
}

pub fn common_channel_presets() -> impl Iterator<Item = &'static ChannelPreset> {
    CHANNEL_PRESETS
        .iter()
        .filter(|preset| preset.id != "chiral")
}

pub fn recommended_channel_presets() -> impl Iterator<Item = &'static ChannelPreset> {
    CHANNEL_PRESETS
        .iter()
        .filter(|preset| preset.id == "chiral")
}

pub fn apply_channel_preset(model: &mut ModelConfig, preset_id: &str) -> bool {
    let Some(preset) = CHANNEL_PRESETS.iter().find(|preset| preset.id == preset_id) else {
        return false;
    };
    model.model = preset.model.to_owned();
    model.alias = preset.alias.to_owned();
    model.alias_customized = Some(false);
    model.base_url = preset.base_url.to_owned();
    model.weight = 1;
    model.extra = "{}".to_owned();
    model.multimodal = "auto".to_owned();
    model.context_window = 0;
    model.auto_compact_percent = 80;
    model.reasoning_mode = "auto".to_owned();
    model.reasoning_levels.clear();
    model.default_reasoning_level.clear();
    model.fast_supported = false;
    model.fast_mode = false;
    model.source = "apikey".to_owned();
    model.oauth_account_id = 0;
    model.oauth_platform.clear();
    true
}

impl ReasoningSpec {
    fn new(
        levels: &[&str],
        default_level: &str,
        supports_fast: bool,
        family_zh: &'static str,
        family_en: &'static str,
        source_zh: &'static str,
        source_en: &'static str,
    ) -> Self {
        Self {
            levels: levels.iter().map(|value| (*value).to_owned()).collect(),
            default_level: default_level.to_owned(),
            supports_fast,
            family_zh,
            family_en,
            source_zh,
            source_en,
        }
    }
}

pub fn detect_reasoning(model_name: &str) -> ReasoningSpec {
    let name = model_name.trim().to_ascii_lowercase();
    if name.contains("gpt-5.6-sol") {
        return ReasoningSpec::new(
            &["low", "medium", "high", "xhigh", "max", "ultra"],
            "medium",
            true,
            "ChatGPT-5.6-Sol",
            "ChatGPT-5.6-Sol",
            "Codex 官方模型目录；常规默认 medium",
            "Official Codex model catalog; medium is the normal default",
        );
    }
    if name.contains("gpt-5.6-terra") {
        return ReasoningSpec::new(
            &["low", "medium", "high", "xhigh", "max", "ultra"],
            "medium",
            true,
            "ChatGPT-5.6-Terra",
            "ChatGPT-5.6-Terra",
            "Codex 官方模型目录",
            "Official Codex model catalog",
        );
    }
    if name.contains("gpt-5.6-luna") {
        return ReasoningSpec::new(
            &["low", "medium", "high", "xhigh", "max"],
            "medium",
            true,
            "ChatGPT-5.6-Luna",
            "ChatGPT-5.6-Luna",
            "Codex 官方模型目录",
            "Official Codex model catalog",
        );
    }
    if name.contains("gpt-5.5") || name.contains("gpt-5.4") {
        return ReasoningSpec::new(
            &["minimal", "low", "medium", "high", "xhigh"],
            "medium",
            true,
            "OpenAI GPT-5.4 / GPT-5.5",
            "OpenAI GPT-5.4 / GPT-5.5",
            "OpenAI 与 Codex 官方文档",
            "Official OpenAI and Codex documentation",
        );
    }
    if name.contains("claude-opus-5")
        || name.contains("claude-sonnet-5")
        || name.contains("claude-fable-5")
    {
        return ReasoningSpec::new(
            &["low", "medium", "high", "xhigh", "max"],
            "high",
            false,
            "Anthropic Claude 5",
            "Anthropic Claude 5",
            "Anthropic 官方 Effort 文档；复杂编程默认 high",
            "Official Anthropic effort guide; high is the default for complex coding",
        );
    }
    if name.contains("gemini-3") {
        return ReasoningSpec::new(
            &["minimal", "low", "medium", "high"],
            "high",
            false,
            "Google Gemini 3",
            "Google Gemini 3",
            "Gemini OpenAI 兼容文档的 reasoning_effort 映射",
            "Gemini OpenAI compatibility reasoning_effort mapping",
        );
    }
    if name == "k3" || name.starts_with("k3-") || name.contains("kimi-k3") {
        return ReasoningSpec::new(
            &["low", "high", "max"],
            "high",
            false,
            "Moonshot Kimi K3",
            "Moonshot Kimi K3",
            "Kimi 官方 reasoning_effort 文档",
            "Official Kimi reasoning_effort documentation",
        );
    }
    if name.contains("kimi-for-coding") || name.contains("kimi-k2.7") {
        return ReasoningSpec::new(
            &["high"],
            "high",
            false,
            "Kimi K2.7 Code 兼容模式",
            "Kimi K2.7 Code compatibility",
            "K2.7 Code 不提供可调 reasoning_effort；固定使用 high 兼容值",
            "K2.7 Code has no adjustable reasoning_effort; high is used for compatibility",
        );
    }
    if name.starts_with("ark-code") || name.contains("doubao-seed-code") {
        return ReasoningSpec::new(
            &["high"],
            "high",
            false,
            "火山方舟 Coding Plan",
            "Volcengine Ark Coding Plan",
            "方舟 Coding Plan 走 OpenAI 兼容端点；未公布可调档位，采用固定兼容值",
            "Ark Coding Plan uses the OpenAI-compatible endpoint with no documented adjustable tiers",
        );
    }
    if name.contains("deepseek-v4") {
        return ReasoningSpec::new(
            &["none", "low", "high", "max"],
            "high",
            false,
            "DeepSeek V4",
            "DeepSeek V4",
            "DeepSeek 官方 Thinking Mode 与 Codex 集成文档",
            "Official DeepSeek Thinking Mode and Codex integration documentation",
        );
    }
    if name.contains("mimo-v2.5") {
        return ReasoningSpec::new(
            &["high"],
            "high",
            false,
            "Xiaomi MiMo V2.5",
            "Xiaomi MiMo V2.5",
            "MiMo 官方模型页确认深度思考；未公布可调档位，采用固定兼容值",
            "Official MiMo model page confirms deep thinking; no adjustable tiers are documented",
        );
    }
    if name.contains("deepseek") {
        return ReasoningSpec::new(
            &["low", "high", "max"],
            "high",
            false,
            "DeepSeek Reasoner",
            "DeepSeek Reasoner",
            "DeepSeek 官方 Thinking Mode 文档",
            "Official DeepSeek Thinking Mode documentation",
        );
    }
    if name.contains("grok-4.5") {
        return ReasoningSpec::new(
            &["low", "medium", "high"],
            "high",
            false,
            "xAI Grok 4.5",
            "xAI Grok 4.5",
            "xAI 官方模型文档",
            "Official xAI model documentation",
        );
    }
    if name.contains("grok") {
        return ReasoningSpec::new(
            &["low", "medium", "high"],
            "medium",
            false,
            "xAI Grok",
            "xAI Grok",
            "xAI 官方模型文档；采用保守兼容档位",
            "Official xAI model documentation; conservative compatible levels",
        );
    }
    ReasoningSpec::new(
        &["medium"],
        "medium",
        false,
        "通用 OpenAI 兼容模型",
        "Generic OpenAI-compatible model",
        "未识别模型；使用 Codex 保守兼容默认值",
        "Unknown model; using a conservative Codex-compatible default",
    )
}

pub fn is_valid_reasoning_level(value: &str) -> bool {
    matches!(
        value,
        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
    )
}

fn manual_reasoning_spec(
    levels: &[String],
    default_level: &str,
    supports_fast: bool,
) -> Option<ReasoningSpec> {
    let mut normalized = Vec::new();
    for value in levels {
        let value = value.trim().to_ascii_lowercase();
        if is_valid_reasoning_level(&value) && !normalized.contains(&value) {
            normalized.push(value);
        }
    }
    if normalized.is_empty() {
        return None;
    }
    let requested_default = default_level.trim().to_ascii_lowercase();
    let default_level = if normalized.contains(&requested_default) {
        requested_default
    } else {
        normalized[0].clone()
    };
    Some(ReasoningSpec {
        levels: normalized,
        default_level,
        supports_fast,
        family_zh: "手动配置",
        family_en: "Manual configuration",
        source_zh: "用户自定义；仅保留 Codex 可识别值",
        source_en: "User-defined; restricted to Codex-recognized values",
    })
}

pub fn resolve_reasoning(model: &ModelConfig, legacy: Option<&ReasoningConfig>) -> ReasoningSpec {
    if model.reasoning_mode == "manual" {
        if let Some(spec) = manual_reasoning_spec(
            &model.reasoning_levels,
            &model.default_reasoning_level,
            model.fast_supported,
        ) {
            return spec;
        }
    }
    if let Some(legacy) = legacy.filter(|value| value.mode == "manual") {
        if let Some(spec) =
            manual_reasoning_spec(&legacy.levels, &legacy.default_level, legacy.supports_fast)
        {
            return spec;
        }
    }
    detect_reasoning(&model.model)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MultimodalDefaults {
    pub supported: bool,
    pub source_zh: &'static str,
    pub source_en: &'static str,
}

pub fn detect_multimodal_defaults(model_name: &str) -> MultimodalDefaults {
    let name = model_name.trim().to_ascii_lowercase();
    let explicit_vision = [
        "vision",
        "multimodal",
        "qwen-vl",
        "qwen2-vl",
        "qwen2.5-vl",
        "qwen3-vl",
        "glm-4v",
        "glm-4.1v",
        "glm-4.5v",
        "glm-4.6v",
        "cogvlm",
        "janus",
        "pixtral",
    ]
    .iter()
    .any(|marker| name.contains(marker))
        || name.ends_with("-vl")
        || name.contains("/vl-")
        || name.contains("_vl");
    if explicit_vision {
        return MultimodalDefaults {
            supported: true,
            source_zh: "型号名称明确标注 Vision / VL / V",
            source_en: "Model id explicitly identifies a Vision / VL / V variant",
        };
    }
    if name.starts_with("ark-code") || name.contains("doubao-seed-code") {
        return MultimodalDefaults {
            supported: false,
            source_zh: "方舟 Coding Plan 为编程语言模型；多模态需改用 Agent Plan 视觉模型",
            source_en: "Ark Coding Plan ships code language models; vision needs Agent Plan models",
        };
    }
    if name.contains("deepseek") {
        return MultimodalDefaults {
            supported: false,
            source_zh: "DeepSeek API 与 OpenRouter 模型元数据：纯文本",
            source_en: "DeepSeek API and OpenRouter metadata: text-only",
        };
    }
    if name.contains("glm") {
        return MultimodalDefaults {
            supported: false,
            source_zh: "普通 GLM 为纯文本；仅 GLM-V / Vision 型号支持图片",
            source_en: "Standard GLM is text-only; only GLM-V / Vision variants accept images",
        };
    }
    if name.contains("qwen") && (name.contains("coder") || name.contains("code")) {
        return MultimodalDefaults {
            supported: false,
            source_zh: "Qwen Coder 系列默认按纯文本处理",
            source_en: "Qwen Coder models default to text-only",
        };
    }
    if name.contains("mimo-v2.5-pro") {
        return MultimodalDefaults {
            supported: false,
            source_zh: "MiMo 官方模型目录：V2.5 Pro 是文本旗舰模型",
            source_en: "Official MiMo catalog: V2.5 Pro is the text flagship model",
        };
    }
    if name.contains("gpt-")
        || name.contains("grok-4")
        || name.contains("claude-")
        || name.contains("gemini")
        || name.contains("kimi")
        || name.contains("moonshot")
        || name.contains("mimo-v2.5")
        || name == "k3"
        || name.starts_with("k3-")
    {
        return MultimodalDefaults {
            supported: true,
            source_zh: "已核对的常见多模态模型系列",
            source_en: "Verified common multimodal model family",
        };
    }
    MultimodalDefaults {
        supported: false,
        source_zh: "未知型号采用保守默认：纯文本，可手动开启",
        source_en:
            "Unknown model uses a conservative text-only default; manual override is available",
    }
}

pub fn resolve_multimodal(model: &ModelConfig) -> bool {
    match model.multimodal.as_str() {
        "true" => true,
        "false" => false,
        _ => detect_multimodal_defaults(&model.model).supported,
    }
}

pub fn resolve_default_route(cfg: &RouterConfig) -> Option<catalog::ModelRoute> {
    let routes = catalog::build_route_plan(cfg);
    routes
        .iter()
        .find(|route| route.include_in_catalog && route.public_model_id == cfg.default_model)
        .or_else(|| {
            routes
                .iter()
                .find(|route| route.include_in_catalog && route.model.model == cfg.default_model)
        })
        .or_else(|| routes.iter().find(|route| route.include_in_catalog))
        .cloned()
}

pub fn resolve_default_model(cfg: &RouterConfig) -> Option<String> {
    resolve_default_route(cfg).map(|route| route.public_model_id)
}

pub fn normalize_default_model(cfg: &mut RouterConfig) {
    cfg.default_model = resolve_default_model(cfg).unwrap_or_default();
}

pub fn canonical_route_model_id(model_id: &str) -> String {
    let mut value = model_id
        .trim()
        .to_ascii_lowercase()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_owned();
    if value == "gpt-5.6" {
        value = "gpt-5.6-sol".to_owned();
    }
    // ChatGPT-branded channel ids (chatgpt-5.6-sol, chatgpt-5.6-luna, ...)
    // name the same OpenAI models as their gpt-* twins. Canonicalizing them
    // onto the gpt-* family keeps fallback pairing, manual channel selections,
    // and display naming global instead of limited to preset gpt-* ids.
    if let Some(suffix) = value.strip_prefix("chatgpt-") {
        value = format!("gpt-{suffix}");
    }
    value
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelIdentity {
    pub provider: String,
    pub real_id: String,
    pub display_candidate: String,
}

pub fn model_identity(model_id: &str) -> ModelIdentity {
    let raw = model_id.trim().to_ascii_lowercase();
    // canonical_route_model_id already normalizes chatgpt-* branded ids onto
    // the gpt-* family, so the provider cascade below covers them via gpt-.
    let mut real_id = canonical_route_model_id(&raw);
    let provider = if raw.starts_with("openai/")
        || raw.starts_with("chatgpt/")
        || raw.starts_with("chatgpt-")
        || real_id.starts_with("gpt-")
        || real_id.starts_with("codex-")
    {
        "openai"
    } else if raw.starts_with("anthropic/")
        || raw.starts_with("claude/")
        || real_id.starts_with("claude-")
    {
        "anthropic"
    } else if raw.starts_with("google/")
        || raw.starts_with("gemini/")
        || real_id.starts_with("gemini-")
    {
        "google"
    } else if raw.starts_with("x-ai/")
        || raw.starts_with("xai/")
        || raw.starts_with("grok/")
        || real_id.starts_with("grok-")
    {
        "x-ai"
    } else if raw.starts_with("deepseek/") || real_id.starts_with("deepseek-") {
        "deepseek"
    } else if raw.starts_with("moonshotai/")
        || raw.starts_with("moonshot/")
        || raw.starts_with("kimi/")
        || real_id.starts_with("kimi-")
        || real_id == "k3"
        || real_id.starts_with("k3-")
    {
        "moonshot"
    } else {
        return ModelIdentity {
            provider: raw
                .split_once('/')
                .map(|(namespace, _)| format!("unknown-{namespace}"))
                .unwrap_or_else(|| "unknown".to_owned()),
            real_id,
            display_candidate: recommended_model_display_name(model_id),
        };
    }
    .to_owned();
    if provider == "google" {
        let parts = real_id.split('-').collect::<Vec<_>>();
        if parts.len() >= 4
            && parts[0] == "gemini"
            && parts[1].chars().all(|c| c.is_ascii_digit())
            && parts[2].chars().all(|c| c.is_ascii_digit())
        {
            real_id = format!("gemini-{}.{}-{}", parts[1], parts[2], parts[3..].join("-"));
        }
    }
    if provider == "anthropic" {
        let parts = real_id.split('-').collect::<Vec<_>>();
        if parts.len() >= 4
            && parts[0] == "claude"
            && parts[2].chars().all(|c| c.is_ascii_digit())
            && parts[3].chars().all(|c| c.is_ascii_digit())
        {
            let mut normalized = format!("claude-{}-{}.{}", parts[1], parts[2], parts[3]);
            if parts.len() > 4 {
                normalized.push('-');
                normalized.push_str(&parts[4..].join("-"));
            }
            real_id = normalized;
        }
    }
    ModelIdentity {
        provider,
        real_id,
        display_candidate: recommended_model_display_name(model_id),
    }
}

pub fn same_model_identity(left: &str, right: &str) -> bool {
    let left = model_identity(left);
    let right = model_identity(right);
    normalized_display_name(&left.display_candidate)
        == normalized_display_name(&right.display_candidate)
        && left.provider == right.provider
        && left.real_id == right.real_id
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardModelRow {
    pub index: usize,
    pub account_count: usize,
}

pub fn dashboard_model_rows(models: &[ModelConfig]) -> Vec<DashboardModelRow> {
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    for (index, model) in models.iter().enumerate() {
        let identity = model_identity(&model.model);
        let key = format!("{}|{}", identity.provider, identity.real_id);
        if !seen.insert(key) {
            continue;
        }
        let representative = models
            .iter()
            .enumerate()
            .find(|(_, candidate)| {
                candidate.source == "oauth"
                    && same_model_identity(&candidate.model, &model.model)
            })
            .map(|(candidate_index, _)| candidate_index)
            .unwrap_or(index);
        let account_count = models
            .iter()
            .filter(|candidate| same_model_identity(&candidate.model, &model.model))
            .count()
            .max(1);
        rows.push(DashboardModelRow {
            index: representative,
            account_count,
        });
    }
    rows
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelSourceType {
    Subscription,
    CodingPlan,
    OfficialApi,
    Relay,
}

impl ChannelSourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Subscription => "subscription",
            Self::CodingPlan => "coding_plan",
            Self::OfficialApi => "official_api",
            Self::Relay => "relay",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "subscription" => Some(Self::Subscription),
            "coding_plan" => Some(Self::CodingPlan),
            "official_api" => Some(Self::OfficialApi),
            "relay" => Some(Self::Relay),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelGateway {
    Sub2Api,
    Direct,
}

impl ChannelGateway {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sub2Api => "sub2api",
            Self::Direct => "direct",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpstreamProtocol {
    Responses,
    ChatCompletions,
    Anthropic,
}

impl UpstreamProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::ChatCompletions => "chat_completions",
            Self::Anthropic => "anthropic",
        }
    }

    pub fn responses_mode(self) -> &'static str {
        match self {
            Self::Responses => "force_responses",
            Self::ChatCompletions | Self::Anthropic => "force_chat_completions",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelRouteProfile {
    pub vendor: String,
    pub source_type: ChannelSourceType,
    pub gateway: ChannelGateway,
    pub base_url: String,
    pub upstream_protocol: UpstreamProtocol,
    pub billing_mode: String,
    pub allow_fallback: bool,
}

fn extra_object(raw: &str) -> serde_json::Map<String, Value> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn endpoint_host_path(base_url: &str) -> (String, String) {
    url::Url::parse(base_url.trim())
        .ok()
        .map(|url| {
            (
                url.host_str().unwrap_or_default().to_ascii_lowercase(),
                url.path().to_ascii_lowercase(),
            )
        })
        .unwrap_or_default()
}

fn looks_like_sub2api_endpoint(base_url: &str) -> bool {
    let lower = base_url.trim().to_ascii_lowercase();
    lower.starts_with("sub2api")
        || lower.contains("127.0.0.1")
        || lower.contains("localhost")
}

fn is_coding_plan_endpoint(host: &str, path: &str) -> bool {
    (host == "api.kimi.com" && path.starts_with("/coding"))
        || (host == "api.moonshot.ai" && path.starts_with("/coding"))
        || (host.ends_with(".volces.com")
            && (path.starts_with("/api/coding") || path.starts_with("/api/plan")))
}

fn is_official_api_host(host: &str) -> bool {
    matches!(
        host,
        "api.openai.com"
            | "api.anthropic.com"
            | "api.deepseek.com"
            | "api.kimi.com"
            | "api.moonshot.ai"
            | "api.moonshot.cn"
            | "generativelanguage.googleapis.com"
            | "api.x.ai"
    ) || (host.ends_with(".volces.com") && !host.is_empty())
}

fn channel_vendor(
    model_id: &str,
    host: &str,
    oauth_platform: &str,
    source_type: ChannelSourceType,
) -> String {
    let platform = oauth_platform.trim().to_ascii_lowercase();
    if !platform.is_empty() {
        return match platform.as_str() {
            "openai" | "chatgpt" => "openai".to_owned(),
            "grok" | "xai" | "x-ai" => "x-ai".to_owned(),
            "gemini" | "antigravity" | "google" => "google".to_owned(),
            "anthropic" | "claude" => "anthropic".to_owned(),
            other => other.to_owned(),
        };
    }
    if host == "api.430123.xyz" || host.ends_with(".430123.xyz") {
        return "chiral".to_owned();
    }
    if host == "openrouter.ai" || host.ends_with(".openrouter.ai") {
        return "openrouter".to_owned();
    }
    if host.ends_with(".volces.com") {
        return "volcengine".to_owned();
    }
    if host == "api.kimi.com" || host == "api.moonshot.ai" || host == "api.moonshot.cn" {
        return "moonshot".to_owned();
    }
    if host == "api.deepseek.com" {
        return "deepseek".to_owned();
    }
    if host == "api.anthropic.com" {
        return "anthropic".to_owned();
    }
    if host == "api.openai.com" {
        return "openai".to_owned();
    }
    if host == "generativelanguage.googleapis.com" {
        return "google".to_owned();
    }
    if host == "api.x.ai" {
        return "x-ai".to_owned();
    }
    let identity = model_identity(model_id);
    if !identity.provider.starts_with("unknown") {
        return identity.provider;
    }
    match source_type {
        ChannelSourceType::Relay => "relay".to_owned(),
        _ => identity.provider,
    }
}

fn endpoint_protocol(
    host: &str,
    path: &str,
    model_id: &str,
    gateway: ChannelGateway,
) -> UpstreamProtocol {
    if host == "api.anthropic.com" {
        return UpstreamProtocol::Anthropic;
    }
    if host == "api.430123.xyz" || host.ends_with(".430123.xyz") {
        return UpstreamProtocol::Responses;
    }
    if host.ends_with(".volces.com") {
        return if path.starts_with("/api/coding") || path.starts_with("/api/plan") {
            UpstreamProtocol::Responses
        } else {
            UpstreamProtocol::ChatCompletions
        };
    }
    if host == "api.kimi.com"
        || host == "api.moonshot.ai"
        || host == "api.moonshot.cn"
        || host == "api.deepseek.com"
        || host == "generativelanguage.googleapis.com"
        || host == "api.x.ai"
    {
        return UpstreamProtocol::ChatCompletions;
    }
    if host == "openrouter.ai" || host.ends_with(".openrouter.ai") {
        let model = model_id.trim().trim_start_matches('~');
        return if model.to_ascii_lowercase().starts_with("deepseek/") {
            UpstreamProtocol::Responses
        } else {
            UpstreamProtocol::ChatCompletions
        };
    }
    if gateway == ChannelGateway::Sub2Api || host == "api.openai.com" || host.is_empty() {
        return UpstreamProtocol::Responses;
    }
    UpstreamProtocol::Responses
}

pub fn classify_channel_route(model: &ModelConfig) -> ChannelRouteProfile {
    let extra = extra_object(&model.extra);
    let (host, path) = endpoint_host_path(&model.base_url);
    let gateway = if model.source == "oauth" || looks_like_sub2api_endpoint(&model.base_url) {
        ChannelGateway::Sub2Api
    } else {
        ChannelGateway::Direct
    };
    let explicit_source = extra
        .get("codex_router_channel_kind")
        .and_then(Value::as_str)
        .and_then(ChannelSourceType::parse);
    let source_type = if model.source == "oauth" {
        ChannelSourceType::Subscription
    } else if let Some(kind) = explicit_source {
        kind
    } else if is_coding_plan_endpoint(&host, &path) {
        ChannelSourceType::CodingPlan
    } else if (is_official_api_host(&host) && !host.ends_with(".volces.com"))
        || (host.ends_with(".volces.com") && path.starts_with("/api/v3"))
    {
        ChannelSourceType::OfficialApi
    } else if host.is_empty() && gateway == ChannelGateway::Sub2Api {
        ChannelSourceType::Subscription
    } else {
        ChannelSourceType::Relay
    };
    let explicit_protocol = extra
        .get("openai_responses_mode")
        .and_then(Value::as_str)
        .map(|mode| mode.trim().to_ascii_lowercase());
    let upstream_protocol = match explicit_protocol.as_deref() {
        Some("force_responses") => UpstreamProtocol::Responses,
        Some("force_chat_completions") => UpstreamProtocol::ChatCompletions,
        _ => endpoint_protocol(&host, &path, &model.model, gateway),
    };
    let allow_fallback = extra
        .get("allow_fallback")
        .and_then(Value::as_bool)
        .unwrap_or(!matches!(
            source_type,
            ChannelSourceType::Subscription | ChannelSourceType::CodingPlan
        ));
    let vendor = extra
        .get("codex_router_vendor")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            channel_vendor(&model.model, &host, &model.oauth_platform, source_type)
        });
    let billing_mode = match source_type {
        ChannelSourceType::Subscription => "subscription",
        ChannelSourceType::CodingPlan => "coding_plan",
        ChannelSourceType::OfficialApi => "payg",
        ChannelSourceType::Relay => "relay",
    }
    .to_owned();
    ChannelRouteProfile {
        vendor,
        source_type,
        gateway,
        base_url: model.base_url.trim().trim_end_matches('/').to_owned(),
        upstream_protocol,
        billing_mode,
        allow_fallback,
    }
}

pub fn is_same_vendor_payg_fallback(source: &ModelConfig, candidate: &ModelConfig) -> bool {
    let source_profile = classify_channel_route(source);
    let candidate_profile = classify_channel_route(candidate);
    !source_profile.allow_fallback
        && candidate_profile.source_type == ChannelSourceType::OfficialApi
        && source_profile.vendor == candidate_profile.vendor
}

pub fn is_eligible_oauth_api_fallback(
    cfg: &RouterConfig,
    source: &ModelConfig,
    candidate: &ModelConfig,
) -> bool {
    if model_route_policy(cfg, &source.model) == ModelRoutePolicy::SubscriptionOnly
        || model_route_policy(cfg, &candidate.model) == ModelRoutePolicy::SubscriptionOnly
    {
        return false;
    }
    if candidate.source == "oauth" || !is_fallback_channel_selected(cfg, candidate) {
        return false;
    }
    let canonical = canonical_route_model_id(&candidate.model);
    let explicitly_listed = cfg.fallback_channel_selections.contains_key(&canonical);
    if explicitly_listed {
        return true;
    }
    !is_same_vendor_payg_fallback(source, candidate)
}

pub fn model_routing_explanation(cfg: &RouterConfig, model: &ModelConfig, zh: bool) -> String {
    let matched = cfg.models.iter().any(|candidate| {
        !std::ptr::eq(candidate, model)
            && candidate.model != model.model
            && candidate.source != model.source
            && same_model_identity(&candidate.model, &model.model)
    });
    let oauth = model.source == "oauth";
    if matched {
        if zh {
            "同名模型：OAuth 优先，API 自动兜底".to_owned()
        } else {
            "Same model: OAuth first, API fallback".to_owned()
        }
    } else if oauth {
        if zh {
            "OAuth 独立路由".to_owned()
        } else {
            "Standalone OAuth route".to_owned()
        }
    } else if zh {
        "API 独立渠道".to_owned()
    } else {
        "Standalone API channel".to_owned()
    }
}

pub fn model_route_chip(
    cfg: &RouterConfig,
    model: &ModelConfig,
    channel_count: usize,
    zh: bool,
) -> String {
    let matched = if model.source == "oauth" {
        cfg.models.iter().any(|candidate| {
            is_oauth_fallback_model(cfg, candidate)
                && same_model_identity(&candidate.model, &model.model)
        })
    } else {
        is_oauth_fallback_model(cfg, model)
    };
    let policy = model_route_policy(cfg, &model.model);
    let label = match (model.source == "oauth", matched, policy, zh) {
        (true, _, ModelRoutePolicy::SubscriptionOnly, true) => "仅订阅",
        (true, _, ModelRoutePolicy::SubscriptionOnly, false) => "sub only",
        (true, true, ModelRoutePolicy::ApiFirst, true) => "API优先",
        (true, true, ModelRoutePolicy::ApiFirst, false) => "API first",
        (true, true, _, true) => "订阅优先",
        (true, true, _, false) => "sub first",
        (true, false, _, true) => "独立订阅",
        (true, false, _, false) => "subscription",
        (false, true, ModelRoutePolicy::ApiFirst, true) => "API优先",
        (false, true, ModelRoutePolicy::ApiFirst, false) => "API first",
        (false, true, _, true) => "API兜底",
        (false, true, _, false) => "API fallback",
        (false, false, _, true) => "独立API",
        (false, false, _, false) => "API",
    };
    if channel_count > 1 {
        format!("{channel_count} · {label}")
    } else {
        label.to_owned()
    }
}

pub fn recommended_model_display_name(model_id: &str) -> String {
    let canonical = canonical_route_model_id(model_id);
    if let Some(suffix) = canonical.strip_prefix("gpt-") {
        if suffix
            .chars()
            .next()
            .is_some_and(|value| value.is_ascii_digit())
        {
            let segments = suffix
                .split(['-', '_'])
                .map(|segment| match segment {
                    "codex" => "Codex".to_owned(),
                    "fast" => "Fast".to_owned(),
                    "high" => "High".to_owned(),
                    "low" => "Low".to_owned(),
                    "max" => "Max".to_owned(),
                    "mini" => "Mini".to_owned(),
                    "nano" => "Nano".to_owned(),
                    value if value.len() <= 1 => value.to_ascii_uppercase(),
                    value => {
                        let mut chars = value.chars();
                        let first = chars.next().unwrap_or_default().to_ascii_uppercase();
                        format!("{first}{}", chars.as_str().to_ascii_lowercase())
                    }
                })
                .collect::<Vec<_>>()
                .join("-");
            return format!("ChatGPT-{segments}");
        }
    }
    for (prefix, display) in [
        ("claude-opus-5-fast", "Claude-Opus-5-Fast"),
        ("claude-opus-5", "Claude-Opus-5"),
        ("claude-sonnet-5", "Claude-Sonnet-5"),
        ("claude-fable-5", "Claude-Fable-5"),
        ("claude-opus-4-8-fast", "Claude-Opus-4.8-Fast"),
        ("claude-opus-4.8-fast", "Claude-Opus-4.8-Fast"),
        ("claude-opus-4-8", "Claude-Opus-4.8"),
        ("claude-opus-4.8", "Claude-Opus-4.8"),
        ("claude-opus-4-7-fast", "Claude-Opus-4.7-Fast"),
        ("claude-opus-4.7-fast", "Claude-Opus-4.7-Fast"),
        ("claude-opus-4-7", "Claude-Opus-4.7"),
        ("claude-opus-4.7", "Claude-Opus-4.7"),
        ("claude-opus-4-6", "Claude-Opus-4.6"),
        ("claude-opus-4.6", "Claude-Opus-4.6"),
        ("claude-sonnet-4-6", "Claude-Sonnet-4.6"),
        ("claude-sonnet-4.6", "Claude-Sonnet-4.6"),
        ("claude-opus-4-5", "Claude-Opus-4.5"),
        ("claude-opus-4.5", "Claude-Opus-4.5"),
        ("claude-sonnet-4-5", "Claude-Sonnet-4.5"),
        ("claude-sonnet-4.5", "Claude-Sonnet-4.5"),
        ("claude-haiku-4-5", "Claude-Haiku-4.5"),
        ("claude-haiku-4.5", "Claude-Haiku-4.5"),
        ("claude-opus-4", "Claude-Opus-4"),
        ("claude-sonnet-4", "Claude-Sonnet-4"),
        ("claude-haiku-4", "Claude-Haiku-4"),
        ("claude-4-opus", "Claude-Opus-4"),
        ("claude-4-sonnet", "Claude-Sonnet-4"),
        ("claude-4-haiku", "Claude-Haiku-4"),
        ("gemini-3-6-flash", "Gemini-3.6-Flash"),
        ("gemini-3.6-flash", "Gemini-3.6-Flash"),
        ("gemini-3-6-pro", "Gemini-3.6-Pro"),
        ("gemini-3.6-pro", "Gemini-3.6-Pro"),
        ("gemini-3-5-flash", "Gemini-3.5-Flash"),
        ("gemini-3.5-flash", "Gemini-3.5-Flash"),
        ("gemini-3-1-pro", "Gemini-3.1-Pro"),
        ("gemini-3.1-pro", "Gemini-3.1-Pro"),
        ("gemini-3-pro-image-preview", "Gemini-3-Pro-Image-Preview"),
        ("gemini-3-pro", "Gemini-3-Pro"),
        ("gemini-3-flash", "Gemini-3-Flash"),
        ("gemini-2-5-pro", "Gemini-2.5-Pro"),
        ("gemini-2.5-pro", "Gemini-2.5-Pro"),
        ("gemini-2-5-flash", "Gemini-2.5-Flash"),
        ("gemini-2.5-flash", "Gemini-2.5-Flash"),
        ("kimi-for-coding", "Kimi-For-Coding"),
        ("kimi-k2.7", "Kimi-K2.7-Code"),
        ("mimo-v2.5-pro", "MiMo-V2.5-Pro"),
        ("deepseek-v4-pro", "DeepSeek-V4-Pro"),
        ("deepseek-v4-flash", "DeepSeek-V4-Flash"),
        ("deepseek-v3.2", "DeepSeek-V3.2"),
        ("deepseek-v3.1", "DeepSeek-V3.1"),
        ("deepseek-v3", "DeepSeek-V3"),
        ("deepseek-r1", "DeepSeek-R1"),
        ("deepseek-reasoner", "DeepSeek-Reasoner"),
        ("deepseek-chat", "DeepSeek-Chat"),
        ("grok-4.5", "Grok-4.5"),
        ("cursor-composer-2.5", "Composer-2.5"),
        ("composer-2.5", "Composer-2.5"),
        ("glm-5.2", "GLM-5.2"),
        ("glm-5-2", "GLM-5.2"),
        ("ark-code-latest", "Ark-Code-Latest"),
        ("ark-code", "Ark-Code"),
        ("doubao-seed-code", "Doubao-Seed-Code"),
    ] {
        if canonical.starts_with(prefix) {
            return display.to_owned();
        }
    }
    if canonical == "k3" || canonical.starts_with("k3-") || canonical.starts_with("kimi-k3") {
        return "Kimi-K3".to_owned();
    }
    model_id
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .replace('_', "-")
}

pub fn fallback_channel_key(model_id: &str, base_url: &str) -> String {
    format!(
        "{}|{}",
        canonical_route_model_id(model_id),
        base_url.trim().trim_end_matches('/').to_ascii_lowercase()
    )
}

pub fn is_fallback_channel_selected(cfg: &RouterConfig, candidate: &ModelConfig) -> bool {
    let canonical = canonical_route_model_id(&candidate.model);
    cfg.fallback_channel_selections
        .get(&canonical)
        .is_none_or(|selected| {
            let key = fallback_channel_key(&candidate.model, &candidate.base_url);
            selected.iter().any(|item| item.eq_ignore_ascii_case(&key))
        })
}

pub fn next_api_channel_priority(cfg: &RouterConfig) -> i32 {
    cfg.models
        .iter()
        .filter(|model| model.source != "oauth")
        .map(|model| model.priority)
        .max()
        .unwrap_or(0)
        .max(0)
        .saturating_add(10)
}

pub fn is_oauth_fallback_model(cfg: &RouterConfig, candidate: &ModelConfig) -> bool {
    if !cfg.oauth_fallback.enabled || candidate.source == "oauth" {
        return false;
    }
    cfg.models.iter().any(|model| {
        model.source == "oauth"
            && cfg
                .oauth_account_ids
                .as_ref()
                .is_none_or(|ids| ids.contains(&model.oauth_account_id))
            && same_model_identity(&model.model, &candidate.model)
            && is_eligible_oauth_api_fallback(cfg, model, candidate)
    })
}

fn normalized_display_name(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

pub fn is_model_alias_customized(model: &ModelConfig) -> bool {
    if let Some(customized) = model.alias_customized {
        return customized;
    }
    if model.alias.trim().is_empty() {
        return false;
    }
    let recommended = recommended_model_display_name(&model.model);
    let mut automatic = vec![
        model.model.clone(),
        canonical_route_model_id(&model.model),
        recommended.clone(),
        format!("{recommended}(OAuth)"),
    ];
    if let Some(gpt_name) = recommended.strip_prefix("Chat") {
        automatic.push(gpt_name.to_owned());
        automatic.push(format!("{gpt_name}(OAuth)"));
    }
    if canonical_route_model_id(&model.model) == "deepseek-v4-pro" {
        automatic.push("DeepSeek V4 Pro".to_owned());
        automatic.push("DeepSeek-V4-Pro".to_owned());
        // Previous releases incorrectly recommended Flash for Pro.
        automatic.push("DeepSeek-V4-Flash".to_owned());
        automatic.push("DeepSeek V4 Flash".to_owned());
    }
    let alias = normalized_display_name(&model.alias);
    !automatic
        .iter()
        .any(|candidate| normalized_display_name(candidate) == alias)
}

#[allow(dead_code)]
pub fn resolved_model_display_name(cfg: &RouterConfig, model: &ModelConfig) -> String {
    if is_model_alias_customized(model) && !model.alias.trim().is_empty() {
        return model.alias.trim().to_owned();
    }
    let _ = cfg;
    recommended_model_display_name(&model.model)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextDefaults {
    pub window: i64,
    pub source_zh: &'static str,
    pub source_en: &'static str,
}

/// Documented model limits used when a Router model does not set a custom value.
/// Unknown ids deliberately receive a smaller compatibility default instead of
/// inheriting Codex's much larger fallback metadata.
pub fn detect_context_defaults(model_name: &str) -> ContextDefaults {
    let name = model_name.trim().to_ascii_lowercase();
    if name.contains("gpt-5.6-sol")
        || name.contains("gpt-5.6-terra")
        || name.contains("gpt-5.6-luna")
    {
        return ContextDefaults {
            window: 272_000,
            source_zh: "Codex 官方模型目录",
            source_en: "official Codex model catalog",
        };
    }
    if name == "k3" || name.contains("kimi-k3") {
        return ContextDefaults {
            window: 1_048_576,
            source_zh: "Kimi K3 官方文档",
            source_en: "official Kimi K3 documentation",
        };
    }
    if name.contains("claude-opus-5")
        || name.contains("claude-sonnet-5")
        || name.contains("claude-fable-5")
    {
        return ContextDefaults {
            window: 1_000_000,
            source_zh: "Anthropic 官方模型目录",
            source_en: "official Anthropic model catalog",
        };
    }
    if name.contains("gemini-3") {
        return ContextDefaults {
            window: 1_048_576,
            source_zh: "Gemini 官方模型页",
            source_en: "official Gemini model page",
        };
    }
    if name.contains("mimo-v2.5") {
        return ContextDefaults {
            window: 1_048_576,
            source_zh: "MiMo V2.5 官方模型页",
            source_en: "official MiMo V2.5 model page",
        };
    }
    if name.starts_with("ark-code") || name.contains("doubao-seed-code") {
        return ContextDefaults {
            window: 262_144,
            source_zh: "方舟 Coding Plan 文档",
            source_en: "Ark Coding Plan documentation",
        };
    }
    if name.contains("kimi-for-coding") || name.contains("k3-256k") {
        return ContextDefaults {
            window: 262_144,
            source_zh: "Kimi Code 官方文档",
            source_en: "official Kimi Code documentation",
        };
    }
    if name.contains("grok-4") {
        return ContextDefaults {
            window: 500_000,
            source_zh: "模型文档预设",
            source_en: "model documentation preset",
        };
    }
    if name.contains("deepseek-v4") {
        return ContextDefaults {
            window: 1_048_576,
            source_zh: "模型文档预设",
            source_en: "model documentation preset",
        };
    }
    ContextDefaults {
        window: 128_000,
        source_zh: "未知模型的保守兼容默认值",
        source_en: "conservative fallback for an unknown model",
    }
}

pub fn resolve_context_window(model: &ModelConfig) -> i64 {
    if model.context_window > 0 {
        model.context_window
    } else {
        detect_context_defaults(&model.model).window
    }
}

pub fn resolve_auto_compact_token_limit(model: &ModelConfig) -> i64 {
    let percent = model.auto_compact_percent.clamp(60, 90) as i64;
    resolve_context_window(model) * percent / 100
}

pub fn slugify(value: &str) -> String {
    let slug = value
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .to_lowercase();
    if slug.is_empty() {
        "model".to_string()
    } else {
        slug
    }
}

/// This manifest intentionally contains credential references, never API keys.
pub fn build_channel_manifest(cfg: &RouterConfig) -> Vec<serde_json::Value> {
    cfg.models
        .iter()
        .map(|model| {
            json!({
                "name": if model.alias.is_empty() { &model.model } else { &model.alias },
                "type": "openai",
                "base_url": model.base_url,
                "credential": model.credential_name,
                "models": [model.model.clone()],
                "priority": model.priority,
                "weight": model.weight,
                "supports_vision": resolve_multimodal(model),
                "extra": serde_json::from_str::<serde_json::Value>(&model.extra).unwrap_or_else(|_| json!({})),
            })
        })
        .collect()
}

fn model_catalog_path(router_root: &Path) -> PathBuf {
    let stable = crate::user_data::state_root(router_root).join("model-catalog.json");
    if stable != router_root.join("model-catalog.json") {
        stable
    } else {
        router_root.join("config").join("model-catalog.json")
    }
}

fn catalog_document(cfg: &RouterConfig, router_root: &Path) -> Value {
    json!({
        "fetched_at": chrono::Utc::now().to_rfc3339(),
        "etag": "codex-router-local-v2",
        "client_version": env!("CARGO_PKG_VERSION"),
        "models": build_model_catalog_with_root(cfg, router_root),
    })
}

pub fn write_model_catalog(cfg: &RouterConfig, router_root: &Path) -> anyhow::Result<()> {
    let catalog_path = model_catalog_path(router_root);
    if let Some(parent) = catalog_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write(
        &catalog_path,
        &serde_json::to_vec_pretty(&catalog_document(cfg, router_root))?,
    )
}

pub fn stamp_channel_route_metadata(model: &mut ModelConfig) {
    let profile = classify_channel_route(model);
    let mut extra = extra_object(&model.extra);
    extra.insert(
        "codex_router_vendor".to_owned(),
        Value::String(profile.vendor),
    );
    extra.insert(
        "codex_router_source_type".to_owned(),
        Value::String(profile.source_type.as_str().to_owned()),
    );
    extra.insert(
        "codex_router_gateway".to_owned(),
        Value::String(profile.gateway.as_str().to_owned()),
    );
    extra.insert(
        "codex_router_upstream_protocol".to_owned(),
        Value::String(profile.upstream_protocol.as_str().to_owned()),
    );
    extra.insert(
        "codex_router_billing_mode".to_owned(),
        Value::String(profile.billing_mode),
    );
    extra.insert("allow_fallback".to_owned(), Value::Bool(profile.allow_fallback));
    extra.insert(
        "codex_router_channel_kind".to_owned(),
        Value::String(profile.source_type.as_str().to_owned()),
    );
    extra
        .entry("openai_responses_mode".to_owned())
        .or_insert_with(|| Value::String(profile.upstream_protocol.responses_mode().to_owned()));
    if profile.base_url.to_ascii_lowercase().contains("api.openai.com")
        && extra
            .get("openai_responses_mode")
            .and_then(Value::as_str)
            .is_some_and(|mode| mode == "force_responses")
    {
        extra.remove("openai_responses_mode");
    }
    model.extra = serde_json::to_string(&Value::Object(extra)).unwrap_or_else(|_| "{}".to_owned());
}

pub fn write_all_files(cfg: &mut RouterConfig, router_root: &Path) -> anyhow::Result<()> {
    for model in &mut cfg.models {
        stamp_channel_route_metadata(model);
    }
    std::fs::create_dir_all(router_root.join("config"))?;
    // The deployment scripts resolve the Router config through
    // `Get-RouterConfigPath`, which points at the persistent user-data root for
    // packaged releases. Writing it next to the executable would leave
    // Apply-Router.ps1 deploying a stale config from a previous session.
    let config_path = crate::user_data::config_path(router_root);
    // The catalog and channel manifest stay beside the executable because the
    // scripts read them from `$routerRoot\config`.
    let catalog_path = model_catalog_path(router_root);
    let channels_path = router_root.join("config").join("sub2api-channels.json");
    let writes = vec![
        (
            catalog_path,
            serde_json::to_vec_pretty(&catalog_document(cfg, router_root))?,
        ),
        (
            channels_path,
            serde_json::to_vec_pretty(&build_channel_manifest(cfg))?,
        ),
        // The Router config is the commit marker and is replaced last.
        (config_path, serde_json::to_vec_pretty(cfg)?),
    ];
    let originals = writes
        .iter()
        .map(|(path, _)| match std::fs::read(path) {
            Ok(content) => Ok(Some(content)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        })
        .collect::<std::io::Result<Vec<_>>>()?;

    for (path, content) in &writes {
        if let Err(error) = atomic_write(path, content) {
            let mut rollback_failed = false;
            for ((rollback_path, _), original) in writes.iter().zip(&originals) {
                let restored = match original {
                    Some(original) => atomic_write(rollback_path, original),
                    None if rollback_path.is_file() => {
                        std::fs::remove_file(rollback_path).map_err(anyhow::Error::from)
                    }
                    None => Ok(()),
                };
                rollback_failed |= restored.is_err();
            }
            if rollback_failed {
                return Err(error).context("配置文件提交失败，且自动回滚未完全成功");
            }
            return Err(error).context("配置文件提交失败，已恢复应用前文件");
        }
    }
    Ok(())
}

struct SecretWide(Vec<u16>);

impl Drop for SecretWide {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

fn router_credential_target(name: &str) -> Vec<u16> {
    format!("CodexRouter/{name}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

fn router_credential_environment(name: &str) -> Option<&'static str> {
    match name {
        "AdminPassword" => Some("CODEX_ROUTER_ADMIN_PASSWORD"),
        "LocalApiKey" => Some("CODEX_ROUTER_LOCAL_API_KEY"),
        "CliManagementSecret" => Some("CODEX_ROUTER_CLI_MANAGEMENT_SECRET"),
        _ => None,
    }
}

fn read_router_credential(name: &str) -> anyhow::Result<Option<SecretWide>> {
    const ERROR_NOT_FOUND: i32 = 1168;

    if let Some(value) = router_credential_environment(name)
        .and_then(|variable| std::env::var(variable).ok())
        .filter(|value| !value.trim().is_empty())
    {
        return Ok(Some(SecretWide(value.encode_utf16().collect())));
    }

    let target = router_credential_target(name);
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
    if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_NOT_FOUND) {
            return Ok(None);
        }
        return Err(error).context("Windows Credential Manager read failed");
    }
    if credential.is_null() {
        bail!("Windows Credential Manager returned an empty record");
    }

    let result = (|| -> anyhow::Result<SecretWide> {
        let record = unsafe { &*credential };
        if record.CredentialBlobSize % 2 != 0 {
            bail!("Windows Credential Manager returned an invalid credential encoding");
        }
        if record.CredentialBlobSize == 0 {
            return Ok(SecretWide(Vec::new()));
        }
        if record.CredentialBlob.is_null() {
            bail!("Windows Credential Manager returned an empty credential blob");
        }
        let units = unsafe {
            std::slice::from_raw_parts(
                record.CredentialBlob.cast::<u16>(),
                record.CredentialBlobSize as usize / 2,
            )
        };
        Ok(SecretWide(units.to_vec()))
    })();
    unsafe { CredFree(credential.cast()) };
    result.map(Some)
}

fn write_router_credential(name: &str, secret: &[u16]) -> anyhow::Result<()> {
    if router_credential_environment(name)
        .is_some_and(|variable| std::env::var_os(variable).is_some())
    {
        bail!("environment-overridden credential is read-only");
    }
    if name.trim().is_empty() || name.contains('\0') {
        bail!("Windows credential name is invalid");
    }
    let mut target = router_credential_target(name);
    let mut username = std::env::var("USERNAME")
        .unwrap_or_default()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let blob_size = u32::try_from(secret.len().saturating_mul(std::mem::size_of::<u16>()))
        .context("Windows credential is too large")?;
    if blob_size > 2560 {
        bail!("Windows credential exceeds 2560 bytes");
    }
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        CredentialBlobSize: blob_size,
        CredentialBlob: secret.as_ptr().cast_mut().cast::<u8>(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: username.as_mut_ptr(),
        ..Default::default()
    };
    let written = unsafe { CredWriteW(&credential, 0) } != 0;
    username.fill(0);
    target.fill(0);
    if !written {
        return Err(std::io::Error::last_os_error())
            .context("Windows Credential Manager write failed");
    }
    Ok(())
}

pub(crate) fn read_router_credential_text(name: &str) -> anyhow::Result<Option<Zeroizing<String>>> {
    read_router_credential(name)?
        .map(|value| String::from_utf16(&value.0).map(Zeroizing::new))
        .transpose()
        .context("Windows credential contains invalid UTF-16")
}

#[cfg(test)]
pub(crate) fn write_router_credential_text(name: &str, secret: &str) -> anyhow::Result<()> {
    let mut encoded = secret.encode_utf16().collect::<Vec<_>>();
    let result = write_router_credential(name, &encoded);
    encoded.fill(0);
    result
}

fn delete_router_credential(name: &str) -> anyhow::Result<()> {
    const ERROR_NOT_FOUND: i32 = 1168;

    let target = router_credential_target(name);
    if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_NOT_FOUND) {
            return Err(error).context("Windows Credential Manager delete failed");
        }
    }
    Ok(())
}

pub(crate) fn remove_isolated_profile_credentials(names: &[String]) -> anyhow::Result<()> {
    let mut first_error = None;
    for name in names {
        if name.trim().is_empty() {
            continue;
        }
        if let Err(error) = delete_router_credential(name) {
            first_error.get_or_insert(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn secure_random_bytes(buf: &mut [u8]) -> anyhow::Result<()> {
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x00000002;
    if unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    } != 0
    {
        bail!("BCryptGenRandom failed");
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

pub fn ensure_local_api_key() -> anyhow::Result<String> {
    if let Some(secret) = read_router_credential("LocalApiKey")? {
        if !secret.0.is_empty() {
            return String::from_utf16(&secret.0)
                .context("the local Router credential has invalid UTF-16 encoding");
        }
    }
    let mut bytes = [0u8; 32];
    secure_random_bytes(&mut bytes).context("failed to generate local Router key")?;
    let key = format!("sk-local-{}", hex_encode(&bytes));
    write_router_credential("LocalApiKey", &key.encode_utf16().collect::<Vec<_>>())
        .context("failed to store local Router key")?;
    Ok(key)
}

pub fn resolve_proxy_runtime(cfg: &RouterConfig) -> anyhow::Result<crate::proxy::ProxyRuntime> {
    let password = if !cfg.proxy.password.is_empty() {
        Some(Zeroizing::new(cfg.proxy.password.clone()))
    } else if cfg.proxy.enabled && !cfg.proxy.username.trim().is_empty() {
        read_router_credential(&cfg.proxy.password_credential)?
            .map(|value| String::from_utf16(&value.0))
            .transpose()
            .context("the proxy credential has invalid UTF-16 encoding")?
            .map(Zeroizing::new)
    } else {
        None
    };
    let settings =
        crate::proxy::resolve_current(&cfg.proxy, password.as_deref().map(String::as_str))?;
    let targets = crate::proxy::evaluate_targets(
        &settings,
        cfg.models
            .iter()
            .filter(|model| model.source != "oauth")
            .map(|model| model.base_url.as_str()),
    );
    Ok(crate::proxy::ProxyRuntime { settings, targets })
}

pub fn store_credentials(cfg: &mut RouterConfig, _router_root: &Path) -> anyhow::Result<usize> {
    let mut writes = Vec::<(String, SecretWide)>::new();
    let mut updated_model_keys = 0usize;
    for (index, model) in cfg.models.iter_mut().enumerate() {
        if model.source == "oauth" {
            model.credential_name.clear();
            continue;
        }
        if model.credential_name.trim().is_empty() {
            model.credential_name = format!("ModelApiKey-{}-{}", index + 1, slugify(&model.model));
        }
        if !model.api_key.trim().is_empty() {
            writes.push((
                model.credential_name.clone(),
                SecretWide(model.api_key.trim().encode_utf16().collect()),
            ));
            updated_model_keys += 1;
        }
        if is_volcengine_plan_url(&model.base_url) {
            if !model.volcengine_access_key_id.trim().is_empty() {
                writes.push((
                    "VolcengineAccessKeyId".to_owned(),
                    SecretWide(
                        model
                            .volcengine_access_key_id
                            .trim()
                            .encode_utf16()
                            .collect(),
                    ),
                ));
            }
            if !model.volcengine_secret_access_key.trim().is_empty() {
                writes.push((
                    "VolcengineSecretAccessKey".to_owned(),
                    SecretWide(
                        model
                            .volcengine_secret_access_key
                            .trim()
                            .encode_utf16()
                            .collect(),
                    ),
                ));
            }
        }
    }
    if cfg.proxy.password_credential.trim().is_empty() {
        cfg.proxy.password_credential = "ProxyPassword".to_string();
    }
    if !cfg.proxy.password.is_empty() {
        writes.push((
            cfg.proxy.password_credential.clone(),
            SecretWide(cfg.proxy.password.encode_utf16().collect()),
        ));
    }
    for (name, secret) in &writes {
        write_router_credential(name, &secret.0)
            .with_context(|| format!("failed to store Windows credential {name}"))?;
    }
    for model in &mut cfg.models {
        model.api_key.clear();
        model.volcengine_access_key_id.clear();
        model.volcengine_secret_access_key.clear();
    }
    cfg.proxy.password.clear();
    cfg.local_api_key.clear();
    Ok(updated_model_keys)
}

pub fn is_volcengine_plan_url(base_url: &str) -> bool {
    let base_url = base_url.to_ascii_lowercase();
    base_url.contains("ark.cn-beijing.volces.com/api/coding")
        || base_url.contains("ark.cn-beijing.volces.com/api/plan")
}

pub fn isolate_profile_credentials(
    cfg: &mut RouterConfig,
    _router_root: &Path,
    profile_id: &str,
) -> anyhow::Result<Vec<String>> {
    let safe_id = slugify(profile_id);
    let mut writes = Vec::<(String, SecretWide)>::new();
    let mut replacements = Vec::new();
    for (index, model) in cfg.models.iter().enumerate() {
        if model.source == "oauth" {
            replacements.push(String::new());
            continue;
        }
        let short_model = slugify(&model.model).chars().take(40).collect::<String>();
        let new_name = format!("Profile-{safe_id}-Model-{}-{short_model}", index + 1);
        let secret = if model.api_key.trim().is_empty() {
            if model.credential_name.trim().is_empty() {
                bail!("ROUTER_PROFILE_CREDENTIAL_MISSING");
            }
            let mut secret = None;
            for attempt in 0..3 {
                secret = read_router_credential(&model.credential_name)
                    .context("ROUTER_PROFILE_CREDENTIAL_READ_FAILED")?
                    .filter(|value| !value.0.is_empty());
                if secret.is_some() {
                    break;
                }
                if attempt < 2 {
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
            secret.context("ROUTER_PROFILE_CREDENTIAL_MISSING")?
        } else {
            SecretWide(model.api_key.encode_utf16().collect())
        };
        writes.push((new_name.clone(), secret));
        replacements.push(new_name);
    }

    let proxy_name = format!("Profile-{safe_id}-ProxyPassword");
    if cfg.proxy.enabled {
        let proxy_secret = if !cfg.proxy.password.is_empty() {
            Some(SecretWide(cfg.proxy.password.encode_utf16().collect()))
        } else if !cfg.proxy.password_credential.trim().is_empty() {
            read_router_credential(&cfg.proxy.password_credential)
                .context("ROUTER_PROFILE_CREDENTIAL_READ_FAILED")?
                .filter(|value| !value.0.is_empty())
        } else {
            None
        };
        if let Some(secret) = proxy_secret {
            writes.push((proxy_name.clone(), secret));
        }
    }

    let mut written = Vec::with_capacity(writes.len());
    for (name, secret) in &writes {
        if write_router_credential(name, &secret.0).is_err() {
            let _ = remove_isolated_profile_credentials(&written);
            bail!("ROUTER_PROFILE_CREDENTIAL_WRITE_FAILED");
        }
        written.push(name.clone());
    }

    for (model, credential_name) in cfg.models.iter_mut().zip(replacements) {
        model.credential_name = credential_name;
        model.api_key.clear();
    }
    cfg.proxy.password_credential = proxy_name;
    cfg.proxy.password.clear();
    Ok(written)
}

pub fn protect_file_for_current_user(source: &Path, destination: &Path) -> anyhow::Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut source_bytes =
        std::fs::read(source).with_context(|| format!("无法读取 {}", source.display()))?;
    let result = crypt_protect_for_current_user(&source_bytes)
        .and_then(|protected| std::fs::write(destination, protected).map_err(Into::into));
    source_bytes.fill(0);
    result.with_context(|| format!("无法保护 {}", destination.display()))
}

pub fn unprotect_file_for_current_user(source: &Path, destination: &Path) -> anyhow::Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut protected =
        std::fs::read(source).with_context(|| format!("无法读取 {}", source.display()))?;
    let result = crypt_unprotect_for_current_user(&protected).and_then(|mut plaintext| {
        let write_result = std::fs::write(destination, &plaintext).map_err(Into::into);
        plaintext.fill(0);
        write_result
    });
    protected.fill(0);
    result.with_context(|| format!("无法解密 {}", source.display()))
}

fn crypt_protect_for_current_user(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    crypt_dpapi(data, true)
}

fn crypt_unprotect_for_current_user(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    crypt_dpapi(data, false)
}

fn crypt_dpapi(data: &[u8], protect: bool) -> anyhow::Result<Vec<u8>> {
    let data_len = u32::try_from(data.len()).context("DPAPI 输入文件过大")?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: data_len,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let succeeded = unsafe {
        if protect {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error()).context("Windows DPAPI 操作失败");
    }

    let result = if output.cbData == 0 {
        Vec::new()
    } else if output.pbData.is_null() {
        unsafe { LocalFree(output.pbData.cast()) };
        bail!("Windows DPAPI 返回了无效的输出缓冲区");
    } else {
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec()
    };
    if !output.pbData.is_null() {
        unsafe {
            std::ptr::write_bytes(output.pbData, 0, output.cbData as usize);
            LocalFree(output.pbData.cast());
        }
    }
    Ok(result)
}

#[cfg(test)]
fn terminate_deployment_process_tree(child: &mut std::process::Child) {
    let taskkill = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .map(|root| root.join("System32").join("taskkill.exe"))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("taskkill.exe"));
    let _ = Command::new(taskkill)
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x08000000)
        .status();
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[cfg(test)]
pub fn run_apply_script<F>(router_root: &Path, on_line: F) -> anyhow::Result<()>
where
    F: FnMut(String),
{
    let cancel = AtomicBool::new(false);
    run_apply_script_with_cancel(router_root, &cancel, None, on_line)
}

#[cfg(test)]
pub fn run_apply_script_with_cancel<F>(
    router_root: &Path,
    cancel: &AtomicBool,
    proxy: Option<&crate::proxy::ProxyRuntime>,
    mut on_line: F,
) -> anyhow::Result<()>
where
    F: FnMut(String),
{
    const DEPLOYMENT_COMPLETE_MARKER: &str = "[codex-router:deployment-complete]";

    fn safe_output_line(line: &str) -> String {
        // Machine-generated deployment flags. Apply-Router builds them from ids,
        // counts, platforms, and already sanitized reasons, so they pass through
        // verbatim and stay greppable for debugging.
        if let Some(flag) = line.trim().strip_prefix("CR-FLAG ") {
            let mut safe = String::from("CR-FLAG ");
            safe.push_str(flag.trim());
            safe.truncate(400);
            return safe;
        }
        for prefix in [
            "[1/7]", "[2/7]", "[3/7]", "[4/7]", "[5/7]", "[6/7]", "[7/7]",
        ] {
            if line.starts_with(prefix) {
                return prefix.to_owned();
            }
        }
        // Normal Apply progress must keep a stable English marker so the UI can
        // localize it. Running it through the error classifier first turns every
        // "Updated channel:" line into class=unclassified_error.
        const PROGRESS_MARKERS: &[&str] = &[
            "Router compliance acknowledgement recorded",
            "Router administrator ready",
            "Codex model catalog generated",
            "Composite routes",
            "Updated channel:",
            "Created channel:",
            "isolated until recovery",
            "Outbound proxy reconciliation",
            "Catalog availability filter",
            "OAuth on-demand recovery delegated",
            "Autostart registered",
            "Autostart removed",
            "will start directly in lightweight tray mode",
            "model channel(s).",
            "Codex configuration written to",
            "Local access key is stored in Windows Credential Manager",
            "Codex Router is running at",
            "Codex Router secrets and data directory",
            "Codex Router is stopped",
            "Configured ",
        ];
        if let Some(marker) = PROGRESS_MARKERS
            .iter()
            .find(|marker| line.contains(**marker))
        {
            return (*marker).to_owned();
        }
        let summary = crate::runtime_logs::summarize_error_for_display(line);
        // PowerShell Write-Warning traffic is often informational (discovery
        // skips, proxy notes). Surface it as a note, not a hard diagnostic.
        if line.contains("WARNING:")
            || summary.contains("class=warning")
            || summary.contains("class=rate_limit")
        {
            return format!("deployment_warning {summary}");
        }
        format!("deployment_diagnostic {summary}")
    }

    if cancel.load(Ordering::Acquire) {
        bail!("部署已因程序退出而取消");
    }

    let script = router_root.join("scripts").join("apply-codex-router.ps1");
    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .current_dir(router_root)
        .env("CODEX_ROUTER_CONFIG_LOCK_HELD", "1")
        .env("CODEX_ROUTER_NATIVE_LIFECYCLE", "1")
        .env("CODEX_ROUTER_NATIVE_AUTOSTART", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x08000000);
    if let Some(proxy) = proxy {
        let settings = &proxy.settings;
        command
            .env("CODEX_ROUTER_NATIVE_PROXY", "1")
            .env("CODEX_ROUTER_PROXY_MODE", &settings.mode)
            .env("CODEX_ROUTER_PROXY_SOURCE", &settings.source)
            .env("CODEX_ROUTER_NO_PROXY", &settings.no_proxy)
            .env(
                "CODEX_ROUTER_PROXY_HAS_CREDENTIALS",
                if settings.has_credentials { "1" } else { "0" },
            )
            .env(
                "CODEX_ROUTER_PROXY_SUPPORTS_ACCOUNT_BINDING",
                if settings.supports_account_binding {
                    "1"
                } else {
                    "0"
                },
            );
        if let Ok(targets) = serde_json::to_string(&proxy.targets) {
            command.env("CODEX_ROUTER_PROXY_TARGET_POLICIES", targets);
        }
        if let Some(proxy_url) = &settings.proxy_url {
            command.env("CODEX_ROUTER_PROXY_URL", proxy_url);
        } else {
            command.env_remove("CODEX_ROUTER_PROXY_URL");
        }
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("无法运行 {}", script.display()))?;

    let stdout = child.stdout.take().context("无法读取部署脚本输出")?;
    let stderr = child.stderr.take().context("无法读取部署脚本错误输出")?;
    let (line_tx, line_rx) = mpsc::channel::<(bool, String)>();
    let stdout_tx = line_tx.clone();
    let stdout_reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let deployment_complete = line.starts_with("[7/7]");
            if stdout_tx.send((false, line)).is_err() {
                break;
            }
            if deployment_complete {
                break;
            }
        }
    });
    let stderr_reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.trim() == DEPLOYMENT_COMPLETE_MARKER {
                break;
            }
            if line_tx.send((true, line)).is_err() {
                break;
            }
        }
    });

    let started = Instant::now();
    let timeout = Duration::from_secs(180);
    let mut stderr_tail = VecDeque::with_capacity(12);
    let mut output_tail = VecDeque::with_capacity(20);
    let status = loop {
        while let Ok((is_error, line)) = line_rx.try_recv() {
            if line.trim() == DEPLOYMENT_COMPLETE_MARKER {
                continue;
            }
            if !line.trim().is_empty() {
                if output_tail.len() == 20 {
                    output_tail.pop_front();
                }
                output_tail.push_back(line.clone());
                if is_error {
                    if stderr_tail.len() == 12 {
                        stderr_tail.pop_front();
                    }
                    stderr_tail.push_back(line.clone());
                }
                on_line(safe_output_line(&line));
            }
        }
        if let Some(status) = child.try_wait().context("无法读取部署脚本状态")? {
            break status;
        }
        if cancel.load(Ordering::Acquire) {
            terminate_deployment_process_tree(&mut child);
            bail!("部署已因程序退出而取消");
        }
        if started.elapsed() >= timeout {
            // Terminate only the deployment shell. Start-Router launches its
            // services independently, so a UI timeout must never tear down a
            // healthy gateway (and its active streams) as a process tree.
            let _ = child.kill();
            let _ = child.wait();
            bail!("部署超过 180 秒，仅停止了配置脚本；已启动的 Router 服务未被强制终止。请查看界面中的最后一个部署阶段。");
        }
        std::thread::sleep(Duration::from_millis(60));
    };

    // A service started by PowerShell can retain an inherited output handle
    // after the deployment shell exits. Never block the UI completion event
    // indefinitely while joining a reader that is waiting on that handle.
    for _ in 0..20 {
        if stdout_reader.is_finished() && stderr_reader.is_finished() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if stdout_reader.is_finished() {
        let _ = stdout_reader.join();
    }
    if stderr_reader.is_finished() {
        let _ = stderr_reader.join();
    }
    while let Ok((is_error, line)) = line_rx.try_recv() {
        if line.trim() == DEPLOYMENT_COMPLETE_MARKER {
            continue;
        }
        if !line.trim().is_empty() {
            if output_tail.len() == 20 {
                output_tail.pop_front();
            }
            output_tail.push_back(line.clone());
            if is_error {
                if stderr_tail.len() == 12 {
                    stderr_tail.pop_front();
                }
                stderr_tail.push_back(line.clone());
            }
            on_line(safe_output_line(&line));
        }
    }

    if !status.success() {
        let stderr_details = stderr_tail.into_iter().collect::<Vec<_>>().join("\n");
        let details = if stderr_details.trim().is_empty() {
            output_tail.into_iter().collect::<Vec<_>>().join("\n")
        } else {
            stderr_details
        };
        let safe_details = if details.trim().is_empty() {
            "class=unclassified_error".to_owned()
        } else {
            crate::runtime_logs::summarize_error_for_display(details.trim())
        };
        bail!(
            "一键配置失败（退出代码 {}）: {}",
            status.code().unwrap_or(-1),
            safe_details
        );
    }
    Ok(())
}

pub fn run_stop_router_script(router_root: &Path) -> anyhow::Result<()> {
    crate::lifecycle::stop_services(router_root, false, false).map(|_| ())
}

fn is_router_provider_id(provider_id: &str) -> bool {
    matches!(provider_id, "codex_router" | "custom" | "sub2api")
}

pub(crate) fn codex_config_uses_router(config_text: &str, router_base_url: &str) -> bool {
    let Ok(document) = config_text.parse::<DocumentMut>() else {
        return false;
    };
    let Some(provider_id) = document
        .get("model_provider")
        .and_then(Item::as_str)
        .filter(|value| is_router_provider_id(value))
    else {
        return false;
    };
    let expected_base = format!("{}/v1", router_base_url.trim_end_matches('/'));
    let Some(provider) = document
        .get("model_providers")
        .and_then(Item::as_table_like)
        .and_then(|providers| providers.get(provider_id))
        .and_then(Item::as_table_like)
    else {
        return false;
    };
    let base_matches = provider
        .get("base_url")
        .and_then(Item::as_str)
        .is_some_and(|value| value.trim_end_matches('/') == expected_base);
    if !base_matches {
        return false;
    }
    let openai_auth = provider
        .get("requires_openai_auth")
        .and_then(Item::as_bool);
    if openai_auth.is_none()
        || provider
            .get("wire_api")
            .and_then(Item::as_str)
            .filter(|value| *value == "responses")
            .is_none()
        || provider
            .get("experimental_bearer_token")
            .and_then(Item::as_str)
            .filter(|value| !value.trim().is_empty())
            .is_none()
        || document
            .get("model_catalog_json")
            .and_then(Item::as_str)
            .filter(|value| !value.trim().is_empty())
            .is_none()
    {
        return false;
    }
    // Reject third-party profiles that reuse the custom id but point at a local
    // URL while naming themselves something other than Codex-Router.
    match provider.get("name").and_then(Item::as_str) {
        None | Some("Codex-Router") => true,
        Some(_) => false,
    }
}

fn local_router_health_available(router_base_url: &str) -> bool {
    let Ok(url) = url::Url::parse(router_base_url.trim_end_matches('/')) else {
        return false;
    };
    if url.scheme() != "http" || !matches!(url.host_str(), Some("127.0.0.1" | "localhost")) {
        return false;
    }
    let Some(port) = url.port_or_known_default() else {
        return false;
    };
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let timeout = Duration::from_millis(450);
    let Ok(mut stream) = TcpStream::connect_timeout(&address, timeout) else {
        return false;
    };
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
    {
        return false;
    }
    let base_path = url.path().trim_end_matches('/');
    let health_path = if base_path.is_empty() {
        "/health".to_owned()
    } else {
        format!("{base_path}/health")
    };
    let request = format!(
        "GET {health_path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = String::new();
    if stream.take(8192).read_to_string(&mut response).is_err() {
        return false;
    }
    matches!(
        response.lines().next(),
        Some(status) if status.starts_with("HTTP/1.1 200") || status.starts_with("HTTP/1.0 200")
    )
}

pub fn codex_router_mode_configured(cfg: &RouterConfig) -> bool {
    let config_path = resolve_codex_home(cfg).join("config.toml");
    let Ok(config_text) = std::fs::read_to_string(config_path) else {
        return false;
    };
    let router_base_url = cfg.deploy.sub2api_host.trim();
    let gateway = responses_gateway::responses_gateway_url(router_base_url)
        .unwrap_or_else(|_| router_base_url.to_owned());
    codex_config_uses_router(&config_text, &gateway)
        || codex_config_uses_router(&config_text, router_base_url)
}

pub fn codex_router_mode_active(cfg: &RouterConfig) -> bool {
    codex_router_mode_configured(cfg)
        && local_router_health_available(cfg.deploy.sub2api_host.trim())
}

pub fn load_oauth_accounts(router_root: &Path) -> anyhow::Result<Vec<crate::OAuthAccountSummary>> {
    let config = RouterConfig::load(&crate::user_data::config_path(router_root))
        .unwrap_or_default();
    load_oauth_accounts_with_config(router_root, &config)
}

pub fn load_oauth_accounts_with_config(
    router_root: &Path,
    config: &RouterConfig,
) -> anyhow::Result<Vec<crate::OAuthAccountSummary>> {
    let mut last_failure = "ROUTER_OAUTH_ACCOUNTS_UNAVAILABLE: class=request_failure".to_owned();
    let mut attempted_repair = false;
    for attempt in 0..5 {
        if attempt == 0 {
            let wait_rounds = if crate::user_data::config_looks_configured(config) {
                8
            } else {
                2
            };
            for _ in 0..wait_rounds {
                if local_router_health_available(&config.deploy.sub2api_host) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        if !attempted_repair
            && attempt > 0
            && oauth_accounts_failure_needs_router_repair(&last_failure)
        {
            attempted_repair = true;
            let _ = ensure_router_healthy_with_config(router_root, config);
            std::thread::sleep(Duration::from_millis(800));
        }
        match load_oauth_accounts_native(router_root, config) {
            Ok(accounts) => return Ok(accounts),
            Err(error) => {
                last_failure = format!(
                    "ROUTER_OAUTH_ACCOUNTS_UNAVAILABLE: {}",
                    native_admin_error(&error)
                );
            }
        }

        if attempt + 1 < 5 && oauth_accounts_failure_is_retryable(&last_failure) {
            std::thread::sleep(Duration::from_millis(1200 + attempt as u64 * 800));
        } else {
            break;
        }
    }
    bail!("{last_failure}")
}

fn load_oauth_accounts_native(
    router_root: &Path,
    config: &RouterConfig,
) -> anyhow::Result<Vec<crate::OAuthAccountSummary>> {
    let admin = usage::retry_admin_read(|| usage::AdminClient::connect(router_root, config))?;
    let groups = usage::data(usage::retry_account_read(|| {
        admin.get(
            "/api/v1/admin/groups/all?include_inactive=true",
            Duration::from_secs(10),
        )
    })?);
    let router_group_id = usage::array(&groups)
        .iter()
        .find(|group| usage::string(group, "name") == "Codex-Router")
        .map(|group| usage::integer(group, "id"))
        .unwrap_or_default();
    let accounts_body = usage::data(usage::retry_account_read(|| {
        admin.get(
            "/api/v1/admin/accounts?page=1&page_size=200",
            Duration::from_secs(10),
        )
    })?);
    let accounts = usage::get(&accounts_body, "items").unwrap_or(&accounts_body);
    let selected = config
        .oauth_account_ids
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect::<HashSet<_>>();
    let mut summaries = Vec::<(String, crate::OAuthAccountSummary)>::new();
    let mut summary_by_identity = HashMap::<String, usize>::new();
    let mut detail_failures = 0;
    let mut oauth_count = 0;

    for account in usage::array(accounts) {
        if usage::string(account, "type") != "oauth" {
            continue;
        }
        oauth_count += 1;
        let account_id = usage::integer(account, "id");
        if account_id <= 0 {
            continue;
        }
        let path = format!("/api/v1/admin/accounts/{account_id}");
        let detail = match usage::retry_account_read(|| admin.get(&path, Duration::from_secs(10))) {
            Ok(detail) => usage::data(detail),
            Err(_) => {
                detail_failures += 1;
                continue;
            }
        };
        let platform = usage::string(&detail, "platform").to_ascii_lowercase();
        let (models, models_error) =
            oauth_model_catalog_result(load_live_oauth_models(&admin, account_id, &platform));

        let credentials = usage::get(&detail, "credentials").unwrap_or(&Value::Null);
        let extra = usage::get(&detail, "extra").unwrap_or(&Value::Null);
        let email = first_nonempty([
            usage::string(credentials, "email"),
            usage::string(extra, "email"),
        ]);
        let plan = first_nonempty([
            usage::string(credentials, "plan_type"),
            usage::string(credentials, "tier_id"),
            usage::string(extra, "subscription_tier"),
        ]);
        let group_ids = group_ids(&detail, account);
        let mut expires_at = first_nonempty([
            usage::string(&detail, "expires_at"),
            usage::string(credentials, "expires_at"),
        ]);
        if expires_at.len() >= 9
            && expires_at.len() <= 12
            && expires_at.bytes().all(|byte| byte.is_ascii_digit())
        {
            expires_at = expires_at
                .parse::<i64>()
                .ok()
                .and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp, 0))
                .map(|date| date.to_rfc3339())
                .unwrap_or(expires_at);
        }
        let error = first_nonempty([
            usage::string(&detail, "error_message"),
            usage::string(&detail, "temp_unschedulable_reason"),
        ]);
        let summary = crate::OAuthAccountSummary {
            id: usage::integer(&detail, "id").max(account_id),
            name: usage::string(&detail, "name"),
            platform: platform.clone(),
            status: usage::string(&detail, "status"),
            email,
            plan,
            priority: usage::integer(&detail, "priority")
                .try_into()
                .unwrap_or_default(),
            bound_to_router: router_group_id > 0 && group_ids.contains(&router_group_id),
            error,
            expires_at,
            models,
            models_error,
        };
        let identity = oauth_stable_identity(&platform, credentials, extra, account_id);
        if let Some(index) = summary_by_identity.get(&identity).copied() {
            if prefer_oauth_summary(&summaries[index].1, &summary, &selected) {
                summaries[index] = (identity, summary);
            }
        } else {
            summary_by_identity.insert(identity.clone(), summaries.len());
            summaries.push((identity, summary));
        }
    }
    if oauth_count > 0 && summaries.is_empty() && detail_failures > 0 {
        bail!("class=request_failure")
    }
    Ok(summaries.into_iter().map(|(_, summary)| summary).collect())
}

fn load_live_oauth_models(
    admin: &usage::AdminClient,
    account_id: i64,
    platform: &str,
) -> anyhow::Result<Vec<crate::OAuthModelSummary>> {
    let sync_path = format!("/api/v1/admin/accounts/{account_id}/models/sync-upstream");
    let sync_result = admin
        .post(&sync_path, None, Duration::from_secs(15))
        .map(|body| oauth_models_from_response(platform, body));

    // Some providers do not implement sync-upstream, while providers that do
    // return a string array instead of the stable catalog's model objects. The
    // canonical GET endpoint is account-scoped and remains the best UI source
    // after sync has refreshed it.
    let catalog_path = format!("/api/v1/admin/accounts/{account_id}/models");
    let catalog_result =
        usage::retry_account_read(|| admin.get(&catalog_path, Duration::from_secs(15)))
            .map(|body| oauth_models_from_response(platform, body));

    match catalog_result {
        Ok(models) if !models.is_empty() => Ok(models),
        Ok(_) => match sync_result {
            Ok(models) => Ok(models),
            Err(_) => Ok(Vec::new()),
        },
        Err(catalog_error) => match sync_result {
            Ok(models) if !models.is_empty() => Ok(models),
            Ok(_) | Err(_) => Err(catalog_error),
        },
    }
}

fn oauth_models_from_response(platform: &str, response: Value) -> Vec<crate::OAuthModelSummary> {
    let body = usage::data(response);
    let items = usage::get(&body, "models")
        .or_else(|| usage::get(&body, "items"))
        .unwrap_or(&body);
    discovered_oauth_models(platform, items)
}

fn oauth_model_catalog_result(
    result: anyhow::Result<Vec<crate::OAuthModelSummary>>,
) -> (Vec<crate::OAuthModelSummary>, String) {
    match result {
        Ok(models) => (models, String::new()),
        Err(error) => (Vec::new(), native_admin_error(&error)),
    }
}

fn group_ids(detail: &Value, summary: &Value) -> Vec<i64> {
    let parse = |value: &Value| {
        usage::array(value)
            .iter()
            .filter_map(|item| item.as_i64().or_else(|| item.as_str()?.parse().ok()))
            .filter(|id| *id >= 0)
            .collect::<Vec<_>>()
    };
    let detailed = usage::get(detail, "group_ids")
        .map(parse)
        .unwrap_or_default();
    if detailed.is_empty() {
        usage::get(summary, "group_ids")
            .map(parse)
            .unwrap_or_default()
    } else {
        detailed
    }
}

fn first_nonempty<const N: usize>(values: [String; N]) -> String {
    values
        .into_iter()
        .find(|value| !value.trim().is_empty())
        .unwrap_or_default()
}

fn oauth_stable_identity(
    platform: &str,
    credentials: &Value,
    extra: &Value,
    account_id: i64,
) -> String {
    oauth::stable_identity(platform, credentials, extra)
        .unwrap_or_else(|| format!("row|{account_id}"))
}

fn prefer_oauth_summary(
    current: &crate::OAuthAccountSummary,
    candidate: &crate::OAuthAccountSummary,
    selected: &HashSet<i64>,
) -> bool {
    let candidate_selected = selected.contains(&candidate.id);
    let current_selected = selected.contains(&current.id);
    (candidate_selected && !current_selected)
        || (candidate_selected == current_selected
            && candidate.bound_to_router
            && !current.bound_to_router)
        || (candidate_selected == current_selected
            && candidate.bound_to_router == current.bound_to_router
            && candidate.id > 0
            && (current.id <= 0 || candidate.id < current.id))
}

fn discovered_oauth_models(platform: &str, models: &Value) -> Vec<crate::OAuthModelSummary> {
    let mut discovered = Vec::new();
    let mut seen = HashSet::new();
    for model in usage::array(models) {
        let mut model_id = model
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| usage::string(model, "id"));
        if platform == "openai" && model_id == "gpt-5.6" {
            model_id = "gpt-5.6-sol".to_owned();
        } else if platform == "antigravity"
            && matches!(
                model_id.as_str(),
                "gemini-3.7-flash-high" | "gemini-3.7-flash-medium" | "gemini-3.7-flash-low"
            )
        {
            model_id = "gemini-3.7-flash".to_owned();
        }
        if model_id.is_empty() || !seen.insert(model_id.clone()) {
            continue;
        }
        let mut display_name = usage::string(model, "display_name");
        if display_name.is_empty() {
            display_name = usage::string(model, "displayName");
        }
        if model_id == "gpt-5.6-sol" {
            display_name = "ChatGPT-5.6-Sol".to_owned();
        } else if model_id == "gemini-3.7-flash" {
            display_name = "Gemini 3.7 Flash".to_owned();
        } else if display_name.is_empty() {
            display_name.clone_from(&model_id);
        }
        discovered.push(crate::OAuthModelSummary {
            id: model_id,
            display_name,
        });
    }
    discovered
}

fn oauth_accounts_failure_needs_router_repair(summary: &str) -> bool {
    let lower = summary.to_ascii_lowercase();
    lower.contains("class=request_failure")
        || lower.contains("rate-limited")
        || lower.contains("429")
        || lower.contains("no access token")
        || lower.contains("503")
        || lower.contains("install_root")
        || lower.contains("health check failed")
        || lower.contains("connection_refused")
}

fn ensure_router_healthy_with_config(router_root: &Path, config: &RouterConfig) -> bool {
    let cancel = AtomicBool::new(false);
    crate::lifecycle::ensure_services_with_config(router_root, config, true, &cancel, false).is_ok()
}

fn native_admin_error(error: &anyhow::Error) -> String {
    let message = error.to_string();
    if message.contains("class=") {
        return message;
    }
    if let Some(code) = extract_router_error_code(&message) {
        let class = if code.contains("VAL-0003") || message.contains("COMPLIANCE") {
            "compliance_required"
        } else if code.contains("AUT-") || message.contains("admin_session") {
            "admin_session"
        } else if code.starts_with("CR-CFG-") || message.contains("ROUTER_DEPLOY_") {
            "config_invalid"
        } else {
            "request_failure"
        };
        return format!("class={class} | code={code}");
    }
    if message.contains("COMPLIANCE") {
        return "class=compliance_required | code=CR-VAL-0003".to_owned();
    }
    if message.contains("ROUTER_DEPLOY_") {
        return format!("class=config_invalid | {message}");
    }
    "class=request_failure".to_owned()
}

fn extract_router_error_code(text: &str) -> Option<String> {
    let start = text.find("CR-")?;
    let rest = &text[start..];
    let end = rest
        .find(|character: char| {
            !(character.is_ascii_uppercase() || character.is_ascii_digit() || character == '-')
        })
        .unwrap_or(rest.len());
    let code = &rest[..end];
    if code.len() >= 10 && code.bytes().filter(|byte| *byte == b'-').count() == 2 {
        Some(code.to_owned())
    } else {
        None
    }
}

fn oauth_accounts_failure_is_retryable(summary: &str) -> bool {
    [
        "class=connection_refused",
        "class=connection_closed",
        "class=request_failure",
        "class=timeout",
        "class=lifecycle_busy",
        "class=lifecycle_deferred",
        "class=process_failure",
        "class=empty_response",
        "class=authentication",
        "class=admin_session",
        "class=health_http",
        "class=config_invalid",
        "router_oauth_accounts_unavailable",
        "admin session",
        "health check failed",
        "actively refused",
        "connection refused",
        "no access token",
        "rate-limited",
        "429",
        "503",
        "install_root",
    ]
    .iter()
    .any(|needle| summary.to_ascii_lowercase().contains(needle))
}

pub fn load_usage_snapshot(
    router_root: &Path,
    profile_name: &str,
    cfg: &RouterConfig,
) -> anyhow::Result<crate::UsageSnapshot> {
    load_usage_snapshot_with_timeout(router_root, profile_name, cfg, Duration::from_secs(120))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthRecoveryResult {
    pub next_check_seconds: u64,
    pub summary: String,
}

pub fn probe_oauth_recovery(
    router_root: &Path,
    cfg: &RouterConfig,
    cancel: &AtomicBool,
) -> anyhow::Result<OAuthRecoveryResult> {
    if cancel.load(std::sync::atomic::Ordering::Acquire) {
        bail!("class=cancelled")
    }
    let mut selected_ids = cfg.oauth_account_ids.clone().unwrap_or_default();
    selected_ids.extend(
        cfg.models
            .iter()
            .filter(|model| model.source == "oauth" && model.oauth_account_id > 0)
            .map(|model| model.oauth_account_id),
    );
    selected_ids.sort_unstable();
    selected_ids.dedup();
    if selected_ids.is_empty() {
        return Ok(OAuthRecoveryResult {
            next_check_seconds: 0,
            summary: "healthy=0 deferred=0 recovered=0".to_owned(),
        });
    }

    let admin = usage::retry_admin_read(|| usage::AdminClient::connect(router_root, cfg))?;
    let accounts_body = usage::data(usage::retry_account_read(|| {
        admin.get(
            "/api/v1/admin/accounts?page=1&page_size=200",
            Duration::from_secs(10),
        )
    })?);
    let accounts = usage::get(&accounts_body, "items").unwrap_or(&accounts_body);
    let disabled_before = usage::array(accounts)
        .iter()
        .filter(|account| {
            let id = usage::integer(account, "id");
            selected_ids.contains(&id)
                && usage::string(account, "type") == "oauth"
                && (usage::string(account, "status") == "error"
                    || usage::get(account, "schedulable").and_then(Value::as_bool) == Some(false))
        })
        .map(|account| usage::integer(account, "id"))
        .collect::<HashSet<_>>();

    let snapshot = usage::query_usage(
        router_root,
        "OAuth recovery",
        cfg,
        Instant::now() + Duration::from_secs(120),
    )?;
    if cancel.load(std::sync::atomic::Ordering::Acquire) {
        bail!("class=cancelled")
    }
    let recovered = snapshot
        .subscriptions
        .iter()
        .filter(|account| disabled_before.contains(&account.id) && account.status == "active")
        .count();
    let healthy = snapshot
        .subscriptions
        .iter()
        .filter(|account| account.health == "healthy")
        .count();
    let deferred = snapshot
        .subscriptions
        .len()
        .saturating_sub(healthy)
        .saturating_sub(recovered);
    if snapshot.routing_changed {
        deployment::sync_routing_only(router_root, cfg)?;
    }
    Ok(OAuthRecoveryResult {
        next_check_seconds: usage::next_oauth_recovery_seconds(router_root, chrono::Utc::now()),
        summary: format!("healthy={healthy} deferred={deferred} recovered={recovered}"),
    })
}

fn load_usage_snapshot_with_timeout(
    router_root: &Path,
    profile_name: &str,
    cfg: &RouterConfig,
    timeout: Duration,
) -> anyhow::Result<crate::UsageSnapshot> {
    let mut last_failure = "class=unclassified_error".to_owned();
    for attempt in 0..USAGE_MONITOR_MAX_ATTEMPTS {
        match usage::query_usage(router_root, profile_name, cfg, Instant::now() + timeout) {
            Ok(snapshot) => return Ok(snapshot),
            Err(error) => last_failure = error.to_string(),
        }
        if attempt + 1 < USAGE_MONITOR_MAX_ATTEMPTS
            && usage_failure_is_locally_retryable(&last_failure)
        {
            std::thread::sleep(usage_monitor_retry_delay(attempt));
        } else {
            break;
        }
    }
    bail!(last_failure)
}

const USAGE_MONITOR_MAX_ATTEMPTS: usize = 5;

fn usage_monitor_retry_delay(attempt: usize) -> Duration {
    match attempt {
        0 => Duration::from_millis(500),
        1 => Duration::from_millis(1_000),
        2 => Duration::from_millis(2_000),
        _ => Duration::from_millis(3_500),
    }
}

fn usage_failure_is_locally_retryable(summary: &str) -> bool {
    let lower = summary.to_ascii_lowercase();
    if ["class=configuration", "class=permission", "class=storage"]
        .iter()
        .any(|class| lower.contains(class))
    {
        return false;
    }
    [
        "class=connection_refused",
        "class=connection_closed",
        "class=request_failure",
        "class=process_failure",
        "class=empty_response",
        "class=authentication",
        "class=admin_session",
        "class=timeout",
        "class=rate_limit",
        "class=lifecycle_busy",
        "class=lifecycle_deferred",
        "class=network",
        "class=dns",
        "class=proxy",
        "class=tls",
        "class=upstream",
        "class=invalid_response",
        "class=unclassified_error",
    ]
    .iter()
    .any(|class| lower.contains(class))
}

pub fn import_grok_sso(router_root: &Path, authorization: &str) -> anyhow::Result<String> {
    let tokens = authorization
        .lines()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        bail!("Grok authorization code / SSO token is empty");
    }
    let config = RouterConfig::load(&crate::user_data::config_path(router_root))
        .context("Grok 授权码导入失败: class=configuration")?;
    let admin = usage::retry_admin_read(|| usage::AdminClient::connect(router_root, &config))
        .context("Grok 授权码导入失败: class=authentication")?;
    let groups = usage::data(usage::retry_account_read(|| {
        admin.get(
            "/api/v1/admin/groups/all?include_inactive=true",
            Duration::from_secs(10),
        )
    })?);
    let group_id = usage::array(&groups)
        .iter()
        .find(|group| usage::string(group, "name") == "Codex-Router")
        .map(|group| usage::integer(group, "id"))
        .filter(|id| *id > 0)
        .context("Grok 授权码导入失败: class=configuration")?;
    let priority = oauth_routing_priorities(Some(&config.oauth_fallback)).oauth_priority;
    let body = json!({
        "sso_tokens": tokens,
        "name": "Grok OAuth",
        "notes": "Imported by Codex-Router using an authorization code / SSO token.",
        "group_ids": [group_id],
        "credentials": {},
        "concurrency": 3,
        "priority": priority,
        "rate_multiplier": 1,
        "auto_pause_on_expired": false,
    });
    let timeout = Duration::from_secs((90 + 30 * tokens.len() as u64).min(300));
    // Conversion creates accounts, so an ambiguous transport failure must not
    // be retried automatically: the server may already have accepted it.
    let response = usage::data(
        admin
            .post("/api/v1/admin/grok/sso-to-oauth", Some(&body), timeout)
            .context("Grok 授权码导入失败")?,
    );
    let created = usage::get(&response, "created")
        .map(usage::array)
        .unwrap_or_default();
    let failed = usage::get(&response, "failed")
        .map(usage::array)
        .unwrap_or_default();
    for account in created {
        let account_id = usage::integer(account, "id");
        if account_id > 0 {
            let _ = ensure_scheduled_oauth_recovery(&admin, account_id, "grok-4.5");
        }
    }
    if created.is_empty() {
        let summary = failed
            .first()
            .map(|failure| usage::string(failure, "error"))
            .filter(|message| !message.trim().is_empty())
            .map(|message| crate::runtime_logs::summarize_error_for_display(&message))
            .unwrap_or_else(|| "class=upstream".to_owned());
        bail!("Grok 授权码导入失败: {summary}")
    }
    Ok(format!(
        "Grok authorization imported: created={} failed={}",
        created.len(),
        failed.len()
    ))
}

fn ensure_scheduled_oauth_recovery(
    admin: &usage::AdminClient,
    account_id: i64,
    model_id: &str,
) -> anyhow::Result<()> {
    let path = format!("/api/v1/admin/accounts/{account_id}/scheduled-test-plans");
    let plans = usage::data(usage::retry_account_read(|| {
        admin.get(&path, Duration::from_secs(10))
    })?);
    let hourly_plan_id = usage::array(&plans)
        .iter()
        .find(|plan| usage::string(plan, "cron_expression") == "0 * * * *")
        .map(|plan| usage::integer(plan, "id"))
        .unwrap_or_default();
    let body = json!({
        "account_id": account_id,
        "model_id": model_id,
        "cron_expression": "0 * * * *",
        "enabled": true,
        "max_results": 48,
        "auto_recover": true,
    });
    if hourly_plan_id > 0 {
        admin.put(
            &format!("/api/v1/admin/scheduled-test-plans/{hourly_plan_id}"),
            &body,
            Duration::from_secs(10),
        )?;
    } else {
        admin.post(
            "/api/v1/admin/scheduled-test-plans",
            Some(&body),
            Duration::from_secs(10),
        )?;
    }
    Ok(())
}

pub fn set_oauth_account_priority(
    router_root: &Path,
    account_id: i64,
    priority: i32,
) -> anyhow::Result<i32> {
    if account_id <= 0 {
        bail!("OAuth 账号 ID 无效");
    }
    if !(1..=999).contains(&priority) {
        bail!("OAuth 优先级必须在 1 到 999 之间");
    }
    usage::set_oauth_account_priority(router_root, account_id, priority)
        .context("无法更新 OAuth 优先级")
}

pub fn revoke_oauth_account(router_root: &Path, account_id: i64) -> anyhow::Result<()> {
    if account_id <= 0 {
        bail!("OAuth 账号 ID 无效");
    }
    let config = RouterConfig::load(&crate::user_data::config_path(router_root))
        .context("无法撤销 OAuth 账号: class=configuration")?;
    let admin = usage::retry_admin_read(|| usage::AdminClient::connect(router_root, &config))
        .context("无法撤销 OAuth 账号: class=authentication")?;
    let path = format!("/api/v1/admin/accounts/{account_id}");
    let account = usage::data(usage::retry_account_read(|| {
        admin.get(&path, Duration::from_secs(10))
    })?);
    if usage::string(&account, "type") != "oauth" {
        bail!("无法撤销 OAuth 账号: class=configuration")
    }
    admin
        .delete(&path, Duration::from_secs(15))
        .context("无法撤销 OAuth 账号")?;
    Ok(())
}

pub fn remove_oauth_account_references(cfg: &mut RouterConfig, account_id: i64) -> bool {
    let selected_before = cfg.oauth_account_ids.clone();
    let seen_before = cfg.oauth_seen_account_ids.clone();
    if let Some(ids) = cfg.oauth_account_ids.as_mut() {
        ids.retain(|id| *id != account_id);
    }
    cfg.oauth_seen_account_ids.retain(|id| *id != account_id);
    let model_count = cfg.models.len();
    cfg.models
        .retain(|model| !(model.source == "oauth" && model.oauth_account_id == account_id));
    let changed = selected_before != cfg.oauth_account_ids
        || seen_before != cfg.oauth_seen_account_ids
        || model_count != cfg.models.len();
    if changed {
        normalize_default_model(cfg);
    }
    changed
}

pub fn remove_oauth_model_reference(
    cfg: &mut RouterConfig,
    account_id: i64,
    model_id: &str,
) -> bool {
    let canonical = canonical_route_model_id(model_id);
    let before = cfg.models.len();
    cfg.models.retain(|model| {
        !(model.source == "oauth"
            && model.oauth_account_id == account_id
            && same_model_identity(&model.model, model_id))
    });
    if cfg.models.len() == before {
        return false;
    }
    let still_imported = cfg.models.iter().any(|model| {
        model.source == "oauth" && canonical_route_model_id(&model.model) == canonical
    });
    if !still_imported {
        cfg.fallback_channel_selections.remove(&canonical);
    }
    normalize_default_model(cfg);
    true
}

pub fn enroll_unseen_oauth_accounts(
    selected: &mut Vec<i64>,
    seen: &mut Vec<i64>,
    bound_account_ids: &[i64],
) -> usize {
    let mut added = 0;
    for account_id in bound_account_ids {
        if seen.contains(account_id) {
            continue;
        }
        seen.push(*account_id);
        if !selected.contains(account_id) {
            selected.push(*account_id);
            added += 1;
        }
    }
    selected.sort_unstable();
    selected.dedup();
    seen.sort_unstable();
    seen.dedup();
    added
}

/// Codex Desktop owns `[windows].sandbox`.
///
/// - `elevated` means the one-time elevated Windows setup finished.
/// - Stripping that marker reopens “Windows 安装未完成” after every login.
/// - Router must never invent, force, or delete this key during Apply/restore/exit.
pub fn normalize_windows_sandbox_config(text: &str) -> String {
    text.to_owned()
}

fn windows_sandbox_value(text: &str) -> Option<String> {
    let mut in_windows = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_windows = trimmed == "[windows]";
            continue;
        }
        if !in_windows {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        if key.trim() == "sandbox" {
            let value = value.trim().trim_matches('"').trim();
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

/// Keep a completed Windows setup marker from the live config when a restore
/// snapshot or sanitized fallback does not carry one.
pub fn preserve_windows_sandbox_config(current: &str, next: &str) -> String {
    let current_value = windows_sandbox_value(current);
    let next_value = windows_sandbox_value(next);
    let preferred = match (current_value.as_deref(), next_value.as_deref()) {
        (Some("elevated"), _) | (_, Some("elevated")) => Some("elevated"),
        (Some(value), None) => Some(value),
        _ => None,
    };
    let Some(value) = preferred else {
        return next.to_owned();
    };
    if next_value.as_deref() == Some(value) {
        return next.to_owned();
    }

    let newline = if next.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines = Vec::new();
    let mut in_windows = false;
    let mut wrote = false;
    let mut saw_windows = false;
    for line in next.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_windows && !wrote {
                lines.push(format!("sandbox = \"{value}\""));
                wrote = true;
            }
            in_windows = trimmed == "[windows]";
            if in_windows {
                saw_windows = true;
            }
            lines.push(line.to_owned());
            continue;
        }
        if in_windows {
            let key = trimmed.split('=').next().unwrap_or_default().trim();
            if key == "sandbox" {
                if !wrote {
                    lines.push(format!("sandbox = \"{value}\""));
                    wrote = true;
                }
                continue;
            }
        }
        lines.push(line.to_owned());
    }
    if in_windows && !wrote {
        lines.push(format!("sandbox = \"{value}\""));
        wrote = true;
    }
    if !saw_windows {
        if !lines.is_empty() && !lines.last().map(|line| line.is_empty()).unwrap_or(true) {
            lines.push(String::new());
        }
        lines.push("[windows]".to_owned());
        lines.push(format!("sandbox = \"{value}\""));
        wrote = true;
    }
    let mut result = lines.join(newline);
    if (next.ends_with('\n') || next.ends_with('\r') || wrote) && !result.ends_with(newline) {
        result.push_str(newline);
    }
    result
}

pub fn resolve_codex_home(cfg: &RouterConfig) -> PathBuf {
    if !cfg.deploy.codex_home.trim().is_empty() {
        return Path::new(&cfg.deploy.codex_home).to_path_buf();
    }
    if let Some(value) = std::env::var_os("CODEX_HOME") {
        if !value.is_empty() {
            return PathBuf::from(value);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| Path::new(".").into())
        .join(".codex")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_priorities_match_subscription_first_api_first_and_split_modes() {
        let mut fallback = crate::config::OAuthFallback {
            enabled: true,
            prefer_oauth: true,
            official_priority: 1,
            fallback_priority: 100,
        };
        assert_eq!(
            oauth_routing_priorities(Some(&fallback)),
            OAuthRoutingPriorities {
                enabled: true,
                prefer_oauth: true,
                oauth_priority: 1,
                api_priority: 100,
            }
        );
        fallback.prefer_oauth = false;
        assert_eq!(
            oauth_routing_priorities(Some(&fallback)),
            OAuthRoutingPriorities {
                enabled: true,
                prefer_oauth: false,
                oauth_priority: 100,
                api_priority: 1,
            }
        );
        fallback.enabled = false;
        assert_eq!(
            oauth_routing_priorities(Some(&fallback)),
            OAuthRoutingPriorities {
                enabled: false,
                prefer_oauth: true,
                oauth_priority: 1,
                api_priority: 10,
            }
        );
    }

    #[test]
    fn channel_route_profiles_keep_source_and_protocol_separate() {
        let grok = ModelConfig {
            model: "grok-4.6".into(),
            source: "oauth".into(),
            oauth_platform: "grok".into(),
            base_url: "Router OAuth / grok".into(),
            ..Default::default()
        };
        let grok_profile = classify_channel_route(&grok);
        assert_eq!(grok_profile.vendor, "x-ai");
        assert_eq!(grok_profile.source_type, ChannelSourceType::Subscription);
        assert_eq!(grok_profile.gateway, ChannelGateway::Sub2Api);
        assert_eq!(grok_profile.upstream_protocol, UpstreamProtocol::Responses);
        assert!(!grok_profile.allow_fallback);

        let ark = ModelConfig {
            model: "deepseek-v4-flash".into(),
            base_url: "https://ark.cn-beijing.volces.com/api/coding/v3".into(),
            ..Default::default()
        };
        let ark_profile = classify_channel_route(&ark);
        assert_eq!(ark_profile.vendor, "volcengine");
        assert_eq!(ark_profile.source_type, ChannelSourceType::CodingPlan);
        assert_eq!(ark_profile.upstream_protocol, UpstreamProtocol::Responses);
        assert!(!ark_profile.allow_fallback);

        let ark_payg = ModelConfig {
            model: "deepseek-v4-flash".into(),
            base_url: "https://ark.cn-beijing.volces.com/api/v3".into(),
            ..Default::default()
        };
        assert_eq!(
            classify_channel_route(&ark_payg).source_type,
            ChannelSourceType::OfficialApi
        );
        assert!(is_same_vendor_payg_fallback(&ark, &ark_payg));

        let kimi = ModelConfig {
            model: "kimi-for-coding".into(),
            base_url: "https://api.kimi.com/coding/v1".into(),
            ..Default::default()
        };
        let kimi_profile = classify_channel_route(&kimi);
        assert_eq!(kimi_profile.source_type, ChannelSourceType::CodingPlan);
        assert_eq!(
            kimi_profile.upstream_protocol,
            UpstreamProtocol::ChatCompletions
        );
        assert!(!kimi_profile.allow_fallback);

        let chiral = ModelConfig {
            model: "gpt-5.6-sol".into(),
            base_url: "https://api.430123.xyz/v1".into(),
            ..Default::default()
        };
        let chiral_profile = classify_channel_route(&chiral);
        assert_eq!(chiral_profile.vendor, "chiral");
        assert_eq!(chiral_profile.source_type, ChannelSourceType::Relay);
        assert_eq!(chiral_profile.upstream_protocol, UpstreamProtocol::Responses);
        assert!(chiral_profile.allow_fallback);

        let chatgpt = ModelConfig {
            model: "gpt-5.6-sol".into(),
            source: "oauth".into(),
            oauth_platform: "openai".into(),
            base_url: "Router OAuth / openai".into(),
            ..Default::default()
        };
        let openai_payg = ModelConfig {
            model: "gpt-5.6-sol".into(),
            base_url: "https://api.openai.com/v1".into(),
            ..Default::default()
        };
        assert!(is_same_vendor_payg_fallback(&chatgpt, &openai_payg));
        assert!(!is_same_vendor_payg_fallback(&chatgpt, &chiral));
    }

    #[test]
    fn effective_api_priority_preserves_configured_channel_order() {
        assert_eq!(effective_api_priority(10, 10, 100, 1, true), 100);
        assert_eq!(effective_api_priority(20, 10, 100, 1, true), 110);
        assert_eq!(effective_api_priority(10, 10, 1, 100, false), 1);
        assert_eq!(effective_api_priority(20, 10, 1, 100, false), 11);
    }

    #[test]
    fn router_mode_requires_the_selected_provider_and_matching_local_url() {
        let config = r#"model_provider = "codex_router"
model = "gpt-5.6-sol"
model_catalog_json = "C:/Users/test/.codex-router/model-catalog.json"

[model_providers.codex_router]
name = "Codex-Router"
base_url = "http://127.0.0.1:18082/v1"
wire_api = "responses"
requires_openai_auth = true
experimental_bearer_token = "local-router-test-key"
"#;
        assert!(codex_config_uses_router(config, "http://127.0.0.1:18082"));
        assert!(!codex_config_uses_router(config, "http://127.0.0.1:19090"));
        assert!(!codex_config_uses_router(
            &config.replace(
                "model_provider = \"codex_router\"",
                "model_provider = \"openai\""
            ),
            "http://127.0.0.1:18082"
        ));

        let legacy_custom = r#"model_provider = "custom"
model = "gpt-5.6-sol"

[model_providers.custom]
name = "Codex-Router"
base_url = "http://127.0.0.1:18082/v1"
"#;
        assert!(
            !codex_config_uses_router(legacy_custom, "http://127.0.0.1:18082"),
            "an incomplete legacy provider must be migrated before it is considered healthy"
        );

        let chiral = r#"model_provider = "custom"
model = "grok-4.5"

[model_providers.custom]
name = "micu"
base_url = "https://api.430123.xyz/v1"
"#;
        assert!(!codex_config_uses_router(chiral, "http://127.0.0.1:18082"));

        let third_party = config.replace(
            "requires_openai_auth = true",
            "requires_openai_auth = false",
        );
        assert!(
            codex_config_uses_router(&third_party, "http://127.0.0.1:18082"),
            "third-party catalog models must keep a valid Router binding without ChatGPT allowlist"
        );

        let legacy = config
            .replace(
                "model_provider = \"codex_router\"",
                "model_provider = \"sub2api\"",
            )
            .replace("model_providers.codex_router", "model_providers.sub2api");
        assert!(codex_config_uses_router(&legacy, "http://127.0.0.1:18082"));
    }

    fn temporary_test_dir(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "codex-router-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn credential_storage_is_native_and_reports_only_actual_model_key_updates() {
        let root = temporary_test_dir("credential-update-count");
        let nonce = uuid::Uuid::now_v7().simple();
        let updated_credential = format!("Test-Store-Credential-{nonce}");
        let mut config = RouterConfig {
            models: vec![
                ModelConfig {
                    model: "existing-model".to_owned(),
                    credential_name: "ExistingCredential".to_owned(),
                    ..ModelConfig::default()
                },
                ModelConfig {
                    model: "updated-model".to_owned(),
                    api_key: "not-a-real-key".to_owned(),
                    credential_name: updated_credential.clone(),
                    ..ModelConfig::default()
                },
            ],
            ..RouterConfig::default()
        };

        let result = (|| -> anyhow::Result<()> {
            let updated = store_credentials(&mut config, &root)?;
            assert_eq!(updated, 1);
            assert!(config.models.iter().all(|model| model.api_key.is_empty()));
            let saved = read_router_credential(&updated_credential)?
                .context("native credential was not written")?;
            assert_eq!(saved.0, "not-a-real-key".encode_utf16().collect::<Vec<_>>());
            Ok(())
        })();
        let _ = delete_router_credential(&updated_credential);
        std::fs::remove_dir_all(root).unwrap();
        result.unwrap();
    }

    #[test]
    fn volcengine_coding_and_agent_plans_share_control_plane_credentials() {
        assert!(is_volcengine_plan_url(
            "https://ark.cn-beijing.volces.com/api/coding/v3"
        ));
        assert!(is_volcengine_plan_url(
            "https://ark.cn-beijing.volces.com/api/plan/v3"
        ));
        assert!(!is_volcengine_plan_url(
            "https://ark.cn-beijing.volces.com/api/v3"
        ));
    }

    #[test]
    fn configured_router_mode_does_not_depend_on_backend_health() {
        let root = temporary_test_dir("configured-router-mode");
        let mut cfg = RouterConfig::default();
        cfg.deploy.codex_home = root.to_string_lossy().into_owned();
        cfg.deploy.sub2api_host = "http://127.0.0.1:1".into();
        std::fs::write(
            root.join("config.toml"),
            "model_provider = \"codex_router\"\n\
             model_catalog_json = \"C:/Users/test/.codex-router/model-catalog.json\"\n\
             [model_providers.codex_router]\n\
             name = \"Codex-Router\"\n\
             base_url = \"http://127.0.0.1:1/v1\"\n\
             wire_api = \"responses\"\n\
             requires_openai_auth = true\n\
             experimental_bearer_token = \"local-router-test-key\"\n",
        )
        .unwrap();

        assert!(codex_router_mode_configured(&cfg));
        assert!(!codex_router_mode_active(&cfg));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn channel_presets_fill_recommended_fields_without_overwriting_the_key() {
        let mut model = ModelConfig {
            api_key: "keep-secret".into(),
            credential_name: "SavedCredential".into(),
            priority: 40,
            extra: r#"{"old":true}"#.into(),
            ..Default::default()
        };
        assert!(apply_channel_preset(&mut model, "chiral"));
        assert_eq!(model.base_url, "https://api.430123.xyz/v1");
        assert_eq!(model.model, "gpt-5.6-sol");
        assert_eq!(model.alias, "ChatGPT-5.6-Sol");
        assert_eq!(model.alias_customized, Some(false));
        assert_eq!(model.api_key, "keep-secret");
        assert_eq!(model.credential_name, "SavedCredential");
        assert_eq!(model.priority, 40);
        assert_eq!(model.context_window, 0);
        assert_eq!(model.auto_compact_percent, 80);
        assert_eq!(model.reasoning_mode, "auto");
        assert_eq!(model.extra, "{}");
        assert!(!apply_channel_preset(&mut model, "unknown"));
    }

    #[test]
    fn common_model_ids_receive_editable_recommended_display_names() {
        for (model_id, expected) in [
            ("gpt-5.6-sol", "ChatGPT-5.6-Sol"),
            ("openai/gpt-5.6-sol-fast", "ChatGPT-5.6-Sol-Fast"),
            ("gpt-5.4-mini", "ChatGPT-5.4-Mini"),
            ("gpt-5.3-codex-high", "ChatGPT-5.3-Codex-High"),
            ("anthropic/claude-opus-5-fast", "Claude-Opus-5-Fast"),
            ("claude-sonnet-4-6-20260501", "Claude-Sonnet-4.6"),
            ("google/gemini-3-1-pro", "Gemini-3.1-Pro"),
            ("gemini-3-pro-image-preview", "Gemini-3-Pro-Image-Preview"),
            ("deepseek/deepseek-v4-pro", "DeepSeek-V4-Pro"),
            ("deepseek/deepseek-v4-flash", "DeepSeek-V4-Flash"),
            ("deepseek/deepseek-v3.2", "DeepSeek-V3.2"),
            ("x-ai/grok-4.5", "Grok-4.5"),
            ("cursor-composer-2.5", "Composer-2.5"),
            ("z-ai/glm-5.2", "GLM-5.2"),
        ] {
            assert_eq!(recommended_model_display_name(model_id), expected);
        }
        assert_eq!(
            recommended_model_display_name("vendor/custom_model"),
            "custom-model"
        );
    }

    #[test]
    fn oauth_display_name_stays_unified_without_overwriting_user_aliases() {
        let oauth = ModelConfig {
            model: "gpt-5.6-sol".into(),
            alias: "GPT-5.6 Sol".into(),
            alias_customized: Some(false),
            source: "oauth".into(),
            oauth_account_id: 42,
            ..Default::default()
        };
        let fallback = ModelConfig {
            model: "openai/gpt-5.6-sol".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            ..Default::default()
        };
        let mut config = RouterConfig {
            models: vec![oauth.clone()],
            oauth_account_ids: Some(vec![42]),
            ..Default::default()
        };
        assert_eq!(
            resolved_model_display_name(&config, &oauth),
            "ChatGPT-5.6-Sol"
        );
        config.models.push(fallback);
        assert_eq!(
            resolved_model_display_name(&config, &oauth),
            "ChatGPT-5.6-Sol"
        );
        config.oauth_fallback.enabled = false;
        assert_eq!(
            resolved_model_display_name(&config, &oauth),
            "ChatGPT-5.6-Sol"
        );
        let custom = ModelConfig {
            alias: "My paid account".into(),
            alias_customized: Some(true),
            ..oauth
        };
        assert_eq!(
            resolved_model_display_name(&config, &custom),
            "My paid account"
        );
    }

    #[test]
    fn channel_presets_keep_current_supported_model_defaults() {
        let expected = [
            ("chiral", "gpt-5.6-sol"),
            ("openai", "gpt-5.6-sol"),
            ("anthropic", "claude-opus-5"),
            ("openrouter", "openai/gpt-5.6-sol"),
            ("kimi-open", "kimi-k3"),
            ("kimi", "kimi-for-coding"),
            ("ark-coding", "ark-code-latest"),
            ("ark-plan", "ark-code-latest"),
            ("mimo", "mimo-v2.5-pro"),
            ("deepseek", "deepseek-v4-pro"),
            ("gemini", "gemini-3.6-flash"),
        ];

        for (id, model) in expected {
            let preset = channel_presets()
                .iter()
                .find(|preset| preset.id == id)
                .unwrap_or_else(|| panic!("missing channel preset: {id}"));
            assert_eq!(preset.model, model, "stale default model for {id}");
        }
    }

    #[test]
    fn ark_coding_plan_presets_are_quick_selectable_and_ranked_as_coding_plan() {
        let common = common_channel_presets()
            .map(|preset| preset.id)
            .collect::<Vec<_>>();
        assert!(common.contains(&"ark-coding"));
        assert!(common.contains(&"ark-plan"));

        for (id, expected_url) in [
            (
                "ark-coding",
                "https://ark.cn-beijing.volces.com/api/coding/v3",
            ),
            ("ark-plan", "https://ark.cn-beijing.volces.com/api/plan/v3"),
        ] {
            let preset = channel_presets()
                .iter()
                .find(|preset| preset.id == id)
                .unwrap_or_else(|| panic!("missing Ark preset: {id}"));
            assert_eq!(preset.base_url, expected_url);

            let mut model = ModelConfig::default();
            assert!(apply_channel_preset(&mut model, id));
            assert_eq!(model.model, "ark-code-latest");
            assert_eq!(model.base_url, expected_url);
            assert_eq!(classify_channel_route(&model).source_type, ChannelSourceType::CodingPlan);
        }

        assert_eq!(
            recommended_model_display_name("ark-code-latest"),
            "Ark-Code-Latest"
        );
        assert_eq!(detect_context_defaults("ark-code-latest").window, 262_144);
        let reasoning = detect_reasoning("ark-code-latest");
        assert_eq!(reasoning.default_level, "high");
        assert!(!reasoning.supports_fast);
    }

    #[test]
    fn chiral_is_exposed_only_through_recommended_platforms() {
        let common = common_channel_presets()
            .map(|preset| preset.id)
            .collect::<Vec<_>>();
        let recommended = recommended_channel_presets()
            .map(|preset| preset.id)
            .collect::<Vec<_>>();

        assert!(common.contains(&"openai"));
        assert!(common.contains(&"anthropic"));
        assert!(common.contains(&"openrouter"));
        assert!(!common.contains(&"chiral"));
        assert_eq!(recommended, vec!["chiral"]);
    }

    #[test]
    fn oauth_revoke_cleanup_removes_only_the_matching_account_references() {
        let mut config = RouterConfig {
            oauth_account_ids: Some(vec![3, 7, 8]),
            models: vec![
                ModelConfig {
                    model: "oauth-one".into(),
                    source: "oauth".into(),
                    oauth_account_id: 7,
                    ..Default::default()
                },
                ModelConfig {
                    model: "oauth-two".into(),
                    source: "oauth".into(),
                    oauth_account_id: 8,
                    ..Default::default()
                },
                ModelConfig {
                    model: "api-model".into(),
                    base_url: "https://example.invalid/v1".into(),
                    ..Default::default()
                },
            ],
            default_model: "oauth-one".into(),
            ..Default::default()
        };

        assert!(remove_oauth_account_references(&mut config, 7));
        assert_eq!(config.oauth_account_ids, Some(vec![3, 8]));
        assert_eq!(config.models.len(), 2);
        assert!(config
            .models
            .iter()
            .all(|model| model.oauth_account_id != 7));
        assert_ne!(config.default_model, "oauth-one");
        assert!(!remove_oauth_account_references(&mut config, 99));
    }

    #[test]
    fn newly_connected_oauth_accounts_are_enrolled_once_and_can_then_be_disabled() {
        let mut selected = vec![1];
        let mut seen = Vec::new();
        assert_eq!(
            enroll_unseen_oauth_accounts(&mut selected, &mut seen, &[1, 2, 3]),
            2
        );
        assert_eq!(selected, vec![1, 2, 3]);
        assert_eq!(seen, vec![1, 2, 3]);

        selected.retain(|account_id| *account_id != 2);
        assert_eq!(
            enroll_unseen_oauth_accounts(&mut selected, &mut seen, &[1, 2, 3]),
            0
        );
        assert_eq!(selected, vec![1, 3]);
    }

    #[test]
    fn oauth_account_request_failures_retry_and_trigger_one_router_repair() {
        let failure = "ROUTER_OAUTH_ACCOUNTS_UNAVAILABLE: class=request_failure";
        assert!(oauth_accounts_failure_is_retryable(failure));
        assert!(oauth_accounts_failure_needs_router_repair(failure));
    }

    #[test]
    fn first_run_without_saved_config_does_not_fail_as_configuration() {
        let root = temporary_test_dir("oauth-first-run-no-config");
        let config_path = crate::user_data::config_path(&root);
        assert!(!config_path.is_file());
        let loaded = RouterConfig::load(&config_path);
        assert!(loaded.is_err());
        let fallback = loaded.unwrap_or_default();
        assert_eq!(fallback.deploy.sub2api_host, "http://127.0.0.1:18080");
        assert!(!crate::user_data::config_looks_configured(&fallback));
        let _loader: fn(&Path) -> anyhow::Result<Vec<crate::OAuthAccountSummary>> =
            load_oauth_accounts;
        let _ = _loader;
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn oauth_duplicate_identity_prefers_selected_then_bound_then_lower_id() {
        let credentials = json!({ "chatgpt_account_id": "acct-123" });
        assert_eq!(
            oauth_stable_identity("openai", &credentials, &Value::Null, 9),
            "openai|account|acct-123"
        );

        let summary = |id, bound_to_router| crate::OAuthAccountSummary {
            id,
            name: String::new(),
            platform: "openai".to_owned(),
            status: String::new(),
            email: String::new(),
            plan: String::new(),
            priority: 0,
            bound_to_router,
            error: String::new(),
            expires_at: String::new(),
            models: Vec::new(),
            models_error: String::new(),
        };
        let selected = HashSet::from([9]);
        assert!(prefer_oauth_summary(
            &summary(3, true),
            &summary(9, false),
            &selected
        ));
        assert!(prefer_oauth_summary(
            &summary(9, false),
            &summary(4, true),
            &HashSet::new()
        ));
        assert!(prefer_oauth_summary(
            &summary(9, true),
            &summary(4, true),
            &HashSet::new()
        ));
    }

    #[test]
    fn oauth_model_catalog_only_exposes_live_account_declarations() {
        let models = discovered_oauth_models(
            "antigravity",
            &json!([
                {"id": "gemini-3.1-pro-high", "display_name": "Gemini 3.1 Pro High"},
                {"id": "claude-fable-5"},
                {"id": "gemini-3.1-pro-high", "display_name": "duplicate"},
                {"display_name": "missing id"}
            ]),
        );
        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["gemini-3.1-pro-high", "claude-fable-5"]
        );
        assert_eq!(models[1].display_name, "claude-fable-5");
        assert!(!models.iter().any(|model| model.id == "gemini-3-flash"));

        let openai = discovered_oauth_models(
            "openai",
            &json!([{"id": "gpt-5.6", "displayName": "GPT 5.6"}]),
        );
        assert_eq!(openai[0].id, "gpt-5.6-sol");
        assert_eq!(openai[0].display_name, "ChatGPT-5.6-Sol");

        let antigravity_flash = discovered_oauth_models(
            "antigravity",
            &json!([
                {"id": "gemini-3.6-flash-high", "display_name": "Gemini 3.6 Flash High"},
                {"id": "gemini-3.7-flash-high", "display_name": "Gemini 3.7 Flash High"},
                {"id": "gemini-3.7-flash-medium", "display_name": "Gemini 3.7 Flash Medium"},
                {"id": "gemini-3.7-flash-low", "display_name": "Gemini 3.7 Flash Low"}
            ]),
        );
        let gemini_37 = antigravity_flash
            .iter()
            .filter(|model| model.id == "gemini-3.7-flash")
            .collect::<Vec<_>>();
        assert_eq!(gemini_37.len(), 1);
        assert_eq!(gemini_37[0].display_name, "Gemini 3.7 Flash");
        assert!(
            !discovered_oauth_models("antigravity", &json!([{"id": "gemini-3.6-flash-low"}]))
                .iter()
                .any(|model| model.id == "gemini-3.7-flash")
        );
    }

    #[test]
    fn oauth_model_catalog_accepts_sync_upstream_string_ids() {
        let models = discovered_oauth_models("grok", &json!(["grok-4.5", "grok-4.6"]));

        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["grok-4.5", "grok-4.6"]
        );
        assert_eq!(models[1].display_name, "grok-4.6");
    }

    #[test]
    fn oauth_model_catalog_parses_live_sub2api_response_wrappers() {
        let antigravity = oauth_models_from_response(
            "antigravity",
            json!({
                "code": 0,
                "message": "success",
                "data": [
                    {"id": "gemini-3.7-flash-medium", "display_name": "Gemini 3.7 Flash Medium"},
                    {"id": "gemini-3.1-pro-high", "display_name": "Gemini 3.1 Pro High"}
                ]
            }),
        );
        assert_eq!(
            antigravity
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["gemini-3.7-flash", "gemini-3.1-pro-high"]
        );

        let grok = oauth_models_from_response(
            "grok",
            json!({
                "code": 0,
                "message": "success",
                "data": {"models": ["grok-4.5", "grok-4.6"]}
            }),
        );
        assert_eq!(
            grok.iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["grok-4.5", "grok-4.6"]
        );

        let chatgpt = oauth_models_from_response(
            "openai",
            json!({
                "code": 0,
                "message": "success",
                "data": [{"id": "gpt-5.6", "displayName": "GPT 5.6"}]
            }),
        );
        assert_eq!(chatgpt.len(), 1);
        assert_eq!(chatgpt[0].id, "gpt-5.6-sol");
        assert_eq!(chatgpt[0].display_name, "ChatGPT-5.6-Sol");
    }

    #[test]
    fn oauth_model_refresh_posts_to_the_live_upstream_catalog() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]).into_owned();
            let body = r#"{"data":{"models":[{"id":"grok-4.6","display_name":"Grok 4.6"}]}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            request
        });
        let admin = usage::AdminClient::for_test(format!("http://{address}"));

        let models = load_live_oauth_models(&admin, 24, "grok").unwrap();
        let request = server.join().unwrap();

        assert!(request.starts_with("POST /api/v1/admin/accounts/24/models/sync-upstream HTTP/1.1"));
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "grok-4.6");
    }

    #[test]
    fn oauth_model_refresh_falls_back_to_stable_catalog_when_sync_is_unsupported() {
        use std::io::{ErrorKind, Read, Write};
        use std::net::TcpListener;
        use std::time::{Duration, Instant};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(10);
            while requests.len() < 2 && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0_u8; 4096];
                        let read = match stream.read(&mut request) {
                            Ok(read) if read > 0 => read,
                            // The admin client may abort a connection before
                            // sending (or reset a keep-alive probe); skip it.
                            _ => continue,
                        };
                        let request = String::from_utf8_lossy(&request[..read]).into_owned();
                        let first_line = request.lines().next().unwrap_or_default().to_owned();
                        let (status, body) = if first_line.starts_with("POST ") {
                            ("404 Not Found", r#"{"error":"sync unsupported"}"#)
                        } else {
                            (
                                "200 OK",
                                r#"{"data":[{"id":"gpt-5.6-sol","display_name":"ChatGPT-5.6-Sol"}]}"#,
                            )
                        };
                        write!(
                            stream,
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .unwrap();
                        requests.push(first_line);
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("mock OAuth catalog server failed: {error}"),
                }
            }
            requests
        });
        let admin = usage::AdminClient::for_test(format!("http://{address}"));

        let result = load_live_oauth_models(&admin, 31, "openai");
        let requests = server.join().unwrap();

        assert_eq!(requests.len(), 2);
        assert!(
            requests[0].starts_with("POST /api/v1/admin/accounts/31/models/sync-upstream HTTP/1.1")
        );
        assert!(requests[1].starts_with("GET /api/v1/admin/accounts/31/models HTTP/1.1"));
        let models = result.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-5.6-sol");
    }

    #[test]
    fn oauth_model_refresh_failure_is_preserved_instead_of_becoming_an_empty_catalog() {
        let (models, error) =
            oauth_model_catalog_result(Err(anyhow::anyhow!("class=request_failure")));

        assert!(models.is_empty());
        assert_eq!(error, "class=request_failure");
    }

    #[test]
    fn oauth_model_removal_is_account_scoped_and_cleans_fallback_only_when_last_copy_is_gone() {
        let oauth_model = |account_id| ModelConfig {
            model: "gemini-3.1-pro-high".into(),
            source: "oauth".into(),
            oauth_account_id: account_id,
            oauth_platform: "antigravity".into(),
            ..Default::default()
        };
        let mut config = RouterConfig {
            models: vec![
                oauth_model(4),
                oauth_model(26),
                ModelConfig {
                    model: "google/gemini-3.1-pro-high".into(),
                    base_url: "https://example.invalid/v1".into(),
                    ..Default::default()
                },
            ],
            default_model: "gemini-3.1-pro-high".into(),
            ..Default::default()
        };
        config.fallback_channel_selections.insert(
            "gemini-3.1-pro-high".into(),
            vec!["gemini-3.1-pro-high|https://example.invalid/v1".into()],
        );

        assert!(remove_oauth_model_reference(
            &mut config,
            4,
            "gemini-3.1-pro-high"
        ));
        assert!(config
            .models
            .iter()
            .any(|model| model.source == "oauth" && model.oauth_account_id == 26));
        assert!(config
            .fallback_channel_selections
            .contains_key("gemini-3.1-pro-high"));
        assert!(!remove_oauth_model_reference(
            &mut config,
            4,
            "gemini-3.1-pro-high"
        ));

        assert!(remove_oauth_model_reference(
            &mut config,
            26,
            "gemini-3.1-pro-high"
        ));
        assert!(config.models.iter().all(|model| model.source != "oauth"));
        assert!(!config
            .fallback_channel_selections
            .contains_key("gemini-3.1-pro-high"));
    }

    #[test]
    fn multimodal_can_be_auto_detected_or_overridden() {
        let mut model = ModelConfig {
            model: "kimi-k3".into(),
            ..Default::default()
        };
        assert!(resolve_multimodal(&model));
        model.multimodal = "false".into();
        assert!(!resolve_multimodal(&model));
        model.model = "custom-vision-model".into();
        model.multimodal = "true".into();
        assert!(resolve_multimodal(&model));
        model.model = "unknown-model".into();
        model.multimodal = "auto".into();
        assert!(!resolve_multimodal(&model));
    }

    #[test]
    fn text_only_and_vision_model_families_are_detected_conservatively() {
        for model in [
            "deepseek/deepseek-v4-flash-0731",
            "deepseek-reasoner",
            "z-ai/glm-4.5",
            "glm-4.6",
            "qwen3-coder",
            "mimo-v2.5-pro",
            "ark-code-latest",
            "doubao-seed-code",
            "unknown-model",
        ] {
            assert!(
                !detect_multimodal_defaults(model).supported,
                "{model} must default to text-only"
            );
        }
        for model in [
            "z-ai/glm-4.5v",
            "glm-4.6v",
            "qwen2.5-vl-72b",
            "x-ai/grok-4.5",
            "gpt-5.6-sol",
            "claude-opus-5",
            "gemini-3.6-flash",
            "kimi-k3",
            "k3-256k",
        ] {
            assert!(
                detect_multimodal_defaults(model).supported,
                "{model} must default to multimodal"
            );
        }
    }

    #[test]
    fn default_model_uses_explicit_selection_and_safe_fallbacks() {
        let mut config = RouterConfig::default();
        config.models.push(ModelConfig {
            model: "first".into(),
            ..Default::default()
        });
        config.models.push(ModelConfig {
            model: "second".into(),
            ..Default::default()
        });

        assert_eq!(resolve_default_model(&config), Some("first".to_owned()));
        normalize_default_model(&mut config);
        assert_eq!(config.default_model, "first");

        config.default_model = "second".into();
        assert_eq!(resolve_default_model(&config), Some("second".to_owned()));

        config.models.remove(1);
        normalize_default_model(&mut config);
        assert_eq!(config.default_model, "first");
    }

    #[test]
    fn split_mode_default_api_model_uses_its_public_route_id() {
        let mut config = RouterConfig {
            oauth_account_ids: Some(vec![7]),
            oauth_fallback: crate::config::OAuthFallback {
                enabled: false,
                ..Default::default()
            },
            models: vec![
                ModelConfig {
                    source: "oauth".into(),
                    model: "gpt-5.6-sol".into(),
                    oauth_account_id: 7,
                    ..Default::default()
                },
                ModelConfig {
                    source: "apikey".into(),
                    model: "gpt-5.6-sol".into(),
                    base_url: "https://api.example.test/v1".into(),
                    credential_name: "test-credential".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let api_route = catalog::build_route_plan(&config)
            .into_iter()
            .find(|route| route.source == "apikey")
            .unwrap();
        assert!(api_route.public_model_id.contains("--api-"));

        config.default_model = api_route.public_model_id.clone();
        normalize_default_model(&mut config);

        assert_eq!(config.default_model, api_route.public_model_id);
        assert_eq!(resolve_default_route(&config).unwrap().source, "apikey");
    }

    #[test]
    fn api_channels_become_fallbacks_only_after_matching_oauth_import() {
        let mut config = RouterConfig {
            oauth_account_ids: Some(vec![7]),
            ..Default::default()
        };
        let grok_api = ModelConfig {
            model: "x-ai/grok-4.5".into(),
            ..Default::default()
        };
        let unrelated_api = ModelConfig {
            model: "deepseek/deepseek-v4-flash-0731".into(),
            ..Default::default()
        };
        config.models = vec![grok_api.clone(), unrelated_api.clone()];
        assert!(!is_oauth_fallback_model(&config, &grok_api));
        assert!(!is_oauth_fallback_model(&config, &unrelated_api));

        config.models.push(ModelConfig {
            model: "grok-4.5".into(),
            source: "oauth".into(),
            oauth_account_id: 7,
            ..Default::default()
        });
        assert!(is_oauth_fallback_model(&config, &grok_api));
        assert!(!is_oauth_fallback_model(&config, &unrelated_api));

        config.oauth_account_ids = Some(vec![]);
        assert!(!is_oauth_fallback_model(&config, &grok_api));
    }

    #[test]
    fn canonical_fallback_ids_handle_provider_prefixes_and_sol_alias() {
        assert_eq!(canonical_route_model_id("x-ai/grok-4.5"), "grok-4.5");
        assert_eq!(
            canonical_route_model_id("openai/gpt-5.6-sol"),
            "gpt-5.6-sol"
        );
        assert_eq!(canonical_route_model_id("gpt-5.6"), "gpt-5.6-sol");
        // ChatGPT-branded ids canonicalize onto their gpt-* twins globally.
        assert_eq!(canonical_route_model_id("chatgpt-5.6-sol"), "gpt-5.6-sol");
        assert_eq!(canonical_route_model_id("chatgpt-5.6-luna"), "gpt-5.6-luna");
        assert_eq!(
            canonical_route_model_id("OpenAI/ChatGPT-5.6-Luna"),
            "gpt-5.6-luna"
        );
    }

    #[test]
    fn model_identity_requires_display_and_real_id_equivalence() {
        for (left, right) in [
            ("gpt-5.6-sol", "openai/gpt-5.6"),
            ("claude-opus-5", "anthropic/claude-opus-5"),
            ("gemini-3.6-flash", "google/gemini-3-6-flash"),
            ("grok-4.5", "x-ai/grok-4.5"),
            ("deepseek-v4-flash", "deepseek/deepseek-v4-flash"),
            ("claude-opus-4-6", "anthropic/claude-opus-4.6"),
            // ChatGPT-branded ids are the same OpenAI family as gpt-* twins,
            // so every ChatGPT model variant pairs for fallback, not only sol.
            ("chatgpt-5.6-sol", "gpt-5.6-sol"),
            ("chatgpt-5.6-luna", "gpt-5.6-luna"),
            ("chatgpt-5.6-luna", "openai/gpt-5.6-luna"),
        ] {
            assert!(
                same_model_identity(left, right),
                "{left} should match {right}"
            );
        }
        for (left, right) in [
            ("grok-4.5", "openai/grok-4.5"),
            ("claude-opus-5", "claude-opus-5-fast"),
            ("gemini-3.1-pro-high", "gemini-3.1-pro-low"),
            ("kimi-for-coding", "kimi-for-coding-highspeed"),
            ("kimi-k3", "k3-256k"),
            ("vendor-a/model-x", "vendor-b/model-x"),
            ("chatgpt-5.6-luna", "gpt-5.6-sol"),
            ("chatgpt-5.6-luna", "chatgpt-5.6-terra"),
        ] {
            assert!(
                !same_model_identity(left, right),
                "{left} must not match {right}"
            );
        }
    }

    #[test]
    fn dashboard_merges_duplicate_oauth_models_and_keeps_distinct_api_rows() {
        let models = vec![
            ModelConfig {
                model: "gemini-3.7-flash".into(),
                source: "oauth".into(),
                oauth_account_id: 4,
                ..Default::default()
            },
            ModelConfig {
                model: "gemini-3.7-flash".into(),
                source: "oauth".into(),
                oauth_account_id: 26,
                ..Default::default()
            },
            ModelConfig {
                model: "gpt-5.6-sol".into(),
                source: "apikey".into(),
                ..Default::default()
            },
        ];
        let rows = dashboard_model_rows(&models);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].index, 0);
        assert_eq!(rows[0].account_count, 2);
        assert_eq!(rows[1].index, 2);
        assert_eq!(rows[1].account_count, 1);
    }

    #[test]
    fn dashboard_folds_oauth_accounts_and_api_channels_for_the_same_model() {
        let models = vec![
            ModelConfig {
                model: "grok-4.6".into(),
                source: "oauth".into(),
                oauth_account_id: 1,
                ..Default::default()
            },
            ModelConfig {
                model: "x-ai/grok-4.6".into(),
                source: "apikey".into(),
                ..Default::default()
            },
            ModelConfig {
                model: "grok-4.6".into(),
                source: "oauth".into(),
                oauth_account_id: 2,
                ..Default::default()
            },
        ];
        let rows = dashboard_model_rows(&models);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].index, 0);
        assert_eq!(rows[0].account_count, 3);
    }

    #[test]
    fn chatgpt_branded_api_channel_becomes_oauth_fallback_for_every_variant() {
        let mut config = RouterConfig {
            oauth_account_ids: Some(vec![7]),
            ..Default::default()
        };
        let luna_api = ModelConfig {
            model: "chatgpt-5.6-luna".into(),
            ..Default::default()
        };
        config.models = vec![luna_api.clone()];
        assert!(!is_oauth_fallback_model(&config, &luna_api));

        config.models.push(ModelConfig {
            model: "gpt-5.6-luna".into(),
            source: "oauth".into(),
            oauth_account_id: 7,
            ..Default::default()
        });
        assert!(is_oauth_fallback_model(&config, &luna_api));

        let plan = catalog::build_route_plan(&config);
        let api_route = plan
            .iter()
            .find(|route| route.model.model == "chatgpt-5.6-luna")
            .expect("API route exists");
        assert!(api_route.is_oauth_fallback);
        assert!(api_route.join_router);
        assert_eq!(api_route.public_model_id, "gpt-5.6-luna");
        assert_eq!(
            api_route.request_model_ids,
            vec!["gpt-5.6-luna".to_owned()]
        );
    }

    #[test]
    fn fallback_channel_keys_are_stable_and_manual_selection_is_enforced() {
        assert_eq!(
            fallback_channel_key("OpenAI/GPT-5.6", "HTTPS://API.EXAMPLE/V1/"),
            "gpt-5.6-sol|https://api.example/v1"
        );

        let first = ModelConfig {
            model: "gpt-5.6-sol".into(),
            base_url: "https://first.example/v1".into(),
            ..Default::default()
        };
        let second = ModelConfig {
            model: "openai/gpt-5.6-sol".into(),
            base_url: "https://second.example/v1/".into(),
            ..Default::default()
        };
        let mut config = RouterConfig::default();
        assert!(is_fallback_channel_selected(&config, &first));
        assert!(is_fallback_channel_selected(&config, &second));

        config.fallback_channel_selections.insert(
            "gpt-5.6-sol".into(),
            vec![fallback_channel_key(&second.model, &second.base_url)],
        );
        assert!(!is_fallback_channel_selected(&config, &first));
        assert!(is_fallback_channel_selected(&config, &second));

        config
            .fallback_channel_selections
            .insert("gpt-5.6-sol".into(), Vec::new());
        assert!(!is_fallback_channel_selected(&config, &first));
        assert!(!is_fallback_channel_selected(&config, &second));
    }

    #[test]
    fn model_route_policy_defaults_to_subscription_first_and_can_disable_api() {
        let mut config = RouterConfig {
            oauth_account_ids: Some(vec![7]),
            ..Default::default()
        };
        let grok_api = ModelConfig {
            model: "x-ai/grok-4.5".into(),
            ..Default::default()
        };
        config.models = vec![
            grok_api.clone(),
            ModelConfig {
                model: "grok-4.5".into(),
                source: "oauth".into(),
                oauth_account_id: 7,
                ..Default::default()
            },
        ];
        assert_eq!(
            model_route_policy(&config, "grok-4.5"),
            ModelRoutePolicy::SubscriptionFirst
        );
        assert!(is_oauth_fallback_model(&config, &grok_api));

        set_model_route_policy(&mut config, "grok-4.5", ModelRoutePolicy::SubscriptionOnly);
        assert_eq!(
            model_route_policy(&config, "x-ai/grok-4.5"),
            ModelRoutePolicy::SubscriptionOnly
        );
        assert!(!is_oauth_fallback_model(&config, &grok_api));
        assert!(!model_oauth_routing_priorities(&config, "grok-4.5").enabled);

        set_model_route_policy(&mut config, "grok-4.5", ModelRoutePolicy::ApiFirst);
        let priorities = model_oauth_routing_priorities(&config, "grok-4.5");
        assert!(priorities.enabled);
        assert!(!priorities.prefer_oauth);
        assert!(is_oauth_fallback_model(&config, &grok_api));
        let oauth = config
            .models
            .iter()
            .find(|model| model.source == "oauth")
            .cloned()
            .unwrap();
        assert_eq!(model_route_chip(&config, &oauth, 2, true), "2 · API优先");
        set_model_route_policy(
            &mut config,
            "grok-4.5",
            ModelRoutePolicy::SubscriptionFirst,
        );
        assert_eq!(model_route_chip(&config, &oauth, 2, true), "2 · 订阅优先");
    }

    #[test]
    fn native_admin_error_keeps_host_codes_instead_of_collapsing() {
        assert_eq!(
            native_admin_error(&anyhow::anyhow!("class=timeout | code=CR-LFC-0006")),
            "class=timeout | code=CR-LFC-0006"
        );
        let classified = native_admin_error(&anyhow::anyhow!(
            "ROUTER_DEPLOY_COMPLIANCE_ACCEPT_FAILED: CR-VAL-0003"
        ));
        assert!(classified.contains("class=compliance_required"));
        assert!(classified.contains("CR-VAL-0003"));
        let cfg = native_admin_error(&anyhow::anyhow!(
            "class=request_failure | code=CR-CFG-0005 | http=502"
        ));
        assert!(cfg.contains("CR-CFG-0005"));
        assert!(!cfg.eq("class=request_failure"));
    }

    #[test]
    fn new_api_channels_receive_a_writable_insertion_order_priority() {
        let mut config = RouterConfig::default();
        assert_eq!(next_api_channel_priority(&config), 10);
        config.models.push(ModelConfig {
            model: "first".into(),
            priority: 10,
            ..Default::default()
        });
        config.models.push(ModelConfig {
            model: "second".into(),
            priority: 30,
            ..Default::default()
        });
        config.models.push(ModelConfig {
            model: "oauth".into(),
            source: "oauth".into(),
            priority: 999,
            ..Default::default()
        });
        assert_eq!(next_api_channel_priority(&config), 40);
    }

    #[test]
    fn channel_manifest_contains_reference_not_secret() {
        let mut config = RouterConfig::default();
        config.models.push(ModelConfig {
            model: "test".into(),
            base_url: "https://example.invalid/v1".into(),
            api_key: "sk-do-not-write".into(),
            credential_name: "ModelApiKey-test".into(),
            ..Default::default()
        });
        let json = serde_json::to_string(&build_channel_manifest(&config)).unwrap();
        assert!(json.contains("ModelApiKey-test"));
        assert!(!json.contains("sk-do-not-write"));
    }

    #[test]
    fn model_catalog_uses_current_codex_input_modalities() {
        let mut config = RouterConfig::default();
        config.models.push(ModelConfig {
            model: "kimi-for-coding".into(),
            ..Default::default()
        });
        let catalog = build_model_catalog(&config);
        assert_eq!(catalog[0]["input_modalities"], json!(["text", "image"]));
        assert_eq!(catalog[0]["supports_image_detail_original"], true);

        config.models[0].multimodal = "false".into();
        let catalog = build_model_catalog(&config);
        assert_eq!(catalog[0]["input_modalities"], json!(["text"]));
        assert_eq!(catalog[0]["supports_image_detail_original"], false);
    }

    #[test]
    fn official_reasoning_presets_are_non_empty_and_provider_accurate() {
        let cases = [
            (
                "gpt-5.6-sol",
                vec!["low", "medium", "high", "xhigh", "max", "ultra"],
                "medium",
                true,
            ),
            (
                "gpt-5.6-terra",
                vec!["low", "medium", "high", "xhigh", "max", "ultra"],
                "medium",
                true,
            ),
            (
                "gpt-5.6-luna",
                vec!["low", "medium", "high", "xhigh", "max"],
                "medium",
                true,
            ),
            (
                "claude-opus-5",
                vec!["low", "medium", "high", "xhigh", "max"],
                "high",
                false,
            ),
            (
                "gemini-3.6-flash",
                vec!["minimal", "low", "medium", "high"],
                "high",
                false,
            ),
            ("kimi-k3", vec!["low", "high", "max"], "high", false),
            ("k3-256k", vec!["low", "high", "max"], "high", false),
            ("kimi-for-coding", vec!["high"], "high", false),
            (
                "deepseek-v4-flash",
                vec!["none", "low", "high", "max"],
                "high",
                false,
            ),
            ("mimo-v2.5-pro", vec!["high"], "high", false),
            ("grok-4.5", vec!["low", "medium", "high"], "high", false),
            ("unknown-provider-model", vec!["medium"], "medium", false),
        ];
        for (model, levels, default_level, fast) in cases {
            let spec = detect_reasoning(model);
            assert_eq!(spec.levels, levels, "wrong levels for {model}");
            assert_eq!(
                spec.default_level, default_level,
                "wrong default for {model}"
            );
            assert_eq!(spec.supports_fast, fast, "wrong Fast support for {model}");
            assert!(spec.levels.contains(&spec.default_level));
        }
    }

    #[test]
    fn valid_manual_reasoning_overrides_auto_and_invalid_manual_falls_back() {
        let mut model = ModelConfig {
            model: "gpt-5.6-sol".into(),
            reasoning_mode: "manual".into(),
            reasoning_levels: vec!["low".into(), "xhigh".into(), "xhigh".into()],
            default_reasoning_level: "xhigh".into(),
            fast_supported: false,
            ..Default::default()
        };
        let manual = resolve_reasoning(&model, None);
        assert_eq!(manual.levels, vec!["low", "xhigh"]);
        assert_eq!(manual.default_level, "xhigh");
        assert!(!manual.supports_fast);

        model.reasoning_levels = vec!["turbo".into(), "".into()];
        model.default_reasoning_level.clear();
        let fallback = resolve_reasoning(&model, None);
        assert_eq!(fallback.default_level, "medium");
        assert!(fallback.levels.contains(&fallback.default_level));
    }

    #[test]
    fn catalog_never_emits_empty_reasoning_values() {
        let mut config = RouterConfig::default();
        for name in ["unknown", "kimi-for-coding", "deepseek-v4-flash"] {
            config.models.push(ModelConfig {
                model: name.into(),
                ..Default::default()
            });
        }
        for entry in build_model_catalog(&config) {
            let default = entry["default_reasoning_level"].as_str().unwrap();
            let levels = entry["supported_reasoning_levels"].as_array().unwrap();
            assert!(!default.is_empty());
            assert!(!levels.is_empty());
            assert!(levels.iter().any(|level| level["effort"] == default));
        }
    }

    #[test]
    fn legacy_global_reasoning_does_not_override_model_presets() {
        let mut config = RouterConfig::default();
        config.reasoning.mode = "manual".into();
        config.reasoning.levels = vec!["high".into()];
        config.reasoning.default_level = "high".into();
        config.models.push(ModelConfig {
            model: "gpt-5.6-sol".into(),
            ..Default::default()
        });
        let catalog = build_model_catalog(&config);
        assert_eq!(catalog[0]["default_reasoning_level"], "medium");
    }

    #[test]
    fn documented_context_defaults_compact_conservatively() {
        let mut kimi = ModelConfig {
            model: "kimi-for-coding".into(),
            ..Default::default()
        };
        assert_eq!(resolve_context_window(&kimi), 262_144);
        assert_eq!(resolve_auto_compact_token_limit(&kimi), 209_715);

        kimi.model = "gpt-5.6-sol".into();
        assert_eq!(resolve_context_window(&kimi), 272_000);
        assert_eq!(resolve_auto_compact_token_limit(&kimi), 217_600);

        kimi.model = "grok-4.6".into();
        assert_eq!(resolve_context_window(&kimi), 500_000);
        assert_eq!(resolve_auto_compact_token_limit(&kimi), 400_000);

        for model in ["gemini-3.6-flash", "mimo-v2.5-pro", "deepseek-v4-pro"] {
            kimi.model = model.into();
            assert_eq!(resolve_context_window(&kimi), 1_048_576, "{model}");
            assert_eq!(resolve_auto_compact_token_limit(&kimi), 838_860, "{model}");
        }

        kimi.model = "k3-256k".into();
        assert_eq!(resolve_context_window(&kimi), 262_144);
        assert_eq!(resolve_auto_compact_token_limit(&kimi), 209_715);

        kimi.model = "claude-opus-5".into();
        assert_eq!(resolve_context_window(&kimi), 1_000_000);
        assert_eq!(resolve_auto_compact_token_limit(&kimi), 800_000);

        kimi.context_window = 200_000;
        kimi.auto_compact_percent = 75;
        assert_eq!(resolve_auto_compact_token_limit(&kimi), 150_000);
    }

    #[test]
    fn generated_catalog_is_cache_compatible_and_secret_free() {
        let root = temporary_test_dir("catalog");
        let mut config = RouterConfig::default();
        config.models.push(ModelConfig {
            model: "portable-test-model".into(),
            alias: "Portable Test".into(),
            api_key: "must-not-be-written".into(),
            credential_name: "ModelApiKey-portable-test".into(),
            ..Default::default()
        });

        write_all_files(&mut config, &root).unwrap();

        let raw = std::fs::read_to_string(root.join("config/model-catalog.json")).unwrap();
        let catalog: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(catalog["models"].is_array());
        assert!(catalog["models"][0]["additional_speed_tiers"].is_array());
        assert_eq!(catalog["models"][0]["slug"], "portable-test-model");
        assert_eq!(catalog["models"][0]["context_window"], 128_000);
        assert_eq!(catalog["models"][0]["auto_compact_token_limit"], 102_400);
        assert_eq!(catalog["models"][0]["input_modalities"], json!(["text"]));
        assert!(!raw.contains("must-not-be-written"));
        // The deployment scripts read the Router config through
        // `Get-RouterConfigPath`, so it must land on the user-data path rather
        // than beside the executable.
        let config_path = crate::user_data::config_path(&root);
        assert!(!std::fs::read_to_string(&config_path)
            .unwrap()
            .contains("must-not-be-written"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn packaged_layout_keeps_the_deployment_config_out_of_the_release_folder() {
        // A packaged release must never treat the extracted folder as the config
        // location: Apply-Router.ps1 resolves it through the user-data root, so a
        // release-relative write would deploy a stale configuration.
        if std::env::var_os("CODEX_ROUTER_USER_DATA_ROOT").is_some() {
            return;
        }
        let root = temporary_test_dir("packaged-config-path");
        std::fs::write(root.join("release-manifest.json"), "{}").unwrap();
        assert_ne!(
            crate::user_data::config_path(&root),
            root.join("codex-router-config.json")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deployment_failure_summarizes_stdout_when_stderr_is_empty() {
        let root = temporary_test_dir("apply-stdout-error");
        let scripts = root.join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(
            scripts.join("apply-codex-router.ps1"),
            "Write-Output 'stdout-only deployment detail'\nexit 1\n",
        )
        .unwrap();

        let mut displayed = Vec::new();
        let error = run_apply_script(&root, |line| displayed.push(line))
            .unwrap_err()
            .to_string();

        assert!(error.contains("class=unclassified_error"));
        assert!(!error.contains("stdout-only deployment detail"));
        assert!(displayed.iter().all(|line| !line.contains("stdout-only")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deployment_failure_keeps_the_stable_marker_for_the_user_facing_message() {
        let root = temporary_test_dir("apply-marker-error");
        let scripts = root.join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(
            scripts.join("apply-codex-router.ps1"),
            "[Console]::Error.WriteLine('ROUTER_DEPLOY_NO_MODELS: none in C:\\secret-user\\config.json')\nexit 1\n",
        )
        .unwrap();

        let mut displayed = Vec::new();
        let error = run_apply_script(&root, |line| displayed.push(line))
            .unwrap_err()
            .to_string();

        // The marker has to reach the UI so the failure can be explained, while
        // the surrounding detail must still be dropped.
        assert!(error.contains("marker=ROUTER_DEPLOY_NO_MODELS"), "{error}");
        assert!(!error.contains("secret-user"));
        assert!(displayed
            .iter()
            .any(|line| line.contains("marker=ROUTER_DEPLOY_NO_MODELS")));
        assert!(displayed.iter().all(|line| !line.contains("secret-user")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deployment_completion_does_not_wait_for_inherited_service_pipes() {
        let root = temporary_test_dir("apply-inherited-pipe");
        let scripts = root.join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(
            scripts.join("apply-codex-router.ps1"),
            r#"Start-Process powershell.exe -ArgumentList @('-NoLogo','-NoProfile','-Command','Start-Sleep -Seconds 4') -WorkingDirectory $env:TEMP -NoNewWindow | Out-Null
Write-Output '[7/7] Deployment complete.'
[Console]::Error.WriteLine('[codex-router:deployment-complete]')
"#,
        )
        .unwrap();

        let started = Instant::now();
        let mut displayed = Vec::new();
        run_apply_script(&root, |line| displayed.push(line)).unwrap();

        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(displayed.iter().any(|line| line == "[7/7]"));
        assert!(displayed
            .iter()
            .all(|line| !line.contains("deployment-complete")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn profile_isolation_copies_an_existing_windows_credential_without_powershell() {
        let nonce = uuid::Uuid::now_v7().simple();
        let source_name = format!("Test-Profile-Source-{nonce}");
        let profile_id = format!("test-profile-{nonce}");
        let mut source_secret = SecretWide("not-a-real-key".encode_utf16().collect());
        write_router_credential(&source_name, &source_secret.0).unwrap();
        source_secret.0.fill(0);
        let mut config = RouterConfig::default();
        config.models.push(ModelConfig {
            model: "test-model".into(),
            base_url: "https://example.invalid/v1".into(),
            credential_name: source_name.clone(),
            ..Default::default()
        });

        let result = (|| -> anyhow::Result<()> {
            let written = isolate_profile_credentials(&mut config, Path::new("."), &profile_id)?;
            assert_eq!(written.len(), 1);
            assert_eq!(config.models[0].credential_name, written[0]);
            let copied = read_router_credential(&written[0])?.context("copied key is missing")?;
            assert_eq!(
                copied.0,
                "not-a-real-key".encode_utf16().collect::<Vec<_>>()
            );
            remove_isolated_profile_credentials(&written)?;
            Ok(())
        })();
        let _ = delete_router_credential(&source_name);
        result.unwrap();
    }

    #[test]
    fn profile_isolation_validates_all_sources_before_writing_destinations() {
        let nonce = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let source_name = format!("Test-Profile-Source-{nonce}");
        let profile_id = format!("test-profile-{nonce}");
        let source_secret = SecretWide("not-a-real-key".encode_utf16().collect());
        write_router_credential(&source_name, &source_secret.0).unwrap();
        let mut config = RouterConfig {
            models: vec![
                ModelConfig {
                    model: "first-model".into(),
                    base_url: "https://example.invalid/v1".into(),
                    credential_name: source_name.clone(),
                    ..Default::default()
                },
                ModelConfig {
                    model: "second-model".into(),
                    base_url: "https://example.invalid/v1".into(),
                    credential_name: format!("missing-{nonce}"),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let error = isolate_profile_credentials(&mut config, Path::new("."), &profile_id)
            .unwrap_err()
            .to_string();
        let first_destination = format!("Profile-{profile_id}-Model-1-first-model");

        assert!(error.contains("ROUTER_PROFILE_CREDENTIAL_MISSING"));
        assert!(read_router_credential(&first_destination)
            .unwrap()
            .is_none());
        delete_router_credential(&source_name).unwrap();
    }

    #[test]
    fn windows_sandbox_normalization_disables_elevated_setup_loop() {
        // Completed elevated setup is owned by Codex Desktop and must survive.
        let input = "model = \"test\"\r\n\r\n[windows]\r\nsandbox = \"elevated\"\r\n";
        let output = normalize_windows_sandbox_config(input);
        assert!(output.contains("sandbox = \"elevated\""));
        assert!(!output.contains("sandbox = \"unelevated\""));

        // Restricted-mode completion markers are also left untouched.
        let legacy = "model = \"test\"\n\n[windows]\nsandbox = \"unelevated\"\n";
        let cleaned = normalize_windows_sandbox_config(legacy);
        assert!(cleaned.contains("sandbox = \"unelevated\""));
        assert!(cleaned.contains("[windows]"));

        // Exit/restore must reattach a live elevated marker onto a snapshot that
        // predated Windows setup completion.
        let restored = preserve_windows_sandbox_config(
            "model = \"live\"\n[windows]\nsandbox = \"elevated\"\n",
            "model = \"first\"\n",
        );
        assert!(restored.contains("sandbox = \"elevated\""));
        assert!(restored.contains("model = \"first\""));
    }

    #[test]
    fn windows_sandbox_normalization_adds_missing_table_without_touching_other_keys() {
        let input = "model_provider = \"sub2api\"\n\nsandbox_mode = \"danger-full-access\"\n";
        let output = normalize_windows_sandbox_config(input);
        assert!(output.contains("sandbox_mode = \"danger-full-access\""));
        // Restore must never invent a [windows] table.
        assert!(!output.contains("[windows]"));
        assert_eq!(output, input);
    }

    #[test]
    #[ignore = "writes the live 1.7.3 catalog; requires CODEX_ROUTER_LIVE_APPLY=1"]
    fn live_apply_173_catalog_and_binding() {
        assert_eq!(
            std::env::var("CODEX_ROUTER_LIVE_APPLY").as_deref(),
            Ok("1")
        );
        let router_root = std::env::var("CODEX_ROUTER_LIVE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .expect("source root")
                    .to_path_buf()
            });
        let config = RouterConfig::load(&crate::user_data::config_path(&router_root))
            .expect("load live Router config");
        write_model_catalog(&config, &router_root).expect("write 1.7.3 catalog");
        super::codex_toml::write_codex_config_from_router_config(&config, &router_root)
            .expect("repair Codex binding");
        let catalog_path = crate::user_data::state_root(&router_root).join("model-catalog.json");
        let catalog: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(catalog_path).unwrap()).unwrap();
        assert_eq!(catalog["client_version"], env!("CARGO_PKG_VERSION"));
        assert!(
            catalog["models"][0]["base_instructions"]
                .as_str()
                .unwrap()
                .contains("默认使用简体中文")
        );
        assert!(catalog["models"][0]["priority"].as_i64().unwrap() >= 1);
        assert!(
            catalog["models"][0]["truncation_policy"]["limit"]
                .as_i64()
                .unwrap()
                >= 32_000
        );
    }
}
