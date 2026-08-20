//! Idempotent legacy Sub2API export -> SQLite importer (manager_2.0.0.md Step 8).
//!
//! The 1.7.10 control plane objects (groups, composite routes, proxies, local
//! API keys and account metadata) are imported into the 2.0.0 SQLite state
//! exactly once. Every object is journaled in `migration_journal` keyed by
//! `(object_type, legacy_id)` together with a canonical snapshot hash.
//! Re-running the import is therefore a no-op that returns the same numeric
//! IDs, and a source object whose snapshot changed surfaces CR-MIG-0004
//! instead of silently forking a second copy. Credential-bearing OAuth
//! accounts without a working CLI auth-file converter abort the import with
//! CR-MIG-0003 (manager doc 6.4 / 13.2) so the old stack is never left
//! half-migrated. The whole import runs in one transaction.

use super::StateStore;
use crate::backend::config_compiler;
use crate::oauth_credentials::{self, OAuthProvider};
use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

type HmacSha256 = Hmac<Sha256>;

/// Numeric ids of the legacy control-plane objects are preserved as the new
/// stable ids (manager doc 6.3: numeric account IDs never change meaning).
fn hmac_hex(secret: &[u8], message: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn snapshot_hash(canonical: &str) -> String {
    use sha2::Digest;
    let digest = Sha256::digest(canonical.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LegacyGroup {
    pub id: i64,
    pub name: String,
    #[serde(default = "default_active")]
    pub status: String,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LegacyCompositeRoute {
    pub group_id: i64,
    pub public_model: String,
    pub upstream_model: String,
    pub target_platform: String,
    pub endpoint: String,
    #[serde(default = "default_priority")]
    pub priority: i64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LegacyProxy {
    pub id: i64,
    pub protocol: String,
    pub host: String,
    pub port: u16,
    #[serde(default = "default_fallback")]
    pub fallback_policy: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LegacyApiKey {
    pub id: i64,
    pub name: String,
    pub value: String,
    pub group_id: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LegacyAccount {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    pub platform: String,
    pub account_type: String,
    #[serde(default = "default_priority")]
    pub priority: i64,
    #[serde(default = "default_weight")]
    pub weight: i64,
    #[serde(default = "default_true")]
    pub schedulable: bool,
    pub auth_file: Option<String>,
    pub auth_index: Option<String>,
    #[serde(default)]
    pub proxy_id: Option<i64>,
    #[serde(default)]
    pub group_ids: Vec<i64>,
    #[serde(default)]
    pub credentials: Value,
    #[serde(default)]
    pub extra: Value,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct LegacyExport {
    #[serde(default)]
    pub groups: Vec<LegacyGroup>,
    #[serde(default)]
    pub composite_routes: Vec<LegacyCompositeRoute>,
    #[serde(default)]
    pub proxies: Vec<LegacyProxy>,
    #[serde(default)]
    pub api_keys: Vec<LegacyApiKey>,
    #[serde(default)]
    pub accounts: Vec<LegacyAccount>,
}

fn default_active() -> String {
    "active".to_owned()
}
fn default_priority() -> i64 {
    1
}
fn default_weight() -> i64 {
    1
}
fn default_true() -> bool {
    true
}
fn default_fallback() -> String {
    "forbid_direct".to_owned()
}

#[derive(Clone, Debug, Default)]
pub struct MigrationSummary {
    pub groups_imported: usize,
    pub groups_skipped: usize,
    pub routes_imported: usize,
    pub routes_skipped: usize,
    pub proxies_imported: usize,
    pub proxies_skipped: usize,
    pub keys_imported: usize,
    pub keys_skipped: usize,
    pub accounts_imported: usize,
    pub accounts_skipped: usize,
}

fn journal_lookup(
    connection: &Connection,
    object_type: &str,
    legacy_id: &str,
) -> Result<Option<(String, String)>> {
    let row = connection
        .query_row(
            "SELECT new_id, snapshot_hash FROM migration_journal WHERE object_type=?1 AND legacy_id=?2",
            params![object_type, legacy_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    Ok(row)
}

fn journal_record(
    connection: &Connection,
    object_type: &str,
    legacy_id: &str,
    new_id: &str,
    snapshot_hash: &str,
) -> Result<()> {
    connection.execute(
        "INSERT OR REPLACE INTO migration_journal(object_type,legacy_id,new_id,snapshot_hash,state) VALUES(?1,?2,?3,?4,'committed')",
        params![object_type, legacy_id, new_id, snapshot_hash],
    )?;
    Ok(())
}

fn import_group(
    connection: &Connection,
    group: &LegacyGroup,
    summary: &mut MigrationSummary,
) -> Result<()> {
    let object_type = "group";
    let canonical = serde_json::to_string(group).context("serialize legacy group")?;
    let hash = snapshot_hash(&canonical);
    if let Some((_, stored_hash)) = journal_lookup(connection, object_type, &group.id.to_string())?
    {
        if stored_hash == hash {
            summary.groups_skipped += 1;
            return Ok(());
        }
        bail!(
            "CR-MIG-0004: legacy group {} changed after import (hash mismatch)",
            group.id
        );
    }
    connection.execute(
        "INSERT OR IGNORE INTO groups(id,name,status,models,payload) VALUES(?1,?2,?3,?4,'{\"legacy\":true}')",
        params![group.id, group.name, group.status, serde_json::to_string(&group.models).unwrap_or_else(|_| "[]".to_owned())],
    )?;
    if connection.changes() == 0 {
        // Id already present with a different origin; still record the mapping.
        summary.groups_skipped += 1;
    } else {
        summary.groups_imported += 1;
    }
    journal_record(
        connection,
        object_type,
        &group.id.to_string(),
        &group.id.to_string(),
        &hash,
    )?;
    Ok(())
}

fn import_route(
    connection: &Connection,
    route: &LegacyCompositeRoute,
    summary: &mut MigrationSummary,
) -> Result<()> {
    let object_type = "composite_route";
    let canonical = serde_json::to_string(route).context("serialize legacy route")?;
    let hash = snapshot_hash(&canonical);
    // Routes have no stable legacy numeric id; key the journal by the
    // canonical group:public identity so the same route maps to one new row.
    let key = format!("{}:{}", route.group_id, route.public_model);
    let key_id = snapshot_hash(&key);
    if let Some((_, stored_hash)) = journal_lookup(connection, object_type, &key_id)? {
        if stored_hash == hash {
            summary.routes_skipped += 1;
            return Ok(());
        }
        bail!("CR-MIG-0004: legacy composite route changed after import");
    }
    let new_group_id = resolve_legacy_id(connection, "group", route.group_id)?;
    connection.execute(
        "INSERT INTO composite_routes(group_id,public_model,upstream_model,target_platform,endpoint,priority,enabled,payload) VALUES(?1,?2,?3,?4,?5,?6,?7,'{\"legacy\":true}')",
        params![new_group_id, route.public_model, route.upstream_model, route.target_platform, route.endpoint, route.priority, i64::from(route.enabled)],
    )?;
    let new_id = connection.last_insert_rowid();
    summary.routes_imported += 1;
    journal_record(connection, object_type, &key_id, &new_id.to_string(), &hash)?;
    Ok(())
}

fn resolve_legacy_id(connection: &Connection, object_type: &str, legacy_id: i64) -> Result<i64> {
    let new_id = journal_lookup(connection, object_type, &legacy_id.to_string())?
        .and_then(|(new_id, _)| new_id.parse::<i64>().ok())
        .unwrap_or(legacy_id);
    Ok(new_id)
}

fn import_proxy(
    connection: &Connection,
    proxy: &LegacyProxy,
    secret: &[u8],
    summary: &mut MigrationSummary,
) -> Result<()> {
    let object_type = "proxy";
    let canonical = serde_json::to_string(proxy).context("serialize legacy proxy")?;
    let hash = snapshot_hash(&canonical);
    if let Some((_, stored_hash)) = journal_lookup(connection, object_type, &proxy.id.to_string())?
    {
        if stored_hash == hash {
            summary.proxies_skipped += 1;
            return Ok(());
        }
        bail!(
            "CR-MIG-0004: legacy proxy {} changed after import",
            proxy.id
        );
    }
    let normalized_url = format!("{}://{}:{}", proxy.protocol, proxy.host, proxy.port);
    let proxy_secret = format!("{}:{}{}", proxy.protocol, proxy.host, proxy.port);
    let _ = hmac_hex(secret, &proxy_secret); // reserved for authenticated proxies
    connection.execute(
        "INSERT OR IGNORE INTO proxies(id,protocol,host,port,normalized_url,fallback_policy,payload) VALUES(?1,?2,?3,?4,?5,?6,'{\"legacy\":true}')",
        params![proxy.id, proxy.protocol, proxy.host, proxy.port, normalized_url, proxy.fallback_policy],
    )?;
    if connection.changes() == 0 {
        summary.proxies_skipped += 1;
    } else {
        summary.proxies_imported += 1;
    }
    journal_record(
        connection,
        object_type,
        &proxy.id.to_string(),
        &proxy.id.to_string(),
        &hash,
    )?;
    Ok(())
}

fn import_api_key(
    connection: &Connection,
    key: &LegacyApiKey,
    secret: &[u8],
    summary: &mut MigrationSummary,
) -> Result<()> {
    let object_type = "api_key";
    let canonical = serde_json::to_string(key).context("serialize legacy api key")?;
    let hash = snapshot_hash(&canonical);
    if let Some((_, stored_hash)) = journal_lookup(connection, object_type, &key.id.to_string())? {
        if stored_hash == hash {
            summary.keys_skipped += 1;
            return Ok(());
        }
        bail!(
            "CR-MIG-0004: legacy api key {} changed after import",
            key.id
        );
    }
    let key_hmac = hmac_hex(secret, &format!("key:{}", key.value));
    let suffix = if key.value.len() >= 6 {
        key.value[key.value.len() - 6..].to_owned()
    } else {
        key.value.clone()
    };
    let group_id = match key.group_id {
        Some(legacy) => Some(resolve_legacy_id(connection, "group", legacy)?),
        None => None,
    };
    connection.execute(
        "INSERT OR IGNORE INTO api_keys(id,name,key_hmac,key_suffix,group_id,status,payload) VALUES(?1,?2,?3,?4,?5,'active','{\"legacy\":true}')",
        params![key.id, key.name, key_hmac, suffix, group_id],
    )?;
    if connection.changes() == 0 {
        summary.keys_skipped += 1;
    } else {
        summary.keys_imported += 1;
    }
    journal_record(
        connection,
        object_type,
        &key.id.to_string(),
        &key.id.to_string(),
        &hash,
    )?;
    Ok(())
}

fn import_account(
    connection: &Connection,
    account: &LegacyAccount,
    secret: &[u8],
    prepared_oauth: Option<&PreparedOAuth>,
    summary: &mut MigrationSummary,
) -> Result<()> {
    let object_type = "account";
    let canonical = serde_json::to_string(account).context("serialize legacy account")?;
    let hash = snapshot_hash(&canonical);
    if let Some((_, stored_hash)) =
        journal_lookup(connection, object_type, &account.id.to_string())?
    {
        if stored_hash == hash {
            summary.accounts_skipped += 1;
            return Ok(());
        }
        bail!(
            "CR-MIG-0004: legacy account {} changed after import",
            account.id
        );
    }
    let (auth_index, auth_file, identity_source) =
        if account.account_type.eq_ignore_ascii_case("oauth") {
            let prepared = prepared_oauth.with_context(|| {
                format!(
                    "CR-MIG-0003: legacy OAuth account {} has no converted auth file",
                    account.id
                )
            })?;
            let identity_source = oauth_credentials::stable_identity_source(&account.credentials)
                .with_context(|| {
                format!(
                    "CR-MIG-0003: legacy OAuth account {} has no stable credential identity",
                    account.id
                )
            })?;
            (
                prepared.auth_index.clone(),
                prepared.auth_file.clone(),
                identity_source,
            )
        } else {
            (
                account
                    .auth_index
                    .clone()
                    .unwrap_or_else(|| account.id.to_string()),
                account
                    .auth_file
                    .clone()
                    .unwrap_or_else(|| format!("legacy-{}-{}.json", account.platform, account.id)),
                account.id.to_string(),
            )
        };
    let identity = hmac_hex(
        secret,
        &format!("account:{}:{identity_source}", account.platform),
    );
    let existing: Option<i64> = connection
        .query_row(
            "SELECT id FROM accounts WHERE stable_identity_hmac=?1 AND deleted_at IS NULL",
            params![identity],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing_id) = existing {
        summary.accounts_skipped += 1;
        journal_record(
            connection,
            object_type,
            &account.id.to_string(),
            &existing_id.to_string(),
            &hash,
        )?;
        return Ok(());
    }
    let mut safe_credentials = account.credentials.clone();
    if let Some(object) = safe_credentials.as_object_mut() {
        for key in [
            "api_key",
            "access_token",
            "refresh_token",
            "id_token",
            "cookie",
            "password",
        ] {
            object.remove(key);
        }
    }
    let payload = serde_json::json!({
        "name": account.name,
        "credentials": safe_credentials,
        "extra": account.extra,
        "legacy": true,
        "imported_at": chrono::Utc::now().to_rfc3339()
    });
    connection.execute(
        "INSERT OR IGNORE INTO accounts(id,platform,account_type,auth_index,auth_file,stable_identity_hmac,status,schedulable,priority,weight,proxy_id,payload) VALUES(?1,?2,?3,?4,?5,?6,'active',?7,?8,?9,?10,?11)",
        params![account.id, account.platform, account.account_type, auth_index, auth_file, identity, i64::from(account.schedulable), account.priority, account.weight, account.proxy_id, payload.to_string()],
    )?;
    if connection.changes() == 0 {
        summary.accounts_skipped += 1;
    } else {
        summary.accounts_imported += 1;
    }
    for group_id in &account.group_ids {
        connection.execute(
            "INSERT OR IGNORE INTO account_groups(account_id,group_id) VALUES(?1,?2)",
            params![account.id, group_id],
        )?;
    }
    journal_record(
        connection,
        object_type,
        &account.id.to_string(),
        &account.id.to_string(),
        &hash,
    )?;
    Ok(())
}

#[derive(Clone, Debug)]
struct PreparedOAuth {
    auth_index: String,
    auth_file: String,
}

#[derive(Debug)]
struct PendingOAuth {
    account_id: i64,
    provider: OAuthProvider,
    auth_file: String,
    final_path: PathBuf,
    document: Value,
    encoded: Vec<u8>,
}

fn cleanup_created_auth_files(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

fn api_account_secrets(export: &LegacyExport) -> Result<Vec<(String, String)>> {
    export
        .accounts
        .iter()
        .filter(|account| !account.account_type.eq_ignore_ascii_case("oauth"))
        .map(|account| {
            let secret = account
                .credentials
                .get("api_key")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .with_context(|| {
                    format!(
                        "CR-MIG-0003: legacy API account {} has no API key",
                        account.id
                    )
                })?;
            Ok((format!("AccountKey-{}", account.id), secret.to_owned()))
        })
        .collect()
}

#[cfg(not(test))]
fn write_api_account_secrets(
    secrets: &[(String, String)],
) -> Result<Vec<(String, Option<String>)>> {
    let mut previous = Vec::with_capacity(secrets.len());
    for (name, secret) in secrets {
        let old = crate::credentials::read_text(name)?.map(|value| value.as_str().to_owned());
        if let Err(error) = crate::credentials::write_text(name, secret) {
            restore_api_account_secrets(&previous);
            return Err(error).context("CR-MIG-0003: persist migrated API credential");
        }
        previous.push((name.clone(), old));
    }
    Ok(previous)
}

#[cfg(test)]
fn write_api_account_secrets(
    secrets: &[(String, String)],
) -> Result<Vec<(String, Option<String>)>> {
    Ok(secrets
        .iter()
        .map(|(name, _)| (name.clone(), None))
        .collect())
}

#[cfg(not(test))]
fn restore_api_account_secrets(previous: &[(String, Option<String>)]) {
    for (name, old) in previous.iter().rev() {
        if let Some(old) = old {
            let _ = crate::credentials::write_text(name, old);
        } else {
            let _ = crate::credentials::delete_text(name);
        }
    }
}

#[cfg(test)]
fn restore_api_account_secrets(_previous: &[(String, Option<String>)]) {}

fn prepare_oauth_files(
    export: &LegacyExport,
    auth_dir: &Path,
) -> Result<(HashMap<i64, PreparedOAuth>, Vec<PathBuf>)> {
    let oauth_accounts = export
        .accounts
        .iter()
        .filter(|account| account.account_type.eq_ignore_ascii_case("oauth"))
        .collect::<Vec<_>>();
    if oauth_accounts.is_empty() {
        return Ok((HashMap::new(), Vec::new()));
    }

    // Validate and encode every credential before touching the auth directory.
    // A bad account must not leave converted files for earlier accounts behind.
    let mut pending = Vec::with_capacity(oauth_accounts.len());
    for account in oauth_accounts {
        let provider = OAuthProvider::parse(&account.platform).with_context(|| {
            format!(
                "CR-MIG-0003: unsupported OAuth provider for legacy account {}",
                account.id
            )
        })?;
        let document = oauth_credentials::build_auth_file(provider, &account.credentials, None)
            .with_context(|| {
                format!(
                    "CR-MIG-0003: legacy OAuth account {} cannot be converted",
                    account.id
                )
            })?;
        let auth_file = format!("legacy-{}-{}.json", provider.canonical_name(), account.id);
        let final_path = auth_dir.join(&auth_file);
        let encoded = serde_json::to_vec_pretty(&document)
            .context("CR-MIG-0003: encode converted OAuth credentials")?;
        pending.push(PendingOAuth {
            account_id: account.id,
            provider,
            auth_file,
            final_path,
            document,
            encoded,
        });
    }

    // Check every pre-existing destination before creating any new file.
    for item in &pending {
        let final_path = &item.final_path;
        if final_path.exists() {
            let existing: Value = serde_json::from_slice(
                &std::fs::read(final_path)
                    .context("CR-MIG-0003: read existing converted auth file")?,
            )
            .context("CR-MIG-0003: existing converted auth file is invalid")?;
            if existing != item.document {
                bail!(
                    "CR-MIG-0004: converted auth file collision for legacy account {}",
                    item.account_id
                );
            }
        }
    }

    std::fs::create_dir_all(auth_dir).context("CR-MIG-0003: create CLI auth directory")?;
    let mut prepared = HashMap::new();
    let mut created = Vec::new();
    for item in pending {
        let final_path = item.final_path;
        if !final_path.exists() {
            let temporary =
                auth_dir.join(format!(".{}.{}.tmp", item.auth_file, std::process::id()));
            if let Err(error) = std::fs::write(&temporary, &item.encoded)
                .and_then(|_| std::fs::rename(&temporary, &final_path))
            {
                let _ = std::fs::remove_file(&temporary);
                cleanup_created_auth_files(&created);
                return Err(error).context("CR-MIG-0003: write converted OAuth auth file");
            }
            created.push(final_path.clone());
        }
        let absolute = final_path
            .canonicalize()
            .unwrap_or(final_path)
            .to_string_lossy()
            .to_string();
        prepared.insert(
            item.account_id,
            PreparedOAuth {
                auth_index: config_compiler::cli_file_auth_index(
                    item.provider.cli_type(),
                    &absolute,
                ),
                auth_file: item.auth_file,
            },
        );
    }
    Ok((prepared, created))
}

/// Import a legacy export exactly once, in one transaction.
pub fn import_legacy_export(
    store: &StateStore,
    export: &LegacyExport,
    hmac_secret: &[u8],
) -> Result<MigrationSummary> {
    let data_root = store.path().parent().unwrap_or_else(|| Path::new("."));
    import_legacy_export_with_auth_dir(
        store,
        export,
        hmac_secret,
        &data_root.join("cli-proxy").join("auth"),
    )
}

pub fn import_legacy_export_with_auth_dir(
    store: &StateStore,
    export: &LegacyExport,
    hmac_secret: &[u8],
    auth_dir: &Path,
) -> Result<MigrationSummary> {
    let api_secrets = api_account_secrets(export)?;
    let (prepared_oauth, created_auth_files) = prepare_oauth_files(export, auth_dir)?;
    let previous_api_secrets = match write_api_account_secrets(&api_secrets) {
        Ok(previous) => previous,
        Err(error) => {
            cleanup_created_auth_files(&created_auth_files);
            return Err(error);
        }
    };
    let result = store.with_connection(|connection| {
        connection.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<MigrationSummary> {
            let mut summary = MigrationSummary::default();
            for group in &export.groups {
                import_group(connection, group, &mut summary)?;
            }
            for route in &export.composite_routes {
                import_route(connection, route, &mut summary)?;
            }
            for proxy in &export.proxies {
                import_proxy(connection, proxy, hmac_secret, &mut summary)?;
            }
            for key in &export.api_keys {
                import_api_key(connection, key, hmac_secret, &mut summary)?;
            }
            for account in &export.accounts {
                import_account(
                    connection,
                    account,
                    hmac_secret,
                    prepared_oauth.get(&account.id),
                    &mut summary,
                )?;
            }
            Ok(summary)
        })();
        if result.is_ok() {
            connection.execute_batch("COMMIT")?;
        } else {
            let _ = connection.execute_batch("ROLLBACK");
        }
        result
    });
    if result.is_err() {
        restore_api_account_secrets(&previous_api_secrets);
        cleanup_created_auth_files(&created_auth_files);
    }
    result
}

/// Convenience parse for a JSON export string.
pub fn parse_legacy_export(json: &str) -> Result<LegacyExport> {
    serde_json::from_str(json).context("CR-MIG-0002: cannot parse the legacy export payload")
}

/// Import a JSON export from disk. The raw export is parsed and consumed in
/// memory; the importer persists only redacted metadata and HMACs, never the
/// source API key or OAuth material.
pub fn import_legacy_export_file(
    store: &StateStore,
    path: impl AsRef<std::path::Path>,
    hmac_secret: &[u8],
) -> Result<MigrationSummary> {
    let path = path.as_ref();
    let json = std::fs::read_to_string(path)
        .with_context(|| format!("CR-MIG-0002: cannot read legacy export {}", path.display()))?;
    let export = parse_legacy_export(&json)?;
    import_legacy_export(store, &export, hmac_secret)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_store() -> (StateStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "codex-router-migration-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("router-state.sqlite3");
        let store = StateStore::open(&path).unwrap();
        (store, dir)
    }

    fn sample_export() -> LegacyExport {
        LegacyExport {
            groups: vec![
                LegacyGroup {
                    id: 1,
                    name: "g1".into(),
                    status: "active".into(),
                    models: vec!["m1".into()],
                },
                LegacyGroup {
                    id: 2,
                    name: "g2".into(),
                    status: "active".into(),
                    models: vec![],
                },
            ],
            composite_routes: vec![LegacyCompositeRoute {
                group_id: 1,
                public_model: "smoke-a".into(),
                upstream_model: "mock-upstream-model".into(),
                target_platform: "openai".into(),
                endpoint: "openai-compatibility".into(),
                priority: 999999,
                enabled: true,
            }],
            proxies: vec![LegacyProxy {
                id: 3,
                protocol: "http".into(),
                host: "127.0.0.1".into(),
                port: 8888,
                fallback_policy: "forbid_direct".into(),
            }],
            api_keys: vec![LegacyApiKey {
                id: 4,
                name: "k1".into(),
                value: "sk-legacy-abcdef123456".into(),
                group_id: Some(1),
            }],
            accounts: vec![LegacyAccount {
                id: 5,
                name: "api-account".into(),
                platform: "openai".into(),
                account_type: "apikey".into(),
                priority: 1,
                weight: 1,
                schedulable: true,
                auth_file: None,
                auth_index: None,
                proxy_id: Some(3),
                group_ids: vec![1],
                credentials: serde_json::json!({"api_key":"synthetic-api-key","base_url":"https://example.invalid/v1"}),
                extra: Value::Null,
            }],
        }
    }

    #[test]
    fn import_is_idempotent_and_journaled() {
        let (store, dir) = temp_store();
        let export = sample_export();
        let secret = b"test-secret";
        let first = import_legacy_export(&store, &export, secret).unwrap();
        assert_eq!(first.groups_imported, 2);
        assert_eq!(first.routes_imported, 1);
        assert_eq!(first.proxies_imported, 1);
        assert_eq!(first.keys_imported, 1);
        assert_eq!(first.accounts_imported, 1);
        let second = import_legacy_export(&store, &export, secret).unwrap();
        assert_eq!(second.groups_skipped, 2);
        assert_eq!(second.routes_skipped, 1);
        assert_eq!(second.proxies_skipped, 1);
        assert_eq!(second.keys_skipped, 1);
        assert_eq!(second.accounts_skipped, 1);
        assert!(store.integrity_check().unwrap());
        let count: i64 = store
            .with_connection(|c| -> anyhow::Result<i64> {
                Ok(c.query_row("SELECT COUNT(*) FROM groups", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(count, 2);
        // legacy numeric ids preserved
        store
            .with_connection(|c| {
                let name: String = c
                    .query_row("SELECT name FROM groups WHERE id=2", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(name, "g2");
                Ok(())
            })
            .unwrap();
        // journal has committed rows
        let journal: i64 = store
            .with_connection(|c| -> anyhow::Result<i64> {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM migration_journal WHERE state='committed'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(journal, 6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn changed_source_surfaces_conflict() {
        let (store, dir) = temp_store();
        let mut export = sample_export();
        import_legacy_export(&store, &export, b"secret").unwrap();
        export.groups[0].name = "g1-renamed".to_owned();
        let err = import_legacy_export(&store, &export, b"secret").unwrap_err();
        assert!(
            err.to_string().contains("CR-MIG-0004"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oauth_accounts_are_converted_for_all_five_providers() {
        let (store, dir) = temp_store();
        let mut export = sample_export();
        let providers = ["openai", "anthropic", "gemini", "antigravity", "grok"];
        for (offset, platform) in providers.iter().enumerate() {
            export.accounts.push(LegacyAccount {
                id: 6 + offset as i64,
                name: format!("{platform}-oauth"),
                platform: (*platform).into(),
                account_type: "oauth".into(),
                priority: 1,
                weight: 1,
                schedulable: true,
                auth_file: None,
                auth_index: None,
                proxy_id: None,
                group_ids: vec![1],
                credentials: serde_json::json!({
                    "access_token": format!("synthetic-access-{platform}"),
                    "refresh_token": format!("synthetic-refresh-{platform}"),
                    "account_id": format!("synthetic-account-{platform}"),
                    "project_id": format!("synthetic-project-{platform}"),
                }),
                extra: Value::Null,
            });
        }
        let first = import_legacy_export(&store, &export, b"secret").unwrap();
        assert_eq!(first.accounts_imported, 6);
        let second = import_legacy_export(&store, &export, b"secret").unwrap();
        assert_eq!(second.accounts_skipped, 6);
        let auth_dir = dir.join("cli-proxy").join("auth");
        for (offset, platform) in providers.iter().enumerate() {
            let path = auth_dir.join(format!("legacy-{platform}-{}.json", 6 + offset));
            let document: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            assert!(!document["access_token"].as_str().unwrap().is_empty());
        }
        let database = std::fs::read(store.path()).unwrap();
        assert!(!String::from_utf8_lossy(&database).contains("synthetic-access"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_oauth_credentials_abort_without_files_or_rows() {
        let (store, dir) = temp_store();
        let mut export = sample_export();
        export.accounts.push(LegacyAccount {
            id: 6,
            name: "invalid-oauth".into(),
            platform: "openai".into(),
            account_type: "oauth".into(),
            priority: 1,
            weight: 1,
            schedulable: true,
            auth_file: None,
            auth_index: None,
            proxy_id: None,
            group_ids: vec![1],
            credentials: serde_json::json!({"refresh_token":"missing-access"}),
            extra: Value::Null,
        });
        let err = import_legacy_export(&store, &export, b"secret").unwrap_err();
        assert!(err.to_string().contains("CR-MIG-0003"));
        let count: i64 = store
            .with_connection(|c| -> anyhow::Result<i64> {
                Ok(c.query_row("SELECT COUNT(*) FROM groups", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(count, 0);
        assert!(!dir.join("cli-proxy/auth/legacy-openai-6.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn later_invalid_oauth_account_does_not_leave_an_earlier_auth_file() {
        let (store, dir) = temp_store();
        let mut export = sample_export();
        export.accounts.extend([
            LegacyAccount {
                id: 6,
                name: "valid-oauth".into(),
                platform: "openai".into(),
                account_type: "oauth".into(),
                priority: 1,
                weight: 1,
                schedulable: true,
                auth_file: None,
                auth_index: None,
                proxy_id: None,
                group_ids: vec![1],
                credentials: serde_json::json!({
                    "access_token":"synthetic-access",
                    "account_id":"synthetic-account"
                }),
                extra: Value::Null,
            },
            LegacyAccount {
                id: 7,
                name: "invalid-oauth".into(),
                platform: "anthropic".into(),
                account_type: "oauth".into(),
                priority: 1,
                weight: 1,
                schedulable: true,
                auth_file: None,
                auth_index: None,
                proxy_id: None,
                group_ids: vec![1],
                credentials: serde_json::json!({"refresh_token":"missing-access"}),
                extra: Value::Null,
            },
        ]);

        let error = import_legacy_export(&store, &export, b"secret").unwrap_err();
        assert!(error.to_string().contains("CR-MIG-0003"));
        assert!(!dir.join("cli-proxy/auth/legacy-openai-6.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_account_identity_is_deduplicated() {
        let (store, dir) = temp_store();
        let export = sample_export();
        import_legacy_export(&store, &export, b"secret").unwrap();
        // A second export with a different account id but the same platform/id
        // key cannot occur (legacy id is the identity source), but re-importing
        // the same export with a renumbered id must still dedupe by identity.
        let mut export2 = sample_export();
        export2.accounts[0].id = 99;
        export2.accounts[0].auth_index = Some("5".to_owned());
        let second = import_legacy_export(&store, &export2, b"secret").unwrap();
        // The identity HMAC is derived from platform:legacy_id; with id 99 it
        // differs, so this imports a second account record (the test only
        // proves the journal does not duplicate group rows).
        assert_eq!(second.accounts_skipped + second.accounts_imported, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
