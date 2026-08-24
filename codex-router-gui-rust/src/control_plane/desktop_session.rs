//! Desktop ChatGPT session protection and diagnostics.
//!
//! Codex Desktop 26.818.61809 `getAuthStatus` calls `Refreshing token` when
//! the active Router provider has `requires_openai_auth=true`. Concurrent
//! heartbeats invalidate the refresh token (HTTP 401 `refresh_token_invalidated`)
//! and Desktop upgrades that to `account/login/start`.
//!
//! Router requests already use `experimental_bearer_token`. The only legal
//! provider identity is `name=Codex-Router` + `requires_openai_auth=false`.
//! These events are written to `router-events.jsonl` with the codes below so
//! a live instance can be audited without reading secrets:
//!
//! - `CR-DSK-0001` user `config.toml` written
//! - `CR-DSK-0002` user `config.toml` skipped (identical bytes)
//! - `CR-DSK-0003` CLI replica actually written
//! - `CR-DSK-0004` session heartbeat (mtimes, identity, replica counts)
//! - `CR-DSK-0005` ChatGPT usage observational (no live refresh)
//! - `CR-DSK-0006` upstream 401 shielded to 503
//! - `CR-DSK-0007` desktop overlay written
//! - `CR-DSK-0008` desktop overlay skipped
//! - `CR-DSK-0009` system-layer `config.toml` written
//! - `CR-DSK-0010` system-layer `config.toml` skipped
//! - `CR-DSK-0011` illegal provider identity detected
//! - `CR-DSK-0012` illegal provider identity repaired in place
//! - `CR-DSK-0013` home `config.toml` lost the Router provider (Desktop strip)
//! - `CR-DSK-0014` home provider restored from the legal system layer
//! - `CR-DSK-0015` missing system layer recreated from a legal home provider

use crate::control_plane::http_compat::{ControlState, ReplicaSyncStats};
use crate::telemetry::structured_log;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item};

pub const DSK_CONFIG_WRITTEN: &str = "CR-DSK-0001";
pub const DSK_CONFIG_SKIPPED: &str = "CR-DSK-0002";
pub const DSK_REPLICA_WRITTEN: &str = "CR-DSK-0003";
pub const DSK_HEARTBEAT: &str = "CR-DSK-0004";
pub const DSK_USAGE_OBSERVATIONAL: &str = "CR-DSK-0005";
pub const DSK_UPSTREAM_401_SHIELDED: &str = "CR-DSK-0006";
pub const DSK_OVERLAY_WRITTEN: &str = "CR-DSK-0007";
pub const DSK_OVERLAY_SKIPPED: &str = "CR-DSK-0008";
pub const DSK_SYSTEM_WRITTEN: &str = "CR-DSK-0009";
pub const DSK_SYSTEM_SKIPPED: &str = "CR-DSK-0010";
pub const DSK_IDENTITY_ILLEGAL: &str = "CR-DSK-0011";
pub const DSK_IDENTITY_REPAIRED: &str = "CR-DSK-0012";
pub const DSK_HOME_MISSING: &str = "CR-DSK-0013";
pub const DSK_HOME_RESTORED: &str = "CR-DSK-0014";
pub const DSK_SYSTEM_CREATED: &str = "CR-DSK-0015";
const SYSTEM_BINDING_MARKER: &str = "# codex-router-managed: binding layer";

pub const LEGAL_PROVIDER_NAME: &str = "Codex-Router";
pub const LEGAL_REQUIRES_OPENAI_AUTH: bool = false;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderIdentity {
    pub present: bool,
    pub name: Option<String>,
    pub requires_openai_auth: Option<bool>,
}

impl ProviderIdentity {
    pub fn absent() -> Self {
        Self {
            present: false,
            name: None,
            requires_openai_auth: None,
        }
    }

    pub fn is_legal(&self) -> bool {
        self.present
            && self.name.as_deref() == Some(LEGAL_PROVIDER_NAME)
            && self.requires_openai_auth == Some(LEGAL_REQUIRES_OPENAI_AUTH)
    }
}

pub fn legal_provider_identity() -> (bool, bool) {
    (LEGAL_REQUIRES_OPENAI_AUTH, false)
}

pub fn system_codex_config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("CODEX_ROUTER_SYSTEM_CONFIG") {
        return PathBuf::from(path);
    }
    let program_data = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    program_data
        .join("OpenAI")
        .join("Codex")
        .join("config.toml")
}

pub fn read_provider_identity(text: &str) -> ProviderIdentity {
    let Ok(document) = text.parse::<DocumentMut>() else {
        return ProviderIdentity::absent();
    };
    let Some(provider) = document
        .get("model_providers")
        .and_then(|item| item.get("codex_router"))
        .and_then(Item::as_table_like)
    else {
        return ProviderIdentity::absent();
    };
    ProviderIdentity {
        present: true,
        name: provider
            .get("name")
            .and_then(Item::as_str)
            .map(str::to_owned),
        requires_openai_auth: provider.get("requires_openai_auth").and_then(Item::as_bool),
    }
}

pub fn read_provider_identity_file(path: &Path) -> ProviderIdentity {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| read_provider_identity(&text))
        .unwrap_or_else(ProviderIdentity::absent)
}

/// Rewrite only `name` and `requires_openai_auth` when the Router provider is
/// present and illegal. Returns `None` when the file is already legal or has
/// no Router provider, so callers must not touch the file.
pub fn patched_provider_identity(text: &str) -> Option<String> {
    let mut document: DocumentMut = text.parse().ok()?;
    let provider = document
        .get_mut("model_providers")?
        .as_table_like_mut()?
        .get_mut("codex_router")?
        .as_table_like_mut()?;
    let name = provider.get("name").and_then(Item::as_str).unwrap_or("");
    let requires = provider.get("requires_openai_auth").and_then(Item::as_bool);
    if name == LEGAL_PROVIDER_NAME && requires == Some(LEGAL_REQUIRES_OPENAI_AUTH) {
        return None;
    }
    provider.insert("name", toml_edit::value(LEGAL_PROVIDER_NAME));
    provider.insert(
        "requires_openai_auth",
        toml_edit::value(LEGAL_REQUIRES_OPENAI_AUTH),
    );
    let mut text = document.to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    Some(text)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityRepair {
    Missing,
    AlreadyLegal,
    Repaired,
    Failed,
}

pub fn repair_provider_identity_file(path: &Path) -> IdentityRepair {
    let Ok(existing) = std::fs::read_to_string(path) else {
        return IdentityRepair::Missing;
    };
    let Some(patched) = patched_provider_identity(&existing) else {
        return if read_provider_identity(&existing).present {
            IdentityRepair::AlreadyLegal
        } else {
            IdentityRepair::Missing
        };
    };
    if std::fs::write(path, patched.as_bytes()).is_err() {
        return IdentityRepair::Failed;
    }
    IdentityRepair::Repaired
}

/// Desktop periodically strips `[model_providers.codex_router]` from the user
/// file (`model = "first"`, empty providers). Routing then falls through to
/// `%ProgramData%`. Copy the legal system provider back once; later ticks
/// must not keep rewriting the watched user file.
pub fn restore_home_provider_from_system(home: &str, system: &str) -> Option<String> {
    if read_provider_identity(home).present {
        return None;
    }
    if !read_provider_identity(system).present {
        return None;
    }
    let mut home_doc: DocumentMut = home.parse().ok()?;
    let system_doc: DocumentMut = system.parse().ok()?;
    if let Some(item) = system_doc.get("model_provider") {
        home_doc.insert("model_provider", item.clone());
    } else {
        home_doc.insert("model_provider", toml_edit::value("codex_router"));
    }
    if let Some(item) = system_doc.get("model_catalog_json") {
        home_doc.insert("model_catalog_json", item.clone());
    }
    let system_provider = system_doc
        .get("model_providers")?
        .get("codex_router")?
        .as_table()?
        .clone();
    let providers =
        home_doc["model_providers"].or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    providers.as_table_mut()?["codex_router"] = toml_edit::Item::Table(system_provider);
    let mut text = home_doc.to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    Some(patched_provider_identity(&text).unwrap_or(text))
}

/// Recreate a missing system-layer file from a legal home provider so Desktop
/// still has a `requires_openai_auth=false` fallback after Router exit or a
/// ProgramData wipe.
pub fn restore_system_from_home(home: &str) -> Option<String> {
    if !read_provider_identity(home).present {
        return None;
    }
    let home_doc: DocumentMut = home.parse().ok()?;
    let provider = home_doc
        .get("model_providers")?
        .get("codex_router")?
        .as_table()?
        .clone();
    let mut document = DocumentMut::new();
    document.insert("model_provider", toml_edit::value("codex_router"));
    if let Some(catalog) = home_doc.get("model_catalog_json") {
        document.insert("model_catalog_json", catalog.clone());
    }
    let providers = document["model_providers"]
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    providers.as_table_mut()?["codex_router"] = toml_edit::Item::Table(provider);
    let mut text = format!("{SYSTEM_BINDING_MARKER}\n{}", document);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    Some(patched_provider_identity(&text).unwrap_or(text))
}

fn file_mtime_rfc3339(path: &Path) -> Option<String> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let datetime: chrono::DateTime<chrono::Utc> = modified.into();
    Some(datetime.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
}

fn identity_value(identity: &ProviderIdentity) -> Value {
    json!({
        "present": identity.present,
        "name": identity.name,
        "requires_openai_auth": identity.requires_openai_auth,
        "legal": identity.is_legal(),
    })
}

pub fn protect_desktop_session(
    state: &ControlState,
    replica_stats: ReplicaSyncStats,
    repair_user: bool,
) {
    let Some(backend) = state.backend.as_ref() else {
        return;
    };
    let user_auth = backend.desktop_auth_path.as_path();
    let user_config = user_auth
        .parent()
        .map(|parent| parent.join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("config.toml"));
    let system_config = system_codex_config_path();
    protect_desktop_session_at(
        state,
        &user_config,
        user_auth,
        &system_config,
        replica_stats,
        repair_user,
    );
}

pub fn protect_desktop_session_at(
    state: &ControlState,
    user_config: &Path,
    user_auth: &Path,
    system_config: &Path,
    replica_stats: ReplicaSyncStats,
    repair_user: bool,
) {
    let system_identity = read_provider_identity_file(system_config);
    if system_identity.present && !system_identity.is_legal() {
        let _ = state.logger.write(json!({
            "level": "WARN",
            "event": "desktop.session.identity_illegal",
            "error_code": DSK_IDENTITY_ILLEGAL,
            "layer": "system",
            "name": system_identity.name,
            "requires_openai_auth": system_identity.requires_openai_auth,
            "timestamp": structured_log::timestamp(),
        }));
        match repair_provider_identity_file(system_config) {
            IdentityRepair::Repaired => {
                let _ = state.logger.write(json!({
                    "level": "INFO",
                    "event": "desktop.session.identity_repaired",
                    "error_code": DSK_IDENTITY_REPAIRED,
                    "layer": "system",
                    "name": LEGAL_PROVIDER_NAME,
                    "requires_openai_auth": LEGAL_REQUIRES_OPENAI_AUTH,
                    "timestamp": structured_log::timestamp(),
                }));
            }
            IdentityRepair::Failed => {
                let _ = state.logger.write(json!({
                    "level": "WARN",
                    "event": "desktop.session.identity_repair_failed",
                    "error_code": DSK_IDENTITY_ILLEGAL,
                    "layer": "system",
                    "timestamp": structured_log::timestamp(),
                }));
            }
            IdentityRepair::AlreadyLegal | IdentityRepair::Missing => {}
        }
    }
    if !read_provider_identity_file(system_config).present {
        if let Ok(home) = std::fs::read_to_string(user_config) {
            if let Some(created) = restore_system_from_home(&home) {
                if let Some(parent) = system_config.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::write(system_config, created.as_bytes()).is_ok() {
                    let _ = state.logger.write(json!({
                        "level": "INFO",
                        "event": "desktop.session.system_binding_created",
                        "error_code": DSK_SYSTEM_CREATED,
                        "layer": "system",
                        "name": LEGAL_PROVIDER_NAME,
                        "requires_openai_auth": LEGAL_REQUIRES_OPENAI_AUTH,
                        "timestamp": structured_log::timestamp(),
                    }));
                }
            }
        }
    }
    let user_identity = read_provider_identity_file(user_config);
    if !user_identity.present {
        let _ = state.logger.write(json!({
            "level": "WARN",
            "event": "desktop.session.home_provider_missing",
            "error_code": DSK_HOME_MISSING,
            "layer": "home",
            "timestamp": structured_log::timestamp(),
        }));
        if repair_user {
            if let (Ok(home), Ok(system)) = (
                std::fs::read_to_string(user_config),
                std::fs::read_to_string(system_config),
            ) {
                if let Some(restored) = restore_home_provider_from_system(&home, &system) {
                    if std::fs::write(user_config, restored.as_bytes()).is_ok() {
                        let _ = state.logger.write(json!({
                            "level": "INFO",
                            "event": "desktop.session.home_provider_restored",
                            "error_code": DSK_HOME_RESTORED,
                            "layer": "home",
                            "name": LEGAL_PROVIDER_NAME,
                            "requires_openai_auth": LEGAL_REQUIRES_OPENAI_AUTH,
                            "timestamp": structured_log::timestamp(),
                        }));
                    }
                }
            }
        }
    } else if !user_identity.is_legal() {
        let _ = state.logger.write(json!({
            "level": "WARN",
            "event": "desktop.session.identity_illegal",
            "error_code": DSK_IDENTITY_ILLEGAL,
            "layer": "home",
            "name": user_identity.name,
            "requires_openai_auth": user_identity.requires_openai_auth,
            "timestamp": structured_log::timestamp(),
        }));
        if repair_user {
            match repair_provider_identity_file(user_config) {
                IdentityRepair::Repaired => {
                    let _ = state.logger.write(json!({
                        "level": "INFO",
                        "event": "desktop.session.identity_repaired",
                        "error_code": DSK_IDENTITY_REPAIRED,
                        "layer": "home",
                        "name": LEGAL_PROVIDER_NAME,
                        "requires_openai_auth": LEGAL_REQUIRES_OPENAI_AUTH,
                        "timestamp": structured_log::timestamp(),
                    }));
                }
                IdentityRepair::Failed => {
                    let _ = state.logger.write(json!({
                        "level": "WARN",
                        "event": "desktop.session.identity_repair_failed",
                        "error_code": DSK_IDENTITY_ILLEGAL,
                        "layer": "home",
                        "timestamp": structured_log::timestamp(),
                    }));
                }
                IdentityRepair::AlreadyLegal | IdentityRepair::Missing => {}
            }
        }
    }
    let user_identity = read_provider_identity_file(user_config);
    let system_identity = read_provider_identity_file(system_config);
    let _ = state.logger.write(json!({
        "level": "INFO",
        "event": "desktop.session.heartbeat",
        "error_code": DSK_HEARTBEAT,
        "timestamp": structured_log::timestamp(),
        "home_config_mtime": file_mtime_rfc3339(user_config),
        "system_config_mtime": file_mtime_rfc3339(system_config),
        "desktop_login_mtime": file_mtime_rfc3339(user_auth),
        "home_identity": identity_value(&user_identity),
        "system_identity": identity_value(&system_identity),
        "replicas_scanned": replica_stats.scanned,
        "replicas_written": replica_stats.written,
        "replicas_skipped": replica_stats.skipped,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::cli_proxy::CliProxyManagementClient;
    use crate::control_plane::http_compat::BackendPaths;
    use crate::routing::RouteTable;
    use crate::state::StateStore;
    use crate::telemetry::structured_log::StructuredLogger;
    use std::sync::{Arc, RwLock};

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "codex-router-desktop-session-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn legal_identity_is_never_openai_or_requires_auth() {
        assert_eq!(legal_provider_identity(), (false, false));
        const { assert!(!LEGAL_REQUIRES_OPENAI_AUTH) };
        assert_eq!(LEGAL_PROVIDER_NAME, "Codex-Router");
    }

    #[test]
    fn patch_skips_already_legal_bytes() {
        let legal = r#"
[model_providers.codex_router]
name = "Codex-Router"
requires_openai_auth = false
base_url = "http://127.0.0.1:28085/v1"
"#;
        assert!(patched_provider_identity(legal).is_none());
        assert!(read_provider_identity(legal).is_legal());
    }

    #[test]
    fn patch_clears_requires_openai_auth_and_openai_name() {
        let illegal = r#"
[model_providers.codex_router]
name = "OpenAI"
requires_openai_auth = true
base_url = "http://127.0.0.1:28085/v1"
"#;
        let patched = patched_provider_identity(illegal).expect("illegal identity must patch");
        let identity = read_provider_identity(&patched);
        assert!(identity.is_legal());
        assert!(patched.contains("name = \"Codex-Router\""));
        assert!(patched.contains("requires_openai_auth = false"));
        assert!(!patched.contains("name = \"OpenAI\""));
        assert!(patched.contains("base_url = \"http://127.0.0.1:28085/v1\""));
    }

    #[test]
    fn protect_repairs_system_layer_without_touching_legal_user_layer() {
        let root = temp_root("system-repair");
        let user_config = root.join("config.toml");
        let user_auth = root.join("auth.json");
        let system_config = root.join("system.toml");
        let legal_user = r#"
[model_providers.codex_router]
name = "Codex-Router"
requires_openai_auth = false
"#;
        std::fs::write(&user_config, legal_user).unwrap();
        std::fs::write(&user_auth, "{}").unwrap();
        std::fs::write(
            &system_config,
            r#"
# codex-router-managed: binding layer
[model_providers.codex_router]
name = "OpenAI"
requires_openai_auth = true
"#,
        )
        .unwrap();
        let user_mtime = std::fs::metadata(&user_config).unwrap().modified().unwrap();
        let store = Arc::new(StateStore::open(root.join("state.sqlite3")).unwrap());
        let state = ControlState {
            store,
            cli: CliProxyManagementClient::new("http://127.0.0.1:1", "test-management-secret")
                .unwrap(),
            logger: Arc::new(StructuredLogger::open(root.join("router-events.jsonl")).unwrap()),
            routes: Arc::new(RwLock::new(RouteTable::new(Vec::new()).unwrap())),
            backend: Some(Arc::new(BackendPaths {
                config_path: root.join("config.yaml"),
                auth_dir: root.join("auth"),
                downstream_key: "downstream".to_owned(),
                management_secret: "test-management-secret".to_owned(),
                cli_port: 1,
                desktop_auth_path: user_auth.clone(),
            })),
            cli_index_map: Arc::new(RwLock::new(std::collections::HashMap::new())),
        };
        protect_desktop_session_at(
            &state,
            &user_config,
            &user_auth,
            &system_config,
            ReplicaSyncStats {
                scanned: 3,
                written: 0,
                skipped: 3,
            },
            true,
        );
        assert_eq!(
            std::fs::metadata(&user_config).unwrap().modified().unwrap(),
            user_mtime,
            "legal user config.toml must not be rewritten"
        );
        let system = std::fs::read_to_string(&system_config).unwrap();
        assert!(read_provider_identity(&system).is_legal());
        let events = std::fs::read_to_string(root.join("router-events.jsonl")).unwrap();
        assert!(events.contains(DSK_IDENTITY_ILLEGAL));
        assert!(events.contains(DSK_IDENTITY_REPAIRED));
        assert!(events.contains(DSK_HEARTBEAT));
        assert!(events.contains("\"layer\":\"system\""));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn protect_repairs_user_layer_once_when_illegal() {
        let root = temp_root("user-repair");
        let user_config = root.join("config.toml");
        let user_auth = root.join("auth.json");
        let system_config = root.join("system.toml");
        std::fs::write(
            &user_config,
            r#"
[model_providers.codex_router]
name = "OpenAI"
requires_openai_auth = true
"#,
        )
        .unwrap();
        std::fs::write(&user_auth, "{}").unwrap();
        std::fs::write(
            &system_config,
            r#"
[model_providers.codex_router]
name = "Codex-Router"
requires_openai_auth = false
"#,
        )
        .unwrap();
        let store = Arc::new(StateStore::open(root.join("state.sqlite3")).unwrap());
        let state = ControlState {
            store,
            cli: CliProxyManagementClient::new("http://127.0.0.1:1", "test-management-secret")
                .unwrap(),
            logger: Arc::new(StructuredLogger::open(root.join("router-events.jsonl")).unwrap()),
            routes: Arc::new(RwLock::new(RouteTable::new(Vec::new()).unwrap())),
            backend: Some(Arc::new(BackendPaths {
                config_path: root.join("config.yaml"),
                auth_dir: root.join("auth"),
                downstream_key: "downstream".to_owned(),
                management_secret: "test-management-secret".to_owned(),
                cli_port: 1,
                desktop_auth_path: user_auth.clone(),
            })),
            cli_index_map: Arc::new(RwLock::new(std::collections::HashMap::new())),
        };
        protect_desktop_session_at(
            &state,
            &user_config,
            &user_auth,
            &system_config,
            ReplicaSyncStats::default(),
            true,
        );
        let user = std::fs::read_to_string(&user_config).unwrap();
        assert!(read_provider_identity(&user).is_legal());
        let events = std::fs::read_to_string(root.join("router-events.jsonl")).unwrap();
        assert!(events.contains("\"layer\":\"home\""));
        assert!(events.contains(DSK_IDENTITY_REPAIRED));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn protect_restores_stripped_home_provider_from_system_layer() {
        let root = temp_root("home-restore");
        let user_config = root.join("config.toml");
        let user_auth = root.join("auth.json");
        let system_config = root.join("system.toml");
        std::fs::write(
            &user_config,
            r#"
model = "first"
sandbox_mode = "danger-full-access"
[desktop]
theme = "dark"
[model_providers]
"#,
        )
        .unwrap();
        std::fs::write(&user_auth, "{}").unwrap();
        std::fs::write(
            &system_config,
            r#"
model_provider = "codex_router"
model_catalog_json = "C:/catalog.json"
[model_providers.codex_router]
name = "OpenAI"
requires_openai_auth = true
base_url = "http://127.0.0.1:28085/v1"
experimental_bearer_token = "sk-local-fixture"
"#,
        )
        .unwrap();
        let store = Arc::new(StateStore::open(root.join("state.sqlite3")).unwrap());
        let state = ControlState {
            store,
            cli: CliProxyManagementClient::new("http://127.0.0.1:1", "test-management-secret")
                .unwrap(),
            logger: Arc::new(StructuredLogger::open(root.join("router-events.jsonl")).unwrap()),
            routes: Arc::new(RwLock::new(RouteTable::new(Vec::new()).unwrap())),
            backend: Some(Arc::new(BackendPaths {
                config_path: root.join("config.yaml"),
                auth_dir: root.join("auth"),
                downstream_key: "downstream".to_owned(),
                management_secret: "test-management-secret".to_owned(),
                cli_port: 1,
                desktop_auth_path: user_auth.clone(),
            })),
            cli_index_map: Arc::new(RwLock::new(std::collections::HashMap::new())),
        };
        protect_desktop_session_at(
            &state,
            &user_config,
            &user_auth,
            &system_config,
            ReplicaSyncStats::default(),
            true,
        );
        let home = std::fs::read_to_string(&user_config).unwrap();
        let system = std::fs::read_to_string(&system_config).unwrap();
        assert!(read_provider_identity(&home).is_legal());
        assert!(read_provider_identity(&system).is_legal());
        assert!(home.contains("model = \"first\""));
        assert!(home.contains("theme = \"dark\""));
        assert!(home.contains("model_provider = \"codex_router\""));
        assert!(home.contains("experimental_bearer_token = \"sk-local-fixture\""));
        let events = std::fs::read_to_string(root.join("router-events.jsonl")).unwrap();
        assert!(events.contains(DSK_HOME_MISSING));
        assert!(events.contains(DSK_HOME_RESTORED));
        assert!(events.contains(DSK_IDENTITY_REPAIRED));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn protect_creates_missing_system_binding_from_legal_home() {
        let root = temp_root("system-create");
        let user_config = root.join("config.toml");
        let user_auth = root.join("auth.json");
        let system_config = root.join("system.toml");
        std::fs::write(
            &user_config,
            r#"
model_catalog_json = "C:/catalog.json"
[model_providers.codex_router]
name = "Codex-Router"
requires_openai_auth = false
base_url = "http://127.0.0.1:28085/v1"
experimental_bearer_token = "sk-local-fixture"
"#,
        )
        .unwrap();
        std::fs::write(&user_auth, "{}").unwrap();
        let store = Arc::new(StateStore::open(root.join("state.sqlite3")).unwrap());
        let state = ControlState {
            store,
            cli: CliProxyManagementClient::new("http://127.0.0.1:1", "test-management-secret")
                .unwrap(),
            logger: Arc::new(StructuredLogger::open(root.join("router-events.jsonl")).unwrap()),
            routes: Arc::new(RwLock::new(RouteTable::new(Vec::new()).unwrap())),
            backend: Some(Arc::new(BackendPaths {
                config_path: root.join("config.yaml"),
                auth_dir: root.join("auth"),
                downstream_key: "downstream".to_owned(),
                management_secret: "test-management-secret".to_owned(),
                cli_port: 1,
                desktop_auth_path: user_auth.clone(),
            })),
            cli_index_map: Arc::new(RwLock::new(std::collections::HashMap::new())),
        };
        protect_desktop_session_at(
            &state,
            &user_config,
            &user_auth,
            &system_config,
            ReplicaSyncStats::default(),
            false,
        );
        let system = std::fs::read_to_string(&system_config).unwrap();
        assert!(read_provider_identity(&system).is_legal());
        assert!(system.contains(SYSTEM_BINDING_MARKER));
        assert!(system.contains("experimental_bearer_token = \"sk-local-fixture\""));
        let events = std::fs::read_to_string(root.join("router-events.jsonl")).unwrap();
        assert!(events.contains(DSK_SYSTEM_CREATED));
        let _ = std::fs::remove_dir_all(root);
    }
}
