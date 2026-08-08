use crate::config::{atomic_write, ModelConfig, ReasoningConfig, RouterConfig};
use anyhow::{bail, Context};
use serde_json::json;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use toml_edit::{DocumentMut, Item};
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
    CRED_TYPE_GENERIC,
};
use windows_sys::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

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

fn is_non_openai_oauth_model(model: &ModelConfig) -> bool {
    if model.source != "oauth" {
        return false;
    }
    let platform = model.oauth_platform.trim().to_ascii_lowercase();
    !platform.is_empty() && platform != "openai" && platform != "chatgpt"
}

pub fn resolve_default_model(cfg: &RouterConfig) -> Option<&str> {
    cfg.models
        .iter()
        .find(|model| model.model == cfg.default_model)
        .or_else(|| cfg.models.first())
        .map(|model| model.model.as_str())
}

pub fn normalize_default_model(cfg: &mut RouterConfig) {
    cfg.default_model = resolve_default_model(cfg).unwrap_or_default().to_owned();
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
    let mut real_id = canonical_route_model_id(&raw);
    let provider = if raw.starts_with("openai/")
        || raw.starts_with("chatgpt/")
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

pub fn api_channel_tier(model: &ModelConfig) -> i32 {
    if model.source == "oauth" {
        return 0;
    }
    let explicit_coding_plan = serde_json::from_str::<serde_json::Value>(&model.extra)
        .ok()
        .and_then(|value| {
            value
                .get("codex_router_channel_kind")
                .and_then(|kind| kind.as_str())
                .map(str::to_owned)
        })
        .is_some_and(|kind| kind.eq_ignore_ascii_case("coding_plan"));
    let coding_endpoint = url::Url::parse(model.base_url.trim())
        .ok()
        .is_some_and(|url| {
            url.scheme() == "https"
                && ((url.host_str() == Some("api.kimi.com") && url.path().starts_with("/coding"))
                    || (url.host_str() == Some("api.moonshot.ai")
                        && url.path().starts_with("/coding"))
                    // ByteDance Volcengine Ark subscription plans: /api/coding/v3
                    // (Coding Plan) and /api/plan/v3 (Agent Plan).
                    || (url
                        .host_str()
                        .is_some_and(|host| host.ends_with(".volces.com"))
                        && (url.path().starts_with("/api/coding")
                            || url.path().starts_with("/api/plan"))))
        });
    if explicit_coding_plan || coding_endpoint {
        1
    } else {
        2
    }
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
    if !cfg.oauth_fallback.enabled
        || candidate.source == "oauth"
        || !is_fallback_channel_selected(cfg, candidate)
    {
        return false;
    }
    cfg.models.iter().any(|model| {
        model.source == "oauth"
            && cfg
                .oauth_account_ids
                .as_ref()
                .is_none_or(|ids| ids.contains(&model.oauth_account_id))
            && same_model_identity(&model.model, &candidate.model)
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

pub fn resolved_model_display_name(cfg: &RouterConfig, model: &ModelConfig) -> String {
    if is_model_alias_customized(model) && !model.alias.trim().is_empty() {
        return model.alias.trim().to_owned();
    }
    let recommended = recommended_model_display_name(&model.model);
    if model.source != "oauth" {
        return recommended;
    }
    let merged = cfg.oauth_fallback.enabled
        && cfg.models.iter().any(|candidate| {
            candidate.source != "oauth"
                && same_model_identity(&candidate.model, &model.model)
                && is_fallback_channel_selected(cfg, candidate)
        });
    if merged {
        recommended
    } else {
        format!("{recommended}(OAuth)")
    }
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
    if name.contains("grok-4.5") {
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

pub fn build_model_catalog(cfg: &RouterConfig) -> Vec<serde_json::Value> {
    let mut seen_model_ids = std::collections::HashSet::new();
    cfg.models
        .iter()
        .filter(|model| {
            let mut public_id = model.model.trim().to_ascii_lowercase();
            if public_id == "gpt-5.6" {
                public_id = "gpt-5.6-sol".to_owned();
            }
            seen_model_ids.insert(public_id)
        })
        .enumerate()
        .map(|(index, model)| {
            let reasoning = resolve_reasoning(model, None);
            let reasoning_levels: Vec<_> = reasoning
                .levels
                .iter()
                .map(|effort| json!({"effort": effort, "description": format!("{} reasoning level", effort)}))
                .collect();
            let supports_images = resolve_multimodal(model);
            let context_window = resolve_context_window(model);
            let auto_compact_token_limit = resolve_auto_compact_token_limit(model);
            let base_instructions = "You are Codex, a coding agent. Work in the user's workspace, follow the user's instructions, preserve unrelated changes, and verify completed work.";
            let service_tiers = if reasoning.supports_fast {
                vec![json!({
                    "id": "priority",
                    "name": "Fast",
                    "description": "1.5x speed, increased usage",
                })]
            } else {
                Vec::new()
            };
            json!({
                "slug": model.model,
                "display_name": resolved_model_display_name(cfg, model),
                "description": format!("Codex-Router model #{}", index + 1),
                "default_reasoning_level": reasoning.default_level,
                "supported_reasoning_levels": reasoning_levels,
                "input_modalities": if supports_images { vec!["text", "image"] } else { vec!["text"] },
                "supports_image_detail_original": supports_images,
                "supports_vision": supports_images,
                "context_window": context_window,
                "max_context_window": context_window,
                "auto_compact_token_limit": auto_compact_token_limit,
                "shell_type": "shell_command",
                "visibility": "list",
                "supported_in_api": true,
                "priority": model.priority,
                "additional_speed_tiers": if reasoning.supports_fast { vec!["fast"] } else { vec![] },
                "service_tiers": service_tiers,
                "availability_nux": null,
                "upgrade": null,
                "base_instructions": base_instructions,
                "model_messages": {
                    "instructions_template": base_instructions,
                    "instructions_variables": null,
                    "approvals": null,
                    "auto_review": null,
                    "permissions": null,
                },
                "include_skills_usage_instructions": false,
                "default_reasoning_summary": "none",
                "support_verbosity": true,
                "default_verbosity": "low",
                "apply_patch_tool_type": "freeform",
            "web_search_tool_type": if is_non_openai_oauth_model(model) { serde_json::Value::Null } else { json!("text_and_image") },
                "truncation_policy": { "mode": "tokens", "limit": 10_000 },
                "supports_parallel_tool_calls": true,
                "comp_hash": "codex-router-v1",
                "effective_context_window_percent": model.auto_compact_percent,
                "experimental_supported_tools": Vec::<String>::new(),
            "supports_search_tool": !is_non_openai_oauth_model(model),
                "use_responses_lite": true,
                "tool_mode": "code_mode_only",
                "multi_agent_version": "v2",
            })
        })
        .collect()
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

pub fn write_all_files(cfg: &RouterConfig, router_root: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(router_root.join("config"))?;
    // The deployment scripts resolve the Router config through
    // `Get-RouterConfigPath`, which points at the persistent user-data root for
    // packaged releases. Writing it next to the executable would leave
    // Apply-Router.ps1 deploying a stale config from a previous session.
    let config_path = crate::user_data::config_path(router_root);
    // The catalog and channel manifest stay beside the executable because the
    // scripts read them from `$routerRoot\config`.
    let catalog_path = {
        let stable = crate::user_data::state_root(router_root).join("model-catalog.json");
        if stable != router_root.join("model-catalog.json") {
            stable
        } else {
            router_root.join("config").join("model-catalog.json")
        }
    };
    let channels_path = router_root.join("config").join("sub2api-channels.json");
    let writes = vec![
        (
            catalog_path,
            serde_json::to_vec_pretty(&json!({
            "fetched_at": chrono::Utc::now().to_rfc3339(),
            "etag": "codex-router-local-v2",
            "client_version": env!("CARGO_PKG_VERSION"),
            "models": build_model_catalog(cfg),
            }))?,
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

pub(crate) fn ps_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub(crate) fn run_powershell_stdin(script: &str) -> anyhow::Result<String> {
    let mut child = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x08000000)
        .spawn()
        .context("无法启动 Windows PowerShell")?;
    child
        .stdin
        .as_mut()
        .context("无法打开 PowerShell 标准输入")?
        .write_all(script.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        let details = String::from_utf8_lossy(&output.stderr);
        bail!(
            "PowerShell 执行失败: {}",
            crate::runtime_logs::summarize_error_for_display(details.trim())
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
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

fn read_router_credential(name: &str) -> anyhow::Result<Option<SecretWide>> {
    const ERROR_NOT_FOUND: i32 = 1168;

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
    let mut target = router_credential_target(name);
    let mut username = std::env::var("USERNAME")
        .unwrap_or_default()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let blob_size = u32::try_from(secret.len().saturating_mul(std::mem::size_of::<u16>()))
        .context("Windows credential is too large")?;
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

pub fn store_credentials(cfg: &mut RouterConfig, router_root: &Path) -> anyhow::Result<()> {
    let module = router_root.join("scripts").join("CredentialStore.psm1");
    let mut script = format!(
        "$ErrorActionPreference='Stop'\nImport-Module {} -Force\n",
        ps_literal(&module.to_string_lossy())
    );
    for (index, model) in cfg.models.iter_mut().enumerate() {
        if model.source == "oauth" {
            model.credential_name.clear();
            continue;
        }
        if model.credential_name.trim().is_empty() {
            model.credential_name = format!("ModelApiKey-{}-{}", index + 1, slugify(&model.model));
        }
        if !model.api_key.trim().is_empty() {
            script.push_str(&format!(
                "Set-RouterCredential -Name {} -Secret {}\n",
                ps_literal(&model.credential_name),
                ps_literal(model.api_key.trim())
            ));
        }
        if model
            .base_url
            .to_ascii_lowercase()
            .contains("ark.cn-beijing.volces.com/api/coding")
        {
            if !model.volcengine_access_key_id.trim().is_empty() {
                script.push_str(&format!(
                    "Set-RouterCredential -Name 'VolcengineAccessKeyId' -Secret {}\n",
                    ps_literal(model.volcengine_access_key_id.trim())
                ));
            }
            if !model.volcengine_secret_access_key.trim().is_empty() {
                script.push_str(&format!(
                    "Set-RouterCredential -Name 'VolcengineSecretAccessKey' -Secret {}\n",
                    ps_literal(model.volcengine_secret_access_key.trim())
                ));
            }
        }
    }
    if cfg.proxy.password_credential.trim().is_empty() {
        cfg.proxy.password_credential = "ProxyPassword".to_string();
    }
    if !cfg.proxy.password.is_empty() {
        script.push_str(&format!(
            "Set-RouterCredential -Name {} -Secret {}\n",
            ps_literal(&cfg.proxy.password_credential),
            ps_literal(&cfg.proxy.password)
        ));
    }
    script.push_str("'credentials-saved'\n");
    let _ = run_powershell_stdin(&script)?;
    for model in &mut cfg.models {
        model.api_key.clear();
    }
    cfg.proxy.password.clear();
    cfg.local_api_key.clear();
    Ok(())
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
    run_apply_script_with_cancel(router_root, &cancel, on_line)
}

pub fn run_apply_script_with_cancel<F>(
    router_root: &Path,
    cancel: &AtomicBool,
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
            "Sub2API compliance acknowledgement recorded",
            "Sub2API administrator ready",
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
            "Codex Router secrets and PostgreSQL",
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
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x08000000);
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
    let script = router_root.join("scripts").join("Stop-Router.ps1");
    let output = Command::new("powershell.exe")
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
        .stdin(Stdio::null())
        .creation_flags(0x08000000)
        .output()
        .with_context(|| format!("无法运行 {}", script.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let details = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    let safe_details = if details.is_empty() {
        "class=unclassified_error".to_owned()
    } else {
        crate::runtime_logs::summarize_error_for_display(details)
    };
    bail!(
        "停止 Router 失败（退出代码 {}）: {}",
        output.status.code().unwrap_or(-1),
        safe_details
    )
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
    // Reject third-party profiles that reuse the custom id but point at a local
    // URL while naming themselves something other than Codex-Router.
    match provider.get("name").and_then(Item::as_str) {
        None | Some("Codex-Router") => true,
        Some(_) if provider_id == "codex_router" => true,
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
    codex_config_uses_router(&config_text, router_base_url)
}

pub fn codex_router_mode_active(cfg: &RouterConfig) -> bool {
    codex_router_mode_configured(cfg)
        && local_router_health_available(cfg.deploy.sub2api_host.trim())
}

pub fn load_oauth_accounts(router_root: &Path) -> anyhow::Result<Vec<crate::OAuthAccountSummary>> {
    let script = router_root.join("scripts").join("Get-OAuthAccounts.ps1");
    let mut last_failure = "ROUTER_OAUTH_ACCOUNTS_UNAVAILABLE".to_owned();
    // Prefer PowerShell 7 when present: JSON arrays and StrictMode edge cases
    // are substantially more reliable than Windows PowerShell 5.1.
    let shell = prefer_powershell_executable();
    let mut attempted_repair = false;
    for attempt in 0..4 {
        if attempt == 0 {
            // Soft wait for the local admin API without failing the whole load.
            for _ in 0..6 {
                if local_router_health_available("http://127.0.0.1:18080") {
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        // When admin login is rate-limited or services are from an older package
        // folder, repair once from the current install root before retrying.
        if !attempted_repair
            && attempt > 0
            && oauth_accounts_failure_needs_router_repair(&last_failure)
        {
            attempted_repair = true;
            let _ = ensure_router_healthy(router_root);
            std::thread::sleep(Duration::from_millis(800));
        }
        let output_result = Command::new(&shell)
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
            .stdin(Stdio::null())
            .creation_flags(0x08000000)
            .output();

        match output_result {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout);
                let trimmed = extract_json_payload(text.as_ref());
                if trimmed.is_empty() || trimmed == "null" {
                    return Ok(Vec::new());
                }
                let parsed = if trimmed.starts_with('[') {
                    serde_json::from_str::<Vec<crate::OAuthAccountSummary>>(trimmed)
                        .map_err(|error| error.to_string())
                } else {
                    serde_json::from_str::<crate::OAuthAccountSummary>(trimmed)
                        .map(|account| vec![account])
                        .map_err(|error| error.to_string())
                };
                match parsed {
                    Ok(accounts) => return Ok(accounts),
                    Err(error) => {
                        last_failure = format!(
                            "ROUTER_OAUTH_ACCOUNTS_PARSE: {}",
                            crate::runtime_logs::summarize_error_for_display(&error)
                        );
                    }
                }
            }
            Ok(output) => {
                last_failure = oauth_accounts_process_failure_summary(&output);
            }
            Err(error) => {
                last_failure = format!(
                    "ROUTER_OAUTH_ACCOUNTS_UNAVAILABLE: {}",
                    crate::runtime_logs::summarize_error_for_display(&format!(
                        "oauth accounts process start failed: {error}"
                    ))
                );
            }
        }

        if attempt + 1 < 4 && oauth_accounts_failure_is_retryable(&last_failure) {
            std::thread::sleep(Duration::from_millis(500 + attempt as u64 * 400));
        } else {
            break;
        }
    }
    bail!("{last_failure}")
}

fn oauth_accounts_failure_needs_router_repair(summary: &str) -> bool {
    let lower = summary.to_ascii_lowercase();
    lower.contains("rate-limited")
        || lower.contains("429")
        || lower.contains("no access token")
        || lower.contains("503")
        || lower.contains("install_root")
        || lower.contains("health check failed")
        || lower.contains("connection_refused")
}

fn ensure_router_healthy(router_root: &Path) -> bool {
    let script = router_root.join("scripts").join("Ensure-RouterHealthy.ps1");
    if !script.is_file() {
        return false;
    }
    let shell = prefer_powershell_executable();
    Command::new(shell)
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
        .stdin(Stdio::null())
        .creation_flags(0x08000000)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn prefer_powershell_executable() -> String {
    let candidates = [
        r"C:\Program Files\PowerShell\7\pwsh.exe",
        r"C:\Program Files\PowerShell\7-preview\pwsh.exe",
    ];
    for candidate in candidates {
        if Path::new(candidate).is_file() {
            return candidate.to_owned();
        }
    }
    "powershell.exe".to_owned()
}

fn extract_json_payload(text: &str) -> &str {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "";
    }
    if let Some(start) = trimmed.find(['[', '{']) {
        let payload = trimmed[start..].trim();
        if payload.starts_with('[') || payload.starts_with('{') {
            return payload;
        }
    }
    trimmed
}

fn oauth_accounts_process_failure_summary(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = [stderr.trim(), stdout.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let detail = if combined.is_empty() {
        "oauth accounts script failed without output".to_owned()
    } else {
        combined
    };
    format!(
        "ROUTER_OAUTH_ACCOUNTS_UNAVAILABLE: {}",
        crate::runtime_logs::summarize_error_for_display(&detail)
    )
}

fn oauth_accounts_failure_is_retryable(summary: &str) -> bool {
    [
        "class=connection_refused",
        "class=connection_closed",
        "class=timeout",
        "class=lifecycle_busy",
        "class=lifecycle_deferred",
        "class=process_failure",
        "class=empty_response",
        "class=authentication",
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

fn load_usage_snapshot_with_timeout(
    router_root: &Path,
    profile_name: &str,
    cfg: &RouterConfig,
    timeout: Duration,
) -> anyhow::Result<crate::UsageSnapshot> {
    let script = router_root.join("scripts").join("Get-UsageMonitor.ps1");
    let state_dir = crate::user_data::data_root(router_root).join("ui");
    std::fs::create_dir_all(&state_dir)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let config_snapshot = state_dir.join(format!(
        "usage-monitor-config-{}-{nonce}.tmp.json",
        std::process::id(),
    ));
    cfg.save(&config_snapshot)?;
    let result = (|| -> anyhow::Result<crate::UsageSnapshot> {
        let mut last_failure = "class=unclassified_error".to_owned();
        for attempt in 0..2 {
            let shell = prefer_powershell_executable();
            let mut child = match Command::new(shell)
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ])
                .arg(&script)
                .args(["-ProfileName", profile_name])
                .arg("-ConfigPath")
                .arg(&config_snapshot)
                .current_dir(router_root)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .creation_flags(0x08000000)
                .spawn()
            {
                Ok(child) => child,
                Err(error) => {
                    last_failure = crate::runtime_logs::summarize_error_for_display(&format!(
                        "usage monitor process start failed: {error}"
                    ));
                    break;
                }
            };
            let stdout = child
                .stdout
                .take()
                .context("usage monitor stdout unavailable")?;
            let stderr = child
                .stderr
                .take()
                .context("usage monitor stderr unavailable")?;
            let stdout_reader = std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = BufReader::new(stdout).read_to_end(&mut bytes);
                bytes
            });
            let stderr_reader = std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = BufReader::new(stderr).read_to_end(&mut bytes);
                bytes
            });
            let started = Instant::now();
            let status = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break Some(status),
                    Ok(None) if started.elapsed() < timeout => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Ok(None) => {
                        terminate_deployment_process_tree(&mut child);
                        last_failure = "class=timeout".to_owned();
                        break None;
                    }
                    Err(error) => {
                        terminate_deployment_process_tree(&mut child);
                        last_failure = crate::runtime_logs::summarize_error_for_display(&format!(
                            "usage monitor process wait failed: {error}"
                        ));
                        break None;
                    }
                }
            };
            let stdout = stdout_reader.join().unwrap_or_default();
            let stderr = stderr_reader.join().unwrap_or_default();
            let Some(status) = status else { break };
            let output = std::process::Output {
                status,
                stdout,
                stderr,
            };

            if output.status.success() {
                match parse_usage_snapshot_output(&output.stdout) {
                    Ok(snapshot) => return Ok(snapshot),
                    Err(error) => last_failure = error.to_string(),
                }
            } else {
                last_failure = usage_process_failure_summary(&output);
            }

            if attempt == 0 && usage_failure_is_locally_retryable(&last_failure) {
                std::thread::sleep(Duration::from_millis(500));
            } else {
                break;
            }
        }
        bail!(last_failure)
    })();
    let _ = std::fs::remove_file(&config_snapshot);
    result
}

fn usage_failure_is_locally_retryable(summary: &str) -> bool {
    [
        "class=connection_refused",
        "class=connection_closed",
        "class=process_failure",
        "class=empty_response",
    ]
    .iter()
    .any(|class| summary.contains(class))
}

fn parse_usage_snapshot_output(stdout: &[u8]) -> anyhow::Result<crate::UsageSnapshot> {
    let text = std::str::from_utf8(stdout)
        .map_err(|_| anyhow::anyhow!("class=invalid_response_encoding"))?;
    let trimmed = text.trim().trim_start_matches('\u{feff}');
    if trimmed.is_empty() || trimmed == "null" {
        bail!("class=empty_response");
    }
    let json = trimmed
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| line.starts_with('{') && line.ends_with('}'))
        .unwrap_or(trimmed);
    serde_json::from_str(json).map_err(|_| anyhow::anyhow!("class=invalid_response"))
}

fn usage_process_failure_summary(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let raw = if !stderr.trim().is_empty() {
        stderr.trim()
    } else {
        stdout.trim()
    };
    let exit_code = output.status.code().unwrap_or(-1);
    if raw.is_empty() {
        return format!("class=process_failure | exit_code={exit_code}");
    }
    let summary = crate::runtime_logs::summarize_error_for_display(raw);
    if summary == "class=unclassified_error" {
        format!("{summary} | exit_code={exit_code}")
    } else {
        summary
    }
}

pub fn import_grok_sso(router_root: &Path, authorization: &str) -> anyhow::Result<String> {
    if authorization.trim().is_empty() {
        bail!("Grok authorization code / SSO token is empty");
    }
    let script = router_root.join("scripts").join("Import-GrokSSO.ps1");
    let mut child = Command::new("powershell.exe")
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
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x08000000)
        .spawn()
        .with_context(|| format!("无法启动 Grok 授权码导入: {}", script.display()))?;
    let mut secret_bytes = authorization.as_bytes().to_vec();
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&secret_bytes)?;
    }
    secret_bytes.fill(0);
    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let details = if stderr.is_empty() { &stdout } else { &stderr };
        bail!(
            "Grok 授权码导入失败: {}",
            crate::runtime_logs::summarize_error_for_display(details)
        );
    }
    Ok("Grok authorization imported".to_owned())
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
    let script = router_root
        .join("scripts")
        .join("Set-OAuthAccountPriority.ps1");
    let shell = prefer_powershell_executable();
    let output = Command::new(&shell)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .args([
            "-AccountId",
            &account_id.to_string(),
            "-Priority",
            &priority.to_string(),
        ])
        .current_dir(router_root)
        .stdin(Stdio::null())
        .creation_flags(0x08000000)
        .output()
        .with_context(|| format!("无法更新 OAuth 优先级: {}", script.display()))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        bail!(
            "无法更新 OAuth 优先级: {}",
            crate::runtime_logs::summarize_error_for_display(&message)
        );
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let trimmed = extract_json_payload(text.as_ref());
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(saved) = value.get("priority").and_then(|v| v.as_i64()) {
            return Ok(saved as i32);
        }
    }
    Ok(priority)
}

pub fn revoke_oauth_account(router_root: &Path, account_id: i64) -> anyhow::Result<()> {
    if account_id <= 0 {
        bail!("OAuth 账号 ID 无效");
    }
    let script = router_root.join("scripts").join("Remove-OAuthAccount.ps1");
    let output = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .args(["-AccountId", &account_id.to_string()])
        .current_dir(router_root)
        .stdin(Stdio::null())
        .creation_flags(0x08000000)
        .output()
        .with_context(|| format!("无法撤销 OAuth 账号: {}", script.display()))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        bail!(
            "无法撤销 OAuth 账号: {}",
            crate::runtime_logs::summarize_error_for_display(&message)
        );
    }
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
    fn router_mode_requires_the_selected_provider_and_matching_local_url() {
        let config = r#"model_provider = "codex_router"
model = "gpt-5.6-sol"

[model_providers.codex_router]
name = "Codex-Router"
base_url = "http://127.0.0.1:18080/v1"
"#;
        assert!(codex_config_uses_router(config, "http://127.0.0.1:18080"));
        assert!(!codex_config_uses_router(config, "http://127.0.0.1:19090"));
        assert!(!codex_config_uses_router(
            &config.replace(
                "model_provider = \"codex_router\"",
                "model_provider = \"openai\""
            ),
            "http://127.0.0.1:18080"
        ));

        let legacy_custom = r#"model_provider = "custom"
model = "gpt-5.6-sol"

[model_providers.custom]
name = "Codex-Router"
base_url = "http://127.0.0.1:18080/v1"
"#;
        assert!(codex_config_uses_router(
            legacy_custom,
            "http://127.0.0.1:18080"
        ));

        let chiral = r#"model_provider = "custom"
model = "grok-4.5"

[model_providers.custom]
name = "micu"
base_url = "https://api.430123.xyz/v1"
"#;
        assert!(!codex_config_uses_router(chiral, "http://127.0.0.1:18080"));

        let legacy = config
            .replace(
                "model_provider = \"codex_router\"",
                "model_provider = \"sub2api\"",
            )
            .replace("model_providers.codex_router", "model_providers.sub2api");
        assert!(codex_config_uses_router(&legacy, "http://127.0.0.1:18080"));
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
    fn configured_router_mode_does_not_depend_on_backend_health() {
        let root = temporary_test_dir("configured-router-mode");
        let mut cfg = RouterConfig::default();
        cfg.deploy.codex_home = root.to_string_lossy().into_owned();
        cfg.deploy.sub2api_host = "http://127.0.0.1:1".into();
        std::fs::write(
            root.join("config.toml"),
            "model_provider = \"codex_router\"\n\
             [model_providers.codex_router]\n\
             name = \"Codex-Router\"\n\
             base_url = \"http://127.0.0.1:1/v1\"\n",
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
    fn oauth_display_name_tracks_merge_state_without_overwriting_user_aliases() {
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
            "ChatGPT-5.6-Sol(OAuth)"
        );
        config.models.push(fallback);
        assert_eq!(
            resolved_model_display_name(&config, &oauth),
            "ChatGPT-5.6-Sol"
        );
        config.oauth_fallback.enabled = false;
        assert_eq!(
            resolved_model_display_name(&config, &oauth),
            "ChatGPT-5.6-Sol(OAuth)"
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
            // Subscription plans outrank pay-as-you-go third-party APIs.
            assert_eq!(api_channel_tier(&model), 1);
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

        assert_eq!(resolve_default_model(&config), Some("first"));
        normalize_default_model(&mut config);
        assert_eq!(config.default_model, "first");

        config.default_model = "second".into();
        assert_eq!(resolve_default_model(&config), Some("second"));

        config.models.remove(1);
        normalize_default_model(&mut config);
        assert_eq!(config.default_model, "first");
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
        ] {
            assert!(
                !same_model_identity(left, right),
                "{left} must not match {right}"
            );
        }
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

        write_all_files(&config, &root).unwrap();

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
    fn usage_monitor_retries_once_after_a_transient_process_failure() {
        let root = temporary_test_dir("usage-retry");
        let scripts = root.join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(
            scripts.join("Get-UsageMonitor.ps1"),
            r#"param([string]$ProfileName, [string]$ConfigPath)
$marker = Join-Path $PSScriptRoot 'attempted.marker'
if (-not (Test-Path -LiteralPath $marker)) {
    [IO.File]::WriteAllText($marker, 'first')
    Write-Output 'connection refused during startup'
    exit 1
}
Write-Output '{}'
"#,
        )
        .unwrap();

        let snapshot = load_usage_snapshot(&root, "test", &RouterConfig::default()).unwrap();

        assert!(snapshot.subscriptions.is_empty());
        assert!(scripts.join("attempted.marker").is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn usage_monitor_does_not_repeat_a_configuration_failure() {
        let root = temporary_test_dir("usage-config-no-retry");
        let scripts = root.join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(
            scripts.join("Get-UsageMonitor.ps1"),
            r#"$attemptPath = Join-Path $PSScriptRoot 'attempt-count.txt'
$attempt = if (Test-Path -LiteralPath $attemptPath) { [int](Get-Content -LiteralPath $attemptPath -Raw) } else { 0 }
[IO.File]::WriteAllText($attemptPath, [string]($attempt + 1))
Write-Output 'configuration snapshot unavailable'
exit 1
"#,
        )
        .unwrap();

        let error = load_usage_snapshot(&root, "test", &RouterConfig::default())
            .unwrap_err()
            .to_string();

        assert_eq!(error, "class=configuration");
        assert_eq!(
            std::fs::read_to_string(scripts.join("attempt-count.txt")).unwrap(),
            "1"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[allow(clippy::permissions_set_readonly_false)]
    fn usage_monitor_uses_a_unique_snapshot_when_a_stale_process_file_exists() {
        let root = temporary_test_dir("usage-unique-snapshot");
        let scripts = root.join("scripts");
        let state = root.join("data").join("ui");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(scripts.join("Get-UsageMonitor.ps1"), "Write-Output '{}'\n").unwrap();
        let stale = state.join(format!(
            "usage-monitor-config-{}.tmp.json",
            std::process::id()
        ));
        std::fs::write(&stale, "stale").unwrap();
        let mut permissions = std::fs::metadata(&stale).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&stale, permissions).unwrap();

        let snapshot = load_usage_snapshot(&root, "test", &RouterConfig::default()).unwrap();

        assert!(snapshot.subscriptions.is_empty());
        assert_eq!(std::fs::read_to_string(&stale).unwrap(), "stale");
        let mut permissions = std::fs::metadata(&stale).unwrap().permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&stale, permissions).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn usage_monitor_uses_stdout_when_stderr_is_empty() {
        let root = temporary_test_dir("usage-stdout-error");
        let scripts = root.join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(
            scripts.join("Get-UsageMonitor.ps1"),
            "Write-Output 'connection refused during startup'\nexit 1\n",
        )
        .unwrap();

        let error = load_usage_snapshot(&root, "test", &RouterConfig::default())
            .unwrap_err()
            .to_string();

        assert_eq!(error, "class=connection_refused");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn usage_monitor_reports_an_exit_code_when_process_output_is_empty() {
        let root = temporary_test_dir("usage-empty-error");
        let scripts = root.join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("Get-UsageMonitor.ps1"), "exit 7\n").unwrap();

        let error = load_usage_snapshot(&root, "test", &RouterConfig::default())
            .unwrap_err()
            .to_string();

        assert_eq!(error, "class=process_failure | exit_code=7");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn usage_monitor_times_out_and_terminates_the_helper() {
        let root = temporary_test_dir("usage-timeout");
        let scripts = root.join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(
            scripts.join("Get-UsageMonitor.ps1"),
            "Start-Sleep -Seconds 30\nWrite-Output '{}'\n",
        )
        .unwrap();

        let started = Instant::now();
        let error = load_usage_snapshot_with_timeout(
            &root,
            "test",
            &RouterConfig::default(),
            Duration::from_millis(250),
        )
        .unwrap_err()
        .to_string();

        assert_eq!(error, "class=timeout");
        assert!(started.elapsed() < Duration::from_secs(5));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn usage_monitor_parser_accepts_utf8_names_and_ignores_prefix_noise() {
        let output = "warning: cached data follows\n{\"profileName\":\"中文配置\",\"subscriptions\":[],\"apiChannels\":[]}\n";
        let snapshot = parse_usage_snapshot_output(output.as_bytes()).unwrap();
        assert_eq!(snapshot.profile_name, "中文配置");
        assert!(snapshot.subscriptions.is_empty());
        assert!(snapshot.api_channels.is_empty());
    }

    #[test]
    fn usage_monitor_parser_classifies_non_utf8_output() {
        let error = parse_usage_snapshot_output(&[0x81, 0x40])
            .unwrap_err()
            .to_string();
        assert_eq!(error, "class=invalid_response_encoding");
    }

    #[test]
    fn profile_isolation_copies_an_existing_windows_credential_without_powershell() {
        let nonce = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
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
}
