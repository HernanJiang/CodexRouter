use crate::config::{atomic_write, RouterConfig};
use crate::logic;
use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use toml_edit::{DocumentMut, Item, Table};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationKind {
    Local,
    CcSwitch,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IsolationProfile {
    pub id: String,
    pub name: String,
    pub kind: IsolationKind,
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePoint {
    pub id: String,
    pub label: String,
    pub created_at: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RestoreOutcome {
    pub auth_available: bool,
    pub shared_state_preserved: bool,
    pub account_changed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateManifest {
    version: u32,
    label: String,
    created_at: String,
    config_present: bool,
    auth_present: bool,
    router_config_present: bool,
    #[serde(default)]
    config_source: String,
    #[serde(default)]
    account_fingerprint: String,
}

fn store_root(router_root: &Path) -> PathBuf {
    router_root.join("backups").join("config-profiles")
}

fn profile_root(router_root: &Path) -> PathBuf {
    store_root(router_root).join("profiles")
}

fn history_root(router_root: &Path) -> PathBuf {
    store_root(router_root).join("history")
}

fn original_root(router_root: &Path) -> PathBuf {
    store_root(router_root).join("original-codex")
}

const MAX_RESTORE_POINTS: usize = 3;
static NEXT_STATE_ID: AtomicU64 = AtomicU64::new(1);

pub struct ConfigApplyLock {
    _file: std::fs::File,
    #[cfg(not(windows))]
    path: PathBuf,
}

#[cfg(not(windows))]
impl Drop for ConfigApplyLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn acquire_config_apply_lock(
    router_root: &Path,
    timeout: Duration,
) -> anyhow::Result<ConfigApplyLock> {
    let lock_dir = router_root.join("data").join("locks");
    std::fs::create_dir_all(&lock_dir)?;
    let path = lock_dir.join("config-apply.lock");
    let started = Instant::now();
    loop {
        match try_open_config_lock(&path) {
            Ok(mut file) => {
                use std::io::{Seek, Write};
                file.set_len(0)?;
                file.rewind()?;
                writeln!(file, "pid={}", std::process::id())?;
                file.sync_data()?;
                return Ok(ConfigApplyLock {
                    _file: file,
                    #[cfg(not(windows))]
                    path,
                });
            }
            Err(error) if started.elapsed() < timeout => {
                if !is_lock_contention(&error) {
                    return Err(error).context("无法打开 Router 配置切换锁");
                }
                std::thread::sleep(Duration::from_millis(75));
            }
            Err(error) => {
                return Err(error).context("等待其他 Router/CC Switch 配置操作完成超时");
            }
        }
    }
}

#[cfg(windows)]
fn try_open_config_lock(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
}

#[cfg(not(windows))]
fn try_open_config_lock(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
}

fn is_lock_contention(error: &std::io::Error) -> bool {
    #[cfg(windows)]
    {
        matches!(error.raw_os_error(), Some(32) | Some(33))
    }
    #[cfg(not(windows))]
    {
        error.kind() == std::io::ErrorKind::AlreadyExists
    }
}

fn timestamp_id(prefix: &str) -> String {
    format!(
        "{}-{}-{}",
        prefix,
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        NEXT_STATE_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    atomic_write(path, serde_json::to_string_pretty(value)?.as_bytes())
}

fn atomic_copy(source: &Path, destination: &Path) -> anyhow::Result<()> {
    atomic_write(destination, &std::fs::read(source)?)
}

fn clear_optional_file(path: &Path) -> anyhow::Result<()> {
    if path.is_file() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
struct AuthInfo {
    valid_chatgpt: bool,
    account_fingerprint: String,
}

fn auth_account_id(value: &serde_json::Value) -> Option<String> {
    let tokens = value.get("tokens");
    [
        tokens.and_then(|item| item.get("account_id")),
        tokens.and_then(|item| item.get("accountId")),
        value.get("account_id"),
        value.get("accountId"),
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

fn account_fingerprint(account_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"codex-router-account-v1\0");
    hasher.update(account_id.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn read_auth_info(path: &Path) -> AuthInfo {
    let Some(value) = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
    else {
        return AuthInfo::default();
    };
    let valid_chatgpt = value.get("auth_mode").and_then(|item| item.as_str()) == Some("chatgpt")
        && value.get("tokens").is_some_and(|item| item.is_object());
    let fingerprint = auth_account_id(&value)
        .map(|account_id| account_fingerprint(&account_id))
        .unwrap_or_default();
    AuthInfo {
        valid_chatgpt,
        account_fingerprint: fingerprint,
    }
}

fn read_snapshot_auth_info(source: &Path, manifest: &StateManifest) -> anyhow::Result<AuthInfo> {
    if !manifest.auth_present {
        return Ok(AuthInfo::default());
    }
    let probe = std::env::temp_dir().join(format!("{}.json", timestamp_id("codex-auth-probe")));
    let result = (|| {
        logic::unprotect_file_for_current_user(&source.join("auth.json.dpapi"), &probe)
            .context("无法读取当前 Windows 用户的 Codex 登录快照")?;
        let mut info = read_auth_info(&probe);
        if info.account_fingerprint.is_empty() {
            info.account_fingerprint = manifest.account_fingerprint.clone();
        }
        Ok(info)
    })();
    let _ = clear_optional_file(&probe);
    result
}

fn capture_state(
    destination: &Path,
    label: &str,
    cfg: Option<&RouterConfig>,
    config_source: Option<&Path>,
    auth_source: Option<&Path>,
    source_description: &str,
) -> anyhow::Result<StateManifest> {
    std::fs::create_dir_all(destination)?;
    let saved_config = destination.join("config.toml");
    let saved_auth = destination.join("auth.json.dpapi");
    let saved_router = destination.join("router-config.json");

    let config_present = config_source.is_some_and(Path::is_file);
    if let Some(source) = config_source.filter(|path| path.is_file()) {
        atomic_copy(source, &saved_config)?;
    } else {
        clear_optional_file(&saved_config)?;
    }

    let auth_present = auth_source.is_some_and(Path::is_file);
    let account_fingerprint = auth_source
        .filter(|path| path.is_file())
        .map(|path| read_auth_info(path).account_fingerprint)
        .unwrap_or_default();
    if let Some(source) = auth_source.filter(|path| path.is_file()) {
        let protected_temp = destination.join(format!("{}.dpapi.tmp", timestamp_id("auth")));
        let result = (|| {
            logic::protect_file_for_current_user(source, &protected_temp)?;
            atomic_copy(&protected_temp, &saved_auth)
        })();
        let _ = clear_optional_file(&protected_temp);
        result?;
    } else {
        clear_optional_file(&saved_auth)?;
    }

    if let Some(config) = cfg {
        config.save(&saved_router)?;
    } else {
        clear_optional_file(&saved_router)?;
    }

    let manifest = StateManifest {
        version: 2,
        label: label.to_owned(),
        created_at: chrono::Utc::now().to_rfc3339(),
        config_present,
        auth_present,
        router_config_present: cfg.is_some(),
        config_source: source_description.to_owned(),
        account_fingerprint,
    };
    write_json(&destination.join("state.json"), &manifest)?;
    Ok(manifest)
}

fn capture_current_state(
    destination: &Path,
    label: &str,
    cfg: Option<&RouterConfig>,
) -> anyhow::Result<StateManifest> {
    let codex_home = match cfg {
        Some(config) => logic::resolve_codex_home(config),
        None => logic::resolve_codex_home(&RouterConfig::default()),
    };
    capture_state(
        destination,
        label,
        cfg,
        Some(&codex_home.join("config.toml")),
        Some(&codex_home.join("auth.json")),
        "current",
    )
}

fn restore_full_state(
    source: &Path,
    cfg: &RouterConfig,
    manifest: &StateManifest,
) -> anyhow::Result<()> {
    let codex_home = logic::resolve_codex_home(cfg);
    std::fs::create_dir_all(&codex_home)?;
    let config_target = codex_home.join("config.toml");
    let auth_target = codex_home.join("auth.json");
    let config_content = if manifest.config_present {
        // A snapshot may have been captured from an older CC-Switch profile
        // that used the UAC-backed elevated sandbox. Normalize it before the
        // next Codex launch so recovery cannot recreate the setup loop.
        Some(
            logic::normalize_windows_sandbox_config(
                &std::fs::read_to_string(source.join("config.toml"))
                    .context("无法读取待还原的 Codex config.toml")?,
            )
            .into_bytes(),
        )
    } else {
        None
    };
    let auth_content = if manifest.auth_present {
        let auth_temp = codex_home.join(format!("{}.json.tmp", timestamp_id("auth-restore")));
        let result = (|| {
            logic::unprotect_file_for_current_user(&source.join("auth.json.dpapi"), &auth_temp)
                .context("无法解密当前 Windows 用户的 Codex 登录快照")?;
            Ok::<_, anyhow::Error>(std::fs::read(&auth_temp)?)
        })();
        let _ = clear_optional_file(&auth_temp);
        Some(result?)
    } else {
        None
    };

    // Both replacements are staged in memory. If the second commit fails,
    // restore the exact pre-operation state so a profile switch cannot leave
    // Codex with config and authentication from different snapshots.
    let original_config = read_optional(&config_target)?;
    let original_auth = read_optional(&auth_target)?;
    let commit = (|| -> anyhow::Result<()> {
        replace_optional(&config_target, config_content.as_deref())?;
        replace_optional(&auth_target, auth_content.as_deref())?;
        Ok(())
    })();
    if let Err(error) = commit {
        let config_rollback = replace_optional(&config_target, original_config.as_deref());
        let auth_rollback = replace_optional(&auth_target, original_auth.as_deref());
        if config_rollback.is_err() || auth_rollback.is_err() {
            return Err(error).context("Codex 状态提交失败，且自动回滚未完全成功");
        }
        return Err(error).context("Codex 状态提交失败，已恢复切换前文件");
    }
    Ok(())
}

fn replace_optional(path: &Path, content: Option<&[u8]>) -> anyhow::Result<()> {
    if let Some(content) = content {
        atomic_write(path, content)
    } else {
        clear_optional_file(path)
    }
}

fn read_optional(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn remove_table_value(document: &mut DocumentMut, table_name: &str, key: &str) {
    if let Some(table) = document
        .get_mut(table_name)
        .and_then(Item::as_table_like_mut)
    {
        table.remove(key);
    }
    let remove_table = document
        .get(table_name)
        .and_then(Item::as_table_like)
        .is_some_and(|table| table.is_empty());
    if remove_table {
        document.remove(table_name);
    }
}

fn replace_table_value(
    current: &mut DocumentMut,
    target: &DocumentMut,
    table_name: &str,
    key: &str,
) -> anyhow::Result<()> {
    let target_value = target
        .get(table_name)
        .and_then(Item::as_table_like)
        .and_then(|table| table.get(key))
        .cloned();
    remove_table_value(current, table_name, key);
    let Some(value) = target_value else {
        return Ok(());
    };
    if current.get(table_name).is_none() {
        current.insert(table_name, Item::Table(Table::new()));
    }
    current
        .get_mut(table_name)
        .and_then(Item::as_table_like_mut)
        .with_context(|| format!("Codex [{table_name}] 必须是 TOML 表"))?
        .insert(key, value);
    Ok(())
}

fn remove_nested_table_value(
    document: &mut DocumentMut,
    parent_name: &str,
    table_name: &str,
    key: &str,
) {
    if let Some(parent) = document
        .get_mut(parent_name)
        .and_then(Item::as_table_like_mut)
    {
        if let Some(table) = parent.get_mut(table_name).and_then(Item::as_table_like_mut) {
            table.remove(key);
        }
        let remove_nested = parent
            .get(table_name)
            .and_then(Item::as_table_like)
            .is_some_and(|table| table.is_empty());
        if remove_nested {
            parent.remove(table_name);
        }
    }
    let remove_parent = document
        .get(parent_name)
        .and_then(Item::as_table_like)
        .is_some_and(|table| table.is_empty());
    if remove_parent {
        document.remove(parent_name);
    }
}

fn replace_nested_table_value(
    current: &mut DocumentMut,
    target: &DocumentMut,
    parent_name: &str,
    table_name: &str,
    key: &str,
) -> anyhow::Result<()> {
    let target_value = target
        .get(parent_name)
        .and_then(Item::as_table_like)
        .and_then(|parent| parent.get(table_name))
        .and_then(Item::as_table_like)
        .and_then(|table| table.get(key))
        .cloned();
    remove_nested_table_value(current, parent_name, table_name, key);
    let Some(value) = target_value else {
        return Ok(());
    };
    if current.get(parent_name).is_none() {
        let mut parent = Table::new();
        parent.set_implicit(true);
        current.insert(parent_name, Item::Table(parent));
    }
    let parent = current
        .get_mut(parent_name)
        .and_then(Item::as_table_like_mut)
        .with_context(|| format!("Codex [{parent_name}] 必须是 TOML 表"))?;
    if parent.get(table_name).is_none() {
        parent.insert(table_name, Item::Table(Table::new()));
    }
    parent
        .get_mut(table_name)
        .and_then(Item::as_table_like_mut)
        .with_context(|| format!("Codex [{parent_name}.{table_name}] 必须是 TOML 表"))?
        .insert(key, value);
    Ok(())
}

pub(crate) fn merge_codex_route_config(current: &str, target: &str) -> anyhow::Result<String> {
    let mut current_doc = current
        .parse::<DocumentMut>()
        .context("当前 Codex config.toml 不是有效 TOML")?;
    let target_doc = target
        .parse::<DocumentMut>()
        .context("待切换的 Codex config.toml 快照不是有效 TOML")?;
    let current_router_provider = current_doc
        .get("model_provider")
        .and_then(Item::as_str)
        .filter(|provider_id| matches!(*provider_id, "custom" | "sub2api"))
        .filter(|provider_id| {
            current_doc
                .get("model_providers")
                .and_then(Item::as_table_like)
                .and_then(|providers| providers.get(*provider_id))
                .and_then(Item::as_table_like)
                .and_then(|provider| provider.get("name"))
                .and_then(Item::as_str)
                == Some("Codex-Router")
        })
        .map(str::to_owned);
    for key in [
        "model_provider",
        "model",
        "model_catalog_json",
        "model_reasoning_effort",
        "service_tier",
        "openai_base_url",
        "disable_response_storage",
    ] {
        current_doc.remove(key);
        if let Some(item) = target_doc.get(key) {
            current_doc.insert(key, item.clone());
        }
    }
    if let Some(providers) = current_doc
        .get_mut("model_providers")
        .and_then(Item::as_table_like_mut)
    {
        if let Some(provider_id) = &current_router_provider {
            providers.remove(provider_id);
        }
        providers.remove("sub2api");
    }

    for key in ["model", "model_reasoning_effort", "service_tier"] {
        replace_nested_table_value(&mut current_doc, &target_doc, "models", "new_thread", key)?;
    }
    replace_table_value(&mut current_doc, &target_doc, "features", "fast_mode")?;
    replace_table_value(
        &mut current_doc,
        &target_doc,
        "desktop",
        "enabled-reasoning-efforts",
    )?;
    if let Some(provider_name) = target_doc.get("model_provider").and_then(Item::as_str) {
        let target_provider = target_doc
            .get("model_providers")
            .and_then(Item::as_table_like)
            .and_then(|providers| providers.get(provider_name))
            .cloned();
        if let Some(provider) = target_provider {
            if current_doc.get("model_providers").is_none() {
                current_doc.insert("model_providers", Item::Table(Table::new()));
            }
            current_doc
                .get_mut("model_providers")
                .and_then(Item::as_table_like_mut)
                .context("Codex model_providers 必须是 TOML 表")?
                .insert(provider_name, provider);
        }
    }

    Ok(logic::normalize_windows_sandbox_config(
        &current_doc.to_string(),
    ))
}

fn restore_shared_config(
    source: &Path,
    cfg: &RouterConfig,
    manifest: &StateManifest,
) -> anyhow::Result<()> {
    let codex_home = logic::resolve_codex_home(cfg);
    std::fs::create_dir_all(&codex_home)?;
    let target_path = codex_home.join("config.toml");
    let snapshot = if manifest.config_present {
        std::fs::read_to_string(source.join("config.toml"))?
    } else {
        String::new()
    };
    let merged = if target_path.is_file() {
        merge_codex_route_config(&std::fs::read_to_string(&target_path)?, &snapshot)?
    } else {
        logic::normalize_windows_sandbox_config(&snapshot)
    };
    if merged.trim().is_empty() {
        clear_optional_file(&target_path)?;
    } else {
        atomic_write(&target_path, merged.as_bytes())?;
    }
    Ok(())
}

fn restore_snapshot_auth(source: &Path, cfg: &RouterConfig) -> anyhow::Result<()> {
    let codex_home = logic::resolve_codex_home(cfg);
    std::fs::create_dir_all(&codex_home)?;
    let target = codex_home.join("auth.json");
    let temp = codex_home.join(format!("{}.json.tmp", timestamp_id("auth-restore")));
    let result = (|| {
        logic::unprotect_file_for_current_user(&source.join("auth.json.dpapi"), &temp)
            .context("无法解密当前 Windows 用户的 Codex 登录快照")?;
        atomic_copy(&temp, &target)
    })();
    let _ = clear_optional_file(&temp);
    result
}

fn restore_state(
    source: &Path,
    cfg: &RouterConfig,
    share_codex_state: bool,
) -> anyhow::Result<RestoreOutcome> {
    let manifest: StateManifest = serde_json::from_str(
        &std::fs::read_to_string(source.join("state.json"))
            .with_context(|| format!("配置快照不完整: {}", source.display()))?,
    )?;
    let auth_target = logic::resolve_codex_home(cfg).join("auth.json");
    let current_auth = read_auth_info(&auth_target);
    let snapshot_auth = read_snapshot_auth_info(source, &manifest)?;
    let account_changed = current_auth.valid_chatgpt
        && snapshot_auth.valid_chatgpt
        && !current_auth.account_fingerprint.is_empty()
        && !snapshot_auth.account_fingerprint.is_empty()
        && current_auth.account_fingerprint != snapshot_auth.account_fingerprint;
    let preserve_shared_state = share_codex_state && !account_changed;

    if preserve_shared_state {
        restore_shared_config(source, cfg, &manifest)?;
        if !current_auth.valid_chatgpt && snapshot_auth.valid_chatgpt {
            restore_snapshot_auth(source, cfg)?;
        } else if !current_auth.valid_chatgpt && !snapshot_auth.valid_chatgpt {
            clear_optional_file(&auth_target)?;
        }
    } else {
        restore_full_state(source, cfg, &manifest)?;
    }

    let final_auth = read_auth_info(&auth_target);
    if auth_target.is_file() && !final_auth.valid_chatgpt {
        clear_optional_file(&auth_target)?;
    }
    Ok(RestoreOutcome {
        auth_available: final_auth.valid_chatgpt,
        shared_state_preserved: preserve_shared_state,
        account_changed,
    })
}

fn sanitize_router_config(text: &str) -> String {
    let Ok(mut document) = text.parse::<DocumentMut>() else {
        return text.to_owned();
    };
    let router_provider = document
        .get("model_provider")
        .and_then(Item::as_str)
        .filter(|provider_id| matches!(*provider_id, "custom" | "sub2api"))
        .filter(|provider_id| {
            document
                .get("model_providers")
                .and_then(Item::as_table_like)
                .and_then(|providers| providers.get(*provider_id))
                .and_then(Item::as_table_like)
                .and_then(|provider| provider.get("name"))
                .and_then(Item::as_str)
                == Some("Codex-Router")
        })
        .map(str::to_owned);
    let Some(provider_id) = router_provider else {
        return text.to_owned();
    };

    for key in [
        "model_provider",
        "model",
        "model_catalog_json",
        "model_reasoning_effort",
        "service_tier",
        "openai_base_url",
        "disable_response_storage",
    ] {
        document.remove(key);
    }
    for key in ["model", "model_reasoning_effort", "service_tier"] {
        remove_nested_table_value(&mut document, "models", "new_thread", key);
    }
    remove_table_value(&mut document, "features", "fast_mode");
    remove_table_value(&mut document, "desktop", "enabled-reasoning-efforts");
    if let Some(providers) = document
        .get_mut("model_providers")
        .and_then(Item::as_table_like_mut)
    {
        providers.remove(&provider_id);
        providers.remove("sub2api");
    }
    let remove_empty_providers = document
        .get("model_providers")
        .and_then(Item::as_table_like)
        .is_some_and(|providers| providers.is_empty());
    if remove_empty_providers {
        document.remove("model_providers");
    }
    let cleaned = document.to_string().trim().to_owned();
    if cleaned.is_empty() {
        String::new()
    } else {
        format!("{cleaned}\r\n")
    }
}

pub fn ensure_original_codex_snapshot(
    router_root: &Path,
    cfg: &RouterConfig,
) -> anyhow::Result<()> {
    let destination = original_root(router_root);
    if destination.join("state.json").is_file() {
        return Ok(());
    }
    std::fs::create_dir_all(&destination)?;
    let codex_home = logic::resolve_codex_home(cfg);
    let current_config = codex_home.join("config.toml");
    let mut historical_backups = std::fs::read_dir(&codex_home)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with("config.toml.codex-router-") && name.ends_with(".bak")
                })
        })
        .collect::<Vec<_>>();
    historical_backups.sort();

    let (config_source, source_description) = if let Some(oldest) = historical_backups.first() {
        (
            Some(oldest.clone()),
            format!("historical-backup:{}", oldest.display()),
        )
    } else if current_config.is_file() {
        let sanitized = sanitize_router_config(&std::fs::read_to_string(&current_config)?);
        if sanitized.is_empty() {
            (None, "sanitized-current-empty".to_owned())
        } else {
            let prepared = destination.join("prepared-original.toml");
            atomic_write(&prepared, sanitized.as_bytes())?;
            (Some(prepared), "sanitized-current".to_owned())
        }
    } else {
        (None, "no-original-config".to_owned())
    };

    capture_state(
        &destination,
        "Codex official login baseline",
        None,
        config_source.as_deref(),
        Some(&codex_home.join("auth.json")),
        &source_description,
    )?;
    clear_optional_file(&destination.join("prepared-original.toml"))?;
    Ok(())
}

pub fn restore_original_codex(
    router_root: &Path,
    cfg: &RouterConfig,
    share_codex_state: bool,
) -> anyhow::Result<RestoreOutcome> {
    ensure_original_codex_snapshot(router_root, cfg)?;
    restore_state(&original_root(router_root), cfg, share_codex_state)
}

pub fn capture_restore_point(
    router_root: &Path,
    cfg: &RouterConfig,
    label: &str,
) -> anyhow::Result<RestorePoint> {
    let id = timestamp_id("restore");
    let destination = history_root(router_root).join(&id);
    let state = capture_current_state(&destination, label, Some(cfg))?;
    prune_restore_points(router_root, MAX_RESTORE_POINTS)?;
    Ok(RestorePoint {
        id,
        label: label.to_owned(),
        created_at: state.created_at,
    })
}

fn prune_restore_points(router_root: &Path, keep: usize) -> anyhow::Result<()> {
    let root = history_root(router_root);
    if !root.is_dir() {
        return Ok(());
    }
    let mut entries = std::fs::read_dir(root)?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("restore-"))
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
    for entry in entries.into_iter().skip(keep) {
        std::fs::remove_dir_all(entry.path())?;
    }
    Ok(())
}

pub fn list_restore_points(router_root: &Path) -> anyhow::Result<Vec<RestorePoint>> {
    let root = history_root(router_root);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut points = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        let state: StateManifest = match std::fs::read_to_string(path.join("state.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
        {
            Some(value) => value,
            None => continue,
        };
        points.push(RestorePoint {
            id: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned(),
            label: state.label,
            created_at: state.created_at,
        });
    }
    points.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    Ok(points)
}

pub fn restore_point_config(
    router_root: &Path,
    point: &RestorePoint,
    fallback: &RouterConfig,
    share_codex_state: bool,
) -> anyhow::Result<(RouterConfig, RestoreOutcome)> {
    let source = history_root(router_root).join(&point.id);
    let outcome = restore_state(&source, fallback, share_codex_state)?;
    let saved_router = source.join("router-config.json");
    let config = if saved_router.is_file() {
        RouterConfig::load(&saved_router)
    } else {
        Ok(fallback.clone())
    }?;
    Ok((config, outcome))
}

pub fn list_profiles(router_root: &Path) -> anyhow::Result<Vec<IsolationProfile>> {
    let root = profile_root(router_root);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut profiles = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        let metadata = path.join("profile.json");
        if metadata.is_file() {
            if let Ok(profile) =
                serde_json::from_str::<IsolationProfile>(&std::fs::read_to_string(metadata)?)
            {
                profiles.push(profile);
            }
        }
    }
    profiles.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(profiles)
}

pub fn create_profile(
    router_root: &Path,
    name: &str,
    kind: IsolationKind,
    cfg: &RouterConfig,
) -> anyhow::Result<(IsolationProfile, RouterConfig)> {
    let name = name.trim();
    if name.is_empty() {
        bail!("请先输入隔离配置名称");
    }
    if list_profiles(router_root)?
        .iter()
        .any(|profile| profile.name.eq_ignore_ascii_case(name))
    {
        bail!("已存在名为“{name}”的隔离配置；原配置不会被覆盖");
    }
    let prefix = match kind {
        IsolationKind::Local => "local",
        IsolationKind::CcSwitch => "cc-switch",
    };
    let id = timestamp_id(prefix);
    let mut isolated = cfg.clone();
    match kind {
        IsolationKind::Local => {
            isolated.deploy.cc_switch_sync = false;
            isolated.deploy.cc_switch_profile_id.clear();
        }
        IsolationKind::CcSwitch => {
            isolated.deploy.cc_switch_sync = true;
            isolated.deploy.cc_switch_profile_name = name.to_owned();
            isolated.deploy.cc_switch_profile_id = format!("codex-router-{id}");
            logic::validate_cc_switch_target(&isolated)?;
        }
    }
    let isolated_credentials = logic::isolate_profile_credentials(&mut isolated, router_root, &id)?;
    let now = chrono::Utc::now().to_rfc3339();
    let profile = IsolationProfile {
        id: id.clone(),
        name: name.to_owned(),
        kind,
        created_at: now.clone(),
        updated_at: now,
    };
    let destination = profile_root(router_root).join(&id);
    let save_result = (|| -> anyhow::Result<()> {
        capture_current_state(&destination, name, Some(&isolated))?;
        write_json(&destination.join("profile.json"), &profile)?;
        Ok(())
    })();
    if let Err(error) = save_result {
        let credentials_rollback =
            logic::remove_isolated_profile_credentials(&isolated_credentials);
        let files_rollback = if destination.is_dir() {
            std::fs::remove_dir_all(&destination).map_err(anyhow::Error::from)
        } else {
            Ok(())
        };
        if credentials_rollback.is_err() || files_rollback.is_err() {
            return Err(error).context("ROUTER_PROFILE_ROLLBACK_INCOMPLETE");
        }
        return Err(error).context("ROUTER_PROFILE_SAVE_FAILED");
    }
    Ok((profile, isolated))
}

pub fn load_profile_config(
    router_root: &Path,
    profile: &IsolationProfile,
) -> anyhow::Result<RouterConfig> {
    RouterConfig::load(
        &profile_root(router_root)
            .join(&profile.id)
            .join("router-config.json"),
    )
}

pub fn restore_profile_codex_state(
    router_root: &Path,
    profile: &IsolationProfile,
    cfg: &RouterConfig,
    share_codex_state: bool,
) -> anyhow::Result<RestoreOutcome> {
    restore_state(
        &profile_root(router_root).join(&profile.id),
        cfg,
        share_codex_state,
    )
}

pub fn update_profile_state(
    router_root: &Path,
    profile_id: &str,
    cfg: &RouterConfig,
) -> anyhow::Result<()> {
    let destination = profile_root(router_root).join(profile_id);
    let profile_path = destination.join("profile.json");
    let mut profile: IsolationProfile = serde_json::from_str(
        &std::fs::read_to_string(&profile_path)
            .with_context(|| format!("隔离配置不存在: {profile_id}"))?,
    )?;
    profile.updated_at = chrono::Utc::now().to_rfc3339();
    capture_current_state(&destination, &profile.name, Some(cfg))?;
    write_json(&profile_path, &profile)?;
    Ok(())
}

pub fn update_profile_oauth_selection(
    router_root: &Path,
    profile_id: &str,
    oauth_account_ids: Option<Vec<i64>>,
    oauth_seen_account_ids: Vec<i64>,
) -> anyhow::Result<()> {
    let destination = profile_root(router_root).join(profile_id);
    let config_path = destination.join("router-config.json");
    let mut config = RouterConfig::load(&config_path)
        .with_context(|| format!("无法读取隔离配置: {profile_id}"))?;
    config.oauth_account_ids = oauth_account_ids;
    config.oauth_seen_account_ids = oauth_seen_account_ids;
    config.save(&config_path)?;

    let profile_path = destination.join("profile.json");
    let mut profile: IsolationProfile = serde_json::from_str(
        &std::fs::read_to_string(&profile_path)
            .with_context(|| format!("隔离配置不存在: {profile_id}"))?,
    )?;
    profile.updated_at = chrono::Utc::now().to_rfc3339();
    write_json(&profile_path, &profile)?;
    Ok(())
}

pub fn purge_oauth_account_references(
    router_root: &Path,
    account_id: i64,
) -> anyhow::Result<usize> {
    let mut changed_profiles = 0;
    for profile in list_profiles(router_root)? {
        let config_path = profile_root(router_root)
            .join(&profile.id)
            .join("router-config.json");
        if !config_path.is_file() {
            continue;
        }
        let mut config = RouterConfig::load(&config_path)?;
        if logic::remove_oauth_account_references(&mut config, account_id) {
            config.save(&config_path)?;
            changed_profiles += 1;
        }
    }
    Ok(changed_profiles)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_test_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "codex-router-profile-{label}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn config_apply_lock_is_exclusive_and_released_on_drop() {
        let root = temporary_test_dir("config-lock");
        let first = acquire_config_apply_lock(&root, Duration::from_millis(100)).unwrap();
        let blocked = acquire_config_apply_lock(&root, Duration::from_millis(150));
        assert!(blocked.is_err());
        drop(first);
        acquire_config_apply_lock(&root, Duration::from_secs(1)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restore_history_keeps_only_the_three_newest_points() {
        let root = temporary_test_dir("prune-history");
        let history = history_root(&root);
        std::fs::create_dir_all(&history).unwrap();
        for index in 1..=5 {
            std::fs::create_dir_all(history.join(format!("restore-{index:02}-1"))).unwrap();
        }

        prune_restore_points(&root, MAX_RESTORE_POINTS).unwrap();

        let mut names = std::fs::read_dir(&history)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, ["restore-03-1", "restore-04-1", "restore-05-1"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn router_fields_are_removed_from_fallback_official_config() {
        let input = r#"model_provider = "sub2api"
model = "kimi-k2"
model_catalog_json = "catalog.json"
approval_policy = "on-request"

[model_providers.sub2api]
name = "Codex-Router"
base_url = "http://127.0.0.1:18080/v1"

[windows]
sandbox = "unelevated"
"#;
        let output = sanitize_router_config(input);
        assert!(!output.contains("model_provider"));
        assert!(!output.contains("kimi-k2"));
        assert!(!output.contains("model_providers.sub2api"));
        assert!(output.contains("approval_policy"));
        assert!(output.contains("[windows]"));
    }

    #[test]
    fn custom_router_fields_and_local_bearer_are_removed_from_official_fallback() {
        let input = r#"model_provider = "custom"
model = "gpt-5.6-sol"
model_catalog_json = "catalog.json"
model_reasoning_effort = "max"
service_tier = "fast"
approval_policy = "never"

[models.new_thread]
model = "gpt-5.6-sol"
model_reasoning_effort = "max"
service_tier = "fast"

[features]
fast_mode = true

[desktop]
enabled-reasoning-efforts = ["low", "medium", "high", "xhigh", "ultra", "max"]

[model_providers.custom]
name = "Codex-Router"
base_url = "http://127.0.0.1:18080/v1"
requires_openai_auth = true
experimental_bearer_token = "local-router-secret"
"#;
        let output = sanitize_router_config(input);
        assert!(output.contains("approval_policy = \"never\""));
        for removed in [
            "model_provider",
            "gpt-5.6-sol",
            "model_catalog_json",
            "model_reasoning_effort",
            "service_tier",
            "fast_mode",
            "enabled-reasoning-efforts",
            "model_providers.custom",
            "local-router-secret",
        ] {
            assert!(!output.contains(removed), "Router field remains: {removed}");
        }
    }

    #[test]
    fn restored_config_cannot_reintroduce_elevated_windows_sandbox() {
        let input = "model = \"custom\"\n[windows]\nsandbox = \"elevated\"\n";
        let output = logic::normalize_windows_sandbox_config(input);
        assert!(output.contains("sandbox = \"unelevated\""));
        assert!(!output.contains("sandbox = \"elevated\""));
    }

    #[test]
    fn restore_points_round_trip_without_auth() {
        let root = temporary_test_dir("restore");
        let codex_home = root.join("codex-home");
        std::fs::create_dir_all(&codex_home).unwrap();
        std::fs::write(codex_home.join("config.toml"), "model = \"first\"\n").unwrap();
        let mut cfg = RouterConfig::default();
        cfg.deploy.codex_home = codex_home.display().to_string();
        let point = capture_restore_point(&root, &cfg, "before test").unwrap();
        std::fs::write(codex_home.join("config.toml"), "model = \"second\"\n").unwrap();
        let (restored, _) = restore_point_config(&root, &point, &cfg, false).unwrap();
        assert_eq!(restored.deploy.codex_home, cfg.deploy.codex_home);
        assert_eq!(
            std::fs::read_to_string(codex_home.join("config.toml")).unwrap(),
            "model = \"first\"\n\n[windows]\nsandbox = \"unelevated\"\n"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn oauth_snapshot_is_dpapi_protected_and_restored_for_current_user() {
        let root = temporary_test_dir("dpapi");
        let source = root.join("auth.json");
        let protected = root.join("auth.json.dpapi");
        let restored = root.join("restored-auth.json");
        let content = br#"{"auth_mode":"chatgpt","tokens":{"access_token":"not-a-real-token"}}"#;
        std::fs::write(&source, content).unwrap();
        logic::protect_file_for_current_user(&source, &protected).unwrap();
        assert_ne!(std::fs::read(&protected).unwrap(), content);
        logic::unprotect_file_for_current_user(&protected, &restored).unwrap();
        assert_eq!(std::fs::read(&restored).unwrap(), content);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn official_restore_keeps_valid_chatgpt_auth() {
        let root = temporary_test_dir("official-auth");
        let codex_home = root.join("codex-home");
        std::fs::create_dir_all(&codex_home).unwrap();
        std::fs::write(
            codex_home.join("config.toml"),
            "approval_policy = \"on-request\"\n",
        )
        .unwrap();
        let valid_auth = r#"{"auth_mode":"chatgpt","tokens":{"access_token":"not-real"}}"#;
        std::fs::write(codex_home.join("auth.json"), valid_auth).unwrap();
        let mut cfg = RouterConfig::default();
        cfg.deploy.codex_home = codex_home.display().to_string();
        ensure_original_codex_snapshot(&root, &cfg).unwrap();
        std::fs::write(codex_home.join("auth.json"), "{}").unwrap();
        assert!(
            restore_original_codex(&root, &cfg, false)
                .unwrap()
                .auth_available
        );
        assert_eq!(
            std::fs::read_to_string(codex_home.join("auth.json")).unwrap(),
            valid_auth
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shared_official_restore_preserves_same_account_tasks_auth_and_settings() {
        let root = temporary_test_dir("shared-same-account");
        let codex_home = root.join("codex-home");
        let sessions = codex_home.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            codex_home.join("config.toml"),
            "model = \"gpt-official\"\napproval_policy = \"on-request\"\n",
        )
        .unwrap();
        let old_auth =
            r#"{"auth_mode":"chatgpt","tokens":{"account_id":"account-a","access_token":"old"}}"#;
        std::fs::write(codex_home.join("auth.json"), old_auth).unwrap();
        std::fs::write(sessions.join("shared-task.jsonl"), "task-data").unwrap();
        let mut cfg = RouterConfig::default();
        cfg.deploy.codex_home = codex_home.display().to_string();
        ensure_original_codex_snapshot(&root, &cfg).unwrap();

        let current_config = r#"model_provider = "custom"
model = "router-model"
model_catalog_json = "router-models.json"
model_reasoning_effort = "max"
service_tier = "fast"
approval_policy = "never"
personality = "pragmatic"

[models.new_thread]
model = "router-model"
model_reasoning_effort = "max"
service_tier = "fast"

[features]
fast_mode = true

[desktop]
enabled-reasoning-efforts = ["low", "medium", "high", "xhigh", "ultra", "max"]

[model_providers.custom]
name = "Codex-Router"
base_url = "http://127.0.0.1:18080/v1"
requires_openai_auth = true
experimental_bearer_token = "local-router-secret"
"#;
        let current_auth = r#"{"auth_mode":"chatgpt","tokens":{"account_id":"account-a","access_token":"refreshed"}}"#;
        std::fs::write(codex_home.join("config.toml"), current_config).unwrap();
        std::fs::write(codex_home.join("auth.json"), current_auth).unwrap();

        let outcome = restore_original_codex(&root, &cfg, true).unwrap();

        assert!(outcome.auth_available);
        assert!(outcome.shared_state_preserved);
        assert!(!outcome.account_changed);
        assert_eq!(
            std::fs::read_to_string(codex_home.join("auth.json")).unwrap(),
            current_auth
        );
        let restored_config = std::fs::read_to_string(codex_home.join("config.toml")).unwrap();
        assert!(restored_config.contains("model = \"gpt-official\""));
        assert!(restored_config.contains("approval_policy = \"never\""));
        assert!(restored_config.contains("personality = \"pragmatic\""));
        for removed in [
            "Codex-Router",
            "router-models.json",
            "local-router-secret",
            "models.new_thread",
            "fast_mode",
            "enabled-reasoning-efforts",
        ] {
            assert!(
                !restored_config.contains(removed),
                "Router field remains after official restore: {removed}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(sessions.join("shared-task.jsonl")).unwrap(),
            "task-data"
        );
        let manifest = std::fs::read_to_string(original_root(&root).join("state.json")).unwrap();
        assert!(!manifest.contains("account-a"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shared_restore_uses_full_snapshot_for_a_different_codex_account() {
        let root = temporary_test_dir("shared-different-account");
        let codex_home = root.join("codex-home");
        std::fs::create_dir_all(&codex_home).unwrap();
        std::fs::write(
            codex_home.join("config.toml"),
            "model = \"account-a-model\"\napproval_policy = \"on-request\"\n",
        )
        .unwrap();
        let account_a =
            r#"{"auth_mode":"chatgpt","tokens":{"account_id":"account-a","access_token":"old"}}"#;
        std::fs::write(codex_home.join("auth.json"), account_a).unwrap();
        let mut cfg = RouterConfig::default();
        cfg.deploy.codex_home = codex_home.display().to_string();
        ensure_original_codex_snapshot(&root, &cfg).unwrap();

        std::fs::write(
            codex_home.join("config.toml"),
            "model_provider = \"sub2api\"\nmodel = \"account-b-router\"\napproval_policy = \"never\"\n\n[model_providers.sub2api]\nbase_url = \"http://127.0.0.1:18080/v1\"\n",
        )
        .unwrap();
        let account_b = r#"{"auth_mode":"chatgpt","tokens":{"account_id":"account-b","access_token":"current"}}"#;
        std::fs::write(codex_home.join("auth.json"), account_b).unwrap();

        let outcome = restore_original_codex(&root, &cfg, true).unwrap();

        assert!(outcome.auth_available);
        assert!(!outcome.shared_state_preserved);
        assert!(outcome.account_changed);
        assert_eq!(
            std::fs::read_to_string(codex_home.join("auth.json")).unwrap(),
            account_a
        );
        let restored_config = std::fs::read_to_string(codex_home.join("config.toml")).unwrap();
        assert!(restored_config.contains("model = \"account-a-model\""));
        assert!(restored_config.contains("approval_policy = \"on-request\""));
        assert!(!restored_config.contains("account-b-router"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_and_cc_profiles_get_independent_credentials_and_names() {
        let root = temporary_test_dir("multiple");
        let codex_home = root.join("codex-home");
        std::fs::create_dir_all(&codex_home).unwrap();
        std::fs::write(codex_home.join("config.toml"), "model = \"test\"\n").unwrap();
        let mut cfg = RouterConfig::default();
        cfg.deploy.codex_home = codex_home.display().to_string();
        std::fs::write(
            codex_home.join("auth.json"),
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"not-real"}}"#,
        )
        .unwrap();
        let cc_db = root.join("cc-switch.db");
        let connection = rusqlite::Connection::open(&cc_db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE providers (id TEXT PRIMARY KEY, app_type TEXT NOT NULL, name TEXT NOT NULL, settings_config TEXT NOT NULL);",
            )
            .unwrap();
        drop(connection);
        cfg.deploy.cc_switch_db = cc_db.display().to_string();
        cfg.models.push(crate::config::ModelConfig {
            model: "kimi-k2.5".into(),
            base_url: "https://example.invalid/v1".into(),
            api_key: "not-a-real-key".into(),
            ..crate::config::ModelConfig::default()
        });

        let (local, local_cfg) =
            create_profile(&root, "Kimi local", IsolationKind::Local, &cfg).unwrap();
        let (cc, cc_cfg) = create_profile(&root, "Kimi CC", IsolationKind::CcSwitch, &cfg).unwrap();
        assert_ne!(local.id, cc.id);
        assert_ne!(
            local_cfg.models[0].credential_name,
            cc_cfg.models[0].credential_name
        );
        assert!(!local_cfg.deploy.cc_switch_sync);
        assert!(cc_cfg.deploy.cc_switch_sync);
        assert_eq!(cc_cfg.deploy.cc_switch_profile_name, "Kimi CC");
        assert_eq!(list_profiles(&root).unwrap().len(), 2);
        let saved = load_profile_config(&root, &local).unwrap();
        assert!(saved.models[0].api_key.is_empty());
        logic::remove_isolated_profile_credentials(&[
            local_cfg.models[0].credential_name.clone(),
            cc_cfg.models[0].credential_name.clone(),
        ])
        .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn profiles_keep_independent_oauth_bindings_after_update() {
        let root = temporary_test_dir("oauth-bindings");
        let codex_home = root.join("codex-home");
        std::fs::create_dir_all(&codex_home).unwrap();
        std::fs::write(codex_home.join("config.toml"), "model = \"test\"\n").unwrap();

        let mut first_config = RouterConfig::default();
        first_config.deploy.codex_home = codex_home.display().to_string();
        first_config.oauth_account_ids = Some(vec![11]);
        let (first, mut first_config) =
            create_profile(&root, "OAuth first", IsolationKind::Local, &first_config).unwrap();

        let mut second_config = first_config.clone();
        second_config.oauth_account_ids = Some(vec![22, 23]);
        let (second, _) =
            create_profile(&root, "OAuth second", IsolationKind::Local, &second_config).unwrap();

        first_config.oauth_account_ids = Some(vec![31, 32]);
        update_profile_state(&root, &first.id, &first_config).unwrap();

        assert_eq!(
            load_profile_config(&root, &first)
                .unwrap()
                .oauth_account_ids,
            Some(vec![31, 32])
        );
        assert_eq!(
            load_profile_config(&root, &second)
                .unwrap()
                .oauth_account_ids,
            Some(vec![22, 23])
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn oauth_auto_enrollment_updates_only_the_target_profiles_oauth_fields() {
        let root = temporary_test_dir("profile-oauth-auto-enrollment");
        let mut config = RouterConfig {
            oauth_account_ids: Some(vec![3]),
            oauth_seen_account_ids: vec![3],
            default_model: "before-save".into(),
            ..Default::default()
        };
        let (profile, _) =
            create_profile(&root, "OAuth profile", IsolationKind::Local, &config).unwrap();

        config.default_model = "unsaved-edit".into();
        update_profile_oauth_selection(&root, &profile.id, Some(vec![3, 9]), vec![3, 9]).unwrap();

        let saved = load_profile_config(&root, &profile).unwrap();
        assert_eq!(saved.oauth_account_ids, Some(vec![3, 9]));
        assert_eq!(saved.oauth_seen_account_ids, vec![3, 9]);
        assert_eq!(saved.default_model, "before-save");
        std::fs::remove_dir_all(root).unwrap();
    }
}
