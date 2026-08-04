use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;

pub(crate) fn atomic_write(path: &std::path::Path, content: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("codex-router-config");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(".{file_name}.{}.{nonce}.tmp", std::process::id()));

    let result = (|| -> anyhow::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(windows)]
fn replace_file(source: &std::path::Path, destination: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(source: &std::path::Path, destination: &std::path::Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseBehavior {
    #[default]
    Ask,
    MinimizeToTray,
    Exit,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiPreferences {
    #[serde(default)]
    pub close_behavior: CloseBehavior,
    #[serde(default)]
    pub close_warning_version: u8,
    #[serde(default)]
    pub active_profile_id: String,
    #[serde(default)]
    pub monitor_subscription_order: Vec<i64>,
    #[serde(default)]
    pub monitor_api_order: Vec<i64>,
    #[serde(default = "default_true")]
    pub share_codex_state: bool,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            close_behavior: CloseBehavior::default(),
            close_warning_version: 0,
            active_profile_id: String::new(),
            monitor_subscription_order: Vec::new(),
            monitor_api_order: Vec::new(),
            share_codex_state: true,
        }
    }
}

impl UiPreferences {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }

    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        atomic_write(path, serde_json::to_string_pretty(self)?.as_bytes())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployConfig {
    #[serde(default)]
    pub codex_home: String,
    #[serde(default = "default_sub2api_host")]
    pub sub2api_host: String,
    #[serde(default = "default_true")]
    pub generate_isolation: bool,
    #[serde(default = "default_true")]
    pub start_with_windows: bool,
}

fn default_sub2api_host() -> String {
    "http://127.0.0.1:18080".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for DeployConfig {
    fn default() -> Self {
        Self {
            codex_home: String::new(),
            sub2api_host: default_sub2api_host(),
            generate_isolation: true,
            start_with_windows: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthFallback {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(
        default = "default_true",
        rename = "preferOAuth",
        alias = "preferOauth"
    )]
    pub prefer_oauth: bool,
    #[serde(default)]
    pub official_priority: i32,
    #[serde(default = "default_100")]
    pub fallback_priority: i32,
}

fn default_100() -> i32 {
    100
}

impl Default for OAuthFallback {
    fn default() -> Self {
        Self {
            enabled: true,
            prefer_oauth: true,
            official_priority: 1,
            fallback_priority: 100,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningConfig {
    #[serde(default = "default_auto")]
    pub mode: String,
    #[serde(default)]
    pub levels: Vec<String>,
    #[serde(default)]
    pub default_level: String,
    #[serde(default)]
    pub supports_fast: bool,
}

fn default_auto() -> String {
    "auto".to_string()
}

impl Default for ReasoningConfig {
    fn default() -> Self {
        Self {
            mode: "auto".to_string(),
            levels: vec![],
            default_level: String::new(),
            supports_fast: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyConfig {
    /// New installations follow the current user's proxy automatically. This
    /// field intentionally uses serde's false default so an existing explicit
    /// proxy object keeps its previous direct/manual behavior after upgrade.
    #[serde(default)]
    pub auto_detect: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_http", alias = "type")]
    pub proxy_type: String,
    #[serde(default = "default_localhost")]
    pub host: String,
    #[serde(default = "default_7890")]
    pub port: String,
    #[serde(default)]
    pub username: String,
    /// Runtime-only input. The persisted value lives in Windows Credential Manager.
    #[serde(skip_serializing, default)]
    pub password: String,
    #[serde(default = "default_proxy_credential")]
    pub password_credential: String,
}

fn default_proxy_credential() -> String {
    "ProxyPassword".to_string()
}

fn default_http() -> String {
    "http".to_string()
}

fn default_localhost() -> String {
    "127.0.0.1".to_string()
}

fn default_7890() -> String {
    "7890".to_string()
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            auto_detect: true,
            enabled: false,
            proxy_type: "http".to_string(),
            host: "127.0.0.1".to_string(),
            port: "7890".to_string(),
            username: String::new(),
            password: String::new(),
            password_credential: default_proxy_credential(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfig {
    pub model: String,
    #[serde(default)]
    pub alias: String,
    /// `Some(true)` means the user edited the display name. `Some(false)` is
    /// an automatic recommendation that may adapt to OAuth merge state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias_customized: Option<bool>,
    #[serde(default)]
    pub base_url: String,
    /// Runtime-only input. API keys are never serialized to project files.
    #[serde(skip_serializing, default)]
    pub api_key: String,
    #[serde(default)]
    pub credential_name: String,
    #[serde(default = "default_10")]
    pub priority: i32,
    #[serde(default = "default_1")]
    pub weight: i32,
    #[serde(default = "default_empty_json")]
    pub extra: String,
    #[serde(default = "default_auto")]
    pub multimodal: String,
    /// Zero selects the documented default detected from the model id.
    #[serde(default)]
    pub context_window: i64,
    #[serde(default = "default_auto_compact_percent")]
    pub auto_compact_percent: i32,
    /// Per-model reasoning configuration. `auto` uses the documented preset.
    #[serde(default = "default_auto")]
    pub reasoning_mode: String,
    #[serde(default)]
    pub reasoning_levels: Vec<String>,
    #[serde(default)]
    pub default_reasoning_level: String,
    /// Manual-mode capability override; auto mode detects this from the model id.
    #[serde(default)]
    pub fast_supported: bool,
    /// Select Fast as the default service tier for new Codex tasks.
    #[serde(default)]
    pub fast_mode: bool,
    /// `apikey` creates a managed upstream channel; `oauth` references an
    /// account whose credentials remain inside Sub2API.
    #[serde(default = "default_apikey")]
    pub source: String,
    #[serde(default)]
    pub oauth_account_id: i64,
    #[serde(default)]
    pub oauth_platform: String,
}

fn default_10() -> i32 {
    10
}

fn default_1() -> i32 {
    1
}

fn default_empty_json() -> String {
    "{}".to_string()
}

fn default_apikey() -> String {
    "apikey".to_string()
}

fn default_auto_compact_percent() -> i32 {
    80
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            alias: String::new(),
            alias_customized: None,
            base_url: String::new(),
            api_key: String::new(),
            credential_name: String::new(),
            priority: 10,
            weight: 1,
            extra: "{}".to_string(),
            multimodal: "auto".to_string(),
            context_window: 0,
            auto_compact_percent: default_auto_compact_percent(),
            reasoning_mode: default_auto(),
            reasoning_levels: vec![],
            default_reasoning_level: String::new(),
            fast_supported: false,
            fast_mode: false,
            source: default_apikey(),
            oauth_account_id: 0,
            oauth_platform: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouterConfig {
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default = "default_oauth")]
    pub auth_mode: String,
    #[serde(default = "default_ui_theme")]
    pub ui_theme: String,
    #[serde(default)]
    pub accept_compliance: bool,
    #[serde(default)]
    pub accepted_terms_version: String,
    /// Runtime-only compatibility field. The generated local key is kept in
    /// Windows Credential Manager and only copied into local integrations.
    #[serde(skip_serializing, default)]
    pub local_api_key: String,
    #[serde(default)]
    pub deploy: DeployConfig,
    #[serde(default)]
    pub oauth_fallback: OAuthFallback,
    /// `None` preserves legacy Sub2API bindings until the user first opens the
    /// OAuth manager. `Some([])` intentionally disables OAuth for this profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_account_ids: Option<Vec<i64>>,
    /// Accounts already considered for automatic profile enrollment. Keeping
    /// this separate lets a user disable an account without it being enabled
    /// again on the next refresh.
    #[serde(default)]
    pub oauth_seen_account_ids: Vec<i64>,
    /// Per canonical OAuth model, the stable API-key channel keys selected as
    /// fallbacks. A missing model key keeps automatic same-name matching;
    /// an explicit empty list disables API fallback for that model.
    #[serde(default)]
    pub fallback_channel_selections: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub reasoning: ReasoningConfig,
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub models: Vec<ModelConfig>,
    #[serde(default)]
    pub default_model: String,
    #[serde(default)]
    pub model_catalog: Vec<serde_json::Value>,
}

fn default_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn default_oauth() -> String {
    "chatgpt_oauth".to_string()
}

fn default_ui_theme() -> String {
    "sky".to_string()
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            auth_mode: "chatgpt_oauth".to_string(),
            ui_theme: default_ui_theme(),
            accept_compliance: false,
            accepted_terms_version: String::new(),
            local_api_key: String::new(),
            deploy: DeployConfig::default(),
            oauth_fallback: OAuthFallback::default(),
            oauth_account_ids: None,
            oauth_seen_account_ids: Vec::new(),
            fallback_channel_selections: BTreeMap::new(),
            reasoning: ReasoningConfig::default(),
            proxy: ProxyConfig::default(),
            models: vec![],
            default_model: String::new(),
            model_catalog: vec![],
        }
    }
}

impl RouterConfig {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let mut cfg: Self = serde_json::from_str(&content)?;
        for model in &mut cfg.models {
            if model.model.eq_ignore_ascii_case("gpt-5.6") {
                model.model = "gpt-5.6-sol".to_owned();
                if model.alias.eq_ignore_ascii_case("gpt-5.6")
                    || model.alias.eq_ignore_ascii_case("gpt-5.6 (sol)")
                {
                    model.alias = "ChatGPT-5.6-Sol".to_owned();
                }
            }
        }
        if cfg.default_model.eq_ignore_ascii_case("gpt-5.6") {
            cfg.default_model = "gpt-5.6-sol".to_owned();
        }
        Ok(cfg)
    }

    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        atomic_write(path, content.as_bytes())
    }

    pub fn find_router_root() -> std::path::PathBuf {
        if let Ok(exe) = std::env::current_exe() {
            let exe_dir = exe.parent().unwrap_or(std::path::Path::new("."));
            if exe_dir.join("scripts").join("Start-Router.ps1").exists() {
                return exe_dir.to_path_buf();
            }
            let parent = exe_dir.parent().unwrap_or(exe_dir);
            if parent.join("scripts").join("Start-Router.ps1").exists() {
                return parent.to_path_buf();
            }
            return exe_dir.to_path_buf();
        }
        std::path::PathBuf::from(".")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_never_serialized() {
        let mut config = RouterConfig {
            local_api_key: "sk-local-secret".into(),
            ..RouterConfig::default()
        };
        config.proxy.password = "proxy-secret".into();
        let model = ModelConfig {
            model: "test-model".into(),
            api_key: "sk-provider-secret".into(),
            credential_name: "ModelApiKey-test".into(),
            ..ModelConfig::default()
        };
        config.models.push(model);
        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("sk-local-secret"));
        assert!(!json.contains("proxy-secret"));
        assert!(!json.contains("sk-provider-secret"));
        assert!(json.contains("ModelApiKey-test"));
    }

    #[test]
    fn legacy_proxy_type_is_accepted() {
        let config: RouterConfig = serde_json::from_str(r#"{"proxy":{"type":"socks5"}}"#).unwrap();
        assert_eq!(config.proxy.proxy_type, "socks5");
        assert!(!config.proxy.auto_detect);
    }

    #[test]
    fn new_configs_auto_detect_proxy_without_changing_legacy_proxy_objects() {
        assert!(RouterConfig::default().proxy.auto_detect);
        let legacy: RouterConfig = serde_json::from_str(r#"{"proxy":{"enabled":false}}"#).unwrap();
        assert!(!legacy.proxy.auto_detect);
    }

    #[test]
    fn sky_is_the_default_theme() {
        assert_eq!(RouterConfig::default().ui_theme, "sky");
        let config: RouterConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config.ui_theme, "sky");
    }

    #[test]
    fn deploy_defaults_enable_start_with_windows() {
        let config: RouterConfig = serde_json::from_str(r#"{"deploy":{}}"#).unwrap();
        assert!(config.deploy.start_with_windows);
    }

    #[test]
    fn oauth_routing_preference_defaults_to_oauth_for_legacy_configs() {
        let legacy: RouterConfig = serde_json::from_str(
            r#"{"oauthFallback":{"enabled":true,"officialPriority":1,"fallbackPriority":100}}"#,
        )
        .unwrap();
        assert!(legacy.oauth_fallback.prefer_oauth);

        let mut api_first = RouterConfig::default();
        api_first.oauth_fallback.prefer_oauth = false;
        let json = serde_json::to_string(&api_first).unwrap();
        assert!(json.contains(r#""preferOAuth":false"#));
        let restored: RouterConfig = serde_json::from_str(&json).unwrap();
        assert!(!restored.oauth_fallback.prefer_oauth);
    }

    #[test]
    fn fallback_channel_selections_preserve_automatic_and_explicit_empty_states() {
        let legacy: RouterConfig = serde_json::from_str("{}").unwrap();
        assert!(legacy.fallback_channel_selections.is_empty());

        let configured: RouterConfig = serde_json::from_str(
            r#"{"fallbackChannelSelections":{"gpt-5.6-sol":[],"gpt-5.6-luna":["gpt-5.6-luna|https://api.example/v1"]}}"#,
        )
        .unwrap();
        assert_eq!(
            configured.fallback_channel_selections.get("gpt-5.6-sol"),
            Some(&Vec::new())
        );
        assert_eq!(
            configured.fallback_channel_selections.get("gpt-5.6-luna"),
            Some(&vec!["gpt-5.6-luna|https://api.example/v1".to_owned()])
        );
    }

    #[test]
    fn legacy_model_gets_conservative_compaction_defaults() {
        let model: ModelConfig = serde_json::from_str(r#"{"model":"legacy"}"#).unwrap();
        assert_eq!(model.context_window, 0);
        assert_eq!(model.auto_compact_percent, 80);
        assert_eq!(model.reasoning_mode, "auto");
        assert!(model.reasoning_levels.is_empty());
        assert!(!model.fast_mode);
    }

    #[test]
    fn legacy_bare_gpt_56_is_migrated_to_sol_on_load() {
        let path =
            std::env::temp_dir().join(format!("codex-router-gpt56-{}.json", std::process::id()));
        std::fs::write(
            &path,
            r#"{"models":[{"model":"gpt-5.6","alias":"GPT-5.6 (Sol)"}],"defaultModel":"gpt-5.6"}"#,
        )
        .unwrap();
        let config = RouterConfig::load(&path).unwrap();
        assert_eq!(config.models[0].model, "gpt-5.6-sol");
        assert_eq!(config.models[0].alias, "ChatGPT-5.6-Sol");
        assert_eq!(config.default_model, "gpt-5.6-sol");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn per_model_reasoning_settings_round_trip() {
        let model = ModelConfig {
            model: "custom-reasoner".into(),
            reasoning_mode: "manual".into(),
            reasoning_levels: vec!["low".into(), "xhigh".into()],
            default_reasoning_level: "xhigh".into(),
            fast_supported: true,
            fast_mode: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&model).unwrap();
        let restored: ModelConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.reasoning_mode, "manual");
        assert_eq!(restored.reasoning_levels, vec!["low", "xhigh"]);
        assert_eq!(restored.default_reasoning_level, "xhigh");
        assert!(restored.fast_supported);
        assert!(restored.fast_mode);
    }

    #[test]
    fn oauth_models_and_profile_account_selection_round_trip_without_tokens() {
        let mut config = RouterConfig {
            oauth_account_ids: Some(vec![7, 11]),
            oauth_seen_account_ids: vec![7, 11],
            ..Default::default()
        };
        config.models.push(ModelConfig {
            model: "gpt-5.6-sol".into(),
            source: "oauth".into(),
            oauth_account_id: 7,
            oauth_platform: "openai".into(),
            ..Default::default()
        });
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains(r#""oauthAccountIds":[7,11]"#));
        assert!(json.contains(r#""source":"oauth""#));
        assert!(!json.contains("access_token"));
        let restored: RouterConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.oauth_account_ids, Some(vec![7, 11]));
        assert_eq!(restored.oauth_seen_account_ids, vec![7, 11]);
        assert_eq!(restored.models[0].oauth_account_id, 7);
    }

    #[test]
    fn default_model_round_trips_without_breaking_legacy_configs() {
        let legacy: RouterConfig = serde_json::from_str(r#"{"models":[]}"#).unwrap();
        assert!(legacy.default_model.is_empty());

        let config = RouterConfig {
            default_model: "gpt-5.6-sol".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains(r#""defaultModel":"gpt-5.6-sol""#));
    }

    #[test]
    fn ui_close_behavior_defaults_to_ask_and_round_trips() {
        let legacy: UiPreferences = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.close_behavior, CloseBehavior::Ask);
        assert_eq!(legacy.close_warning_version, 0);
        assert!(legacy.share_codex_state);

        let preferences = UiPreferences {
            close_behavior: CloseBehavior::MinimizeToTray,
            close_warning_version: 1,
            active_profile_id: "local-test".into(),
            monitor_subscription_order: vec![8, 3],
            monitor_api_order: vec![4, 1],
            share_codex_state: false,
        };
        let json = serde_json::to_string(&preferences).unwrap();
        assert!(json.contains(r#""closeBehavior":"minimize_to_tray""#));
        let restored: UiPreferences = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.close_behavior, CloseBehavior::MinimizeToTray);
        assert_eq!(restored.close_warning_version, 1);
        assert_eq!(restored.active_profile_id, "local-test");
        assert_eq!(restored.monitor_subscription_order, vec![8, 3]);
        assert_eq!(restored.monitor_api_order, vec![4, 1]);
        assert!(!restored.share_codex_state);
    }

    #[test]
    fn atomic_write_replaces_existing_file_without_leaving_temporary_state() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "codex-router-atomic-write-{}-{}",
            std::process::id(),
            nonce
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("config.json");
        atomic_write(&path, b"first").unwrap();
        atomic_write(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
}
