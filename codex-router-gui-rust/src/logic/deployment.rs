use super::catalog::{build_route_plan, ModelRoute};
use super::usage::{self, AdminClient};
use crate::config::{ModelConfig, RouterConfig};
use crate::proxy::ProxyRuntime;
use anyhow::{bail, Context};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use url::Url;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MANAGED_GROUP_NAME: &str = "Codex-Router";
const MANAGED_PROXY_NAME: &str = "Codex-Router / Auto-detected outbound proxy";

fn api_account_name(model: &ModelConfig) -> String {
    let label = if model.alias.trim().is_empty() {
        model.model.trim()
    } else {
        model.alias.trim()
    };
    let vendor = super::classify_channel_route(model).vendor;
    let seed = format!(
        "{}\n{}",
        model.credential_name.trim().to_ascii_lowercase(),
        model.base_url.trim().trim_end_matches('/').to_ascii_lowercase()
    );
    let digest = Sha256::digest(seed.as_bytes());
    let token = digest[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("Codex-Router / {label} / {vendor}-{token}")
}

trait AdminApi {
    fn get(&self, path: &str) -> anyhow::Result<Value>;
    fn post(&self, path: &str, body: Option<&Value>) -> anyhow::Result<Value>;
    fn put(&self, path: &str, body: &Value) -> anyhow::Result<Value>;
    fn delete(&self, path: &str) -> anyhow::Result<Value>;
}

impl AdminApi for AdminClient {
    fn get(&self, path: &str) -> anyhow::Result<Value> {
        self.get(path, REQUEST_TIMEOUT)
    }

    fn post(&self, path: &str, body: Option<&Value>) -> anyhow::Result<Value> {
        self.post(path, body, REQUEST_TIMEOUT)
    }

    fn put(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        self.put(path, body, REQUEST_TIMEOUT)
    }

    fn delete(&self, path: &str) -> anyhow::Result<Value> {
        self.delete(path, REQUEST_TIMEOUT)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeploymentSummary {
    pub group_id: i64,
    pub api_channels: usize,
    pub oauth_accounts: usize,
    pub visible_models: usize,
    pub composite_routes: usize,
}

/// Reconcile only the live backend routing state after a quota or recovery
/// observation changes. This deliberately excludes lifecycle management,
/// credential writes, Codex config/catalog file writes, and process restarts.
/// It is safe to run while an active Codex task is streaming through Router.
pub fn sync_routing_only(router_root: &Path, cfg: &RouterConfig) -> anyhow::Result<()> {
    let admin = usage::retry_admin_read(|| AdminClient::connect(router_root, cfg))?;
    sync_routing_only_with_admin(&admin, cfg)
}

fn sync_routing_only_with_admin(admin: &impl AdminApi, cfg: &RouterConfig) -> anyhow::Result<()> {
    let groups = usage::data(AdminApi::get(
        admin,
        "/api/v1/admin/groups/all?include_inactive=true",
    )?);
    let group_id = usage::array(&groups)
        .iter()
        .find(|group| usage::string(group, "name") == MANAGED_GROUP_NAME)
        .map(|group| usage::integer(group, "id"))
        .filter(|id| *id > 0)
        .context("ROUTER_ROUTING_SYNC_GROUP_MISSING")?;
    let route_plan = build_route_plan(cfg);
    let accounts = list_accounts(admin)?;
    let selected = selected_oauth_ids(cfg);
    let mut isolated = HashSet::new();
    for account in &accounts {
        if usage::string(account, "type") != "oauth" {
            continue;
        }
        let id = usage::integer(account, "id");
        if id <= 0 {
            continue;
        }
        let detail = usage::data(AdminApi::get(
            admin,
            &format!("/api/v1/admin/accounts/{id}"),
        )?);
        let wanted = selected.contains(&id);
        let should_isolate = wanted && oauth_should_isolate(&detail);
        if should_isolate {
            isolated.insert(id);
        }
        let mut groups = group_ids(&detail, Some(account));
        let before = groups.clone();
        groups.retain(|candidate| *candidate != group_id);
        if wanted && !should_isolate {
            groups.push(group_id);
        }
        groups.sort_unstable();
        groups.dedup();
        if groups != before {
            AdminApi::put(
                admin,
                &format!("/api/v1/admin/accounts/{id}"),
                &json!({
                    "group_ids": groups,
                    "confirm_mixed_channel_risk": true,
                }),
            )?;
        }
        if !wanted
            && usage::get(&detail, "schedulable").and_then(Value::as_bool) != Some(false)
        {
            AdminApi::post(
                admin,
                &format!("/api/v1/admin/accounts/{id}/schedulable"),
                Some(&json!({ "schedulable": false })),
            )?;
        }
    }
    let visible_models = visible_public_models(&route_plan, &isolated, cfg);
    if visible_models.is_empty() {
        bail!("ROUTER_ROUTING_SYNC_NO_SERVABLE_MODEL")
    }
    ensure_group(admin, &visible_models)?;
    let platform_by_id = account_platforms(&accounts);
    let servable = servable_routes(&route_plan, &isolated, cfg);
    let composite = build_composite_routes(&servable, &platform_by_id, &isolated);
    sync_composite_routes(admin, group_id, &composite)
        .context("ROUTER_ROUTING_SYNC_COMPOSITE_FAILED")?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CompositeRoute {
    public_model: String,
    upstream_model: String,
    target_platform: String,
    priority: i32,
}

#[derive(Clone, Copy, Debug, Default)]
struct ManagedProxy {
    managed_id: i64,
    desired_id: i64,
}

pub fn apply_native<F>(
    router_root: &Path,
    cfg: &RouterConfig,
    proxy_runtime: &ProxyRuntime,
    cancel: &AtomicBool,
    mut on_log: F,
) -> anyhow::Result<DeploymentSummary>
where
    F: FnMut(String),
{
    check_cancel(cancel)?;
    on_log("[2/7] Starting Router Host and CLIProxyAPI...".to_owned());
    crate::lifecycle::ensure_services(router_root, true, cancel, true)
        .context("ROUTER_DEPLOY_NATIVE_LIFECYCLE_FAILED")?;
    on_log("CR-FLAG STAGE-02-SERVICES-OK".to_owned());

    check_cancel(cancel)?;
    on_log("[3/7] Local services are ready; signing in to the admin API...".to_owned());
    let admin = usage::retry_admin_read(|| AdminClient::connect(router_root, cfg))?;
    accept_compliance(&admin, cfg)?;
    ensure_local_administrator(&admin)?;
    let admin = usage::retry_admin_read(|| AdminClient::connect(router_root, cfg))?;
    on_log("CR-FLAG STAGE-03-ADMIN-OK".to_owned());

    check_cancel(cancel)?;
    on_log("[4/7] Checking Router compliance status...".to_owned());
    let summary = sync_router_state(&admin, router_root, cfg, proxy_runtime, cancel, &mut on_log)
        .map_err(|error| {
            on_log(format!("deployment_diagnostic {error:#}"));
            error
        })?;
    on_log("CR-FLAG STAGE-06-CODEX-OK".to_owned());
    on_log("[7/7] Deployment complete.".to_owned());
    on_log("CR-FLAG STAGE-07-DONE".to_owned());
    Ok(summary)
}

fn check_cancel(cancel: &AtomicBool) -> anyhow::Result<()> {
    if cancel.load(Ordering::Acquire) {
        bail!("class=cancelled")
    }
    Ok(())
}

fn accept_compliance(admin: &impl AdminApi, cfg: &RouterConfig) -> anyhow::Result<()> {
    let compliance = usage::data(admin.get("/api/v1/admin/compliance")?);
    if usage::get(&compliance, "required").and_then(Value::as_bool) != Some(true) {
        return Ok(());
    }
    if !cfg.accept_compliance {
        bail!("ROUTER_DEPLOY_COMPLIANCE_REQUIRED: accept the local deployment commitment first")
    }
    let phrase = usage::string(&compliance, "ack_phrase_zh");
    if phrase.trim().is_empty() {
        bail!("ROUTER_DEPLOY_COMPLIANCE_ACCEPT_FAILED: CR-VAL-0003 empty ack phrase")
    }
    admin
        .post(
            "/api/v1/admin/compliance/accept",
            Some(&json!({ "phrase": phrase, "language": "zh" })),
        )
        .context("ROUTER_DEPLOY_COMPLIANCE_ACCEPT_FAILED: CR-VAL-0003")?;
    Ok(())
}

fn ensure_local_administrator(admin: &impl AdminApi) -> anyhow::Result<()> {
    let user = usage::data(admin.get("/api/v1/admin/users/1")?);
    let password = super::read_router_credential_text("AdminPassword")?
        .context("ROUTER_DEPLOY_ADMIN_PASSWORD_MISSING")?;
    let body = json!({
        "email": "admin@admin.com",
        "password": password.as_str(),
        "username": usage::string(&user, "username"),
        "notes": usage::string(&user, "notes"),
        "role": usage::string(&user, "role"),
        "concurrency": usage::integer(&user, "concurrency"),
        "rpm_limit": usage::integer(&user, "rpm_limit"),
    });
    let _ = admin.put(
        "/api/v1/admin/settings",
        &json!({ "site_subtitle": "仅限 127.0.0.1；登录凭据由 Codex-Router 安全管理" }),
    );
    admin.put("/api/v1/admin/users/1", &body)?;
    Ok(())
}

fn sync_router_state<F>(
    admin: &impl AdminApi,
    router_root: &Path,
    cfg: &RouterConfig,
    proxy_runtime: &ProxyRuntime,
    cancel: &AtomicBool,
    on_log: &mut F,
) -> anyhow::Result<DeploymentSummary>
where
    F: FnMut(String),
{
    let route_plan = build_route_plan(cfg);
    let initial_models = visible_public_models(&route_plan, &HashSet::new(), cfg);
    if initial_models.is_empty() {
        bail!("ROUTER_DEPLOY_NO_SERVABLE_MODEL: no routed model is available")
    }
    let mut accounts = list_accounts(admin).context("ROUTER_DEPLOY_ADMIN_ACCOUNTS_FAILED")?;
    let group_id =
        ensure_group(admin, &initial_models).context("ROUTER_DEPLOY_GROUP_SYNC_FAILED")?;
    let proxy = match sync_managed_proxy(admin, proxy_runtime) {
        Ok(proxy) => proxy,
        Err(error) => {
            on_log(format!("deployment_diagnostic {error:#}"));
            ManagedProxy::default()
        }
    };
    let managed_names = sync_api_channels(
        admin,
        router_root,
        cfg,
        &route_plan,
        group_id,
        proxy,
        proxy_runtime,
        &mut accounts,
        cancel,
        on_log,
    )
    .context("ROUTER_DEPLOY_API_CHANNELS_FAILED")?;
    check_cancel(cancel)?;
    accounts = list_accounts(admin).context("ROUTER_DEPLOY_ADMIN_ACCOUNTS_FAILED")?;
    let isolated = sync_oauth_and_stale_accounts(
        admin,
        cfg,
        &route_plan,
        group_id,
        proxy,
        proxy_runtime,
        &managed_names,
        &accounts,
        cancel,
    )
    .context("ROUTER_DEPLOY_OAUTH_SYNC_FAILED")?;
    let servable = servable_routes(&route_plan, &isolated, cfg);
    let visible_models = servable
        .iter()
        .filter(|route| route.include_in_catalog)
        .map(|route| route.public_model_id.clone())
        .collect::<Vec<_>>();
    if visible_models.is_empty() {
        bail!("ROUTER_DEPLOY_NO_SERVABLE_MODEL: no model can currently be served")
    }
    ensure_group(admin, &visible_models).context("ROUTER_DEPLOY_GROUP_SYNC_FAILED")?;
    let platform_by_id = account_platforms(&accounts);
    let composite = build_composite_routes(&servable, &platform_by_id, &isolated);
    sync_composite_routes(admin, group_id, &composite)
        .context("ROUTER_DEPLOY_COMPOSITE_FAILED")?;
    disable_overlapping_recovery_plans(admin, cfg);
    ensure_local_api_key(admin, group_id).context("ROUTER_DEPLOY_LOCAL_KEY_FAILED")?;
    super::codex_toml::write_codex_config_from_router_config(cfg, router_root)
        .context("ROUTER_DEPLOY_CODEX_CONFIG_FAILED")?;
    on_log(format!(
        "Configured {} model channel(s); visible models={}; composite routes={}",
        cfg.models.len(),
        visible_models.len(),
        composite.len()
    ));
    Ok(DeploymentSummary {
        group_id,
        api_channels: cfg
            .models
            .iter()
            .filter(|model| model.source != "oauth")
            .count(),
        oauth_accounts: selected_oauth_ids(cfg).len(),
        visible_models: visible_models.len(),
        composite_routes: composite.len(),
    })
}

fn list_accounts(admin: &impl AdminApi) -> anyhow::Result<Vec<Value>> {
    let body = usage::data(admin.get("/api/v1/admin/accounts?page=1&page_size=200")?);
    Ok(usage::array(usage::get(&body, "items").unwrap_or(&body)).to_vec())
}

fn ensure_group(admin: &impl AdminApi, models: &[String]) -> anyhow::Result<i64> {
    let body = usage::data(admin.get("/api/v1/admin/groups/all?include_inactive=true")?);
    let existing = usage::array(&body)
        .iter()
        .find(|group| usage::string(group, "name") == MANAGED_GROUP_NAME);
    let models = models.to_vec();
    let group_body = json!({
        "name": MANAGED_GROUP_NAME,
        "description": "Single-user local Codex multi-model router managed by Codex-Router.",
        "platform": "composite",
        "rate_multiplier": 1.0,
        "is_exclusive": false,
        "subscription_type": "standard",
        "status": "active",
        "allow_messages_dispatch": false,
        "allow_live": false,
        "require_oauth_only": false,
        "models_list_config": { "enabled": true, "models": models },
    });
    let response = if let Some(existing) = existing {
        let id = usage::integer(existing, "id");
        if id <= 0 {
            bail!("ROUTER_DEPLOY_GROUP_INVALID: existing Codex-Router group has no id")
        }
        match admin.put(&format!("/api/v1/admin/groups/{id}"), &group_body) {
            Ok(response) => response,
            Err(_) => return Ok(id),
        }
    } else {
        match admin.post("/api/v1/admin/groups", Some(&group_body)) {
            Ok(response) => response,
            Err(error) => {
                let body = usage::data(
                    admin
                        .get("/api/v1/admin/groups/all?include_inactive=true")
                        .context(format!(
                            "ROUTER_DEPLOY_GROUP_SYNC_FAILED: create failed and reload failed: {error}"
                        ))?,
                );
                if let Some(created) = usage::array(&body)
                    .iter()
                    .find(|group| usage::string(group, "name") == MANAGED_GROUP_NAME)
                {
                    let id = usage::integer(created, "id");
                    if id > 0 {
                        return Ok(id);
                    }
                }
                return Err(error).context("ROUTER_DEPLOY_GROUP_SYNC_FAILED: create group failed");
            }
        }
    };
    let response = usage::data(response);
    let id = usage::integer(&response, "id").max(
        existing
            .map(|group| usage::integer(group, "id"))
            .unwrap_or_default(),
    );
    if id <= 0 {
        bail!("ROUTER_DEPLOY_GROUP_INVALID: Router Host did not return a group id")
    }
    Ok(id)
}

#[allow(clippy::too_many_arguments)]
fn sync_api_channels<F>(
    admin: &impl AdminApi,
    router_root: &Path,
    cfg: &RouterConfig,
    routes: &[ModelRoute],
    group_id: i64,
    managed_proxy: ManagedProxy,
    proxy_runtime: &ProxyRuntime,
    accounts: &mut Vec<Value>,
    cancel: &AtomicBool,
    on_log: &mut F,
) -> anyhow::Result<HashSet<String>>
where
    F: FnMut(String),
{
    let mut managed_names = HashSet::new();
    for (model, route) in cfg.models.iter().zip(routes) {
        check_cancel(cancel)?;
        if model.source == "oauth" {
            continue;
        }
        if model.credential_name.trim().is_empty() {
            bail!(
                "ROUTER_DEPLOY_CREDENTIAL_REFERENCE_MISSING: model '{}'",
                model.model
            )
        }
        let api_key = super::read_router_credential_text(&model.credential_name)?
            .with_context(|| format!("Missing API Key for model '{}'", model.model))?;
        let account_name = api_account_name(model);
        managed_names.insert(account_name.clone());
        let existing = accounts
            .iter()
            .find(|account| usage::string(account, "name") == account_name)
            .cloned();
        let detail = if let Some(existing) = &existing {
            usage::data(admin.get(&format!(
                "/api/v1/admin/accounts/{}",
                usage::integer(existing, "id")
            ))?)
        } else {
            Value::Null
        };
        let mut group_ids = group_ids(&detail, existing.as_ref());
        group_ids.retain(|id| *id != group_id);
        if route.join_router {
            group_ids.push(group_id);
        }
        group_ids.sort_unstable();
        group_ids.dedup();
        let upstream = upstream_model_id(&model.model, &model.base_url);
        let mut mapping = Map::new();
        for request_id in &route.request_model_ids {
            mapping.insert(request_id.clone(), Value::String(upstream.clone()));
        }
        mapping
            .entry(model.model.clone())
            .or_insert_with(|| Value::String(upstream));
        let mut extra = parse_extra(&model.extra)?;
        apply_openai_channel_policy(model, &mut extra);
        extra.insert(
            "codex_router_oauth_fallback".to_owned(),
            Value::Bool(route.is_oauth_fallback),
        );
        let target = model.base_url.trim().trim_end_matches('/');
        let target_policy = proxy_runtime.targets.get(target);
        extra.insert(
            "proxy_direct_fallback".to_owned(),
            Value::Bool(target_policy.is_some_and(|policy| policy.direct_fallback)),
        );
        let minimum_priority = cfg
            .models
            .iter()
            .filter(|candidate| {
                candidate.source != "oauth"
                    && super::same_model_identity(&candidate.model, &model.model)
                    && cfg.models.iter().any(|oauth| {
                        oauth.source == "oauth"
                            && super::same_model_identity(&oauth.model, &model.model)
                            && super::is_eligible_oauth_api_fallback(cfg, oauth, candidate)
                    })
            })
            .map(|candidate| candidate.priority.max(1))
            .min()
            .unwrap_or(model.priority.max(1));
        let mut priority = super::display_priority(model.priority);
        if route.is_oauth_fallback {
            let priorities = super::model_oauth_routing_priorities(cfg, &model.model);
            priority = super::display_priority(super::effective_api_priority(
                priority,
                super::display_priority(minimum_priority),
                super::display_priority(priorities.api_priority),
                super::display_priority(priorities.oauth_priority),
                priorities.prefer_oauth,
            ));
        }
        let mut credentials = json!({
            "base_url": target,
            "api_key": api_key.as_str(),
            "model_mapping": Value::Object(mapping),
        });
        if extra.get("openai_responses_mode").and_then(Value::as_str)
            == Some("force_chat_completions")
        {
            credentials["openai_capabilities"] = json!(["chat_completions"]);
        }
        let current_proxy = usage::integer(&detail, "proxy_id");
        let desired_proxy = reconcile_proxy_id(
            current_proxy,
            managed_proxy,
            route.join_router
                && !target_policy.is_some_and(|policy| policy.bypass)
                && !official_direct_api_host(&model.base_url),
        );
        let concurrency = api_account_concurrency(model);
        let mut body = json!({
            "name": account_name,
            "platform": "openai",
            "type": "apikey",
            "credentials": credentials,
            "extra": extra,
            "concurrency": concurrency,
            "priority": priority,
            "rate_multiplier": 1.0,
            "group_ids": group_ids,
            "confirm_mixed_channel_risk": true,
        });
        if let Some(proxy_id) = desired_proxy {
            body["proxy_id"] = proxy_id.into();
        }
        if let Some(existing) = existing {
            let id = usage::integer(&existing, "id");
            admin.put(&format!("/api/v1/admin/accounts/{id}"), &body)?;
            if usage::string(&existing, "status") == "error"
                || usage::get(&existing, "schedulable").and_then(Value::as_bool) == Some(false)
            {
                let _ = admin.post(&format!("/api/v1/admin/accounts/{id}/clear-error"), None);
                let _ = admin.post(
                    &format!("/api/v1/admin/accounts/{id}/schedulable"),
                    Some(&json!({ "schedulable": true })),
                );
            }
            on_log(format!(
                "Updated channel: {}",
                body["name"].as_str().unwrap_or_default()
            ));
        } else {
            let created = usage::data(admin.post("/api/v1/admin/accounts", Some(&body))?);
            accounts.push(created);
            on_log(format!(
                "Created channel: {}",
                body["name"].as_str().unwrap_or_default()
            ));
        }
    }
    let _ = router_root;
    Ok(managed_names)
}

fn api_account_concurrency(model: &ModelConfig) -> i64 {
    if super::is_volcengine_plan_url(&model.base_url) {
        2
    } else {
        8
    }
}

#[allow(clippy::too_many_arguments)]
fn sync_oauth_and_stale_accounts(
    admin: &impl AdminApi,
    cfg: &RouterConfig,
    routes: &[ModelRoute],
    group_id: i64,
    managed_proxy: ManagedProxy,
    proxy_runtime: &ProxyRuntime,
    managed_names: &HashSet<String>,
    accounts: &[Value],
    cancel: &AtomicBool,
) -> anyhow::Result<HashSet<i64>> {
    let selected = selected_oauth_ids(cfg);
    let oauth_model_mappings = selected_oauth_model_mappings(routes);
    let default_priority =
        super::oauth_routing_priorities(Some(&cfg.oauth_fallback)).oauth_priority;
    let mut isolated = HashSet::new();
    for account in accounts {
        check_cancel(cancel)?;
        let id = usage::integer(account, "id");
        if id <= 0 {
            continue;
        }
        let detail = usage::data(admin.get(&format!("/api/v1/admin/accounts/{id}"))?);
        let kind = usage::string(&detail, "type");
        let mut groups = group_ids(&detail, Some(account));
        let currently_managed = groups.contains(&group_id);
        if kind == "oauth" {
            let wanted = selected.contains(&id);
            let should_isolate = wanted && oauth_should_isolate(&detail);
            if should_isolate {
                isolated.insert(id);
            }
            groups.retain(|candidate| *candidate != group_id);
            if wanted && !should_isolate {
                groups.push(group_id);
            }
            groups.sort_unstable();
            groups.dedup();
            let configured_priority = cfg
                .models
                .iter()
                .filter(|model| model.source == "oauth" && model.oauth_account_id == id)
                .map(|model| super::display_priority(model.priority))
                .min();
            let existing_priority =
                super::display_priority(usage::integer(&detail, "priority") as i32);
            let priority = configured_priority.unwrap_or(existing_priority.max(default_priority.min(999)));
            let current_proxy = usage::integer(&detail, "proxy_id");
            let desired_proxy = reconcile_proxy_id(
                current_proxy,
                managed_proxy,
                wanted && !proxy_runtime.settings.proxy_url.is_none(),
            );
            let mut body = json!({
                "priority": priority,
                "group_ids": groups,
                "confirm_mixed_channel_risk": true,
            });
            if wanted {
                if let Some(mapping) = oauth_model_mappings.get(&id) {
                    let mut credentials = usage::get(&detail, "credentials")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    credentials.insert("model_mapping".to_owned(), json!(mapping));
                    body["credentials"] = Value::Object(credentials);
                }
                let platform = usage::string(&detail, "platform").to_ascii_lowercase();
                if matches!(
                    platform.as_str(),
                    "grok" | "xai" | "x-ai" | "antigravity" | "gemini" | "claude" | "anthropic"
                ) {
                    let mut extra = usage::get(&detail, "extra")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    extra.insert(
                        "openai_compact_supported".to_owned(),
                        Value::Bool(matches!(platform.as_str(), "grok" | "xai" | "x-ai")),
                    );
                    body["extra"] = Value::Object(extra);
                }
            }
            if let Some(proxy_id) = desired_proxy {
                body["proxy_id"] = proxy_id.into();
            }
            admin.put(&format!("/api/v1/admin/accounts/{id}"), &body)?;
            if !wanted
                && usage::get(&detail, "schedulable").and_then(Value::as_bool) != Some(false)
            {
                AdminApi::post(
                    admin,
                    &format!("/api/v1/admin/accounts/{id}/schedulable"),
                    Some(&json!({ "schedulable": false })),
                )?;
            }
        } else if currently_managed
            && usage::string(&detail, "name").starts_with("Codex-Router / ")
            && !managed_names.contains(&usage::string(&detail, "name"))
        {
            groups.retain(|candidate| *candidate != group_id);
            admin.put(
                &format!("/api/v1/admin/accounts/{id}"),
                &json!({ "group_ids": groups }),
            )?;
        }
    }
    Ok(isolated)
}

fn selected_oauth_model_mappings(routes: &[ModelRoute]) -> HashMap<i64, BTreeMap<String, String>> {
    let mut mappings = HashMap::<i64, BTreeMap<String, String>>::new();
    for route in routes.iter().filter(|route| {
        route.source == "oauth"
            && route.model.oauth_account_id > 0
            && !route.model.model.trim().is_empty()
    }) {
        let model = route.model.model.trim().to_owned();
        let upstream = oauth_upstream_model_id(&route.model.oauth_platform, &model);
        mappings
            .entry(route.model.oauth_account_id)
            .or_default()
            .insert(model, upstream);
    }
    mappings
}

fn oauth_upstream_model_id(platform: &str, requested: &str) -> String {
    if platform.eq_ignore_ascii_case("antigravity") && requested == "gemini-3.7-flash" {
        return "gemini-3.7-flash-medium".to_owned();
    }
    requested.to_owned()
}

fn selected_oauth_ids(cfg: &RouterConfig) -> HashSet<i64> {
    let mut ids = cfg
        .oauth_account_ids
        .clone()
        .unwrap_or_else(|| {
            cfg.models
                .iter()
                .filter(|model| model.source == "oauth")
                .map(|model| model.oauth_account_id)
                .collect()
        })
        .into_iter()
        .filter(|id| *id > 0)
        .collect::<HashSet<_>>();
    ids.extend(
        cfg.models
            .iter()
            .filter(|model| model.source == "oauth" && cfg.oauth_account_ids.is_none())
            .map(|model| model.oauth_account_id)
            .filter(|id| *id > 0),
    );
    ids
}

fn oauth_should_isolate(account: &Value) -> bool {
    let status = usage::string(account, "status").to_ascii_lowercase();
    let schedulable = usage::get(account, "schedulable").and_then(Value::as_bool);
    let reason = format!(
        "{} {}",
        usage::string(account, "error_message"),
        usage::string(account, "temp_unschedulable_reason")
    )
    .to_ascii_lowercase();
    status == "error"
        || schedulable == Some(false)
        || [
            "quota",
            "rate_limit",
            "rate limit",
            "usage limit",
            "billing cycle",
            "exhausted",
            "限额",
            "额度",
        ]
        .iter()
        .any(|needle| reason.contains(needle))
}

fn servable_routes(
    routes: &[ModelRoute],
    isolated: &HashSet<i64>,
    cfg: &RouterConfig,
) -> Vec<ModelRoute> {
    let selected = selected_oauth_ids(cfg);
    let joined_api = routes
        .iter()
        .filter(|route| route.source != "oauth" && route.join_router)
        .map(|route| route.public_model_id.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut visible_ids = HashSet::new();
    let mut visible = Vec::new();
    for route in routes.iter().filter(|route| route.include_in_catalog) {
        let mut route = route.clone();
        if route.source == "oauth" {
            let id = route.model.oauth_account_id;
            if !selected.contains(&id) {
                continue;
            }
            if isolated.contains(&id) {
                if !joined_api.contains(&route.public_model_id.to_ascii_lowercase()) {
                    continue;
                }
                route.served_by = "api".to_owned();
            }
        }
        if visible_ids.insert(route.public_model_id.to_ascii_lowercase()) {
            visible.push(route);
        }
    }
    // The catalog keeps only one row for duplicate OAuth accounts that expose
    // the same public model. If that representative account is isolated, a
    // healthy duplicate must take over the row; otherwise all accounts on that
    // platform become unreachable even though one is still schedulable.
    for route in routes.iter().filter(|route| {
        route.source == "oauth"
            && !route.include_in_catalog
            && route.join_router
            && selected.contains(&route.model.oauth_account_id)
            && !isolated.contains(&route.model.oauth_account_id)
    }) {
        let public_id = route.public_model_id.to_ascii_lowercase();
        if visible_ids.contains(&public_id)
            || !routes.iter().any(|candidate| {
                candidate.include_in_catalog
                    && candidate
                        .public_model_id
                        .eq_ignore_ascii_case(&route.public_model_id)
            })
        {
            continue;
        }
        let mut promoted = route.clone();
        promoted.include_in_catalog = true;
        promoted.served_by = "oauth".to_owned();
        visible_ids.insert(public_id);
        visible.push(promoted);
    }
    let served_ids = visible
        .iter()
        .map(|route| route.public_model_id.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    for route in routes {
        if route.include_in_catalog
            || !route.join_router
            || !served_ids.contains(&route.public_model_id.to_ascii_lowercase())
            || (route.source == "oauth" && isolated.contains(&route.model.oauth_account_id))
        {
            continue;
        }
        visible.push(route.clone());
    }
    visible
}

fn visible_public_models(
    routes: &[ModelRoute],
    isolated: &HashSet<i64>,
    cfg: &RouterConfig,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut ordered = Vec::new();
    for route in servable_routes(routes, isolated, cfg) {
        if route.include_in_catalog && seen.insert(route.public_model_id.clone()) {
            ordered.push(route.public_model_id);
        }
    }
    ordered
}

fn account_platforms(accounts: &[Value]) -> HashMap<i64, String> {
    accounts
        .iter()
        .filter_map(|account| {
            let id = usage::integer(account, "id");
            let platform = usage::string(account, "platform").to_ascii_lowercase();
            (id > 0 && !platform.is_empty()).then_some((id, platform))
        })
        .collect()
}

fn build_composite_routes(
    routes: &[ModelRoute],
    account_platforms: &HashMap<i64, String>,
    isolated: &HashSet<i64>,
) -> Vec<CompositeRoute> {
    let mut by_target = BTreeMap::<String, CompositeRoute>::new();
    for route in routes {
        if !route.include_in_catalog && !route.join_router {
            continue;
        }
        if route.source == "oauth" && isolated.contains(&route.model.oauth_account_id) {
            continue;
        }
        let platform = composite_platform(&route.model, account_platforms);
        let priority = if route.source == "oauth" { 1 } else { 100 };
        let key = format!(
            "{}|{}",
            route.public_model_id.to_ascii_lowercase(),
            platform
        );
        let candidate = CompositeRoute {
            public_model: route.public_model_id.clone(),
            upstream_model: route.model.model.clone(),
            target_platform: platform,
            priority,
        };
        if by_target
            .get(&key)
            .is_none_or(|existing| candidate.priority < existing.priority)
        {
            by_target.insert(key, candidate);
        }
    }
    by_target.into_values().collect()
}

fn composite_platform(model: &ModelConfig, account_platforms: &HashMap<i64, String>) -> String {
    if model.source != "oauth" {
        return "openai".to_owned();
    }
    let platform = if !model.oauth_platform.trim().is_empty() {
        model.oauth_platform.trim().to_ascii_lowercase()
    } else {
        account_platforms
            .get(&model.oauth_account_id)
            .cloned()
            .unwrap_or_else(|| "openai".to_owned())
    };
    match platform.as_str() {
        "google_one" | "gemini" => "gemini".to_owned(),
        _ => platform,
    }
}

fn sync_composite_routes(
    admin: &impl AdminApi,
    group_id: i64,
    desired: &[CompositeRoute],
) -> anyhow::Result<()> {
    let path = format!("/api/v1/admin/groups/{group_id}/composite-routes");
    let existing_body = usage::data(admin.get(&path).unwrap_or(Value::Null));
    let existing = usage::array(usage::get(&existing_body, "items").unwrap_or(&existing_body));
    let mut existing_by_key = HashMap::<String, Vec<Value>>::new();
    for route in existing {
        let key = format!(
            "{}|{}",
            usage::string(route, "public_model").to_ascii_lowercase(),
            usage::string(route, "target_platform").to_ascii_lowercase()
        );
        existing_by_key.entry(key).or_default().push(route.clone());
    }
    let desired_keys = desired
        .iter()
        .map(|route| {
            format!(
                "{}|{}",
                route.public_model.to_ascii_lowercase(),
                route.target_platform
            )
        })
        .collect::<HashSet<_>>();
    for route in desired {
        let key = format!(
            "{}|{}",
            route.public_model.to_ascii_lowercase(),
            route.target_platform
        );
        let body = json!({
            "public_model": route.public_model,
            "upstream_model": route.upstream_model,
            "target_platform": route.target_platform,
            "match_type": "exact",
            "endpoint": "any",
            "priority": route.priority,
            "enabled": true,
        });
        let matches = existing_by_key.get(&key).cloned().unwrap_or_default();
        if let Some(primary) = matches.first() {
            let id = usage::integer(primary, "id");
            admin.put(&format!("{path}/{id}"), &body)?;
            for duplicate in matches.iter().skip(1) {
                let duplicate_id = usage::integer(duplicate, "id");
                let _ = admin.delete(&format!("{path}/{duplicate_id}"));
            }
        } else {
            admin.post(&path, Some(&body))?;
        }
    }
    for (key, routes) in existing_by_key {
        if desired_keys.contains(&key) {
            continue;
        }
        for route in routes {
            let id = usage::integer(&route, "id");
            let _ = admin.delete(&format!("{path}/{id}"));
        }
    }
    Ok(())
}

fn sync_managed_proxy(
    admin: &impl AdminApi,
    runtime: &ProxyRuntime,
) -> anyhow::Result<ManagedProxy> {
    if runtime.settings.mode == "unsupported" {
        bail!("ROUTER_PROXY_UNSUPPORTED: {}", runtime.settings.diagnostic)
    }
    let body = usage::data(admin.get("/api/v1/admin/proxies?page=1&page_size=200")?);
    let items = usage::array(usage::get(&body, "items").unwrap_or(&body));
    let Some(proxy_url) = runtime.settings.proxy_url.as_deref() else {
        let existing_id = items
            .iter()
            .find(|proxy| usage::string(proxy, "name") == MANAGED_PROXY_NAME)
            .map(|proxy| usage::integer(proxy, "id"))
            .unwrap_or_default();
        return Ok(ManagedProxy {
            managed_id: existing_id,
            desired_id: 0,
        });
    };
    if runtime.settings.has_credentials {
        bail!("ROUTER_PROXY_CREDENTIAL_STORAGE_UNSUPPORTED")
    }
    let url = Url::parse(proxy_url).context("ROUTER_PROXY_UNSUPPORTED: invalid proxy URL")?;
    let existing_id = items
        .iter()
        .find(|proxy| proxy_record_matches(proxy, &url))
        .map(|proxy| usage::integer(proxy, "id"))
        .unwrap_or_default();
    let proxy_body = json!({
        "name": MANAGED_PROXY_NAME,
        "protocol": url.scheme(),
        "host": url.host_str().unwrap_or_default(),
        "port": url.port_or_known_default().unwrap_or_default(),
        "fallback_mode": "none",
        "expiry_warn_days": 0,
    });
    let id = if existing_id > 0 {
        let _ = admin.put(&format!("/api/v1/admin/proxies/{existing_id}"), &proxy_body);
        existing_id
    } else {
        match admin.post("/api/v1/admin/proxies", Some(&proxy_body)) {
            Ok(created) => usage::integer(&usage::data(created), "id"),
            Err(error) => {
                let text = error.to_string();
                if text.contains("CR-PRX-0001") || text.contains("http=409") {
                    let body = usage::data(
                        admin.get("/api/v1/admin/proxies?page=1&page_size=200")?,
                    );
                    usage::array(usage::get(&body, "items").unwrap_or(&body))
                        .iter()
                        .find(|proxy| proxy_record_matches(proxy, &url))
                        .map(|proxy| usage::integer(proxy, "id"))
                        .unwrap_or_default()
                } else {
                    return Err(error);
                }
            }
        }
    };
    if id <= 0 {
        return Ok(ManagedProxy::default());
    }
    Ok(ManagedProxy {
        managed_id: id,
        desired_id: id,
    })
}

fn proxy_record_matches(proxy: &Value, url: &Url) -> bool {
    if usage::string(proxy, "name") == MANAGED_PROXY_NAME {
        return true;
    }
    let host = usage::string(proxy, "host");
    let protocol = usage::string(proxy, "protocol");
    let port = usage::integer(proxy, "port");
    let wanted_host = url.host_str().unwrap_or_default();
    let wanted_port = i64::from(url.port_or_known_default().unwrap_or_default());
    let wanted_url = format!("{}://{wanted_host}:{wanted_port}", url.scheme());
    (host.eq_ignore_ascii_case(wanted_host)
        && port == wanted_port
        && (protocol.is_empty() || protocol.eq_ignore_ascii_case(url.scheme())))
        || usage::string(proxy, "normalized_url").eq_ignore_ascii_case(&wanted_url)
}

fn reconcile_proxy_id(current: i64, managed: ManagedProxy, should_use: bool) -> Option<i64> {
    let current_is_managed = current > 0 && current == managed.managed_id;
    if should_use {
        if current == 0 || current_is_managed {
            return (current != managed.desired_id).then_some(managed.desired_id);
        }
        return None;
    }
    current_is_managed.then_some(0)
}

fn ensure_local_api_key(admin: &impl AdminApi, group_id: i64) -> anyhow::Result<()> {
    let key = super::ensure_local_api_key()?;
    sync_local_api_key(admin, group_id, &key)
}

fn key_group_id(item: &Value) -> i64 {
    usage::integer(item, "group_id")
        .max(usage::integer(item, "groupId"))
        .max(
            usage::get(item, "group")
                .map(|group| usage::integer(group, "id"))
                .unwrap_or_default(),
        )
}

fn key_value_is_redacted(value: &str) -> bool {
    let value = value.trim();
    value.is_empty() || value.contains('*') || value.contains("...") || value.contains('\u{2026}')
}

fn sync_local_api_key(admin: &impl AdminApi, group_id: i64, key: &str) -> anyhow::Result<()> {
    let response = usage::data(admin.get("/api/v1/keys?page=1&page_size=200")?);
    let items = usage::array(usage::get(&response, "items").unwrap_or(&response));
    if items.iter().any(|item| {
        let returned_key = usage::string(item, "key");
        returned_key == key
            || (usage::string(item, "name") == MANAGED_GROUP_NAME
                && key_group_id(item) == group_id
                && key_value_is_redacted(&returned_key))
    }) {
        return Ok(());
    }
    admin.post(
        "/api/v1/keys",
        Some(&json!({
            "name": MANAGED_GROUP_NAME,
            "group_id": group_id,
            "custom_key": key,
            "quota": 0,
        })),
    )?;
    Ok(())
}

fn disable_overlapping_recovery_plans(admin: &impl AdminApi, cfg: &RouterConfig) {
    for account_id in selected_oauth_ids(cfg) {
        let path = format!("/api/v1/admin/accounts/{account_id}/scheduled-test-plans");
        let Ok(body) = admin.get(&path).map(usage::data) else {
            continue;
        };
        for plan in usage::array(usage::get(&body, "items").unwrap_or(&body)) {
            if usage::get(plan, "enabled").and_then(Value::as_bool) == Some(false) {
                continue;
            }
            let id = usage::integer(plan, "id");
            if id <= 0 {
                continue;
            }
            let mut updated = plan.clone();
            updated["enabled"] = Value::Bool(false);
            let _ = admin.put(
                &format!("/api/v1/admin/scheduled-test-plans/{id}"),
                &updated,
            );
        }
    }
}

fn group_ids(detail: &Value, summary: Option<&Value>) -> Vec<i64> {
    let source = usage::get(detail, "group_ids")
        .or_else(|| summary.and_then(|value| usage::get(value, "group_ids")))
        .unwrap_or(&Value::Null);
    usage::array(source)
        .iter()
        .filter_map(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
        .filter(|id| *id > 0)
        .collect()
}

fn parse_extra(raw: &str) -> anyhow::Result<Map<String, Value>> {
    if raw.trim().is_empty() {
        return Ok(Map::new());
    }
    serde_json::from_str::<Value>(raw)
        .context("ROUTER_DEPLOY_MODEL_EXTRA_INVALID")?
        .as_object()
        .cloned()
        .context("ROUTER_DEPLOY_MODEL_EXTRA_INVALID: expected an object")
}

fn official_direct_api_host(base_url: &str) -> bool {
    matches!(
        Url::parse(base_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default()
            .as_str(),
        "api.deepseek.com"
            | "api.kimi.com"
            | "api.moonshot.ai"
            | "api.moonshot.cn"
            | "ark.cn-beijing.volces.com"
    )
}

fn apply_openai_channel_policy(model: &ModelConfig, extra: &mut Map<String, Value>) {
    let profile = super::classify_channel_route(model);
    extra.insert(
        "codex_router_vendor".to_owned(),
        Value::String(profile.vendor.clone()),
    );
    extra.insert(
        "codex_router_source_type".to_owned(),
        Value::String(profile.source_type.as_str().to_owned()),
    );
    extra.insert(
        "codex_router_gateway".to_owned(),
        Value::String(profile.gateway.as_str().to_owned()),
    );
    extra.insert(
        "codex_router_upstream_protocol".to_owned(),
        Value::String(profile.upstream_protocol.as_str().to_owned()),
    );
    extra.insert(
        "codex_router_billing_mode".to_owned(),
        Value::String(profile.billing_mode.clone()),
    );
    extra.insert(
        "allow_fallback".to_owned(),
        Value::Bool(profile.allow_fallback),
    );
    extra
        .entry("openai_responses_mode".to_owned())
        .or_insert_with(|| Value::String(profile.upstream_protocol.responses_mode().to_owned()));
    if profile.base_url.to_ascii_lowercase().contains("api.openai.com")
        && extra
            .get("openai_responses_mode")
            .and_then(Value::as_str)
            .is_some_and(|mode| mode == "force_responses")
    {
        extra.remove("openai_responses_mode");
    }
    extra
        .entry("openai_compact_supported".to_owned())
        .or_insert_with(|| {
            Value::Bool(matches!(
                (profile.source_type, profile.upstream_protocol),
                (
                    super::ChannelSourceType::Relay,
                    super::UpstreamProtocol::Responses
                )
            ))
        });
}

fn upstream_model_id(model_id: &str, base_url: &str) -> String {
    let host = Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default();
    let model = model_id.trim();
    if host == "openrouter.ai" || host.ends_with(".openrouter.ai") {
        if let Some(suffix) = model.strip_prefix("claude/") {
            return format!("anthropic/{suffix}");
        }
        if model.starts_with("claude-") && !model.contains('/') {
            return format!("anthropic/{model}");
        }
        if model.eq_ignore_ascii_case("google/gemini-3.1-pro-high") {
            return "google/gemini-3.1-pro-preview".to_owned();
        }
    }
    model.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OAuthFallback;
    use std::sync::Mutex;

    #[test]
    fn api_account_names_are_stable_distinct_and_secret_free() {
        let school = ModelConfig {
            model: "deepseek-v4-flash".to_owned(),
            alias: "DeepSeek-V4-Flash".to_owned(),
            base_url: "https://school.example/v1".to_owned(),
            credential_name: "SchoolCredential".to_owned(),
            api_key: "must-not-appear".to_owned(),
            ..Default::default()
        };
        let plan = ModelConfig {
            base_url: "https://plan.example/v1".to_owned(),
            credential_name: "PlanCredential".to_owned(),
            ..school.clone()
        };

        let school_name = api_account_name(&school);
        let plan_name = api_account_name(&plan);
        assert_eq!(school_name, api_account_name(&school));
        assert_ne!(school_name, plan_name);
        for secret in [
            school.api_key.as_str(),
            school.base_url.as_str(),
            school.credential_name.as_str(),
        ] {
            assert!(!school_name.contains(secret));
        }
    }

    #[derive(Clone, Debug)]
    struct AdminCall {
        method: &'static str,
        path: String,
        body: Option<Value>,
    }

    #[derive(Default)]
    struct MockAdmin {
        responses: Mutex<HashMap<String, Value>>,
        calls: Mutex<Vec<AdminCall>>,
    }

    impl MockAdmin {
        fn with_response(self, path: &str, response: Value) -> Self {
            self.responses
                .lock()
                .unwrap()
                .insert(path.to_owned(), response);
            self
        }

        fn calls(&self) -> Vec<AdminCall> {
            self.calls.lock().unwrap().clone()
        }

        fn record(&self, method: &'static str, path: &str, body: Option<&Value>) {
            self.calls.lock().unwrap().push(AdminCall {
                method,
                path: path.to_owned(),
                body: body.cloned(),
            });
        }
    }

    impl AdminApi for MockAdmin {
        fn get(&self, path: &str) -> anyhow::Result<Value> {
            self.record("GET", path, None);
            Ok(self
                .responses
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .unwrap_or(Value::Null))
        }

        fn post(&self, path: &str, body: Option<&Value>) -> anyhow::Result<Value> {
            self.record("POST", path, body);
            Ok(Value::Null)
        }

        fn put(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
            self.record("PUT", path, Some(body));
            Ok(Value::Null)
        }

        fn delete(&self, path: &str) -> anyhow::Result<Value> {
            self.record("DELETE", path, None);
            Ok(Value::Null)
        }
    }

    fn model(source: &str, id: &str, account: i64) -> ModelConfig {
        ModelConfig {
            source: source.to_owned(),
            model: id.to_owned(),
            oauth_account_id: account,
            base_url: "https://api.example.com/v1".to_owned(),
            credential_name: "test-key".to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn exhausted_oauth_keeps_merged_api_route_servable() {
        let cfg = RouterConfig {
            oauth_account_ids: Some(vec![7]),
            oauth_fallback: OAuthFallback {
                enabled: true,
                ..Default::default()
            },
            models: vec![
                model("oauth", "gpt-5.6-sol", 7),
                model("apikey", "gpt-5.6-sol", 0),
            ],
            ..Default::default()
        };
        let plan = build_route_plan(&cfg);
        let routes = servable_routes(&plan, &HashSet::from([7]), &cfg);
        let visible = routes
            .iter()
            .filter(|route| route.include_in_catalog)
            .collect::<Vec<_>>();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].public_model_id, "gpt-5.6-sol");
        assert_eq!(visible[0].served_by, "api");
        assert!(routes
            .iter()
            .any(|route| route.source == "apikey" && route.join_router));
        let composite = build_composite_routes(
            &routes,
            &HashMap::from([(7, "openai".to_owned())]),
            &HashSet::from([7]),
        );
        assert_eq!(composite.len(), 1);
        assert_eq!(composite[0].public_model, "gpt-5.6-sol");
        assert_eq!(composite[0].upstream_model, "gpt-5.6-sol");
        assert_eq!(composite[0].target_platform, "openai");
    }

    #[test]
    fn routing_only_sync_parks_oauth_and_rebuilds_live_fallback_without_codex_writes() {
        let groups_path = "/api/v1/admin/groups/all?include_inactive=true";
        let accounts_path = "/api/v1/admin/accounts?page=1&page_size=200";
        let account_path = "/api/v1/admin/accounts/7";
        let routes_path = "/api/v1/admin/groups/3/composite-routes";
        let admin = MockAdmin::default()
            .with_response(
                groups_path,
                json!([{
                    "id": 3,
                    "name": MANAGED_GROUP_NAME,
                    "models_list_config": {"enabled": true, "models": ["gpt-5.6-sol"]}
                }]),
            )
            .with_response(
                accounts_path,
                json!({"items": [{"id": 7, "type": "oauth", "platform": "openai", "group_ids": [3]}]}),
            )
            .with_response(
                account_path,
                json!({
                    "id": 7,
                    "type": "oauth",
                    "platform": "openai",
                    "status": "error",
                    "schedulable": false,
                    "temp_unschedulable_reason": "quota exhausted",
                    "group_ids": [3]
                }),
            )
            .with_response(
                routes_path,
                json!({"items": [{
                    "id": 9,
                    "public_model": "gpt-5.6-sol",
                    "target_platform": "openai"
                }]}),
            );
        let cfg = RouterConfig {
            oauth_account_ids: Some(vec![7]),
            oauth_fallback: OAuthFallback {
                enabled: true,
                ..Default::default()
            },
            models: vec![
                model("oauth", "gpt-5.6-sol", 7),
                model("apikey", "gpt-5.6-sol", 0),
            ],
            ..Default::default()
        };

        sync_routing_only_with_admin(&admin, &cfg).unwrap();

        let calls = admin.calls();
        let account_update = calls
            .iter()
            .find(|call| call.method == "PUT" && call.path == account_path)
            .unwrap();
        assert_eq!(
            account_update.body.as_ref().unwrap()["group_ids"],
            json!([])
        );
        let route_update = calls
            .iter()
            .find(|call| call.method == "PUT" && call.path == format!("{routes_path}/9"))
            .unwrap();
        assert_eq!(
            route_update.body.as_ref().unwrap()["target_platform"],
            "openai"
        );
        assert_eq!(route_update.body.as_ref().unwrap()["priority"], 100);
        assert!(calls.iter().all(|call| {
            !call.path.contains("config.toml")
                && !call.path.contains("lifecycle")
                && !call.path.contains("restart")
        }));
    }

    #[test]
    fn routing_only_sync_detaches_unselected_oauth_duplicates() {
        let groups_path = "/api/v1/admin/groups/all?include_inactive=true";
        let accounts_path = "/api/v1/admin/accounts?page=1&page_size=200";
        let selected_path = "/api/v1/admin/accounts/7";
        let duplicate_path = "/api/v1/admin/accounts/11";
        let routes_path = "/api/v1/admin/groups/3/composite-routes";
        let admin = MockAdmin::default()
            .with_response(
                groups_path,
                json!([{"id": 3, "name": MANAGED_GROUP_NAME}]),
            )
            .with_response(
                accounts_path,
                json!({"items": [
                    {"id": 7, "type": "oauth", "platform": "openai", "group_ids": [3]},
                    {"id": 11, "type": "oauth", "platform": "openai", "group_ids": [3, 99]}
                ]}),
            )
            .with_response(
                selected_path,
                json!({"id": 7, "status": "error", "schedulable": false, "group_ids": [3]}),
            )
            .with_response(
                duplicate_path,
                json!({"id": 11, "status": "active", "schedulable": true, "group_ids": [3, 99]}),
            )
            .with_response(routes_path, json!({"items": []}));
        let cfg = RouterConfig {
            oauth_account_ids: Some(vec![7]),
            oauth_fallback: OAuthFallback {
                enabled: true,
                ..Default::default()
            },
            models: vec![
                model("oauth", "gpt-5.6-sol", 7),
                model("apikey", "gpt-5.6-sol", 0),
            ],
            ..Default::default()
        };

        sync_routing_only_with_admin(&admin, &cfg).unwrap();

        let update = admin
            .calls()
            .into_iter()
            .find(|call| call.method == "PUT" && call.path == duplicate_path)
            .unwrap();
        assert_eq!(update.body.unwrap()["group_ids"], json!([99]));
        let scheduling_update = admin
            .calls()
            .into_iter()
            .find(|call| call.method == "POST" && call.path == format!("{duplicate_path}/schedulable"))
            .unwrap();
        assert_eq!(
            scheduling_update.body.unwrap()["schedulable"],
            json!(false)
        );
    }

    #[test]
    fn routing_only_sync_restores_every_selected_duplicate_oauth_account() {
        let groups_path = "/api/v1/admin/groups/all?include_inactive=true";
        let accounts_path = "/api/v1/admin/accounts?page=1&page_size=200";
        let account_4_path = "/api/v1/admin/accounts/4";
        let account_26_path = "/api/v1/admin/accounts/26";
        let routes_path = "/api/v1/admin/groups/8/composite-routes";
        let account_summary = |id| {
            json!({
                "id": id,
                "type": "oauth",
                "platform": "antigravity",
                "group_ids": []
            })
        };
        let account_detail = |id| {
            json!({
                "id": id,
                "type": "oauth",
                "platform": "antigravity",
                "status": "active",
                "schedulable": true,
                "group_ids": []
            })
        };
        let admin = MockAdmin::default()
            .with_response(
                groups_path,
                json!([{
                    "id": 8,
                    "name": MANAGED_GROUP_NAME,
                    "models_list_config": {"enabled": true, "models": ["gemini-3.7-flash"]}
                }]),
            )
            .with_response(
                accounts_path,
                json!({"items": [account_summary(4), account_summary(26)]}),
            )
            .with_response(account_4_path, account_detail(4))
            .with_response(account_26_path, account_detail(26))
            .with_response(routes_path, json!({"items": []}));
        let mut representative = model("oauth", "gemini-3.7-flash", 4);
        representative.oauth_platform = "antigravity".to_owned();
        let cfg = RouterConfig {
            oauth_account_ids: Some(vec![4, 26]),
            models: vec![representative],
            ..Default::default()
        };

        sync_routing_only_with_admin(&admin, &cfg).unwrap();

        for account_path in [account_4_path, account_26_path] {
            let update = admin
                .calls()
                .into_iter()
                .find(|call| call.method == "PUT" && call.path == account_path)
                .unwrap();
            assert_eq!(update.body.unwrap()["group_ids"], json!([8]));
        }
    }

    #[test]
    fn exhausted_oauth_without_fallback_is_removed() {
        let cfg = RouterConfig {
            oauth_account_ids: Some(vec![7]),
            models: vec![model("oauth", "gpt-5.6-sol", 7)],
            ..Default::default()
        };
        let plan = build_route_plan(&cfg);
        assert!(servable_routes(&plan, &HashSet::from([7]), &cfg).is_empty());
    }

    #[test]
    fn healthy_duplicate_oauth_account_keeps_shared_models_servable() {
        let mut first_gemini = model("oauth", "gemini-3.1-pro-high", 4);
        first_gemini.oauth_platform = "antigravity".to_owned();
        let mut first_fable = model("oauth", "claude-fable-5", 4);
        first_fable.oauth_platform = "antigravity".to_owned();
        let mut second_gemini = model("oauth", "gemini-3.1-pro-high", 26);
        second_gemini.oauth_platform = "antigravity".to_owned();
        let mut second_fable = model("oauth", "claude-fable-5", 26);
        second_fable.oauth_platform = "antigravity".to_owned();
        let cfg = RouterConfig {
            oauth_account_ids: Some(vec![4, 26]),
            models: vec![first_gemini, first_fable, second_gemini, second_fable],
            ..Default::default()
        };
        let plan = build_route_plan(&cfg);
        let routes = servable_routes(&plan, &HashSet::from([4]), &cfg);
        let visible = routes
            .iter()
            .filter(|route| route.include_in_catalog)
            .collect::<Vec<_>>();

        assert_eq!(visible.len(), 2);
        assert!(visible
            .iter()
            .all(|route| route.model.oauth_account_id == 26));
        let composite = build_composite_routes(
            &routes,
            &HashMap::from([
                (4, "antigravity".to_owned()),
                (26, "antigravity".to_owned()),
            ]),
            &HashSet::from([4]),
        );
        assert_eq!(composite.len(), 2);
        assert!(composite
            .iter()
            .all(|route| route.target_platform == "antigravity"));
    }

    #[test]
    fn selected_duplicate_oauth_accounts_all_join_the_router_group() {
        let account_4_path = "/api/v1/admin/accounts/4";
        let account_26_path = "/api/v1/admin/accounts/26";
        let admin = MockAdmin::default()
            .with_response(
                account_4_path,
                json!({
                    "id": 4,
                    "type": "oauth",
                    "platform": "antigravity",
                    "status": "active",
                    "schedulable": true,
                    "priority": 10,
                    "group_ids": []
                }),
            )
            .with_response(
                account_26_path,
                json!({
                    "id": 26,
                    "type": "oauth",
                    "platform": "antigravity",
                    "status": "active",
                    "schedulable": true,
                    "priority": 20,
                    "group_ids": []
                }),
            );
        let mut representative = model("oauth", "gemini-3.7-flash", 4);
        representative.oauth_platform = "antigravity".to_owned();
        let cfg = RouterConfig {
            oauth_account_ids: Some(vec![4, 26]),
            models: vec![representative],
            ..Default::default()
        };
        let routes = build_route_plan(&cfg);
        let proxy_runtime = ProxyRuntime {
            settings: crate::proxy::ProxySettings {
                mode: "direct".to_owned(),
                source: "test".to_owned(),
                proxy_url: None,
                no_proxy: String::new(),
                has_credentials: false,
                supports_account_binding: false,
                diagnostic: String::new(),
            },
            targets: BTreeMap::new(),
        };

        sync_oauth_and_stale_accounts(
            &admin,
            &cfg,
            &routes,
            8,
            ManagedProxy::default(),
            &proxy_runtime,
            &HashSet::new(),
            &[
                json!({"id": 4, "type": "oauth"}),
                json!({"id": 26, "type": "oauth"}),
            ],
            &AtomicBool::new(false),
        )
        .unwrap();

        for account_path in [account_4_path, account_26_path] {
            let update = admin
                .calls()
                .into_iter()
                .find(|call| call.method == "PUT" && call.path == account_path)
                .unwrap();
            assert_eq!(update.body.unwrap()["group_ids"], json!([8]));
        }
    }

    #[test]
    fn split_mode_builds_distinct_composite_routes() {
        let cfg = RouterConfig {
            oauth_account_ids: Some(vec![7]),
            oauth_fallback: OAuthFallback {
                enabled: false,
                ..Default::default()
            },
            models: vec![
                model("oauth", "gpt-5.6-sol", 7),
                model("apikey", "gpt-5.6-sol", 0),
            ],
            ..Default::default()
        };
        let plan = build_route_plan(&cfg);
        let routes = servable_routes(&plan, &HashSet::new(), &cfg);
        let composite = build_composite_routes(
            &routes,
            &HashMap::from([(7, "openai".to_owned())]),
            &HashSet::new(),
        );
        assert_eq!(composite.len(), 2);
        assert_ne!(composite[0].public_model, composite[1].public_model);
    }

    #[test]
    fn openrouter_model_and_transport_policy_are_normalized() {
        assert_eq!(
            upstream_model_id("claude/opus-4.6", "https://openrouter.ai/api/v1"),
            "anthropic/opus-4.6"
        );
        let mut extra = Map::new();
        apply_openai_channel_policy(
            &ModelConfig {
                model: "kimi-for-coding".to_owned(),
                base_url: "https://api.kimi.com/coding/v1".to_owned(),
                ..Default::default()
            },
            &mut extra,
        );
        assert_eq!(
            extra["openai_responses_mode"],
            Value::String("force_chat_completions".to_owned())
        );
        assert_eq!(extra["openai_compact_supported"], Value::Bool(false));
        assert_eq!(extra["codex_router_source_type"], "coding_plan");
        assert_eq!(extra["codex_router_upstream_protocol"], "chat_completions");
        assert_eq!(extra["allow_fallback"], false);
        assert!(official_direct_api_host("https://api.deepseek.com/v1"));
        assert!(official_direct_api_host("https://api.kimi.com/coding/v1"));
        assert!(!official_direct_api_host("https://api.openai.com/v1"));
    }

    #[test]
    fn custom_proxy_is_preserved_while_managed_proxy_can_be_cleared() {
        let managed = ManagedProxy {
            managed_id: 12,
            desired_id: 12,
        };
        assert_eq!(reconcile_proxy_id(99, managed, true), None);
        assert_eq!(reconcile_proxy_id(12, managed, false), Some(0));
        assert_eq!(reconcile_proxy_id(0, managed, true), Some(12));
    }

    #[test]
    fn hidden_managed_local_key_is_reused_without_creating_a_duplicate() {
        let path = "/api/v1/keys?page=1&page_size=200";
        let admin = MockAdmin::default().with_response(
            path,
            json!({
                "items": [{
                    "id": 3,
                    "name": MANAGED_GROUP_NAME,
                    "group_id": 7,
                    "key": "test...value"
                }]
            }),
        );

        sync_local_api_key(&admin, 7, "test-local-credential").unwrap();

        assert!(admin.calls().iter().all(|call| call.method != "POST"));
    }

    #[test]
    fn missing_local_key_record_is_created_once_with_the_managed_group() {
        let path = "/api/v1/keys?page=1&page_size=200";
        let admin = MockAdmin::default().with_response(path, json!({ "items": [] }));

        sync_local_api_key(&admin, 9, "test-local-credential").unwrap();

        let posts = admin
            .calls()
            .into_iter()
            .filter(|call| call.method == "POST")
            .collect::<Vec<_>>();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].path, "/api/v1/keys");
        assert_eq!(posts[0].body.as_ref().unwrap()["group_id"], 9);
        assert_eq!(
            posts[0].body.as_ref().unwrap()["custom_key"],
            "test-local-credential"
        );
    }

    #[test]
    fn composite_route_sync_updates_one_deletes_duplicates_and_removes_stale_routes() {
        let path = "/api/v1/admin/groups/7/composite-routes";
        let admin = MockAdmin::default().with_response(
            path,
            json!({
                "items": [
                    {"id": 1, "public_model": "gpt-test", "target_platform": "openai"},
                    {"id": 2, "public_model": "GPT-TEST", "target_platform": "openai"},
                    {"id": 3, "public_model": "stale", "target_platform": "openai"}
                ]
            }),
        );
        let desired = [CompositeRoute {
            public_model: "gpt-test".to_owned(),
            upstream_model: "gpt-upstream".to_owned(),
            target_platform: "openai".to_owned(),
            priority: 1,
        }];

        sync_composite_routes(&admin, 7, &desired).unwrap();

        let calls = admin.calls();
        assert!(calls
            .iter()
            .any(|call| call.method == "PUT" && call.path.ends_with("/1")));
        assert!(calls
            .iter()
            .any(|call| call.method == "DELETE" && call.path.ends_with("/2")));
        assert!(calls
            .iter()
            .any(|call| call.method == "DELETE" && call.path.ends_with("/3")));
        assert!(calls.iter().all(|call| call.method != "POST"));
    }

    #[test]
    fn stale_managed_channel_only_leaves_router_group() {
        let account_path = "/api/v1/admin/accounts/11";
        let admin = MockAdmin::default().with_response(
            account_path,
            json!({
                "id": 11,
                "name": "Codex-Router / stale",
                "type": "apikey",
                "group_ids": [7, 99]
            }),
        );
        let cfg = RouterConfig::default();
        let proxy_runtime = ProxyRuntime {
            settings: crate::proxy::ProxySettings {
                mode: "direct".to_owned(),
                source: "test".to_owned(),
                proxy_url: None,
                no_proxy: String::new(),
                has_credentials: false,
                supports_account_binding: false,
                diagnostic: String::new(),
            },
            targets: BTreeMap::new(),
        };
        let cancel = AtomicBool::new(false);

        sync_oauth_and_stale_accounts(
            &admin,
            &cfg,
            &[],
            7,
            ManagedProxy::default(),
            &proxy_runtime,
            &HashSet::new(),
            &[json!({"id": 11, "group_ids": [7, 99]})],
            &cancel,
        )
        .unwrap();

        let calls = admin.calls();
        let update = calls
            .iter()
            .find(|call| call.method == "PUT" && call.path == account_path)
            .unwrap();
        assert_eq!(update.body.as_ref().unwrap()["group_ids"], json!([99]));
        assert!(calls.iter().all(|call| call.method != "DELETE"));
    }

    #[test]
    fn oauth_account_mapping_contains_only_models_selected_by_the_user() {
        let account_path = "/api/v1/admin/accounts/24";
        let admin = MockAdmin::default().with_response(
            account_path,
            json!({
                "id": 24,
                "type": "oauth",
                "platform": "grok",
                "status": "active",
                "schedulable": true,
                "priority": 10,
                "group_ids": [],
                "credentials": {
                    "project_id": "existing-project",
                    "plan_type": "pro"
                }
            }),
        );
        let cfg = RouterConfig {
            oauth_account_ids: Some(vec![24]),
            models: vec![
                model("oauth", "grok-4.5", 24),
                model("oauth", "grok-4.6", 24),
            ],
            ..Default::default()
        };
        let routes = build_route_plan(&cfg);
        let proxy_runtime = ProxyRuntime {
            settings: crate::proxy::ProxySettings {
                mode: "direct".to_owned(),
                source: "test".to_owned(),
                proxy_url: None,
                no_proxy: String::new(),
                has_credentials: false,
                supports_account_binding: false,
                diagnostic: String::new(),
            },
            targets: BTreeMap::new(),
        };

        sync_oauth_and_stale_accounts(
            &admin,
            &cfg,
            &routes,
            8,
            ManagedProxy::default(),
            &proxy_runtime,
            &HashSet::new(),
            &[json!({"id": 24, "type": "oauth"})],
            &AtomicBool::new(false),
        )
        .unwrap();

        let update = admin
            .calls()
            .into_iter()
            .find(|call| call.method == "PUT" && call.path == account_path)
            .unwrap();
        let body = update.body.unwrap();
        assert_eq!(body["credentials"]["project_id"], "existing-project");
        assert_eq!(body["credentials"]["plan_type"], "pro");
        assert_eq!(
            body["credentials"]["model_mapping"],
            json!({"grok-4.5": "grok-4.5", "grok-4.6": "grok-4.6"})
        );
    }

    #[test]
    fn antigravity_gemini_37_uses_the_medium_upstream_tier() {
        let cfg = RouterConfig {
            oauth_account_ids: Some(vec![26]),
            models: vec![ModelConfig {
                source: "oauth".to_owned(),
                model: "gemini-3.7-flash".to_owned(),
                oauth_account_id: 26,
                oauth_platform: "antigravity".to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let mappings = selected_oauth_model_mappings(&build_route_plan(&cfg));
        assert_eq!(mappings[&26]["gemini-3.7-flash"], "gemini-3.7-flash-medium");
    }

    #[test]
    fn kimi_k3_channel_payload_joins_router_group_with_supported_transport_mapping() {
        let credential_name = format!(
            "Codex-Router-Test-Kimi-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let result = (|| -> anyhow::Result<()> {
            crate::logic::write_router_credential_text(&credential_name, "test-kimi-credential")?;
            let cfg = RouterConfig {
                models: vec![ModelConfig {
                    source: "apikey".to_owned(),
                    model: "k3-256k".to_owned(),
                    alias: "Kimi K3 256K".to_owned(),
                    base_url: "https://api.kimi.com/coding/v1".to_owned(),
                    credential_name: credential_name.clone(),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let routes = build_route_plan(&cfg);
            let admin = MockAdmin::default();
            let proxy_runtime = ProxyRuntime {
                settings: crate::proxy::ProxySettings {
                    mode: "direct".to_owned(),
                    source: "test".to_owned(),
                    proxy_url: None,
                    no_proxy: String::new(),
                    has_credentials: false,
                    supports_account_binding: false,
                    diagnostic: String::new(),
                },
                targets: BTreeMap::new(),
            };
            let mut accounts = Vec::new();
            let cancel = AtomicBool::new(false);
            sync_api_channels(
                &admin,
                Path::new("."),
                &cfg,
                &routes,
                7,
                ManagedProxy::default(),
                &proxy_runtime,
                &mut accounts,
                &cancel,
                &mut |_| {},
            )?;

            let calls = admin.calls();
            let create = calls
                .iter()
                .find(|call| call.method == "POST" && call.path == "/api/v1/admin/accounts")
                .context("Kimi account create call was not emitted")?;
            let body = create
                .body
                .as_ref()
                .context("Kimi account body is missing")?;
            assert_eq!(body["group_ids"], json!([7]));
            assert_eq!(body["priority"], 10);
            assert_eq!(body["credentials"]["model_mapping"]["k3-256k"], "k3-256k");
            assert_eq!(
                body["extra"]["openai_responses_mode"],
                "force_chat_completions"
            );
            assert_eq!(
                body["credentials"]["openai_capabilities"],
                json!(["chat_completions"])
            );
            Ok(())
        })();
        let _ = crate::logic::remove_isolated_profile_credentials(&[credential_name]);
        result.unwrap();
    }

    #[test]
    fn volcengine_coding_plan_uses_conservative_concurrency() {
        let volcengine = ModelConfig {
            base_url: "https://ark.cn-beijing.volces.com/api/coding/v3".to_owned(),
            ..Default::default()
        };
        let generic = ModelConfig {
            base_url: "https://api.example.test/v1".to_owned(),
            ..Default::default()
        };
        assert_eq!(api_account_concurrency(&volcengine), 2);
        assert_eq!(api_account_concurrency(&generic), 8);
    }

    #[test]
    fn sync_managed_proxy_reuses_an_existing_url_instead_of_creating_a_duplicate() {
        let path = "/api/v1/admin/proxies?page=1&page_size=200";
        let admin = MockAdmin::default().with_response(
            path,
            json!({"items":[{
                "id": 8,
                "name": "legacy-proxy",
                "protocol": "http",
                "host": "127.0.0.1",
                "port": 7890
            }]}),
        );
        let runtime = ProxyRuntime {
            settings: crate::proxy::ProxySettings {
                mode: "auto".to_owned(),
                source: "environment".to_owned(),
                proxy_url: Some("http://127.0.0.1:7890".to_owned()),
                no_proxy: String::new(),
                has_credentials: false,
                supports_account_binding: false,
                diagnostic: String::new(),
            },
            targets: Default::default(),
        };
        let proxy = sync_managed_proxy(&admin, &runtime).unwrap();
        assert_eq!(proxy.desired_id, 8);
        assert!(admin.calls().iter().all(|call| call.method != "POST"));
    }
}
