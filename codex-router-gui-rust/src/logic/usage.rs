use super::read_router_credential;
use crate::config::{atomic_write, ModelConfig, RouterConfig};
use crate::{UsageAccount, UsageModelSummary, UsageSnapshot, UsageTotals, UsageWindow};
use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, SecondsFormat, Utc};
use hmac::{Hmac, Mac};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use url::Url;
use zeroize::{Zeroize, Zeroizing};

const CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const MAX_CONCURRENCY: usize = 4;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug)]
struct AccountTask {
    account: Value,
    channel: Option<ModelConfig>,
    query_provider_usage: bool,
    auto_isolate_on_exhaustion: bool,
}

#[derive(Clone, Debug)]
struct AccountRecord {
    account: UsageAccount,
    configured_model: String,
    quota_evidence: OAuthQuotaEvidence,
    auto_isolate_on_exhaustion: bool,
    routing_changed: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum OAuthQuotaEvidence {
    Usable,
    Exhausted,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct OAuthRecoveryObservation {
    account_id: i64,
    #[serde(default)]
    exhausted: bool,
    #[serde(default)]
    observed_at: String,
    #[serde(default)]
    last_probe_at: String,
    #[serde(default)]
    next_probe_at: String,
    #[serde(default)]
    reset_at: String,
    #[serde(default)]
    recent_error: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct OAuthRecoveryObservations {
    #[serde(default)]
    entries: Vec<OAuthRecoveryObservation>,
}

const OAUTH_UNKNOWN_MAX_ISOLATION: chrono::Duration = chrono::Duration::hours(5);
const OAUTH_RECOVERY_MIN_INTERVAL_SECONDS: u64 = 10 * 60;

#[derive(Clone, Debug, Default)]
struct ProviderUsage {
    provider: String,
    windows: Vec<UsageWindow>,
    note: String,
    cached: bool,
}

fn credential_rejected(note: &str) -> bool {
    let note = note.to_ascii_lowercase();
    note.contains("router_kimi_credential_rejected")
        || note.contains("class=authentication")
        || note.contains("class=permission")
}

fn live_quota_is_usable(windows: &[UsageWindow]) -> bool {
    !windows.is_empty()
        && !windows
            .iter()
            .any(|window| window.used_percent.is_some_and(|value| value >= 99.999))
}

fn live_quota_is_exhausted(windows: &[UsageWindow]) -> bool {
    !windows.is_empty()
        && windows
            .iter()
            .any(|window| window.used_percent.is_some_and(|value| value >= 99.999))
}

fn maybe_isolate_exhausted_oauth_account(
    admin: &AdminClient,
    task: &AccountTask,
    windows: &[UsageWindow],
    fresh: bool,
    query_note: &str,
    deadline: Instant,
) -> bool {
    if !task.auto_isolate_on_exhaustion
        || string(&task.account, "type") != "oauth"
        || !fresh
        || !live_quota_is_exhausted(windows)
        || credential_rejected(query_note)
    {
        return false;
    }

    if string(&task.account, "status") == "error"
        || get(&task.account, "schedulable").and_then(Value::as_bool) == Some(false)
    {
        return true;
    }

    let id = integer(&task.account, "id");
    if id <= 0 {
        return false;
    }
    retry_account_read(|| {
        admin.post(
            &format!("/api/v1/admin/accounts/{id}/schedulable"),
            Some(&json!({ "schedulable": false })),
            remaining(deadline, Duration::from_secs(10))?,
        )
    })
    .is_ok()
}

fn oauth_accounts_with_api_fallback(cfg: &RouterConfig) -> HashSet<i64> {
    if !cfg.oauth_fallback.enabled {
        return HashSet::new();
    }
    cfg.models
        .iter()
        .filter(|model| model.source == "oauth" && model.oauth_account_id > 0)
        .filter(|oauth| {
            cfg.models.iter().any(|candidate| {
                candidate.source != "oauth"
                    && super::same_model_identity(&oauth.model, &candidate.model)
                    && super::is_fallback_channel_selected(cfg, candidate)
            })
        })
        .map(|model| model.oauth_account_id)
        .collect()
}

fn maybe_recover_misdisabled_account(
    admin: &AdminClient,
    task: &AccountTask,
    windows: &[UsageWindow],
    fresh: bool,
    query_note: &str,
    deadline: Instant,
) -> bool {
    let status = string(&task.account, "status");
    let schedulable = get(&task.account, "schedulable").and_then(Value::as_bool);
    if (status != "error" && schedulable != Some(false))
        || !fresh
        || !live_quota_is_usable(windows)
        || credential_rejected(query_note)
    {
        return false;
    }

    let id = integer(&task.account, "id");
    if id <= 0 {
        return false;
    }
    let recover_path = format!("/api/v1/admin/accounts/{id}/recover-state");
    if retry_account_read(|| {
        admin.post(
            &recover_path,
            None,
            remaining(deadline, Duration::from_secs(10))?,
        )
    })
    .is_err()
    {
        return false;
    }
    let schedulable_path = format!("/api/v1/admin/accounts/{id}/schedulable");
    retry_account_read(|| {
        admin.post(
            &schedulable_path,
            Some(&json!({ "schedulable": true })),
            remaining(deadline, Duration::from_secs(10))?,
        )
    })
    .is_ok()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CacheEntry {
    updated_at: String,
    windows: Vec<UsageWindow>,
    #[serde(default)]
    note: String,
}

type UsageCache = BTreeMap<String, CacheEntry>;

#[derive(Clone)]
pub(super) struct AdminClient {
    client: Client,
    base_url: String,
    bearer: Arc<Zeroizing<String>>,
}

impl AdminClient {
    #[cfg(test)]
    pub(super) fn for_test(base_url: String) -> Self {
        Self {
            client: Client::builder().no_proxy().build().unwrap(),
            base_url,
            bearer: Arc::new(Zeroizing::new("test-admin-token".to_owned())),
        }
    }

    pub(super) fn connect(router_root: &Path, cfg: &RouterConfig) -> anyhow::Result<Self> {
        let base_url = validate_loopback_base_url(&cfg.deploy.sub2api_host)?;
        let client = Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(4))
            .timeout(Duration::from_secs(15))
            .build()
            .context("class=configuration")?;
        if let Some(token) = read_cached_admin_token(router_root) {
            let candidate = Self {
                client: client.clone(),
                base_url: base_url.clone(),
                bearer: Arc::new(Zeroizing::new(token)),
            };
            if candidate
                .get(
                    "/api/v1/admin/groups/all?include_inactive=true",
                    Duration::from_secs(8),
                )
                .is_ok()
            {
                return Ok(candidate);
            }
        }

        let mut candidates = Vec::new();
        if let Some(secret) = read_router_credential("AdminPassword")? {
            let password =
                Zeroizing::new(String::from_utf16(&secret.0).context("class=authentication")?);
            if !password.trim().is_empty() {
                candidates.push(("admin@admin.com", password.clone()));
                candidates.push(("admin@sub2api.local", password));
            }
        }
        candidates.push(("admin@admin.com", Zeroizing::new("adminadmin".to_owned())));
        let mut rate_limited = false;
        for (email, password) in candidates {
            let response = client
                .post(format!("{base_url}/api/v1/auth/login"))
                .header(ACCEPT, "application/json")
                .json(&json!({ "email": email, "password": password.as_str() }))
                .send();
            match response {
                Ok(response) if response.status().is_success() => {
                    let body: Value = response
                        .json()
                        .map_err(|_| anyhow!("class=invalid_response"))?;
                    let token = get_path(&body, &["data", "access_token"])
                        .or_else(|| get(&body, "access_token"))
                        .and_then(Value::as_str)
                        .filter(|value| !value.trim().is_empty());
                    if let Some(token) = token {
                        let _ = write_cached_admin_token(router_root, token);
                        return Ok(Self {
                            client,
                            base_url,
                            bearer: Arc::new(Zeroizing::new(token.to_owned())),
                        });
                    }
                }
                Ok(response) if response.status().as_u16() == 429 => {
                    rate_limited = true;
                    break;
                }
                Ok(_) | Err(_) => {}
            }
        }
        if rate_limited {
            bail!("class=rate_limit")
        }
        bail!("class=authentication")
    }

    pub(super) fn get(&self, path: &str, timeout: Duration) -> anyhow::Result<Value> {
        let response = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .timeout(timeout)
            .bearer_auth(self.bearer.as_str())
            .header(ACCEPT, "application/json")
            .send()
            .map_err(classify_request_error)?;
        if !response.status().is_success() {
            return Err(anyhow!(classify_status(response.status().as_u16())));
        }
        response
            .json()
            .map_err(|_| anyhow!("class=invalid_response"))
    }

    pub(super) fn post(
        &self,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        let mut request = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .timeout(timeout)
            .bearer_auth(self.bearer.as_str())
            .header(ACCEPT, "application/json");
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().map_err(classify_request_error)?;
        if !response.status().is_success() {
            return Err(anyhow!(classify_status(response.status().as_u16())));
        }
        let text = response
            .text()
            .map_err(|_| anyhow!("class=invalid_response"))?;
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|_| anyhow!("class=invalid_response"))
    }

    pub(super) fn put(&self, path: &str, body: &Value, timeout: Duration) -> anyhow::Result<Value> {
        let response = self
            .client
            .put(format!("{}{}", self.base_url, path))
            .timeout(timeout)
            .bearer_auth(self.bearer.as_str())
            .header(ACCEPT, "application/json")
            .json(body)
            .send()
            .map_err(classify_request_error)?;
        if !response.status().is_success() {
            return Err(anyhow!(classify_status(response.status().as_u16())));
        }
        let text = response
            .text()
            .map_err(|_| anyhow!("class=invalid_response"))?;
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|_| anyhow!("class=invalid_response"))
    }

    pub(super) fn delete(&self, path: &str, timeout: Duration) -> anyhow::Result<Value> {
        let response = self
            .client
            .delete(format!("{}{}", self.base_url, path))
            .timeout(timeout)
            .bearer_auth(self.bearer.as_str())
            .header(ACCEPT, "application/json")
            .send()
            .map_err(classify_request_error)?;
        if !response.status().is_success() {
            return Err(anyhow!(classify_status(response.status().as_u16())));
        }
        let text = response
            .text()
            .map_err(|_| anyhow!("class=invalid_response"))?;
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|_| anyhow!("class=invalid_response"))
    }
}

pub(super) fn set_oauth_account_priority(
    router_root: &Path,
    account_id: i64,
    priority: i32,
) -> anyhow::Result<i32> {
    let config_path = crate::user_data::config_path(router_root);
    let config = RouterConfig::load(&config_path).context("class=configuration")?;
    let admin = retry_admin_read(|| AdminClient::connect(router_root, &config))?;
    set_oauth_account_priority_with_admin(&admin, account_id, priority)
}

fn set_oauth_account_priority_with_admin(
    admin: &AdminClient,
    account_id: i64,
    priority: i32,
) -> anyhow::Result<i32> {
    let path = format!("/api/v1/admin/accounts/{account_id}");
    let detail = data(retry_account_read(|| {
        admin.get(&path, Duration::from_secs(10))
    })?);
    if string(&detail, "type") != "oauth" {
        bail!("class=configuration")
    }
    retry_account_read(|| {
        admin.put(
            &path,
            &json!({
                "priority": priority,
                "confirm_mixed_channel_risk": true,
            }),
            Duration::from_secs(10),
        )
    })?;
    let updated = data(retry_account_read(|| {
        admin.get(&path, Duration::from_secs(10))
    })?);
    Ok(integer(&updated, "priority")
        .try_into()
        .ok()
        .filter(|saved: &i32| (1..=999).contains(saved))
        .unwrap_or(priority))
}

pub(super) fn query_usage(
    router_root: &Path,
    profile_name: &str,
    cfg: &RouterConfig,
    deadline: Instant,
) -> anyhow::Result<UsageSnapshot> {
    let admin = retry_admin_read(|| AdminClient::connect(router_root, cfg))?;
    let groups = data(retry_admin_read(|| {
        admin.get(
            "/api/v1/admin/groups/all?include_inactive=true",
            remaining(deadline, Duration::from_secs(10))?,
        )
    })?);
    let group_id = array(&groups)
        .iter()
        .find(|group| string(group, "name") == "Codex-Router")
        .map(|group| integer(group, "id"))
        .unwrap_or_default();
    let accounts_body = data(retry_admin_read(|| {
        admin.get(
            "/api/v1/admin/accounts?page=1&page_size=200",
            remaining(deadline, Duration::from_secs(10))?,
        )
    })?);
    let accounts = get(&accounts_body, "items").unwrap_or(&accounts_body);

    let mut oauth_ids = cfg.oauth_account_ids.clone().unwrap_or_default();
    let oauth_fallback_ids = oauth_accounts_with_api_fallback(cfg);
    let mut api_channels: HashMap<String, ModelConfig> = HashMap::new();
    for model in &cfg.models {
        if model.source == "oauth" && model.oauth_account_id > 0 {
            if !oauth_ids.contains(&model.oauth_account_id) {
                oauth_ids.push(model.oauth_account_id);
            }
        } else {
            let alias = if model.alias.trim().is_empty() {
                model.model.trim()
            } else {
                model.alias.trim()
            };
            if !alias.is_empty() {
                api_channels.insert(format!("Codex-Router / {alias}"), model.clone());
            }
        }
    }

    let mut tasks = Vec::new();
    let mut queried_api_pools = HashSet::new();
    for account in array(accounts) {
        let id = integer(account, "id");
        let kind = string(account, "type");
        let name = string(account, "name");
        let selected_oauth = kind == "oauth" && oauth_ids.contains(&id);
        let selected_api = kind == "apikey"
            && api_channels.contains_key(&name)
            && (group_id <= 0
                || array(get(account, "group_ids").unwrap_or(&Value::Null))
                    .iter()
                    .any(|value| value.as_i64() == Some(group_id)));
        if selected_oauth || selected_api {
            let channel = api_channels.get(&name).cloned();
            let query_provider_usage = selected_api
                && channel
                    .as_ref()
                    .is_some_and(|channel| queried_api_pools.insert(api_quota_pool_key(channel)));
            tasks.push(AccountTask {
                account: account.clone(),
                channel,
                query_provider_usage,
                auto_isolate_on_exhaustion: selected_oauth && oauth_fallback_ids.contains(&id),
            });
        }
    }

    let cache_path = usage_cache_path(router_root);
    let cache = Arc::new(Mutex::new(read_usage_cache(&cache_path)));
    let mut records = query_accounts_bounded(tasks, admin.clone(), cache.clone(), deadline);
    let routing_changed = reconcile_oauth_recovery_observations(
        router_root,
        &admin,
        group_id,
        &mut records,
        deadline,
    );
    let mut subscriptions = Vec::new();
    let mut api_records = Vec::new();
    for record in records {
        if record.account.kind == "oauth" {
            subscriptions.push(record.account);
        } else {
            api_records.push(record);
        }
    }
    let api_channels = merge_api_quota_pools(api_records, &api_channels)
        .into_iter()
        .map(|record| record.account)
        .collect::<Vec<_>>();
    if let Ok(cache) = cache.lock() {
        let _ = write_usage_cache(&cache_path, &cache);
    }
    let all = subscriptions.iter().chain(api_channels.iter());
    let (mut total_tokens, mut total_requests, mut total_cost) = (0_i64, 0_i64, 0.0_f64);
    for account in all {
        total_tokens += account.totals.total_tokens;
        total_requests += account.totals.requests;
        total_cost += account.totals.cost;
    }
    Ok(UsageSnapshot {
        profile_name: profile_name.to_owned(),
        queried_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        total_tokens,
        total_requests,
        total_cost,
        subscriptions,
        api_channels,
        routing_changed,
    })
}

fn query_accounts_bounded(
    tasks: Vec<AccountTask>,
    admin: AdminClient,
    cache: Arc<Mutex<UsageCache>>,
    deadline: Instant,
) -> Vec<AccountRecord> {
    if tasks.is_empty() {
        return Vec::new();
    }
    let queue = Arc::new(Mutex::new(VecDeque::from(tasks)));
    let worker_count = MAX_CONCURRENCY.min(queue.lock().map(|q| q.len()).unwrap_or(1));
    let (sender, receiver) = mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = queue.clone();
            let sender = sender.clone();
            let admin = admin.clone();
            let cache = cache.clone();
            scope.spawn(move || loop {
                let task = queue.lock().ok().and_then(|mut queue| queue.pop_front());
                let Some(task) = task else { break };
                let fallback = task.clone();
                let record = query_account(&admin, task, &cache, deadline).unwrap_or_else(|_| {
                    failed_account_record(&fallback, deadline <= Instant::now())
                });
                let _ = sender.send(record);
            });
        }
    });
    drop(sender);
    receiver.into_iter().collect()
}

fn query_account(
    admin: &AdminClient,
    task: AccountTask,
    cache: &Arc<Mutex<UsageCache>>,
    deadline: Instant,
) -> anyhow::Result<AccountRecord> {
    let id = integer(&task.account, "id");
    let kind = string(&task.account, "type");
    let platform = string(&task.account, "platform");
    let mut query_note = String::new();
    let stats = match retry_account_read(|| {
        admin.get(
            &format!("/api/v1/admin/accounts/{id}/stats"),
            remaining(deadline, Duration::from_secs(10))?,
        )
    }) {
        Ok(value) => data(value),
        Err(error) => {
            query_note = safe_error(&error);
            Value::Null
        }
    };
    let mut usage = Value::Null;
    let mut grok_quota = Value::Null;
    if kind == "oauth" {
        let path = if platform == "grok" {
            format!("/api/v1/admin/grok/accounts/{id}/quota")
        } else {
            format!("/api/v1/admin/accounts/{id}/usage?force=true")
        };
        let timeout = if platform == "grok" { 30 } else { 10 };
        match retry_account_read(|| {
            admin.get(&path, remaining(deadline, Duration::from_secs(timeout))?)
        }) {
            Ok(value) => {
                usage = data(value);
                if platform == "grok" {
                    grok_quota = usage.clone();
                }
            }
            Err(error) if query_note.is_empty() => {
                query_note = if platform == "grok" {
                    "Grok billing quota is unavailable; showing local usage statistics.".to_owned()
                } else {
                    safe_error(&error)
                };
            }
            Err(_) => {}
        }
    }

    let mut provider_usage = None;
    if kind == "apikey" && task.query_provider_usage {
        if let Some(channel) = task.channel.as_ref() {
            provider_usage = Some(query_provider(channel, cache));
        }
    }

    let mut windows = Vec::new();
    add_standard_window(&mut windows, "fiveHour", get(&usage, "five_hour"), "");
    add_standard_window(&mut windows, "weekly", get(&usage, "seven_day"), "");
    add_standard_window(&mut windows, "monthly", get(&usage, "monthly"), "");
    let mut fresh = kind == "oauth" && !usage.is_null() && !windows.is_empty();
    if kind == "oauth" && platform == "openai" {
        if let Ok(quota) = retry_account_read(|| {
            admin.get(
                &format!("/api/v1/admin/openai/accounts/{id}/quota"),
                remaining(deadline, Duration::from_secs(10))?,
            )
        }) {
            let quota = data(quota);
            if let Some(rate_limit) = get(&quota, "rate_limit") {
                for name in ["primary_window", "secondary_window"] {
                    if let Some(window) = get(rate_limit, name) {
                        let seconds = integer(window, "limit_window_seconds");
                        let kind = window_kind(seconds, "other");
                        if let Some(candidate) = standard_window(&kind, window, "") {
                            windows.retain(|item| item.kind != kind);
                            windows.push(candidate);
                            fresh = true;
                        }
                    }
                }
            }
        } else if query_note.is_empty() {
            query_note = "OpenAI quota refresh unavailable; showing cached usage.".to_owned();
        }
    }
    if kind == "oauth" && platform == "grok" {
        let parsed = normalize_grok(if grok_quota.is_null() {
            &usage
        } else {
            &grok_quota
        });
        fresh |= !grok_quota.is_null() && !parsed.is_empty();
        windows.extend(parsed);
    }
    if kind == "oauth" && platform == "antigravity" {
        let parsed = normalize_antigravity(&usage);
        fresh |= !parsed.is_empty();
        windows.extend(parsed);
        if windows.is_empty() {
            let error_code = string(&usage, "error_code");
            let error_class = match error_code.as_str() {
                "unauthenticated" => "class=authentication",
                "forbidden" => "class=permission",
                "rate_limited" => "class=rate_limit",
                "network_error" => "class=network",
                _ if get(&usage, "needs_reauth").and_then(Value::as_bool) == Some(true) => {
                    "class=authentication"
                }
                _ => "",
            };
            append_note(&mut query_note, error_class);
        }
    }
    if let Some(provider) = provider_usage {
        debug_assert!(!provider.provider.is_empty() || provider.windows.is_empty());
        fresh |= !provider.cached && !provider.windows.is_empty();
        windows.extend(provider.windows);
        append_note(&mut query_note, &provider.note);
    }

    if kind == "oauth" {
        let key = cache_key(&format!("oauth-{platform}"), &format!("account:{id}"), "");
        if !windows.is_empty() {
            save_cache(cache, &key, &mut windows, !query_note.is_empty(), "");
        } else if let Some(fallback) = cached_usage(
            cache,
            &key,
            &platform,
            &format!("{platform} live quota query failed; showing cached quota."),
        ) {
            windows = fallback.windows;
            append_note(&mut query_note, &fallback.note);
        }
    }

    if fresh && !credential_rejected(&query_note) {
        query_note.clear();
    }
    let was_unschedulable = string(&task.account, "status") == "error"
        || get(&task.account, "schedulable").and_then(Value::as_bool) == Some(false);
    let recovered =
        maybe_recover_misdisabled_account(admin, &task, &windows, fresh, &query_note, deadline);
    let isolated =
        maybe_isolate_exhausted_oauth_account(admin, &task, &windows, fresh, &query_note, deadline);

    let mut status = string(&task.account, "status");
    let mut schedulable = get(&task.account, "schedulable").and_then(Value::as_bool);
    let mut detail = ["temp_unschedulable_reason", "error_message"]
        .into_iter()
        .map(|name| string(&task.account, name))
        .find(|value| !value.is_empty())
        .unwrap_or_default();
    if recovered {
        status = "active".to_owned();
        schedulable = Some(true);
        detail.clear();
    } else if isolated {
        schedulable = Some(false);
        detail = "OAuth quota exhausted; matching API fallback is active.".to_owned();
    }
    let (health, status_detail) = resolve_state(&status, schedulable, &detail, &windows, fresh);
    let channel = task.channel.as_ref();
    let quota_evidence = if kind == "oauth" && fresh && !credential_rejected(&query_note) {
        if live_quota_is_exhausted(&windows) {
            OAuthQuotaEvidence::Exhausted
        } else if live_quota_is_usable(&windows) {
            OAuthQuotaEvidence::Usable
        } else {
            OAuthQuotaEvidence::Unknown
        }
    } else {
        OAuthQuotaEvidence::Unknown
    };
    Ok(AccountRecord {
        account: UsageAccount {
            id,
            name: string(&task.account, "name"),
            kind,
            platform,
            status,
            health,
            status_detail,
            query_note,
            last_used_at: string(&task.account, "last_used_at"),
            updated_at: string(&usage, "updated_at"),
            totals: normalize_stats(&stats),
            windows,
        },
        configured_model: channel.map(|item| item.model.clone()).unwrap_or_default(),
        quota_evidence,
        auto_isolate_on_exhaustion: task.auto_isolate_on_exhaustion,
        routing_changed: recovered || (isolated && !was_unschedulable),
    })
}

fn failed_account_record(task: &AccountTask, timed_out: bool) -> AccountRecord {
    AccountRecord {
        account: UsageAccount {
            id: integer(&task.account, "id"),
            name: string(&task.account, "name"),
            kind: string(&task.account, "type"),
            platform: string(&task.account, "platform"),
            status: string(&task.account, "status"),
            health: "upstreamError".to_owned(),
            query_note: if timed_out {
                "Usage query timed out; local token statistics are shown.".to_owned()
            } else {
                "Usage query failed; local token statistics are shown.".to_owned()
            },
            last_used_at: string(&task.account, "last_used_at"),
            ..UsageAccount::default()
        },
        configured_model: task
            .channel
            .as_ref()
            .map(|item| item.model.clone())
            .unwrap_or_default(),
        quota_evidence: OAuthQuotaEvidence::Unknown,
        auto_isolate_on_exhaustion: task.auto_isolate_on_exhaustion,
        routing_changed: false,
    }
}

fn normalize_antigravity(usage: &Value) -> Vec<UsageWindow> {
    let quota_map = get(usage, "antigravity_quota").unwrap_or(&Value::Null);
    let detail_map = get(usage, "antigravity_quota_details").unwrap_or(&Value::Null);
    let mut live_models = quota_map
        .as_object()
        .map(|models| models.keys().map(String::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    live_models.sort_unstable();
    let mut grouped: Vec<(AntigravityQuotaSignature, UsageWindow, Vec<String>)> = Vec::new();
    for model in live_models {
        if !is_public_antigravity_model(model) {
            continue;
        }
        if let Some(window) = get(quota_map, model) {
            let display = get(detail_map, model)
                .map(|detail| string(detail, "display_name"))
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| model.to_owned());
            if let Some(parsed) = standard_window("model", window, &display) {
                let signature = AntigravityQuotaSignature::from_window(&parsed);
                if let Some((_, _, models)) = grouped
                    .iter_mut()
                    .find(|(candidate, _, _)| candidate == &signature)
                {
                    models.push(model.to_owned());
                } else {
                    grouped.push((signature, parsed, vec![model.to_owned()]));
                }
            }
        }
    }
    grouped
        .into_iter()
        .filter(|(signature, _, _)| {
            ["five_hour", "seven_day", "monthly"]
                .into_iter()
                .filter_map(|name| get(usage, name))
                .filter_map(|window| standard_window("existing", window, ""))
                .all(|window| AntigravityQuotaSignature::from_window(&window) != *signature)
        })
        .map(|(_, mut window, models)| {
            if models.len() > 1 {
                window.kind = "sharedPool".to_owned();
                window.display_name = antigravity_shared_pool_label(&models).to_owned();
            }
            window
        })
        .collect()
}

fn is_public_antigravity_model(model: &str) -> bool {
    ["gemini-", "claude-", "gpt-"]
        .into_iter()
        .any(|prefix| model.starts_with(prefix))
}

fn antigravity_shared_pool_label(models: &[String]) -> &'static str {
    let all_gemini = models.iter().all(|model| model.starts_with("gemini-"));
    let all_claude = models.iter().all(|model| model.starts_with("claude-"));
    let all_gpt = models.iter().all(|model| model.starts_with("gpt-"));
    if all_gemini {
        "Gemini shared quota"
    } else if all_claude {
        "Claude shared quota"
    } else if all_gpt {
        "GPT shared quota"
    } else if models
        .iter()
        .all(|model| model.starts_with("claude-") || model.starts_with("gpt-"))
    {
        "Claude / GPT shared quota"
    } else {
        "Antigravity shared quota"
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AntigravityQuotaSignature {
    used_percent_milli: Option<i64>,
    reset_at: String,
    requests: i64,
    tokens: i64,
    remaining_amount_micros: Option<i64>,
    limit_amount_micros: Option<i64>,
    used_amount_micros: Option<i64>,
    currency: String,
}

impl AntigravityQuotaSignature {
    fn from_window(window: &UsageWindow) -> Self {
        let fixed = |value: Option<f64>, scale: f64| {
            value
                .filter(|value| value.is_finite())
                .map(|value| (value * scale).round() as i64)
        };
        Self {
            used_percent_milli: fixed(window.used_percent.map(f64::from), 1_000.0),
            reset_at: window.reset_at.trim().to_owned(),
            requests: window.requests,
            tokens: window.tokens,
            remaining_amount_micros: fixed(window.remaining_amount, 1_000_000.0),
            limit_amount_micros: fixed(window.limit_amount, 1_000_000.0),
            used_amount_micros: fixed(window.used_amount, 1_000_000.0),
            currency: window.currency.trim().to_ascii_uppercase(),
        }
    }
}

fn query_provider(channel: &ModelConfig, cache: &Arc<Mutex<UsageCache>>) -> ProviderUsage {
    if let Some(endpoint) = coding_plan_endpoint(&channel.base_url) {
        return query_coding_plan(channel, &endpoint, cache);
    }
    query_api_provider(channel, cache)
}

#[derive(Clone, Debug)]
struct CodingEndpoint {
    provider: &'static str,
    urls: Vec<String>,
}

fn coding_plan_endpoint(base_url: &str) -> Option<CodingEndpoint> {
    let url = Url::parse(base_url.trim_end_matches('/')).ok()?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let path = url.path().trim_end_matches('/').to_ascii_lowercase();
    match host.as_str() {
        "api.kimi.com" if path == "/coding" || path.starts_with("/coding/") => {
            Some(CodingEndpoint {
                provider: "Kimi Coding Plan",
                urls: vec!["https://api.kimi.com/coding/v1/usages".to_owned()],
            })
        }
        "open.bigmodel.cn" | "bigmodel.cn"
            if path == "/api/monitor" || path.starts_with("/api/monitor/") =>
        {
            Some(CodingEndpoint {
                provider: "Zhipu GLM Coding Plan",
                urls: vec!["https://open.bigmodel.cn/api/monitor/usage/quota/limit".to_owned()],
            })
        }
        "api.z.ai" if path == "/api/monitor" || path.starts_with("/api/monitor/") => {
            Some(CodingEndpoint {
                provider: "Zhipu GLM Coding Plan",
                urls: vec!["https://api.z.ai/api/monitor/usage/quota/limit".to_owned()],
            })
        }
        "api.minimaxi.com" | "api.minimax.io"
            if path == "/v1/api/openplatform/coding_plan"
                || path.starts_with("/v1/api/openplatform/coding_plan/") =>
        {
            Some(CodingEndpoint {
                provider: "MiniMax Coding Plan",
                urls: vec![
                    "https://api.minimax.io/v1/token_plan/remains".to_owned(),
                    "https://api.minimax.io/v1/api/openplatform/coding_plan/remains".to_owned(),
                    "https://api.minimaxi.com/v1/token_plan/remains".to_owned(),
                    "https://api.minimaxi.com/v1/api/openplatform/coding_plan/remains".to_owned(),
                ],
            })
        }
        "api.zenmux.ai" | "zenmux.ai" if zenmux_path_allowed(&path) => Some(CodingEndpoint {
            provider: "ZenMux Coding Plan",
            urls: vec![url.as_str().trim_end_matches('/').to_owned()],
        }),
        "ark.cn-beijing.volces.com"
            if path == "/api/coding"
                || path.starts_with("/api/coding/")
                || path == "/api/plan"
                || path.starts_with("/api/plan/") =>
        {
            Some(CodingEndpoint {
                provider: "Volcengine Coding Plan",
                urls: Vec::new(),
            })
        }
        _ => None,
    }
}

fn zenmux_path_allowed(path: &str) -> bool {
    let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
    parts.first() == Some(&"api")
        && (parts
            .get(1)
            .is_some_and(|part| *part == "usage" || *part == "quota")
            || (parts.get(1).is_some_and(|part| {
                part.starts_with('v') && part[1..].chars().all(|c| c.is_ascii_digit())
            }) && parts
                .get(2)
                .is_none_or(|part| *part == "usage" || *part == "quota")))
}

fn query_coding_plan(
    channel: &ModelConfig,
    endpoint: &CodingEndpoint,
    cache: &Arc<Mutex<UsageCache>>,
) -> ProviderUsage {
    let base_url = channel_base_url(channel);
    let key = cache_key(endpoint.provider, &base_url, &channel.credential_name);
    if endpoint.provider == "Volcengine Coding Plan" {
        return query_volcengine(&key, cache);
    }
    let Some(secret) = credential_string(&channel.credential_name) else {
        return ProviderUsage {
            provider: endpoint.provider.to_owned(),
            note: format!("{} credential is unavailable.", endpoint.provider),
            ..ProviderUsage::default()
        };
    };
    let client = provider_client();
    let mut last_status = 0;
    for url in &endpoint.urls {
        match send_json_with_retry(
            client
                .get(url)
                .bearer_auth(secret.as_str())
                .header(ACCEPT, "application/json"),
        ) {
            Ok(body) => {
                let subscription = if endpoint.provider == "Zhipu GLM Coding Plan" {
                    let subscription_url = if base_url.contains("open.bigmodel.cn")
                        || base_url.contains("bigmodel.cn")
                    {
                        "https://open.bigmodel.cn/api/biz/subscription/list"
                    } else {
                        "https://api.z.ai/api/biz/subscription/list"
                    };
                    send_json_with_retry(
                        client
                            .get(subscription_url)
                            .bearer_auth(secret.as_str())
                            .header(ACCEPT, "application/json"),
                    )
                    .ok()
                } else {
                    None
                };
                let mut windows = match endpoint.provider {
                    "Kimi Coding Plan" => normalize_kimi(&body),
                    "Zhipu GLM Coding Plan" => normalize_zhipu(&body, subscription.as_ref()),
                    "MiniMax Coding Plan" => normalize_minimax(&body),
                    "ZenMux Coding Plan" => normalize_zenmux(&body),
                    _ => Vec::new(),
                };
                if !windows.is_empty() {
                    save_cache(cache, &key, &mut windows, false, "");
                    return ProviderUsage {
                        provider: endpoint.provider.to_owned(),
                        windows,
                        ..ProviderUsage::default()
                    };
                }
            }
            Err(status) => last_status = status,
        }
    }
    let status = status_label(last_status);
    let note = if endpoint.provider == "Kimi Coding Plan" && matches!(last_status, 401 | 403) {
        format!("ROUTER_KIMI_CREDENTIAL_REJECTED: Kimi Coding Plan API Key is invalid or lacks Coding Plan access ({status}).")
    } else {
        format!("{} live quota query failed ({status}).", endpoint.provider)
    };
    cached_usage(
        cache,
        &key,
        endpoint.provider,
        &format!("{note} Showing cached quota."),
    )
    .unwrap_or(ProviderUsage {
        provider: endpoint.provider.to_owned(),
        note,
        ..ProviderUsage::default()
    })
}

fn query_volcengine(key: &str, cache: &Arc<Mutex<UsageCache>>) -> ProviderUsage {
    let Some(access_key) = credential_string("VolcengineAccessKeyId") else {
        return ProviderUsage { provider: "Volcengine Coding Plan".to_owned(), note: "Add the Volcengine control-plane AK/SK in this model to query official 5-hour, weekly, and monthly quota.".to_owned(), ..ProviderUsage::default() };
    };
    let Some(secret_key) = credential_string("VolcengineSecretAccessKey") else {
        return ProviderUsage { provider: "Volcengine Coding Plan".to_owned(), note: "Add the Volcengine control-plane AK/SK in this model to query official 5-hour, weekly, and monthly quota.".to_owned(), ..ProviderUsage::default() };
    };
    let headers = volcengine_headers(access_key.as_str(), secret_key.as_str(), Utc::now());
    let request = provider_client()
        .post("https://open.volcengineapi.com/?Action=GetCodingPlanUsage&Version=2024-01-01")
        .headers(headers)
        .body("");
    match send_json_with_retry(request) {
        Ok(body) => {
            let mut windows = normalize_volcengine(&body);
            if windows.is_empty() {
                return cached_usage(
                    cache,
                    key,
                    "Volcengine Coding Plan",
                    "Volcengine Coding Plan query failed (invalid response); showing cached quota.",
                )
                .unwrap_or(ProviderUsage {
                    provider: "Volcengine Coding Plan".to_owned(),
                    note: "Volcengine Coding Plan quota query failed (invalid response)."
                        .to_owned(),
                    ..ProviderUsage::default()
                });
            }
            save_cache(cache, key, &mut windows, false, "");
            ProviderUsage {
                provider: "Volcengine Coding Plan".to_owned(),
                windows,
                ..ProviderUsage::default()
            }
        }
        Err(status) => {
            let label = status_label(status);
            cached_usage(
                cache,
                key,
                "Volcengine Coding Plan",
                &format!("Volcengine Coding Plan query failed ({label}); showing cached quota."),
            )
            .unwrap_or(ProviderUsage {
                provider: "Volcengine Coding Plan".to_owned(),
                note: format!("Volcengine Coding Plan quota query failed ({label})."),
                ..ProviderUsage::default()
            })
        }
    }
}

fn query_api_provider(channel: &ModelConfig, cache: &Arc<Mutex<UsageCache>>) -> ProviderUsage {
    let provider = provider_from_channel(&channel.base_url);
    let base_url = channel_base_url(channel);
    let key = cache_key(&provider, &base_url, &channel.credential_name);
    let Some(secret) = credential_string(&channel.credential_name) else {
        return ProviderUsage {
            provider: provider.clone(),
            note: format!("{provider} credential is unavailable."),
            ..ProviderUsage::default()
        };
    };
    let client = provider_client();
    let result: Result<(Vec<UsageWindow>, String, bool), i32> = match provider.as_str() {
        "openrouter" => {
            let headers = |request: RequestBuilder| request.bearer_auth(secret.as_str()).header(ACCEPT, "application/json").header("HTTP-Referer", "https://github.com/Javis603/token-monitor").header("X-OpenRouter-Title", "Token Monitor");
            let key_body = send_json_with_retry(headers(client.get("https://openrouter.ai/api/v1/key"))).ok();
            let credits_body = send_json_with_retry(headers(client.get("https://openrouter.ai/api/v1/credits"))).ok();
            let windows = normalize_openrouter(key_body.as_ref(), credits_body.as_ref());
            if windows.is_empty() { Err(0) } else { Ok((windows, "OpenRouter official key and credit usage.".to_owned(), key_body.is_none() || credits_body.is_none())) }
        }
        "deepseek" => send_json_with_retry(client.get("https://api.deepseek.com/user/balance").bearer_auth(secret.as_str()).header(ACCEPT, "application/json"))
            .and_then(|body| normalize_deepseek(&body).map(|window| (vec![window], "DeepSeek official balance API.".to_owned(), false)).ok_or(0)),
        "mimo" => {
            if !secret.to_ascii_lowercase().contains("api-platform_servicetoken=") || !secret.to_ascii_lowercase().contains("userid=") {
                return ProviderUsage { provider: "MiMo".to_owned(), note: "MiMo official Token Plan usage requires a browser Cookie containing api-platform_serviceToken and userId; local token statistics are shown.".to_owned(), ..ProviderUsage::default() };
            }
            let headers = |request: RequestBuilder| request.header(ACCEPT, "application/json, text/plain, */*").header("Cookie", secret.as_str()).header("Origin", "https://platform.xiaomimimo.com").header("Referer", "https://platform.xiaomimimo.com/#/console/balance").header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131 Safari/537.36");
            match send_json_with_retry(headers(client.get("https://platform.xiaomimimo.com/api/v1/balance"))) {
                Ok(balance) => {
                    let detail = send_json_with_retry(headers(client.get("https://platform.xiaomimimo.com/api/v1/tokenPlan/detail"))).ok();
                    let usage = send_json_with_retry(headers(client.get("https://platform.xiaomimimo.com/api/v1/tokenPlan/usage"))).ok();
                    let windows = normalize_mimo(Some(&balance), detail.as_ref(), usage.as_ref());
                    if windows.is_empty() { Err(0) } else { Ok((windows, "MiMo official balance and Token Plan usage.".to_owned(), true)) }
                }
                Err(status) => Err(status),
            }
        }
        _ => return ProviderUsage { provider: provider.clone(), note: format!("{provider} does not expose a reliable official time-window usage endpoint with the configured credential; local token statistics are shown."), ..ProviderUsage::default() },
    };
    match result {
        Ok((mut windows, note, merge)) => {
            save_cache(cache, &key, &mut windows, merge, &note);
            ProviderUsage {
                provider,
                windows,
                note,
                cached: false,
            }
        }
        Err(status) => cached_usage(
            cache,
            &key,
            &provider,
            &format!("{provider} live usage query failed; showing cached usage."),
        )
        .unwrap_or(ProviderUsage {
            provider: provider.clone(),
            note: format!(
                "{provider} usage query failed ({}); local token statistics are shown.",
                status_label(status)
            ),
            ..ProviderUsage::default()
        }),
    }
}

fn provider_client() -> Client {
    Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(REQUEST_TIMEOUT)
        .build()
        .expect("provider HTTP client")
}

fn send_json_with_retry(builder: RequestBuilder) -> Result<Value, i32> {
    let retry = builder.try_clone();
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(350));
        }
        let Some(request) = (if attempt == 0 {
            builder.try_clone()
        } else {
            retry.as_ref().and_then(RequestBuilder::try_clone)
        }) else {
            return Err(0);
        };
        match request.send() {
            Ok(response) if response.status().is_success() => {
                return response.json().map_err(|_| 0)
            }
            Ok(response) => {
                let status = response.status().as_u16() as i32;
                if attempt == 1 || !retryable_status(status) {
                    return Err(status);
                }
            }
            Err(_) if attempt == 1 => return Err(0),
            Err(_) => {}
        }
    }
    Err(0)
}

fn retryable_status(status: i32) -> bool {
    status == 0 || matches!(status, 408 | 425 | 429) || status >= 500
}

fn normalize_kimi(body: &Value) -> Vec<UsageWindow> {
    let mut body = data_ref(body);
    body = data_ref(body);
    if get(body, "error").is_some()
        || get(body, "code")
            .and_then(number)
            .is_some_and(|code| code != 0.0)
    {
        return Vec::new();
    }
    let mut classified: BTreeMap<String, UsageWindow> = BTreeMap::new();
    let mut unclassified = Vec::new();
    if let Some(usage) = get(body, "usage") {
        let detail = first(usage, &["detail", "quota"]).unwrap_or(usage);
        if let Some(percent) = normalized_percent(detail) {
            let kind = kimi_kind(usage).unwrap_or("weekly");
            classified.insert(
                kind.to_owned(),
                coding_window(kind, percent, reset_value(detail), ""),
            );
        }
    }
    let limits = first(
        body,
        &[
            "limits",
            "limitInfos",
            "limit_infos",
            "rateLimits",
            "rate_limits",
            "windows",
        ],
    );
    for item in array(limits.unwrap_or(&Value::Null)) {
        let detail = first(item, &["detail", "usage", "quota"]).unwrap_or(item);
        let Some(percent) = normalized_percent(detail) else {
            continue;
        };
        let window = first(
            item,
            &[
                "window",
                "period",
                "rateLimit",
                "rate_limit",
                "timeWindow",
                "time_window",
            ],
        )
        .unwrap_or(item);
        let kind = kimi_kind(window);
        let reset = reset_value(detail).or_else(|| reset_value(window));
        let parsed = coding_window(kind.unwrap_or("other"), percent, reset, "");
        if let Some(kind) = kind {
            classified.entry(kind.to_owned()).or_insert(parsed);
        } else {
            unclassified.push(parsed);
        }
    }
    for mut window in unclassified {
        let kind = if !classified.contains_key("fiveHour") {
            "fiveHour"
        } else if !classified.contains_key("weekly") {
            "weekly"
        } else {
            break;
        };
        window.kind = kind.to_owned();
        classified.insert(kind.to_owned(), window);
    }
    ["fiveHour", "weekly"]
        .into_iter()
        .filter_map(|kind| classified.remove(kind))
        .collect()
}

fn normalize_zhipu(body: &Value, subscription: Option<&Value>) -> Vec<UsageWindow> {
    let data = get(body, "data").unwrap_or(&Value::Null);
    let limits = array(get(data, "limits").unwrap_or(&Value::Null));
    let mut token_limits = limits
        .iter()
        .filter(|item| string(item, "type").eq_ignore_ascii_case("TOKENS_LIMIT"))
        .filter_map(|item| {
            zhipu_percent(item)
                .map(|percent| (item, percent, zhipu_minutes(item).unwrap_or(f64::MAX)))
        })
        .collect::<Vec<_>>();
    token_limits.sort_by(|left, right| left.2.total_cmp(&right.2));
    let mut result = Vec::new();
    if token_limits.len() >= 2 {
        let shortest = token_limits[0];
        let last = token_limits[token_limits.len() - 1];
        result.push(coding_window(
            "fiveHour",
            shortest.1,
            first(shortest.0, &["nextResetTime", "next_reset_time"]),
            "",
        ));
        result.push(coding_window(
            "weekly",
            last.1,
            first(last.0, &["nextResetTime", "next_reset_time"]),
            "",
        ));
    } else if let Some(item) = token_limits.first() {
        let kind = if item.2 <= 360.0 {
            "fiveHour"
        } else {
            "weekly"
        };
        result.push(coding_window(
            kind,
            item.1,
            first(item.0, &["nextResetTime", "next_reset_time"]),
            "",
        ));
    }
    if let Some(item) = limits.iter().find(|item| {
        string(item, "type").eq_ignore_ascii_case("TIME_LIMIT") && zhipu_percent(item).is_some()
    }) {
        let reset = first(item, &["nextResetTime", "next_reset_time"]).or_else(|| {
            subscription
                .and_then(|body| array(get(body, "data").unwrap_or(&Value::Null)).first())
                .and_then(|item| first(item, &["nextRenewTime", "next_renew_time"]))
        });
        result.push(coding_window(
            "monthly",
            zhipu_percent(item).unwrap_or_default(),
            reset,
            "Z.ai MCP monthly quota",
        ));
    }
    result
}

fn normalize_minimax(body: &Value) -> Vec<UsageWindow> {
    if get(body, "base_resp")
        .and_then(|base| first(base, &["status_code", "statusCode", "code"]))
        .and_then(number)
        .is_some_and(|code| code != 0.0)
    {
        return Vec::new();
    }
    let rows = get(body, "model_remains").or_else(|| get_path(body, &["data", "model_remains"]));
    let Some(record) = array(rows.unwrap_or(&Value::Null))
        .iter()
        .find(|item| string(item, "model_name") == "general")
    else {
        return Vec::new();
    };
    let mut windows = Vec::new();
    let remaining = get(record, "current_interval_remaining_percent").and_then(number);
    let placeholder = number(get(record, "current_interval_status").unwrap_or(&Value::Null))
        == Some(3.0)
        && remaining.is_none_or(|value| value >= 100.0);
    if let Some(remaining) = remaining.filter(|_| !placeholder) {
        windows.push(coding_window(
            "fiveHour",
            100.0 - remaining,
            get(record, "end_time"),
            "",
        ));
    }
    let remaining = get(record, "current_weekly_remaining_percent").and_then(number);
    let placeholder = number(get(record, "current_weekly_status").unwrap_or(&Value::Null))
        == Some(3.0)
        && remaining.is_none_or(|value| value >= 100.0);
    if let Some(remaining) = remaining.filter(|_| !placeholder) {
        windows.push(coding_window(
            "weekly",
            100.0 - remaining,
            get(record, "weekly_end_time"),
            "",
        ));
    }
    windows
}

fn normalize_zenmux(body: &Value) -> Vec<UsageWindow> {
    if get(body, "success").and_then(Value::as_bool) != Some(true) {
        return Vec::new();
    }
    let data = get(body, "data").unwrap_or(&Value::Null);
    [("quota_5_hour", "fiveHour"), ("quota_7_day", "weekly")]
        .into_iter()
        .filter_map(|(name, kind)| {
            let quota = get(data, name)?;
            let mut percent = get(quota, "usage_percentage").and_then(number)?;
            if percent <= 1.0 {
                percent *= 100.0;
            }
            Some(coding_window(kind, percent, get(quota, "resets_at"), ""))
        })
        .collect()
}

fn normalize_volcengine(body: &Value) -> Vec<UsageWindow> {
    let result = get(body, "Result").unwrap_or(body);
    let rows = first(result, &["QuotaUsage", "quotaUsage"]);
    array(rows.unwrap_or(&Value::Null))
        .iter()
        .filter_map(|quota| {
            let percent = get(quota, "Percent").and_then(number)?;
            let kind = match string(quota, "Level").trim().to_ascii_lowercase().as_str() {
                "session" | "5h" | "5-hour" | "fivehour" | "five_hour" | "rolling_5h" => "fiveHour",
                "weekly" | "week" | "7d" => "weekly",
                "monthly" | "month" => "monthly",
                _ => return None,
            };
            let display = match kind {
                "fiveHour" => "Volcengine 5-hour quota",
                "weekly" => "Volcengine weekly quota",
                _ => "Volcengine monthly quota",
            };
            Some(coding_window(
                kind,
                percent,
                get(quota, "ResetTimestamp"),
                display,
            ))
        })
        .collect()
}

fn normalize_grok(body: &Value) -> Vec<UsageWindow> {
    if body.is_null() {
        return Vec::new();
    }
    let mut windows = Vec::new();
    let config = get(body, "config").unwrap_or(body);
    let current = first(config, &["currentPeriod", "current_period"]).unwrap_or(&Value::Null);
    let period_type = first(current, &["type", "period_type"])
        .and_then(Value::as_str)
        .unwrap_or("");
    let config_kind = if period_type.to_ascii_lowercase().contains("week") {
        "weekly"
    } else if period_type.to_ascii_lowercase().contains("day") {
        "other"
    } else {
        "monthly"
    };
    let reset = first(
        current,
        &[
            "end",
            "billingPeriodEnd",
            "billing_period_end",
            "resetAt",
            "reset_at",
        ],
    );
    if let Some(percent) =
        first(config, &["creditUsagePercent", "credit_usage_percent"]).and_then(number)
    {
        windows.push(coding_window(config_kind, percent, reset, "Grok credits"));
    }
    let billing = get(body, "billing").unwrap_or(body);
    let subscription_kind = match string(billing, "period_type").as_str() {
        "monthly" => "monthly",
        "daily" => "other",
        _ => "weekly",
    };
    if let Some(percent) = get(billing, "usage_percent").and_then(number) {
        windows.push(coding_window(
            subscription_kind,
            percent,
            get(billing, "period_end"),
            &string(billing, "plan"),
        ));
    }
    if subscription_kind != "monthly" {
        if let Some(percent) = get(billing, "used_percent").and_then(number) {
            let used = integer(billing, "used_cents");
            let limit = integer(billing, "monthly_limit_cents");
            let display = if limit > 0 {
                format!(
                    "Monthly quota ${:.2} / ${:.2}",
                    used as f64 / 100.0,
                    limit as f64 / 100.0
                )
            } else {
                "Monthly quota".to_owned()
            };
            windows.push(coding_window(
                "monthly",
                percent,
                get(billing, "billing_period_end"),
                &display,
            ));
        }
    }
    for product in array(get(billing, "product_usage").unwrap_or(&Value::Null)) {
        if let Some(percent) = get(product, "usage_percent").and_then(number) {
            let name = string(product, "product");
            windows.push(coding_window(
                "model",
                percent,
                get(billing, "period_end"),
                if name.is_empty() { "Grok" } else { &name },
            ));
        }
    }
    let snapshot = get(body, "snapshot").unwrap_or(&Value::Null);
    for (name, display) in [
        ("tokens", "Grok token quota"),
        ("requests", "Grok request quota"),
    ] {
        if let Some(quota) = get(snapshot, name) {
            let limit = number(get(quota, "limit").unwrap_or(&Value::Null)).unwrap_or_default();
            if let (true, Some(remaining)) = (limit > 0.0, get(quota, "remaining").and_then(number))
            {
                windows.push(coding_window(
                    "other",
                    (limit - remaining) / limit * 100.0,
                    first(quota, &["reset_at", "reset_unix"]),
                    display,
                ));
            }
        }
    }
    windows
}

fn normalize_openrouter(
    key_body: Option<&Value>,
    credits_body: Option<&Value>,
) -> Vec<UsageWindow> {
    let mut windows = Vec::new();
    if let Some(data) = key_body.map(data_ref) {
        if let Some(limit) = first(data, &["limit", "total_limit"]).and_then(number) {
            let used = first(data, &["usage", "used", "total_usage"])
                .and_then(number)
                .or_else(|| {
                    first(data, &["limit_remaining", "remaining"])
                        .and_then(number)
                        .map(|remaining| limit - remaining)
                });
            if let Some(used) = used {
                let reset = string(data, "limit_reset").to_ascii_lowercase();
                let kind = match reset.as_str() {
                    "daily" => "daily",
                    "weekly" => "weekly",
                    _ => "monthly",
                };
                let display = match reset.as_str() {
                    "daily" => "OpenRouter daily limit",
                    "weekly" => "OpenRouter weekly limit",
                    "monthly" => "OpenRouter monthly limit",
                    _ => "OpenRouter API key limit",
                };
                let mut window = balance_window(
                    display,
                    (limit - used).max(0.0),
                    "USD",
                    Some(limit),
                    Some(used),
                );
                window.kind = kind.to_owned();
                windows.push(window);
            }
        }
    }
    if let Some(data) = credits_body.map(data_ref) {
        if let (Some(limit), Some(used)) = (
            first(data, &["total_credits", "totalCredits", "limit"]).and_then(number),
            first(data, &["total_usage", "totalUsage", "usage", "used"]).and_then(number),
        ) {
            windows.push(balance_window(
                "OpenRouter credits",
                (limit - used).max(0.0),
                "USD",
                Some(limit),
                Some(used),
            ));
        }
    }
    windows
}

fn normalize_deepseek(body: &Value) -> Option<UsageWindow> {
    let mut rows = array(get(body, "balance_infos").unwrap_or(&Value::Null))
        .iter()
        .filter_map(|row| {
            Some((
                get(row, "total_balance").and_then(number)?,
                string(row, "currency").to_ascii_uppercase(),
            ))
        })
        .filter(|(_, currency)| !currency.is_empty())
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return None;
    }
    rows.sort_by(|left, right| right.0.total_cmp(&left.0));
    let selected = rows
        .iter()
        .find(|row| row.0 > 0.0)
        .or_else(|| rows.iter().find(|row| row.1 == "USD"))
        .unwrap_or(&rows[0]);
    Some(balance_window(
        "DeepSeek balance",
        selected.0,
        &selected.1,
        None,
        None,
    ))
}

fn normalize_mimo(
    balance: Option<&Value>,
    detail: Option<&Value>,
    usage: Option<&Value>,
) -> Vec<UsageWindow> {
    if [balance, detail, usage].into_iter().flatten().any(|body| {
        first(
            body,
            &[
                "code",
                "statusCode",
                "status_code",
                "errorCode",
                "error_code",
            ],
        )
        .and_then(number)
        .is_some_and(|code| code != 0.0)
    }) {
        return Vec::new();
    }
    let mut windows = Vec::new();
    let balance_data = balance.map(data_ref).unwrap_or(&Value::Null);
    if let Some(amount) = first(balance_data, &["balance", "amount", "remaining"]).and_then(number)
    {
        let currency = string(balance_data, "currency");
        windows.push(balance_window(
            "MiMo balance",
            amount,
            if currency.is_empty() {
                "USD"
            } else {
                &currency
            },
            None,
            None,
        ));
    }
    let detail = detail.map(data_ref).unwrap_or(&Value::Null);
    let status = first(
        detail,
        &[
            "planStatus",
            "plan_status",
            "subscriptionStatus",
            "subscription_status",
            "status",
            "state",
        ],
    )
    .and_then(Value::as_str)
    .unwrap_or("")
    .to_ascii_lowercase();
    if status.contains("expired") || status.contains("ended") {
        return windows;
    }
    let usage = usage.map(data_ref).unwrap_or(&Value::Null);
    let month = first(usage, &["monthUsage", "month_usage"]).unwrap_or(usage);
    let items = array(get(month, "items").unwrap_or(&Value::Null));
    let item = if items.is_empty() {
        Some(month)
    } else {
        items
            .iter()
            .find(|item| string(item, "name").eq_ignore_ascii_case("month_total_token"))
    };
    if let Some(item) = item {
        let percent = match (
            first(item, &["limit", "total", "quota"]).and_then(number),
            first(item, &["used", "current", "consumed"]).and_then(number),
        ) {
            (Some(limit), Some(used)) if limit > 0.0 => Some(used / limit * 100.0),
            _ => get(item, "percent")
                .and_then(number)
                .map(|value| if value <= 1.0 { value * 100.0 } else { value })
                .or_else(|| normalized_percent(item)),
        };
        if let Some(percent) = percent {
            windows.push(coding_window(
                "monthly",
                percent,
                first(detail, &["currentPeriodEnd", "current_period_end"]),
                "MiMo Token Plan",
            ));
        }
    }
    windows
}

fn normalize_stats(stats: &Value) -> UsageTotals {
    let summary = get(stats, "summary").unwrap_or(&Value::Null);
    let models = array(get(stats, "models").unwrap_or(&Value::Null))
        .iter()
        .map(|model| UsageModelSummary {
            name: string(model, "model"),
            requests: integer(model, "requests"),
            input_tokens: integer(model, "input_tokens"),
            output_tokens: integer(model, "output_tokens"),
            cache_read_tokens: integer(model, "cache_read_tokens"),
            cache_creation_tokens: integer(model, "cache_creation_tokens"),
            total_tokens: integer(model, "total_tokens"),
            cost: number(get(model, "actual_cost").unwrap_or(&Value::Null)).unwrap_or_default(),
        })
        .collect();
    UsageTotals {
        requests: integer(summary, "total_requests"),
        total_tokens: integer(summary, "total_tokens"),
        cost: number(get(summary, "total_cost").unwrap_or(&Value::Null)).unwrap_or_default(),
        models,
    }
}

fn api_quota_pool_provider(channel: &ModelConfig) -> String {
    coding_plan_endpoint(&channel.base_url)
        .map(|endpoint| endpoint.provider.to_owned())
        .unwrap_or_else(|| provider_from_channel(&channel.base_url))
}

fn credential_pool_digest(names: &[&str]) -> Option<String> {
    let mut digest = Sha256::new();
    let mut found = false;
    for name in names {
        if let Some(secret) = credential_string(name) {
            found = true;
            digest.update((secret.len() as u64).to_le_bytes());
            digest.update(secret.as_bytes());
        }
    }
    found.then(|| format!("{:x}", digest.finalize()))
}

fn api_quota_pool_key(channel: &ModelConfig) -> String {
    let provider = api_quota_pool_provider(channel);
    let endpoint = Url::parse(&channel.base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_else(|| channel_base_url(channel).to_ascii_lowercase());
    let credential = if provider == "Volcengine Coding Plan" {
        credential_pool_digest(&["VolcengineAccessKeyId", "VolcengineSecretAccessKey"])
    } else {
        credential_pool_digest(&[&channel.credential_name])
    }
    .unwrap_or_else(|| {
        format!(
            "credential:{}",
            channel.credential_name.to_ascii_lowercase()
        )
    });
    format!("{}|{endpoint}|{credential}", provider.to_ascii_lowercase())
}

fn merge_api_quota_pools(
    records: Vec<AccountRecord>,
    channels: &HashMap<String, ModelConfig>,
) -> Vec<AccountRecord> {
    let mut groups: BTreeMap<String, (String, Vec<AccountRecord>)> = BTreeMap::new();
    for record in records {
        let (key, label) = channels
            .get(&record.account.name)
            .map(|channel| {
                (
                    api_quota_pool_key(channel),
                    api_quota_pool_provider(channel),
                )
            })
            .unwrap_or_else(|| {
                (
                    format!("account:{}", record.account.id),
                    record.account.platform.clone(),
                )
            });
        groups
            .entry(key)
            .or_insert_with(|| (label, Vec::new()))
            .1
            .push(record);
    }
    let mut merged = Vec::new();
    for (label, mut group) in groups.into_values() {
        if group.len() == 1 {
            merged.extend(group);
            continue;
        }
        group.sort_by_key(|record| record.account.windows.is_empty());
        let mut first = group.remove(0);
        first.account.name = format!("Codex-Router / {label}");
        first.configured_model = label;
        for record in group {
            merge_totals(&mut first.account.totals, &record.account.totals);
            merge_windows(&mut first.account.windows, &record.account.windows);
            append_note(&mut first.account.query_note, &record.account.query_note);
        }
        merged.push(first);
    }
    merged
}

fn merge_totals(target: &mut UsageTotals, incoming: &UsageTotals) {
    target.requests += incoming.requests;
    target.total_tokens += incoming.total_tokens;
    target.cost += incoming.cost;
    for model in &incoming.models {
        if let Some(existing) = target
            .models
            .iter_mut()
            .find(|item| item.name == model.name)
        {
            existing.requests += model.requests;
            existing.input_tokens += model.input_tokens;
            existing.output_tokens += model.output_tokens;
            existing.cache_read_tokens += model.cache_read_tokens;
            existing.cache_creation_tokens += model.cache_creation_tokens;
            existing.total_tokens += model.total_tokens;
            existing.cost += model.cost;
        } else {
            target.models.push(model.clone());
        }
    }
}

fn merge_windows(target: &mut Vec<UsageWindow>, incoming: &[UsageWindow]) {
    let mut seen = target.iter().map(window_cache_key).collect::<HashSet<_>>();
    for window in incoming {
        if seen.insert(window_cache_key(window)) {
            target.push(window.clone());
        }
    }
}

fn resolve_state(
    status: &str,
    schedulable: Option<bool>,
    detail: &str,
    windows: &[UsageWindow],
    fresh: bool,
) -> (String, String) {
    let mut detail = detail.to_owned();
    if fresh || (status == "active" && schedulable != Some(false)) {
        detail.clear();
    }
    let exhausted = windows
        .iter()
        .any(|window| window.used_percent.is_some_and(|value| value >= 99.999));
    let health = if exhausted {
        "quotaExhausted"
    } else if status == "active" && schedulable != Some(false) {
        "healthy"
    } else if detail.to_ascii_lowercase().contains("usage limit")
        || detail.to_ascii_lowercase().contains("quota")
        || detail.to_ascii_lowercase().contains("billing cycle")
    {
        "quotaExhausted"
    } else if !detail.is_empty() || status == "error" {
        "upstreamError"
    } else {
        "cooldown"
    };
    (health.to_owned(), detail)
}

fn standard_window(kind: &str, window: &Value, display: &str) -> Option<UsageWindow> {
    let percent = normalized_percent(window)?;
    let reset = reset_value(window);
    let reset_at = reset.map(reset_at).unwrap_or_default();
    let remaining_seconds = get(window, "remaining_seconds")
        .and_then(number)
        .map(|value| value as i64)
        .unwrap_or_else(|| {
            DateTime::parse_from_rfc3339(&reset_at)
                .ok()
                .map(|date| {
                    (date.with_timezone(&Utc) - Utc::now())
                        .num_seconds()
                        .max(-1)
                })
                .unwrap_or(-1)
        });
    let stats = get(window, "window_stats").unwrap_or(&Value::Null);
    Some(UsageWindow {
        kind: kind.to_owned(),
        display_name: display.to_owned(),
        used_percent: Some(percent as f32),
        reset_at,
        remaining_seconds,
        requests: integer(stats, "requests"),
        tokens: integer(stats, "tokens"),
        ..UsageWindow::default()
    })
}

fn add_standard_window(
    target: &mut Vec<UsageWindow>,
    kind: &str,
    window: Option<&Value>,
    display: &str,
) {
    if let Some(window) = window.and_then(|value| standard_window(kind, value, display)) {
        target.push(window);
    }
}

fn coding_window(kind: &str, percent: f64, reset: Option<&Value>, display: &str) -> UsageWindow {
    UsageWindow {
        kind: kind.to_owned(),
        display_name: display.to_owned(),
        used_percent: Some(percent.clamp(0.0, 100.0) as f32),
        reset_at: reset.map(reset_at).unwrap_or_default(),
        remaining_seconds: -1,
        ..UsageWindow::default()
    }
}

fn balance_window(
    display: &str,
    remaining: f64,
    currency: &str,
    limit: Option<f64>,
    used: Option<f64>,
) -> UsageWindow {
    UsageWindow {
        kind: "balance".to_owned(),
        display_name: display.to_owned(),
        used_percent: limit
            .zip(used)
            .filter(|(limit, _)| *limit > 0.0)
            .map(|(limit, used)| (used / limit * 100.0).clamp(0.0, 100.0) as f32),
        remaining_seconds: -1,
        remaining_amount: Some(remaining.max(0.0)),
        limit_amount: limit,
        used_amount: used,
        currency: currency.to_ascii_uppercase(),
        ..UsageWindow::default()
    }
}

fn normalized_percent(window: &Value) -> Option<f64> {
    for name in [
        "utilization",
        "used_percent",
        "usedPercent",
        "percentage",
        "percent",
        "usage_percentage",
        "usagePercentage",
        "usedRatio",
        "used_ratio",
        "ratio",
        "amountUsedRatio",
        "amount_used_ratio",
        "remaining_percent",
        "remainingPercent",
    ] {
        if let Some(mut value) = get(window, name).and_then(number) {
            if matches!(name, "usage_percentage" | "usagePercentage") && value <= 1.0 {
                value *= 100.0;
            }
            if matches!(
                name,
                "usedRatio" | "used_ratio" | "ratio" | "amountUsedRatio" | "amount_used_ratio"
            ) {
                value *= 100.0;
            }
            if matches!(name, "remaining_percent" | "remainingPercent") {
                value = 100.0 - value;
            }
            return Some(value.clamp(0.0, 100.0));
        }
    }
    let limit = first(
        window,
        &[
            "limit",
            "limitValue",
            "limit_value",
            "total",
            "totalValue",
            "total_value",
            "quota",
            "quotaValue",
            "quota_value",
            "max",
            "maxValue",
            "max_value",
        ],
    )
    .and_then(number)?;
    if limit <= 0.0 {
        return None;
    }
    if let Some(used) = first(
        window,
        &[
            "used",
            "usedValue",
            "used_value",
            "usedAmount",
            "used_amount",
            "consumed",
            "consumedValue",
            "consumed_value",
            "current",
            "currentValue",
            "current_value",
            "total_used",
        ],
    )
    .and_then(number)
    {
        return Some((used / limit * 100.0).clamp(0.0, 100.0));
    }
    first(
        window,
        &[
            "remaining",
            "remainingValue",
            "remaining_value",
            "limit_remaining",
        ],
    )
    .and_then(number)
    .map(|remaining| ((limit - remaining) / limit * 100.0).clamp(0.0, 100.0))
}

fn zhipu_percent(window: &Value) -> Option<f64> {
    if let Some(total) = get(window, "usage")
        .and_then(number)
        .filter(|value| *value > 0.0)
    {
        let remaining = get(window, "remaining")
            .and_then(number)
            .map(|value| total - value);
        let current = first(window, &["currentValue", "current_value"]).and_then(number);
        if let Some(used) = match (remaining, current) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (left, right) => left.or(right),
        } {
            return Some((used.clamp(0.0, total) / total * 100.0).clamp(0.0, 100.0));
        }
    }
    normalized_percent(window)
}

fn zhipu_minutes(window: &Value) -> Option<f64> {
    let unit = get(window, "unit").and_then(number)? as i64;
    let number = get(window, "number").and_then(number);
    match (unit, number) {
        (3, None) => Some(300.0),
        (6, None) => Some(10080.0),
        (5, Some(n)) if n > 0.0 => Some(n),
        (3, Some(n)) if n > 0.0 => Some(n * 60.0),
        (1, Some(n)) if n > 0.0 => Some(n * 24.0 * 60.0),
        (6, Some(n)) if n > 0.0 => Some(n * 7.0 * 24.0 * 60.0),
        _ => None,
    }
}

fn kimi_kind(window: &Value) -> Option<&'static str> {
    if let Some(duration) = first(
        window,
        &[
            "duration",
            "windowDuration",
            "window_duration",
            "size",
            "value",
            "length",
        ],
    )
    .and_then(number)
    .filter(|value| *value > 0.0)
    {
        let unit = first(
            window,
            &["timeUnit", "time_unit", "unit", "windowUnit", "window_unit"],
        )
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_uppercase();
        let minutes = if unit.contains("MIN") {
            Some(duration)
        } else if unit.contains("HOUR") {
            Some(duration * 60.0)
        } else if unit.contains("DAY") {
            Some(duration * 1440.0)
        } else if unit.contains("WEEK") {
            Some(duration * 10080.0)
        } else if unit.contains("MONTH") {
            Some(duration * 43200.0)
        } else {
            None
        };
        if let Some(minutes) = minutes {
            return Some(if minutes <= 360.0 {
                "fiveHour"
            } else {
                "weekly"
            });
        }
    }
    let name = first(window, &["name", "label", "title"])
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    if name.contains("hour") || name.contains("5h") {
        Some("fiveHour")
    } else if name.contains("week") || name.contains("7d") || name.contains("day") {
        Some("weekly")
    } else {
        None
    }
}

fn reset_value(window: &Value) -> Option<&Value> {
    first(
        window,
        &[
            "resets_at",
            "reset_time",
            "reset_at",
            "resetAt",
            "resetTime",
            "next_reset_at",
            "nextResetAt",
            "ResetTimestamp",
            "reset_timestamp",
            "period_end",
            "billing_period_end",
            "expireTime",
            "expire_time",
        ],
    )
}

fn reset_at(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        if !text.trim().is_empty() && !text.chars().all(|c| c.is_ascii_digit()) {
            return text.to_owned();
        }
    }
    let Some(timestamp) = number(value)
        .map(|value| value as i64)
        .filter(|value| *value > 0)
    else {
        return String::new();
    };
    let seconds = if timestamp >= 1_000_000_000_000 {
        timestamp / 1000
    } else {
        timestamp
    };
    DateTime::from_timestamp(seconds, 0)
        .map(|date| date.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_default()
}

fn provider_from_channel(base_url: &str) -> String {
    let Ok(url) = Url::parse(base_url) else {
        return "thirdparty".to_owned();
    };
    match url.host_str().unwrap_or("").to_ascii_lowercase().as_str() {
        "openrouter.ai" => "openrouter",
        "api.deepseek.com" => "deepseek",
        "api.xiaomimimo.com" | "platform.xiaomimimo.com" => "mimo",
        "api.openai.com" => "openai-api",
        "api.anthropic.com" => "anthropic-api",
        "api.moonshot.ai" | "api.moonshot.cn" => "moonshot",
        _ => "thirdparty",
    }
    .to_owned()
}

fn channel_base_url(channel: &ModelConfig) -> String {
    channel.base_url.trim_end_matches('/').to_owned()
}

fn usage_cache_path(router_root: &Path) -> std::path::PathBuf {
    crate::user_data::data_root(router_root)
        .join("state")
        .join("usage-monitor-last-good.json")
}

fn oauth_recovery_observations_path(router_root: &Path) -> std::path::PathBuf {
    crate::user_data::data_root(router_root)
        .join("state")
        .join("oauth-recovery-observations.json")
}

pub(super) fn next_oauth_recovery_seconds(router_root: &Path, now: DateTime<Utc>) -> u64 {
    let observations =
        read_oauth_recovery_observations(&oauth_recovery_observations_path(router_root));
    next_oauth_recovery_seconds_from(&observations, now)
}

fn next_oauth_recovery_seconds_from(
    observations: &OAuthRecoveryObservations,
    now: DateTime<Utc>,
) -> u64 {
    observations
        .entries
        .iter()
        .filter_map(|entry| {
            let reset_at = DateTime::parse_from_rfc3339(&entry.reset_at)
                .ok()
                .map(|value| value.with_timezone(&Utc))
                .filter(|value| *value > now);
            let due = if entry.exhausted && reset_at.is_some() {
                reset_at?
            } else if !entry.next_probe_at.trim().is_empty() {
                DateTime::parse_from_rfc3339(&entry.next_probe_at)
                    .ok()?
                    .with_timezone(&Utc)
            } else if !entry.observed_at.trim().is_empty() {
                DateTime::parse_from_rfc3339(&entry.observed_at)
                    .ok()?
                    .with_timezone(&Utc)
                    + OAUTH_UNKNOWN_MAX_ISOLATION
            } else {
                return None;
            };
            let seconds = due.signed_duration_since(now).num_seconds().max(1) as u64;
            Some(seconds)
        })
        .min()
        .unwrap_or(OAUTH_RECOVERY_MIN_INTERVAL_SECONDS)
        .min(OAUTH_UNKNOWN_MAX_ISOLATION.num_seconds() as u64)
        .max(OAUTH_RECOVERY_MIN_INTERVAL_SECONDS)
}

fn retain_active_oauth_recovery_observations(
    observations: &mut OAuthRecoveryObservations,
    active_account_ids: &HashSet<i64>,
) -> bool {
    let previous_len = observations.entries.len();
    observations
        .entries
        .retain(|entry| active_account_ids.contains(&entry.account_id));
    observations.entries.len() != previous_len
}

fn read_oauth_recovery_observations(path: &Path) -> OAuthRecoveryObservations {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_oauth_recovery_observations(
    path: &Path,
    observations: &OAuthRecoveryObservations,
) -> anyhow::Result<()> {
    atomic_write(path, &serde_json::to_vec_pretty(observations)?)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OAuthRecoveryDirective {
    None,
    Isolate,
    Recover,
}

fn oauth_recovery_directive(
    evidence: OAuthQuotaEvidence,
    currently_isolated: bool,
    observed_at: &str,
    now: DateTime<Utc>,
) -> OAuthRecoveryDirective {
    match evidence {
        OAuthQuotaEvidence::Usable if currently_isolated => OAuthRecoveryDirective::Recover,
        OAuthQuotaEvidence::Usable => OAuthRecoveryDirective::None,
        OAuthQuotaEvidence::Exhausted => OAuthRecoveryDirective::Isolate,
        OAuthQuotaEvidence::Unknown if observed_at.is_empty() => OAuthRecoveryDirective::None,
        OAuthQuotaEvidence::Unknown => {
            let expired = DateTime::parse_from_rfc3339(observed_at)
                .ok()
                .map(|started| {
                    now.signed_duration_since(started.with_timezone(&Utc))
                        >= OAUTH_UNKNOWN_MAX_ISOLATION
                })
                .unwrap_or(false);
            if expired {
                OAuthRecoveryDirective::Recover
            } else {
                OAuthRecoveryDirective::Isolate
            }
        }
    }
}

fn recover_oauth_account(
    admin: &AdminClient,
    account_id: i64,
    router_group_id: i64,
    deadline: Instant,
) -> anyhow::Result<()> {
    retry_account_read(|| {
        admin.post(
            &format!("/api/v1/admin/accounts/{account_id}/recover-state"),
            None,
            remaining(deadline, Duration::from_secs(10))?,
        )
    })?;
    retry_account_read(|| {
        admin.post(
            &format!("/api/v1/admin/accounts/{account_id}/schedulable"),
            Some(&json!({ "schedulable": true })),
            remaining(deadline, Duration::from_secs(10))?,
        )
    })?;
    if router_group_id > 0 {
        let detail = data(retry_account_read(|| {
            admin.get(
                &format!("/api/v1/admin/accounts/{account_id}"),
                remaining(deadline, Duration::from_secs(10))?,
            )
        })?);
        let mut group_ids = array(get(&detail, "group_ids").unwrap_or(&Value::Null))
            .iter()
            .filter_map(Value::as_i64)
            .collect::<Vec<_>>();
        if !group_ids.contains(&router_group_id) {
            group_ids.push(router_group_id);
            group_ids.sort_unstable();
            group_ids.dedup();
            retry_account_read(|| {
                admin.put(
                    &format!("/api/v1/admin/accounts/{account_id}"),
                    &json!({
                        "group_ids": group_ids,
                        "confirm_mixed_channel_risk": true,
                    }),
                    remaining(deadline, Duration::from_secs(10))?,
                )
            })?;
        }
    }
    Ok(())
}

fn isolate_oauth_account(
    admin: &AdminClient,
    account_id: i64,
    deadline: Instant,
) -> anyhow::Result<()> {
    retry_account_read(|| {
        admin.post(
            &format!("/api/v1/admin/accounts/{account_id}/schedulable"),
            Some(&json!({ "schedulable": false })),
            remaining(deadline, Duration::from_secs(10))?,
        )
    })?;
    Ok(())
}

fn reconcile_oauth_recovery_observations(
    router_root: &Path,
    admin: &AdminClient,
    router_group_id: i64,
    records: &mut [AccountRecord],
    deadline: Instant,
) -> bool {
    let path = oauth_recovery_observations_path(router_root);
    let mut observations = read_oauth_recovery_observations(&path);
    let now = Utc::now();
    let active_account_ids = records
        .iter()
        .filter(|record| record.account.kind == "oauth" && record.auto_isolate_on_exhaustion)
        .map(|record| record.account.id)
        .collect::<HashSet<_>>();
    let mut observations_changed =
        retain_active_oauth_recovery_observations(&mut observations, &active_account_ids);
    let mut routing_changed = records.iter().any(|record| record.routing_changed);

    for record in records
        .iter_mut()
        .filter(|record| record.account.kind == "oauth" && record.auto_isolate_on_exhaustion)
    {
        let account_id = record.account.id;
        let observation_index = observations
            .entries
            .iter()
            .position(|entry| entry.account_id == account_id);
        let observed_at = observation_index
            .and_then(|index| observations.entries.get(index))
            .map(|entry| entry.observed_at.as_str())
            .unwrap_or("");
        let currently_isolated = record.account.status == "error"
            || record.account.status_detail.contains("fallback")
            || (record.quota_evidence == OAuthQuotaEvidence::Unknown
                && observation_index.is_some());
        let directive =
            oauth_recovery_directive(record.quota_evidence, currently_isolated, observed_at, now);

        match directive {
            OAuthRecoveryDirective::None => {
                if let Some(index) = observation_index {
                    observations.entries.remove(index);
                    observations_changed = true;
                }
            }
            OAuthRecoveryDirective::Isolate => {
                if record.account.health != "quotaExhausted"
                    && isolate_oauth_account(admin, account_id, deadline).is_ok()
                {
                    record.account.health = "quotaExhausted".to_owned();
                    record.account.status_detail =
                        "OAuth quota is unavailable; matching API fallback remains active."
                            .to_owned();
                    routing_changed = true;
                }
                let previous = observation_index.and_then(|index| observations.entries.get(index));
                let observed_at = previous
                    .map(|entry| entry.observed_at.clone())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| now.to_rfc3339_opts(SecondsFormat::Millis, true));
                let last_probe_at = now.to_rfc3339_opts(SecondsFormat::Millis, true);
                let probe_delay = if record.quota_evidence == OAuthQuotaEvidence::Exhausted {
                    OAUTH_UNKNOWN_MAX_ISOLATION
                } else {
                    chrono::Duration::minutes(10)
                };
                let next_probe_at =
                    (now + probe_delay).to_rfc3339_opts(SecondsFormat::Millis, true);
                let entry = OAuthRecoveryObservation {
                    account_id,
                    exhausted: record.quota_evidence == OAuthQuotaEvidence::Exhausted,
                    observed_at,
                    last_probe_at,
                    next_probe_at,
                    reset_at: record
                        .account
                        .windows
                        .iter()
                        .filter(|window| window.used_percent.is_some_and(|value| value >= 99.999))
                        .map(|window| window.reset_at.clone())
                        .filter(|value| !value.is_empty())
                        .max()
                        .unwrap_or_default(),
                    recent_error: if record.quota_evidence == OAuthQuotaEvidence::Unknown {
                        "quota_unavailable".to_owned()
                    } else {
                        String::new()
                    },
                };
                if let Some(index) = observation_index {
                    observations_changed |= observations.entries[index] != entry;
                    observations.entries[index] = entry;
                } else {
                    observations.entries.push(entry);
                    observations_changed = true;
                }
            }
            OAuthRecoveryDirective::Recover => {
                if recover_oauth_account(admin, account_id, router_group_id, deadline).is_ok() {
                    record.account.status = "active".to_owned();
                    record.account.health = "healthy".to_owned();
                    record.account.status_detail.clear();
                    if record.quota_evidence == OAuthQuotaEvidence::Unknown {
                        append_note(
                            &mut record.account.query_note,
                            "OAuth isolation reached its five-hour safety limit; the account was restored.",
                        );
                    }
                    if let Some(index) = observation_index {
                        observations.entries.remove(index);
                    }
                    observations_changed = true;
                    routing_changed = true;
                }
            }
        }
    }

    observations.entries.sort_by_key(|entry| entry.account_id);
    if observations_changed {
        let _ = write_oauth_recovery_observations(&path, &observations);
    }
    routing_changed
}

fn read_usage_cache(path: &Path) -> UsageCache {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_usage_cache(path: &Path, cache: &UsageCache) -> anyhow::Result<()> {
    atomic_write(path, &serde_json::to_vec(cache)?)
}

fn cache_key(provider: &str, base_url: &str, credential: &str) -> String {
    format!(
        "{}|{}|{}",
        provider.to_ascii_lowercase(),
        base_url.trim_end_matches('/').to_ascii_lowercase(),
        credential.to_ascii_lowercase()
    )
}

fn cached_usage(
    cache: &Arc<Mutex<UsageCache>>,
    key: &str,
    provider: &str,
    note: &str,
) -> Option<ProviderUsage> {
    let cache = cache.lock().ok()?;
    let entry = cache.get(key)?;
    if entry.windows.is_empty() {
        return None;
    }
    let updated = DateTime::parse_from_rfc3339(&entry.updated_at)
        .ok()?
        .with_timezone(&Utc);
    let age = Utc::now().signed_duration_since(updated);
    if age.num_seconds() < -300 || age.to_std().ok()? > CACHE_TTL {
        return None;
    }
    Some(ProviderUsage {
        provider: provider.to_owned(),
        windows: entry.windows.clone(),
        note: format!("{note} Last successful refresh: {}.", entry.updated_at),
        cached: true,
    })
}

fn save_cache(
    cache: &Arc<Mutex<UsageCache>>,
    key: &str,
    windows: &mut Vec<UsageWindow>,
    merge: bool,
    note: &str,
) {
    let Ok(mut cache) = cache.lock() else { return };
    if merge {
        if let Some(existing) = cache.get(key) {
            let mut merged = windows.clone();
            merge_windows(&mut merged, &existing.windows);
            *windows = merged;
        }
    }
    if !windows.is_empty() {
        cache.insert(
            key.to_owned(),
            CacheEntry {
                updated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                windows: windows.clone(),
                note: note.to_owned(),
            },
        );
    }
}

fn window_cache_key(window: &UsageWindow) -> String {
    format!(
        "{}|{}|{}|{}",
        window.kind.to_ascii_lowercase(),
        window.display_name.to_ascii_lowercase(),
        window.currency.to_ascii_lowercase(),
        window
            .limit_amount
            .map(|value| value.to_string())
            .unwrap_or_default()
    )
}

fn validate_loopback_base_url(input: &str) -> anyhow::Result<String> {
    let url =
        Url::parse(input.trim_end_matches('/')).map_err(|_| anyhow!("class=configuration"))?;
    if url.scheme() != "http"
        || !matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
        || url.port().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
    {
        bail!("class=configuration")
    }
    Ok(format!(
        "http://{}:{}",
        url.host_str().unwrap_or("127.0.0.1"),
        url.port().unwrap_or(18080)
    ))
}

fn admin_cache_path(router_root: &Path) -> std::path::PathBuf {
    crate::user_data::data_root(router_root)
        .join("state")
        .join("admin-session.cache.json")
}

fn read_cached_admin_token(router_root: &Path) -> Option<String> {
    let body: Value =
        serde_json::from_str(&std::fs::read_to_string(admin_cache_path(router_root)).ok()?).ok()?;
    let expires = DateTime::parse_from_rfc3339(get(&body, "expiresAtUtc")?.as_str()?)
        .ok()?
        .with_timezone(&Utc);
    if expires <= Utc::now() + chrono::Duration::minutes(1) {
        return None;
    }
    get(&body, "accessToken")?
        .as_str()
        .map(str::to_owned)
        .filter(|value| !value.is_empty())
}

fn write_cached_admin_token(router_root: &Path, token: &str) -> anyhow::Result<()> {
    let body = json!({ "accessToken": token, "expiresAtUtc": (Utc::now() + chrono::Duration::hours(12)).to_rfc3339_opts(SecondsFormat::Millis, true) });
    atomic_write(&admin_cache_path(router_root), &serde_json::to_vec(&body)?)
}

fn credential_string(name: &str) -> Option<Zeroizing<String>> {
    if name.trim().is_empty() {
        return None;
    }
    let secret = read_router_credential(name).ok().flatten()?;
    String::from_utf16(&secret.0)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(Zeroizing::new)
}

pub(super) fn retry_admin_read<T>(
    mut operation: impl FnMut() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let mut last = None;
    for delay in [0, 250, 750] {
        if delay > 0 {
            std::thread::sleep(Duration::from_millis(delay));
        }
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("class=request_failure")))
}

pub(super) fn retry_account_read<T>(
    mut operation: impl FnMut() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let mut last = None;
    for attempt in 0..super::USAGE_MONITOR_MAX_ATTEMPTS {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) => last = Some(error),
        }
        let retryable = last
            .as_ref()
            .is_some_and(|error| super::usage_failure_is_locally_retryable(&error.to_string()));
        if attempt + 1 >= super::USAGE_MONITOR_MAX_ATTEMPTS || !retryable {
            break;
        }
        std::thread::sleep(super::usage_monitor_retry_delay(attempt));
    }
    Err(last.unwrap_or_else(|| anyhow!("class=request_failure")))
}

fn remaining(deadline: Instant, maximum: Duration) -> anyhow::Result<Duration> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| anyhow!("class=timeout"))?;
    Ok(remaining.min(maximum).max(Duration::from_millis(50)))
}

fn classify_request_error(error: reqwest::Error) -> anyhow::Error {
    if error.is_timeout() {
        anyhow!("class=timeout")
    } else if error.is_connect() {
        anyhow!("class=connection_refused")
    } else {
        anyhow!("class=request_failure")
    }
}

fn classify_status(status: u16) -> &'static str {
    match status {
        401 => "class=authentication",
        403 => "class=permission",
        429 => "class=rate_limit",
        500..=599 => "class=upstream",
        _ => "class=request_failure",
    }
}

fn status_label(status: i32) -> String {
    if status > 0 {
        format!("HTTP {status}")
    } else {
        "transport error".to_owned()
    }
}

fn safe_error(error: &anyhow::Error) -> String {
    let message = error.to_string();
    if message.starts_with("class=") {
        message
    } else {
        "class=request_failure".to_owned()
    }
}

fn append_note(target: &mut String, note: &str) {
    if note.trim().is_empty() {
        return;
    }
    if !target.is_empty() {
        target.push(' ');
    }
    target.push_str(note.trim());
}

pub(super) fn data(value: Value) -> Value {
    value.get("data").cloned().unwrap_or(value)
}
fn data_ref(value: &Value) -> &Value {
    get(value, "data").unwrap_or(value)
}
pub(super) fn get<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    value
        .as_object()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
}
fn get_path<'a>(mut value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    for name in path {
        value = get(value, name)?;
    }
    Some(value)
}
fn first<'a>(value: &'a Value, names: &[&str]) -> Option<&'a Value> {
    names
        .iter()
        .find_map(|name| get(value, name))
        .filter(|value| {
            !value.is_null() && value.as_str().is_none_or(|text| !text.trim().is_empty())
        })
}
pub(super) fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
pub(super) fn string(value: &Value, name: &str) -> String {
    get(value, name)
        .and_then(|value| {
            value.as_str().map(str::to_owned).or_else(|| {
                if value.is_null() {
                    None
                } else {
                    Some(value.to_string().trim_matches('"').to_owned())
                }
            })
        })
        .unwrap_or_default()
}
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|value| value.is_finite())
}
pub(super) fn integer(value: &Value, name: &str) -> i64 {
    get(value, name)
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str()?.parse().ok())
                .or_else(|| value.as_f64().map(|value| value as i64))
        })
        .unwrap_or_default()
}
fn window_kind(seconds: i64, fallback: &str) -> String {
    if seconds > 0 && seconds <= 21_600 {
        "fiveHour"
    } else if seconds > 21_600 && seconds <= 691_200 {
        "weekly"
    } else if seconds > 691_200 {
        "monthly"
    } else {
        fallback
    }
    .to_owned()
}

fn volcengine_headers(access_key: &str, secret: &str, now: DateTime<Utc>) -> HeaderMap {
    type HmacSha256 = Hmac<Sha256>;
    fn sign(key: &[u8], value: &str) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key");
        mac.update(value.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }
    let host = "open.volcengineapi.com";
    let region = "cn-beijing";
    let service = "ark";
    let content_type = "application/json; charset=UTF-8";
    let signed_headers = "content-type;host;x-content-sha256;x-date";
    let payload_hash = format!("{:x}", Sha256::digest([]));
    let x_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let short_date = &x_date[..8];
    let canonical_headers = format!("content-type:{content_type}\nhost:{host}\nx-content-sha256:{payload_hash}\nx-date:{x_date}\n");
    let canonical = format!("POST\n/\nAction=GetCodingPlanUsage&Version=2024-01-01\n{canonical_headers}\n{signed_headers}\n{payload_hash}");
    let request_hash = format!("{:x}", Sha256::digest(canonical.as_bytes()));
    let scope = format!("{short_date}/{region}/{service}/request");
    let string_to_sign = format!("HMAC-SHA256\n{x_date}\n{scope}\n{request_hash}");
    let mut secret_bytes = secret.as_bytes().to_vec();
    let mut date_key = sign(&secret_bytes, short_date);
    let mut region_key = sign(&date_key, region);
    let mut service_key = sign(&region_key, service);
    let mut signing_key = sign(&service_key, "request");
    let mut signature = sign(&signing_key, &string_to_sign);
    let authorization = format!(
        "HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed_headers}, Signature={}",
        signature
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    secret_bytes.zeroize();
    date_key.zeroize();
    region_key.zeroize();
    service_key.zeroize();
    signing_key.zeroize();
    signature.zeroize();
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=UTF-8"),
    );
    headers.insert(
        HeaderName::from_static("x-date"),
        HeaderValue::from_str(&x_date).expect("valid date"),
    );
    headers.insert(
        HeaderName::from_static("x-content-sha256"),
        HeaderValue::from_str(&payload_hash).expect("valid hash"),
    );
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&authorization).expect("valid authorization"),
    );
    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn usable_oauth_quota_recovers_immediately() {
        let now = DateTime::parse_from_rfc3339("2026-08-12T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            oauth_recovery_directive(
                OAuthQuotaEvidence::Usable,
                true,
                "2026-08-11T23:59:00Z",
                now,
            ),
            OAuthRecoveryDirective::Recover
        );
    }

    #[test]
    fn antigravity_models_with_the_same_quota_signature_share_one_pool() {
        let usage = json!({
            "antigravity_quota": {
                "claude-opus": {"used_percent": 20, "reset_at": "2026-08-13T09:16:47Z"},
                "gemini-flash": {"used_percent": 20, "reset_at": "2026-08-13T09:16:47Z"},
                "gemini-pro": {"used_percent": 20, "reset_at": "2026-08-13T09:16:47Z"}
            },
            "antigravity_quota_details": {
                "claude-opus": {"display_name": "Claude Opus"},
                "gemini-flash": {"display_name": "Gemini Flash"},
                "gemini-pro": {"display_name": "Gemini Pro"}
            }
        });

        let windows = normalize_antigravity(&usage);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].kind, "sharedPool");
        assert_eq!(windows[0].display_name, "Antigravity shared quota");
    }

    #[test]
    fn antigravity_models_with_different_quota_signatures_stay_separate() {
        let usage = json!({
            "antigravity_quota": {
                "claude-opus": {"used_percent": 20, "reset_at": "2026-08-13T09:16:47Z"},
                "gemini-flash": {"used_percent": 20, "reset_at": "2026-08-14T09:16:47Z"},
                "gemini-pro": {"used_percent": 35, "reset_at": "2026-08-13T09:16:47Z"}
            }
        });

        let windows = normalize_antigravity(&usage);
        assert_eq!(windows.len(), 3);
        assert!(windows.iter().all(|window| window.kind == "model"));
    }

    #[test]
    fn antigravity_shared_pool_does_not_duplicate_an_account_window() {
        let usage = json!({
            "five_hour": {"used_percent": 20, "reset_at": "2026-08-13T09:16:47Z"},
            "antigravity_quota": {
                "claude-opus": {"used_percent": 20, "reset_at": "2026-08-13T09:16:47Z"},
                "gemini-flash": {"used_percent": 20, "reset_at": "2026-08-13T09:16:47Z"}
            }
        });

        assert!(normalize_antigravity(&usage).is_empty());
    }

    #[test]
    fn antigravity_internal_entries_are_hidden_and_real_pools_are_named_by_family() {
        let usage = json!({
            "antigravity_quota": {
                "chat_20706": {"utilization": 0, "reset_time": ""},
                "tab_flash_lite_preview": {"utilization": 0, "reset_time": ""},
                "gemini-3.6-flash-high": {"utilization": 0, "reset_time": "2026-08-14T05:10:20Z"},
                "gemini-3.7-flash": {"utilization": 0, "reset_time": "2026-08-14T05:10:20Z"},
                "claude-sonnet-4-6": {"utilization": 0, "reset_time": "2026-08-14T06:14:07Z"},
                "gpt-oss-120b-medium": {"utilization": 0, "reset_time": "2026-08-14T06:14:07Z"}
            }
        });

        let windows = normalize_antigravity(&usage);
        assert_eq!(windows.len(), 2);
        assert_eq!(
            windows
                .iter()
                .map(|window| window.display_name.as_str())
                .collect::<HashSet<_>>(),
            HashSet::from(["Gemini shared quota", "Claude / GPT shared quota"])
        );
    }

    #[test]
    fn api_models_with_the_same_real_key_merge_into_one_quota_pool() {
        let nonce = Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let first_credential = format!("Codex-Router-Usage-Pool-A-{nonce}");
        let second_credential = format!("Codex-Router-Usage-Pool-B-{nonce}");
        let result = (|| -> anyhow::Result<()> {
            crate::logic::write_router_credential_text(&first_credential, "same-real-kimi-key")?;
            crate::logic::write_router_credential_text(&second_credential, "same-real-kimi-key")?;
            let first_channel = ModelConfig {
                model: "kimi-for-coding".to_owned(),
                base_url: "https://api.kimi.com/coding/v1".to_owned(),
                credential_name: first_credential.clone(),
                ..Default::default()
            };
            let second_channel = ModelConfig {
                model: "k3-256k".to_owned(),
                base_url: "https://api.kimi.com/coding/v1".to_owned(),
                credential_name: second_credential.clone(),
                ..Default::default()
            };
            assert_eq!(
                api_quota_pool_key(&first_channel),
                api_quota_pool_key(&second_channel)
            );

            let first_name = "Codex-Router / Kimi For Coding".to_owned();
            let second_name = "Codex-Router / Kimi K3".to_owned();
            let channels = HashMap::from([
                (first_name.clone(), first_channel),
                (second_name.clone(), second_channel),
            ]);
            let record = |id, name: String, model: &str, requests| AccountRecord {
                account: UsageAccount {
                    id,
                    name,
                    kind: "apikey".to_owned(),
                    platform: "openai".to_owned(),
                    totals: UsageTotals {
                        requests,
                        ..Default::default()
                    },
                    windows: vec![coding_window("weekly", 25.0, None, "Kimi Coding Plan")],
                    ..Default::default()
                },
                configured_model: model.to_owned(),
                quota_evidence: OAuthQuotaEvidence::Unknown,
                auto_isolate_on_exhaustion: false,
                routing_changed: false,
            };
            let merged = merge_api_quota_pools(
                vec![
                    record(1, first_name, "kimi-for-coding", 2),
                    record(2, second_name, "k3-256k", 3),
                ],
                &channels,
            );
            assert_eq!(merged.len(), 1);
            assert_eq!(merged[0].account.name, "Codex-Router / Kimi Coding Plan");
            assert_eq!(merged[0].account.totals.requests, 5);
            assert_eq!(merged[0].account.windows.len(), 1);
            Ok(())
        })();
        let _ = crate::logic::delete_router_credential(&first_credential);
        let _ = crate::logic::delete_router_credential(&second_credential);
        result.unwrap();
    }

    #[test]
    fn unknown_oauth_quota_stays_isolated_before_five_hours() {
        let now = DateTime::parse_from_rfc3339("2026-08-12T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            oauth_recovery_directive(
                OAuthQuotaEvidence::Unknown,
                true,
                "2026-08-11T19:00:01Z",
                now,
            ),
            OAuthRecoveryDirective::Isolate
        );
    }

    #[test]
    fn unknown_oauth_quota_recovers_at_five_hour_limit() {
        let now = DateTime::parse_from_rfc3339("2026-08-12T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            oauth_recovery_directive(
                OAuthQuotaEvidence::Unknown,
                true,
                "2026-08-11T19:00:00Z",
                now,
            ),
            OAuthRecoveryDirective::Recover
        );
        assert_eq!(
            oauth_recovery_directive(
                OAuthQuotaEvidence::Exhausted,
                true,
                "2026-08-11T18:00:00Z",
                now,
            ),
            OAuthRecoveryDirective::Isolate,
            "freshly confirmed exhaustion must not be force-recovered"
        );
    }

    #[test]
    fn oauth_recovery_schedule_uses_reset_ten_minute_probe_and_five_hour_cap() {
        let now = DateTime::parse_from_rfc3339("2026-08-12T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let reset = OAuthRecoveryObservations {
            entries: vec![OAuthRecoveryObservation {
                account_id: 1,
                exhausted: true,
                observed_at: "2026-08-11T23:00:00Z".into(),
                next_probe_at: "2026-08-12T05:00:00Z".into(),
                reset_at: "2026-08-12T02:00:00Z".into(),
                ..Default::default()
            }],
        };
        assert_eq!(next_oauth_recovery_seconds_from(&reset, now), 2 * 60 * 60);

        let unknown = OAuthRecoveryObservations {
            entries: vec![OAuthRecoveryObservation {
                account_id: 2,
                observed_at: "2026-08-11T19:10:00Z".into(),
                next_probe_at: "2026-08-12T00:10:00Z".into(),
                ..Default::default()
            }],
        };
        assert_eq!(next_oauth_recovery_seconds_from(&unknown, now), 10 * 60);

        let capped = OAuthRecoveryObservations {
            entries: vec![OAuthRecoveryObservation {
                account_id: 3,
                observed_at: "2026-08-11T19:00:00Z".into(),
                next_probe_at: "2026-08-12T00:10:00Z".into(),
                ..Default::default()
            }],
        };
        // The directive force-recovers at the cap during the current query;
        // a retained edge-case observation must still be retried promptly.
        assert_eq!(next_oauth_recovery_seconds_from(&capped, now), 10 * 60);
    }

    #[test]
    fn overdue_oauth_recovery_observation_never_creates_a_sub_ten_minute_loop() {
        let now = DateTime::parse_from_rfc3339("2026-08-12T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let overdue = OAuthRecoveryObservations {
            entries: vec![OAuthRecoveryObservation {
                account_id: 15,
                exhausted: true,
                observed_at: "2026-08-10T13:40:15Z".into(),
                next_probe_at: "2026-08-11T04:45:17Z".into(),
                reset_at: "2026-08-11T11:33:07Z".into(),
                ..Default::default()
            }],
        };

        assert_eq!(next_oauth_recovery_seconds_from(&overdue, now), 10 * 60);
    }

    #[test]
    fn recovery_observations_drop_accounts_without_an_active_fallback() {
        let mut observations = OAuthRecoveryObservations {
            entries: vec![
                OAuthRecoveryObservation {
                    account_id: 1,
                    exhausted: true,
                    ..Default::default()
                },
                OAuthRecoveryObservation {
                    account_id: 15,
                    exhausted: true,
                    ..Default::default()
                },
            ],
        };

        assert!(retain_active_oauth_recovery_observations(
            &mut observations,
            &HashSet::from([1]),
        ));
        assert_eq!(
            observations
                .entries
                .iter()
                .map(|entry| entry.account_id)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert!(!retain_active_oauth_recovery_observations(
            &mut observations,
            &HashSet::from([1]),
        ));
    }

    #[test]
    fn kimi_nested_limits_keep_five_hour_and_weekly_lanes() {
        let body = json!({ "data": { "data": { "limits": [
            { "window": { "duration": 5, "timeUnit": "HOUR" }, "detail": { "used": 25, "limit": 100, "resetAt": 1785751200000_i64 } },
            { "window": { "duration": 7, "timeUnit": "DAY" }, "detail": { "remaining": 40, "limit": 100, "resetAt": 1786000000000_i64 } }
        ] } } });
        let windows = normalize_kimi(&body);
        assert_eq!(
            windows
                .iter()
                .map(|window| (&window.kind, window.used_percent))
                .collect::<Vec<_>>(),
            vec![
                (&"fiveHour".to_owned(), Some(25.0)),
                (&"weekly".to_owned(), Some(60.0))
            ]
        );
    }

    #[test]
    fn zhipu_minimax_and_zenmux_payloads_are_normalized() {
        let zhipu = normalize_zhipu(
            &json!({ "data": { "limits": [
            { "type": "TOKENS_LIMIT", "unit": 6, "percentage": 45 },
            { "type": "tokens_limit", "unit": 3, "percentage": 10 }
        ] } }),
            None,
        );
        assert_eq!(
            zhipu
                .iter()
                .map(|window| (&window.kind, window.used_percent))
                .collect::<Vec<_>>(),
            vec![
                (&"fiveHour".to_owned(), Some(10.0)),
                (&"weekly".to_owned(), Some(45.0))
            ]
        );
        let minimax = normalize_minimax(
            &json!({ "model_remains": [{ "model_name": "general", "current_interval_remaining_percent": 35, "current_weekly_remaining_percent": 80 }] }),
        );
        assert_eq!(
            minimax
                .iter()
                .map(|window| window.used_percent)
                .collect::<Vec<_>>(),
            vec![Some(65.0), Some(20.0)]
        );
        let zenmux = normalize_zenmux(
            &json!({ "success": true, "data": { "quota_5_hour": { "usage_percentage": 0.25 }, "quota_7_day": { "usage_percentage": 0.8 } } }),
        );
        assert_eq!(
            zenmux
                .iter()
                .map(|window| window.used_percent)
                .collect::<Vec<_>>(),
            vec![Some(25.0), Some(80.0)]
        );
    }

    #[test]
    fn stale_cache_is_rejected_and_partial_windows_are_merged() {
        let cache = Arc::new(Mutex::new(UsageCache::new()));
        cache.lock().unwrap().insert(
            "old".to_owned(),
            CacheEntry {
                updated_at: (Utc::now() - chrono::Duration::hours(7)).to_rfc3339(),
                windows: vec![coding_window("weekly", 99.0, None, "")],
                note: String::new(),
            },
        );
        assert!(cached_usage(&cache, "old", "Provider", "failed").is_none());
        cache.lock().unwrap().insert(
            "fresh".to_owned(),
            CacheEntry {
                updated_at: Utc::now().to_rfc3339(),
                windows: vec![
                    coding_window("fiveHour", 20.0, None, ""),
                    coding_window("weekly", 30.0, None, ""),
                ],
                note: String::new(),
            },
        );
        let mut incoming = vec![coding_window("weekly", 40.0, None, "")];
        save_cache(&cache, "fresh", &mut incoming, true, "");
        assert_eq!(incoming.len(), 2);
        assert_eq!(incoming[0].used_percent, Some(40.0));
    }

    #[test]
    fn fresh_kimi_data_clears_historical_403_state() {
        let windows = vec![
            coding_window("fiveHour", 0.0, None, ""),
            coding_window("weekly", 0.0, None, ""),
        ];
        let state = resolve_state(
            "active",
            Some(true),
            "class=rate_limit status=403",
            &windows,
            true,
        );
        assert_eq!(state, ("healthy".to_owned(), String::new()));
    }

    #[test]
    fn fresh_live_quota_recovers_a_misdisabled_api_account() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(750);
            let mut paths = Vec::new();
            while Instant::now() < deadline && paths.len() < 2 {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("mock server accept failed: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                paths.push(request.lines().next().unwrap_or_default().to_owned());
                let body = r#"{"data":{"status":"active"}}"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            paths
        });
        let admin = AdminClient {
            client: Client::builder().no_proxy().build().unwrap(),
            base_url: format!("http://{address}"),
            bearer: Arc::new(Zeroizing::new("test-token".to_owned())),
        };
        let task = AccountTask {
            account: json!({
                "id": 42,
                "type": "apikey",
                "platform": "openai",
                "status": "error",
                "schedulable": false,
                "error_message": "k3-256k supports only 256K context."
            }),
            channel: Some(ModelConfig {
                base_url: "https://api.kimi.com/coding/v1".to_owned(),
                model: "k3-256k".to_owned(),
                ..ModelConfig::default()
            }),
            query_provider_usage: false,
            auto_isolate_on_exhaustion: false,
        };
        let windows = vec![coding_window("weekly", 25.0, None, "")];

        assert!(maybe_recover_misdisabled_account(
            &admin,
            &task,
            &windows,
            true,
            "",
            Instant::now() + Duration::from_secs(2),
        ));
        let paths = server.join().unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].contains("/api/v1/admin/accounts/42/recover-state"));
        assert!(paths[1].contains("/api/v1/admin/accounts/42/schedulable"));
    }

    #[test]
    fn recovery_rejects_invalid_credentials_cached_quota_and_exhausted_quota() {
        let admin = AdminClient {
            client: Client::builder().no_proxy().build().unwrap(),
            base_url: "http://127.0.0.1:9".to_owned(),
            bearer: Arc::new(Zeroizing::new("test-token".to_owned())),
        };
        let task = AccountTask {
            account: json!({
                "id": 43,
                "type": "apikey",
                "platform": "openai",
                "status": "error",
                "schedulable": false,
                "error_message": "401 invalid API key"
            }),
            channel: Some(ModelConfig {
                base_url: "https://api.kimi.com/coding/v1".to_owned(),
                model: "k3-256k".to_owned(),
                ..ModelConfig::default()
            }),
            query_provider_usage: false,
            auto_isolate_on_exhaustion: false,
        };
        let mut windows = vec![coding_window("weekly", 25.0, None, "")];

        assert!(!maybe_recover_misdisabled_account(
            &admin,
            &task,
            &windows,
            true,
            "class=authentication | marker=ROUTER_KIMI_CREDENTIAL_REJECTED",
            Instant::now() + Duration::from_secs(1),
        ));
        assert!(!maybe_recover_misdisabled_account(
            &admin,
            &task,
            &windows,
            false,
            "",
            Instant::now() + Duration::from_secs(1),
        ));
        windows[0].used_percent = Some(100.0);
        assert!(!maybe_recover_misdisabled_account(
            &admin,
            &task,
            &windows,
            true,
            "",
            Instant::now() + Duration::from_secs(1),
        ));
    }

    #[test]
    fn fresh_live_quota_recovers_a_misdisabled_oauth_account() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(750);
            let mut paths = Vec::new();
            while Instant::now() < deadline && paths.len() < 2 {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("mock server accept failed: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                paths.push(request.lines().next().unwrap_or_default().to_owned());
                let body = r#"{"data":{"status":"active"}}"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            paths
        });
        let admin = AdminClient {
            client: Client::builder().no_proxy().build().unwrap(),
            base_url: format!("http://{address}"),
            bearer: Arc::new(Zeroizing::new("test-token".to_owned())),
        };
        let task = AccountTask {
            account: json!({
                "id": 44,
                "type": "oauth",
                "platform": "grok",
                "status": "error",
                "schedulable": false,
                "error_message": "stale provider error"
            }),
            channel: None,
            query_provider_usage: false,
            auto_isolate_on_exhaustion: false,
        };
        let windows = vec![coding_window("weekly", 25.0, None, "")];

        assert!(maybe_recover_misdisabled_account(
            &admin,
            &task,
            &windows,
            true,
            "",
            Instant::now() + Duration::from_secs(2),
        ));
        let paths = server.join().unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].contains("/api/v1/admin/accounts/44/recover-state"));
        assert!(paths[1].contains("/api/v1/admin/accounts/44/schedulable"));
    }

    #[test]
    fn fresh_exhausted_oauth_is_made_unschedulable_for_api_fallback() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]).into_owned();
            let body = r#"{"data":{"schedulable":false}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            request
        });
        let admin = AdminClient {
            client: Client::builder().no_proxy().build().unwrap(),
            base_url: format!("http://{address}"),
            bearer: Arc::new(Zeroizing::new("test-token".to_owned())),
        };
        let task = AccountTask {
            account: json!({
                "id": 45,
                "type": "oauth",
                "platform": "openai",
                "status": "active",
                "schedulable": true
            }),
            channel: None,
            query_provider_usage: false,
            auto_isolate_on_exhaustion: true,
        };
        let windows = vec![coding_window("weekly", 100.0, None, "")];

        assert!(maybe_isolate_exhausted_oauth_account(
            &admin,
            &task,
            &windows,
            true,
            "",
            Instant::now() + Duration::from_secs(2),
        ));
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /api/v1/admin/accounts/45/schedulable "));
        assert!(request.contains(r#""schedulable":false"#));
    }

    #[test]
    fn only_oauth_accounts_with_a_selected_matching_api_fallback_are_auto_isolated() {
        let mut cfg = RouterConfig {
            oauth_fallback: crate::config::OAuthFallback {
                enabled: true,
                ..Default::default()
            },
            models: vec![
                ModelConfig {
                    source: "oauth".to_owned(),
                    model: "gpt-5.6-sol".to_owned(),
                    oauth_account_id: 7,
                    ..Default::default()
                },
                ModelConfig {
                    source: "oauth".to_owned(),
                    model: "claude-opus-4.6".to_owned(),
                    oauth_account_id: 8,
                    ..Default::default()
                },
                ModelConfig {
                    source: "apikey".to_owned(),
                    model: "gpt-5.6-sol".to_owned(),
                    base_url: "https://api.example.test/v1".to_owned(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(oauth_accounts_with_api_fallback(&cfg), HashSet::from([7]));

        cfg.oauth_fallback.enabled = false;
        assert!(oauth_accounts_with_api_fallback(&cfg).is_empty());
    }

    #[test]
    fn oauth_priority_update_uses_native_admin_get_put_get_flow() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut requests = Vec::new();
            while Instant::now() < deadline && requests.len() < 3 {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("mock server accept failed: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]).to_string();
                let index = requests.len();
                requests.push(request);
                let body = match index {
                    0 => r#"{"data":{"id":51,"type":"oauth","priority":1}}"#,
                    1 => r#"{"data":{}}"#,
                    _ => r#"{"data":{"id":51,"type":"oauth","priority":37}}"#,
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            requests
        });
        let admin = AdminClient {
            client: Client::builder().no_proxy().build().unwrap(),
            base_url: format!("http://{address}"),
            bearer: Arc::new(Zeroizing::new("test-token".to_owned())),
        };

        assert_eq!(
            set_oauth_account_priority_with_admin(&admin, 51, 37).unwrap(),
            37
        );
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests[0].starts_with("GET /api/v1/admin/accounts/51 "));
        assert!(requests[1].starts_with("PUT /api/v1/admin/accounts/51 "));
        assert!(requests[1].contains("\"priority\":37"));
        assert!(requests[1].contains("\"confirm_mixed_channel_risk\":true"));
        assert!(requests[2].starts_with("GET /api/v1/admin/accounts/51 "));
    }

    #[test]
    fn oauth_account_query_retries_transient_request_failure_before_showing_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut usage_requests = 0;
            while Instant::now() < deadline && usage_requests < 2 {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("mock server accept failed: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                let (status, body) = if request.contains("/stats ") {
                    ("200 OK", r#"{"data":{"total_tokens":1}}"#)
                } else if request.contains("/usage?force=true ") {
                    usage_requests += 1;
                    if usage_requests == 1 {
                        ("503 Service Unavailable", r#"{"error":"transient"}"#)
                    } else {
                        ("200 OK", r#"{"data":{"five_hour":{"used_percent":25}}}"#)
                    }
                } else {
                    ("404 Not Found", r#"{"error":"unexpected path"}"#)
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            usage_requests
        });
        let admin = AdminClient {
            client: Client::builder().no_proxy().build().unwrap(),
            base_url: format!("http://{address}"),
            bearer: Arc::new(Zeroizing::new("test-token".to_owned())),
        };
        let task = AccountTask {
            account: json!({"id": 7, "type": "oauth", "platform": "gemini", "status": "active", "schedulable": true}),
            channel: None,
            query_provider_usage: false,
            auto_isolate_on_exhaustion: false,
        };
        let cache = Arc::new(Mutex::new(UsageCache::new()));

        let record = query_account(
            &admin,
            task,
            &cache,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        let usage_requests = server.join().unwrap();

        assert_eq!(
            usage_requests, 2,
            "the transient provider request was not retried"
        );
        assert_eq!(record.account.windows.len(), 1);
        assert!(
            record.account.query_note.is_empty(),
            "transient failure leaked into OAuth UI"
        );
    }

    #[test]
    fn usage_refresh_does_not_read_or_import_the_oauth_model_catalog() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut paths = Vec::new();
            while Instant::now() < deadline && paths.len() < 3 {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("mock server accept failed: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or_default()
                    .to_owned();
                let body = if path.ends_with("/stats") {
                    r#"{"data":{"total_tokens":1}}"#
                } else if path.contains("/usage?force=true") {
                    r#"{"data":{"five_hour":{"used_percent":25}}}"#
                } else if path.ends_with("/models") {
                    r#"{"data":{"items":[{"id":"grok-4.6"}]}}"#
                } else {
                    r#"{"error":"unexpected path"}"#
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
                paths.push(path);
            }
            paths
        });
        let admin = AdminClient {
            client: Client::builder().no_proxy().build().unwrap(),
            base_url: format!("http://{address}"),
            bearer: Arc::new(Zeroizing::new("test-token".to_owned())),
        };
        let task = AccountTask {
            account: json!({"id": 10, "type": "oauth", "platform": "grok", "status": "active", "schedulable": true}),
            channel: None,
            query_provider_usage: false,
            auto_isolate_on_exhaustion: false,
        };

        query_account(
            &admin,
            task,
            &Arc::new(Mutex::new(UsageCache::new())),
            Instant::now() + Duration::from_secs(3),
        )
        .unwrap();
        let paths = server.join().unwrap();

        assert!(paths.iter().all(|path| !path.ends_with("/models")));
    }

    #[test]
    fn antigravity_quota_uses_live_models_even_when_configured_names_are_stale() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut requests = 0;
            while Instant::now() < deadline && requests < 2 {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("mock server accept failed: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                let body = if request.contains("/stats ") {
                    r#"{"data":{"total_tokens":1}}"#
                } else if request.contains("/usage?force=true ") {
                    r#"{"data":{"antigravity_quota":{"gemini-live":{"utilization":25,"reset_time":"2026-08-13T00:00:00Z"}},"antigravity_quota_details":{"gemini-live":{"display_name":"Gemini Live"}}}}"#
                } else {
                    r#"{"error":"unexpected path"}"#
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
                requests += 1;
            }
        });
        let admin = AdminClient {
            client: Client::builder().no_proxy().build().unwrap(),
            base_url: format!("http://{address}"),
            bearer: Arc::new(Zeroizing::new("test-token".to_owned())),
        };
        let task = AccountTask {
            account: json!({"id": 8, "type": "oauth", "platform": "antigravity", "status": "active", "schedulable": true}),
            channel: None,
            query_provider_usage: false,
            auto_isolate_on_exhaustion: false,
        };

        let record = query_account(
            &admin,
            task,
            &Arc::new(Mutex::new(UsageCache::new())),
            Instant::now() + Duration::from_secs(3),
        )
        .unwrap();
        server.join().unwrap();

        assert_eq!(record.account.windows.len(), 1);
        assert_eq!(record.account.windows[0].display_name, "Gemini Live");
        assert!(record.account.query_note.is_empty());
    }

    #[test]
    fn antigravity_degraded_authentication_is_not_reported_as_request_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut requests = 0;
            while Instant::now() < deadline && requests < 2 {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("mock server accept failed: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                let body = if request.contains("/stats ") {
                    r#"{"data":{"total_tokens":1}}"#
                } else {
                    r#"{"data":{"error_code":"unauthenticated","needs_reauth":true}}"#
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
                requests += 1;
            }
        });
        let admin = AdminClient {
            client: Client::builder().no_proxy().build().unwrap(),
            base_url: format!("http://{address}"),
            bearer: Arc::new(Zeroizing::new("test-token".to_owned())),
        };
        let task = AccountTask {
            account: json!({"id": 9, "type": "oauth", "platform": "antigravity", "status": "active", "schedulable": true}),
            channel: None,
            query_provider_usage: false,
            auto_isolate_on_exhaustion: false,
        };

        let record = query_account(
            &admin,
            task,
            &Arc::new(Mutex::new(UsageCache::new())),
            Instant::now() + Duration::from_secs(3),
        )
        .unwrap();
        server.join().unwrap();

        assert_eq!(record.account.query_note, "class=authentication");
        assert!(!record.account.query_note.contains("request_failure"));
    }

    #[test]
    fn credential_endpoints_reject_host_and_scheme_confusion() {
        assert!(coding_plan_endpoint("https://api.zenmux.ai/api/v1").is_some());
        for url in [
            "https://attacker.example/zenmux",
            "https://api.zenmux.ai.attacker.example/v1",
            "http://api.zenmux.ai/api/v1",
            "https://user@api.zenmux.ai/api/v1",
            "https://api.kimi.com/v1",
        ] {
            assert!(coding_plan_endpoint(url).is_none(), "accepted {url}");
        }
    }

    #[test]
    #[ignore = "requires the running local Router and explicit CODEX_ROUTER_LIVE_USAGE_TEST=1"]
    fn live_usage_snapshot_returns_configured_accounts_without_exposing_identity() {
        assert_eq!(
            std::env::var("CODEX_ROUTER_LIVE_USAGE_TEST").as_deref(),
            Ok("1")
        );
        let router_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("source root");
        let config = RouterConfig::load(&crate::user_data::config_path(router_root))
            .expect("load the real Router config");
        let snapshot = query_usage(
            router_root,
            "live-acceptance",
            &config,
            Instant::now() + Duration::from_secs(120),
        )
        .expect("query the running Router");
        assert!(
            !snapshot.subscriptions.is_empty() || !snapshot.api_channels.is_empty(),
            "the real profile unexpectedly returned no configured accounts"
        );
        assert!(
            snapshot
                .subscriptions
                .iter()
                .all(|account| !account.query_note.contains("class=request_failure")),
            "a subscription still exposes class=request_failure"
        );
        if let Some(kimi) = snapshot
            .api_channels
            .iter()
            .find(|account| account.name.to_ascii_lowercase().contains("kimi"))
        {
            assert!(
                !kimi.windows.is_empty(),
                "Kimi returned no readable quota window"
            );
            assert!(
                !kimi.query_note.contains("ROUTER_KIMI_CREDENTIAL_REJECTED"),
                "Kimi rejected the credential selected in the application"
            );
        }
        eprintln!(
            "live usage acceptance: subscriptions={}, api_channels={}",
            snapshot.subscriptions.len(),
            snapshot.api_channels.len()
        );
    }
}
