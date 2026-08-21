//! Codex `config.toml` generation and update logic, migrated from
//! `scripts/CodexIntegration.psm1`.

#![allow(dead_code, clippy::too_many_arguments)]

use crate::config::RouterConfig;
use anyhow::{bail, Context};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item};

const CODEX_ROUTER_REQUEST_MAX_RETRIES: i64 = 1;
// The local gateway owns retry classification and staged backoff. A second
// Codex retry loop would replay the same request with a fixed UI cadence.
const CODEX_ROUTER_STREAM_MAX_RETRIES: i64 = 0;

const REASONING_LEVELS: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

/// Ensure `base_url` points to a local HTTP endpoint as required by Codex-Router.
fn validate_local_base_url(base_url: &str) -> anyhow::Result<String> {
    let url = url::Url::parse(base_url.trim_end_matches('/')).context("invalid Codex base URL")?;
    if url.scheme() != "http" {
        bail!("Codex-Router provider URL must use http");
    }
    let host = url.host_str().unwrap_or("");
    if host != "127.0.0.1" && host != "localhost" {
        bail!("Codex-Router provider URL must point to 127.0.0.1 or localhost");
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn normalize_reasoning_effort(value: &str) -> anyhow::Result<String> {
    let effort = value.trim().to_ascii_lowercase();
    if !REASONING_LEVELS.contains(&effort.as_str()) {
        bail!("unsupported Codex reasoning effort: '{}'", value);
    }
    Ok(effort)
}

fn table_string<'a>(item: &'a Item, key: &str) -> Option<&'a str> {
    item.as_table_like()?.get(key)?.as_str()
}

fn is_legacy_custom_router_provider(item: &Item) -> bool {
    let name = table_string(item, "name").unwrap_or("").trim();
    let base_url = table_string(item, "base_url")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let bearer = table_string(item, "experimental_bearer_token")
        .unwrap_or("")
        .trim();

    matches!(
        name,
        "Codex-Router" | "Codex Router" | "Codex Unified Router"
    ) || base_url.contains("127.0.0.1:18081")
        || (base_url.contains("127.0.0.1:15721/v1") && bearer == "PROXY_MANAGED")
}

/// Generate the new Codex `config.toml` text from an existing document (or empty string).
#[allow(clippy::too_many_arguments)]
pub fn generate_codex_router_config(
    existing: &str,
    model: &str,
    catalog_path: &str,
    local_api_key: &str,
    base_url: &str,
    reasoning_effort: &str,
    fast_mode: bool,
    require_openai_auth: bool,
    display_openai_provider: bool,
    permission_source: Option<&str>,
) -> anyhow::Result<String> {
    if local_api_key.is_empty() {
        bail!("the local Router credential is required for Codex integration");
    }
    let base = validate_local_base_url(base_url)?;
    let reasoning_effort = normalize_reasoning_effort(reasoning_effort)?;
    let catalog_path = catalog_path.replace('\\', "/");

    let mut doc: DocumentMut = if existing.is_empty() {
        DocumentMut::new()
    } else {
        existing
            .parse()
            .context("existing Codex config.toml is not valid TOML")?
    };

    // Remove keys that Codex-Router now owns through the provider block.
    for key in [
        "base_url",
        "wire_api",
        "experimental_bearer_token",
        "openai_base_url",
        "service_tier",
        "disable_response_storage",
    ] {
        doc.remove(key);
    }

    doc.insert("model_provider", toml_edit::value("codex_router"));
    doc.insert("model", toml_edit::value(model));
    doc.insert("model_catalog_json", toml_edit::value(&catalog_path));
    doc.insert(
        "model_reasoning_effort",
        toml_edit::value(&reasoning_effort),
    );
    // `authMode` describes which upstream routes the local Router may use. It
    // must not restrict Codex's own sign-in method. Strip a leftover ChatGPT
    // lock only when this machine is using API-key login.
    if !require_openai_auth
        && doc.get("forced_login_method").and_then(Item::as_str) == Some("chatgpt")
    {
        doc.remove("forced_login_method");
    }
    // features.apps = false
    let mut features = doc
        .remove("features")
        .and_then(|item| item.into_table().ok())
        .unwrap_or_default();
    features.insert("apps", toml_edit::value(false));

    // models.new_thread defaults
    let mut new_thread = toml_edit::Table::new();
    new_thread.insert("model", toml_edit::value(model));
    new_thread.insert(
        "model_reasoning_effort",
        toml_edit::value(&reasoning_effort),
    );

    if fast_mode {
        doc.insert("service_tier", toml_edit::value("fast"));
        new_thread.insert("service_tier", toml_edit::value("fast"));
    }
    // Keep the feature-visibility gate aligned with the requested mode. The
    // service tier and the feature flag must be cleared together when Fast is
    // disabled, otherwise a later Codex reload resurrects the old Fast state.
    features.insert("fast_mode", toml_edit::value(fast_mode));

    // Copy permission-related settings from a source document.
    let mut windows = doc
        .remove("windows")
        .and_then(|item| item.into_table().ok())
        .unwrap_or_default();
    if let Some(source) = permission_source {
        if !source.is_empty() {
            let source_doc: DocumentMut = source
                .parse()
                .context("permission source config.toml is not valid TOML")?;
            for key in ["approval_policy", "sandbox_mode"] {
                if let Some(item) = source_doc.get(key) {
                    doc.insert(key, item.clone());
                }
            }
            if let Some(Item::Table(source_windows)) = source_doc.get("windows") {
                let source_sandbox = source_windows.get("sandbox").and_then(|item| item.as_str());
                let target_elevated = windows
                    .get("sandbox")
                    .and_then(|item| item.as_str())
                    .map(|s| s == "elevated")
                    .unwrap_or(false);
                if !target_elevated && source_sandbox == Some("elevated") {
                    windows.insert("sandbox", toml_edit::value("elevated"));
                } else if !target_elevated && source_sandbox.is_some() {
                    windows.insert("sandbox", source_windows.get("sandbox").unwrap().clone());
                }
            }
        }
    }

    // Merge [desktop]: only own enabled-reasoning-efforts. Layout, window,
    // plugin and other far-side keys stay so cloud sync cannot wipe local
    // typesetting with an older remote document, and Router apply cannot
    // wipe a newer Desktop layout.
    let mut desktop = doc
        .remove("desktop")
        .and_then(|item| item.into_table().ok())
        .unwrap_or_default();
    if !desktop.contains_key("enabled-reasoning-efforts") {
        let levels = ["low", "medium", "high", "xhigh", "ultra", "max"];
        let mut arr = toml_edit::Array::new();
        for level in levels {
            arr.push(level);
        }
        desktop.insert("enabled-reasoning-efforts", toml_edit::value(arr));
    }
    merge_desktop_overlay(&mut desktop, existing);

    // Assemble tables into the document.
    {
        let mut models = doc
            .remove("models")
            .and_then(|item| item.into_table().ok())
            .unwrap_or_default();
        models.insert("new_thread", toml_edit::Item::Table(new_thread));
        doc["models"] = toml_edit::Item::Table(models);
    }
    let mut agents = doc
        .remove("agents")
        .and_then(|item| item.into_table().ok())
        .unwrap_or_default();
    if super::responses_compat::is_openai_family_model(model) {
        agents.insert("default_subagent_model", toml_edit::value(model));
        agents.insert(
            "default_subagent_reasoning_effort",
            toml_edit::value(&reasoning_effort),
        );
    } else {
        // The two defaults are Router-managed. Leaving an OpenAI v2 default in
        // place while the selected model is DeepSeek/Gemini/Kimi/Grok makes
        // Codex emit v2 agent_message payloads even though their catalog entry
        // deliberately advertises multi-agent v1.
        agents.remove("default_subagent_model");
        agents.remove("default_subagent_reasoning_effort");
    }
    if !agents.is_empty() {
        doc["agents"] = toml_edit::Item::Table(agents);
    }
    doc["features"] = toml_edit::Item::Table(features);
    doc["windows"] = toml_edit::Item::Table(windows);
    doc["desktop"] = toml_edit::Item::Table(desktop);
    // Remove legacy provider tables.
    if let Some(model_providers) = doc["model_providers"].as_table_like_mut() {
        model_providers.remove("codex_router");
        model_providers.remove("sub2api");
        model_providers.remove("codex_loopback_proxy");
        if model_providers
            .get("custom")
            .is_some_and(is_legacy_custom_router_provider)
        {
            model_providers.remove("custom");
        }
    }

    // Insert the new provider block.
    let provider = build_codex_router_provider(
        &base,
        local_api_key,
        require_openai_auth,
        display_openai_provider,
    );
    let model_providers =
        doc["model_providers"].or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    model_providers["codex_router"] = toml_edit::Item::Table(provider);

    let mut text = doc.to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(text)
}

fn build_codex_router_provider(
    base: &str,
    local_api_key: &str,
    require_openai_auth: bool,
    display_openai_provider: bool,
) -> toml_edit::Table {
    let mut provider = toml_edit::Table::new();
    provider.insert(
        "name",
        toml_edit::value(if display_openai_provider {
            "OpenAI"
        } else {
            "Codex-Router"
        }),
    );
    provider.insert("base_url", toml_edit::value(format!("{}/v1", base)));
    provider.insert("wire_api", toml_edit::value("responses"));
    provider.insert(
        "requires_openai_auth",
        toml_edit::value(require_openai_auth),
    );
    provider.insert("experimental_bearer_token", toml_edit::value(local_api_key));
    provider.insert(
        "request_max_retries",
        toml_edit::value(CODEX_ROUTER_REQUEST_MAX_RETRIES),
    );
    provider.insert(
        "stream_max_retries",
        toml_edit::value(CODEX_ROUTER_STREAM_MAX_RETRIES),
    );
    provider.insert("stream_idle_timeout_ms", toml_edit::value(1_800_000_i64));
    provider.insert("supports_websockets", toml_edit::value(false));
    provider
}

fn read_permission_source(codex_home: &Path, current: &str) -> anyhow::Result<Option<String>> {
    if current_has_permission_keys(current)? {
        return Ok(Some(current.to_string()));
    }
    // If the current config already has a Codex-Router provider block but no
    // permission settings, consult the most recent backups for those keys.
    if !current.contains("[model_providers.codex_router]") {
        return Ok(None);
    }
    let mut candidates = std::fs::read_dir(codex_home)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
                name.starts_with("config.toml.codex-router-") && name.ends_with(".bak")
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|entry| {
        entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH)
    });
    candidates.reverse();
    for entry in candidates {
        let candidate = std::fs::read_to_string(entry.path())
            .with_context(|| format!("failed to read backup {:?}", entry.path()))?;
        for key in ["approval_policy", "sandbox_mode"] {
            if doc_contains_top_level(&candidate, key)? {
                return Ok(Some(candidate));
            }
        }
        if doc_contains_table_key(&candidate, "windows", "sandbox")? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn doc_contains_top_level(text: &str, key: &str) -> anyhow::Result<bool> {
    if text.is_empty() {
        return Ok(false);
    }
    let doc: DocumentMut = text
        .parse()
        .context("backup config.toml is not valid TOML")?;
    Ok(doc.get(key).is_some())
}

fn doc_contains_table_key(text: &str, table: &str, key: &str) -> anyhow::Result<bool> {
    if text.is_empty() {
        return Ok(false);
    }
    let doc: DocumentMut = text
        .parse()
        .context("backup config.toml is not valid TOML")?;
    Ok(doc.get(table).and_then(|item| item.get(key)).is_some())
}

fn current_has_permission_keys(text: &str) -> anyhow::Result<bool> {
    if text.is_empty() {
        return Ok(false);
    }
    for key in ["approval_policy", "sandbox_mode"] {
        if doc_contains_top_level(text, key)? {
            return Ok(true);
        }
    }
    doc_contains_table_key(text, "windows", "sandbox")
}

fn limit_backups(codex_home: &Path, filter: &str, keep: usize) -> anyhow::Result<()> {
    let prefix = filter.trim_start_matches('*').trim_end_matches('*');
    let ext = std::path::Path::new(prefix)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let stem = std::path::Path::new(prefix)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let mut files: Vec<_> = std::fs::read_dir(codex_home)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(stem) && name.ends_with(ext))
        })
        .collect();
    files.sort_by_key(|entry| {
        entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH)
    });
    files.reverse();
    for entry in files.into_iter().skip(keep) {
        let path = entry.path();
        if path.is_file() {
            std::fs::remove_file(&path)
                .with_context(|| format!("failed to remove old backup {:?}", path))?;
        }
    }
    Ok(())
}

fn codex_public_base_url(cfg: &RouterConfig) -> String {
    super::responses_gateway::responses_gateway_url(&cfg.deploy.sub2api_host).unwrap_or_else(|_| {
        cfg.deploy
            .sub2api_host
            .trim()
            .trim_end_matches('/')
            .to_owned()
    })
}
/// Write the Codex `config.toml` to `codex_home`, preserving a timestamped backup and
/// keeping only the most recent backups.
pub fn write_codex_router_config(
    codex_home: &Path,
    model: &str,
    catalog_path: &Path,
    local_api_key: &str,
    base_url: &str,
    reasoning_effort: &str,
    fast_mode: bool,
    require_openai_auth: bool,
    display_openai_provider: bool,
    permission_source: Option<&str>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(codex_home)?;
    let config_path = codex_home.join("config.toml");
    let existing = if config_path.is_file() {
        std::fs::read_to_string(&config_path)
            .context("failed to read existing Codex config.toml")?
    } else {
        String::new()
    };
    let permission_source = if let Some(content) = permission_source {
        if content.is_empty() {
            None
        } else {
            Some(content.to_owned())
        }
    } else {
        read_permission_source(codex_home, &existing)?
    };
    let catalog_path_str = catalog_path.to_string_lossy().into_owned();
    let overlay_path = codex_home.join("codex-router-desktop-overlay.toml");
    persist_desktop_overlay_file(&overlay_path, &existing);
    let mut generated = generate_codex_router_config(
        &existing,
        model,
        &catalog_path_str,
        local_api_key,
        base_url,
        reasoning_effort,
        fast_mode,
        require_openai_auth,
        display_openai_provider,
        permission_source.as_deref(),
    )?;
    generated = apply_desktop_overlay_file(&generated, &overlay_path);

    if !existing.is_empty() && existing.trim() != generated.trim() {
        let id = chrono::Local::now().format("%Y%m%d%H%M%S%3f").to_string();
        let backup_name = format!("config.toml.codex-router-{}.bak", id);
        let backup_path = codex_home.join(&backup_name);
        std::fs::write(&backup_path, &existing)
            .context("failed to write Codex config.toml backup")?;
        limit_backups(codex_home, "config.toml.codex-router-*.bak", 3)
            .context("failed to limit Codex config.toml backups")?;
    }

    crate::config::atomic_write(&config_path, generated.as_bytes())
        .context("failed to write Codex config.toml")?;
    Ok(())
}

fn desktop_overlay_path(router_root: &Path) -> PathBuf {
    crate::user_data::state_root(router_root).join("codex-desktop-overlay.toml")
}

fn extract_desktop_overlay(existing: &str) -> Option<toml_edit::Table> {
    let document: DocumentMut = existing.parse().ok()?;
    let desktop = document.get("desktop")?.as_table()?;
    let mut overlay = toml_edit::Table::new();
    for (key, item) in desktop.iter() {
        if key != "enabled-reasoning-efforts" {
            overlay.insert(key, item.clone());
        }
    }
    (!overlay.is_empty()).then_some(overlay)
}

fn merge_desktop_overlay(desktop: &mut toml_edit::Table, existing: &str) {
    let Some(overlay) = extract_desktop_overlay(existing) else {
        return;
    };
    for (key, item) in overlay.iter() {
        if key != "enabled-reasoning-efforts" && !desktop.contains_key(key) {
            desktop.insert(key, item.clone());
        }
    }
}

fn persist_desktop_overlay_file(path: &Path, existing: &str) {
    let mut overlay = extract_desktop_overlay(existing).unwrap_or_default();
    if let Ok(previous) = std::fs::read_to_string(path) {
        if let Some(previous_overlay) = extract_desktop_overlay(&previous) {
            for (key, item) in previous_overlay.iter() {
                if key != "enabled-reasoning-efforts" && !overlay.contains_key(key) {
                    overlay.insert(key, item.clone());
                }
            }
        }
    }
    if overlay.is_empty() {
        return;
    }
    let mut document = DocumentMut::new();
    document["desktop"] = toml_edit::Item::Table(overlay);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = crate::config::atomic_write(path, document.to_string().as_bytes());
}

fn apply_desktop_overlay_file(generated: &str, path: &Path) -> String {
    let Ok(overlay) = std::fs::read_to_string(path) else {
        return generated.to_owned();
    };
    let Ok(mut document) = generated.parse::<DocumentMut>() else {
        return generated.to_owned();
    };
    let mut desktop = document
        .remove("desktop")
        .and_then(|item| item.into_table().ok())
        .unwrap_or_default();
    merge_desktop_overlay(&mut desktop, &overlay);
    document["desktop"] = toml_edit::Item::Table(desktop);
    let mut text = document.to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn resolve_codex_home(cfg: &RouterConfig) -> PathBuf {
    if !cfg.deploy.codex_home.is_empty() {
        return PathBuf::from(&cfg.deploy.codex_home);
    }
    if let Some(path) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(path);
    }
    dirs::home_dir()
        .map(|home| home.join(".codex"))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

pub fn write_codex_config_from_router_config(
    cfg: &RouterConfig,
    router_root: &Path,
) -> anyhow::Result<()> {
    write_codex_config_from_router_config_impl(cfg, router_root, &codex_system_config_path())
}

pub(crate) fn write_codex_config_from_router_config_impl(
    cfg: &RouterConfig,
    router_root: &Path,
    system_config_path: &Path,
) -> anyhow::Result<()> {
    let codex_home = resolve_codex_home(cfg);
    let existing = std::fs::read_to_string(codex_home.join("config.toml")).unwrap_or_default();
    let mut settings = if existing.trim().is_empty() {
        codex_router_settings(cfg)
    } else {
        // Keep the model and reasoning already shown in Desktop. Saving Router
        // settings must refresh the local binding, not yank the user back to
        // Router's default_model.
        codex_router_repair_settings(cfg, &existing)
    };
    settings.require_openai_auth = login_requires_openai_auth(&codex_home, cfg, &existing);
    let catalog_path = crate::user_data::state_root(router_root).join("model-catalog.json");
    let local_api_key = super::ensure_local_api_key()?;
    persist_desktop_overlay_file(&desktop_overlay_path(router_root), &existing);
    write_codex_router_config(
        &codex_home,
        &settings.model,
        &catalog_path,
        &local_api_key,
        &super::responses_gateway::responses_gateway_url(&cfg.deploy.sub2api_host)?,
        &settings.reasoning_effort,
        settings.fast_mode,
        settings.require_openai_auth,
        settings.display_openai_provider,
        None,
    )?;
    // Mirror the binding into the system layer so a Codex Desktop rewrite of
    // the user config.toml can no longer sever non-ChatGPT model routing.
    write_codex_system_binding_to(
        system_config_path,
        &catalog_path,
        &local_api_key,
        &codex_public_base_url(cfg),
        settings.require_openai_auth,
        settings.display_openai_provider,
    )?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CodexRouterSettings {
    model: String,
    reasoning_effort: String,
    fast_mode: bool,
    require_openai_auth: bool,
    display_openai_provider: bool,
}

fn codex_router_settings(cfg: &RouterConfig) -> CodexRouterSettings {
    let default_route = super::resolve_default_route(cfg);
    let model = default_route
        .as_ref()
        .map(|route| route.public_model_id.clone())
        .unwrap_or_else(|| "gpt-5.6-sol".to_owned());
    let (reasoning_effort, supports_fast) = default_route
        .as_ref()
        .map(|route| {
            let reasoning = super::resolve_reasoning(&route.model, Some(&cfg.reasoning));
            (
                reasoning.default_level,
                reasoning.supports_fast,
            )
        })
        .unwrap_or_else(|| ("medium".to_owned(), false));
    CodexRouterSettings {
        model,
        reasoning_effort,
        fast_mode: supports_fast,
        require_openai_auth: !cfg.auth_mode.trim().eq_ignore_ascii_case("local_api_key"),
        display_openai_provider: false,
    }
}

fn codex_router_repair_settings(cfg: &RouterConfig, existing: &str) -> CodexRouterSettings {
    let mut settings = codex_router_settings(cfg);
    let Ok(document) = existing.parse::<DocumentMut>() else {
        return settings;
    };
    let selected_model = document
        .get("model")
        .and_then(Item::as_str)
        .map(str::to_owned);
    let Some(selected_model) = selected_model else {
        return settings;
    };
    let Some(route) = super::catalog::build_route_plan(cfg)
        .into_iter()
        .find(|route| {
            route.include_in_catalog
                && (route.public_model_id == selected_model || route.model.model == selected_model)
        })
    else {
        return settings;
    };
    let reasoning = super::resolve_reasoning(&route.model, Some(&cfg.reasoning));
    settings.model = route.public_model_id;
    settings.reasoning_effort = document
        .get("model_reasoning_effort")
        .and_then(Item::as_str)
        .filter(|value| REASONING_LEVELS.contains(&value.to_ascii_lowercase().as_str()))
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or(reasoning.default_level);
    settings.fast_mode = reasoning.supports_fast;
    settings
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodexLoginKind {
    Chatgpt,
    ApiKey,
    Unknown,
}

fn read_codex_login_kind(codex_home: &Path) -> CodexLoginKind {
    let Ok(text) = std::fs::read_to_string(codex_home.join("auth.json")) else {
        return CodexLoginKind::Unknown;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return CodexLoginKind::Unknown;
    };
    match value.get("auth_mode").and_then(|item| item.as_str()) {
        Some("chatgpt") if value.get("tokens").is_some_and(serde_json::Value::is_object) => {
            CodexLoginKind::Chatgpt
        }
        Some("apikey") => CodexLoginKind::ApiKey,
        _ => {
            if value
                .get("OPENAI_API_KEY")
                .and_then(|item| item.as_str())
                .is_some_and(|key| !key.trim().is_empty())
            {
                CodexLoginKind::ApiKey
            } else {
                CodexLoginKind::Unknown
            }
        }
    }
}

fn existing_provider_requires_openai_auth(existing: &str) -> Option<bool> {
    existing
        .parse::<DocumentMut>()
        .ok()?
        .get("model_providers")?
        .as_table_like()?
        .get("codex_router")?
        .as_table_like()?
        .get("requires_openai_auth")?
        .as_bool()
}

fn login_requires_openai_auth(codex_home: &Path, cfg: &RouterConfig, existing: &str) -> bool {
    match read_codex_login_kind(codex_home) {
        CodexLoginKind::Chatgpt => true,
        CodexLoginKind::ApiKey => false,
        CodexLoginKind::Unknown => {
            if cfg.auth_mode.trim().eq_ignore_ascii_case("local_api_key") {
                false
            } else {
                existing_provider_requires_openai_auth(existing).unwrap_or(true)
            }
        }
    }
}

fn codex_router_binding_matches(
    cfg: &RouterConfig,
    router_root: &Path,
    existing: &str,
    local_api_key: &str,
) -> bool {
    if local_api_key.is_empty()
        || !super::codex_config_uses_router(existing, &codex_public_base_url(cfg))
    {
        return false;
    }
    let Ok(document) = existing.parse::<DocumentMut>() else {
        return false;
    };
    if document.get("model_provider").and_then(Item::as_str) != Some("codex_router") {
        return false;
    }
    let Some(provider) = document
        .get("model_providers")
        .and_then(Item::as_table_like)
        .and_then(|providers| providers.get("codex_router"))
        .and_then(Item::as_table_like)
    else {
        return false;
    };
    if provider.get("name").and_then(Item::as_str) != Some("Codex-Router")
        || provider
            .get("experimental_bearer_token")
            .and_then(Item::as_str)
            != Some(local_api_key)
        || provider
            .get("requires_openai_auth")
            .and_then(Item::as_bool)
            != Some(login_requires_openai_auth(
                &resolve_codex_home(cfg),
                cfg,
                existing,
            ))
    {
        return false;
    }

    let expected_catalog = crate::user_data::state_root(router_root)
        .join("model-catalog.json")
        .to_string_lossy()
        .replace('\\', "/");
    let Some(actual_catalog) = document
        .get("model_catalog_json")
        .and_then(Item::as_str)
        .map(|value| value.replace('\\', "/"))
    else {
        return false;
    };
    if cfg!(windows) {
        actual_catalog.eq_ignore_ascii_case(&expected_catalog)
    } else {
        actual_catalog == expected_catalog
    }
}

pub(crate) fn repair_codex_router_binding_with_key(
    cfg: &RouterConfig,
    router_root: &Path,
    local_api_key: &str,
) -> anyhow::Result<bool> {
    repair_codex_router_binding_impl(cfg, router_root, local_api_key, &codex_system_config_path())
}

pub(crate) fn repair_codex_router_binding_impl(
    cfg: &RouterConfig,
    router_root: &Path,
    local_api_key: &str,
    system_config_path: &Path,
) -> anyhow::Result<bool> {
    let codex_home = resolve_codex_home(cfg);
    let config_path = codex_home.join("config.toml");
    let existing = match std::fs::read_to_string(&config_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).context("failed to read Codex config for Router repair"),
    };
    let catalog_path = crate::user_data::state_root(router_root).join("model-catalog.json");
    if codex_router_binding_matches(cfg, router_root, &existing, local_api_key) {
        // The user-layer binding is intact; make sure the system-layer safety
        // net exists as well so later external rewrites cannot sever routing.
        write_codex_system_binding_to(
            system_config_path,
            &catalog_path,
            local_api_key,
            &codex_public_base_url(cfg),
            login_requires_openai_auth(&codex_home, cfg, &existing),
            false,
        )?;
        return Ok(false);
    }

    let mut settings = codex_router_repair_settings(cfg, &existing);
    settings.require_openai_auth = login_requires_openai_auth(&codex_home, cfg, &existing);
    write_codex_router_config(
        &codex_home,
        &settings.model,
        &catalog_path,
        local_api_key,
        &codex_public_base_url(cfg),
        &settings.reasoning_effort,
        settings.fast_mode,
        settings.require_openai_auth,
        settings.display_openai_provider,
        None,
    )?;

    let repaired = std::fs::read_to_string(&config_path)
        .context("failed to verify the repaired Codex Router binding")?;
    if !super::codex_config_uses_router(&repaired, &codex_public_base_url(cfg)) {
        bail!("CODEX_ROUTER_BINDING_REPAIR_INCOMPLETE");
    }
    write_codex_system_binding_to(
        system_config_path,
        &catalog_path,
        local_api_key,
        &codex_public_base_url(cfg),
        settings.require_openai_auth,
        settings.display_openai_provider,
    )?;
    Ok(true)
}

pub fn repair_codex_router_binding(cfg: &RouterConfig, router_root: &Path) -> anyhow::Result<bool> {
    let local_api_key = super::ensure_local_api_key()?;
    repair_codex_router_binding_with_key(cfg, router_root, &local_api_key)
}

/// Detection-only counterpart of [`repair_codex_router_binding`]: reports
/// whether an external program overwrote Codex's native `config.toml` so the
/// local Router binding no longer matches. Returns the fingerprint of the
/// overwritten content so the app can remember a user's "keep it" decision
/// until the file changes again. `Ok(None)` means the binding is intact.
pub fn probe_codex_router_binding_overwrite(
    cfg: &RouterConfig,
    router_root: &Path,
) -> anyhow::Result<Option<String>> {
    let local_api_key = super::ensure_local_api_key()?;
    probe_codex_router_binding_overwrite_with_key(cfg, router_root, &local_api_key)
}

/// Full binding health across the user layer and the mirrored system layer.
/// Codex Desktop periodically strips the user file; as long as the system
/// layer still carries the Router binding, routing keeps working and the app
/// must not nag the user with the overwrite prompt.
#[derive(Clone, Debug)]
pub struct CodexBindingProbe {
    /// Fingerprint of the user-layer content, used for keep-decision
    /// suppression when the binding is truly lost.
    pub fingerprint: String,
    /// The user `config.toml` still points at the Router by itself.
    pub user_layer_bound: bool,
    /// The `%ProgramData%` system layer still carries a valid Router binding.
    pub system_layer_bound: bool,
    /// The user-layer `model` value before any repair, when present.
    pub user_model: Option<String>,
    /// `Some((previous, repaired))` when an invalid/missing user-layer `model`
    /// key (for example `model = "first"` after a ChatGPT update) was silently
    /// rewritten to the Router default model. Repairing only runs while at
    /// least one layer still routes through the local gateway.
    pub model_repair: Option<(Option<String>, String)>,
}

pub fn probe_codex_binding_state(
    cfg: &RouterConfig,
    router_root: &Path,
) -> anyhow::Result<CodexBindingProbe> {
    let local_api_key = super::ensure_local_api_key()?;
    probe_codex_binding_state_with_key(
        cfg,
        router_root,
        &local_api_key,
        &codex_system_config_path(),
    )
}

pub(crate) fn probe_codex_binding_state_with_key(
    cfg: &RouterConfig,
    _router_root: &Path,
    _local_api_key: &str,
    system_config_path: &Path,
) -> anyhow::Result<CodexBindingProbe> {
    let codex_home = resolve_codex_home(cfg);
    let config_path = codex_home.join("config.toml");
    let existing = match std::fs::read_to_string(&config_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).context("failed to read Codex config for overwrite detection")
        }
    };
    let base_url = codex_public_base_url(cfg);
    let user_layer_bound =
        !existing.trim().is_empty() && super::codex_config_uses_router(&existing, &base_url);
    let system_content = std::fs::read_to_string(system_config_path).unwrap_or_default();
    let system_layer_bound = system_content
        .lines()
        .any(|line| line.trim() == SYSTEM_BINDING_MARKER)
        && super::codex_config_uses_router(&system_content, &base_url);
    let user_model = existing
        .parse::<DocumentMut>()
        .ok()
        .and_then(|document| {
            document
                .get("model")
                .and_then(Item::as_str)
                .map(str::to_owned)
        });
    let mut model_repair = None;
    if (user_layer_bound || system_layer_bound) && !existing.trim().is_empty() {
        model_repair = repair_codex_user_model_key(cfg, &existing, &config_path)?;
    }
    Ok(CodexBindingProbe {
        fingerprint: codex_config_fingerprint(&existing),
        user_layer_bound,
        system_layer_bound,
        user_model,
        model_repair,
    })
}

/// Silently rewrite the user-layer `model` key when Codex Desktop replaced it
/// with a value outside the Router catalog. The Desktop model picker falls
/// back to "Custom" for unknown ids, which hides the configured default
/// model even though routing itself still works. Only the single key is
/// touched so Desktop-owned content (desktop section, plugins, projects)
/// survives the repair byte-for-byte. Returns `Some((old, new))` on rewrite.
fn repair_codex_user_model_key(
    cfg: &RouterConfig,
    existing: &str,
    config_path: &Path,
) -> anyhow::Result<Option<(Option<String>, String)>> {
    let Ok(mut document) = existing.parse::<DocumentMut>() else {
        return Ok(None);
    };
    let valid: Vec<String> = super::catalog::build_route_plan(cfg)
        .iter()
        .filter(|route| route.include_in_catalog)
        .map(|route| route.public_model_id.clone())
        .collect();
    if valid.is_empty() {
        return Ok(None);
    }
    let current = document
        .get("model")
        .and_then(Item::as_str)
        .map(str::to_owned);
    if current
        .as_deref()
        .is_some_and(|model| valid.iter().any(|candidate| candidate == model))
    {
        return Ok(None);
    }
    let target = if valid.iter().any(|candidate| candidate == &cfg.default_model) {
        cfg.default_model.clone()
    } else {
        valid[0].clone()
    };
    document["model"] = toml_edit::value(target.as_str());
    std::fs::write(config_path, document.to_string())
        .context("failed to repair the Codex user-layer model key")?;
    Ok(Some((current, target)))
}

pub(crate) fn probe_codex_router_binding_overwrite_with_key(
    cfg: &RouterConfig,
    _router_root: &Path,
    _local_api_key: &str,
) -> anyhow::Result<Option<String>> {
    let codex_home = resolve_codex_home(cfg);
    let config_path = codex_home.join("config.toml");
    let existing = match std::fs::read_to_string(&config_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).context("failed to read Codex config for overwrite detection")
        }
    };
    if !existing.trim().is_empty()
        && super::codex_config_uses_router(&existing, &codex_public_base_url(cfg))
    {
        return Ok(None);
    }
    Ok(Some(codex_config_fingerprint(&existing)))
}

/// Stable fingerprint of the current Codex config content. A missing file is
/// hashed as empty content so its "deleted" state is equally trackable.
pub fn codex_config_fingerprint(content: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(content.as_bytes());
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

/// Marker comment identifying a system-layer Codex config owned by
/// Codex-Router. Kept line-oriented so rewrites stay idempotent.
const SYSTEM_BINDING_MARKER: &str = "# codex-router-managed: binding layer";

/// Codex merges `%ProgramData%\OpenAI\Codex\config.toml` as a system default
/// layer beneath the user `config.toml`. Codex Desktop periodically rewrites
/// the user file and drops the Router keys (`model_provider`,
/// `model_catalog_json`, the provider block, `features.fast_mode`), which
/// sends non-ChatGPT models straight to the ChatGPT backend where they are
/// rejected. Mirroring the binding into the system layer keeps every
/// registered model routed through the local gateway even while the user file
/// is stripped.
///
/// NOTE: the system layer is machine-wide. Codex-Router targets single-user
/// machines; on shared machines every local Windows user inherits this
/// binding.
pub fn codex_system_config_path() -> PathBuf {
    let program_data = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    program_data.join("OpenAI").join("Codex").join("config.toml")
}

fn strip_system_binding_marker(content: &str) -> String {
    content
        .lines()
        .filter(|line| line.trim() != SYSTEM_BINDING_MARKER)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Write or refresh the system-layer Router binding. Existing unrelated
/// content is preserved; a foreign file that pins a different
/// `model_provider` without our marker is left untouched and reported so an
/// enterprise-managed file is never hijacked.
pub(crate) fn write_codex_system_binding_to(
    path: &Path,
    catalog_path: &Path,
    local_api_key: &str,
    base_url: &str,
    require_openai_auth: bool,
    display_openai_provider: bool,
) -> anyhow::Result<()> {
    if local_api_key.is_empty() {
        bail!("the local Router credential is required for the Codex system binding");
    }
    let base = validate_local_base_url(base_url)?;
    let catalog_path = catalog_path.to_string_lossy().replace('\\', "/");
    let existing = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).context("failed to read the system Codex config"),
    };
    let had_marker = existing
        .lines()
        .any(|line| line.trim() == SYSTEM_BINDING_MARKER);
    let body = strip_system_binding_marker(&existing);
    let mut doc: DocumentMut = if body.trim().is_empty() {
        DocumentMut::new()
    } else {
        body.parse()
            .context("system Codex config.toml is not valid TOML")?
    };
    if !had_marker {
        if let Some(provider) = doc.get("model_provider").and_then(Item::as_str) {
            if provider != "codex_router" {
                bail!(
                    "system Codex config.toml already sets model_provider = \"{provider}\"; leaving the foreign file untouched"
                );
            }
        }
        // One-time safety net before merging into foreign content.
        if !existing.trim().is_empty() {
            let backup = path.with_file_name(format!(
                "{}.codex-router.bak",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("config.toml")
            ));
            if !backup.exists() {
                std::fs::write(&backup, &existing)
                    .context("failed to back up the system Codex config")?;
            }
        }
    }

    doc.insert("model_provider", toml_edit::value("codex_router"));
    doc.insert("model_catalog_json", toml_edit::value(&catalog_path));
    let mut features = doc
        .remove("features")
        .and_then(|item| item.into_table().ok())
        .unwrap_or_default();
    features.insert("fast_mode", toml_edit::value(true));
    doc["features"] = toml_edit::Item::Table(features);
    let mut desktop = doc
        .remove("desktop")
        .and_then(|item| item.into_table().ok())
        .unwrap_or_default();
    if !desktop.contains_key("enabled-reasoning-efforts") {
        let mut levels = toml_edit::Array::new();
        for level in ["low", "medium", "high", "xhigh", "ultra", "max"] {
            levels.push(level);
        }
        desktop.insert("enabled-reasoning-efforts", toml_edit::value(levels));
    }
    doc["desktop"] = toml_edit::Item::Table(desktop);
    let provider = build_codex_router_provider(
        &base,
        local_api_key,
        require_openai_auth,
        display_openai_provider,
    );
    let model_providers =
        doc["model_providers"].or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    model_providers["codex_router"] = toml_edit::Item::Table(provider);

    let mut text = doc.to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let text = format!("{SYSTEM_BINDING_MARKER}\n{text}");
    if existing == text {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::config::atomic_write(path, text.as_bytes())
        .context("failed to write the system Codex config")?;
    Ok(())
}

pub fn write_codex_system_binding(
    catalog_path: &Path,
    local_api_key: &str,
    base_url: &str,
    require_openai_auth: bool,
    display_openai_provider: bool,
) -> anyhow::Result<()> {
    write_codex_system_binding_to(
        &codex_system_config_path(),
        catalog_path,
        local_api_key,
        base_url,
        require_openai_auth,
        display_openai_provider,
    )
}

/// Remove the Router-owned keys from the system-layer Codex config, deleting
/// the file when nothing else remains. A foreign file without our marker and
/// without a Router provider binding is left untouched.
pub(crate) fn remove_codex_system_binding_from(path: &Path) -> anyhow::Result<bool> {
    let existing = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("failed to read the system Codex config"),
    };
    let had_marker = existing
        .lines()
        .any(|line| line.trim() == SYSTEM_BINDING_MARKER);
    let body = strip_system_binding_marker(&existing);
    let mut doc: DocumentMut = body
        .parse()
        .context("system Codex config.toml is not valid TOML")?;
    let router_provider =
        doc.get("model_provider").and_then(Item::as_str) == Some("codex_router");
    let router_provider_block = doc
        .get("model_providers")
        .and_then(Item::as_table_like)
        .is_some_and(|providers| providers.get("codex_router").is_some());
    if !had_marker && !router_provider && !router_provider_block {
        return Ok(false);
    }
    if router_provider {
        doc.remove("model_provider");
    }
    let prune_providers = if let Some(providers) = doc["model_providers"].as_table_like_mut() {
        providers.remove("codex_router");
        providers.iter().next().is_none()
    } else {
        false
    };
    if prune_providers {
        doc.remove("model_providers");
    }
    if had_marker {
        doc.remove("model_catalog_json");
        let prune_features = if let Some(features) = doc["features"].as_table_like_mut() {
            features.remove("fast_mode");
            features.iter().next().is_none()
        } else {
            false
        };
        if prune_features {
            doc.remove("features");
        }
        let prune_desktop = if let Some(desktop) = doc["desktop"].as_table_like_mut() {
            desktop.remove("enabled-reasoning-efforts");
            desktop.iter().next().is_none()
        } else {
            false
        };
        if prune_desktop {
            doc.remove("desktop");
        }
    }
    let text = doc.to_string();
    if text.trim().is_empty() {
        std::fs::remove_file(path).context("failed to remove the system Codex config")?;
        // Best-effort cleanup of directories Codex-Router created.
        if let Some(dir) = path.parent() {
            let _ = std::fs::remove_dir(dir);
            if let Some(parent) = dir.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    } else {
        crate::config::atomic_write(path, text.as_bytes())
            .context("failed to update the system Codex config")?;
    }
    Ok(true)
}

pub fn remove_codex_system_binding() -> anyhow::Result<bool> {
    remove_codex_system_binding_from(&codex_system_config_path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ModelConfig, OAuthFallback};

    #[test]
    fn oauth_profile_keeps_v1_5_2_login_and_custom_catalog_contract() {
        let cfg = RouterConfig {
            auth_mode: "chatgpt_oauth".to_owned(),
            default_model: "vendor/custom-alpha".to_owned(),
            models: vec![
                ModelConfig {
                    model: "vendor/custom-alpha".to_owned(),
                    alias: "My Custom Alpha".to_owned(),
                    alias_customized: Some(true),
                    ..Default::default()
                },
                ModelConfig {
                    model: "vendor/custom-beta".to_owned(),
                    alias: "My Custom Beta".to_owned(),
                    alias_customized: Some(true),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let settings = codex_router_settings(&cfg);
        let generated = generate_codex_router_config(
            "",
            &settings.model,
            "C:/isolated/model-catalog.json",
            "fixture-local-key",
            "http://127.0.0.1:18082",
            &settings.reasoning_effort,
            settings.fast_mode,
            settings.require_openai_auth,
            settings.display_openai_provider,
            None,
        )
        .unwrap();
        let catalog = crate::logic::catalog::build_model_catalog_with_root(
            &cfg,
            Path::new("missing-catalog-template-root"),
        );
        let visible = catalog
            .iter()
            .map(|entry| {
                (
                    entry["slug"].as_str().unwrap(),
                    entry["display_name"].as_str().unwrap(),
                )
            })
            .collect::<Vec<_>>();

        // Preserve Codex's upstream ChatGPT login contract for OAuth profiles.
        assert!(generated.contains("name = \"Codex-Router\""));
        assert!(generated.contains("requires_openai_auth = true"));
        assert!(generated.contains("request_max_retries = 1"));
        assert!(generated.contains("stream_max_retries = 0"));
        assert!(!generated.contains("forced_login_method"));
        assert!(generated.contains("model_catalog_json = \"C:/isolated/model-catalog.json\""));
        assert_eq!(
            visible,
            vec![
                ("vendor/custom-alpha", "My Custom Alpha"),
                ("vendor/custom-beta", "My Custom Beta"),
            ]
        );
    }

    #[test]
    fn mixed_oauth_api_routes_keep_the_v1_5_2_account_contract() {
        let cfg = RouterConfig {
            default_model: "gpt-5.6-sol".to_owned(),
            oauth_account_ids: Some(vec![7]),
            oauth_fallback: OAuthFallback {
                enabled: true,
                prefer_oauth: true,
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
                    source: "apikey".to_owned(),
                    model: "gpt-5.6-sol".to_owned(),
                    ..Default::default()
                },
                ModelConfig {
                    source: "apikey".to_owned(),
                    model: "deepseek-v4-flash".to_owned(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let settings = codex_router_settings(&cfg);
        assert_eq!(settings.model, "gpt-5.6-sol");
        assert!(settings.require_openai_auth);
        assert!(!settings.display_openai_provider);
    }

    #[test]
    fn api_only_router_uses_the_router_provider_label() {
        let cfg = RouterConfig {
            auth_mode: "local_api_key".to_owned(),
            models: vec![ModelConfig {
                source: "apikey".to_owned(),
                model: "deepseek-v4-flash".to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let settings = codex_router_settings(&cfg);
        assert!(!settings.display_openai_provider);
        assert!(!settings.require_openai_auth);
    }

    #[test]
    fn generate_keeps_chatgpt_forced_login_when_openai_auth_is_required() {
        let text = generate_codex_router_config(
            "forced_login_method = \"chatgpt\"\nmodel = \"gpt-5.6-sol\"\n",
            "gpt-5.6-sol",
            "C:/catalog.json",
            "sk-local-key",
            "http://127.0.0.1:18082",
            "medium",
            false,
            true,
            false,
            None,
        )
        .unwrap();
        assert!(text.contains("forced_login_method = \"chatgpt\""));
        assert!(text.contains("requires_openai_auth = true"));
    }

    #[test]
    fn oauth_upstream_label_does_not_force_the_codex_login_method() {
        let text = generate_codex_router_config(
            "",
            "gpt-5.6-sol",
            "C:/catalog.json",
            "sk-local-key",
            "http://127.0.0.1:18082",
            "medium",
            false,
            false,
            true,
            None,
        )
        .unwrap();

        assert!(text.contains("name = \"OpenAI\""));
        assert!(text.contains("requires_openai_auth = false"));
        assert!(!text.contains("forced_login_method"));
    }

    #[test]
    fn empty_config_generates_router_provider() {
        let text = generate_codex_router_config(
            "",
            "gpt-5.6-sol",
            "C:/users/test/.codex-router/model-catalog.json",
            "sk-local-abc123",
            "http://127.0.0.1:18082",
            "medium",
            false,
            false,
            false,
            None,
        )
        .unwrap();
        assert!(text.contains("model_provider = \"codex_router\""));
        assert!(text.contains("model = \"gpt-5.6-sol\""));
        assert!(text
            .contains("model_catalog_json = \"C:/users/test/.codex-router/model-catalog.json\""));
        assert!(text.contains("[features]") && text.contains("apps = false"));
        assert!(text.contains("[model_providers.codex_router]"));
        assert!(text.contains("base_url = \"http://127.0.0.1:18082/v1\""));
        assert!(text.contains("experimental_bearer_token = \"sk-local-abc123\""));
        assert!(text.contains("requires_openai_auth = false"));
        assert!(text.contains("name = \"Codex-Router\""));
        assert!(text.contains("[desktop]"));
        assert!(text.contains("enabled-reasoning-efforts = [\"low\", \"medium\", \"high\", \"xhigh\", \"ultra\", \"max\"]"));
        assert!(!text.contains("service_tier"));
        assert!(text.contains("fast_mode = false"));
    }

    #[test]
    fn desktop_layout_keys_survive_router_rewrite() {
        let existing = r#"
[desktop]
enabled-reasoning-efforts = ["low"]
window-layout = "sidebar-wide"
theme = "dark"
"#;
        let text = generate_codex_router_config(
            existing,
            "gpt-5.6-sol",
            "C:/catalog.json",
            "sk-local-key",
            "http://127.0.0.1:18082",
            "medium",
            false,
            true,
            false,
            None,
        )
        .unwrap();
        assert!(text.contains("window-layout = \"sidebar-wide\""));
        assert!(text.contains("theme = \"dark\""));
        assert!(text.contains("enabled-reasoning-efforts"));
    }

    #[test]
    fn existing_agents_block_is_preserved() {
        let existing = r#"
[agents]
default_subagent_model = "gpt-5.6-luna"
default_subagent_reasoning_effort = "max"
custom_agent_setting = "preserve-me"
"#;
        let text = generate_codex_router_config(
            existing,
            "gpt-5.6-sol",
            "C:/catalog.json",
            "sk-local-key",
            "http://127.0.0.1:18082",
            "medium",
            false,
            true,
            false,
            None,
        )
        .unwrap();
        assert!(text.contains("[agents]"));
        assert!(text.contains("default_subagent_model = \"gpt-5.6-sol\""));
        assert!(text.contains("default_subagent_reasoning_effort = \"medium\""));
        assert!(text.contains("custom_agent_setting = \"preserve-me\""));
        assert!(!text.contains("gpt-5.6-luna"));
    }

    #[test]
    fn deepseek_removes_openai_subagent_defaults_but_preserves_custom_agent_settings() {
        let existing = r#"
[agents]
default_subagent_model = "gpt-5.6-sol"
default_subagent_reasoning_effort = "max"
custom_agent_setting = "preserve-me"
"#;
        let text = generate_codex_router_config(
            existing,
            "deepseek-v4-flash",
            "C:/catalog.json",
            "sk-local-key",
            "http://127.0.0.1:18082",
            "high",
            false,
            true,
            false,
            None,
        )
        .unwrap();
        assert!(text.contains("custom_agent_setting = \"preserve-me\""));
        assert!(!text.contains("default_subagent_model"));
        assert!(!text.contains("default_subagent_reasoning_effort"));
    }

    #[test]
    fn fast_mode_sets_fast_tier() {
        let text = generate_codex_router_config(
            "",
            "gpt-5.6-sol",
            "C:/catalog.json",
            "sk-local-key",
            "http://127.0.0.1:18082",
            "medium",
            true,
            true,
            true,
            None,
        )
        .unwrap();
        assert!(text.contains("service_tier = \"fast\""));
        assert!(text.contains("fast_mode = true"));
        assert!(text.contains("requires_openai_auth = true"));
        assert!(!text.contains("forced_login_method"));
        assert!(text.contains("name = \"OpenAI\""));
        assert!(!text.contains("name = \"Codex-Router\""));
    }

    #[test]
    fn legacy_loopback_proxy_is_removed_without_deleting_unrelated_custom_providers() {
        let legacy = r#"
[model_providers.custom]
name = "Legacy Loopback Proxy"
base_url = "http://127.0.0.1:15721/v1"
experimental_bearer_token = "PROXY_MANAGED"
"#;
        let migrated = generate_codex_router_config(
            legacy,
            "gpt-5.6-sol",
            "C:/catalog.json",
            "sk-local-key",
            "http://127.0.0.1:18082",
            "medium",
            false,
            true,
            false,
            None,
        )
        .unwrap();
        assert!(!migrated.contains("127.0.0.1:15721"));
        assert!(!migrated.contains("PROXY_MANAGED"));

        let unrelated = r#"
[model_providers.custom]
name = "micu"
base_url = "https://api.example.test/v1"
experimental_bearer_token = "keep-this-provider"
"#;
        let preserved = generate_codex_router_config(
            unrelated,
            "gpt-5.6-sol",
            "C:/catalog.json",
            "sk-local-key",
            "http://127.0.0.1:18082",
            "medium",
            false,
            true,
            false,
            None,
        )
        .unwrap();
        assert!(preserved.contains("name = \"micu\""));
        assert!(preserved.contains("keep-this-provider"));
    }

    #[test]
    fn permission_settings_are_copied() {
        let source = r#"
approval_policy = "always"
sandbox_mode = "elevated"

[windows]
sandbox = "unelevated"
"#;
        let text = generate_codex_router_config(
            "",
            "gpt-5.6-sol",
            "C:/catalog.json",
            "sk-local-key",
            "http://127.0.0.1:18082",
            "medium",
            false,
            false,
            false,
            Some(source),
        )
        .unwrap();
        assert!(text.contains("approval_policy = \"always\""));
        assert!(text.contains("sandbox_mode = \"elevated\""));
        assert!(text.contains("sandbox = \"unelevated\""));
    }

    #[test]
    fn windows_sandbox_elevated_is_preserved() {
        let existing = r#"
[windows]
sandbox = "elevated"
"#;
        let source = r#"
[windows]
sandbox = "unelevated"
"#;
        let text = generate_codex_router_config(
            existing,
            "gpt-5.6-sol",
            "C:/catalog.json",
            "sk-local-key",
            "http://127.0.0.1:18082",
            "medium",
            false,
            false,
            false,
            Some(source),
        )
        .unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let idx = lines.iter().position(|l| l.trim() == "[windows]").unwrap();
        let next = lines[idx + 1];
        assert!(
            next.contains("elevated"),
            "elevated marker should be preserved, got: {}",
            next
        );
    }

    #[test]
    fn write_creates_backup_and_limits() {
        let tmp = std::env::temp_dir().join(format!("codex-toml-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let catalog = tmp.join("model-catalog.json");
        std::fs::write(&catalog, "{}").unwrap();

        std::fs::write(tmp.join("config.toml"), "model = \"old\"").unwrap();

        write_codex_router_config(
            &tmp,
            "gpt-5.6-sol",
            &catalog,
            "sk-local-key",
            "http://127.0.0.1:18082",
            "medium",
            false,
            false,
            false,
            None,
        )
        .unwrap();

        assert!(tmp.join("config.toml").exists());
        let text = std::fs::read_to_string(tmp.join("config.toml")).unwrap();
        assert!(text.contains("model_provider = \"codex_router\""));

        let backups: Vec<_> = std::fs::read_dir(&tmp)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .unwrap_or("")
                    .starts_with("config.toml.codex-router-")
            })
            .collect();
        assert_eq!(backups.len(), 1);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn permission_settings_from_existing_config_are_preserved_on_first_apply() {
        let tmp = std::env::temp_dir().join(format!("codex-toml-perm-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let catalog = tmp.join("model-catalog.json");
        std::fs::write(&catalog, "{}").unwrap();
        std::fs::write(
            tmp.join("config.toml"),
            r#"approval_policy = "always"
sandbox_mode = "elevated"

[windows]
sandbox = "unelevated"
"#,
        )
        .unwrap();
        write_codex_router_config(
            &tmp,
            "gpt-5.6-sol",
            &catalog,
            "sk-local-key",
            "http://127.0.0.1:18082",
            "medium",
            false,
            false,
            false,
            None,
        )
        .unwrap();
        let text = std::fs::read_to_string(tmp.join("config.toml")).unwrap();
        assert!(text.contains(r#"approval_policy = "always""#));
        assert!(text.contains(r#"sandbox_mode = "elevated""#));
        assert!(text.contains(r#"sandbox = "unelevated""#));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn repair_overwritten_binding_preserves_chatgpt_auth_and_user_settings() {
        let tmp = std::env::temp_dir().join(format!(
            "codex-router-binding-repair-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let codex_home = tmp.join("codex-home");
        let system_config = tmp.join("system-config.toml");
        std::fs::create_dir_all(&codex_home).unwrap();
        let auth = br#"{\"auth_mode\":\"chatgpt\",\"tokens\":{\"access_token\":\"fixture-only\"}}"#;
        std::fs::write(codex_home.join("auth.json"), auth).unwrap();
        std::fs::write(
            codex_home.join("config.toml"),
            "model = \"deepseek-v4-flash\"\napproval_policy = \"never\"\n\n\
             [mcp_servers.user-tool]\ncommand = \"user-tool.exe\"\n",
        )
        .unwrap();

        let cfg = RouterConfig {
            default_model: "gpt-5.6-sol".to_owned(),
            models: vec![
                ModelConfig {
                    source: "apikey".to_owned(),
                    model: "gpt-5.6-sol".to_owned(),
                    ..Default::default()
                },
                ModelConfig {
                    source: "apikey".to_owned(),
                    model: "deepseek-v4-flash".to_owned(),
                    ..Default::default()
                },
            ],
            deploy: crate::config::DeployConfig {
                codex_home: codex_home.to_string_lossy().into_owned(),
                sub2api_host: "http://127.0.0.1:18082".to_owned(),
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(
            repair_codex_router_binding_impl(&cfg, &tmp, "fixture-local-router-key", &system_config).unwrap()
        );
        let repaired = std::fs::read_to_string(codex_home.join("config.toml")).unwrap();
        assert!(repaired.contains("model_provider = \"codex_router\""));
        assert!(repaired.contains("model = \"deepseek-v4-flash\""));
        assert!(repaired.contains("model_catalog_json ="));
        assert!(repaired.contains("requires_openai_auth = true"));
        assert!(repaired.contains("experimental_bearer_token = \"fixture-local-router-key\""));
        assert!(repaired.contains("approval_policy = \"never\""));
        assert!(repaired.contains("[mcp_servers.user-tool]"));
        assert_eq!(std::fs::read(codex_home.join("auth.json")).unwrap(), auth);

        let mut stale_bearer: DocumentMut = repaired.parse().unwrap();
        stale_bearer["model_reasoning_effort"] = toml_edit::value("high");
        stale_bearer["model_providers"]["codex_router"]["experimental_bearer_token"] =
            toml_edit::value("stale-nonempty-router-key");
        std::fs::write(codex_home.join("config.toml"), stale_bearer.to_string()).unwrap();
        assert!(
            repair_codex_router_binding_impl(&cfg, &tmp, "fixture-local-router-key", &system_config).unwrap()
        );
        let repaired = std::fs::read_to_string(codex_home.join("config.toml")).unwrap();
        assert!(repaired.contains("experimental_bearer_token = \"fixture-local-router-key\""));
        assert!(repaired.contains("model_reasoning_effort = \"high\""));

        let mut stale_catalog: DocumentMut = repaired.parse().unwrap();
        stale_catalog["model_catalog_json"] = toml_edit::value("C:/stale/model-catalog.json");
        std::fs::write(codex_home.join("config.toml"), stale_catalog.to_string()).unwrap();
        assert!(
            repair_codex_router_binding_impl(&cfg, &tmp, "fixture-local-router-key", &system_config).unwrap()
        );
        let repaired = std::fs::read_to_string(codex_home.join("config.toml")).unwrap();
        assert!(!repaired.contains("C:/stale/model-catalog.json"));

        assert!(
            !repair_codex_router_binding_impl(&cfg, &tmp, "fixture-local-router-key", &system_config).unwrap()
        );
        assert_eq!(std::fs::read(codex_home.join("auth.json")).unwrap(), auth);

        // Every repair also refreshes the system-layer safety net, even when
        // the user-layer binding was already intact.
        let system = std::fs::read_to_string(&system_config).unwrap();
        assert!(system.contains(SYSTEM_BINDING_MARKER));
        assert!(system.contains("model_provider = \"codex_router\""));
        assert!(system.contains("[model_providers.codex_router]"));
        assert!(system.contains("experimental_bearer_token = \"fixture-local-router-key\""));
        assert!(system.contains("requires_openai_auth = true"));
        assert!(system.contains("fast_mode = true"));
        assert_eq!(
            system.matches(SYSTEM_BINDING_MARKER).count(),
            1,
            "rewrites must stay idempotent"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn overwrite_probe_reports_fingerprint_only_when_binding_is_broken() {
        let tmp = std::env::temp_dir().join(format!(
            "codex-router-binding-probe-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let codex_home = tmp.join("codex-home");
        let system_config = tmp.join("system-config.toml");
        std::fs::create_dir_all(&codex_home).unwrap();
        let cfg = RouterConfig {
            default_model: "gpt-5.6-sol".to_owned(),
            models: vec![ModelConfig {
                source: "apikey".to_owned(),
                model: "gpt-5.6-sol".to_owned(),
                ..Default::default()
            }],
            deploy: crate::config::DeployConfig {
                codex_home: codex_home.to_string_lossy().into_owned(),
                sub2api_host: "http://127.0.0.1:18082".to_owned(),
                ..Default::default()
            },
            ..Default::default()
        };

        // A missing config.toml counts as overwritten (deleted) and reports
        // the empty-content fingerprint.
        let missing =
            probe_codex_router_binding_overwrite_with_key(&cfg, &tmp, "fixture-local-router-key")
                .unwrap();
        assert_eq!(
            missing.as_deref(),
            Some(codex_config_fingerprint("").as_str())
        );

        // Repair writes the standard binding; the probe then reports intact.
        assert!(
            repair_codex_router_binding_impl(&cfg, &tmp, "fixture-local-router-key", &system_config).unwrap()
        );
        assert!(
            probe_codex_router_binding_overwrite_with_key(&cfg, &tmp, "fixture-local-router-key")
                .unwrap()
                .is_none()
        );

        // An external program rewriting the file is detected, and the
        // fingerprint tracks that exact overwritten content.
        let overwritten = "model = \"gpt-5.6-sol\"\nmodel_provider = \"micu\"\n";
        std::fs::write(codex_home.join("config.toml"), overwritten).unwrap();
        let detected =
            probe_codex_router_binding_overwrite_with_key(&cfg, &tmp, "fixture-local-router-key")
                .unwrap();
        assert_eq!(
            detected.as_deref(),
            Some(codex_config_fingerprint(overwritten).as_str())
        );

        // A second, different overwrite yields a different fingerprint.
        let overwritten_again = "model_provider = \"micu\"\nmodel = \"deepseek-v4-flash\"\n";
        std::fs::write(codex_home.join("config.toml"), overwritten_again).unwrap();
        let detected_again =
            probe_codex_router_binding_overwrite_with_key(&cfg, &tmp, "fixture-local-router-key")
                .unwrap();
        assert_eq!(
            detected_again.as_deref(),
            Some(codex_config_fingerprint(overwritten_again).as_str())
        );
        assert_ne!(detected, detected_again);

        assert!(
            repair_codex_router_binding_impl(&cfg, &tmp, "fixture-local-router-key", &system_config)
                .unwrap()
        );
        let mut desktop_owned: DocumentMut = std::fs::read_to_string(codex_home.join("config.toml"))
            .unwrap()
            .parse()
            .unwrap();
        desktop_owned["forced_login_method"] = toml_edit::value("chatgpt");
        let desktop_text = desktop_owned.to_string();
        std::fs::write(codex_home.join("config.toml"), &desktop_text).unwrap();
        assert!(
            probe_codex_router_binding_overwrite_with_key(&cfg, &tmp, "fixture-local-router-key")
                .unwrap()
                .is_none(),
            "Desktop writing forced_login_method back is not an external overwrite"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn probe_fixture(tmp: &Path) -> (PathBuf, PathBuf, RouterConfig) {
        let router_root = tmp.join("router-root");
        let codex_home = tmp.join("codex-home");
        std::fs::create_dir_all(&router_root).unwrap();
        std::fs::create_dir_all(&codex_home).unwrap();
        let cfg = RouterConfig {
            default_model: "gpt-5.6-sol".to_owned(),
            models: vec![
                ModelConfig {
                    source: "apikey".to_owned(),
                    model: "gpt-5.6-sol".to_owned(),
                    ..Default::default()
                },
                ModelConfig {
                    source: "apikey".to_owned(),
                    model: "deepseek-v4-flash".to_owned(),
                    ..Default::default()
                },
            ],
            deploy: crate::config::DeployConfig {
                codex_home: codex_home.to_string_lossy().into_owned(),
                sub2api_host: "http://127.0.0.1:18082".to_owned(),
                ..Default::default()
            },
            ..Default::default()
        };
        (router_root, codex_home, cfg)
    }

    #[test]
    fn binding_probe_repairs_invalid_user_model_when_system_layer_still_routes() {
        let tmp = std::env::temp_dir().join(format!(
            "codex-router-probe-repair-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let (_root, codex_home, cfg) = probe_fixture(&tmp);
        let system_config = tmp.join("system").join("config.toml");
        let catalog = tmp.join("model-catalog.json");
        std::fs::write(&catalog, "{}").unwrap();
        // The public gateway base URL is the Sub2API port + 2 (18084 here).
        write_codex_system_binding_to(
            &system_config,
            &catalog,
            "fixture-local-router-key",
            "http://127.0.0.1:18084",
            true,
            false,
        )
        .unwrap();
        std::fs::write(
            codex_home.join("config.toml"),
            "model = \"first\"\nsandbox_mode = \"danger-full-access\"\n\n[desktop]\nsansFontSize = 17\n",
        )
        .unwrap();

        let probe = probe_codex_binding_state_with_key(
            &cfg,
            &tmp,
            "fixture-local-router-key",
            &system_config,
        )
        .unwrap();
        assert!(!probe.user_layer_bound);
        assert!(probe.system_layer_bound);
        assert_eq!(probe.user_model.as_deref(), Some("first"));
        assert_eq!(
            probe.model_repair,
            Some((Some("first".to_owned()), "gpt-5.6-sol".to_owned()))
        );
        let repaired = std::fs::read_to_string(codex_home.join("config.toml")).unwrap();
        assert!(repaired.contains("model = \"gpt-5.6-sol\""));
        assert!(repaired.contains("sansFontSize = 17"));

        // A second pass sees a valid model and leaves the file untouched.
        let again = probe_codex_binding_state_with_key(
            &cfg,
            &tmp,
            "fixture-local-router-key",
            &system_config,
        )
        .unwrap();
        assert_eq!(again.model_repair, None);
        assert_eq!(again.user_model.as_deref(), Some("gpt-5.6-sol"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn binding_probe_reports_full_loss_only_when_both_layers_unbound() {
        let tmp = std::env::temp_dir().join(format!(
            "codex-router-probe-loss-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let (_root, codex_home, cfg) = probe_fixture(&tmp);
        let overwritten = "model_provider = \"micu\"\nmodel = \"deepseek-v4-flash\"\n";
        std::fs::write(codex_home.join("config.toml"), overwritten).unwrap();
        let missing_system = tmp.join("missing-system").join("config.toml");
        let probe = probe_codex_binding_state_with_key(
            &cfg,
            &tmp,
            "fixture-local-router-key",
            &missing_system,
        )
        .unwrap();
        assert!(!probe.user_layer_bound);
        assert!(!probe.system_layer_bound);
        assert_eq!(probe.fingerprint, codex_config_fingerprint(overwritten));
        assert_eq!(probe.model_repair, None);
        // No silent edit happens while routing is genuinely broken.
        assert_eq!(
            std::fs::read_to_string(codex_home.join("config.toml")).unwrap(),
            overwritten
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn binding_probe_keeps_a_valid_nondefault_user_model() {
        let tmp = std::env::temp_dir().join(format!(
            "codex-router-probe-valid-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let (_root, codex_home, cfg) = probe_fixture(&tmp);
        let system_config = tmp.join("system").join("config.toml");
        assert!(
            repair_codex_router_binding_impl(&cfg, &tmp, "fixture-local-router-key", &system_config)
                .unwrap()
        );
        // The user picked a different catalog model in Codex; that choice is
        // valid and must survive the probe untouched.
        let chosen = std::fs::read_to_string(codex_home.join("config.toml"))
            .unwrap()
            .replace("model = \"gpt-5.6-sol\"", "model = \"deepseek-v4-flash\"");
        std::fs::write(codex_home.join("config.toml"), &chosen).unwrap();
        let probe = probe_codex_binding_state_with_key(
            &cfg,
            &tmp,
            "fixture-local-router-key",
            &system_config,
        )
        .unwrap();
        assert!(probe.user_layer_bound);
        assert_eq!(probe.model_repair, None);
        assert_eq!(
            std::fs::read_to_string(codex_home.join("config.toml")).unwrap(),
            chosen
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn apply_reuses_existing_apikey_login_instead_of_forcing_openai_auth() {
        let tmp = std::env::temp_dir().join(format!(
            "codex-router-apikey-login-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let router_root = tmp.join("router-root");
        let codex_home = tmp.join("codex-home");
        std::fs::create_dir_all(&router_root).unwrap();
        std::fs::create_dir_all(&codex_home).unwrap();
        let auth = br#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-user-fixture"}"#;
        std::fs::write(codex_home.join("auth.json"), auth).unwrap();
        std::fs::write(
            codex_home.join("config.toml"),
            "model = \"deepseek-v4-flash\"\nmodel_provider = \"codex_router\"\n",
        )
        .unwrap();
        let cfg = RouterConfig {
            auth_mode: "chatgpt_oauth".to_owned(),
            default_model: "deepseek-v4-flash".to_owned(),
            models: vec![ModelConfig {
                source: "apikey".to_owned(),
                model: "deepseek-v4-flash".to_owned(),
                ..Default::default()
            }],
            deploy: crate::config::DeployConfig {
                codex_home: codex_home.to_string_lossy().into_owned(),
                sub2api_host: "http://127.0.0.1:18082".to_owned(),
                ..Default::default()
            },
            ..Default::default()
        };
        write_codex_config_from_router_config_impl(&cfg, &router_root, &tmp.join("system.toml"))
            .unwrap();
        let written = std::fs::read_to_string(codex_home.join("config.toml")).unwrap();
        assert!(written.contains("requires_openai_auth = false"));
        assert!(!written.contains("forced_login_method"));
        assert_eq!(std::fs::read(codex_home.join("auth.json")).unwrap(), auth);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn write_codex_config_preserves_desktop_selected_model_and_reasoning() {
        let tmp = std::env::temp_dir().join(format!(
            "codex-router-write-preserve-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let router_root = tmp.join("router-root");
        let codex_home = tmp.join("codex-home");
        std::fs::create_dir_all(&router_root).unwrap();
        std::fs::create_dir_all(&codex_home).unwrap();
        std::fs::write(
            codex_home.join("config.toml"),
            "model = \"deepseek-v4-flash\"\nmodel_provider = \"codex_router\"\nmodel_reasoning_effort = \"high\"\n",
        )
        .unwrap();

        let cfg = RouterConfig {
            default_model: "gpt-5.6-sol".to_owned(),
            models: vec![
                ModelConfig {
                    source: "apikey".to_owned(),
                    model: "gpt-5.6-sol".to_owned(),
                    ..Default::default()
                },
                ModelConfig {
                    source: "apikey".to_owned(),
                    model: "deepseek-v4-flash".to_owned(),
                    ..Default::default()
                },
            ],
            deploy: crate::config::DeployConfig {
                codex_home: codex_home.to_string_lossy().into_owned(),
                sub2api_host: "http://127.0.0.1:18082".to_owned(),
                ..Default::default()
            },
            ..Default::default()
        };

        write_codex_config_from_router_config_impl(&cfg, &router_root, &tmp.join("system.toml"))
            .unwrap();
        let written = std::fs::read_to_string(codex_home.join("config.toml")).unwrap();
        assert!(written.contains("model = \"deepseek-v4-flash\""));
        assert!(written.contains("model_reasoning_effort = \"high\""));
        assert!(!written.contains("model = \"gpt-5.6-sol\""));
        let system = std::fs::read_to_string(tmp.join("system.toml")).unwrap();
        assert!(system.contains("model_provider = \"codex_router\""));
        assert!(system.contains(SYSTEM_BINDING_MARKER));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn system_binding_write_preserves_foreign_keys_and_removal_restores_them() {
        let tmp = std::env::temp_dir().join(format!(
            "codex-router-system-binding-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("config.toml");
        let catalog = tmp.join("model-catalog.json");
        std::fs::write(&catalog, "{}").unwrap();
        std::fs::write(
            &path,
            "# enterprise defaults\napproval_policy = \"never\"\n\n[features]\nmy_feature = true\n",
        )
        .unwrap();

        write_codex_system_binding_to(
            &path,
            &catalog,
            "fixture-local-key",
            "http://127.0.0.1:18082",
            true,
            false,
        )
        .unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains(SYSTEM_BINDING_MARKER));
        assert!(written.contains("approval_policy = \"never\""));
        assert!(written.contains("my_feature = true"));
        assert!(written.contains("model_provider = \"codex_router\""));
        assert!(written.contains("[model_providers.codex_router]"));
        assert!(written.contains("experimental_bearer_token = \"fixture-local-key\""));
        assert!(written.contains("fast_mode = true"));
        assert!(written.contains("enabled-reasoning-efforts"));
        assert!(written.contains("model_catalog_json = "));
        // The first merge into foreign content leaves a one-time backup.
        assert!(tmp.join("config.toml.codex-router.bak").exists());

        // Rewriting over our own marker keeps foreign keys and stays stable.
        write_codex_system_binding_to(
            &path,
            &catalog,
            "fixture-local-key-2",
            "http://127.0.0.1:18082",
            true,
            false,
        )
        .unwrap();
        let rewritten = std::fs::read_to_string(&path).unwrap();
        assert!(rewritten.contains("experimental_bearer_token = \"fixture-local-key-2\""));
        assert!(rewritten.contains("approval_policy = \"never\""));
        assert_eq!(rewritten.matches(SYSTEM_BINDING_MARKER).count(), 1);

        // Removal restores the foreign document without Router keys.
        assert!(remove_codex_system_binding_from(&path).unwrap());
        let restored = std::fs::read_to_string(&path).unwrap();
        assert!(!restored.contains(SYSTEM_BINDING_MARKER));
        assert!(!restored.contains("codex_router"));
        assert!(!restored.contains("model_catalog_json"));
        assert!(!restored.contains("fast_mode"));
        assert!(!restored.contains("enabled-reasoning-efforts"));
        assert!(restored.contains("approval_policy = \"never\""));
        assert!(restored.contains("my_feature = true"));

        // A foreign file without Router content is left untouched.
        assert!(!remove_codex_system_binding_from(&path).unwrap());
        assert!(!remove_codex_system_binding_from(&tmp.join("missing.toml")).unwrap());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn system_binding_write_refuses_foreign_model_provider() {
        let tmp = std::env::temp_dir().join(format!(
            "codex-router-system-foreign-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("config.toml");
        let original = "model_provider = \"enterprise\"\n";
        std::fs::write(&path, original).unwrap();

        let error = write_codex_system_binding_to(
            &path,
            &tmp.join("model-catalog.json"),
            "fixture-local-key",
            "http://127.0.0.1:18082",
            true,
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("enterprise"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn system_binding_removal_deletes_a_pure_router_file() {
        let tmp = std::env::temp_dir().join(format!(
            "codex-router-system-pure-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("config.toml");
        write_codex_system_binding_to(
            &path,
            &tmp.join("model-catalog.json"),
            "fixture-local-key",
            "http://127.0.0.1:18082",
            true,
            false,
        )
        .unwrap();
        assert!(path.exists());
        assert!(remove_codex_system_binding_from(&path).unwrap());
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
