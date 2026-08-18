//! Old `/api/v1` compatibility control plane backed by SQLite and CLIProxyAPI.

use crate::backend::cli_proxy::CliProxyManagementClient;
use crate::backend::config_compiler::{self, RouteTarget};
use crate::routing::{PoolRoute, RouteTable};
use crate::state::StateStore;
use crate::telemetry::ledger;
use crate::telemetry::structured_log::StructuredLogger;
use anyhow::{Context, Result};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// Everything the control plane needs to recompile and republish the
/// CLIProxyAPI runtime configuration after a state mutation.
pub struct BackendPaths {
    pub config_path: PathBuf,
    pub auth_dir: PathBuf,
    pub downstream_key: String,
    pub management_secret: String,
    pub cli_port: u16,
}

#[derive(Clone)]
pub struct ControlState {
    pub store: Arc<StateStore>,
    pub cli: CliProxyManagementClient,
    pub logger: Arc<StructuredLogger>,
    pub routes: Arc<RwLock<RouteTable>>,
    pub backend: Option<Arc<BackendPaths>>,
    /// CLI auth index (X-CPA-TRACE-ID middle segment) -> Router account id.
    /// Rebuilt by every `sync_backend`; used for account-level ledger
    /// attribution without ever logging the index itself.
    pub cli_index_map: Arc<RwLock<HashMap<String, i64>>>,
}

pub fn sha256_hex(value: &[u8]) -> String {
    use sha2::Digest;
    let digest = Sha256::digest(value);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn success(data: impl Into<Value>) -> Response {
    let body = json!({"success": true, "data": data.into()});
    response_json(StatusCode::OK, &body, None)
}

pub fn failure(status: StatusCode, code: &str, message: &str) -> Response {
    let body = json!({
        "success": false,
        "message": message,
        "error_code": code,
    });
    response_json(status, &body, Some(code))
}

pub fn response_json(status: StatusCode, body: &Value, error_code: Option<&str>) -> Response {
    let mut response = (status, Json(body.clone())).into_response();
    if let Ok(value) = HeaderValue::from_str(&crate::telemetry::structured_log::request_id()) {
        response.headers_mut().insert("x-request-id", value);
    }
    if let Some(code) = error_code {
        if let Ok(value) = HeaderValue::from_str(code) {
            response
                .headers_mut()
                .insert("x-codex-router-error-code", value);
        }
    }
    response
}

pub fn data(body: &Value) -> Value {
    body.get("data").cloned().unwrap_or_else(|| body.clone())
}

pub fn stable_identity_hmac(platform: &str, identity: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(b"CodexRouter-stable-identity-v2").expect("stable HMAC key");
    mac.update(platform.as_bytes());
    mac.update(&[0]);
    mac.update(identity.as_bytes());
    let digest = mac.finalize().into_bytes();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn deleted_account_tombstone(account_id: i64) -> String {
    stable_identity_hmac(
        "deleted",
        &format!("{account_id}:{}", uuid::Uuid::now_v7().simple()),
    )
}

pub fn sanitize_state_payload(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.retain(|key, _| {
                let key = key.to_ascii_lowercase();
                !matches!(
                    key.as_str(),
                    "api_key"
                        | "apikey"
                        | "access_token"
                        | "refresh_token"
                        | "id_token"
                        | "password"
                        | "authorization"
                        | "cookie"
                        | "sso_token"
                )
            });
            for child in object.values_mut() {
                sanitize_state_payload(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(sanitize_state_payload),
        _ => {}
    }
}

fn json_string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn account_secret_name(id: i64) -> String {
    format!("AccountKey-{id}")
}

fn read_account_secret(id: i64) -> Option<String> {
    crate::credentials::read_text(&account_secret_name(id))
        .ok()
        .flatten()
        .map(|secret| secret.to_string())
        .filter(|secret| !secret.trim().is_empty())
}

/// Deep-merge a partial update into a stored JSON payload. Objects merge
/// recursively; every other value (including explicit null) replaces.
fn merge_payload(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(base_map), Value::Object(patch_map)) => {
            for (key, value) in patch_map {
                merge_payload(base_map.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
        (base, patch) => *base = patch.clone(),
    }
}

/// Rebuild the route table and recompile the CLIProxyAPI configuration from
/// SQLite state. Returns the number of compiled route targets. Best effort by
/// design: callers log the structured error instead of failing the API call.
pub async fn sync_backend(state: &ControlState) -> Result<usize> {
    let Some(backend) = state.backend.clone() else {
        return Ok(0);
    };
    let rows = state.store.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT r.id, r.public_model, r.upstream_model, r.target_platform, r.priority,
                    a.id, a.platform, a.account_type, a.priority, a.weight, a.payload, a.auth_index,
                    a.schedulable, p.normalized_url
             FROM composite_routes r
             JOIN account_groups ag ON ag.group_id = r.group_id
             JOIN accounts a ON a.id = ag.account_id
             LEFT JOIN proxies p ON p.id = CAST(json_extract(a.payload, '$.proxy_id') AS INTEGER)
                AND p.deleted_at IS NULL
             WHERE r.enabled = 1 AND r.deleted_at IS NULL
               AND a.deleted_at IS NULL
             ORDER BY r.id, a.id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, i64>(12)?,
                row.get::<_, Option<String>>(13)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    })?;
    let mut targets = Vec::new();
    let mut pool_routes = Vec::new();
    let mut cli_index_map: HashMap<String, i64> = HashMap::new();
    let mut seen_pools = std::collections::HashSet::new();
    let mut pool_available: std::collections::HashMap<String, bool> =
        std::collections::HashMap::new();
    for (
        route_id,
        public_model,
        upstream_model,
        target_platform,
        route_priority,
        account_id,
        _account_platform,
        account_type,
        account_priority,
        weight,
        payload,
        auth_index,
        schedulable,
        joined_proxy_url,
    ) in rows
    {
        let pool_route_id = format!("r{route_id}");
        let account_available = schedulable != 0;
        pool_available
            .entry(pool_route_id.clone())
            .and_modify(|available| *available |= account_available)
            .or_insert(account_available);
        if seen_pools.insert(pool_route_id.clone()) {
            pool_routes.push(PoolRoute {
                pool_id: config_compiler::pool_id(&pool_route_id, &target_platform),
                prefix: config_compiler::pool_prefix(&pool_route_id, &target_platform),
                public_model: public_model.clone(),
                upstream_model: upstream_model.clone(),
                provider: config_compiler::normalize_platform(&target_platform),
                priority: route_priority as i32,
                enabled: true,
                available: false,
            });
        }
        if !account_available {
            // Disabled accounts never reach the CLI candidate set; the Router
            // availability gate above is what reports CR-RTE-0002 to callers.
            continue;
        }
        let payload: Value = serde_json::from_str(&payload).unwrap_or_else(|_| json!({}));
        if account_type == "oauth" {
            // OAuth credentials live in CLI auth files; pool namespacing is
            // applied by patching the auth file prefix.
            let auth_file = backend.auth_dir.join(format!("{auth_index}.json"));
            if auth_file.is_file() {
                if let Ok(text) = std::fs::read_to_string(&auth_file) {
                    if let Ok(document) = serde_json::from_str::<Value>(&text) {
                        let auth_type = document
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if !auth_type.is_empty() {
                            let cli_index = config_compiler::cli_file_auth_index(
                                auth_type,
                                &auth_file.to_string_lossy(),
                            );
                            cli_index_map.insert(cli_index, account_id);
                        }
                    }
                }
                if let Err(error) = patch_auth_file_prefix(
                    &auth_file,
                    &config_compiler::pool_prefix(&pool_route_id, &target_platform),
                ) {
                    let _ = state.logger.write(json!({"level":"WARN","event":"backend.auth_file_prefix_failed","error_description":error.to_string()}));
                }
            }
            continue;
        }
        let Some(secret) = read_account_secret(account_id) else {
            let _ = state.logger.write(json!({"level":"WARN","event":"backend.account_secret_missing","account_id":account_id}));
            continue;
        };
        let base_url = payload
            .pointer("/credentials/base_url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let proxy_url = joined_proxy_url.or_else(|| {
            payload
                .get("proxy_url")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        let cli_index =
            config_compiler::cli_key_auth_index(&target_platform, base_url.as_deref(), &secret);
        cli_index_map.insert(cli_index, account_id);
        targets.push(RouteTarget {
            route_id: pool_route_id,
            public_model,
            upstream_model,
            platform: target_platform,
            base_url,
            credential_ref: secret,
            priority: account_priority as i32,
            weight: weight as i32,
            proxy_url,
        });
    }
    for route in &mut pool_routes {
        if let Some(id) = route
            .pool_id
            .strip_prefix("cr/")
            .and_then(|tail| tail.split('/').next())
        {
            route.available = pool_available.get(id).copied().unwrap_or(false);
        }
    }
    let table = RouteTable::new(pool_routes)?;
    *state
        .routes
        .write()
        .unwrap_or_else(|error| error.into_inner()) = table;
    *state
        .cli_index_map
        .write()
        .unwrap_or_else(|error| error.into_inner()) = cli_index_map;
    let config = if targets.is_empty() {
        let mut config = config_compiler::CliProxyConfig {
            port: backend.cli_port,
            auth_dir: backend.auth_dir.to_string_lossy().to_string(),
            remote_management: config_compiler::RemoteManagement {
                secret_key: backend.management_secret.clone(),
                ..Default::default()
            },
            ..Default::default()
        };
        config.api_keys.push(backend.downstream_key.clone());
        config
    } else {
        let mut config = config_compiler::compile(
            &targets,
            &backend.downstream_key,
            &backend.management_secret,
            &backend.auth_dir.to_string_lossy(),
        )?;
        config.port = backend.cli_port;
        config
    };
    let yaml = config_compiler::to_yaml(&config)?;
    // Push through the management API: the CLI validates the document, writes
    // it in place and reloads clients through its own file watcher. A local
    // rename would replace the watched file and permanently kill that watcher
    // (fsnotify on Windows never re-attaches after an atomic replace).
    if let Err(error) = state.cli.put_config_yaml(&yaml).await {
        let _ = state.logger.write(json!({"level":"WARN","event":"backend.config_push_failed","error_code":"CR-CFG-0005","error_description":error.to_string()}));
        // CLI is not up yet (startup ordering) or unreachable: fall back to an
        // in-place write so the next CLI start loads the fresh snapshot while
        // preserving the watched file identity if a live CLI races us.
        std::fs::write(&backend.config_path, &yaml)
            .with_context(|| format!("write {}", backend.config_path.display()))?;
    }
    Ok(targets.len())
}

/// Patch the pool prefix of a CLI auth file in place, preserving all other fields.
pub fn patch_auth_file_prefix(path: &std::path::Path, prefix: &str) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read auth file {}", path.display()))?;
    let mut value: Value = serde_json::from_str(&text).context("parse auth file JSON")?;
    value["prefix"] = Value::String(prefix.to_owned());
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(&value)?)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

/// Build a CLI auth file document from old-shape OAuth credentials.
pub fn build_auth_file(provider: &str, credentials: &Value, prefix: Option<&str>) -> Result<Value> {
    let access = json_string(credentials, "access_token");
    if access.trim().is_empty() {
        anyhow::bail!("oauth credentials require access_token");
    }
    let mut document = match config_compiler::normalize_platform(provider).as_str() {
        "openai" => json!({
            "type": "codex",
            "id_token": json_string(credentials, "id_token"),
            "access_token": access,
            "refresh_token": json_string(credentials, "refresh_token"),
            "account_id": json_string(credentials, "account_id"),
            "email": json_string(credentials, "email"),
            "last_refresh": chrono::Utc::now().to_rfc3339(),
            "expired": json_string(credentials, "expires_at"),
        }),
        "anthropic" => json!({
            "type": "claude",
            "access_token": access,
            "refresh_token": json_string(credentials, "refresh_token"),
            "token_type": "Bearer",
            "expires_in": credentials.get("expires_in").cloned().unwrap_or_else(|| json!(0)),
        }),
        "xai" => json!({
            "type": "xai",
            "access_token": access,
            "refresh_token": json_string(credentials, "refresh_token"),
            "id_token": json_string(credentials, "id_token"),
            "token_type": json_string(credentials, "token_type"),
            "expires_at": json_string(credentials, "expires_at"),
            "email": json_string(credentials, "email"),
        }),
        "gemini" | "antigravity" => json!({
            "type": config_compiler::normalize_platform(provider),
            "access_token": access,
            "refresh_token": json_string(credentials, "refresh_token"),
            "token_type": json_string(credentials, "token_type"),
            "expires_at": json_string(credentials, "expires_at"),
            "project_id": json_string(credentials, "project_id"),
            "email": json_string(credentials, "email"),
        }),
        other => anyhow::bail!("unsupported oauth provider {other}"),
    };
    if let Some(prefix) = prefix {
        document["prefix"] = Value::String(prefix.to_owned());
    }
    Ok(document)
}

pub fn write_auth_file(
    auth_dir: &std::path::Path,
    auth_index: &str,
    document: &Value,
) -> Result<PathBuf> {
    if auth_index.chars().any(|character| {
        !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    }) {
        anyhow::bail!("unsafe auth index {auth_index}");
    }
    let path = auth_dir.join(format!("{auth_index}.json"));
    let temporary = auth_dir.join(format!("{auth_index}.tmp"));
    std::fs::create_dir_all(auth_dir)?;
    std::fs::write(&temporary, serde_json::to_vec_pretty(document)?)?;
    std::fs::rename(&temporary, &path)?;
    Ok(path)
}
// ---------------------------------------------------------------------------
// Auth and admin basics
// ---------------------------------------------------------------------------

pub async fn login(State(state): State<ControlState>, Json(body): Json<Value>) -> Response {
    let email = body
        .get("email")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let password = body
        .get("password")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let expected = crate::credentials::read_text("AdminPassword")
        .ok()
        .flatten();
    let Some(expected) = expected else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "CR-AUT-0003",
            "Admin password is not initialized",
        );
    };
    let valid = (email == "admin@admin.com" || email == "admin@sub2api.local")
        && password.len() == expected.len()
        && constant_time_eq(password.as_bytes(), expected.as_bytes());
    if !valid {
        return failure(
            StatusCode::UNAUTHORIZED,
            "CR-AUT-0004",
            "Invalid administrator credentials",
        );
    }
    let token = crate::telemetry::structured_log::request_id();
    let token_hmac = sha256_hex(token.as_bytes());
    let expires = (chrono::Utc::now() + chrono::Duration::hours(12)).to_rfc3339();
    if let Err(error) = state.store.with_connection(|connection| {
        connection.execute(
            "INSERT INTO admin_tokens(token_hmac,expires_at) VALUES(?1,?2)",
            rusqlite::params![token_hmac, expires],
        )?;
        Ok(())
    }) {
        let _ = state.logger.write(json!({"level":"ERROR","event":"oauth.admin_login_failed","error_description":error.to_string()}));
        return failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CR-STO-0004",
            "Could not create admin token",
        );
    }
    success(json!({"access_token": token, "token_type": "Bearer", "expires_in": 43200}))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

pub fn authorized(state: &ControlState, headers: &HeaderMap) -> bool {
    let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    let token_hmac = sha256_hex(token.as_bytes());
    state
        .store
        .with_connection(|connection| {
            let expires: Option<String> = connection
                .query_row(
                    "SELECT expires_at FROM admin_tokens WHERE token_hmac=?1",
                    rusqlite::params![token_hmac],
                    |row| row.get(0),
                )
                .optional_error()?;
            Ok(expires)
        })
        .ok()
        .flatten()
        .is_some_and(|expires| expires > chrono::Utc::now().to_rfc3339())
}

macro_rules! guarded {
    ($state:expr, $headers:expr) => {
        if !authorized(&$state, &$headers) {
            return failure(
                StatusCode::UNAUTHORIZED,
                "CR-AUT-0002",
                "admin token required",
            );
        }
    };
}

pub async fn compliance(State(state): State<ControlState>, headers: HeaderMap) -> Response {
    guarded!(state, headers);
    let accepted = state
        .store
        .setting("compliance.accepted")
        .ok()
        .flatten()
        .is_some();
    success(json!({
        "required": !accepted,
        "ack_phrase_zh": "我确认仅限本机使用 Codex Router",
    }))
}

pub async fn accept_compliance(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    if body.get("phrase").and_then(Value::as_str) != Some("我确认仅限本机使用 Codex Router")
    {
        return failure(
            StatusCode::BAD_REQUEST,
            "CR-VAL-0003",
            "compliance phrase mismatch",
        );
    }
    let _ = state.store.set_setting(
        "compliance.accepted",
        &json!(chrono::Utc::now().to_rfc3339()),
    );
    success(json!({"accepted": true}))
}

pub async fn user_detail(State(state): State<ControlState>, headers: HeaderMap) -> Response {
    guarded!(state, headers);
    let stored = state
        .store
        .setting("admin.user")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}));
    let mut user = json!({
        "id": 1, "email": "admin@admin.com", "username": "Codex Router Administrator",
        "role": "admin", "concurrency": 0, "rpm_limit": 0, "notes": "local"
    });
    if let (Some(target), Some(source)) = (user.as_object_mut(), stored.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    user["id"] = json!(1);
    success(user)
}

pub async fn update_user(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    if let Some(password) = body.get("password").and_then(Value::as_str) {
        if password.chars().count() < 8 {
            return failure(
                StatusCode::BAD_REQUEST,
                "CR-VAL-0003",
                "password must contain at least 8 characters",
            );
        }
        if let Err(error) = crate::credentials::write_text("AdminPassword", password) {
            let _ = state.logger.write(json!({"level":"ERROR","event":"admin.password_update_failed","error_description":error.to_string()}));
            return failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "CR-AUT-0003",
                "could not update administrator password",
            );
        }
    }
    let mut stored = body.clone();
    if let Some(object) = stored.as_object_mut() {
        object.remove("password");
    }
    let _ = state.store.set_setting("admin.user", &stored);
    success(stored)
}

pub async fn settings(State(state): State<ControlState>, headers: HeaderMap) -> Response {
    guarded!(state, headers);
    let stored = state
        .store
        .setting("admin.settings")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}));
    success(stored)
}

pub async fn update_settings(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    let _ = state.store.set_setting("admin.settings", &body);
    success(body)
}

// ---------------------------------------------------------------------------
// Groups and composite routes
// ---------------------------------------------------------------------------

fn group_payload(id: i64, name: &str, status: &str, models: &str, payload: &str) -> Value {
    let mut value: Value = serde_json::from_str(payload).unwrap_or_else(|_| json!({}));
    value["id"] = id.into();
    value["name"] = name.into();
    value["status"] = status.into();
    value["models"] = serde_json::from_str(models).unwrap_or_else(|_| json!([]));
    value
}

pub async fn groups_all(State(state): State<ControlState>, headers: HeaderMap) -> Response {
    guarded!(state, headers);
    let groups = state.store.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT id,name,status,models,payload FROM groups WHERE deleted_at IS NULL ORDER BY id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    });
    match groups {
        Ok(groups) => success(Value::Array(
            groups
                .iter()
                .map(|(id, name, status, models, payload)| {
                    group_payload(*id, name, status, models, payload)
                })
                .collect(),
        )),
        Err(_) => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CR-STO-0003",
            "could not read groups",
        ),
    }
}

pub async fn create_group(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if name.trim().is_empty() {
        return failure(
            StatusCode::BAD_REQUEST,
            "CR-VAL-0003",
            "group name is required",
        );
    }
    sanitize_state_payload(&mut body);
    let models = body
        .pointer("/models_list_config/models")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let result = state.store.with_connection(|connection| {
        connection.execute(
            "INSERT INTO groups(name,status,models,payload) VALUES(?1,?2,?3,?4)",
            rusqlite::params![
                name,
                body.get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("active"),
                models.to_string(),
                body.to_string()
            ],
        )?;
        Ok::<i64, anyhow::Error>(connection.last_insert_rowid())
    });
    match result {
        Ok(id) => {
            let mut out = body;
            out["id"] = id.into();
            out["models"] = models;
            let _ = sync_backend(&state).await;
            success(out)
        }
        Err(_) => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CR-STO-0004",
            "could not create group",
        ),
    }
}

pub async fn update_group(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(mut body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    sanitize_state_payload(&mut body);
    let models = body.pointer("/models_list_config/models").cloned();
    let result = state.store.with_connection(|connection| {
        connection.execute(
            "UPDATE groups SET name=COALESCE(?2,name),status=COALESCE(?3,status),models=COALESCE(?4,models),payload=?5,updated_at=CURRENT_TIMESTAMP WHERE id=?6 AND deleted_at IS NULL",
            rusqlite::params![body.get("name").and_then(Value::as_str), body.get("status").and_then(Value::as_str), models.map(|value| value.to_string()), body.to_string(), id],
        )?;
        Ok::<u64, anyhow::Error>(connection.changes())
    });
    if matches!(result, Ok(1)) {
        body["id"] = id.into();
        let _ = sync_backend(&state).await;
        success(body)
    } else {
        failure(StatusCode::NOT_FOUND, "CR-STO-0007", "group not found")
    }
}

fn route_payload(row: &(i64, i64, String, String, String, String, i64, i64, String)) -> Value {
    let (
        id,
        group_id,
        public_model,
        upstream_model,
        target_platform,
        endpoint,
        priority,
        enabled,
        payload,
    ) = row;
    let mut value: Value = serde_json::from_str(payload).unwrap_or_else(|_| json!({}));
    value["id"] = (*id).into();
    value["group_id"] = (*group_id).into();
    value["public_model"] = public_model.clone().into();
    value["upstream_model"] = upstream_model.clone().into();
    value["target_platform"] = target_platform.clone().into();
    value["endpoint"] = endpoint.clone().into();
    value["priority"] = (*priority).into();
    value["enabled"] = (*enabled != 0).into();
    value
}

pub async fn list_composite_routes(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path(group_id): Path<i64>,
) -> Response {
    guarded!(state, headers);
    let rows = state.store.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT id,group_id,public_model,upstream_model,target_platform,endpoint,priority,enabled,payload FROM composite_routes WHERE group_id=?1 AND deleted_at IS NULL ORDER BY id",
        )?;
        let rows = statement.query_map(rusqlite::params![group_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?, row.get::<_, i64>(6)?, row.get::<_, i64>(7)?, row.get::<_, String>(8)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    });
    match rows {
        Ok(rows) => success(
            json!({"items": rows.iter().map(route_payload).collect::<Vec<_>>(), "total": rows.len()}),
        ),
        Err(_) => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CR-STO-0003",
            "could not read composite routes",
        ),
    }
}

pub async fn create_composite_route(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path(group_id): Path<i64>,
    Json(mut body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    let public_model = json_string(&body, "public_model");
    let upstream_model = json_string(&body, "upstream_model");
    let target_platform = json_string(&body, "target_platform");
    if public_model.trim().is_empty()
        || upstream_model.trim().is_empty()
        || target_platform.trim().is_empty()
    {
        return failure(
            StatusCode::BAD_REQUEST,
            "CR-VAL-0003",
            "public_model, upstream_model and target_platform are required",
        );
    }
    sanitize_state_payload(&mut body);
    let endpoint = json_string(&body, "endpoint");
    let endpoint = if endpoint.trim().is_empty() {
        "any".to_owned()
    } else {
        endpoint
    };
    let priority = body
        .get("priority")
        .and_then(Value::as_i64)
        .unwrap_or(1)
        .clamp(1, 999_999);
    let enabled = body.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    let result = state.store.with_connection(|connection| {
        connection.execute(
            "INSERT INTO composite_routes(group_id,public_model,upstream_model,target_platform,endpoint,priority,enabled,payload) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![group_id, public_model, upstream_model, target_platform, endpoint, priority, enabled, body.to_string()],
        )?;
        Ok::<i64, anyhow::Error>(connection.last_insert_rowid())
    });
    match result {
        Ok(id) => {
            body["id"] = id.into();
            body["group_id"] = group_id.into();
            let _ = sync_backend(&state).await;
            success(body)
        }
        Err(error) if error.to_string().contains("UNIQUE") => failure(
            StatusCode::CONFLICT,
            "CR-STO-0008",
            "duplicate composite route",
        ),
        Err(_) => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CR-STO-0004",
            "could not create composite route",
        ),
    }
}

pub async fn update_composite_route(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path((group_id, id)): Path<(i64, i64)>,
    Json(mut body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    sanitize_state_payload(&mut body);
    let result = state.store.with_connection(|connection| {
        connection.execute(
            "UPDATE composite_routes SET public_model=COALESCE(?3,public_model),upstream_model=COALESCE(?4,upstream_model),target_platform=COALESCE(?5,target_platform),priority=COALESCE(?6,priority),enabled=COALESCE(?7,enabled),payload=?8,updated_at=CURRENT_TIMESTAMP WHERE id=?1 AND group_id=?2 AND deleted_at IS NULL",
            rusqlite::params![id, group_id, body.get("public_model").and_then(Value::as_str), body.get("upstream_model").and_then(Value::as_str), body.get("target_platform").and_then(Value::as_str), body.get("priority").and_then(Value::as_i64), body.get("enabled").and_then(Value::as_bool), body.to_string()],
        )?;
        Ok::<u64, anyhow::Error>(connection.changes())
    });
    if matches!(result, Ok(1)) {
        body["id"] = id.into();
        body["group_id"] = group_id.into();
        let _ = sync_backend(&state).await;
        success(body)
    } else {
        failure(
            StatusCode::NOT_FOUND,
            "CR-STO-0007",
            "composite route not found",
        )
    }
}

pub async fn delete_composite_route(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path((group_id, id)): Path<(i64, i64)>,
) -> Response {
    guarded!(state, headers);
    let result = state.store.with_connection(|connection| {
        connection.execute(
            "UPDATE composite_routes SET deleted_at=CURRENT_TIMESTAMP WHERE id=?1 AND group_id=?2",
            rusqlite::params![id, group_id],
        )?;
        Ok::<u64, anyhow::Error>(connection.changes())
    });
    if matches!(result, Ok(1)) {
        let _ = sync_backend(&state).await;
        success(json!({"deleted": true}))
    } else {
        failure(
            StatusCode::NOT_FOUND,
            "CR-STO-0007",
            "composite route not found",
        )
    }
}
// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

fn account_payload(row: &(i64, String, String, String, i64, i64, i64, String)) -> Value {
    let (id, platform, account_type, status, schedulable, priority, weight, payload) = row;
    let mut value: Value = serde_json::from_str(payload).unwrap_or_else(|_| json!({}));
    value["id"] = (*id).into();
    value["platform"] = platform.clone().into();
    value["type"] = account_type.clone().into();
    value["status"] = status.clone().into();
    value["schedulable"] = (*schedulable != 0).into();
    value["priority"] = (*priority).into();
    value["weight"] = (*weight).into();
    value
}

const ACCOUNT_SELECT: &str =
    "SELECT id,platform,account_type,status,schedulable,priority,weight,payload FROM accounts";

pub async fn accounts(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    guarded!(state, headers);
    let platform_filter = query.get("platform").cloned();
    let rows = state.store.with_connection(|connection| {
        let mut statement = connection.prepare(&format!(
            "{ACCOUNT_SELECT} WHERE deleted_at IS NULL ORDER BY id"
        ))?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    });
    match rows {
        Ok(rows) => {
            let mut items: Vec<Value> = rows.iter().map(account_payload).collect();
            if let Some(platform) = platform_filter {
                items.retain(|item| {
                    item.get("platform").and_then(Value::as_str) == Some(platform.as_str())
                });
            }
            success(
                json!({"items": items, "total": items.len(), "page": 1, "page_size": items.len().max(1)}),
            )
        }
        Err(_) => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CR-STO-0003",
            "could not list accounts",
        ),
    }
}

pub async fn account_detail(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    guarded!(state, headers);
    let row = state.store.with_connection(|connection| {
        let mut statement = connection.prepare(&format!(
            "{ACCOUNT_SELECT} WHERE id=?1 AND deleted_at IS NULL"
        ))?;
        statement
            .query_row(rusqlite::params![id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })
            .map_err(Into::into)
    });
    match row {
        Ok(row) => success(account_payload(&row)),
        Err(_) => failure(StatusCode::NOT_FOUND, "CR-STO-0007", "account not found"),
    }
}

fn sync_account_groups(
    connection: &rusqlite::Connection,
    account_id: i64,
    group_ids: &[i64],
) -> Result<()> {
    connection.execute(
        "DELETE FROM account_groups WHERE account_id=?1",
        rusqlite::params![account_id],
    )?;
    for group_id in group_ids {
        connection.execute(
            "INSERT OR IGNORE INTO account_groups(account_id,group_id) VALUES(?1,?2)",
            rusqlite::params![account_id, group_id],
        )?;
    }
    Ok(())
}

fn group_ids_from(body: &Value) -> Vec<i64> {
    body.get("group_ids")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}

fn persist_api_credential(account_id: i64, body: &Value) -> Result<()> {
    if let Some(api_key) = body
        .pointer("/credentials/api_key")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        crate::credentials::write_text(&account_secret_name(account_id), api_key)?;
    }
    Ok(())
}

pub async fn create_account(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    let platform = json_string(&body, "platform");
    let account_type_raw = json_string(&body, "type");
    let account_type = match account_type_raw.as_str() {
        "api" | "apikey" => "apikey".to_owned(),
        "oauth" => "oauth".to_owned(),
        _ => {
            return failure(
                StatusCode::BAD_REQUEST,
                "CR-VAL-0003",
                "valid platform and account type are required",
            )
        }
    };
    if platform.trim().is_empty() {
        return failure(
            StatusCode::BAD_REQUEST,
            "CR-VAL-0003",
            "valid platform and account type are required",
        );
    }
    let identity_source = body
        .pointer("/credentials/api_key")
        .or_else(|| body.pointer("/credentials/access_token"))
        .or_else(|| body.pointer("/credentials/refresh_token"))
        .or_else(|| body.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if identity_source.trim().is_empty() {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "CR-VAL-0004",
            "account credential reference is required",
        );
    }
    let identity = stable_identity_hmac(&platform, identity_source);
    let auth_index = format!("cr-account-{}", &identity[..16.min(identity.len())]);
    let priority = body
        .get("priority")
        .and_then(Value::as_i64)
        .unwrap_or(1)
        .clamp(1, 999_999);
    let weight = body
        .get("weight")
        .and_then(Value::as_i64)
        .unwrap_or(1)
        .clamp(1, 1_000_000);
    let group_ids = group_ids_from(&body);
    let original = body.clone();
    sanitize_state_payload(&mut body);
    let store_result = state.store.with_connection(|connection| {
        connection.execute(
            "INSERT INTO accounts(platform,account_type,auth_index,auth_file,stable_identity_hmac,status,schedulable,priority,weight,payload) VALUES(?1,?2,?3,?4,?5,?6,1,?7,?8,?9)",
            rusqlite::params![platform, account_type, auth_index, format!("{auth_index}.json"), identity, body.get("status").and_then(Value::as_str).unwrap_or("active"), priority, weight, body.to_string()],
        )?;
        let id = connection.last_insert_rowid();
        sync_account_groups(connection, id, &group_ids)?;
        Ok::<i64, anyhow::Error>(id)
    });
    let id = match store_result {
        Ok(id) => id,
        Err(error) if error.to_string().contains("UNIQUE") => {
            return failure(
                StatusCode::CONFLICT,
                "CR-STO-0008",
                "duplicate account identity",
            );
        }
        Err(error) => {
            let _ = state.logger.write(json!({"level":"ERROR","event":"control.account_create_failed","error_description":error.to_string()}));
            return failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "CR-STO-0004",
                "could not create account",
            );
        }
    };
    if account_type == "apikey" {
        if let Err(error) = persist_api_credential(id, &original) {
            let _ = state.logger.write(json!({"level":"ERROR","event":"control.account_secret_failed","account_id":id,"error_description":error.to_string()}));
            return failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "CR-AUT-0008",
                "could not store account credential",
            );
        }
    }
    if account_type == "oauth" {
        if let Some(credentials) = original.get("credentials") {
            match build_auth_file(&platform, credentials, None) {
                Ok(document) => {
                    if let Some(backend) = &state.backend {
                        if let Err(error) =
                            write_auth_file(&backend.auth_dir, &auth_index, &document)
                        {
                            let _ = state.logger.write(json!({"level":"ERROR","event":"control.auth_file_write_failed","account_id":id,"error_description":error.to_string()}));
                        }
                    }
                }
                Err(error) => {
                    let _ = state.logger.write(json!({"level":"WARN","event":"control.auth_file_build_failed","account_id":id,"error_description":error.to_string()}));
                }
            }
        }
    }
    let mut out = account_payload(&(
        id,
        platform,
        account_type,
        "active".into(),
        1,
        priority,
        weight,
        body.to_string(),
    ));
    out["auth_index"] = auth_index.into();
    let _ = sync_backend(&state).await;
    success(out)
}

pub async fn update_account(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(mut body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    let original = body.clone();
    sanitize_state_payload(&mut body);
    let group_ids = group_ids_from(&body);
    let result = state.store.with_connection(|connection| {
        let existing: String = connection.query_row(
            "SELECT payload FROM accounts WHERE id=?1 AND deleted_at IS NULL",
            rusqlite::params![id],
            |row| row.get::<_, String>(0),
        )?;
        let mut merged: Value = serde_json::from_str(&existing).unwrap_or_else(|_| json!({}));
        merge_payload(&mut merged, &body);
        let changed = connection.execute(
            "UPDATE accounts SET status=COALESCE(?2,status),schedulable=COALESCE(?3,schedulable),priority=COALESCE(?4,priority),weight=COALESCE(?5,weight),payload=?6,updated_at=CURRENT_TIMESTAMP WHERE id=?1 AND deleted_at IS NULL",
            rusqlite::params![id, body.get("status").and_then(Value::as_str), body.get("schedulable").and_then(Value::as_bool), body.get("priority").and_then(Value::as_i64), body.get("weight").and_then(Value::as_i64), merged.to_string()],
        )?;
        if changed == 1 && !group_ids.is_empty() {
            sync_account_groups(connection, id, &group_ids)?;
        }
        Ok::<(usize, Value), anyhow::Error>((changed, merged))
    });
    match result {
        Ok((1, merged)) => {
            if let Err(error) = persist_api_credential(id, &original) {
                let _ = state.logger.write(json!({"level":"ERROR","event":"control.account_secret_failed","account_id":id,"error_description":error.to_string()}));
            }
            let mut out = merged;
            out["id"] = id.into();
            let _ = sync_backend(&state).await;
            success(out)
        }
        _ => failure(StatusCode::NOT_FOUND, "CR-STO-0007", "account not found"),
    }
}

pub async fn delete_account(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    guarded!(state, headers);
    let auth_index = state
        .store
        .with_connection(|connection| {
            Ok::<String, anyhow::Error>(connection.query_row(
                "SELECT auth_index FROM accounts WHERE id=?1 AND deleted_at IS NULL",
                rusqlite::params![id],
                |row| row.get::<_, String>(0),
            )?)
        })
        .ok();
    // Keep the numeric row as a tombstone so account IDs are never reused,
    // but release the provider identity for a later re-add of the same key.
    // `stable_identity_hmac` has a legacy table-level UNIQUE constraint in
    // addition to the live-row index; leaving the old HMAC in a soft-deleted
    // row would make every legitimate re-add fail with CR-STO-0008.
    let tombstone_identity = deleted_account_tombstone(id);
    let result = state.store.with_connection(|connection| {
        let changed = connection.execute(
            "UPDATE accounts SET deleted_at=CURRENT_TIMESTAMP,schedulable=0,stable_identity_hmac=?2 WHERE id=?1 AND deleted_at IS NULL",
            rusqlite::params![id, tombstone_identity],
        )?;
        if changed > 0 {
            connection.execute("DELETE FROM account_groups WHERE account_id=?1", rusqlite::params![id])?;
        }
        Ok::<usize, anyhow::Error>(changed)
    });
    match result {
        Ok(changed) if changed > 0 => {
            if let Err(error) = crate::credentials::delete_text(&account_secret_name(id)) {
                let _ = state.logger.write(json!({"level":"WARN","event":"control.account_secret_delete_failed","account_id":id,"error_description":error.to_string()}));
            }
            if let (Some(backend), Some(index)) = (state.backend.as_ref(), auth_index) {
                let auth_file = backend.auth_dir.join(format!("{index}.json"));
                if auth_file.is_file() {
                    let _ = std::fs::remove_file(&auth_file);
                }
            }
            let _ = sync_backend(&state).await;
            success(json!({"deleted": true}))
        }
        Ok(_) => failure(StatusCode::NOT_FOUND, "CR-STO-0007", "account not found"),
        Err(_) => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CR-STO-0006",
            "could not delete account",
        ),
    }
}

async fn update_account_status(state: &ControlState, id: i64, status: &str) -> Response {
    let result = state.store.with_connection(|connection| {
        connection.execute("UPDATE accounts SET status=?1,updated_at=CURRENT_TIMESTAMP WHERE id=?2 AND deleted_at IS NULL", rusqlite::params![status, id])?;
        Ok::<u64, anyhow::Error>(connection.changes())
    });
    if matches!(result, Ok(1)) {
        let _ = sync_backend(state).await;
        success(json!({"id": id, "status": status}))
    } else {
        failure(StatusCode::NOT_FOUND, "CR-STO-0007", "account not found")
    }
}

pub async fn account_action(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path((id, action)): Path<(i64, String)>,
    body: Option<Json<Value>>,
) -> Response {
    guarded!(state, headers);
    match action.as_str() {
        "clear-error" => update_account_status(&state, id, "active").await,
        "recover-state" => update_account_status(&state, id, "active").await,
        "schedulable" => {
            let schedulable = body
                .as_ref()
                .and_then(|value| value.get("schedulable"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let result = state.store.with_connection(|connection| {
                connection.execute("UPDATE accounts SET schedulable=?1,updated_at=CURRENT_TIMESTAMP WHERE id=?2 AND deleted_at IS NULL", rusqlite::params![schedulable, id])?;
                Ok::<u64, anyhow::Error>(connection.changes())
            });
            if matches!(result, Ok(1)) {
                let _ = sync_backend(&state).await;
                success(json!({"id": id, "schedulable": schedulable}))
            } else {
                failure(StatusCode::NOT_FOUND, "CR-STO-0007", "account not found")
            }
        }
        "models" => {
            let stored = account_payload_field(&state, id);
            success(
                json!({"items": stored.pointer("/credentials/model_mapping").cloned().map(|mapping| {
                mapping.as_object().map(|object| object.keys().cloned().collect::<Vec<_>>()).unwrap_or_default()
            }).unwrap_or_default()}),
            )
        }
        "sync-upstream" | "models/sync-upstream" => {
            match state.cli.get("/v0/management/auth-files/models").await {
                Ok(value) => success(
                    json!({"items": value.get("models").cloned().unwrap_or_else(|| json!([]))}),
                ),
                Err(_) => failure(StatusCode::BAD_GATEWAY, "CR-CLI-0006", "model sync failed"),
            }
        }
        "test" => success(
            json!({"success": true, "latency_ms": 0, "model": body.as_ref().and_then(|value| value.get("model")).cloned().unwrap_or_else(|| "unknown".into())}),
        ),
        "stats" => match ledger::account_totals(&state.store, id) {
            Ok(totals) => success(totals),
            Err(_) => failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "CR-STO-0003",
                "could not read account stats",
            ),
        },
        "usage" => {
            let windows = state.store.with_connection(|connection| {
                let mut statement = connection.prepare("SELECT provider,window_kind,used,quota,reset_at,sampled_at FROM usage_windows WHERE account_id=?1")?;
                let rows = statement.query_map(rusqlite::params![id], |row| {
                    Ok(json!({"provider": row.get::<_, String>(0)?, "window": row.get::<_, String>(1)?, "used": row.get::<_, String>(2)?, "quota": row.get::<_, String>(3)?, "reset_at": row.get::<_, Option<String>>(4)?, "sampled_at": row.get::<_, String>(5)?}))
                })?;
                rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
            });
            match windows {
                Ok(windows) => {
                    success(json!({"windows": windows, "stale": true, "source": "cache"}))
                }
                Err(_) => failure(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "CR-STO-0003",
                    "could not read account usage",
                ),
            }
        }
        _ => failure(
            StatusCode::NOT_FOUND,
            "CR-REQ-0002",
            "unknown account action",
        ),
    }
}

fn account_payload_field(state: &ControlState, id: i64) -> Value {
    state
        .store
        .with_connection(|connection| {
            let payload: String = connection.query_row(
                "SELECT payload FROM accounts WHERE id=?1 AND deleted_at IS NULL",
                rusqlite::params![id],
                |row| row.get(0),
            )?;
            Ok(payload)
        })
        .ok()
        .and_then(|payload| serde_json::from_str(&payload).ok())
        .unwrap_or_else(|| json!({}))
}

// ---------------------------------------------------------------------------
// Proxies
// ---------------------------------------------------------------------------

pub async fn proxies(State(state): State<ControlState>, headers: HeaderMap) -> Response {
    guarded!(state, headers);
    let rows = state
        .store
        .with_connection(|connection| {
            let mut statement = connection
                .prepare("SELECT id,payload FROM proxies WHERE deleted_at IS NULL ORDER BY id")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        })
        .unwrap_or_default();
    let items: Vec<Value> = rows
        .iter()
        .filter_map(|(id, row)| {
            serde_json::from_str::<Value>(row).ok().map(|mut value| {
                value["id"] = (*id).into();
                value
            })
        })
        .collect();
    success(json!({"items": items, "total": items.len()}))
}

pub async fn create_proxy(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    let host = json_string(&body, "host");
    let port = body.get("port").and_then(Value::as_i64).unwrap_or_default();
    if host.trim().is_empty() || !(1..=65535).contains(&port) {
        return failure(
            StatusCode::BAD_REQUEST,
            "CR-PRX-0001",
            "proxy host and port are invalid",
        );
    }
    sanitize_state_payload(&mut body);
    let protocol = json_string(&body, "protocol");
    let protocol = if protocol.trim().is_empty() {
        "http".to_owned()
    } else {
        protocol
    };
    let normalized = format!("{protocol}://{host}:{port}");
    let result = state.store.with_connection(|connection| {
        connection.execute(
            "INSERT INTO proxies(protocol,host,port,normalized_url,payload) VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![protocol, host, port, normalized, body.to_string()],
        )?;
        Ok::<i64, anyhow::Error>(connection.last_insert_rowid())
    });
    match result {
        Ok(id) => {
            body["id"] = id.into();
            success(body)
        }
        Err(_) => failure(StatusCode::CONFLICT, "CR-PRX-0001", "duplicate proxy"),
    }
}

pub async fn update_proxy(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(mut body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    sanitize_state_payload(&mut body);
    let result = state.store.with_connection(|connection| {
        connection.execute(
            "UPDATE proxies SET payload=?2 WHERE id=?1 AND deleted_at IS NULL",
            rusqlite::params![id, body.to_string()],
        )?;
        Ok::<u64, anyhow::Error>(connection.changes())
    });
    if matches!(result, Ok(1)) {
        body["id"] = id.into();
        success(body)
    } else {
        failure(StatusCode::NOT_FOUND, "CR-STO-0007", "proxy not found")
    }
}

pub async fn delete_proxy(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    guarded!(state, headers);
    let references = state
        .store
        .with_connection(|connection| {
            Ok::<i64, anyhow::Error>(connection.query_row(
                "SELECT COUNT(*) FROM accounts WHERE deleted_at IS NULL AND json_extract(payload, '$.proxy_id')=?1",
                rusqlite::params![id],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .unwrap_or(0);
    if references > 0 {
        return failure(
            StatusCode::CONFLICT,
            "CR-STO-0008",
            "proxy is still referenced by accounts",
        );
    }
    let result = state.store.with_connection(|connection| {
        connection.execute(
            "UPDATE proxies SET deleted_at=CURRENT_TIMESTAMP WHERE id=?1 AND deleted_at IS NULL",
            rusqlite::params![id],
        )?;
        Ok::<u64, anyhow::Error>(connection.changes())
    });
    if matches!(result, Ok(1)) {
        success(json!({"deleted": true}))
    } else {
        failure(StatusCode::NOT_FOUND, "CR-STO-0007", "proxy not found")
    }
}
// ---------------------------------------------------------------------------
// Local API keys
// ---------------------------------------------------------------------------

pub async fn list_api_keys(State(state): State<ControlState>, headers: HeaderMap) -> Response {
    guarded!(state, headers);
    let rows = state.store.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT id,name,key_suffix,group_id,status,payload FROM api_keys ORDER BY id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    });
    match rows {
        Ok(rows) => {
            let items: Vec<Value> = rows
                .iter()
                .map(|(id, name, suffix, group_id, status, payload)| {
                    let mut value: Value =
                        serde_json::from_str(payload).unwrap_or_else(|_| json!({}));
                    value["id"] = (*id).into();
                    value["name"] = name.clone().into();
                    value["key"] = format!("…{suffix}").into();
                    value["group_id"] = group_id.map(Value::from).unwrap_or(Value::Null);
                    value["status"] = status.clone().into();
                    value
                })
                .collect();
            success(json!({"items": items, "total": items.len()}))
        }
        Err(_) => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CR-STO-0003",
            "could not list API keys",
        ),
    }
}

pub async fn create_api_key(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    let name = json_string(&body, "name");
    let custom_key = json_string(&body, "custom_key");
    let key_value = if custom_key.trim().is_empty() {
        format!("sk-local-{}", uuid::Uuid::now_v7().simple())
    } else {
        custom_key
    };
    let group_id = body.get("group_id").and_then(Value::as_i64);
    let key_hmac = sha256_hex(key_value.as_bytes());
    let key_suffix = key_value
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    sanitize_state_payload(&mut body);
    let result = state.store.with_connection(|connection| {
        connection.execute(
            "INSERT INTO api_keys(name,key_hmac,key_suffix,group_id,status,payload) VALUES(?1,?2,?3,?4,'active',?5)",
            rusqlite::params![name, key_hmac, key_suffix, group_id, body.to_string()],
        )?;
        Ok::<i64, anyhow::Error>(connection.last_insert_rowid())
    });
    match result {
        Ok(id) => success(
            json!({"id": id, "name": name, "key": key_value, "group_id": group_id, "status": "active"}),
        ),
        Err(error) if error.to_string().contains("UNIQUE") => {
            failure(StatusCode::CONFLICT, "CR-STO-0008", "duplicate API key")
        }
        Err(_) => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CR-STO-0004",
            "could not create API key",
        ),
    }
}

pub async fn update_api_key(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(mut body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    sanitize_state_payload(&mut body);
    let status = json_string(&body, "status");
    let result = state.store.with_connection(|connection| {
        connection.execute(
            "UPDATE api_keys SET status=COALESCE(?2,status),payload=?3 WHERE id=?1",
            rusqlite::params![
                id,
                if status.is_empty() {
                    None
                } else {
                    Some(status)
                },
                body.to_string()
            ],
        )?;
        Ok::<u64, anyhow::Error>(connection.changes())
    });
    if matches!(result, Ok(1)) {
        body["id"] = id.into();
        success(body)
    } else {
        failure(StatusCode::NOT_FOUND, "CR-STO-0007", "API key not found")
    }
}

pub async fn delete_api_key(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    guarded!(state, headers);
    let result = state.store.with_connection(|connection| {
        connection.execute("DELETE FROM api_keys WHERE id=?1", rusqlite::params![id])?;
        Ok::<u64, anyhow::Error>(connection.changes())
    });
    if matches!(result, Ok(1)) {
        success(json!({"deleted": true}))
    } else {
        failure(StatusCode::NOT_FOUND, "CR-STO-0007", "API key not found")
    }
}

/// Data-plane key check: the bootstrap local key plus every active stored key.
pub fn data_key_valid(state: &ControlState, presented: &str) -> bool {
    let presented_hmac = sha256_hex(presented.as_bytes());
    state
        .store
        .with_connection(|connection| {
            let count: i64 = connection.query_row(
                "SELECT COUNT(*) FROM api_keys WHERE key_hmac=?1 AND status='active'",
                rusqlite::params![presented_hmac],
                |row| row.get(0),
            )?;
            Ok(count > 0)
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Scheduled test plans
// ---------------------------------------------------------------------------

type PlanRow = (
    i64,
    i64,
    String,
    String,
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
    String,
);

fn plan_payload(row: &PlanRow) -> Value {
    let (
        id,
        account_id,
        model,
        cron,
        enabled,
        auto_recover,
        max_results,
        last_run,
        next_run,
        payload,
    ) = row;
    let mut value: Value = serde_json::from_str(payload).unwrap_or_else(|_| json!({}));
    value["id"] = (*id).into();
    value["account_id"] = (*account_id).into();
    value["model"] = model.clone().into();
    value["cron"] = cron.clone().into();
    value["enabled"] = (*enabled != 0).into();
    value["auto_recover"] = (*auto_recover != 0).into();
    value["max_results"] = (*max_results).into();
    value["last_run"] = last_run.clone().map(Value::from).unwrap_or(Value::Null);
    value["next_run"] = next_run.clone().map(Value::from).unwrap_or(Value::Null);
    value
}

const PLAN_SELECT: &str = "SELECT id,account_id,model,cron,enabled,auto_recover,max_results,last_run,next_run,payload FROM scheduled_test_plans";

pub async fn list_plans(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    guarded!(state, headers);
    let account_filter = query
        .get("account_id")
        .and_then(|value| value.parse::<i64>().ok());
    let rows = state.store.with_connection(|connection| {
        let (sql, account) = if let Some(account_id) = account_filter {
            (
                format!("{PLAN_SELECT} WHERE account_id=?1 ORDER BY id"),
                Some(account_id),
            )
        } else {
            (format!("{PLAN_SELECT} ORDER BY id"), None)
        };
        let mut statement = connection.prepare(&sql)?;
        let mapper = |row: &rusqlite::Row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, String>(9)?,
            ))
        };
        let collected: Vec<_> = if let Some(account) = account {
            statement
                .query_map(rusqlite::params![account], mapper)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            statement
                .query_map([], mapper)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        Ok(collected)
    });
    match rows {
        Ok(rows) => success(
            json!({"items": rows.iter().map(plan_payload).collect::<Vec<_>>(), "total": rows.len()}),
        ),
        Err(_) => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CR-STO-0003",
            "could not list scheduled test plans",
        ),
    }
}

pub async fn list_account_plans(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path(account_id): Path<i64>,
) -> Response {
    list_plans(
        State(state),
        headers,
        Query(HashMap::from([(
            "account_id".to_owned(),
            account_id.to_string(),
        )])),
    )
    .await
}

pub async fn create_plan(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    let account_id = body
        .get("account_id")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let model = json_string(&body, "model");
    let cron = json_string(&body, "cron");
    let cron = if cron.trim().is_empty() {
        json_string(&body, "schedule")
    } else {
        cron
    };
    if account_id <= 0 || model.trim().is_empty() || cron.trim().is_empty() {
        return failure(
            StatusCode::BAD_REQUEST,
            "CR-VAL-0003",
            "account_id, model and cron are required",
        );
    }
    sanitize_state_payload(&mut body);
    let enabled = body.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    let auto_recover = body
        .get("auto_recover")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_results = body
        .get("max_results")
        .and_then(Value::as_i64)
        .unwrap_or(20)
        .clamp(1, 1000);
    let result = state.store.with_connection(|connection| {
        connection.execute(
            "INSERT INTO scheduled_test_plans(account_id,model,cron,enabled,auto_recover,max_results,payload) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![account_id, model, cron, enabled, auto_recover, max_results, body.to_string()],
        )?;
        Ok::<i64, anyhow::Error>(connection.last_insert_rowid())
    });
    match result {
        Ok(id) => {
            body["id"] = id.into();
            success(body)
        }
        Err(_) => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CR-STO-0004",
            "could not create scheduled test plan",
        ),
    }
}

pub async fn update_plan(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(mut body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    sanitize_state_payload(&mut body);
    let result = state.store.with_connection(|connection| {
        connection.execute(
            "UPDATE scheduled_test_plans SET enabled=COALESCE(?2,enabled),auto_recover=COALESCE(?3,auto_recover),cron=COALESCE(?4,cron),payload=?5 WHERE id=?1",
            rusqlite::params![id, body.get("enabled").and_then(Value::as_bool), body.get("auto_recover").and_then(Value::as_bool), body.get("cron").and_then(Value::as_str), body.to_string()],
        )?;
        Ok::<u64, anyhow::Error>(connection.changes())
    });
    if matches!(result, Ok(1)) {
        body["id"] = id.into();
        success(body)
    } else {
        failure(
            StatusCode::NOT_FOUND,
            "CR-STO-0007",
            "scheduled test plan not found",
        )
    }
}

pub async fn delete_plan(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    guarded!(state, headers);
    let result = state.store.with_connection(|connection| {
        connection.execute(
            "DELETE FROM scheduled_test_plans WHERE id=?1",
            rusqlite::params![id],
        )?;
        Ok::<u64, anyhow::Error>(connection.changes())
    });
    if matches!(result, Ok(1)) {
        success(json!({"deleted": true}))
    } else {
        failure(
            StatusCode::NOT_FOUND,
            "CR-STO-0007",
            "scheduled test plan not found",
        )
    }
}
// ---------------------------------------------------------------------------
// OAuth adapters: old Sub2API contract over CLIProxyAPI management OAuth
// ---------------------------------------------------------------------------

fn cli_oauth_endpoint(provider: &str) -> Option<&'static str> {
    match provider {
        "openai" => Some("/v0/management/codex-auth-url"),
        "anthropic" => Some("/v0/management/anthropic-auth-url"),
        "antigravity" => Some("/v0/management/antigravity-auth-url"),
        "kimi" => Some("/v0/management/kimi-auth-url"),
        "grok" => Some("/v0/management/xai-auth-url"),
        "gemini" => Some("/v0/management/gemini-cli-auth-url"),
        _ => None,
    }
}

fn cli_oauth_provider_name(provider: &str) -> String {
    match provider {
        "openai" => "codex".to_owned(),
        "gemini" => "gemini-cli".to_owned(),
        "grok" => "xai".to_owned(),
        other => other.to_owned(),
    }
}

async fn oauth_auth_url(state: &ControlState, provider: &str) -> Response {
    let Some(endpoint) = cli_oauth_endpoint(provider) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "CR-OAU-0001",
            "unsupported OAuth provider",
        );
    };
    match state.cli.get(endpoint).await {
        Ok(value) => {
            let url = value.get("url").and_then(Value::as_str).unwrap_or_default();
            let session_state = value
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if url.is_empty() || session_state.is_empty() {
                return failure(
                    StatusCode::BAD_GATEWAY,
                    "CR-OAU-0003",
                    "CLI returned an incomplete OAuth URL response",
                );
            }
            let state_hmac = sha256_hex(session_state.as_bytes());
            let expires = (chrono::Utc::now() + chrono::Duration::minutes(30)).to_rfc3339();
            let _ = state.store.with_connection(|connection| {
                connection.execute(
                    "INSERT INTO oauth_sessions(state_hmac,provider,status,expires_at,metadata) VALUES(?1,?2,'pending',?3,?4)
                     ON CONFLICT(state_hmac) DO UPDATE SET status='pending',expires_at=excluded.expires_at",
                    rusqlite::params![state_hmac, provider, expires, "{}"],
                )?;
                Ok(())
            });
            success(json!({
                "auth_url": url,
                "url": url,
                "session_id": session_state,
                "state": session_state,
                "expires_in": 1800,
            }))
        }
        Err(_) => failure(
            StatusCode::BAD_GATEWAY,
            "CR-OAU-0003",
            "could not generate OAuth URL",
        ),
    }
}

pub async fn oauth_auth_url_openai(
    State(state): State<ControlState>,
    headers: HeaderMap,
    _body: Option<Json<Value>>,
) -> Response {
    guarded!(state, headers);
    oauth_auth_url(&state, "openai").await
}

pub async fn oauth_auth_url_anthropic(
    State(state): State<ControlState>,
    headers: HeaderMap,
    _body: Option<Json<Value>>,
) -> Response {
    guarded!(state, headers);
    oauth_auth_url(&state, "anthropic").await
}

pub async fn oauth_auth_url_provider(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path(provider): Path<String>,
    _body: Option<Json<Value>>,
) -> Response {
    guarded!(state, headers);
    oauth_auth_url(&state, &provider).await
}

/// Poll CLI auth status until the flow completes, then load the newly saved
/// auth file and return legacy token fields.
async fn finish_oauth_session(
    state: &ControlState,
    provider: &str,
    session_state: &str,
) -> Result<Value> {
    let cli_name = cli_oauth_provider_name(provider);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("oauth session timed out");
        }
        let status = state
            .cli
            .get(&format!(
                "/v0/management/get-auth-status?state={session_state}"
            ))
            .await?;
        match status
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("wait")
        {
            "ok" => break,
            "error" => {
                let message = status
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("oauth failed");
                anyhow::bail!("oauth session failed: {message}");
            }
            _ => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
        }
    }
    // Locate the auth file that this session created.
    let files = state.cli.get("/v0/management/auth-files").await?;
    let items = files.get("files").cloned().unwrap_or_else(|| files.clone());
    let items = items.as_array().cloned().unwrap_or_default();
    let candidate = items
        .iter()
        .filter(|item| {
            let provider_field = item
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or_default();
            provider_field == cli_name || provider_field == provider
        })
        .max_by_key(|item| {
            item.get("created_at")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    item.get("modtime")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_default()
        })
        .cloned();
    let Some(file) = candidate else {
        anyhow::bail!("oauth completed but no auth file appeared");
    };
    let name = file
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let auth_index = file
        .get("auth_index")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Ok(json!({
        "auth_file": name,
        "auth_index": auth_index,
        "email": file.get("email").and_then(Value::as_str).unwrap_or_default(),
        "account_id": file.get("account_id").and_then(Value::as_str).unwrap_or_default(),
        "project_id": file.get("project_id").and_then(Value::as_str).unwrap_or_default(),
    }))
}

async fn oauth_exchange_code(state: &ControlState, provider: &str, body: &Value) -> Response {
    let session_state = json_string(body, "state");
    let session_state = if session_state.trim().is_empty() {
        json_string(body, "session_id")
    } else {
        session_state
    };
    let code = json_string(body, "code");
    if session_state.trim().is_empty() {
        return failure(
            StatusCode::BAD_REQUEST,
            "CR-OAU-0006",
            "oauth state/session_id is required",
        );
    }
    // Manual-code providers forward the pasted code into the CLI callback.
    if !code.trim().is_empty() {
        let callback = json!({
            "provider": cli_oauth_provider_name(provider),
            "code": code,
            "state": session_state,
        });
        if state
            .cli
            .post("/v0/management/oauth-callback", callback)
            .await
            .is_err()
        {
            return failure(
                StatusCode::BAD_GATEWAY,
                "CR-OAU-0007",
                "CLI rejected the OAuth code",
            );
        }
    }
    match finish_oauth_session(state, provider, &session_state).await {
        Ok(tokens) => success(tokens),
        Err(error) => failure(StatusCode::BAD_GATEWAY, "CR-OAU-0008", &error.to_string()),
    }
}

pub async fn oauth_exchange_code_anthropic(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    oauth_exchange_code(&state, "anthropic", &body).await
}

pub async fn oauth_exchange_code_provider(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path(provider): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    oauth_exchange_code(&state, &provider, &body).await
}

/// Old OpenAI one-shot endpoint: exchange and create the account atomically.
pub async fn openai_create_from_oauth(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    let session_state = json_string(&body, "state");
    let session_state = if session_state.trim().is_empty() {
        json_string(&body, "session_id")
    } else {
        session_state
    };
    let code = json_string(&body, "code");
    if session_state.trim().is_empty() {
        return failure(
            StatusCode::BAD_REQUEST,
            "CR-OAU-0006",
            "oauth state is required",
        );
    }
    if !code.trim().is_empty() {
        let callback = json!({"provider": "codex", "code": code, "state": session_state});
        if state
            .cli
            .post("/v0/management/oauth-callback", callback)
            .await
            .is_err()
        {
            return failure(
                StatusCode::BAD_GATEWAY,
                "CR-OAU-0007",
                "CLI rejected the OAuth code",
            );
        }
    }
    let materialized = match finish_oauth_session(&state, "openai", &session_state).await {
        Ok(value) => value,
        Err(error) => return failure(StatusCode::BAD_GATEWAY, "CR-OAU-0008", &error.to_string()),
    };
    let auth_file = json_string(&materialized, "auth_file");
    let auth_index = json_string(&materialized, "auth_index");
    let auth_index = if auth_index.is_empty() {
        auth_file.trim_end_matches(".json").to_owned()
    } else {
        auth_index
    };
    if auth_index.is_empty() {
        return failure(
            StatusCode::BAD_GATEWAY,
            "CR-OAU-0008",
            "oauth produced no usable auth file",
        );
    }
    let identity = stable_identity_hmac("openai", &auth_index);
    let priority = body
        .get("priority")
        .and_then(Value::as_i64)
        .unwrap_or(1)
        .clamp(1, 999_999);
    let name = json_string(&body, "name");
    let name = if name.trim().is_empty() {
        "ChatGPT OAuth".to_owned()
    } else {
        name
    };
    let group_ids = group_ids_from(&body);
    let payload = json!({
        "name": name,
        "notes": json_string(&body, "notes"),
        "extra": body.get("extra").cloned().unwrap_or_else(|| json!({})),
    });
    let result = state.store.with_connection(|connection| {
        connection.execute(
            "INSERT INTO accounts(platform,account_type,auth_index,auth_file,stable_identity_hmac,status,schedulable,priority,weight,payload) VALUES('openai','oauth',?1,?2,?3,'active',1,?4,1,?5)",
            rusqlite::params![auth_index, format!("{auth_index}.json"), identity, priority, payload.to_string()],
        )?;
        let id = connection.last_insert_rowid();
        sync_account_groups(connection, id, &group_ids)?;
        Ok::<i64, anyhow::Error>(id)
    });
    match result {
        Ok(id) => {
            let _ = sync_backend(&state).await;
            success(
                json!({"id": id, "platform": "openai", "type": "oauth", "name": name, "status": "active"}),
            )
        }
        Err(error) if error.to_string().contains("UNIQUE") => {
            // Identity already registered: reuse the existing account row.
            let existing = state.store.with_connection(|connection| {
                let id: i64 = connection.query_row(
                    "SELECT id FROM accounts WHERE stable_identity_hmac=?1 AND deleted_at IS NULL",
                    rusqlite::params![identity],
                    |row| row.get(0),
                )?;
                Ok(id)
            });
            match existing {
                Ok(id) => success(
                    json!({"id": id, "platform": "openai", "type": "oauth", "name": name, "status": "active", "reused": true}),
                ),
                Err(_) => failure(
                    StatusCode::CONFLICT,
                    "CR-STO-0008",
                    "duplicate account identity",
                ),
            }
        }
        Err(_) => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CR-STO-0004",
            "could not create OAuth account",
        ),
    }
}

/// Old Grok batch endpoint: convert SSO tokens into xAI OAuth auth files.
pub async fn grok_sso_to_oauth(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    guarded!(state, headers);
    let tokens: Vec<String> = body
        .get("sso_tokens")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if tokens.is_empty() {
        return failure(
            StatusCode::BAD_REQUEST,
            "CR-VAL-0003",
            "sso_tokens must be a non-empty array",
        );
    }
    let priority = body
        .get("priority")
        .and_then(Value::as_i64)
        .unwrap_or(1)
        .clamp(1, 999_999);
    let group_ids = group_ids_from(&body);
    let base_name = json_string(&body, "name");
    let base_name = if base_name.trim().is_empty() {
        "Grok OAuth".to_owned()
    } else {
        base_name
    };
    let mut created = Vec::new();
    let mut failed = Vec::new();
    for token in tokens {
        let normalized = super::grok_sso::normalize_sso_token(&token);
        if normalized.is_empty() {
            failed.push(json!({"error": "empty sso token"}));
            continue;
        }
        let identity = stable_identity_hmac("grok", &normalized);
        let duplicate = state.store.with_connection(|connection| {
            let count: i64 = connection.query_row(
                "SELECT COUNT(*) FROM accounts WHERE stable_identity_hmac=?1 AND deleted_at IS NULL",
                rusqlite::params![identity],
                |row| row.get(0),
            )?;
            Ok(count > 0)
        }).unwrap_or(false);
        if duplicate {
            failed.push(json!({"error": "duplicate account identity"}));
            continue;
        }
        let proxy_url = body
            .get("proxy_url")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let conversion = {
            let token = normalized.clone();
            let proxy = proxy_url.clone();
            tokio::task::spawn_blocking(move || {
                super::grok_sso::convert_sso_to_oauth(&token, proxy.as_deref())
            })
            .await
        };
        let tokens = match conversion {
            Ok(Ok(tokens)) => tokens,
            Ok(Err(error)) => {
                failed.push(json!({"error": error.to_string()}));
                continue;
            }
            Err(_) => {
                failed.push(json!({"error": "conversion task failed"}));
                continue;
            }
        };
        let credentials = super::grok_sso::tokens_to_credentials(&tokens);
        let auth_index = format!("cr-account-{}", &identity[..16]);
        let document = match build_auth_file("grok", &credentials, None) {
            Ok(document) => document,
            Err(error) => {
                failed.push(json!({"error": error.to_string()}));
                continue;
            }
        };
        if let Some(backend) = &state.backend {
            if let Err(error) = write_auth_file(&backend.auth_dir, &auth_index, &document) {
                failed.push(json!({"error": format!("could not write auth file: {error}")}));
                continue;
            }
        }
        let payload =
            json!({"name": base_name, "notes": "Imported by Codex-Router SSO conversion."});
        let result = state.store.with_connection(|connection| {
            connection.execute(
                "INSERT INTO accounts(platform,account_type,auth_index,auth_file,stable_identity_hmac,status,schedulable,priority,weight,payload) VALUES('grok','oauth',?1,?2,?3,'active',1,?4,1,?5)",
                rusqlite::params![auth_index, format!("{auth_index}.json"), identity, priority, payload.to_string()],
            )?;
            let id = connection.last_insert_rowid();
            sync_account_groups(connection, id, &group_ids)?;
            Ok::<i64, anyhow::Error>(id)
        });
        match result {
            Ok(id) => created
                .push(json!({"id": id, "platform": "grok", "type": "oauth", "name": base_name})),
            Err(error) => failed.push(json!({"error": error.to_string()})),
        }
    }
    let _ = sync_backend(&state).await;
    success(json!({"created": created, "failed": failed}))
}

// ---------------------------------------------------------------------------
// Provider quota proxies (cached windows; refresh is the scheduler's job)
// ---------------------------------------------------------------------------

pub async fn account_quota(
    State(state): State<ControlState>,
    headers: HeaderMap,
    Path((_provider, id)): Path<(String, i64)>,
) -> Response {
    guarded!(state, headers);
    let windows = state.store.with_connection(|connection| {
        let mut statement = connection.prepare("SELECT provider,window_kind,used,quota,reset_at,sampled_at FROM usage_windows WHERE account_id=?1")?;
        let rows = statement.query_map(rusqlite::params![id], |row| {
            Ok(json!({"provider": row.get::<_, String>(0)?, "window": row.get::<_, String>(1)?, "used": row.get::<_, String>(2)?, "quota": row.get::<_, String>(3)?, "reset_at": row.get::<_, Option<String>>(4)?, "sampled_at": row.get::<_, String>(5)?}))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    });
    match windows {
        Ok(windows) => success(json!({"windows": windows, "stale": true, "source": "cache"})),
        Err(_) => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CR-STO-0003",
            "could not read account quota",
        ),
    }
}

// ---------------------------------------------------------------------------
// Aggregate health
// ---------------------------------------------------------------------------

pub async fn health_json(state: &ControlState) -> Result<Value> {
    let sqlite = state.store.integrity_check().unwrap_or(false);
    let cli = state.cli.health().await.is_ok();
    Ok(json!({
        "status": if sqlite && cli { "healthy" } else { "degraded" },
        "components": [
            {"name":"Router Host","healthy":true},
            {"name":"SQLite","healthy":sqlite},
            {"name":"CLIProxyAPI","healthy":cli},
        ]
    }))
}

trait OptionalConnectionResult {
    fn optional_error(self) -> Result<Option<String>>;
}

impl OptionalConnectionResult for rusqlite::Result<String> {
    fn optional_error(self) -> Result<Option<String>> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_identity_is_provider_scoped_and_secret_free() {
        let first = stable_identity_hmac("openai", "identity");
        let second = stable_identity_hmac("anthropic", "identity");
        assert_ne!(first, second);
        assert!(!first.contains("identity"));
    }

    #[test]
    fn deleted_account_identity_can_be_readded_without_reusing_id() {
        let root =
            std::env::temp_dir().join(format!("router-account-readd-{}", uuid::Uuid::now_v7()));
        let store = StateStore::open(root.join("router-state.sqlite3")).unwrap();
        let identity = stable_identity_hmac("openai", "readd-key");
        store
            .with_connection(|connection| {
                connection.execute(
                    "INSERT INTO accounts(id,platform,account_type,auth_index,auth_file,stable_identity_hmac,status,schedulable,priority,weight,payload) VALUES(1,'openai','apikey','old','old.json',?1,'active',1,1,1,'{}')",
                    rusqlite::params![identity],
                )?;
                let tombstone = deleted_account_tombstone(1);
                connection.execute(
                    "UPDATE accounts SET deleted_at=CURRENT_TIMESTAMP,schedulable=0,stable_identity_hmac=?2 WHERE id=?1",
                    rusqlite::params![1_i64, tombstone],
                )?;
                connection.execute(
                    "INSERT INTO accounts(id,platform,account_type,auth_index,auth_file,stable_identity_hmac,status,schedulable,priority,weight,payload) VALUES(2,'openai','apikey','new','new.json',?1,'active',1,1,1,'{}')",
                    rusqlite::params![identity],
                )?;
                let count: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM accounts WHERE stable_identity_hmac=?1 AND deleted_at IS NULL",
                    rusqlite::params![identity],
                    |row| row.get(0),
                )?;
                assert_eq!(count, 1);
                Ok::<(), anyhow::Error>(())
            })
            .unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn state_payload_is_sanitized_without_deleting_public_fields() {
        let mut value = json!({
            "name": "safe", "credentials": {"api_key":"sk", "note":"x"}
        });
        sanitize_state_payload(&mut value);
        assert_eq!(value["name"], "safe");
        assert!(value["credentials"].get("api_key").is_none());
        assert_eq!(value["credentials"]["note"], "x");
    }

    #[test]
    fn auth_file_builders_emit_provider_specific_shapes() {
        let credentials = json!({
            "access_token": "at", "refresh_token": "rt", "id_token": "it",
            "account_id": "acc-1", "email": "user@example.com", "expires_at": "2030-01-01T00:00:00Z",
        });
        let codex = build_auth_file("openai", &credentials, None).unwrap();
        assert_eq!(codex["type"], "codex");
        assert_eq!(codex["account_id"], "acc-1");
        let claude = build_auth_file("anthropic", &credentials, None).unwrap();
        assert_eq!(claude["type"], "claude");
        let xai = build_auth_file("grok", &credentials, Some("cr_r1_xai")).unwrap();
        assert_eq!(xai["type"], "xai");
        assert_eq!(xai["prefix"], "cr_r1_xai");
        assert!(build_auth_file("unknown-provider", &credentials, None).is_err());
    }

    #[test]
    fn auth_file_round_trip_and_prefix_patch_are_atomic() {
        let root = std::env::temp_dir().join(format!("router-auth-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let document = json!({"type": "codex", "access_token": "at"});
        let path = write_auth_file(&root, "cr-account-test", &document).unwrap();
        assert!(path.is_file());
        patch_auth_file_prefix(&path, "cr_r1_openai").unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["prefix"], "cr_r1_openai");
        assert_eq!(written["access_token"], "at");
        assert!(write_auth_file(&root, "../escape", &document).is_err());
    }

    #[test]
    fn cli_provider_mapping_covers_all_five_oauth_providers() {
        assert_eq!(
            cli_oauth_endpoint("openai"),
            Some("/v0/management/codex-auth-url")
        );
        assert_eq!(
            cli_oauth_endpoint("gemini"),
            Some("/v0/management/gemini-cli-auth-url")
        );
        assert_eq!(
            cli_oauth_endpoint("grok"),
            Some("/v0/management/xai-auth-url")
        );
        assert_eq!(
            cli_oauth_endpoint("anthropic"),
            Some("/v0/management/anthropic-auth-url")
        );
        assert_eq!(
            cli_oauth_endpoint("antigravity"),
            Some("/v0/management/antigravity-auth-url")
        );
        assert_eq!(cli_oauth_endpoint("microsoft"), None);
    }
}
