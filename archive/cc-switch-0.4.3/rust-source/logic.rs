use crate::config::{atomic_write, ModelConfig, ReasoningConfig, RouterConfig};
use anyhow::{bail, Context};
#[cfg(test)]
use flate2::read::GzDecoder;
use flate2::{write::GzEncoder, Compression};
use serde_json::json;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use toml_edit::{Array, DocumentMut, Item, Table};
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
    CRED_TYPE_GENERIC,
};
#[cfg(test)]
use windows_sys::Win32::Security::Cryptography::CryptUnprotectData;
use windows_sys::Win32::Security::Cryptography::{
    CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
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
        id: "opencode-go",
        label_zh: "OpenCode Go / 官方编程模型订阅",
        label_en: "OpenCode Go / Official coding subscription",
        base_url: "https://opencode.ai/zen/go/v1",
        model: "gpt-5.6-luna",
        alias: "GPT 5.6 Luna / OpenCode Go",
        website_url: "https://opencode.ai/zen",
        docs_url: "https://opencode.ai/docs/go/",
    },
    ChannelPreset {
        id: "openai",
        label_zh: "OpenAI 官方 API",
        label_en: "OpenAI official API",
        base_url: "https://api.openai.com/v1",
        model: "gpt-5.6-sol",
        alias: "GPT-5.6 Sol",
        website_url: "https://platform.openai.com/",
        docs_url: "https://developers.openai.com/api/docs/models/gpt-5.6-sol",
    },
    ChannelPreset {
        id: "anthropic",
        label_zh: "Anthropic / Claude",
        label_en: "Anthropic / Claude",
        base_url: "https://api.anthropic.com/v1",
        model: "claude-opus-5",
        alias: "Claude Opus 5",
        website_url: "https://console.anthropic.com/",
        docs_url: "https://platform.claude.com/docs/en/about-claude/models/overview",
    },
    ChannelPreset {
        id: "openrouter",
        label_zh: "OpenRouter",
        label_en: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        model: "openai/gpt-5.6-sol",
        alias: "GPT-5.6 Sol via OpenRouter",
        website_url: "https://openrouter.ai/",
        docs_url: "https://openrouter.ai/docs/quickstart",
    },
    ChannelPreset {
        id: "kimi",
        label_zh: "Kimi Coding Plan",
        label_en: "Kimi Coding Plan",
        base_url: "https://api.kimi.com/coding/v1",
        model: "k3-256k",
        alias: "Kimi K3 256K (Coding Plan)",
        website_url: "https://www.kimi.com/code/console",
        docs_url: "https://www.kimi.com/code/docs/en/third-party-tools/codex.html",
    },
    ChannelPreset {
        id: "mimo",
        label_zh: "Xiaomi MiMo Token Plan",
        label_en: "Xiaomi MiMo Token Plan",
        base_url: "https://api.xiaomimimo.com/v1",
        model: "mimo-v2.5-pro",
        alias: "MiMo V2.5 Pro",
        website_url: "https://platform.xiaomimimo.com/token-plan",
        docs_url: "https://mimo.mi.com/docs/models/mimo-v2-5-pro",
    },
    ChannelPreset {
        id: "deepseek",
        label_zh: "DeepSeek 官方 API",
        label_en: "DeepSeek official API",
        base_url: "https://api.deepseek.com/v1",
        model: "deepseek-v4-pro",
        alias: "DeepSeek V4 Pro",
        website_url: "https://platform.deepseek.com/",
        docs_url: "https://api-docs.deepseek.com/",
    },
    ChannelPreset {
        id: "gemini",
        label_zh: "Google Gemini 兼容 API",
        label_en: "Google Gemini compatible API",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai/",
        model: "gemini-3.6-flash",
        alias: "Gemini 3.6 Flash",
        website_url: "https://aistudio.google.com/",
        docs_url: "https://ai.google.dev/gemini-api/docs/openai",
    },
];

pub fn channel_presets() -> &'static [ChannelPreset] {
    CHANNEL_PRESETS
}

pub fn apply_channel_preset(model: &mut ModelConfig, preset_id: &str) -> bool {
    let Some(preset) = CHANNEL_PRESETS.iter().find(|preset| preset.id == preset_id) else {
        return false;
    };
    model.model = preset.model.to_owned();
    model.alias = preset.alias.to_owned();
    model.base_url = preset.base_url.to_owned();
    model.priority = 10;
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
            "OpenAI GPT-5.6 Sol",
            "OpenAI GPT-5.6 Sol",
            "Codex 官方模型目录；常规默认 medium",
            "Official Codex model catalog; medium is the normal default",
        );
    }
    if name.contains("gpt-5.6-terra") {
        return ReasoningSpec::new(
            &["low", "medium", "high", "xhigh", "max", "ultra"],
            "medium",
            true,
            "OpenAI GPT-5.6 Terra",
            "OpenAI GPT-5.6 Terra",
            "Codex 官方模型目录",
            "Official Codex model catalog",
        );
    }
    if name.contains("gpt-5.6-luna") {
        return ReasoningSpec::new(
            &["low", "medium", "high", "xhigh", "max"],
            "medium",
            true,
            "OpenAI GPT-5.6 Luna",
            "OpenAI GPT-5.6 Luna",
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

pub fn is_oauth_fallback_model(cfg: &RouterConfig, candidate: &ModelConfig) -> bool {
    if !cfg.oauth_fallback.enabled || candidate.source == "oauth" {
        return false;
    }
    let canonical = canonical_route_model_id(&candidate.model);
    cfg.models.iter().any(|model| {
        model.source == "oauth"
            && cfg
                .oauth_account_ids
                .as_ref()
                .is_none_or(|ids| ids.contains(&model.oauth_account_id))
            && canonical_route_model_id(&model.model) == canonical
    })
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

pub fn detect_cc_switch_db() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".cc-switch").join("cc-switch.db"));
    }
    if let Some(config_dir) = dirs::config_dir() {
        candidates.extend([
            config_dir.join("com.ccswitch.desktop").join("cc-switch.db"),
            config_dir.join("CC Switch").join("cc-switch.db"),
            config_dir.join("cc-switch").join("cc-switch.db"),
        ]);
    }
    if let Some(data_dir) = dirs::data_local_dir() {
        candidates.extend([
            data_dir.join("com.ccswitch.desktop").join("cc-switch.db"),
            data_dir.join("CC Switch").join("cc-switch.db"),
            data_dir.join("cc-switch").join("cc-switch.db"),
        ]);
    }
    if let Some(custom_home) = std::env::var_os("CC_SWITCH_HOME") {
        candidates.insert(0, PathBuf::from(custom_home).join("cc-switch.db"));
    }
    candidates.into_iter().find(|path| path.is_file())
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
                "display_name": if model.alias.is_empty() { &model.model } else { &model.alias },
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
                "web_search_tool_type": "text_and_image",
                "truncation_policy": { "mode": "tokens", "limit": 10_000 },
                "supports_parallel_tool_calls": true,
                "comp_hash": "codex-router-v1",
                "effective_context_window_percent": model.auto_compact_percent,
                "experimental_supported_tools": Vec::<String>::new(),
                "supports_search_tool": true,
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
    let config_path = router_root.join("codex-router-config.json");
    let catalog_path = router_root.join("config").join("model-catalog.json");
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
            read_router_credential(&model.credential_name)
                .context("ROUTER_PROFILE_CREDENTIAL_READ_FAILED")?
                .filter(|value| !value.0.is_empty())
                .context("ROUTER_PROFILE_CREDENTIAL_MISSING")?
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
    let script = format!(
        "$ErrorActionPreference='Stop'\nAdd-Type -AssemblyName System.Security\n$data=[IO.File]::ReadAllBytes({})\n$protected=[Security.Cryptography.ProtectedData]::Protect($data,$null,[Security.Cryptography.DataProtectionScope]::CurrentUser)\n[IO.File]::WriteAllBytes({},$protected)\n[Array]::Clear($data,0,$data.Length)\n[Array]::Clear($protected,0,$protected.Length)\n'protected'\n",
        ps_literal(&source.to_string_lossy()),
        ps_literal(&destination.to_string_lossy()),
    );
    let _ = run_powershell_stdin(&script)?;
    Ok(())
}

pub fn unprotect_file_for_current_user(source: &Path, destination: &Path) -> anyhow::Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let script = format!(
        "$ErrorActionPreference='Stop'\nAdd-Type -AssemblyName System.Security\n$protected=[IO.File]::ReadAllBytes({})\n$data=[Security.Cryptography.ProtectedData]::Unprotect($protected,$null,[Security.Cryptography.DataProtectionScope]::CurrentUser)\n[IO.File]::WriteAllBytes({},$data)\n[Array]::Clear($protected,0,$protected.Length)\n[Array]::Clear($data,0,$data.Length)\n'unprotected'\n",
        ps_literal(&source.to_string_lossy()),
        ps_literal(&destination.to_string_lossy()),
    );
    let _ = run_powershell_stdin(&script)?;
    Ok(())
}

const CC_SWITCH_BACKUP_MAGIC: &[u8; 8] = b"CRCCBKP1";
const CC_SWITCH_BACKUP_CHUNK_BYTES: usize = 1024 * 1024;
const CC_SWITCH_BACKUP_MAX_FILES: usize = 2;
const CC_SWITCH_BACKUP_MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;

fn dpapi_protect_chunk(data: &[u8]) -> io::Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "DPAPI chunk is too large"))?,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let succeeded = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    let protected = unsafe {
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData.cast());
        bytes
    };
    Ok(protected)
}

#[cfg(test)]
fn dpapi_unprotect_chunk(data: &[u8]) -> io::Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "DPAPI chunk is too large"))?,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let succeeded = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    let plaintext = unsafe {
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData.cast());
        bytes
    };
    Ok(plaintext)
}

struct DpapiChunkWriter<W: Write> {
    inner: W,
    pending: Vec<u8>,
}

impl<W: Write> DpapiChunkWriter<W> {
    fn new(mut inner: W) -> io::Result<Self> {
        inner.write_all(CC_SWITCH_BACKUP_MAGIC)?;
        Ok(Self {
            inner,
            pending: Vec::with_capacity(CC_SWITCH_BACKUP_CHUNK_BYTES),
        })
    }

    fn flush_pending(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let protected = dpapi_protect_chunk(&self.pending)?;
        let length = u32::try_from(protected.len()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "protected DPAPI chunk is too large",
            )
        })?;
        self.inner.write_all(&length.to_le_bytes())?;
        self.inner.write_all(&protected)?;
        self.pending.fill(0);
        self.pending.clear();
        Ok(())
    }

    fn finish(mut self) -> io::Result<W> {
        self.flush_pending()?;
        self.inner.write_all(&0_u32.to_le_bytes())?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for DpapiChunkWriter<W> {
    fn write(&mut self, mut input: &[u8]) -> io::Result<usize> {
        let input_len = input.len();
        while !input.is_empty() {
            let available = CC_SWITCH_BACKUP_CHUNK_BYTES - self.pending.len();
            let copied = available.min(input.len());
            self.pending.extend_from_slice(&input[..copied]);
            input = &input[copied..];
            if self.pending.len() == CC_SWITCH_BACKUP_CHUNK_BYTES {
                self.flush_pending()?;
            }
        }
        Ok(input_len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_pending()?;
        self.inner.flush()
    }
}

#[cfg(test)]
struct DpapiChunkReader<R: Read> {
    inner: R,
    plaintext: Vec<u8>,
    offset: usize,
    finished: bool,
}

#[cfg(test)]
impl<R: Read> DpapiChunkReader<R> {
    fn new(mut inner: R) -> io::Result<Self> {
        let mut magic = [0_u8; CC_SWITCH_BACKUP_MAGIC.len()];
        inner.read_exact(&mut magic)?;
        if &magic != CC_SWITCH_BACKUP_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported CC Switch backup format",
            ));
        }
        Ok(Self {
            inner,
            plaintext: Vec::new(),
            offset: 0,
            finished: false,
        })
    }

    fn read_next_chunk(&mut self) -> io::Result<()> {
        let mut encoded_length = [0_u8; 4];
        self.inner.read_exact(&mut encoded_length)?;
        let length = u32::from_le_bytes(encoded_length) as usize;
        if length == 0 {
            self.finished = true;
            self.plaintext.clear();
            self.offset = 0;
            return Ok(());
        }
        if length > CC_SWITCH_BACKUP_CHUNK_BYTES * 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "encrypted CC Switch backup chunk exceeds the format limit",
            ));
        }
        let mut protected = vec![0_u8; length];
        self.inner.read_exact(&mut protected)?;
        self.plaintext = dpapi_unprotect_chunk(&protected)?;
        self.offset = 0;
        Ok(())
    }
}

#[cfg(test)]
impl<R: Read> Read for DpapiChunkReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        while self.offset == self.plaintext.len() && !self.finished {
            self.plaintext.fill(0);
            self.read_next_chunk()?;
        }
        if self.finished {
            return Ok(0);
        }
        let copied = output.len().min(self.plaintext.len() - self.offset);
        output[..copied].copy_from_slice(&self.plaintext[self.offset..self.offset + copied]);
        self.offset += copied;
        Ok(copied)
    }
}

fn protect_compressed_file_for_current_user(
    source: &Path,
    destination: &Path,
) -> anyhow::Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let result = (|| -> anyhow::Result<()> {
        let mut input = BufReader::new(File::open(source)?);
        let output = BufWriter::new(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)?,
        );
        let protected = DpapiChunkWriter::new(output)?;
        let mut compressed = GzEncoder::new(protected, Compression::default());
        io::copy(&mut input, &mut compressed)?;
        let protected = compressed.finish()?;
        let _ = protected.finish()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(destination);
    }
    result
}

#[cfg(test)]
fn unprotect_compressed_file_for_current_user(
    source: &Path,
    destination: &Path,
) -> anyhow::Result<()> {
    let input = BufReader::new(File::open(source)?);
    let protected = DpapiChunkReader::new(input)?;
    let mut decompressed = GzDecoder::new(protected);
    let mut output = BufWriter::new(File::create(destination)?);
    io::copy(&mut decompressed, &mut output)?;
    output.flush()?;
    Ok(())
}

fn limit_cc_switch_backups(backup_dir: &Path) -> anyhow::Result<()> {
    let mut backups = std::fs::read_dir(backup_dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            if !entry.file_type().ok()?.is_file()
                || !name.starts_with("cc-switch-before-codex-router-")
                || !name.ends_with(".dpapi")
            {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            Some((path, metadata.len()))
        })
        .collect::<Vec<_>>();
    backups.sort_by(|left, right| right.0.file_name().cmp(&left.0.file_name()));
    let mut retained_bytes = 0_u64;
    for (index, (path, bytes)) in backups.into_iter().enumerate() {
        let keep = index == 0
            || (index < CC_SWITCH_BACKUP_MAX_FILES
                && retained_bytes.saturating_add(bytes) <= CC_SWITCH_BACKUP_MAX_TOTAL_BYTES);
        if keep {
            retained_bytes = retained_bytes.saturating_add(bytes);
        } else {
            std::fs::remove_file(&path)
                .with_context(|| format!("无法清理旧 CC Switch 备份 {}", path.display()))?;
        }
    }
    Ok(())
}

fn create_cc_switch_backup(db_path: &Path, backup_dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(backup_dir)?;
    let backup_stem = format!(
        "cc-switch-before-codex-router-{}-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S-%6f"),
        std::process::id()
    );
    let backup_plain = backup_dir.join(format!("{backup_stem}.db.tmp"));
    let backup = backup_dir.join(format!("{backup_stem}.db.gz.dpapi"));
    let result = (|| -> anyhow::Result<()> {
        let backup_connection = rusqlite::Connection::open(db_path)?;
        let backup_sql = format!(
            "VACUUM INTO '{}'",
            backup_plain.to_string_lossy().replace('\'', "''")
        );
        backup_connection
            .execute_batch(&backup_sql)
            .with_context(|| format!("无法创建 CC Switch 临时备份 {}", backup_plain.display()))?;
        protect_compressed_file_for_current_user(&backup_plain, &backup).with_context(|| {
            format!(
                "无法使用当前 Windows 用户保护 CC Switch 备份 {}",
                backup.display()
            )
        })?;
        Ok(())
    })();
    let cleanup = std::fs::remove_file(&backup_plain);
    if let Err(error) = result {
        let _ = std::fs::remove_file(&backup);
        return Err(error);
    }
    if let Err(error) = cleanup {
        let _ = std::fs::remove_file(&backup);
        return Err(error).with_context(|| {
            format!("无法清理 CC Switch 明文临时备份 {}", backup_plain.display())
        });
    }
    Ok(backup)
}

pub fn run_apply_script<F>(router_root: &Path, mut on_line: F) -> anyhow::Result<()>
where
    F: FnMut(String),
{
    fn safe_output_line(line: &str) -> String {
        for prefix in [
            "[1/7]", "[2/7]", "[3/7]", "[4/7]", "[5/7]", "[6/7]", "[7/7]",
        ] {
            if line.starts_with(prefix) {
                return prefix.to_owned();
            }
        }
        format!(
            "deployment_diagnostic {}",
            crate::runtime_logs::summarize_error_for_display(line)
        )
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
            if stdout_tx.send((false, line)).is_err() {
                break;
            }
        }
    });
    let stderr_reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
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

    let _ = stdout_reader.join();
    let _ = stderr_reader.join();
    while let Ok((is_error, line)) = line_rx.try_recv() {
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

pub(crate) fn codex_config_uses_router(config_text: &str, router_base_url: &str) -> bool {
    let Ok(document) = config_text.parse::<DocumentMut>() else {
        return false;
    };
    let Some(provider_id) = document
        .get("model_provider")
        .and_then(Item::as_str)
        .filter(|value| matches!(*value, "custom" | "sub2api"))
    else {
        return false;
    };
    let expected_base = format!("{}/v1", router_base_url.trim_end_matches('/'));
    document
        .get("model_providers")
        .and_then(Item::as_table_like)
        .and_then(|providers| providers.get(provider_id))
        .and_then(Item::as_table_like)
        .and_then(|provider| provider.get("base_url"))
        .and_then(Item::as_str)
        .is_some_and(|value| value.trim_end_matches('/') == expected_base)
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

pub fn codex_router_mode_active(cfg: &RouterConfig) -> bool {
    let config_path = resolve_codex_home(cfg).join("config.toml");
    let Ok(config_text) = std::fs::read_to_string(config_path) else {
        return false;
    };
    let router_base_url = cfg.deploy.sub2api_host.trim();
    codex_config_uses_router(&config_text, router_base_url)
        && local_router_health_available(router_base_url)
}

pub fn load_oauth_accounts(router_root: &Path) -> anyhow::Result<Vec<crate::OAuthAccountSummary>> {
    let script = router_root.join("scripts").join("Get-OAuthAccounts.ps1");
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
        .with_context(|| format!("无法读取 OAuth 账号: {}", script.display()))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        bail!(
            "无法读取 OAuth 账号: {}",
            crate::runtime_logs::summarize_error_for_display(&message)
        );
    }
    let text = String::from_utf8(output.stdout).context("OAuth 账号清单不是 UTF-8")?;
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == "null" {
        return Ok(Vec::new());
    }
    if trimmed.starts_with('[') {
        return serde_json::from_str(trimmed).context("无法解析 OAuth 账号清单");
    }
    let account = serde_json::from_str(trimmed).context("无法解析 OAuth 账号清单")?;
    Ok(vec![account])
}

pub fn load_usage_snapshot(
    router_root: &Path,
    profile_name: &str,
    cfg: &RouterConfig,
) -> anyhow::Result<crate::UsageSnapshot> {
    let script = router_root.join("scripts").join("Get-UsageMonitor.ps1");
    let state_dir = router_root.join("data").join("ui");
    std::fs::create_dir_all(&state_dir)?;
    let config_snapshot = state_dir.join(format!(
        "usage-monitor-config-{}.tmp.json",
        std::process::id()
    ));
    cfg.save(&config_snapshot)?;
    let result = (|| -> anyhow::Result<crate::UsageSnapshot> {
        let mut last_failure = "class=unclassified_error".to_owned();
        for attempt in 0..2 {
            let output_result = Command::new("powershell.exe")
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
                .creation_flags(0x08000000)
                .output();

            match output_result {
                Ok(output) if output.status.success() => {
                    let text = String::from_utf8(output.stdout)
                        .map_err(|_| anyhow::anyhow!("class=invalid_response"))?;
                    let trimmed = text.trim().trim_start_matches('\u{feff}');
                    if trimmed.is_empty() || trimmed == "null" {
                        last_failure = "class=empty_response".to_owned();
                    } else {
                        match serde_json::from_str(trimmed) {
                            Ok(snapshot) => return Ok(snapshot),
                            Err(_) => last_failure = "class=invalid_response".to_owned(),
                        }
                    }
                }
                Ok(output) => last_failure = usage_process_failure_summary(&output),
                Err(error) => {
                    last_failure = crate::runtime_logs::summarize_error_for_display(&format!(
                        "usage monitor process start failed: {error}"
                    ));
                }
            }

            if attempt == 0 {
                std::thread::sleep(Duration::from_millis(350));
            }
        }
        bail!(last_failure)
    })();
    let _ = std::fs::remove_file(&config_snapshot);
    result
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

fn resolve_cc_switch_db(cfg: &RouterConfig) -> anyhow::Result<PathBuf> {
    let db_path = if cfg.deploy.cc_switch_db.trim().is_empty() {
        detect_cc_switch_db()
            .context("未检测到 CC Switch；请先安装并运行一次，或关闭 CC Switch 隔离配置")?
    } else {
        Path::new(&cfg.deploy.cc_switch_db).to_path_buf()
    };
    if !db_path.is_file() {
        bail!("未找到 CC Switch 数据库: {}", db_path.display());
    }
    Ok(db_path)
}

/// Router profiles must never inherit the elevated Windows sandbox setting.
/// That setting starts Codex's UAC-backed installer on every launch; on
/// machines where the default profile is protected, the installer loops
/// forever after the user accepts the prompt. Keep the profile self-contained
/// by normalizing only the exact `[windows] sandbox` key.
pub fn normalize_windows_sandbox_config(text: &str) -> String {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let had_trailing_newline = text.ends_with('\n') || text.ends_with('\r');
    let mut lines = Vec::new();
    let mut in_windows = false;
    let mut found_windows = false;
    let mut found_sandbox = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_windows && !found_sandbox {
                lines.push("sandbox = \"unelevated\"".to_owned());
                found_sandbox = true;
            }
            in_windows = trimmed == "[windows]";
            if in_windows {
                found_windows = true;
                found_sandbox = false;
            }
        }

        if in_windows {
            let key = trimmed.split('=').next().unwrap_or_default().trim();
            if key == "sandbox" {
                if !found_sandbox {
                    lines.push("sandbox = \"unelevated\"".to_owned());
                    found_sandbox = true;
                }
                continue;
            }
        }
        lines.push(line.to_owned());
    }

    if in_windows && !found_sandbox {
        lines.push("sandbox = \"unelevated\"".to_owned());
        // no further state is needed after appending the missing key
    }
    if !found_windows {
        if !lines.is_empty() && !lines.last().is_some_and(|line| line.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push("[windows]".to_owned());
        lines.push("sandbox = \"unelevated\"".to_owned());
    }

    let mut result = lines.join(newline);
    if had_trailing_newline && !result.ends_with(newline) {
        result.push_str(newline);
    }
    result
}

/// CC Switch injects this temporary provider into the live config while its
/// proxy is active. Persisting it back into a Router-managed profile makes the
/// next switch depend on port 15721 and can strand existing streams.
pub fn strip_cc_switch_proxy_provider(text: &str) -> String {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing_newline = text.ends_with('\n');
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with('[') || blocks.is_empty() {
            blocks.push(Vec::new());
        }
        if let Some(block) = blocks.last_mut() {
            block.push(line);
        }
    }
    let mut kept = Vec::new();
    for block in blocks {
        let joined = block.join("\n");
        let injected_proxy = block
            .first()
            .is_some_and(|line| line.trim() == "[model_providers.custom]")
            && joined.contains("127.0.0.1:15721/v1")
            && joined.contains("experimental_bearer_token = \"PROXY_MANAGED\"");
        if !injected_proxy {
            kept.extend(block);
        }
    }
    let mut result = kept.join(newline);
    while result.contains(&format!("{newline}{newline}{newline}")) {
        result = result.replace(
            &format!("{newline}{newline}{newline}"),
            &format!("{newline}{newline}"),
        );
    }
    if trailing_newline && !result.ends_with(newline) {
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

fn read_chatgpt_auth(cfg: &RouterConfig) -> anyhow::Result<serde_json::Value> {
    let auth_path = resolve_codex_home(cfg).join("auth.json");
    let auth: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&auth_path)
            .with_context(|| format!("无法读取当前 Codex 登录状态: {}", auth_path.display()))?,
    )
    .with_context(|| format!("Codex 登录状态不是有效 JSON: {}", auth_path.display()))?;
    let is_chatgpt = auth.get("auth_mode").and_then(|value| value.as_str()) == Some("chatgpt")
        && auth.get("tokens").is_some_and(|value| value.is_object());
    if !is_chatgpt {
        bail!("当前 auth.json 不是完整的 ChatGPT OAuth 登录状态；为防止 Windows 设置循环，已停止写入 CC Switch");
    }
    Ok(auth)
}

const CODEX_DESKTOP_REASONING_EFFORTS: &[&str] =
    &["low", "medium", "high", "xhigh", "ultra", "max"];

fn codex_config_has_managed_router_marker(config_text: &str) -> bool {
    let Ok(document) = config_text.parse::<DocumentMut>() else {
        return false;
    };
    let Some(provider_id) = document
        .get("model_provider")
        .and_then(Item::as_str)
        .filter(|value| matches!(*value, "custom" | "sub2api"))
    else {
        return false;
    };
    let Some(provider) = document
        .get("model_providers")
        .and_then(Item::as_table_like)
        .and_then(|providers| providers.get(provider_id))
        .and_then(Item::as_table_like)
    else {
        return false;
    };
    if provider.get("name").and_then(Item::as_str) != Some("Codex-Router") {
        return false;
    }
    provider
        .get("base_url")
        .and_then(Item::as_str)
        .and_then(|value| url::Url::parse(value).ok())
        .is_some_and(|url| {
            url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
                && url.path().trim_end_matches('/') == "/v1"
        })
}

fn ensure_codex_desktop_reasoning_efforts(config_text: &str) -> anyhow::Result<String> {
    let mut document = config_text
        .parse::<DocumentMut>()
        .context("Codex config.toml 不是有效 TOML")?;
    if document.get("desktop").is_none() {
        document.insert("desktop", Item::Table(Table::new()));
    }
    let desktop = document
        .get_mut("desktop")
        .and_then(Item::as_table_mut)
        .context("Codex [desktop] 必须是 TOML 表")?;
    let mut efforts = Array::new();
    for effort in CODEX_DESKTOP_REASONING_EFFORTS {
        efforts.push(*effort);
    }
    desktop.insert(
        "enabled-reasoning-efforts",
        Item::Value(toml_edit::Value::Array(efforts)),
    );
    Ok(document.to_string())
}

fn validate_cc_switch_router_config(cfg: &RouterConfig, config_text: &str) -> anyhow::Result<()> {
    let document = config_text
        .parse::<DocumentMut>()
        .context("待同步到 CC Switch 的 Codex config.toml 不是有效 TOML")?;
    if document.get("model_provider").and_then(Item::as_str) != Some("custom") {
        bail!("Codex-Router 必须使用共享的 custom Provider，已停止写入 CC Switch");
    }
    let provider = document
        .get("model_providers")
        .and_then(Item::as_table_like)
        .and_then(|providers| providers.get("custom"))
        .and_then(Item::as_table_like)
        .context("Codex-Router 缺少 [model_providers.custom]")?;
    let router_base = if cfg.deploy.sub2api_host.trim().is_empty() {
        "http://127.0.0.1:18080"
    } else {
        cfg.deploy.sub2api_host.trim()
    };
    let expected_base = format!("{}/v1", router_base.trim_end_matches('/'));
    if provider.get("base_url").and_then(Item::as_str) != Some(expected_base.as_str()) {
        bail!("Codex-Router 的 custom Provider 未指向本机 Sub2API: {expected_base}");
    }
    if provider.get("requires_openai_auth").and_then(Item::as_bool) != Some(true) {
        bail!("Codex-Router 的 custom Provider 必须保留 OpenAI 登录模式");
    }
    if !provider
        .get("experimental_bearer_token")
        .and_then(Item::as_str)
        .is_some_and(|value| !value.trim().is_empty())
    {
        bail!("Codex-Router 缺少本地 Bearer；请重新应用配置后再同步 CC Switch");
    }
    let catalog_path = document
        .get("model_catalog_json")
        .and_then(Item::as_str)
        .context("Codex-Router 缺少 model_catalog_json，模型菜单将无法加载")?;
    if !Path::new(catalog_path).is_file() {
        bail!("Codex-Router 模型目录不存在: {catalog_path}");
    }
    Ok(())
}

struct PendingCcSwitchSettings {
    path: PathBuf,
    original: Vec<u8>,
    updated: Vec<u8>,
}

fn prepare_cc_switch_settings_update(
    db_path: &Path,
    profile_id: &str,
) -> anyhow::Result<Option<PendingCcSwitchSettings>> {
    let path = db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("settings.json");
    let original = std::fs::read(&path)
        .with_context(|| format!("无法读取 CC Switch 设置: {}", path.display()))?;
    let mut settings: serde_json::Value = serde_json::from_slice(&original)
        .with_context(|| format!("CC Switch settings.json 不是有效 JSON: {}", path.display()))?;
    let object = settings
        .as_object_mut()
        .context("CC Switch settings.json 必须是 JSON 对象")?;
    object.insert("preserveCodexOfficialAuthOnSwitch".into(), json!(true));
    object.insert("unifyCodexSessionHistory".into(), json!(true));
    object.insert("unifyCodexMigrateExisting".into(), json!(true));
    object.insert("currentProviderCodex".into(), json!(profile_id));
    let mut updated = serde_json::to_vec_pretty(&settings)?;
    updated.push(b'\n');
    if serde_json::from_slice::<serde_json::Value>(&original)? == settings {
        return Ok(None);
    }
    Ok(Some(PendingCcSwitchSettings {
        path,
        original,
        updated,
    }))
}

fn managed_profile_matches(settings: &serde_json::Value, profile_id: &str) -> bool {
    let has_metadata = settings
        .get("codexRouter")
        .and_then(|value| value.as_object())
        .is_some_and(|managed| {
            managed.get("managed").and_then(|value| value.as_bool()) == Some(true)
                && managed.get("profileId").and_then(|value| value.as_str()) == Some(profile_id)
        });
    if has_metadata {
        return true;
    }

    // CC-Switch currently rewrites settings_config when a provider is
    // selected and may discard unknown metadata fields. Reclaim only a
    // Router-generated ID whose config still has the Router provider markers;
    // arbitrary user profiles remain protected from overwrite.
    if !profile_id.starts_with("codex-router-") {
        return false;
    }
    settings
        .get("config")
        .and_then(|value| value.as_str())
        .is_some_and(codex_config_has_managed_router_marker)
}

pub fn ensure_cc_switch_profile_id(cfg: &mut RouterConfig) {
    if cfg.deploy.cc_switch_profile_id.trim().is_empty() {
        cfg.deploy.cc_switch_profile_id = format!(
            "codex-router-{}-{}",
            chrono::Utc::now().timestamp_millis(),
            std::process::id()
        );
    }
}

pub fn validate_cc_switch_target(cfg: &RouterConfig) -> anyhow::Result<()> {
    use rusqlite::{Connection, OptionalExtension};
    let name = cfg.deploy.cc_switch_profile_name.trim();
    let profile_id = cfg.deploy.cc_switch_profile_id.trim();
    if name.is_empty() {
        bail!("请为新建的 CC Switch 配置输入名称");
    }
    if profile_id.is_empty() {
        bail!("CC Switch 配置 ID 尚未生成");
    }
    let db_path = resolve_cc_switch_db(cfg)?;
    let connection = Connection::open(&db_path)?;
    let name_conflict: Option<String> = connection
        .query_row(
            "SELECT id FROM providers WHERE app_type = 'codex' AND name = ?1 AND id <> ?2 LIMIT 1",
            [name, profile_id],
            |row| row.get(0),
        )
        .optional()?;
    if name_conflict.is_some() {
        bail!("CC Switch 中已存在名为“{name}”的 Codex 配置；请换一个名称，原配置不会被覆盖");
    }
    let existing_settings: Option<String> = connection
        .query_row(
            "SELECT settings_config FROM providers WHERE app_type = 'codex' AND id = ?1",
            [profile_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(raw) = existing_settings {
        let settings: serde_json::Value = serde_json::from_str(&raw)
            .context("CC Switch 中已有同 ID 配置，但其内容不是有效 JSON")?;
        if !managed_profile_matches(&settings, profile_id) {
            bail!("CC Switch 中已有同 ID 的非 Router 配置；为避免覆盖，已停止同步");
        }
    }
    let _ = read_chatgpt_auth(cfg)?;
    Ok(())
}

fn codex_auth_account_id(auth: &serde_json::Value) -> Option<String> {
    let tokens = auth.get("tokens");
    [
        tokens.and_then(|item| item.get("account_id")),
        tokens.and_then(|item| item.get("accountId")),
        auth.get("account_id"),
        auth.get("accountId"),
    ]
    .into_iter()
    .flatten()
    .find_map(|item| {
        item.as_str()
            .map(str::to_owned)
            .or_else(|| item.as_i64().map(|number| number.to_string()))
            .or_else(|| item.as_u64().map(|number| number.to_string()))
    })
    .filter(|item| !item.trim().is_empty())
}

pub fn sync_cc_switch(cfg: &RouterConfig, share_codex_state: bool) -> anyhow::Result<()> {
    use rusqlite::{params, Connection, OptionalExtension};
    validate_cc_switch_target(cfg)?;
    let db_path = resolve_cc_switch_db(cfg)?;
    let profile_name = cfg.deploy.cc_switch_profile_name.trim();
    let profile_id = cfg.deploy.cc_switch_profile_id.trim();
    let codex_home = resolve_codex_home(cfg);
    let config_path = codex_home.join("config.toml");
    let original_config_text = std::fs::read_to_string(&config_path)?;
    let normalized =
        strip_cc_switch_proxy_provider(&normalize_windows_sandbox_config(&original_config_text));
    let config_text = ensure_codex_desktop_reasoning_efforts(&normalized)?;
    validate_cc_switch_router_config(cfg, &config_text)?;
    if config_text != original_config_text {
        atomic_write(&config_path, config_text.as_bytes())?;
    }
    let auth = read_chatgpt_auth(cfg)?;
    let backup_dir = db_path.parent().unwrap_or(Path::new(".")).join("backups");
    let mut connection = Connection::open(&db_path)?;
    let has_category = {
        let mut statement = connection.prepare("PRAGMA table_info(providers)")?;
        let found = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(Result::ok)
            .any(|column| column == "category");
        found
    };
    let existing: Option<String> = connection
        .query_row(
            "SELECT settings_config FROM providers WHERE app_type = 'codex' AND id = ?1",
            [profile_id],
            |row| row.get(0),
        )
        .optional()?;
    let mut settings = match existing.as_deref() {
        Some(raw) => serde_json::from_str(raw)?,
        None => json!({}),
    };
    let object = settings
        .as_object_mut()
        .context("CC Switch Provider 配置必须是 JSON 对象")?;
    object.insert("auth".into(), auth);
    object.insert("config".into(), json!(config_text));
    object.insert(
        "codexRouter".into(),
        json!({"managed": true, "profileId": profile_id, "version": 1}),
    );
    let settings_text = serde_json::to_string(&settings)?;
    let pending_cc_settings = prepare_cc_switch_settings_update(&db_path, profile_id)?;
    let transaction = connection.transaction()?;
    let mut changed_rows = 0_usize;
    if existing.is_some() {
        changed_rows += if has_category {
            transaction.execute(
                "UPDATE providers SET name = ?1, settings_config = ?2, category = 'third_party' WHERE id = ?3 AND app_type = 'codex' AND (name IS NOT ?1 OR settings_config IS NOT ?2 OR category IS NOT 'third_party')",
                params![profile_name, settings_text, profile_id],
            )?
        } else {
            transaction.execute(
                "UPDATE providers SET name = ?1, settings_config = ?2 WHERE id = ?3 AND app_type = 'codex' AND (name IS NOT ?1 OR settings_config IS NOT ?2)",
                params![profile_name, settings_text, profile_id],
            )?
        };
    } else {
        changed_rows += if has_category {
            transaction.execute(
                "INSERT INTO providers (id, app_type, name, settings_config, category) VALUES (?1, 'codex', ?2, ?3, 'third_party')",
                params![profile_id, profile_name, settings_text],
            )?
        } else {
            transaction.execute(
                "INSERT INTO providers (id, app_type, name, settings_config) VALUES (?1, 'codex', ?2, ?3)",
                params![profile_id, profile_name, settings_text],
            )?
        };
    }

    if share_codex_state {
        let current_account = codex_auth_account_id(
            settings
                .get("auth")
                .context("Router 的 CC Switch 配置缺少 Codex 登录状态")?,
        );
        let mut statement = transaction.prepare(
            "SELECT id, settings_config FROM providers WHERE app_type = 'codex' AND id <> ?1",
        )?;
        let managed = statement
            .query_map([profile_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (managed_id, raw) in managed {
            let mut managed_settings: serde_json::Value = match serde_json::from_str(&raw) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if !managed_profile_matches(&managed_settings, &managed_id) {
                continue;
            }
            let managed_account = managed_settings.get("auth").and_then(codex_auth_account_id);
            if current_account.is_some()
                && managed_account.is_some()
                && current_account != managed_account
            {
                continue;
            }
            let Some(saved_route) = managed_settings
                .get("config")
                .and_then(|item| item.as_str())
            else {
                continue;
            };
            let merged = crate::profiles::merge_codex_route_config(&config_text, saved_route)?;
            let object = managed_settings
                .as_object_mut()
                .context("CC Switch Router 配置必须是 JSON 对象")?;
            object.insert("auth".into(), settings["auth"].clone());
            object.insert("config".into(), json!(merged));
            let managed_settings_text = serde_json::to_string(&managed_settings)?;
            if managed_settings_text != raw {
                changed_rows += transaction.execute(
                    "UPDATE providers SET settings_config = ?1 WHERE id = ?2 AND app_type = 'codex'",
                    params![managed_settings_text, managed_id],
                )?;
            }
        }
    }

    // The Router profile has already been written to the live Codex home by
    // the apply workflow. Keep CC Switch's view of the active provider in the
    // same transaction so its UI cannot later restore a stale provider over
    // the working local configuration. Older CC Switch releases only use
    // `is_current`; current releases also store the provider selection inside
    // the active configuration-group payload.
    changed_rows += transaction.execute(
        "UPDATE providers SET is_current = CASE WHEN id = ?1 THEN 1 ELSE 0 END WHERE app_type = 'codex' AND COALESCE(is_current, 0) <> CASE WHEN id = ?1 THEN 1 ELSE 0 END",
        [profile_id],
    )?;

    let active_group_id: Option<String> = transaction
        .query_row(
            "SELECT value FROM settings WHERE key = 'current_profile_id_codex' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap_or(None);
    if let Some(group_id) = active_group_id.filter(|value| !value.trim().is_empty()) {
        let group_payload: Option<String> = transaction
            .query_row(
                "SELECT payload FROM profiles WHERE id = ?1 LIMIT 1",
                [&group_id],
                |row| row.get(0),
            )
            .optional()
            .unwrap_or(None);
        if let Some(raw_payload) = group_payload {
            let mut payload: serde_json::Value = serde_json::from_str(&raw_payload)
                .context("CC Switch 当前配置分组不是有效 JSON，已停止切换")?;
            let providers = payload
                .as_object_mut()
                .context("CC Switch 当前配置分组必须是 JSON 对象")?
                .entry("providers")
                .or_insert_with(|| json!({}));
            providers
                .as_object_mut()
                .context("CC Switch 当前配置分组的 providers 字段必须是 JSON 对象")?
                .insert("codex".into(), json!(profile_id));
            let payload_text = serde_json::to_string(&payload)?;
            if payload_text != raw_payload {
                changed_rows += transaction.execute(
                    "UPDATE profiles SET payload = ?1 WHERE id = ?2",
                    params![payload_text, group_id],
                )?;
            }
        }
    }
    if changed_rows > 0 {
        let _backup = create_cc_switch_backup(&db_path, &backup_dir)?;
        limit_cc_switch_backups(&backup_dir)?;
    }
    if let Some(update) = &pending_cc_settings {
        atomic_write(&update.path, &update.updated)?;
    }
    if let Err(error) = transaction.commit() {
        if let Some(update) = &pending_cc_settings {
            let _ = atomic_write(&update.path, &update.original);
        }
        return Err(error.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn router_mode_requires_the_selected_provider_and_matching_local_url() {
        let config = r#"model_provider = "custom"
model = "gpt-5.6-sol"

[model_providers.custom]
name = "Codex-Router"
base_url = "http://127.0.0.1:18080/v1"
"#;
        assert!(codex_config_uses_router(config, "http://127.0.0.1:18080"));
        assert!(!codex_config_uses_router(config, "http://127.0.0.1:19090"));
        assert!(!codex_config_uses_router(
            &config.replace("model_provider = \"custom\"", "model_provider = \"openai\""),
            "http://127.0.0.1:18080"
        ));

        let legacy = config
            .replace(
                "model_provider = \"custom\"",
                "model_provider = \"sub2api\"",
            )
            .replace("model_providers.custom", "model_providers.sub2api");
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

    fn cc_switch_router_config(root: &Path, model: &str, settings: &str) -> String {
        let catalog_path = root
            .join("model-catalog.json")
            .display()
            .to_string()
            .replace('\\', "\\\\");
        format!(
            "model_provider = \"custom\"\nmodel = \"{model}\"\nmodel_catalog_json = \"{catalog_path}\"\n{settings}\n[windows]\nsandbox = \"unelevated\"\n\n[model_providers.custom]\nname = \"Codex-Router\"\nbase_url = \"http://127.0.0.1:18080/v1\"\nrequires_openai_auth = true\nexperimental_bearer_token = \"sk-local-test\"\n"
        )
    }

    fn cc_switch_fixture(root: &Path, profile_name: &str) -> RouterConfig {
        let codex_home = root.join("codex-home");
        std::fs::create_dir_all(&codex_home).unwrap();
        let catalog_path = root.join("model-catalog.json");
        std::fs::write(&catalog_path, r#"{"models":[]}"#).unwrap();
        std::fs::write(
            codex_home.join("config.toml"),
            cc_switch_router_config(root, "kimi-for-coding", ""),
        )
        .unwrap();
        std::fs::write(
            codex_home.join("auth.json"),
            serde_json::to_vec(&json!({
                "auth_mode": "chatgpt",
                "OPENAI_API_KEY": null,
                "tokens": {"access_token": "oauth-secret"},
                "last_refresh": "now"
            }))
            .unwrap(),
        )
        .unwrap();
        let db_path = root.join("cc-switch.db");
        let connection = Connection::open(&db_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE providers (
                    id TEXT NOT NULL,
                    app_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    settings_config TEXT NOT NULL,
                    category TEXT,
                    is_current BOOLEAN NOT NULL DEFAULT 0,
                    PRIMARY KEY (id, app_type)
                );
                INSERT INTO providers (id, app_type, name, settings_config, is_current)
                VALUES ('existing-user-profile', 'codex', 'Existing profile',
                        '{\"untouched\":true}', 1);
                CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);
                CREATE TABLE profiles (id TEXT PRIMARY KEY, payload TEXT NOT NULL);
                INSERT INTO settings (key, value)
                VALUES ('current_profile_id_codex', 'active-group');
                INSERT INTO profiles (id, payload)
                VALUES ('active-group',
                        '{\"providers\":{\"codex\":\"existing-user-profile\"},\"keep\":true}');",
            )
            .unwrap();
        drop(connection);
        std::fs::write(
            root.join("settings.json"),
            r#"{"preserveCodexOfficialAuthOnSwitch":false,"unifyCodexSessionHistory":false,"unifyCodexMigrateExisting":false,"currentProviderCodex":"existing-user-profile","keep":true}"#,
        )
        .unwrap();

        let mut config = RouterConfig::default();
        config.deploy.codex_home = codex_home.display().to_string();
        config.deploy.cc_switch_db = db_path.display().to_string();
        config.deploy.cc_switch_sync = true;
        config.deploy.cc_switch_profile_name = profile_name.to_string();
        config.deploy.cc_switch_profile_id = "codex-router-generated-profile".to_string();
        config
    }

    #[test]
    fn channel_presets_fill_recommended_fields_without_overwriting_the_key() {
        let mut model = ModelConfig {
            api_key: "keep-secret".into(),
            credential_name: "SavedCredential".into(),
            extra: r#"{"old":true}"#.into(),
            ..Default::default()
        };
        assert!(apply_channel_preset(&mut model, "chiral"));
        assert_eq!(model.base_url, "https://api.430123.xyz/v1");
        assert_eq!(model.model, "gpt-5.6-sol");
        assert_eq!(model.alias, "ChatGPT-5.6-Sol");
        assert_eq!(model.api_key, "keep-secret");
        assert_eq!(model.credential_name, "SavedCredential");
        assert_eq!(model.context_window, 0);
        assert_eq!(model.auto_compact_percent, 80);
        assert_eq!(model.reasoning_mode, "auto");
        assert_eq!(model.extra, "{}");
        assert!(!apply_channel_preset(&mut model, "unknown"));
    }

    #[test]
    fn channel_presets_keep_current_supported_model_defaults() {
        let expected = [
            ("chiral", "gpt-5.6-sol"),
            ("opencode-go", "gpt-5.6-luna"),
            ("openai", "gpt-5.6-sol"),
            ("anthropic", "claude-opus-5"),
            ("openrouter", "openai/gpt-5.6-sol"),
            ("kimi", "k3-256k"),
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
        assert!(
            !std::fs::read_to_string(root.join("codex-router-config.json"))
                .unwrap()
                .contains("must-not-be-written")
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
        let input = "model = \"test\"\r\n\r\n[windows]\r\nsandbox = \"elevated\"\r\n";
        let output = normalize_windows_sandbox_config(input);
        assert!(output.contains("[windows]\r\nsandbox = \"unelevated\"\r\n"));
        assert!(!output.contains("sandbox = \"elevated\""));
    }

    #[test]
    fn windows_sandbox_normalization_adds_missing_table_without_touching_other_keys() {
        let input = "model_provider = \"sub2api\"\n\nsandbox_mode = \"danger-full-access\"\n";
        let output = normalize_windows_sandbox_config(input);
        assert!(output.contains("sandbox_mode = \"danger-full-access\""));
        assert!(output.ends_with("[windows]\nsandbox = \"unelevated\"\n"));
    }

    #[test]
    fn cc_switch_temporary_proxy_is_not_persisted_in_router_profile() {
        let input = r#"model_provider = "sub2api"

[model_providers.custom]
name = "CC Switch Proxy"
base_url = "http://127.0.0.1:15721/v1"
experimental_bearer_token = "PROXY_MANAGED"

[model_providers.sub2api]
name = "Codex-Router"
base_url = "http://127.0.0.1:18080/v1"
"#;
        let output = strip_cc_switch_proxy_provider(input);
        assert!(!output.contains("15721"));
        assert!(!output.contains("PROXY_MANAGED"));
        assert!(output.contains("model_provider = \"sub2api\""));
        assert!(output.contains("127.0.0.1:18080/v1"));

        let unrelated = r#"[model_providers.custom]
base_url = "https://example.invalid/v1"
"#;
        assert_eq!(strip_cc_switch_proxy_provider(unrelated), unrelated);
    }

    #[test]
    fn cc_switch_sync_creates_a_new_profile_and_preserves_chatgpt_auth() {
        let root = temporary_test_dir("cc-switch-create");
        let config = cc_switch_fixture(&root, "My Router recovery");

        sync_cc_switch(&config, false).unwrap();

        let connection = Connection::open(&config.deploy.cc_switch_db).unwrap();
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM providers WHERE app_type = 'codex'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
        let existing: (String, i64) = connection
            .query_row(
                "SELECT settings_config, is_current FROM providers WHERE id = 'existing-user-profile'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(existing, ("{\"untouched\":true}".to_string(), 0));
        let created: (String, String, i64, String) = connection
            .query_row(
                "SELECT name, settings_config, is_current, category FROM providers WHERE id = 'codex-router-generated-profile'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(created.0, "My Router recovery");
        assert_eq!(created.2, 1);
        assert_eq!(created.3, "third_party");
        let active_group: String = connection
            .query_row(
                "SELECT payload FROM profiles WHERE id = 'active-group'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let active_group: serde_json::Value = serde_json::from_str(&active_group).unwrap();
        assert_eq!(
            active_group["providers"]["codex"],
            "codex-router-generated-profile"
        );
        assert_eq!(active_group["keep"], true);
        let settings: serde_json::Value = serde_json::from_str(&created.1).unwrap();
        assert_eq!(settings["auth"]["auth_mode"], "chatgpt");
        assert!(settings["auth"]["tokens"].is_object());
        assert!(settings["auth"]["OPENAI_API_KEY"].is_null());
        assert_eq!(settings["codexRouter"]["managed"], true);
        assert_eq!(
            settings["codexRouter"]["profileId"],
            "codex-router-generated-profile"
        );
        assert!(settings["config"]
            .as_str()
            .unwrap()
            .contains("sandbox = \"unelevated\""));
        let saved_config = settings["config"].as_str().unwrap();
        assert!(saved_config.contains("model_provider = \"custom\""));
        assert!(saved_config.contains("experimental_bearer_token = \"sk-local-test\""));
        assert!(saved_config.contains(
            "enabled-reasoning-efforts = [\"low\", \"medium\", \"high\", \"xhigh\", \"ultra\", \"max\"]"
        ));
        drop(connection);
        let cc_settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(cc_settings["preserveCodexOfficialAuthOnSwitch"], true);
        assert_eq!(cc_settings["unifyCodexSessionHistory"], true);
        assert_eq!(cc_settings["unifyCodexMigrateExisting"], true);
        assert_eq!(
            cc_settings["currentProviderCodex"],
            "codex-router-generated-profile"
        );
        assert_eq!(cc_settings["keep"], true);
        let backups = std::fs::read_dir(root.join("backups"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            backups[0].extension().and_then(|value| value.to_str()),
            Some("dpapi")
        );
        assert!(backups[0]
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.ends_with(".db.gz.dpapi")));
        let restored_backup = root.join("restored-cc-switch.db");
        unprotect_compressed_file_for_current_user(&backups[0], &restored_backup).unwrap();
        let restored_connection = Connection::open(&restored_backup).unwrap();
        let restored_provider_count: i64 = restored_connection
            .query_row("SELECT COUNT(*) FROM providers", [], |row| row.get(0))
            .unwrap();
        assert_eq!(restored_provider_count, 1);
        drop(restored_connection);
        std::fs::remove_file(restored_backup).unwrap();

        sync_cc_switch(&config, false).unwrap();
        let unchanged_backups = std::fs::read_dir(root.join("backups"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(unchanged_backups.len(), 1);

        // CC-Switch may strip unknown Router metadata while rewriting a
        // selected provider. The generated ID and provider markers still
        // identify this profile safely on the next sync.
        let connection = Connection::open(&config.deploy.cc_switch_db).unwrap();
        connection
            .execute(
                "UPDATE providers SET settings_config = json_remove(settings_config, '$.codexRouter') WHERE id = 'codex-router-generated-profile'",
                [],
            )
            .unwrap();
        drop(connection);

        std::fs::write(
            root.join("codex-home").join("config.toml"),
            cc_switch_router_config(&root, "updated-model", ""),
        )
        .unwrap();
        sync_cc_switch(&config, false).unwrap();
        let connection = Connection::open(&config.deploy.cc_switch_db).unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM providers", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
        let current: i64 = connection
            .query_row(
                "SELECT is_current FROM providers WHERE id = 'existing-user-profile'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(current, 0);
        drop(connection);

        std::fs::write(
            root.join("codex-home").join("config.toml"),
            cc_switch_router_config(&root, "third-model", ""),
        )
        .unwrap();
        sync_cc_switch(&config, false).unwrap();
        let retained_backups = std::fs::read_dir(root.join("backups"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(retained_backups.len(), CC_SWITCH_BACKUP_MAX_FILES);
        assert!(retained_backups.iter().all(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.ends_with(".db.gz.dpapi"))
        }));
        let retained_bytes = retained_backups
            .iter()
            .map(|path| std::fs::metadata(path).unwrap().len())
            .sum::<u64>();
        assert!(retained_bytes <= CC_SWITCH_BACKUP_MAX_TOTAL_BYTES);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cc_switch_shared_profiles_refresh_settings_only_for_the_same_codex_account() {
        let root = temporary_test_dir("cc-switch-shared-state");
        let first = cc_switch_fixture(&root, "First Router profile");
        let codex_home = PathBuf::from(&first.deploy.codex_home);
        std::fs::write(
            codex_home.join("config.toml"),
            cc_switch_router_config(&root, "first-route", "approval_policy = \"on-request\"\n"),
        )
        .unwrap();
        std::fs::write(
            codex_home.join("auth.json"),
            serde_json::to_vec(&json!({
                "auth_mode": "chatgpt",
                "tokens": {"account_id": "account-a", "access_token": "first"}
            }))
            .unwrap(),
        )
        .unwrap();
        sync_cc_switch(&first, false).unwrap();

        let mut second = first.clone();
        second.deploy.cc_switch_profile_id = "codex-router-second-profile".into();
        second.deploy.cc_switch_profile_name = "Second Router profile".into();
        std::fs::write(
            codex_home.join("config.toml"),
            cc_switch_router_config(
                &root,
                "second-route",
                "approval_policy = \"never\"\npersonality = \"pragmatic\"\n",
            ),
        )
        .unwrap();
        std::fs::write(
            codex_home.join("auth.json"),
            serde_json::to_vec(&json!({
                "auth_mode": "chatgpt",
                "tokens": {"account_id": "account-a", "access_token": "refreshed"}
            }))
            .unwrap(),
        )
        .unwrap();
        sync_cc_switch(&second, true).unwrap();

        let connection = Connection::open(&first.deploy.cc_switch_db).unwrap();
        let first_settings: String = connection
            .query_row(
                "SELECT settings_config FROM providers WHERE id = ?1",
                [&first.deploy.cc_switch_profile_id],
                |row| row.get(0),
            )
            .unwrap();
        drop(connection);
        let first_settings: serde_json::Value = serde_json::from_str(&first_settings).unwrap();
        let first_config = first_settings["config"].as_str().unwrap();
        assert!(first_config.contains("model = \"first-route\""));
        assert!(first_config.contains("approval_policy = \"never\""));
        assert!(first_config.contains("personality = \"pragmatic\""));
        assert_eq!(
            first_settings["auth"]["tokens"]["access_token"],
            "refreshed"
        );

        let mut third = first.clone();
        third.deploy.cc_switch_profile_id = "codex-router-third-profile".into();
        third.deploy.cc_switch_profile_name = "Different account profile".into();
        std::fs::write(
            codex_home.join("config.toml"),
            cc_switch_router_config(&root, "third-route", "approval_policy = \"full-access\"\n"),
        )
        .unwrap();
        std::fs::write(
            codex_home.join("auth.json"),
            serde_json::to_vec(&json!({
                "auth_mode": "chatgpt",
                "tokens": {"account_id": "account-b", "access_token": "other-account"}
            }))
            .unwrap(),
        )
        .unwrap();
        sync_cc_switch(&third, true).unwrap();

        let connection = Connection::open(&first.deploy.cc_switch_db).unwrap();
        let isolated_settings: String = connection
            .query_row(
                "SELECT settings_config FROM providers WHERE id = ?1",
                [&first.deploy.cc_switch_profile_id],
                |row| row.get(0),
            )
            .unwrap();
        drop(connection);
        let isolated_settings: serde_json::Value =
            serde_json::from_str(&isolated_settings).unwrap();
        assert!(isolated_settings["config"]
            .as_str()
            .unwrap()
            .contains("approval_policy = \"never\""));
        assert_eq!(
            isolated_settings["auth"]["tokens"]["access_token"],
            "refreshed"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cc_switch_sync_rejects_a_duplicate_name_without_a_backup() {
        let root = temporary_test_dir("cc-switch-name-conflict");
        let config = cc_switch_fixture(&root, "Existing profile");

        let error = validate_cc_switch_target(&config).unwrap_err().to_string();

        assert!(error.contains("已存在名为"));
        assert!(!root.join("backups").exists());
        let connection = Connection::open(&config.deploy.cc_switch_db).unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM providers", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        drop(connection);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cc_switch_sync_rejects_api_key_only_auth() {
        let root = temporary_test_dir("cc-switch-invalid-auth");
        let config = cc_switch_fixture(&root, "Safe profile");
        std::fs::write(
            root.join("codex-home").join("auth.json"),
            r#"{"auth_mode":"apikey","OPENAI_API_KEY":"must-not-copy"}"#,
        )
        .unwrap();

        let error = validate_cc_switch_target(&config).unwrap_err().to_string();

        assert!(error.contains("不是完整的 ChatGPT OAuth"));
        assert!(!root.join("backups").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
