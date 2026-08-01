use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployConfig {
    #[serde(default)]
    pub codex_home: String,
    #[serde(default = "default_sub2api_host")]
    pub sub2api_host: String,
    #[serde(default)]
    pub cc_switch_db: String,
    #[serde(default = "default_true")]
    pub generate_isolation: bool,
    #[serde(default)]
    pub cc_switch_sync: bool,
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
            cc_switch_db: String::new(),
            generate_isolation: true,
            cc_switch_sync: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthFallback {
    #[serde(default = "default_true")]
    pub enabled: bool,
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

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            alias: String::new(),
            base_url: String::new(),
            api_key: String::new(),
            credential_name: String::new(),
            priority: 10,
            weight: 1,
            extra: "{}".to_string(),
            multimodal: "auto".to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcSwitchProviderSettings {
    pub model_provider: String,
    pub model: String,
    pub api_url: String,
    pub api_key: String,
    pub requires_openai_auth: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcSwitchProvider {
    pub id: String,
    pub name: String,
    pub app_type: String,
    pub settings: CcSwitchProviderSettings,
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
    /// Windows Credential Manager and only copied into Codex/CC Switch locally.
    #[serde(skip_serializing, default)]
    pub local_api_key: String,
    #[serde(default)]
    pub deploy: DeployConfig,
    #[serde(default)]
    pub oauth_fallback: OAuthFallback,
    #[serde(default)]
    pub reasoning: ReasoningConfig,
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub models: Vec<ModelConfig>,
    #[serde(default)]
    pub model_catalog: Vec<serde_json::Value>,
    #[serde(default)]
    pub cc_switch_providers: Vec<CcSwitchProvider>,
}

fn default_version() -> String {
    "0.3.0".to_string()
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
            version: "0.3.0".to_string(),
            auth_mode: "chatgpt_oauth".to_string(),
            ui_theme: default_ui_theme(),
            accept_compliance: false,
            accepted_terms_version: String::new(),
            local_api_key: String::new(),
            deploy: DeployConfig::default(),
            oauth_fallback: OAuthFallback::default(),
            reasoning: ReasoningConfig::default(),
            proxy: ProxyConfig::default(),
            models: vec![],
            model_catalog: vec![],
            cc_switch_providers: vec![],
        }
    }
}

impl RouterConfig {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let cfg: Self = serde_json::from_str(&content)?;
        Ok(cfg)
    }

    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
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
        let mut config = RouterConfig::default();
        config.local_api_key = "sk-local-secret".into();
        config.proxy.password = "proxy-secret".into();
        let mut model = ModelConfig::default();
        model.model = "test-model".into();
        model.api_key = "sk-provider-secret".into();
        model.credential_name = "ModelApiKey-test".into();
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
    }

    #[test]
    fn sky_is_the_default_theme() {
        assert_eq!(RouterConfig::default().ui_theme, "sky");
        let config: RouterConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config.ui_theme, "sky");
    }
}
