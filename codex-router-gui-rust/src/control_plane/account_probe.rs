//! Credential-scoped account probes shared by manual tests, recovery and the
//! scheduled test runner. Probe responses deliberately exclude upstream bodies.

use crate::backend::cli_proxy::CliProxyManagementClient;
use crate::backend::config_compiler;
use crate::state::StateStore;
use anyhow::{bail, Context, Result};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;
use std::time::Instant;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProbeSuccess {
    pub account_id: i64,
    pub model: String,
    pub latency_ms: u64,
    pub upstream_status: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProbeFailure {
    pub error_code: &'static str,
    pub message: &'static str,
    pub http_status: u16,
    pub upstream_status: Option<u16>,
    pub latency_ms: u64,
}

impl ProbeFailure {
    fn new(error_code: &'static str, message: &'static str, http_status: u16) -> Self {
        Self {
            error_code,
            message,
            http_status,
            upstream_status: None,
            latency_ms: 0,
        }
    }
}

#[derive(Clone, Debug)]
struct ProbeMaterial {
    platform: String,
    account_type: String,
    auth_index: String,
    auth_file: String,
    proxy_url: Option<String>,
    base_url: Option<String>,
    project_id: Option<String>,
}

fn account_material(store: &StateStore, account_id: i64) -> Result<Option<ProbeMaterial>> {
    store.with_connection(|connection| {
        connection
            .query_row(
                "SELECT a.platform,a.account_type,a.auth_index,a.auth_file,
                        COALESCE(p.normalized_url,json_extract(a.payload,'$.proxy_url')),
                        json_extract(a.payload,'$.credentials.base_url'),
                        json_extract(a.payload,'$.credentials.project_id')
                 FROM accounts a
                 LEFT JOIN proxies p ON p.id=COALESCE(
                    a.proxy_id,
                    CAST(json_extract(a.payload,'$.proxy_id') AS INTEGER)
                 ) AND p.deleted_at IS NULL
                 WHERE a.id=?1 AND a.deleted_at IS NULL",
                rusqlite::params![account_id],
                |row| {
                    Ok(ProbeMaterial {
                        platform: row.get(0)?,
                        account_type: row.get(1)?,
                        auth_index: row.get(2)?,
                        auth_file: row.get(3)?,
                        proxy_url: row.get(4)?,
                        base_url: row.get(5)?,
                        project_id: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    })
}

async fn authoritative_auth_index(
    cli: &CliProxyManagementClient,
    cli_index_map: &RwLock<HashMap<String, i64>>,
    account_id: i64,
    material: &ProbeMaterial,
) -> String {
    if material.account_type.eq_ignore_ascii_case("oauth") {
        let expected_name = Path::new(&material.auth_file)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if !expected_name.is_empty() {
            if let Ok(response) = cli.auth_files().await {
                let files = response.get("files").cloned().unwrap_or(response);
                if let Some(index) = files.as_array().and_then(|items| {
                    items.iter().find_map(|item| {
                        let name = item
                            .get("name")
                            .or_else(|| item.get("filename"))
                            .and_then(Value::as_str)?;
                        if name != expected_name
                            || item.get("runtime_only").and_then(Value::as_bool) == Some(true)
                        {
                            return None;
                        }
                        item.get("auth_index")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                }) {
                    return index;
                }
            }
        }
    } else {
        let mut indexes = cli_index_map
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter_map(|(index, mapped_id)| (*mapped_id == account_id).then_some(index.clone()))
            .collect::<Vec<_>>();
        indexes.sort_unstable();
        if let Some(index) = indexes.into_iter().next() {
            return index;
        }
    }
    material.auth_index.clone()
}

fn clean_optional(value: &Option<String>) -> Option<&str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn endpoint(base_url: &str, suffix: &str) -> Result<String> {
    let base_url = base_url.trim_end_matches('/');
    let suffix = if base_url.ends_with("/v1") && suffix.starts_with("/v1/") {
        suffix.trim_start_matches("/v1")
    } else if base_url.ends_with("/v1beta") && suffix.starts_with("/v1beta/") {
        suffix.trim_start_matches("/v1beta")
    } else {
        suffix
    };
    let value = format!("{base_url}{suffix}");
    let parsed = url::Url::parse(&value).context("parse account probe URL")?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        bail!("account probe URL must be absolute HTTP(S)");
    }
    Ok(value)
}

fn build_probe_request(material: &ProbeMaterial, auth_index: &str) -> Result<Value> {
    let platform = config_compiler::normalize_platform(&material.platform);
    let oauth = material.account_type.eq_ignore_ascii_case("oauth");
    let (method, url, headers, data) = if oauth {
        match platform.as_str() {
            "openai" => (
                "GET",
                "https://api.openai.com/v1/models".to_owned(),
                json!({
                    "Authorization":"Bearer $TOKEN$",
                    "Accept":"application/json"
                }),
                None,
            ),
            "anthropic" => (
                "GET",
                "https://api.anthropic.com/api/oauth/profile".to_owned(),
                json!({
                    "Authorization":"Bearer $TOKEN$",
                    "Accept":"application/json",
                    "User-Agent":"axios/1.15.2",
                    "Cache-Control":"no-cache"
                }),
                None,
            ),
            "gemini" => (
                "GET",
                "https://www.googleapis.com/oauth2/v2/userinfo?alt=json".to_owned(),
                json!({"Authorization":"Bearer $TOKEN$","Accept":"application/json"}),
                None,
            ),
            "antigravity" => {
                let data = clean_optional(&material.project_id)
                    .map(|project| json!({"project":project}).to_string())
                    .unwrap_or_else(|| "{}".to_owned());
                (
                    "POST",
                    "https://cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels"
                        .to_owned(),
                    json!({
                        "Authorization":"Bearer $TOKEN$",
                        "Content-Type":"application/json",
                        "User-Agent":"antigravity"
                    }),
                    Some(data),
                )
            }
            "xai" => (
                "GET",
                "https://api.x.ai/v1/models".to_owned(),
                json!({"Authorization":"Bearer $TOKEN$","Accept":"application/json"}),
                None,
            ),
            _ => bail!("unsupported OAuth probe provider"),
        }
    } else {
        match platform.as_str() {
            "anthropic" => {
                let base =
                    clean_optional(&material.base_url).unwrap_or("https://api.anthropic.com");
                (
                    "GET",
                    endpoint(base, "/v1/models?limit=1")?,
                    json!({
                        "x-api-key":"$TOKEN$",
                        "anthropic-version":"2023-06-01",
                        "Accept":"application/json"
                    }),
                    None,
                )
            }
            "gemini" => {
                let base = clean_optional(&material.base_url)
                    .unwrap_or("https://generativelanguage.googleapis.com");
                (
                    "GET",
                    endpoint(base, "/v1beta/models")?,
                    json!({"x-goog-api-key":"$TOKEN$","Accept":"application/json"}),
                    None,
                )
            }
            "xai" => {
                let base = clean_optional(&material.base_url).unwrap_or("https://api.x.ai/v1");
                (
                    "GET",
                    endpoint(base, "/v1/models")?,
                    json!({"Authorization":"Bearer $TOKEN$","Accept":"application/json"}),
                    None,
                )
            }
            "openai" => {
                let base =
                    clean_optional(&material.base_url).unwrap_or("https://api.openai.com/v1");
                (
                    "GET",
                    endpoint(base, "/v1/models")?,
                    json!({"Authorization":"Bearer $TOKEN$","Accept":"application/json"}),
                    None,
                )
            }
            _ => {
                let Some(base) = clean_optional(&material.base_url) else {
                    bail!("custom API account probe requires base_url");
                };
                (
                    "GET",
                    endpoint(base, "/v1/models")?,
                    json!({"Authorization":"Bearer $TOKEN$","Accept":"application/json"}),
                    None,
                )
            }
        }
    };

    let mut request = json!({
        "auth_index":auth_index,
        "method":method,
        "url":url,
        "header":headers,
    });
    if let Some(data) = data {
        request["data"] = Value::String(data);
    }
    let runtime_proxy = std::env::var("CODEX_ROUTER_PROXY_URL").ok();
    if let Some(proxy_url) = clean_optional(&material.proxy_url).or_else(|| {
        runtime_proxy
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }) {
        request["proxy_url"] = Value::String(proxy_url.to_owned());
    }
    Ok(request)
}

fn upstream_failure(status: u16, latency_ms: u64) -> ProbeFailure {
    let (code, message, http_status) = match status {
        400 => ("CR-UP-0001", "account probe request was rejected", 400),
        401 => ("CR-UP-0002", "account credential was rejected", 502),
        403 => (
            "CR-UP-0003",
            "account is not permitted to use the provider",
            502,
        ),
        404 => ("CR-UP-0004", "account probe resource was not found", 502),
        408 => ("CR-UP-0005", "account probe timed out upstream", 504),
        409 => ("CR-UP-0006", "account probe conflicted upstream", 409),
        413 => ("CR-UP-0007", "account probe was too large", 413),
        429 => ("CR-UP-0008", "account is rate limited", 429),
        500 => ("CR-UP-0009", "provider failed the account probe", 502),
        502 => (
            "CR-UP-0010",
            "provider gateway failed the account probe",
            502,
        ),
        503 => ("CR-UP-0011", "provider is unavailable", 503),
        504 => ("CR-UP-0012", "provider gateway timed out", 504),
        _ => (
            "CR-UP-0013",
            "provider returned an unexpected probe response",
            502,
        ),
    };
    ProbeFailure {
        error_code: code,
        message,
        http_status,
        upstream_status: Some(status),
        latency_ms,
    }
}

pub async fn probe_account(
    store: &StateStore,
    cli: &CliProxyManagementClient,
    cli_index_map: &RwLock<HashMap<String, i64>>,
    account_id: i64,
    model: &str,
) -> std::result::Result<ProbeSuccess, ProbeFailure> {
    let model = model.trim();
    if model.is_empty() {
        return Err(ProbeFailure::new(
            "CR-VAL-0001",
            "model is required for an account probe",
            400,
        ));
    }
    let material = account_material(store, account_id)
        .map_err(|_| ProbeFailure::new("CR-STO-0003", "could not read account", 500))?
        .ok_or_else(|| ProbeFailure::new("CR-STO-0007", "account not found", 404))?;
    let auth_index = authoritative_auth_index(cli, cli_index_map, account_id, &material).await;
    if auth_index.trim().is_empty() {
        return Err(ProbeFailure::new(
            "CR-OAU-0008",
            "account has no CLI auth mapping",
            502,
        ));
    }
    let request = build_probe_request(&material, &auth_index)
        .map_err(|_| ProbeFailure::new("CR-VAL-0003", "account provider cannot be probed", 400))?;
    let started = Instant::now();
    let (management_status, response) = cli
        .post_status("/v0/management/api-call", request)
        .await
        .map_err(|_| {
        let mut failure = ProbeFailure::new("CR-CLI-0005", "CLI account probe is unavailable", 502);
        failure.latency_ms = started.elapsed().as_millis() as u64;
        failure
    })?;
    let latency_ms = started.elapsed().as_millis() as u64;
    if !management_status.is_success() {
        return Err(upstream_failure(management_status.as_u16(), latency_ms));
    }
    let Some(response) = response else {
        let mut failure =
            ProbeFailure::new("CR-CLI-0006", "CLI account probe response is invalid", 502);
        failure.latency_ms = latency_ms;
        return Err(failure);
    };
    let Some(status) = response.get("status_code").and_then(Value::as_u64) else {
        let mut failure =
            ProbeFailure::new("CR-CLI-0006", "CLI account probe response is invalid", 502);
        failure.latency_ms = latency_ms;
        return Err(failure);
    };
    let status = status as u16;
    if !(200..300).contains(&status) {
        return Err(upstream_failure(status, latency_ms));
    }
    if response
        .get("body")
        .and_then(Value::as_str)
        .is_some_and(|body| !body.trim().is_empty() && serde_json::from_str::<Value>(body).is_err())
    {
        let mut failure = ProbeFailure::new(
            "CR-UP-0013",
            "provider returned an invalid probe payload",
            502,
        );
        failure.upstream_status = Some(status);
        failure.latency_ms = latency_ms;
        return Err(failure);
    }
    Ok(ProbeSuccess {
        account_id,
        model: model.to_owned(),
        latency_ms,
        upstream_status: status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_api_endpoints_do_not_duplicate_version_segments() {
        assert_eq!(
            endpoint("https://api.openai.com/v1", "/v1/models").unwrap(),
            "https://api.openai.com/v1/models"
        );
        assert_eq!(
            endpoint(
                "https://generativelanguage.googleapis.com/v1beta",
                "/v1beta/models"
            )
            .unwrap(),
            "https://generativelanguage.googleapis.com/v1beta/models"
        );
    }

    #[test]
    fn upstream_statuses_have_stable_error_codes() {
        assert_eq!(upstream_failure(401, 1).error_code, "CR-UP-0002");
        assert_eq!(upstream_failure(429, 1).error_code, "CR-UP-0008");
        assert_eq!(upstream_failure(503, 1).error_code, "CR-UP-0011");
    }
}
