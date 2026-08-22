use super::{oauth_routing_priorities, usage};
use crate::config::RouterConfig;
use anyhow::{bail, Context};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use url::Url;
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    OpenAi,
    Anthropic,
    Gemini,
    Antigravity,
    Grok,
}

impl Provider {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "openai" => Ok(Self::OpenAi),
            "anthropic" => Ok(Self::Anthropic),
            "gemini" => Ok(Self::Gemini),
            "antigravity" => Ok(Self::Antigravity),
            "grok" => Ok(Self::Grok),
            _ => bail!("class=configuration"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::Antigravity => "antigravity",
            Self::Grok => "grok",
        }
    }

    fn automatic_callback(self) -> bool {
        matches!(self, Self::OpenAi | Self::Antigravity | Self::Grok)
    }

    fn router_owns_callback(self) -> bool {
        matches!(self, Self::Grok)
    }

    fn callback_port(self) -> Option<u16> {
        match self {
            Self::OpenAi => Some(1455),
            Self::Antigravity => Some(8085),
            Self::Grok => Some(56121),
            Self::Anthropic | Self::Gemini => None,
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::OpenAi => "ChatGPT OAuth",
            Self::Anthropic => "Claude OAuth",
            Self::Gemini => "Gemini OAuth",
            Self::Antigravity => "Antigravity OAuth",
            Self::Grok => "Grok OAuth",
        }
    }

    fn recovery_model(self) -> &'static str {
        match self {
            Self::OpenAi => "gpt-5.6-sol",
            Self::Anthropic => "claude-opus-4-6",
            Self::Gemini | Self::Antigravity => "gemini-3.1-pro-high",
            Self::Grok => "grok-4.5",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prompt {
    GeminiConfiguration { detected_project_id: String },
    AuthorizationCode { provider: Provider, manual: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptResponse {
    GeminiConfiguration {
        oauth_type: String,
        tier_id: String,
        project_id: String,
    },
    AuthorizationCode(Zeroizing<String>),
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthResult {
    pub account_id: i64,
    pub account_name: String,
    pub reused_existing: bool,
}

#[derive(Clone, Debug)]
struct ExistingAccount {
    id: i64,
    name: String,
    priority: i32,
    identity: Option<String>,
}

struct CallbackListeners {
    listeners: Vec<TcpListener>,
}

impl CallbackListeners {
    fn bind(port: u16) -> anyhow::Result<Self> {
        let mut listeners = Vec::new();
        let mut failures = Vec::new();
        for address in [
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port),
            SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port),
        ] {
            match TcpListener::bind(address) {
                Ok(listener) => {
                    listener.set_nonblocking(true)?;
                    listeners.push(listener);
                }
                Err(error) => failures.push(format!("{address}: {error}")),
            }
        }
        if listeners.len() != 2 {
            bail!(
                "ROUTER_OAUTH_PORT_IN_USE: callback port {port} is not exclusively available ({}); use the manual callback URL flow; class=configuration",
                failures.join("; ")
            )
        }
        Ok(Self { listeners })
    }

    fn wait<F>(
        &self,
        expected_state: &str,
        timeout: Duration,
        cancel: &AtomicBool,
        poll_every: Duration,
        mut on_poll: F,
    ) -> anyhow::Result<CallbackWait>
    where
        F: FnMut() -> anyhow::Result<Option<OAuthResult>>,
    {
        let deadline = Instant::now() + timeout;
        let mut last_poll = Instant::now()
            .checked_sub(poll_every)
            .unwrap_or_else(Instant::now);
        while Instant::now() < deadline {
            if cancel.load(Ordering::Acquire) {
                bail!("ROUTER_OAUTH_CANCELLED: class=cancelled")
            }
            for listener in &self.listeners {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if let Ok(callback) = read_callback(&mut stream, expected_state) {
                            let _ = send_callback_page(&mut stream, true);
                            return Ok(CallbackWait::Code(callback.0, callback.1));
                        }
                        let _ = send_callback_page(&mut stream, false);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) => return Err(error).context("class=request_failure"),
                }
            }
            if last_poll.elapsed() >= poll_every {
                last_poll = Instant::now();
                // Polling the account list is best-effort. A transient admin
                // read must not abort an otherwise successful device login.
                if let Ok(Some(account)) = on_poll() {
                    return Ok(CallbackWait::Account(account));
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        bail!("ROUTER_OAUTH_CALLBACK_TIMEOUT: class=timeout")
    }
}

enum CallbackWait {
    Code(Zeroizing<String>, String),
    Account(OAuthResult),
}

pub fn prepare(
    router_root: &Path,
    config: &RouterConfig,
    provider: &str,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    Provider::parse(provider)?;
    // A first run on a new machine serializes behind a concurrent Apply that
    // may still be running initdb / pg_ctl / the Sub2API boot. Waiting minutes
    // here is what keeps the first-run wizard from failing with
    // ROUTER_OAUTH_PREPARE_LIFECYCLE_BUSY on every attempt.
    let _lock = crate::lifecycle::acquire_lifecycle_lock(
        router_root,
        Duration::from_secs(120),
        "Prepare provider OAuth",
    )?;
    crate::lifecycle::ensure_services_with_config(router_root, config, true, cancel, true)
        .context("ROUTER_OAUTH_PREPARE_ROUTER_START")?;
    let admin = usage::retry_admin_read(|| usage::AdminClient::connect(router_root, config))
        .context("ROUTER_OAUTH_PREPARE_ADMIN_LOGIN")?;
    usage::retry_account_read(|| admin.get("/api/v1/admin/compliance", Duration::from_secs(10)))
        .context("ROUTER_OAUTH_PREPARE_COMPLIANCE")?;
    let capabilities = usage::data(usage::retry_account_read(|| {
        admin.get(
            "/api/v1/admin/oauth/capabilities",
            Duration::from_secs(10),
        )
    })?);
    if usage::get(&capabilities, "ready").and_then(Value::as_bool) != Some(true)
        || !usage::array(usage::get(&capabilities, "providers").unwrap_or(&Value::Null))
            .iter()
            .any(|item| item.as_str() == Some(provider))
    {
        bail!("ROUTER_OAUTH_PREPARE_PROVIDER_ADAPTER: class=invalid_response")
    }
    Ok(())
}

pub fn run<F>(
    router_root: &Path,
    config: &RouterConfig,
    provider: &str,
    compliance_accepted: bool,
    cancel: &AtomicBool,
    mut prompt: F,
) -> anyhow::Result<OAuthResult>
where
    F: FnMut(Prompt) -> anyhow::Result<PromptResponse>,
{
    let provider = Provider::parse(provider)?;
    let _lock = crate::lifecycle::acquire_lifecycle_lock(
        router_root,
        Duration::from_secs(120),
        "Start provider OAuth",
    )?;
    crate::lifecycle::ensure_services_with_config(router_root, config, true, cancel, true)
        .context("ROUTER_OAUTH_PREPARE_ROUTER_START")?;
    let admin = usage::retry_admin_read(|| usage::AdminClient::connect(router_root, config))?;
    accept_compliance_if_required(&admin, compliance_accepted)?;
    let group_id = ensure_router_group(&admin)?;
    let existing = load_existing_accounts(&admin, provider)?;
    let priority = next_priority(&existing, config);
    let mut gemini = GeminiOptions::default();
    if provider == Provider::Gemini {
        let detected_project_id = detect_google_project(&admin)?;
        gemini = match prompt(Prompt::GeminiConfiguration {
            detected_project_id: detected_project_id.clone(),
        })? {
            PromptResponse::GeminiConfiguration {
                oauth_type,
                tier_id,
                project_id,
            } => validate_gemini_options(oauth_type, tier_id, project_id)?,
            PromptResponse::Cancelled => bail!("ROUTER_OAUTH_CANCELLED: class=cancelled"),
            PromptResponse::AuthorizationCode(_) => bail!("class=configuration"),
        };
    }

    let listeners = provider
        .router_owns_callback()
        .then(|| provider.callback_port())
        .flatten()
        .and_then(|port| CallbackListeners::bind(port).ok());
    let cli_callback_unavailable = provider.automatic_callback()
        && !provider.router_owns_callback()
        && provider
            .callback_port()
            .is_some_and(|port| CallbackListeners::bind(port).is_err());
    let auth = create_authorization(&admin, provider, &gemini)?;
    let mut auth_url = usage::string(&auth, "auth_url");
    let session_id = usage::string(&auth, "session_id");
    let expected_state = usage::string(&auth, "state");
    if auth_url.is_empty() || session_id.is_empty() {
        bail!("class=invalid_response")
    }
    if !existing.is_empty() {
        auth_url = force_account_chooser(&auth_url, provider)?;
    }
    open_authorization_url(&auth_url)?;

    let automatic_listener = if provider.router_owns_callback() {
        listeners.as_ref()
    } else {
        None
    };
    let (code, state, finished_account) = if let Some(listener) = automatic_listener {
        match listener.wait(
            &expected_state,
            Duration::from_secs(300),
            cancel,
            Duration::from_secs(1),
            || {
                match load_existing_accounts(&admin, provider) {
                    Ok(current) => Ok(first_new_account(&existing, current)),
                    Err(_) => Ok(None),
                }
            },
        )? {
            CallbackWait::Code(code, state) => (code, state, None),
            CallbackWait::Account(account) => {
                (Zeroizing::new(String::new()), String::new(), Some(account))
            }
        }
    } else if provider.automatic_callback()
        && !provider.router_owns_callback()
        && !cli_callback_unavailable
    {
        (Zeroizing::new(String::new()), expected_state.clone(), None)
    } else {
        let manual = provider.automatic_callback()
            && (listeners.is_none() || cli_callback_unavailable);
        let (code, state) = match prompt(Prompt::AuthorizationCode { provider, manual })? {
            PromptResponse::AuthorizationCode(code) if !code.trim().is_empty() => {
                parse_manual_authorization(code.trim(), &expected_state)?
            }
            PromptResponse::Cancelled => bail!("ROUTER_OAUTH_CANCELLED: class=cancelled"),
            PromptResponse::AuthorizationCode(_) => bail!("class=configuration"),
            PromptResponse::GeminiConfiguration { .. } => bail!("class=configuration"),
        };
        (code, state, None)
    };
    if cancel.load(Ordering::Acquire) {
        bail!("ROUTER_OAUTH_CANCELLED: class=cancelled")
    }

    let created = if let Some(account) = finished_account {
        return finalize_oauth_account(&admin, provider, account);
    } else {
        create_account(
            &admin,
            provider,
            &session_id,
            &code,
            &state,
            group_id,
            priority,
            &existing,
            &gemini,
        )?
    };
    let result = reconcile_new_account(&admin, provider, created, &existing, group_id)?;
    finalize_oauth_account(&admin, provider, result)
}



fn platform_matches(provider: Provider, platform: &str) -> bool {
    let normalize = |value: &str| match value.trim().to_ascii_lowercase().as_str() {
        "chatgpt" => "openai".to_owned(),
        "claude" => "anthropic".to_owned(),
        "xai" | "x-ai" => "grok".to_owned(),
        value => value.to_owned(),
    };
    normalize(provider.as_str()) == normalize(platform)
}

fn first_new_account(
    known: &[ExistingAccount],
    current: Vec<ExistingAccount>,
) -> Option<OAuthResult> {
    current.into_iter().find(|account| {
        known.iter().all(|existing| existing.id != account.id)
    }).map(|account| OAuthResult {
        account_id: account.id,
        account_name: account.name,
        reused_existing: false,
    })
}

fn finalize_oauth_account(
    admin: &usage::AdminClient,
    provider: Provider,
    result: OAuthResult,
) -> anyhow::Result<OAuthResult> {
    // The account row is the source of truth. Model catalog sync can lag a
    // few seconds after Grok device authorization; do not keep the UI waiting
    // or fail the login just because the first catalog read is empty.
    let _ = super::load_live_oauth_models(admin, result.account_id, provider.as_str());
    let _ = ensure_scheduled_recovery(admin, result.account_id, provider.recovery_model());
    Ok(result)
}

fn parse_manual_authorization(
    input: &str,
    expected_state: &str,
) -> anyhow::Result<(Zeroizing<String>, String)> {
    let trimmed = input.trim();
    if let Ok(url) = Url::parse(trimmed) {
        let mut code = None;
        let mut state = None;
        let mut callback_error = None;
        for (name, value) in url.query_pairs() {
            match name.as_ref() {
                "code" => code = Some(value.into_owned()),
                "state" => state = Some(value.into_owned()),
                "error" => callback_error = Some(value.into_owned()),
                _ => {}
            }
        }
        if callback_error.is_some() {
            bail!("ROUTER_OAUTH_CALLBACK_REJECTED: class=authentication")
        }
        let state = state.context("ROUTER_OAUTH_STATE_MISSING: class=configuration")?;
        if state != expected_state {
            bail!("ROUTER_OAUTH_STATE_MISMATCH: class=authentication")
        }
        let code = code
            .filter(|value| !value.trim().is_empty())
            .context("ROUTER_OAUTH_CODE_MISSING: class=configuration")?;
        return Ok((Zeroizing::new(code), state));
    }
    Ok((
        Zeroizing::new(trimmed.to_owned()),
        expected_state.to_owned(),
    ))
}

#[derive(Clone, Debug)]
struct GeminiOptions {
    oauth_type: String,
    tier_id: String,
    project_id: String,
}

impl Default for GeminiOptions {
    fn default() -> Self {
        Self {
            oauth_type: "google_one".to_owned(),
            tier_id: "google_one_free".to_owned(),
            project_id: String::new(),
        }
    }
}

fn validate_gemini_options(
    oauth_type: String,
    tier_id: String,
    project_id: String,
) -> anyhow::Result<GeminiOptions> {
    let expected_tier = match oauth_type.as_str() {
        "google_one" => "google_one_free",
        "code_assist" => "gcp_standard",
        _ => bail!("class=configuration"),
    };
    if tier_id != expected_tier || project_id.contains(['\r', '\n', '\0']) {
        bail!("class=configuration")
    }
    Ok(GeminiOptions {
        oauth_type,
        tier_id,
        project_id: project_id.trim().to_owned(),
    })
}

fn accept_compliance_if_required(admin: &usage::AdminClient, accepted: bool) -> anyhow::Result<()> {
    let compliance = usage::data(usage::retry_account_read(|| {
        admin.get("/api/v1/admin/compliance", Duration::from_secs(10))
    })?);
    let required = usage::get(&compliance, "required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !required {
        return Ok(());
    }
    if !accepted {
        bail!("class=permission")
    }
    let phrase = usage::string(&compliance, "ack_phrase_zh");
    if phrase.is_empty() {
        bail!("class=invalid_response")
    }
    admin.post(
        "/api/v1/admin/compliance/accept",
        Some(&json!({ "phrase": phrase, "language": "zh" })),
        Duration::from_secs(10),
    )?;
    Ok(())
}

fn ensure_router_group(admin: &usage::AdminClient) -> anyhow::Result<i64> {
    let find = || -> anyhow::Result<Option<i64>> {
        let groups = usage::data(usage::retry_account_read(|| {
            admin.get(
                "/api/v1/admin/groups/all?include_inactive=true",
                Duration::from_secs(10),
            )
        })?);
        Ok(usage::array(&groups)
            .iter()
            .find(|group| {
                matches!(
                    usage::string(group, "name").as_str(),
                    "Codex-Router" | "Codex Unified Router"
                )
            })
            .map(|group| usage::integer(group, "id"))
            .filter(|id| *id > 0))
    };
    if let Some(id) = find()? {
        return Ok(id);
    }
    let body = json!({
        "name": "Codex-Router",
        "description": "Single-user local Codex multi-model router managed by Codex-Router.",
        "platform": "composite",
        "rate_multiplier": 1.0,
        "is_exclusive": false,
        "subscription_type": "standard",
        "status": "active",
        "allow_messages_dispatch": false,
        "allow_live": false,
        "require_oauth_only": false,
        "models_list_config": { "enabled": false, "models": [] },
    });
    match admin.post("/api/v1/admin/groups", Some(&body), Duration::from_secs(15)) {
        Ok(response) => {
            let id = usage::integer(&usage::data(response), "id");
            if id > 0 {
                return Ok(id);
            }
        }
        Err(_) => {
            if let Some(id) = find()? {
                return Ok(id);
            }
        }
    }
    bail!("class=invalid_response")
}

fn load_existing_accounts(
    admin: &usage::AdminClient,
    provider: Provider,
) -> anyhow::Result<Vec<ExistingAccount>> {
    let body = usage::data(usage::retry_account_read(|| {
        admin.get(
            "/api/v1/admin/accounts?page=1&page_size=200",
            Duration::from_secs(10),
        )
    })?);
    let items = usage::get(&body, "items").unwrap_or(&body);
    let mut existing = Vec::new();
    for summary in usage::array(items) {
        if usage::string(summary, "type") != "oauth"
            || !platform_matches(provider, &usage::string(summary, "platform"))
        {
            continue;
        }
        let id = usage::integer(summary, "id");
        if id <= 0 {
            continue;
        }
        let detail = usage::data(usage::retry_account_read(|| {
            admin.get(
                &format!("/api/v1/admin/accounts/{id}"),
                Duration::from_secs(10),
            )
        })?);
        let credentials = usage::get(&detail, "credentials").unwrap_or(&Value::Null);
        let extra = usage::get(&detail, "extra").unwrap_or(&Value::Null);
        existing.push(ExistingAccount {
            id,
            name: usage::string(&detail, "name"),
            priority: usage::integer(&detail, "priority").try_into().unwrap_or(1),
            identity: stable_identity(provider.as_str(), credentials, extra),
        });
    }
    Ok(existing)
}

fn next_priority(existing: &[ExistingAccount], config: &RouterConfig) -> i32 {
    let base = oauth_routing_priorities(Some(&config.oauth_fallback)).oauth_priority;
    existing
        .iter()
        .map(|account| account.priority)
        .max()
        .map(|priority| priority.saturating_add(1).clamp(1, 999))
        .unwrap_or(base.clamp(1, 999))
}

fn detect_google_project(admin: &usage::AdminClient) -> anyhow::Result<String> {
    let body = usage::data(usage::retry_account_read(|| {
        admin.get(
            "/api/v1/admin/accounts?page=1&page_size=200",
            Duration::from_secs(10),
        )
    })?);
    let items = usage::get(&body, "items").unwrap_or(&body);
    let mut projects = HashSet::new();
    for account in usage::array(items) {
        let platform = usage::string(account, "platform");
        if usage::string(account, "type") != "oauth"
            || !matches!(platform.as_str(), "gemini" | "antigravity")
        {
            continue;
        }
        let id = usage::integer(account, "id");
        if id <= 0 {
            continue;
        }
        if let Ok(detail) = admin.get(
            &format!("/api/v1/admin/accounts/{id}"),
            Duration::from_secs(10),
        ) {
            let detail = usage::data(detail);
            let credentials = usage::get(&detail, "credentials").unwrap_or(&Value::Null);
            let project = usage::string(credentials, "project_id");
            if !project.trim().is_empty() {
                projects.insert(project.trim().to_owned());
            }
        }
    }
    Ok(if projects.len() == 1 {
        projects.into_iter().next().unwrap_or_default()
    } else {
        String::new()
    })
}

fn create_authorization(
    admin: &usage::AdminClient,
    provider: Provider,
    gemini: &GeminiOptions,
) -> anyhow::Result<Value> {
    let redirect_uri = provider
        .callback_port()
        .map(|port| format!("http://localhost:{port}/auth/callback"));
    let (path, body) = match provider {
        Provider::OpenAi => (
            "/api/v1/admin/openai/generate-auth-url",
            json!({ "redirect_uri": redirect_uri }),
        ),
        Provider::Anthropic => ("/api/v1/admin/accounts/generate-auth-url", json!({})),
        Provider::Gemini => {
            let mut body = json!({
                "oauth_type": gemini.oauth_type,
                "tier_id": gemini.tier_id,
            });
            if !gemini.project_id.is_empty() {
                body["project_id"] = Value::String(gemini.project_id.clone());
            }
            ("/api/v1/admin/gemini/oauth/auth-url", body)
        }
        Provider::Antigravity => ("/api/v1/admin/antigravity/oauth/auth-url", json!({})),
        Provider::Grok => ("/api/v1/admin/grok/oauth/auth-url", json!({})),
    };
    Ok(usage::data(admin.post(
        path,
        Some(&body),
        Duration::from_secs(30),
    )?))
}

fn force_account_chooser(auth_url: &str, provider: Provider) -> anyhow::Result<String> {
    let mut url = url::Url::parse(auth_url).context("class=invalid_response")?;
    if url.query_pairs().all(|(key, _)| key != "prompt") {
        let value = if matches!(provider, Provider::Gemini | Provider::Antigravity) {
            "select_account"
        } else {
            "login"
        };
        url.query_pairs_mut().append_pair("prompt", value);
    }
    if matches!(provider, Provider::OpenAi | Provider::Grok)
        && url.query_pairs().all(|(key, _)| key != "max_age")
    {
        url.query_pairs_mut().append_pair("max_age", "0");
    }
    Ok(url.into())
}

fn open_authorization_url(auth_url: &str) -> anyhow::Result<()> {
    crate::platform::open_external_https_url(auth_url)
}

#[allow(clippy::too_many_arguments)]
fn create_account(
    admin: &usage::AdminClient,
    provider: Provider,
    session_id: &str,
    code: &str,
    state: &str,
    group_id: i64,
    priority: i32,
    existing: &[ExistingAccount],
    gemini: &GeminiOptions,
) -> anyhow::Result<Value> {
    if provider == Provider::OpenAi {
        if state.trim().is_empty() {
            bail!("class=invalid_response")
        }
        return Ok(usage::data(admin.post(
            "/api/v1/admin/openai/create-from-oauth",
            Some(&json!({
                "session_id": session_id,
                "code": code,
                "state": state,
                "redirect_uri": "http://localhost:1455/auth/callback",
                "name": unique_name(provider.display_name(), existing),
                "concurrency": 3,
                "priority": priority,
                "group_ids": [group_id],
            })),
            Duration::from_secs(90),
        )?));
    }

    let (path, mut exchange_body) = match provider {
        Provider::Anthropic => (
            "/api/v1/admin/accounts/exchange-code",
            json!({ "session_id": session_id, "code": code }),
        ),
        Provider::Gemini => {
            let mut body = json!({
                "session_id": session_id,
                "state": state,
                "code": code,
                "oauth_type": gemini.oauth_type,
                "tier_id": gemini.tier_id,
            });
            if !gemini.project_id.is_empty() {
                body["project_id"] = Value::String(gemini.project_id.clone());
            }
            ("/api/v1/admin/gemini/oauth/exchange-code", body)
        }
        Provider::Antigravity => (
            "/api/v1/admin/antigravity/oauth/exchange-code",
            json!({ "session_id": session_id, "state": state, "code": code }),
        ),
        Provider::Grok => (
            "/api/v1/admin/grok/oauth/exchange-code",
            json!({ "session_id": session_id, "state": state, "code": code }),
        ),
        Provider::OpenAi => unreachable!(),
    };
    if let Some(object) = exchange_body.as_object_mut() {
        object.insert(
            "name".to_owned(),
            Value::String(unique_name(provider.display_name(), existing)),
        );
        object.insert("priority".to_owned(), Value::Number(priority.into()));
        object.insert(
            "group_ids".to_owned(),
            json!([group_id]),
        );
    }
    let exchanged = usage::data(admin.post(
        path,
        Some(&exchange_body),
        Duration::from_secs(90),
    )?);
    // Router Host 2.0 materializes the CLI auth file into the local SQLite
    // account during exchange. Keep accepting the old token-shaped response
    // for older hosts, but do not submit an empty credential object as a new
    // account when the host already returned a real account row.
    if usage::integer(&exchanged, "id") > 0
        && usage::string(&exchanged, "type").eq_ignore_ascii_case("oauth")
    {
        return Ok(exchanged);
    }
    let tokens = exchanged;
    let (credentials, extra) = split_credentials(provider, &tokens);
    let email = first_string(&credentials, &["email", "email_address"])
        .or_else(|| first_string(&extra, &["email", "email_address"]));
    let base_name = provider.display_name();
    let email_name = email.map(|email| format!("{base_name} ({email})"));
    let name = email_name
        .filter(|name| existing.iter().all(|account| account.name != *name))
        .unwrap_or_else(|| unique_name(base_name, existing));
    Ok(usage::data(admin.post(
        "/api/v1/admin/accounts",
        Some(&json!({
            "name": name,
            "notes": "Created by Codex-Router direct OAuth flow.",
            "platform": provider.as_str(),
            "type": "oauth",
            "credentials": credentials,
            "extra": extra,
            "concurrency": 3,
            "priority": priority,
            "rate_multiplier": 1,
            "group_ids": [group_id],
            "auto_pause_on_expired": false,
        })),
        Duration::from_secs(30),
    )?))
}

fn split_credentials(provider: Provider, tokens: &Value) -> (Value, Value) {
    let mut credentials = Map::new();
    let mut extra = Map::new();
    match provider {
        Provider::Anthropic => {
            if let Some(map) = tokens.as_object() {
                credentials.extend(map.clone());
                credentials.remove("extra");
            }
            copy_fields(
                tokens,
                &mut extra,
                &["org_uuid", "account_uuid", "email_address"],
            );
        }
        Provider::Gemini => {
            copy_fields(
                tokens,
                &mut credentials,
                &[
                    "access_token",
                    "refresh_token",
                    "token_type",
                    "expires_at",
                    "scope",
                    "project_id",
                    "oauth_type",
                    "tier_id",
                ],
            );
            if let Some(map) = usage::get(tokens, "extra").and_then(Value::as_object) {
                extra.extend(map.clone());
            }
        }
        Provider::Antigravity => copy_fields(
            tokens,
            &mut credentials,
            &[
                "access_token",
                "refresh_token",
                "token_type",
                "expires_at",
                "project_id",
                "email",
            ],
        ),
        Provider::Grok => {
            copy_fields(
                tokens,
                &mut credentials,
                &[
                    "access_token",
                    "refresh_token",
                    "id_token",
                    "token_type",
                    "expires_at",
                    "client_id",
                    "scope",
                    "email",
                    "sub",
                    "team_id",
                    "subscription_tier",
                    "entitlement_status",
                ],
            );
            credentials.insert(
                "base_url".to_owned(),
                Value::String("https://cli-chat-proxy.grok.com/v1".to_owned()),
            );
            copy_fields(
                tokens,
                &mut extra,
                &["email", "subscription_tier", "entitlement_status"],
            );
        }
        Provider::OpenAi => {}
    }
    (Value::Object(credentials), Value::Object(extra))
}

fn copy_fields(source: &Value, target: &mut Map<String, Value>, names: &[&str]) {
    for name in names {
        if let Some(value) = usage::get(source, name).filter(|value| !value.is_null()) {
            target.insert((*name).to_owned(), value.clone());
        }
    }
}

fn first_string(value: &Value, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .map(|field| usage::string(value, field))
        .find(|value| !value.trim().is_empty())
}

fn unique_name(base: &str, existing: &[ExistingAccount]) -> String {
    if existing.iter().all(|account| account.name != base) {
        return base.to_owned();
    }
    for suffix in 2..=999 {
        let candidate = format!("{base} {suffix}");
        if existing.iter().all(|account| account.name != candidate) {
            return candidate;
        }
    }
    format!("{base} {}", chrono::Utc::now().timestamp())
}

fn reconcile_new_account(
    admin: &usage::AdminClient,
    provider: Provider,
    created: Value,
    existing: &[ExistingAccount],
    group_id: i64,
) -> anyhow::Result<OAuthResult> {
    let created_id = usage::integer(&created, "id");
    if created_id <= 0 {
        bail!("class=invalid_response")
    }
    let detail = usage::data(usage::retry_account_read(|| {
        admin.get(
            &format!("/api/v1/admin/accounts/{created_id}"),
            Duration::from_secs(10),
        )
    })?);
    let credentials = usage::get(&detail, "credentials").unwrap_or(&Value::Null);
    let extra = usage::get(&detail, "extra").unwrap_or(&Value::Null);
    let identity = stable_identity(provider.as_str(), credentials, extra);
    let duplicate = identity.as_ref().and_then(|identity| {
        existing
            .iter()
            .find(|account| account.identity.as_ref() == Some(identity))
    });
    if let Some(existing) = duplicate {
        admin.put(
            &format!("/api/v1/admin/accounts/{}", existing.id),
            &json!({
                "credentials": credentials,
                "extra": extra,
                "status": "active",
                "group_ids": [group_id],
                "priority": existing.priority,
                "confirm_mixed_channel_risk": true,
            }),
            Duration::from_secs(15),
        )?;
        admin.delete(
            &format!("/api/v1/admin/accounts/{created_id}"),
            Duration::from_secs(15),
        )?;
        return Ok(OAuthResult {
            account_id: existing.id,
            account_name: existing.name.clone(),
            reused_existing: true,
        });
    }
    Ok(OAuthResult {
        account_id: created_id,
        account_name: usage::string(&detail, "name"),
        reused_existing: false,
    })
}

pub(super) fn stable_identity(
    platform: &str,
    credentials: &Value,
    extra: &Value,
) -> Option<String> {
    let platform = platform.trim().to_ascii_lowercase();
    let fields: &[(&str, &str)] = match platform.as_str() {
        "openai" => &[
            ("account", "chatgpt_account_id"),
            ("user", "chatgpt_user_id"),
        ],
        "anthropic" => &[
            ("account", "account_uuid"),
            ("organization", "org_uuid"),
            ("email", "email_address"),
        ],
        "gemini" | "antigravity" | "google_one" => &[("email", "email"), ("account", "account_id")],
        "grok" => &[("subject", "sub"), ("team", "team_id"), ("email", "email")],
        _ => &[],
    };
    for (kind, field) in fields {
        let value = first_string(credentials, &[*field]).or_else(|| first_string(extra, &[*field]));
        if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
            return Some(format!(
                "{platform}|{kind}|{}",
                value.trim().to_ascii_lowercase()
            ));
        }
    }
    None
}

fn ensure_scheduled_recovery(
    admin: &usage::AdminClient,
    account_id: i64,
    model_id: &str,
) -> anyhow::Result<()> {
    let path = format!("/api/v1/admin/accounts/{account_id}/scheduled-test-plans");
    let plans = usage::data(usage::retry_account_read(|| {
        admin.get(&path, Duration::from_secs(10))
    })?);
    let plan_id = usage::array(&plans)
        .iter()
        .find(|plan| usage::string(plan, "cron_expression") == "0 * * * *")
        .map(|plan| usage::integer(plan, "id"))
        .unwrap_or_default();
    let body = json!({
        "account_id": account_id,
        "model_id": model_id,
        "cron_expression": "0 * * * *",
        "enabled": true,
        "max_results": 48,
        "auto_recover": true,
    });
    if plan_id > 0 {
        admin.put(
            &format!("/api/v1/admin/scheduled-test-plans/{plan_id}"),
            &body,
            Duration::from_secs(10),
        )?;
    } else {
        admin.post(
            "/api/v1/admin/scheduled-test-plans",
            Some(&body),
            Duration::from_secs(10),
        )?;
    }
    Ok(())
}

fn read_callback(
    stream: &mut TcpStream,
    expected_state: &str,
) -> anyhow::Result<(Zeroizing<String>, String)> {
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    if !request_line.starts_with("GET ") {
        bail!("class=invalid_response")
    }
    let target = request_line
        .split_whitespace()
        .nth(1)
        .context("class=invalid_response")?;
    let url =
        url::Url::parse(&format!("http://localhost{target}")).context("class=invalid_response")?;
    let mut code = None;
    let mut state = String::new();
    let mut callback_error = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(Zeroizing::new(value.into_owned())),
            "state" => state = value.into_owned(),
            "error" => callback_error = Some(value.into_owned()),
            _ => {}
        }
    }
    if callback_error.is_some() {
        bail!("class=permission")
    }
    let code = code
        .filter(|code| !code.trim().is_empty())
        .context("class=invalid_response")?;
    if !expected_state.is_empty() && state != expected_state {
        bail!("class=permission")
    }
    if state.is_empty() {
        state = expected_state.to_owned();
    }
    Ok((code, state))
}

fn send_callback_page(stream: &mut TcpStream, success: bool) -> anyhow::Result<()> {
    let message = if success {
        "Authorization received successfully. You can close this tab."
    } else {
        "Authorization response was rejected. Return to Codex-Router and retry."
    };
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Codex-Router OAuth</title></head><body><h2>{message}</h2></body></html>"
    );
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )?;
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_contracts_keep_callback_ports_and_manual_modes_stable() {
        assert_eq!(Provider::OpenAi.callback_port(), Some(1455));
        assert_eq!(Provider::Antigravity.callback_port(), Some(8085));
        assert_eq!(Provider::Grok.callback_port(), Some(56121));
        assert_eq!(Provider::Anthropic.callback_port(), None);
        assert_eq!(Provider::Gemini.callback_port(), None);
    }

    #[test]
    fn stable_identity_covers_each_provider_without_using_row_ids() {
        assert_eq!(
            stable_identity(
                "openai",
                &json!({ "chatgpt_account_id": "Acct-1" }),
                &Value::Null
            ),
            Some("openai|account|acct-1".to_owned())
        );
        assert_eq!(
            stable_identity(
                "gemini",
                &json!({}),
                &json!({ "email": "User@Example.com" })
            ),
            Some("gemini|email|user@example.com".to_owned())
        );
        assert_eq!(
            stable_identity("grok", &json!({ "sub": "ABC" }), &Value::Null),
            Some("grok|subject|abc".to_owned())
        );
        assert_eq!(stable_identity("unknown", &json!({}), &Value::Null), None);
    }

    #[test]
    fn chooser_rewrite_preserves_existing_query_and_does_not_duplicate_controls() {
        let rewritten =
            force_account_chooser("https://example.com/oauth?client_id=1", Provider::OpenAi)
                .unwrap();
        let url = url::Url::parse(&rewritten).unwrap();
        let pairs = url.query_pairs().collect::<Vec<_>>();
        assert!(pairs
            .iter()
            .any(|(key, value)| key == "prompt" && value == "login"));
        assert!(pairs
            .iter()
            .any(|(key, value)| key == "max_age" && value == "0"));
        let second = force_account_chooser(&rewritten, Provider::OpenAi).unwrap();
        assert_eq!(
            url::Url::parse(&second)
                .unwrap()
                .query_pairs()
                .filter(|(key, _)| key == "prompt")
                .count(),
            1
        );
    }

    #[test]
    fn second_grok_account_keeps_pkce_and_forces_a_fresh_login() {
        let original = "https://accounts.x.ai/oauth2/auth?client_id=grok-cli&code_challenge=pkce-value&state=oauth-state";
        let rewritten = force_account_chooser(original, Provider::Grok).unwrap();
        let url = url::Url::parse(&rewritten).unwrap();
        let pairs = url.query_pairs().collect::<Vec<_>>();

        for (key, expected) in [
            ("client_id", "grok-cli"),
            ("code_challenge", "pkce-value"),
            ("state", "oauth-state"),
            ("prompt", "login"),
            ("max_age", "0"),
        ] {
            assert_eq!(
                pairs
                    .iter()
                    .filter(|(candidate, _)| candidate == key)
                    .map(|(_, value)| value.as_ref())
                    .collect::<Vec<_>>(),
                vec![expected],
                "unexpected {key} query parameter"
            );
        }
    }

    #[test]
    fn every_oauth_provider_uses_the_system_https_handler() {
        let long_state = "x".repeat(4096);
        for provider in [
            Provider::OpenAi,
            Provider::Anthropic,
            Provider::Gemini,
            Provider::Antigravity,
            Provider::Grok,
        ] {
            let url = format!(
                "https://auth.example.test/{}/authorize?state={long_state}&prompt=login",
                provider.as_str()
            );
            assert_eq!(
                crate::platform::external_https_url(&url).unwrap(),
                url,
                "{} OAuth URL was not preserved",
                provider.as_str()
            );
        }
        assert!(crate::platform::external_https_url(r"C:\Users\test\Documents").is_err());
        assert!(crate::platform::external_https_url("file:///C:/Users/test/Documents").is_err());
        assert!(crate::platform::external_https_url("http://example.test/oauth").is_err());
    }

    #[test]
    fn callback_parser_rejects_state_mismatch_and_accepts_matching_state() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let sender = std::thread::spawn(move || {
            let mut client = TcpStream::connect(address).unwrap();
            client
                .write_all(b"GET /auth/callback?code=secret&state=right HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .unwrap();
        });
        let (mut stream, _) = listener.accept().unwrap();
        let (code, state) = read_callback(&mut stream, "right").unwrap();
        sender.join().unwrap();
        assert_eq!(code.as_str(), "secret");
        assert_eq!(state, "right");

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let sender = std::thread::spawn(move || {
            let mut client = TcpStream::connect(address).unwrap();
            client
                .write_all(b"GET /auth/callback?code=secret&state=wrong HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .unwrap();
        });
        let (mut stream, _) = listener.accept().unwrap();
        assert!(read_callback(&mut stream, "right").is_err());
        sender.join().unwrap();
    }

    #[test]
    fn manual_callback_accepts_full_url_and_rejects_wrong_state() {
        let (code, state) = parse_manual_authorization(
            "http://localhost:1455/auth/callback?code=secret%2Dcode&state=right",
            "right",
        )
        .unwrap();
        assert_eq!(code.as_str(), "secret-code");
        assert_eq!(state, "right");
        assert!(parse_manual_authorization(
            "http://localhost:1455/auth/callback?code=secret&state=wrong",
            "right"
        )
        .is_err());
        let (code, state) = parse_manual_authorization("plain-code", "right").unwrap();
        assert_eq!(code.as_str(), "plain-code");
        assert_eq!(state, "right");
    }

    #[test]
    fn credential_split_keeps_provider_specific_fields_only() {
        let tokens = json!({
            "access_token": "a",
            "refresh_token": "r",
            "email": "user@example.com",
            "subscription_tier": "premium",
            "unrelated": "drop-me"
        });
        let (credentials, extra) = split_credentials(Provider::Grok, &tokens);
        assert!(usage::get(&credentials, "access_token").is_some());
        assert!(usage::get(&credentials, "unrelated").is_none());
        assert_eq!(usage::string(&extra, "subscription_tier"), "premium");
    }
    #[test]
    fn first_new_account_returns_the_unseen_id() {
        let known = vec![ExistingAccount {
            id: 11,
            name: "Grok one".to_owned(),
            priority: 1,
            identity: Some("grok|subject|a".to_owned()),
        }];
        let current = vec![
            ExistingAccount {
                id: 11,
                name: "Grok one".to_owned(),
                priority: 1,
                identity: Some("grok|subject|a".to_owned()),
            },
            ExistingAccount {
                id: 24,
                name: "Grok three".to_owned(),
                priority: 3,
                identity: Some("grok|subject|c".to_owned()),
            },
        ];
        let found = first_new_account(&known, current).expect("new account");
        assert_eq!(found.account_id, 24);
        assert_eq!(found.account_name, "Grok three");
        assert!(!found.reused_existing);
    }

    #[test]
    fn first_new_account_ignores_already_known_ids() {
        let known = vec![ExistingAccount {
            id: 11,
            name: "Grok one".to_owned(),
            priority: 1,
            identity: Some("grok|subject|a".to_owned()),
        }];
        let current = vec![ExistingAccount {
            id: 11,
            name: "Grok one".to_owned(),
            priority: 1,
            identity: Some("grok|subject|a".to_owned()),
        }];
        assert!(first_new_account(&known, current).is_none());
    }

}
