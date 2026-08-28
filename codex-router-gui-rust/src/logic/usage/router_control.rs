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
    status: String,
    schedulable: bool,
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
                                 json_extract(a.payload,'$.credentials.account_id'),''),
                        a.status,a.schedulable
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
                        status: row.get(7)?,
                        schedulable: row.get::<_, i64>(8)? != 0,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    })
}

/// How long a `needs_reauth` (401 / rotated-out refresh token) OAuth account
/// stays away from the upstream after the first failed live call. Without the
/// cooldown every quota poll re-presents the dead refresh token, which makes
/// the provider invalidate the whole token family — including the Codex
/// desktop session that shares the account — and spams the event log.
const REAUTH_COOLDOWN: chrono::Duration = chrono::Duration::hours(1);

fn reauth_cooldown_key(id: i64) -> String {
    format!("oauth_reauth_cooldown_until.{id}")
}

pub(crate) fn reauth_cooldown_active(store: &StateStore, id: i64) -> anyhow::Result<bool> {
    let account_type = store.with_connection(|connection| {
        connection
            .query_row(
                "SELECT account_type FROM accounts WHERE id=?1 AND deleted_at IS NULL",
                rusqlite::params![id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(Into::into)
    })?;
    if account_type.as_deref() != Some("oauth") {
        return Ok(false);
    }
    let Some(raw) = store.setting(&reauth_cooldown_key(id))? else {
        return Ok(false);
    };
    let text = serde_json::from_str::<String>(&raw).context("decode re-auth cooldown")?;
    if text.trim().is_empty() {
        return Ok(false);
    }
    let until = chrono::DateTime::parse_from_rfc3339(&text).context("parse re-auth cooldown")?;
    Ok(until > chrono::Utc::now())
}

pub(crate) fn note_reauth_failure(store: &StateStore, id: i64) -> anyhow::Result<()> {
    let is_oauth = store.with_connection(|connection| {
        connection
            .query_row(
                "SELECT account_type='oauth' FROM accounts WHERE id=?1 AND deleted_at IS NULL",
                rusqlite::params![id],
                |row| row.get::<_, bool>(0),
            )
            .optional()
            .map(|value| value.unwrap_or(false))
            .map_err(Into::into)
    })?;
    if !is_oauth {
        return Ok(());
    }
    let until = chrono::Utc::now() + REAUTH_COOLDOWN;
    store.set_setting(&reauth_cooldown_key(id), &Value::String(until.to_rfc3339()))
}

pub(crate) fn clear_reauth_cooldown(store: &StateStore, id: i64) -> anyhow::Result<()> {
    store.with_connection(|connection| {
        connection.execute(
            "DELETE FROM admin_settings WHERE key=?1",
            rusqlite::params![reauth_cooldown_key(id)],
        )?;
        Ok(())
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
    desktop_auth_path: Option<&Path>,
    id: i64,
) -> anyhow::Result<AccountUsage> {
    let Some(material) = material(store, id)? else {
        return Ok(AccountUsage::NotFound);
    };
    // A disabled or isolated OAuth account must never touch its upstream
    // token: every live call would re-present the credential the operator
    // deliberately parked.
    if material.account_type == "oauth" && (!material.schedulable || material.status != "active") {
        return Ok(AccountUsage::Found(cached(store, id, &material.platform)?));
    }
    // After a 401 the refresh token is dead until the user re-authenticates.
    // Serve the last-good cache during the cooldown instead of hammering the
    // provider with the rotated-out token on every poll.
    if material.account_type == "oauth" && reauth_cooldown_active(store, id)? {
        let mut value = cached(store, id, &material.platform)?;
        value["error_code"] = Value::String("unauthenticated".to_owned());
        value["needs_reauth"] = Value::Bool(true);
        return Ok(AccountUsage::Found(value));
    }
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
                if failure.needs_reauth {
                    note_reauth_failure(store, id)?;
                }
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
    if material.account_type == "oauth" && material.platform == "openai" {
        // Read-only wham probe using Desktop's current access token (never
        // the CLIProxyAPI refresh-capable path), mirroring the Grok probe.
        // Scheduling stays observational -- Desktop owns this account's
        // health -- but the quota windows are live so the dashboard shows the
        // real 5-hour / weekly caps instead of a stale empty cache.
        match fetch_provider(cli, desktop_auth_path, &material).await {
            Ok(windows) => {
                if let Err(error) = persist(store, id, "openai", &windows) {
                    let _ = logger.write(json!({
                        "level":"WARN", "event":"control.openai_quota_cache_failed",
                        "account_id":id, "error_description":error.to_string()
                    }));
                }
                return Ok(AccountUsage::Found(openai_payload(
                    &windows, false, "upstream",
                )));
            }
            Err(failure) => {
                if failure.needs_reauth {
                    note_reauth_failure(store, id)?;
                }
                let mut value = cached(store, id, "openai")
                    .unwrap_or_else(|_| openai_payload(&[], true, "cache"));
                value["error_code"] = Value::String(failure.error_code.to_owned());
                value["needs_reauth"] = Value::Bool(failure.needs_reauth);
                let _ = logger.write(json!({
                    "level":"WARN", "event":"control.oauth_quota_refresh_failed",
                    "account_id":id, "platform":"openai", "error_code":failure.error_code
                }));
                return Ok(AccountUsage::Found(value));
            }
        }
    }
    if material.account_type == "oauth" && material.platform == "grok" {
        match fetch_provider(cli, desktop_auth_path, &material).await {
            Ok(windows) => {
                let _ = persist(store, id, "grok", &windows);
                return Ok(AccountUsage::Found(grok_payload(
                    &windows, false, "upstream",
                )));
            }
            Err(failure) => {
                if failure.needs_reauth {
                    note_reauth_failure(store, id)?;
                }
                let mut value =
                    cached(store, id, "grok").unwrap_or_else(|_| grok_payload(&[], true, "cache"));
                value["error_code"] = Value::String(failure.error_code.to_owned());
                value["needs_reauth"] = Value::Bool(failure.needs_reauth);
                let _ = logger.write(json!({
                    "level":"WARN", "event":"control.oauth_quota_refresh_failed",
                    "account_id":id, "platform":"grok", "error_code":failure.error_code
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
            "creditUsagePercent",
            "credit_usage_percent",
            "utilization",
        ],
    ) {
        return Some(percent.clamp(0.0, 100.0));
    }
    let used = number_field(
        value,
        &[
            "used",
            "used_credits",
            "usedCredits",
            "credits_used",
            "creditsUsed",
        ],
    );
    let limit = number_field(
        value,
        &[
            "limit",
            "quota",
            "total",
            "allowed",
            "total_credits",
            "totalCredits",
            "max",
        ],
    );
    let remaining = number_field(
        value,
        &[
            "remaining",
            "remaining_credits",
            "remainingCredits",
            "balance",
            "left",
        ],
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
    let root = upstream.get("data").unwrap_or(upstream);
    let billing = root
        .get("billing")
        .or_else(|| root.get("config"))
        .or_else(|| root.get("usage"))
        .unwrap_or(root);
    let current = billing
        .get("currentPeriod")
        .or_else(|| billing.get("current_period"))
        .unwrap_or(billing);
    let credits = root
        .get("credits")
        .or_else(|| billing.get("credits"))
        .or_else(|| current.get("credits"));
    let used_percent = used_percent_from(billing)
        .or_else(|| used_percent_from(current))
        .or_else(|| credits.and_then(used_percent_from))
        .or_else(|| used_percent_from(root))
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
    match response.get("body") {
        Some(Value::String(text)) => serde_json::from_str(text).map_err(|_| LiveFailure {
            error_code: "invalid_response",
            needs_reauth: false,
        }),
        Some(value) if !value.is_null() => Ok(value.clone()),
        _ => Err(LiveFailure {
            error_code: "invalid_response",
            needs_reauth: false,
        }),
    }
}

/// ChatGPT usage endpoint. Overridable so tests can point the Desktop-owned
/// quota probe at a loopback mock instead of the real upstream.
fn openai_usage_endpoint() -> String {
    std::env::var("CODEX_ROUTER_WHAM_USAGE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "https://chatgpt.com/backend-api/wham/usage".to_owned())
}

/// Fetch ChatGPT subscription quota for a Desktop-owned OpenAI OAuth account.
/// Desktop's auth.json is the only credential source and this is a read-only
/// GET with its current access token. It must never travel through
/// CLIProxyAPI's `$TOKEN$` machinery: that path lets the CLI side refresh the
/// credential, and a second refresh client makes OpenAI revoke the whole
/// token family (refresh_token_invalidated), logging Codex Desktop out.
async fn fetch_openai_usage_direct(
    desktop_auth_path: Option<&Path>,
    material: &Material,
) -> Result<Vec<Window>, LiveFailure> {
    let unavailable = || LiveFailure {
        error_code: "auth_unavailable",
        needs_reauth: false,
    };
    let Some(path) = desktop_auth_path else {
        return Err(unavailable());
    };
    let token = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|auth| {
            auth.pointer("/tokens/access_token")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
        .ok_or_else(unavailable)?;
    let mut builder = reqwest::Client::builder()
        .user_agent("codex_cli_rs")
        .timeout(std::time::Duration::from_secs(15));
    if let Some(proxy) = proxy_url(material).filter(|value| !value.trim().is_empty()) {
        if let Ok(proxy) = reqwest::Proxy::all(proxy.as_str()) {
            builder = builder.proxy(proxy);
        }
    }
    let client = builder.build().map_err(|_| LiveFailure {
        error_code: "network_error",
        needs_reauth: false,
    })?;
    let mut request = client
        .get(openai_usage_endpoint())
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json");
    if !material.account_id.trim().is_empty() {
        request = request.header("ChatGPT-Account-ID", material.account_id.trim());
    }
    let response = request.send().await.map_err(|_| LiveFailure {
        error_code: "network_error",
        needs_reauth: false,
    })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(classify(status));
    }
    let body: Value = response.json().await.map_err(|_| LiveFailure {
        error_code: "invalid_response",
        needs_reauth: false,
    })?;
    let windows = normalize_openai_usage(&body);
    if windows.is_empty() {
        return Err(LiveFailure {
            error_code: "invalid_response",
            needs_reauth: false,
        });
    }
    Ok(windows)
}

async fn fetch_provider(
    cli: &CliProxyManagementClient,
    desktop_auth_path: Option<&Path>,
    material: &Material,
) -> Result<Vec<Window>, LiveFailure> {
    match material.platform.as_str() {
        "openai" => fetch_openai_usage_direct(desktop_auth_path, material).await,
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
        let from_credits = normalize_grok_usage(&json!({
            "data": {
                "credits": { "remaining": 250.0, "total": 1000.0 },
                "currentPeriod": { "type": "month", "end": "2026-09-01T00:00:00Z" }
            }
        }));
        assert_eq!(from_credits.len(), 1);
        assert_eq!(from_credits[0].used_percent, 75.0);
        assert_eq!(from_credits[0].model, "monthly");
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

    fn test_store() -> (std::path::PathBuf, StateStore) {
        let root = std::env::temp_dir().join(format!("router-usage-{}", uuid::Uuid::now_v7()));
        let store = StateStore::open(root.join("router-state.sqlite3")).unwrap();
        (root, store)
    }

    fn insert_openai_oauth_account(store: &StateStore, status: &str, schedulable: i64) {
        store
            .with_connection(|connection| {
                connection.execute(
                    "INSERT INTO accounts(id,platform,account_type,auth_index,auth_file,
                        stable_identity_hmac,status,schedulable,priority,weight,payload)
                     VALUES(1,'openai','oauth','stored-index','legacy-openai-1.json',
                        'usage-account',?1,?2,1,1,
                        '{\"credentials\":{\"chatgpt_account_id\":\"00000000-0000-4000-8000-000000000001\"}}')",
                    rusqlite::params![status, schedulable],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn test_logger(root: &std::path::Path) -> StructuredLogger {
        StructuredLogger::open(root.join("router-events.jsonl")).unwrap()
    }

    fn unreachable_cli() -> CliProxyManagementClient {
        CliProxyManagementClient::new("http://127.0.0.1:1", "test-secret").unwrap()
    }

    #[test]
    fn reauth_cooldown_set_clear_cycle() {
        let (_root, store) = test_store();
        insert_openai_oauth_account(&store, "active", 1);
        assert!(!reauth_cooldown_active(&store, 1).unwrap());
        note_reauth_failure(&store, 1).unwrap();
        assert!(reauth_cooldown_active(&store, 1).unwrap());
        assert!(!reauth_cooldown_active(&store, 8).unwrap());
        clear_reauth_cooldown(&store, 1).unwrap();
        assert!(!reauth_cooldown_active(&store, 1).unwrap());
    }

    #[tokio::test]
    async fn disabled_oauth_account_serves_cache_without_touching_upstream() {
        let (root, store) = test_store();
        insert_openai_oauth_account(&store, "disabled", 0);
        let logger = test_logger(&root);
        let usage = query_account_usage(&store, &unreachable_cli(), &logger, None, 1)
            .await
            .unwrap();
        let AccountUsage::Found(value) = usage else {
            panic!("disabled accounts must be served from cache");
        };
        assert_eq!(value["stale"], true);
        assert_eq!(value["source"], "cache");
        assert!(value.get("error_code").is_none());
    }

    #[tokio::test]
    async fn cooling_down_account_serves_cache_with_needs_reauth_flag() {
        let (root, store) = test_store();
        insert_openai_oauth_account(&store, "active", 1);
        note_reauth_failure(&store, 1).unwrap();
        let logger = test_logger(&root);
        let usage = query_account_usage(&store, &unreachable_cli(), &logger, None, 1)
            .await
            .unwrap();
        let AccountUsage::Found(value) = usage else {
            panic!("cooling-down accounts must be served from cache");
        };
        assert_eq!(value["error_code"], "unauthenticated");
        assert_eq!(value["needs_reauth"], true);
    }

    #[tokio::test]
    async fn desktop_openai_quota_without_login_serves_cache_and_flags_unavailable() {
        // With no desktop auth path the read-only wham probe cannot run; the
        // account must fall back to cache and flag the cause instead of
        // arming a re-auth cooldown (the failure is not a dead refresh token).
        let (root, store) = test_store();
        insert_openai_oauth_account(&store, "active", 1);
        let logger = test_logger(&root);
        let usage = query_account_usage(&store, &unreachable_cli(), &logger, None, 1)
            .await
            .unwrap();
        let AccountUsage::Found(value) = usage else {
            panic!("desktop-owned ChatGPT quota must still resolve to a payload");
        };
        assert_eq!(value["error_code"], "auth_unavailable");
        assert_eq!(value["source"], "cache");
        assert_ne!(value["needs_reauth"], true);
        assert!(!reauth_cooldown_active(&store, 1).unwrap());
        let events = std::fs::read_to_string(logger.path()).unwrap();
        assert!(events.contains("control.oauth_quota_refresh_failed"));
        let _ = root;
    }

    #[tokio::test]
    async fn desktop_openai_quota_reads_live_wham_windows() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await;
            let body = concat!(
                r#"{"plan_type":"plus","rate_limit":{"allowed":true,"limit_reached":false,"#,
                r#""primary_window":{"used_percent":48,"limit_window_seconds":18000,"reset_after_seconds":5290,"reset_at":1787801733},"#,
                r#""secondary_window":{"used_percent":23,"limit_window_seconds":604800,"reset_after_seconds":573921,"reset_at":1788370364}}}"#
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });

        let (root, store) = test_store();
        insert_openai_oauth_account(&store, "active", 1);
        let auth_path = root.join("desktop-auth.json");
        std::fs::write(
            &auth_path,
            json!({"tokens":{"access_token":"test-access-token"}}).to_string(),
        )
        .unwrap();
        let logger = test_logger(&root);

        // Route the probe at the loopback mock and drop any system proxy env
        // so the request never leaves this process.
        for name in [
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            unsafe { std::env::remove_var(name) };
        }
        unsafe {
            std::env::set_var(
                "CODEX_ROUTER_WHAM_USAGE_URL",
                format!("http://{address}/backend-api/wham/usage"),
            );
        }

        let usage = query_account_usage(&store, &unreachable_cli(), &logger, Some(&auth_path), 1)
            .await
            .unwrap();
        let AccountUsage::Found(value) = usage else {
            panic!("live openai quota must resolve to a payload");
        };
        assert_eq!(value["five_hour"]["used_percent"], 48.0);
        assert_eq!(value["seven_day"]["used_percent"], 23.0);
        assert_eq!(value["stale"], false);
        assert_eq!(value["source"], "upstream");
        assert!(!reauth_cooldown_active(&store, 1).unwrap());
        let _ = root;
    }
}
