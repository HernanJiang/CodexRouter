//! Router Host account usage querying, normalization, and last-good caching.

use crate::backend::cli_proxy::CliProxyManagementClient;
use crate::state::StateStore;
use crate::telemetry::structured_log::StructuredLogger;
use anyhow::Context;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Clone, Debug)]
struct Material {
    platform: String,
    account_type: String,
    auth_index: String,
    auth_file: String,
    proxy_url: Option<String>,
    project_id: String,
    account_id: String,
}

#[derive(Clone, Debug)]
struct Window {
    model: String,
    display_name: String,
    used_percent: f64,
    reset_at: Option<String>,
    sampled_at: String,
}

#[derive(Clone, Copy, Debug)]
struct LiveFailure {
    error_code: &'static str,
    needs_reauth: bool,
}

pub(crate) enum AccountUsage {
    Found(Value),
    LiveUnavailable(Value),
    NotFound,
}

fn material(store: &StateStore, id: i64) -> anyhow::Result<Option<Material>> {
    store.with_connection(|connection| {
        connection
            .query_row(
                "SELECT a.platform,a.account_type,a.auth_index,a.auth_file,
                        COALESCE(p.normalized_url,json_extract(a.payload,'$.proxy_url')),
                        COALESCE(json_extract(a.payload,'$.credentials.project_id'),''),
                        COALESCE(json_extract(a.payload,'$.credentials.chatgpt_account_id'),
                                 json_extract(a.payload,'$.credentials.account_id'),'')
                 FROM accounts a
                 LEFT JOIN proxies p ON p.id=COALESCE(a.proxy_id,
                    CAST(json_extract(a.payload,'$.proxy_id') AS INTEGER))
                    AND p.deleted_at IS NULL
                 WHERE a.id=?1 AND a.deleted_at IS NULL",
                rusqlite::params![id],
                |row| {
                    Ok(Material {
                        platform: normalize_provider(&row.get::<_, String>(0)?),
                        account_type: row.get(1)?,
                        auth_index: row.get(2)?,
                        auth_file: row.get(3)?,
                        proxy_url: row.get(4)?,
                        project_id: row.get(5)?,
                        account_id: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    })
}

fn normalize_provider(platform: &str) -> String {
    match platform.trim().to_ascii_lowercase().as_str() {
        "chatgpt" => "openai".to_owned(),
        "xai" | "x-ai" => "grok".to_owned(),
        "claude" => "anthropic".to_owned(),
        value => value.to_owned(),
    }
}

fn auth_file_name_matches(actual: &str, expected: &str) -> bool {
    if cfg!(windows) {
        actual.eq_ignore_ascii_case(expected)
    } else {
        actual == expected
    }
}

fn proxy_url(material: &Material) -> Option<String> {
    material
        .proxy_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]
                .into_iter()
                .find_map(|name| {
                    std::env::var(name)
                        .ok()
                        .map(|value| value.trim().to_owned())
                        .filter(|value| !value.is_empty())
                })
        })
}

async fn authoritative_auth_index(cli: &CliProxyManagementClient, material: &Material) -> String {
    let expected = Path::new(&material.auth_file)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if !expected.is_empty() {
        if let Ok(response) = cli.auth_files().await {
            let files = response.get("files").cloned().unwrap_or(response);
            if let Some(index) = files.as_array().and_then(|items| {
                items.iter().find_map(|item| {
                    let name = item
                        .get("name")
                        .or_else(|| item.get("filename"))
                        .and_then(Value::as_str)?;
                    if !auth_file_name_matches(name, expected)
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
    material.auth_index.clone()
}

fn normalize(upstream: &Value) -> anyhow::Result<Vec<Window>> {
    let models = upstream
        .get("models")
        .and_then(Value::as_object)
        .context("Antigravity models payload is missing models")?;
    let sampled_at = chrono::Utc::now().to_rfc3339();
    let mut windows = models
        .iter()
        .filter_map(|(model, detail)| {
            let quota = detail.get("quotaInfo")?;
            let remaining = quota
                .get("remainingFraction")
                .and_then(|value| {
                    value
                        .as_f64()
                        .or_else(|| value.as_str()?.trim().parse::<f64>().ok())
                })?
                .clamp(0.0, 1.0);
            Some(Window {
                model: model.clone(),
                display_name: detail
                    .get("displayName")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or(model)
                    .to_owned(),
                used_percent: (1.0 - remaining) * 100.0,
                reset_at: quota
                    .get("resetTime")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned),
                sampled_at: sampled_at.clone(),
            })
        })
        .collect::<Vec<_>>();
    windows.sort_by(|left, right| left.model.cmp(&right.model));
    Ok(windows)
}

fn payload(windows: &[Window], stale: bool, source: &str) -> Value {
    let mut quota = serde_json::Map::new();
    let mut details = serde_json::Map::new();
    let generic = windows
        .iter()
        .map(|window| {
            quota.insert(
                window.model.clone(),
                json!({"utilization":window.used_percent,"reset_time":window.reset_at}),
            );
            details.insert(
                window.model.clone(),
                json!({"display_name":window.display_name}),
            );
            json!({
                "provider":"antigravity", "window":window.model,
                "used":window.used_percent.to_string(), "quota":"100",
                "reset_at":window.reset_at, "sampled_at":window.sampled_at,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "antigravity_quota":quota, "antigravity_quota_details":details,
        "windows":generic, "stale":stale, "source":source,
    })
}

fn persist(store: &StateStore, id: i64, provider: &str, windows: &[Window]) -> anyhow::Result<()> {
    store.with_connection(|connection| {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "DELETE FROM usage_windows WHERE account_id=?1 AND provider=?2",
            rusqlite::params![id, provider],
        )?;
        for window in windows {
            transaction.execute(
                "INSERT INTO usage_windows(account_id,provider,window_kind,used,quota,
                    reset_at,source,sampled_at)
                 VALUES(?1,?2,?3,?4,'100',?5,'live',?6)",
                rusqlite::params![
                    id,
                    provider,
                    window.model,
                    window.used_percent.to_string(),
                    window.reset_at,
                    window.sampled_at
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    })
}

fn cached(store: &StateStore, id: i64, platform: &str) -> anyhow::Result<Value> {
    let rows = store.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT provider,window_kind,used,quota,reset_at,source,sampled_at
             FROM usage_windows WHERE account_id=?1 ORDER BY provider,window_kind",
        )?;
        let rows = statement.query_map(rusqlite::params![id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    })?;
    let windows = rows
        .iter()
        .filter(|row| row.0 == platform)
        .filter_map(|row| {
            Some(Window {
                model: row.1.clone(),
                display_name: row.1.clone(),
                used_percent: row.2.parse().ok()?,
                reset_at: row.4.clone(),
                sampled_at: row.6.clone(),
            })
        })
        .collect::<Vec<_>>();
    match platform {
        "antigravity" => Ok(payload(&windows, true, "cache")),
        "openai" => Ok(openai_payload(&windows, true, "cache")),
        "grok" => Ok(grok_payload(&windows, true, "cache")),
        _ => Ok(json!({
            "windows":rows.into_iter().map(|row| json!({
                "provider":row.0, "window":row.1, "used":row.2, "quota":row.3,
                "reset_at":row.4, "source":row.5, "sampled_at":row.6,
            })).collect::<Vec<_>>(),
            "stale":true, "source":"cache",
        })),
    }
}

fn classify(status: u16) -> LiveFailure {
    match status {
        401 => LiveFailure {
            error_code: "unauthenticated",
            needs_reauth: true,
        },
        403 => LiveFailure {
            error_code: "forbidden",
            needs_reauth: false,
        },
        429 => LiveFailure {
            error_code: "rate_limited",
            needs_reauth: false,
        },
        _ => LiveFailure {
            error_code: "upstream_error",
            needs_reauth: false,
        },
    }
}

async fn fetch(
    cli: &CliProxyManagementClient,
    material: &Material,
) -> Result<Vec<Window>, LiveFailure> {
    let auth_index = authoritative_auth_index(cli, material).await;
    if auth_index.trim().is_empty() {
        return Err(LiveFailure {
            error_code: "unauthenticated",
            needs_reauth: true,
        });
    }
    let proxy = proxy_url(material);
    let mut last = LiveFailure {
        error_code: "network_error",
        needs_reauth: false,
    };
    for base in [
        "https://daily-cloudcode-pa.googleapis.com",
        "https://cloudcode-pa.googleapis.com",
    ] {
        let data = if material.project_id.trim().is_empty() {
            "{}".to_owned()
        } else {
            json!({"project":material.project_id.trim()}).to_string()
        };
        let mut request = json!({
            "auth_index":auth_index, "method":"POST",
            "url":format!("{base}/v1internal:fetchAvailableModels"),
            "header":{
                "Authorization":"Bearer $TOKEN$", "Content-Type":"application/json",
                "User-Agent":"antigravity"
            },
            "data":data,
        });
        if let Some(proxy) = proxy.as_deref() {
            request["proxy_url"] = Value::String(proxy.to_owned());
        }
        let response = match cli.post("/v0/management/api-call", request).await {
            Ok(response) => response,
            Err(_) => continue,
        };
        let status = response
            .get("status_code")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u16;
        if !(200..300).contains(&status) {
            last = classify(status);
            if matches!(status, 401 | 403 | 429) {
                return Err(last);
            }
            continue;
        }
        let Some(body) = response.get("body").and_then(Value::as_str) else {
            last = LiveFailure {
                error_code: "invalid_response",
                needs_reauth: false,
            };
            continue;
        };
        let upstream: Value = match serde_json::from_str(body) {
            Ok(value) => value,
            Err(_) => {
                last = LiveFailure {
                    error_code: "invalid_response",
                    needs_reauth: false,
                };
                continue;
            }
        };
        return normalize(&upstream).map_err(|_| LiveFailure {
            error_code: "invalid_response",
            needs_reauth: false,
        });
    }
    Err(last)
}

pub(crate) async fn query_account_usage(
    store: &StateStore,
    cli: &CliProxyManagementClient,
    logger: &StructuredLogger,
    id: i64,
) -> anyhow::Result<AccountUsage> {
    let Some(material) = material(store, id)? else {
        return Ok(AccountUsage::NotFound);
    };
    if material.platform == "antigravity" && material.account_type == "oauth" {
        match fetch(cli, &material).await {
            Ok(windows) => {
                if let Err(error) = persist(store, id, "antigravity", &windows) {
                    let _ = logger.write(json!({
                        "level":"WARN", "event":"control.antigravity_quota_cache_failed",
                        "account_id":id, "error_description":error.to_string()
                    }));
                }
                return Ok(AccountUsage::Found(payload(&windows, false, "upstream")));
            }
            Err(failure) => {
                let mut value = cached(store, id, &material.platform)
                    .unwrap_or_else(|_| payload(&[], true, "cache"));
                value["error_code"] = Value::String(failure.error_code.to_owned());
                value["needs_reauth"] = Value::Bool(failure.needs_reauth);
                let _ = logger.write(json!({
                    "level":"WARN", "event":"control.antigravity_quota_refresh_failed",
                    "account_id":id, "error_code":failure.error_code
                }));
                return Ok(AccountUsage::LiveUnavailable(value));
            }
        }
    }
    if material.account_type == "oauth" && matches!(material.platform.as_str(), "openai" | "grok") {
        let provider = material.platform.as_str();
        match fetch_provider(cli, &material).await {
            Ok(windows) => {
                let _ = persist(store, id, provider, &windows);
                return Ok(AccountUsage::Found(if provider == "openai" {
                    openai_payload(&windows, false, "upstream")
                } else {
                    grok_payload(&windows, false, "upstream")
                }));
            }
            Err(failure) => {
                let mut value = cached(store, id, provider).unwrap_or_else(|_| {
                    if provider == "openai" {
                        openai_payload(&[], true, "cache")
                    } else {
                        grok_payload(&[], true, "cache")
                    }
                });
                value["error_code"] = Value::String(failure.error_code.to_owned());
                value["needs_reauth"] = Value::Bool(failure.needs_reauth);
                let _ = logger.write(json!({
                    "level":"WARN", "event":"control.oauth_quota_refresh_failed",
                    "account_id":id, "platform":provider, "error_code":failure.error_code
                }));
                return Ok(AccountUsage::Found(value));
            }
        }
    }
    Ok(AccountUsage::Found(cached(store, id, &material.platform)?))
}

fn openai_payload(windows: &[Window], stale: bool, source: &str) -> Value {
    let mut value = json!({
        "stale": stale,
        "source": source,
    });
    for window in windows {
        let kind = match window.model.as_str() {
            "weekly" | "seven_day" => "seven_day",
            "monthly" => "monthly",
            _ => "five_hour",
        };
        value[kind] = json!({
            "used_percent": window.used_percent,
            "reset_at": window.reset_at,
        });
    }
    value
}

fn grok_payload(windows: &[Window], stale: bool, source: &str) -> Value {
    let mut value = json!({
        "stale": stale,
        "source": source,
    });
    if let Some(window) = windows.first() {
        value["billing"] = json!({
            "usage_percent": window.used_percent,
            "period_type": if window.model == "monthly" {
                "monthly"
            } else {
                "weekly"
            },
            "period_end": window.reset_at,
            "plan": if window.display_name.is_empty() {
                "Grok".to_owned()
            } else {
                window.display_name.clone()
            },
        });
    }
    value
}

fn number_field(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|item| {
            item.as_f64()
                .or_else(|| item.as_i64().map(|number| number as f64))
                .or_else(|| item.as_str()?.trim().parse::<f64>().ok())
        })
    })
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
    })
}

fn used_percent_from(value: &Value) -> Option<f64> {
    if let Some(percent) = number_field(
        value,
        &[
            "used_percent",
            "usedPercent",
            "usage_percent",
            "usagePercent",
            "utilization",
        ],
    ) {
        return Some(percent.clamp(0.0, 100.0));
    }
    let used = number_field(value, &["used", "used_credits", "usedCredits"]);
    let limit = number_field(value, &["limit", "quota", "total", "allowed"]);
    let remaining = number_field(
        value,
        &["remaining", "remaining_credits", "remainingCredits"],
    );
    match (used, limit, remaining) {
        (Some(used), Some(limit), _) if limit > 0.0 => {
            Some((used / limit * 100.0).clamp(0.0, 100.0))
        }
        (_, Some(limit), Some(remaining)) if limit > 0.0 => {
            Some(((limit - remaining) / limit * 100.0).clamp(0.0, 100.0))
        }
        _ => None,
    }
}

fn window_kind_from(seconds: i64, fallback: &str) -> String {
    match seconds {
        1..=30_000 => "fiveHour".to_owned(),
        30_001..=900_000 => "weekly".to_owned(),
        900_001.. => "monthly".to_owned(),
        _ => fallback.to_owned(),
    }
}

fn normalize_openai_usage(upstream: &Value) -> Vec<Window> {
    let sampled_at = chrono::Utc::now().to_rfc3339();
    let mut windows = Vec::new();
    let rate_limit = upstream
        .get("rate_limit")
        .or_else(|| upstream.get("rateLimit"))
        .unwrap_or(upstream);
    for (name, fallback) in [
        ("primary_window", "fiveHour"),
        ("secondary_window", "weekly"),
        ("five_hour", "fiveHour"),
        ("seven_day", "weekly"),
        ("monthly", "monthly"),
    ] {
        let Some(window) = rate_limit.get(name).or_else(|| upstream.get(name)) else {
            continue;
        };
        let Some(used_percent) = used_percent_from(window) else {
            continue;
        };
        let seconds = number_field(window, &["limit_window_seconds", "limitWindowSeconds"])
            .unwrap_or(0.0) as i64;
        windows.push(Window {
            model: window_kind_from(seconds, fallback),
            display_name: "ChatGPT".to_owned(),
            used_percent,
            reset_at: string_field(window, &["reset_at", "resetAt", "reset_time", "resetTime"]),
            sampled_at: sampled_at.clone(),
        });
    }
    windows
}

fn normalize_grok_usage(upstream: &Value) -> Vec<Window> {
    let sampled_at = chrono::Utc::now().to_rfc3339();
    let billing = upstream
        .get("billing")
        .or_else(|| upstream.get("config"))
        .unwrap_or(upstream);
    let current = billing
        .get("currentPeriod")
        .or_else(|| billing.get("current_period"))
        .unwrap_or(billing);
    let used_percent = used_percent_from(billing)
        .or_else(|| used_percent_from(current))
        .or_else(|| number_field(billing, &["creditUsagePercent", "credit_usage_percent"]));
    let Some(used_percent) = used_percent else {
        return Vec::new();
    };
    let period = string_field(current, &["type", "period_type", "periodType"])
        .or_else(|| string_field(billing, &["period_type", "periodType"]))
        .unwrap_or_default();
    let kind = if period.to_ascii_lowercase().contains("month") {
        "monthly"
    } else {
        "weekly"
    };
    vec![Window {
        model: kind.to_owned(),
        display_name: string_field(billing, &["plan", "product"])
            .unwrap_or_else(|| "Grok".to_owned()),
        used_percent,
        reset_at: string_field(
            current,
            &["end", "period_end", "periodEnd", "reset_at", "resetAt"],
        )
        .or_else(|| string_field(billing, &["period_end", "periodEnd", "reset_at", "resetAt"])),
        sampled_at,
    }]
}

async fn api_call(
    cli: &CliProxyManagementClient,
    material: &Material,
    method: &str,
    url: &str,
    headers: Value,
    data: Option<String>,
) -> Result<Value, LiveFailure> {
    let auth_index = authoritative_auth_index(cli, material).await;
    if auth_index.trim().is_empty() {
        return Err(LiveFailure {
            error_code: "unauthenticated",
            needs_reauth: true,
        });
    }
    let mut request = json!({
        "auth_index": auth_index,
        "method": method,
        "url": url,
        "header": headers,
    });
    if let Some(data) = data {
        request["data"] = Value::String(data);
    }
    if let Some(proxy) = proxy_url(material) {
        request["proxy_url"] = Value::String(proxy);
    }
    let response = cli
        .post("/v0/management/api-call", request)
        .await
        .map_err(|_| LiveFailure {
            error_code: "network_error",
            needs_reauth: false,
        })?;
    let status = response
        .get("status_code")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u16;
    if !(200..300).contains(&status) {
        return Err(classify(status));
    }
    let Some(body) = response.get("body").and_then(Value::as_str) else {
        return Err(LiveFailure {
            error_code: "invalid_response",
            needs_reauth: false,
        });
    };
    serde_json::from_str(body).map_err(|_| LiveFailure {
        error_code: "invalid_response",
        needs_reauth: false,
    })
}

async fn fetch_provider(
    cli: &CliProxyManagementClient,
    material: &Material,
) -> Result<Vec<Window>, LiveFailure> {
    match material.platform.as_str() {
        "openai" => {
            let mut headers = json!({
                "Authorization": "Bearer $TOKEN$",
                "Accept": "application/json",
                "User-Agent": "codex_cli_rs"
            });
            if !material.account_id.trim().is_empty() {
                headers["ChatGPT-Account-ID"] =
                    Value::String(material.account_id.trim().to_owned());
            }
            let upstream = api_call(
                cli,
                material,
                "GET",
                "https://chatgpt.com/backend-api/wham/usage",
                headers,
                None,
            )
            .await?;
            let windows = normalize_openai_usage(&upstream);
            if windows.is_empty() {
                return Err(LiveFailure {
                    error_code: "invalid_response",
                    needs_reauth: false,
                });
            }
            Ok(windows)
        }
        "grok" => {
            let headers = json!({
                "Authorization": "Bearer $TOKEN$",
                "Accept": "application/json",
                "Content-Type": "application/json",
                "User-Agent": "grok-pager/0.2.93 grok-shell/0.2.93 (windows; x86_64)",
                "x-xai-token-auth": "xai-grok-cli",
                "x-grok-client-version": "0.2.93"
            });
            let mut last = LiveFailure {
                error_code: "network_error",
                needs_reauth: false,
            };
            for url in [
                "https://cli-chat-proxy.grok.com/v1/billing?format=credits",
                "https://cli-chat-proxy.grok.com/v1/billing",
            ] {
                match api_call(cli, material, "GET", url, headers.clone(), None).await {
                    Ok(upstream) => {
                        let windows = normalize_grok_usage(&upstream);
                        if !windows.is_empty() {
                            return Ok(windows);
                        }
                        last = LiveFailure {
                            error_code: "invalid_response",
                            needs_reauth: false,
                        };
                    }
                    Err(error) => {
                        last = error;
                        if matches!(
                            error.error_code,
                            "unauthenticated" | "forbidden" | "rate_limited"
                        ) {
                            return Err(error);
                        }
                    }
                }
            }
            Err(last)
        }
        _ => fetch(cli, material).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_usage_reads_primary_and_weekly_windows() {
        let windows = normalize_openai_usage(&json!({
            "rate_limit": {
                "primary_window": {
                    "used_percent": 12.5,
                    "limit_window_seconds": 18000,
                    "reset_at": "2026-08-20T12:00:00Z"
                },
                "secondary_window": {
                    "used_percent": 40.0,
                    "limit_window_seconds": 604800
                }
            }
        }));
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].model, "fiveHour");
        assert_eq!(windows[0].used_percent, 12.5);
        assert_eq!(windows[1].model, "weekly");
        let payload = openai_payload(&windows, false, "upstream");
        assert_eq!(payload["five_hour"]["used_percent"], 12.5);
        assert_eq!(payload["seven_day"]["used_percent"], 40.0);
        assert_eq!(payload["stale"], false);
    }

    #[test]
    fn grok_usage_reads_billing_percent_and_omits_empty_payload() {
        let windows = normalize_grok_usage(&json!({
            "billing": {
                "usage_percent": 33.0,
                "period_type": "weekly",
                "period_end": "2026-08-21T00:00:00Z",
                "plan": "SuperGrok"
            }
        }));
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].used_percent, 33.0);
        assert_eq!(windows[0].model, "weekly");
        let payload = grok_payload(&windows, false, "upstream");
        assert_eq!(payload["billing"]["usage_percent"], 33.0);
        assert_eq!(payload["billing"]["plan"], "SuperGrok");
        assert!(grok_payload(&[], true, "cache").get("billing").is_none());
        let from_config = normalize_grok_usage(&json!({
            "config": {
                "creditUsagePercent": 18.0,
                "currentPeriod": { "type": "week", "end": "2026-08-22T00:00:00Z" }
            }
        }));
        assert_eq!(from_config.len(), 1);
        assert_eq!(from_config[0].used_percent, 18.0);
        assert_eq!(from_config[0].model, "weekly");
    }

    #[test]
    fn provider_aliases_and_windows_auth_file_names_use_one_query_path() {
        assert_eq!(normalize_provider("grok"), "grok");
        assert_eq!(normalize_provider("xAI"), "grok");
        assert_eq!(normalize_provider("x-ai"), "grok");
        assert_eq!(normalize_provider("ChatGPT"), "openai");
        if cfg!(windows) {
            assert!(auth_file_name_matches(
                "GROK-ACCOUNT.JSON",
                "grok-account.json"
            ));
        }
    }
}
