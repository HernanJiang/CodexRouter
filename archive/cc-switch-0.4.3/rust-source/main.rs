#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod logic;
mod profiles;
mod runtime_logs;
mod theme;
mod ui;

use config::{CloseBehavior, ModelConfig, RouterConfig, UiPreferences};
use eframe::egui;
use profiles::{IsolationKind, IsolationProfile};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

const LOGO_PNG: &[u8] = include_bytes!("../assets/logo.png");
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const CURRENT_CONFIG_VERSION: &str = "0.4.3";
const CURRENT_TERMS_VERSION: &str = "codex-router-terms-v1.1-2026-08-01";
const OFFICIAL_GITHUB_URL: &str = "https://github.com/HernanJiang/Codex-Router";
const MAX_LOG_BYTES: usize = 256 * 1024;
const RETAIN_LOG_BYTES: usize = 192 * 1024;
const CREATE_NO_WINDOW: u32 = 0x08000000;
const HEALTHY_PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
const FAILED_PROBE_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
const RECOVERY_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
const HEALTH_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    Welcome,
    Project,
    Auth,
    Model,
    Proxy,
    Finish,
    Dashboard,
    Profiles,
    OAuth,
    Monitor,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuthModelSummary {
    id: String,
    display_name: String,
    #[serde(default)]
    suggested: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuthAccountSummary {
    id: i64,
    name: String,
    platform: String,
    status: String,
    email: String,
    plan: String,
    priority: i32,
    bound_to_router: bool,
    error: String,
    expires_at: String,
    #[serde(default)]
    models: Vec<OAuthModelSummary>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageModelSummary {
    #[serde(default)]
    name: String,
    #[serde(default)]
    requests: i64,
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
    #[serde(default)]
    cache_read_tokens: i64,
    #[serde(default)]
    cache_creation_tokens: i64,
    #[serde(default)]
    total_tokens: i64,
    #[serde(default)]
    cost: f64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageTotals {
    #[serde(default)]
    requests: i64,
    #[serde(default)]
    total_tokens: i64,
    #[serde(default)]
    cost: f64,
    #[serde(default)]
    models: Vec<UsageModelSummary>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageWindow {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    used_percent: Option<f32>,
    #[serde(default)]
    reset_at: String,
    #[serde(default = "negative_one")]
    remaining_seconds: i64,
    #[serde(default)]
    requests: i64,
    #[serde(default)]
    tokens: i64,
}

fn negative_one() -> i64 {
    -1
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageAccount {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    platform: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    health: String,
    #[serde(default)]
    status_detail: String,
    #[serde(default)]
    query_note: String,
    #[serde(default)]
    last_used_at: String,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    totals: UsageTotals,
    #[serde(default)]
    windows: Vec<UsageWindow>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageSnapshot {
    #[serde(default)]
    profile_name: String,
    #[serde(default)]
    queried_at: String,
    #[serde(default)]
    total_tokens: i64,
    #[serde(default)]
    total_requests: i64,
    #[serde(default)]
    total_cost: f64,
    #[serde(default)]
    subscriptions: Vec<UsageAccount>,
    #[serde(default)]
    api_channels: Vec<UsageAccount>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageMonitorCache {
    profile_key: String,
    snapshot: UsageSnapshot,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitHubUpdateInfo {
    #[serde(default)]
    status: String,
    #[serde(default)]
    current_version: String,
    #[serde(default)]
    latest_version: String,
    #[serde(default)]
    release_name: String,
    #[serde(default)]
    release_notes: String,
    #[serde(default)]
    release_url: String,
    #[serde(default)]
    asset_name: String,
    #[serde(default)]
    download_url: String,
    #[serde(default)]
    asset_size: u64,
    #[serde(default)]
    download_path: String,
    #[serde(default)]
    message: String,
}

enum AppEvent {
    Log(String),
    Complete,
    Error(String),
    Tray(TrayAction),
    OAuthAccountsLoaded(Vec<OAuthAccountSummary>),
    OAuthAccountsError(String),
    ProviderOAuthFinished,
    ProviderOAuthError(String),
    UsageLoaded {
        profile_key: String,
        snapshot: Box<UsageSnapshot>,
    },
    UsageError(String),
    RouterModeDisabled(profiles::RestoreOutcome),
    RouterModeSwitchError(String),
    OAuthRecoveryFinished,
    OAuthRecoveryError(String),
    GrokSsoImported,
    GrokSsoImportError(String),
    OAuthAccountRevoked {
        account_id: i64,
        account_name: String,
    },
    OAuthAccountRevokeError(String),
    UpdateResult(Box<GitHubUpdateInfo>),
    UpdateError(String),
    RouterHealthProbeFinished(Result<(), String>),
    RouterHealthRecoveryFinished(Result<(), String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrayAction {
    RestoreWindow,
    HideWindow,
    OpenConsole,
    ChooseProfile,
    ApplyCurrent,
    StartForwarding,
    StopForwarding,
    Exit,
}

struct CodexRouterApp {
    page: Page,
    router_root: PathBuf,
    project_path_input: String,
    config: RouterConfig,
    temp_model: ModelConfig,
    editing_model: Option<usize>,
    model_from_wizard: bool,
    proxy_from_wizard: bool,
    status_text: String,
    logs: String,
    event_rx: Receiver<AppEvent>,
    event_tx: Sender<AppEvent>,
    runtime_log_rx: Receiver<runtime_logs::RuntimeLogBatch>,
    applying: bool,
    configured: bool,
    logo_texture: Option<egui::TextureHandle>,
    fonts_loaded: bool,
    tray_icon: Option<tray_icon::TrayIcon>,
    last_page: Page,
    profiles_return_page: Page,
    page_changed_at: std::time::Instant,
    installed_theme: String,
    installed_compact_layout: bool,
    ui_language: String,
    terms_open: bool,
    terms_scroll_complete: bool,
    advanced_json_open: bool,
    advanced_json_draft: String,
    reasoning_open: bool,
    reasoning_mode_draft: String,
    reasoning_levels_draft: String,
    reasoning_default_draft: String,
    reasoning_fast_supported_draft: bool,
    reasoning_fast_mode_draft: bool,
    close_behavior: CloseBehavior,
    close_prompt_open: bool,
    remember_close_choice: bool,
    exit_after_prompt: bool,
    local_profile_name_input: String,
    cc_profile_name_input: String,
    isolation_profiles: Vec<IsolationProfile>,
    active_profile_id: String,
    pending_profile_activation: Option<String>,
    oauth_accounts: Vec<OAuthAccountSummary>,
    oauth_loading: bool,
    oauth_error: String,
    oauth_return_page: Page,
    oauth_provider_draft: String,
    provider_oauth_running: bool,
    pending_oauth_provider: Option<String>,
    oauth_revoke_target: Option<OAuthAccountSummary>,
    oauth_revoking: bool,
    oauth_manual_model_target: Option<OAuthAccountSummary>,
    oauth_manual_model_id_draft: String,
    oauth_manual_model_alias_draft: String,
    usage_snapshot: Option<UsageSnapshot>,
    usage_snapshot_profile_key: String,
    usage_loading: bool,
    usage_error: String,
    usage_return_page: Page,
    usage_refresh_due: Option<std::time::Instant>,
    monitor_subscription_order: Vec<i64>,
    monitor_api_order: Vec<i64>,
    share_codex_state: bool,
    router_mode_enabled: bool,
    router_mode_switching: bool,
    oauth_recovery_due: Option<std::time::Instant>,
    oauth_recovery_running: bool,
    grok_sso_dialog_open: bool,
    grok_sso_draft: String,
    grok_sso_importing: bool,
    grok_sso_error: String,
    grok_sso_auto_select_pending: bool,
    channel_preset_dialog_open: bool,
    sub2api_intro_open: bool,
    log_scroll_to_bottom: bool,
    log_follow_latest: bool,
    log_dialog_open: bool,
    runtime_log_stop: Arc<AtomicBool>,
    runtime_log_paused: Arc<AtomicBool>,
    tray_lightweight_mode: bool,
    health_probe_due: Option<std::time::Instant>,
    health_probe_running: bool,
    health_probe_failures: u8,
    health_recovery_running: bool,
    health_recovery_cancel: Arc<AtomicBool>,
    update_checking: bool,
    update_downloading: bool,
    update_dialog_open: bool,
    update_info: Option<GitHubUpdateInfo>,
}

fn parse_update_script_output(output: std::process::Output) -> anyhow::Result<GitHubUpdateInfo> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "GitHub update helper failed: {}",
            runtime_logs::summarize_error_for_display(stderr.trim())
        );
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| anyhow::anyhow!("GitHub update helper returned invalid UTF-8: {error}"))?;
    let json = stdout.trim().trim_start_matches('\u{feff}');
    if json.is_empty() {
        anyhow::bail!("GitHub update helper returned no result");
    }
    serde_json::from_str(json)
        .map_err(|error| anyhow::anyhow!("Invalid GitHub update result: {error}"))
}

fn decode_icon() -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let image = image::load_from_memory(LOGO_PNG)?.to_rgba8();
    let (width, height) = image.dimensions();

    // The source artwork intentionally contains a wide transparent halo. That
    // looks fine at full resolution, but makes the robot face (especially the
    // terminal underscore) disappear in title-bar and tray icon sizes. Trim the
    // halo once and use the same square, padded crop everywhere so every logo is
    // enlarged proportionally without stretching or clipping.
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0;
    let mut max_y = 0;
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel[3] >= 48 {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    if min_x > max_x || min_y > max_y {
        return Ok((image.into_raw(), width, height));
    }

    let content_width = max_x - min_x + 1;
    let content_height = max_y - min_y + 1;
    let side = content_width
        .max(content_height)
        .saturating_add((content_width.max(content_height) as f32 * 0.12) as u32)
        .min(width.min(height));
    let center_x = (min_x + max_x) / 2;
    let center_y = (min_y + max_y) / 2;
    let crop_x = center_x.saturating_sub(side / 2).min(width - side);
    let crop_y = center_y.saturating_sub(side / 2).min(height - side);
    let cropped = image::imageops::crop_imm(&image, crop_x, crop_y, side, side).to_image();
    Ok((cropped.into_raw(), side, side))
}

#[cfg(windows)]
fn system_ui_language() -> String {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }

    // LANGID uses the low ten bits for the primary language. Chinese is 0x04
    // for both simplified and traditional variants; every non-Chinese system
    // defaults to English as the common fallback.
    let primary_language = unsafe { GetUserDefaultUILanguage() } & 0x03ff;
    if primary_language == 0x04 {
        "zh".to_owned()
    } else {
        "en".to_owned()
    }
}

#[cfg(not(windows))]
fn system_ui_language() -> String {
    "en".to_owned()
}

fn localized_deployment_line(zh: bool, line: String) -> String {
    let localized = [
        (
            "[1/7]",
            "[1/7] 正在初始化本地凭据与数据库…",
            "[1/7] Initializing local credentials and database…",
        ),
        (
            "[2/7]",
            "[2/7] 正在启动 PostgreSQL、Redis 与 Sub2API…",
            "[2/7] Starting PostgreSQL, Redis, and Sub2API…",
        ),
        (
            "[3/7]",
            "[3/7] 本地服务已就绪，正在登录管理接口…",
            "[3/7] Local services are ready; signing in to the admin API…",
        ),
        (
            "[4/7]",
            "[4/7] 正在确认 Sub2API 合规状态…",
            "[4/7] Confirming Sub2API compliance state…",
        ),
        (
            "[5/7]",
            "[5/7] 正在创建或更新模型渠道…",
            "[5/7] Creating or updating model channels…",
        ),
        (
            "[6/7]",
            "[6/7] 正在写入 Codex 配置与本地访问密钥…",
            "[6/7] Writing Codex configuration and local access key…",
        ),
        ("[7/7]", "[7/7] 部署完成。", "[7/7] Deployment complete."),
    ];
    localized
        .into_iter()
        .find_map(|(prefix, chinese, english)| {
            line.starts_with(prefix)
                .then(|| if zh { chinese } else { english }.to_owned())
        })
        .unwrap_or_else(|| {
            format!(
                "{}: {}",
                if zh {
                    "部署诊断"
                } else {
                    "Deployment diagnostic"
                },
                localized_error_summary(zh, &line)
            )
        })
}

fn localized_error_summary(zh: bool, text: &str) -> String {
    for (marker, chinese, english) in [
        (
            "ROUTER_PROFILE_CREDENTIAL_MISSING",
            "无法读取已保存的 API Key。请返回模型页面重新输入并保存后再试。",
            "A saved API key is missing. Re-enter and save it on the Models page, then retry.",
        ),
        (
            "ROUTER_PROFILE_CREDENTIAL_READ_FAILED",
            "Windows 凭据管理器暂时无法读取 API Key。请重新输入并保存 Key 后再试。",
            "Windows Credential Manager could not read an API key. Re-enter and save the key, then retry.",
        ),
        (
            "ROUTER_PROFILE_CREDENTIAL_WRITE_FAILED",
            "Windows 凭据管理器无法创建隔离凭据；已回滚本次操作。请重试，或重新输入并保存 API Key。",
            "Windows Credential Manager could not create isolated credentials. This attempt was rolled back. Retry or re-enter and save the API key.",
        ),
        (
            "ROUTER_PROFILE_SAVE_FAILED",
            "隔离配置未能完整保存；已回滚本次操作，现有配置未被覆盖。",
            "The isolated profile could not be saved completely. This attempt was rolled back without overwriting the current profile.",
        ),
        (
            "ROUTER_PROFILE_ROLLBACK_INCOMPLETE",
            "隔离配置保存失败，且自动回滚未完全完成。请导出诊断信息后重启程序。",
            "The isolated profile failed to save and automatic rollback was incomplete. Export diagnostics and restart the app.",
        ),
    ] {
        if text.contains(marker) {
            return if zh { chinese } else { english }.to_owned();
        }
    }
    let summary = runtime_logs::summarize_error_for_display(text);
    if summary.contains("class=install_root_conflict") {
        return if zh {
            "另一份 Codex-Router 正在占用本地服务端口。请关闭另一份，或改用当前已运行的安装目录。"
        } else {
            "Another Codex-Router installation owns a required local port. Close it or use the installation that is already running."
        }
        .to_owned();
    }
    if summary.contains("class=port_conflict") {
        return if zh {
            "Codex-Router 所需的本地端口已被其他程序占用。请关闭占用程序后重试。"
        } else {
            "A local port required by Codex-Router is owned by another program. Close that program and retry."
        }
        .to_owned();
    }
    summary
}

fn usage_error_for_display(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.starts_with("class=")
        && !trimmed.contains('\r')
        && !trimmed.contains('\n')
        && trimmed.len() <= 512
        && trimmed.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'=' | b'_' | b'+' | b'|' | b' ' | b'-' | b'.')
        })
    {
        trimmed.to_owned()
    } else {
        runtime_logs::summarize_error_for_display(trimmed)
    }
}

fn append_bounded_log(logs: &mut String, message: &str) {
    logs.push_str(message);
    logs.push('\n');
    if logs.len() <= MAX_LOG_BYTES {
        return;
    }

    let mut start = logs.len().saturating_sub(RETAIN_LOG_BYTES);
    while start < logs.len() && !logs.is_char_boundary(start) {
        start += 1;
    }
    start = logs[start..]
        .find('\n')
        .map_or(logs.len(), |newline| start + newline + 1);

    let mut retained = String::with_capacity(logs.len().saturating_sub(start));
    retained.push_str(&logs[start..]);
    *logs = retained;
}

fn read_windows_credential(target: &str) -> Result<String, String> {
    use windows_sys::Win32::Security::Credentials::{
        CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
    };

    let target_wide = target
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
    if unsafe { CredReadW(target_wide.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) } == 0 {
        return Err(format!(
            "Windows Credential Manager read failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    if credential.is_null() {
        return Err("Windows Credential Manager returned an empty record".to_owned());
    }

    let result = (|| {
        let record = unsafe { &*credential };
        if record.CredentialBlobSize == 0 || record.CredentialBlob.is_null() {
            return Err("The local Router credential is empty".to_owned());
        }
        if record.CredentialBlobSize % 2 != 0 {
            return Err("The local Router credential has an invalid encoding".to_owned());
        }
        let units = unsafe {
            std::slice::from_raw_parts(
                record.CredentialBlob.cast::<u16>(),
                record.CredentialBlobSize as usize / 2,
            )
        };
        String::from_utf16(units)
            .map_err(|_| "The local Router credential has an invalid encoding".to_owned())
    })();
    unsafe { CredFree(credential.cast()) };
    result
}

fn router_deep_health(base_uri: &str, timeout: std::time::Duration) -> Result<(), String> {
    let base = url::Url::parse(base_uri).map_err(|_| "Router URL is invalid".to_owned())?;
    if base.scheme() != "http" {
        return Err("Router health checks require a local HTTP URL".to_owned());
    }
    let host = base
        .host_str()
        .ok_or_else(|| "Router URL has no host".to_owned())?;
    let local_host = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !local_host {
        return Err("Router health checks refuse non-local addresses".to_owned());
    }
    let port = base
        .port_or_known_default()
        .ok_or_else(|| "Router URL has no port".to_owned())?;
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("Router address resolution failed: {error}"))?
        .filter(|address| address.ip().is_loopback())
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err("Router URL did not resolve to a loopback address".to_owned());
    }

    let started = std::time::Instant::now();
    let mut last_error = None;
    let mut stream = None;
    for address in addresses {
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        match TcpStream::connect_timeout(&address, remaining) {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let mut stream = stream.ok_or_else(|| {
        format!(
            "Router connection failed: {}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "timeout".to_owned())
        )
    })?;
    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err("Router health check timed out".to_owned());
    }
    stream
        .set_read_timeout(Some(remaining))
        .map_err(|error| format!("Router read timeout setup failed: {error}"))?;
    stream
        .set_write_timeout(Some(remaining))
        .map_err(|error| format!("Router write timeout setup failed: {error}"))?;

    let api_key = read_windows_credential("CodexRouter/LocalApiKey")?;
    if api_key.contains(['\r', '\n']) {
        return Err("The local Router credential contains invalid characters".to_owned());
    }
    let host_header = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut request = format!(
        "GET /v1/models HTTP/1.1\r\nHost: {host_header}\r\nAuthorization: Bearer {api_key}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    )
    .into_bytes();
    let write_result = stream.write_all(&request);
    request.fill(0);
    write_result.map_err(|error| format!("Router health request failed: {error}"))?;

    let mut response = [0u8; 1024];
    let read = stream
        .read(&mut response)
        .map_err(|error| format!("Router health response failed: {error}"))?;
    if read == 0 {
        return Err("Router health response was empty".to_owned());
    }
    let first_line_end = response[..read]
        .windows(2)
        .position(|window| window == b"\r\n")
        .unwrap_or(read);
    let status_line = String::from_utf8_lossy(&response[..first_line_end]);
    if status_line.starts_with("HTTP/1.1 200 ") || status_line.starts_with("HTTP/1.0 200 ") {
        Ok(())
    } else {
        Err(format!("Router health returned {status_line}"))
    }
}

fn router_health_failure_recoverable(error: &str) -> bool {
    let normalized = error.to_ascii_lowercase();
    !(normalized.contains("credential")
        || normalized.contains("router url")
        || normalized.contains("non-local")
        || normalized.contains("invalid encoding")
        || normalized.contains("http/1.1 401")
        || normalized.contains("http/1.0 401")
        || normalized.contains("http/1.1 403")
        || normalized.contains("http/1.0 403"))
}

fn install_app_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let font_specs = [
        ("msyh", "C:/Windows/Fonts/msyh.ttc"),
        ("segoe", "C:/Windows/Fonts/segoeui.ttf"),
        ("segoe-symbol", "C:/Windows/Fonts/seguisym.ttf"),
        ("arial-black", "C:/Windows/Fonts/ariblk.ttf"),
        ("georgia-italic", "C:/Windows/Fonts/georgiai.ttf"),
        ("consolas", "C:/Windows/Fonts/consola.ttf"),
    ];
    for (name, path) in font_specs {
        if let Ok(data) = std::fs::read(path) {
            fonts
                .font_data
                .insert(name.into(), egui::FontData::from_owned(data).into());
        }
    }
    if fonts.font_data.contains_key("segoe") {
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "segoe".into());
    }
    if fonts.font_data.contains_key("msyh") {
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "msyh".into());
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .insert(0, "msyh".into());
    }
    if fonts.font_data.contains_key("segoe-symbol") {
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push("segoe-symbol".into());
    }
    if fonts.font_data.contains_key("consolas") {
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .insert(0, "consolas".into());
    }
    let mut display_fonts = Vec::new();
    if fonts.font_data.contains_key("arial-black") {
        display_fonts.push("arial-black".into());
    }
    if fonts.font_data.contains_key("msyh") {
        display_fonts.push("msyh".into());
    }
    if fonts.font_data.contains_key("segoe-symbol") {
        display_fonts.push("segoe-symbol".into());
    }
    fonts
        .families
        .insert(theme::display_family(), display_fonts);
    let mut serif_fonts = Vec::new();
    if fonts.font_data.contains_key("georgia-italic") {
        serif_fonts.push("georgia-italic".into());
    }
    if fonts.font_data.contains_key("msyh") {
        serif_fonts.push("msyh".into());
    }
    fonts.families.insert(theme::serif_family(), serif_fonts);
    ctx.set_fonts(fonts);
}

fn install_lightweight_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let fallback = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    fonts
        .families
        .insert(theme::display_family(), fallback.clone());
    fonts.families.insert(theme::serif_family(), fallback);
    ctx.set_fonts(fonts);
}

impl CodexRouterApp {
    fn new(cc: &eframe::CreationContext<'_>, start_in_background: bool) -> Self {
        let (event_tx, event_rx) = channel();
        let (runtime_log_tx, runtime_log_rx) = runtime_logs::bounded_channel();
        let router_root = RouterConfig::find_router_root();
        let ui_preferences_path = router_root.join("codex-router-ui-preferences.json");
        let mut ui_preferences = UiPreferences::load(&ui_preferences_path).unwrap_or_default();
        if ui_preferences.close_warning_version < 1 {
            ui_preferences.close_behavior = CloseBehavior::Ask;
            ui_preferences.close_warning_version = 1;
        }
        // Rewrite legacy preference files once so newly defaulted fields are
        // explicit and portable across subsequent versions.
        let _ = ui_preferences.save(&ui_preferences_path);
        let close_behavior = ui_preferences.close_behavior;
        let active_profile_id = ui_preferences.active_profile_id;
        let monitor_subscription_order = ui_preferences.monitor_subscription_order;
        let monitor_api_order = ui_preferences.monitor_api_order;
        let share_codex_state = ui_preferences.share_codex_state;
        let isolation_profiles = profiles::list_profiles(&router_root).unwrap_or_default();
        let config_path = router_root.join("codex-router-config.json");
        let (mut config, mut page, configured) = match RouterConfig::load(&config_path) {
            Ok(cfg) => (cfg, Page::Dashboard, true),
            Err(_) => (RouterConfig::default(), Page::Welcome, false),
        };
        config.version = CURRENT_CONFIG_VERSION.to_owned();
        logic::normalize_default_model(&mut config);
        if config.deploy.cc_switch_db.trim().is_empty() {
            if let Some(path) = logic::detect_cc_switch_db() {
                config.deploy.cc_switch_db = path.display().to_string();
            }
        }
        if config.accepted_terms_version != CURRENT_TERMS_VERSION {
            config.accept_compliance = false;
            config.accepted_terms_version.clear();
            if configured {
                page = Page::Finish;
            }
        }
        if let Some(saved_theme) = cc
            .storage
            .and_then(|storage| storage.get_string("codex-router-ui-theme-v3"))
        {
            if matches!(saved_theme.as_str(), "coffee" | "sky") {
                config.ui_theme = saved_theme;
            }
        }
        let ui_language = cc
            .storage
            .and_then(|storage| storage.get_string("codex-router-ui-language-v1"))
            .filter(|language| matches!(language.as_str(), "zh" | "en"))
            .unwrap_or_else(system_ui_language);
        if !start_in_background {
            install_app_fonts(&cc.egui_ctx);
        } else {
            install_lightweight_fonts(&cc.egui_ctx);
        }
        theme::install(&cc.egui_ctx, &theme::palette(&config.ui_theme));
        let installed_theme = config.ui_theme.clone();
        let installed_compact_layout = cc.egui_ctx.content_rect().height() < 700.0;
        let tray_menu = tray_icon::menu::Menu::new();
        let menu_text = |zh_text, en_text| {
            if ui_language == "zh" {
                zh_text
            } else {
                en_text
            }
        };
        let open_console =
            tray_icon::menu::MenuItem::new(menu_text("打开控制台", "Open console"), true, None);
        let choose_profile = tray_icon::menu::MenuItem::new(
            menu_text("选择配置", "Choose configuration"),
            true,
            None,
        );
        let apply_current = tray_icon::menu::MenuItem::new(
            menu_text("保存并应用当前配置", "Save and apply current configuration"),
            true,
            None,
        );
        let start_forwarding =
            tray_icon::menu::MenuItem::new(menu_text("启动转发", "Start forwarding"), true, None);
        let stop_forwarding =
            tray_icon::menu::MenuItem::new(menu_text("关闭转发", "Stop forwarding"), true, None);
        let hide_window = tray_icon::menu::MenuItem::new(
            menu_text("关闭配置窗口", "Close configuration window"),
            true,
            None,
        );
        let exit_app = tray_icon::menu::MenuItem::new(menu_text("退出软件", "Exit"), true, None);
        for item in [
            &open_console,
            &choose_profile,
            &apply_current,
            &start_forwarding,
            &stop_forwarding,
            &hide_window,
            &exit_app,
        ] {
            let _ = tray_menu.append(item);
        }
        let open_console_id = open_console.id().clone();
        let choose_profile_id = choose_profile.id().clone();
        let apply_current_id = apply_current.id().clone();
        let start_forwarding_id = start_forwarding.id().clone();
        let stop_forwarding_id = stop_forwarding.id().clone();
        let hide_window_id = hide_window.id().clone();
        let exit_app_id = exit_app.id().clone();

        let tray = decode_icon().ok().and_then(|(rgba, width, height)| {
            tray_icon::Icon::from_rgba(rgba, width, height)
                .ok()
                .and_then(|icon| {
                    tray_icon::TrayIconBuilder::new()
                        .with_tooltip("Codex-Router")
                        .with_icon(icon)
                        .with_menu(Box::new(tray_menu))
                        .with_menu_on_left_click(false)
                        .build()
                        .ok()
                })
        });
        if let Some(tray_icon) = &tray {
            let tray_id = tray_icon.id().clone();
            let tray_ctx = cc.egui_ctx.clone();
            let tray_tx = event_tx.clone();
            tray_icon::TrayIconEvent::set_event_handler(Some(
                move |event: tray_icon::TrayIconEvent| {
                    if event.id() != &tray_id {
                        return;
                    }
                    let restore = matches!(
                        event,
                        tray_icon::TrayIconEvent::Click {
                            button: tray_icon::MouseButton::Left,
                            button_state: tray_icon::MouseButtonState::Up,
                            ..
                        } | tray_icon::TrayIconEvent::DoubleClick {
                            button: tray_icon::MouseButton::Left,
                            ..
                        }
                    );
                    if restore {
                        let _ = tray_tx.send(AppEvent::Tray(TrayAction::RestoreWindow));
                        tray_ctx.request_repaint();
                    }
                },
            ));
        }
        let menu_ctx = cc.egui_ctx.clone();
        let menu_tx = event_tx.clone();
        tray_icon::menu::MenuEvent::set_event_handler(Some(
            move |event: tray_icon::menu::MenuEvent| {
                let action = if event.id() == &open_console_id {
                    Some(TrayAction::OpenConsole)
                } else if event.id() == &choose_profile_id {
                    Some(TrayAction::ChooseProfile)
                } else if event.id() == &apply_current_id {
                    Some(TrayAction::ApplyCurrent)
                } else if event.id() == &start_forwarding_id {
                    Some(TrayAction::StartForwarding)
                } else if event.id() == &stop_forwarding_id {
                    Some(TrayAction::StopForwarding)
                } else if event.id() == &hide_window_id {
                    Some(TrayAction::HideWindow)
                } else if event.id() == &exit_app_id {
                    Some(TrayAction::Exit)
                } else {
                    None
                };
                if let Some(action) = action {
                    let _ = menu_tx.send(AppEvent::Tray(action));
                    menu_ctx.request_repaint();
                }
            },
        ));
        let project_path_input = router_root.to_string_lossy().to_string();
        let router_mode_enabled = logic::codex_router_mode_active(&config);
        let oauth_recovery_due = router_mode_enabled
            .then(|| std::time::Instant::now() + std::time::Duration::from_secs(60 * 60));
        let runtime_log_paused = Arc::new(AtomicBool::new(start_in_background));
        let mut app = Self {
            page,
            router_root,
            project_path_input,
            config,
            temp_model: ModelConfig::default(),
            editing_model: None,
            model_from_wizard: true,
            proxy_from_wizard: true,
            status_text: String::new(),
            logs: String::new(),
            event_rx,
            event_tx,
            runtime_log_rx,
            applying: false,
            configured,
            logo_texture: None,
            fonts_loaded: !start_in_background,
            tray_icon: tray,
            last_page: page,
            profiles_return_page: Page::Dashboard,
            page_changed_at: std::time::Instant::now(),
            installed_theme,
            installed_compact_layout,
            ui_language,
            terms_open: false,
            terms_scroll_complete: false,
            advanced_json_open: false,
            advanced_json_draft: String::new(),
            reasoning_open: false,
            reasoning_mode_draft: "auto".to_owned(),
            reasoning_levels_draft: String::new(),
            reasoning_default_draft: String::new(),
            reasoning_fast_supported_draft: false,
            reasoning_fast_mode_draft: false,
            close_behavior,
            close_prompt_open: false,
            remember_close_choice: false,
            exit_after_prompt: false,
            local_profile_name_input: String::new(),
            cc_profile_name_input: String::new(),
            isolation_profiles,
            active_profile_id,
            pending_profile_activation: None,
            oauth_accounts: Vec::new(),
            oauth_loading: false,
            oauth_error: String::new(),
            oauth_return_page: Page::Dashboard,
            oauth_provider_draft: "openai".to_owned(),
            provider_oauth_running: false,
            pending_oauth_provider: None,
            oauth_revoke_target: None,
            oauth_revoking: false,
            oauth_manual_model_target: None,
            oauth_manual_model_id_draft: String::new(),
            oauth_manual_model_alias_draft: String::new(),
            usage_snapshot: None,
            usage_snapshot_profile_key: String::new(),
            usage_loading: false,
            usage_error: String::new(),
            usage_return_page: Page::Dashboard,
            usage_refresh_due: None,
            monitor_subscription_order,
            monitor_api_order,
            share_codex_state,
            router_mode_enabled,
            router_mode_switching: false,
            oauth_recovery_due,
            oauth_recovery_running: false,
            grok_sso_dialog_open: false,
            grok_sso_draft: String::new(),
            grok_sso_importing: false,
            grok_sso_error: String::new(),
            grok_sso_auto_select_pending: false,
            channel_preset_dialog_open: false,
            sub2api_intro_open: false,
            log_scroll_to_bottom: true,
            log_follow_latest: true,
            log_dialog_open: false,
            runtime_log_stop: Arc::new(AtomicBool::new(false)),
            runtime_log_paused,
            tray_lightweight_mode: start_in_background,
            health_probe_due: router_mode_enabled
                .then(|| std::time::Instant::now() + FAILED_PROBE_RETRY_INTERVAL),
            health_probe_running: false,
            health_probe_failures: 0,
            health_recovery_running: false,
            health_recovery_cancel: Arc::new(AtomicBool::new(false)),
            update_checking: false,
            update_downloading: false,
            update_dialog_open: false,
            update_info: None,
        };
        if !start_in_background {
            app.load_usage_monitor_cache();
            app.refresh_oauth_accounts();
        } else if let Some(tray) = &app.tray_icon {
            let _ = tray.set_tooltip(Some(if app.ui_language == "zh" {
                "Codex-Router - 轻量托盘模式（仅保留转发保护）"
            } else {
                "Codex-Router - lightweight tray mode (forwarding protection only)"
            }));
        }
        runtime_logs::spawn(
            app.router_root.clone(),
            runtime_log_tx,
            cc.egui_ctx.clone(),
            app.runtime_log_stop.clone(),
            app.runtime_log_paused.clone(),
        );
        app
    }

    fn persist_close_behavior(&mut self) -> bool {
        let path = self.router_root.join("codex-router-ui-preferences.json");
        let preferences = UiPreferences {
            close_behavior: self.close_behavior,
            close_warning_version: 1,
            active_profile_id: self.active_profile_id.clone(),
            monitor_subscription_order: self.monitor_subscription_order.clone(),
            monitor_api_order: self.monitor_api_order.clone(),
            share_codex_state: self.share_codex_state,
        };
        match preferences.save(&path) {
            Ok(()) => true,
            Err(error) => {
                let message = if self.ui_language == "zh" {
                    format!("无法保存窗口关闭设置：{error}")
                } else {
                    format!("Could not save the window close setting: {error}")
                };
                self.report_error(message);
                false
            }
        }
    }

    fn persist_ui_preferences(&mut self) -> bool {
        self.persist_close_behavior()
    }

    fn refresh_isolation_profiles(&mut self) {
        match profiles::list_profiles(&self.router_root) {
            Ok(items) => self.isolation_profiles = items,
            Err(error) => self.report_error(format!("无法读取隔离配置：{error}")),
        }
    }

    fn open_profiles(&mut self) {
        if self.page != Page::Profiles {
            self.profiles_return_page = self.page;
        }
        match profiles::ensure_original_codex_snapshot(&self.router_root, &self.config) {
            Ok(()) => {
                self.refresh_isolation_profiles();
                self.page = Page::Profiles;
            }
            Err(error) => {
                self.report_error(format!("无法建立 Codex 原始配置快照：{error}"));
                self.page = Page::Profiles;
            }
        }
    }

    fn create_isolation_profile(&mut self, kind: IsolationKind, requested_name: String) {
        if self.applying {
            return;
        }
        if !self.config.accept_compliance
            || self.config.accepted_terms_version != CURRENT_TERMS_VERSION
        {
            self.report_error("请先阅读并同意当前使用与分发承诺，再创建可应用的隔离配置");
            return;
        }
        if self.config.models.is_empty() {
            self.report_error("请至少添加一个模型，再创建隔离配置");
            return;
        }
        let requested_name = requested_name.trim().to_owned();
        match profiles::create_profile(&self.router_root, &requested_name, kind, &self.config) {
            Ok((profile, isolated_config)) => {
                let profile_id = profile.id.clone();
                self.config = isolated_config;
                self.pending_profile_activation = Some(profile_id);
                self.refresh_isolation_profiles();
                if self.apply_all_with_backup(true, None) {
                    if self.local_profile_name_input.trim() == requested_name {
                        self.local_profile_name_input.clear();
                    }
                    if self.cc_profile_name_input.trim() == requested_name {
                        self.cc_profile_name_input.clear();
                    }
                    self.status_text = match kind {
                        IsolationKind::Local => {
                            format!("正在创建并应用本地隔离配置“{}”…", profile.name)
                        }
                        IsolationKind::CcSwitch => {
                            format!("正在创建“{}”、应用到本机并同步到 CC Switch…", profile.name)
                        }
                    };
                } else {
                    self.pending_profile_activation = None;
                }
            }
            Err(error) => self.report_error(error.to_string()),
        }
    }

    fn apply_isolation_profile(&mut self, profile: &IsolationProfile) {
        if self.applying {
            return;
        }
        let loaded = profiles::load_profile_config(&self.router_root, profile);
        let target_config = match loaded {
            Ok(config) => config,
            Err(error) => {
                self.report_error(format!("无法读取隔离配置“{}”：{error}", profile.name));
                return;
            }
        };
        if !target_config.accept_compliance
            || target_config.accepted_terms_version != CURRENT_TERMS_VERSION
            || target_config.models.is_empty()
        {
            self.report_error("该隔离配置缺少当前条款确认或模型，请先在控制台补全后再应用");
            return;
        }
        let config_lock = match profiles::acquire_config_apply_lock(
            &self.router_root,
            std::time::Duration::from_secs(10),
        ) {
            Ok(lock) => lock,
            Err(error) => {
                self.report_error(format!("无法开始配置切换：{error}"));
                return;
            }
        };
        if let Err(error) = profiles::capture_restore_point(
            &self.router_root,
            &self.config,
            &format!("切换到“{}”之前", profile.name),
        ) {
            self.report_error(format!("切换前备份失败，已停止操作：{error}"));
            return;
        }
        let state_outcome = match profiles::restore_profile_codex_state(
            &self.router_root,
            profile,
            &target_config,
            self.share_codex_state,
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.report_error(format!(
                    "无法准备配置“{}”的 Codex 状态：{error}",
                    profile.name
                ));
                return;
            }
        };
        self.config = target_config;
        self.pending_profile_activation = Some(profile.id.clone());
        if self.apply_all_with_backup(false, Some(config_lock)) {
            self.status_text = if state_outcome.account_changed {
                format!(
                    "检测到不同 Codex 账号，正在使用“{}”自己的登录与设置并应用路由…",
                    profile.name
                )
            } else if state_outcome.shared_state_preserved {
                format!(
                    "正在应用“{}”；当前账号、会话与个人设置保持共享…",
                    profile.name
                )
            } else {
                format!("正在直接应用隔离配置“{}”…", profile.name)
            };
        } else {
            self.pending_profile_activation = None;
        }
    }

    fn restore_original_codex(&mut self) {
        if self.applying {
            return;
        }
        let _config_lock = match profiles::acquire_config_apply_lock(
            &self.router_root,
            std::time::Duration::from_secs(10),
        ) {
            Ok(lock) => lock,
            Err(error) => {
                self.report_error(format!("无法开始恢复：{error}"));
                return;
            }
        };
        if let Err(error) = profiles::capture_restore_point(
            &self.router_root,
            &self.config,
            "恢复 Codex 官方登录配置之前",
        ) {
            self.report_error(format!("恢复前备份失败，已停止操作：{error}"));
            return;
        }
        match profiles::restore_original_codex(
            &self.router_root,
            &self.config,
            self.share_codex_state,
        ) {
            Ok(outcome) => {
                self.active_profile_id.clear();
                self.pending_profile_activation = None;
                self.router_mode_enabled = false;
                self.oauth_recovery_due = None;
                self.persist_ui_preferences();
                self.status_text = if outcome.shared_state_preserved && outcome.auth_available {
                    "已恢复 Codex 官方路由；当前账号、会话记录和个人设置保持共享。请完全退出并重新打开 Codex。".into()
                } else if outcome.account_changed && outcome.auth_available {
                    "已恢复 Codex 官方路由及其账号快照；检测到 Codex 账号不同，因此未合并两个账号的会话与设置。请完全退出并重新打开 Codex。".into()
                } else if outcome.auth_available {
                    "已恢复 Codex 官方登录配置与有效登录快照。请完全退出并重新打开 Codex。".into()
                } else {
                    "已恢复 Codex 官方配置；旧登录快照缺失或无效，已安全移除以避免 Windows 设置循环。重新打开 Codex 后请按官方流程登录。".into()
                };
            }
            Err(error) => {
                self.report_error(format!("无法恢复 Codex 官方登录配置：{error}"));
            }
        }
    }

    fn restore_previous_codex(&mut self) {
        if self.applying {
            return;
        }
        let target = match profiles::list_restore_points(&self.router_root) {
            Ok(points) => points.into_iter().next(),
            Err(error) => {
                self.report_error(format!("无法读取应用前备份：{error}"));
                return;
            }
        };
        let Some(target) = target else {
            self.status_text = "还没有可还原的应用前配置".into();
            return;
        };
        let _config_lock = match profiles::acquire_config_apply_lock(
            &self.router_root,
            std::time::Duration::from_secs(10),
        ) {
            Ok(lock) => lock,
            Err(error) => {
                self.report_error(format!("无法开始还原：{error}"));
                return;
            }
        };
        if let Err(error) = profiles::capture_restore_point(
            &self.router_root,
            &self.config,
            "执行返回上一次配置之前",
        ) {
            self.report_error(format!("还原前备份失败，已停止操作：{error}"));
            return;
        }
        match profiles::restore_point_config(
            &self.router_root,
            &target,
            &self.config,
            self.share_codex_state,
        ) {
            Ok((config, outcome)) => {
                self.config = config;
                self.active_profile_id.clear();
                self.pending_profile_activation = None;
                self.router_mode_enabled = logic::codex_router_mode_active(&self.config);
                self.oauth_recovery_due = self
                    .router_mode_enabled
                    .then(|| std::time::Instant::now() + std::time::Duration::from_secs(60 * 60));
                self.persist_ui_preferences();
                self.status_text = if outcome.shared_state_preserved {
                    format!(
                        "已返回上一次配置（{}），当前账号、会话记录和个人设置保持共享。请完全退出并重新打开 Codex。",
                        target.label
                    )
                } else {
                    format!(
                        "已返回上一次配置（{}）；检测到不同 Codex 账号，已使用该账号自己的完整快照。请完全退出并重新打开 Codex。",
                        target.label
                    )
                };
            }
            Err(error) => self.report_error(format!("无法返回上一次配置：{error}")),
        }
    }

    fn minimize_to_tray(&mut self, ctx: &egui::Context) {
        self.close_prompt_open = false;
        self.remember_close_choice = false;
        if self.tray_icon.is_some() {
            self.tray_lightweight_mode = true;
            self.runtime_log_paused.store(true, Ordering::Relaxed);
            self.usage_refresh_due = None;
            self.oauth_recovery_due = None;
            if self.fonts_loaded {
                install_lightweight_fonts(ctx);
                self.fonts_loaded = false;
                self.logo_texture = None;
            }
            if let Some(tray) = &self.tray_icon {
                let _ = tray.set_tooltip(Some(if self.ui_language == "zh" {
                    "Codex-Router - 轻量托盘模式（仅保留转发保护）"
                } else {
                    "Codex-Router - lightweight tray mode (forwarding protection only)"
                }));
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        } else {
            self.status_text = if self.ui_language == "zh" {
                "系统托盘不可用，窗口保持打开".into()
            } else {
                "The system tray is unavailable, so the window was kept open".into()
            };
        }
    }

    fn restore_from_tray(&mut self, ctx: &egui::Context) {
        let was_lightweight = self.tray_lightweight_mode;
        if !self.fonts_loaded {
            install_app_fonts(ctx);
            self.fonts_loaded = true;
            self.installed_theme.clear();
        }
        self.tray_lightweight_mode = false;
        self.load_logo_texture(ctx);
        self.runtime_log_paused.store(false, Ordering::Relaxed);
        if self.router_mode_enabled
            && self
                .config
                .models
                .iter()
                .any(|model| model.source == "oauth")
        {
            self.oauth_recovery_due =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(60 * 60));
        }
        if let Some(tray) = &self.tray_icon {
            let _ = tray.set_tooltip(Some("Codex-Router"));
        }
        if was_lightweight {
            if self.usage_snapshot.is_none() {
                self.load_usage_monitor_cache();
            }
            self.status_text = if self.ui_language == "zh" {
                "已退出托盘轻量模式；界面刷新、日志和账号维护已恢复".to_owned()
            } else {
                "Left lightweight tray mode; UI refresh, logs, and account maintenance resumed"
                    .to_owned()
            };
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    fn request_exit(&mut self, ctx: &egui::Context) {
        self.close_prompt_open = false;
        self.remember_close_choice = false;
        self.exit_after_prompt = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|input| input.viewport().close_requested()) {
            return;
        }
        if self.exit_after_prompt {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        match self.close_behavior {
            CloseBehavior::Ask => {
                self.close_prompt_open = true;
                ctx.request_repaint();
            }
            CloseBehavior::MinimizeToTray => self.minimize_to_tray(ctx),
            CloseBehavior::Exit => self.request_exit(ctx),
        }
    }

    fn handle_native_minimize(&mut self, ctx: &egui::Context) {
        if !self.tray_lightweight_mode
            && self.tray_icon.is_some()
            && ctx.input(|input| input.viewport().minimized == Some(true))
        {
            self.minimize_to_tray(ctx);
        }
    }

    fn process_app_events(&mut self, ctx: &egui::Context) {
        let zh = self.ui_language == "zh";
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                AppEvent::Log(message) => self.log(message),
                AppEvent::Complete => {
                    self.applying = false;
                    self.configured = true;
                    self.router_mode_enabled = true;
                    self.router_mode_switching = false;
                    self.oauth_recovery_due =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(60 * 60));
                    self.health_probe_failures = 0;
                    self.health_probe_due =
                        Some(std::time::Instant::now() + FAILED_PROBE_RETRY_INTERVAL);
                    self.status_text = if zh {
                        "配置完成：模型渠道、Codex 和所选集成均已生效"
                    } else {
                        "Configuration complete: model channels, Codex, and integrations are active"
                    }
                    .into();
                    self.log(if zh {
                        "配置完成"
                    } else {
                        "Configuration complete"
                    });
                    self.schedule_usage_refresh();
                    if let Some(profile_id) = self.pending_profile_activation.take() {
                        let switched_profile = self.active_profile_id != profile_id;
                        match profiles::update_profile_state(
                            &self.router_root,
                            &profile_id,
                            &self.config,
                        ) {
                            Ok(()) => {
                                self.active_profile_id = profile_id;
                                self.persist_ui_preferences();
                                self.refresh_isolation_profiles();
                                self.status_text = if switched_profile {
                                    if zh {
                                        "配置切换完成；已有 Codex 任务不会热切换，请新建任务使用新配置"
                                    } else {
                                        "Configuration switched. Existing Codex tasks do not hot-switch; start a new task to use it"
                                    }
                                } else if zh {
                                    "当前配置已保存并应用；OAuth 授权、模型和回退策略已写入本配置"
                                } else {
                                    "Current profile saved and applied. Its OAuth bindings, models, and fallback policy were updated"
                                }
                                .into();
                            }
                            Err(error) => {
                                let detail =
                                    runtime_logs::summarize_error_for_display(&error.to_string());
                                self.status_text = format!(
                                    "{}；但无法更新隔离配置快照：{detail}",
                                    self.status_text
                                );
                                self.log(self.status_text.clone());
                            }
                        }
                    }
                }
                AppEvent::Error(error) => {
                    self.applying = false;
                    self.router_mode_switching = false;
                    self.router_mode_enabled = logic::codex_router_mode_active(&self.config);
                    self.pending_profile_activation = None;
                    let detail = localized_error_summary(zh, &error);
                    self.status_text = format!(
                        "{}: {detail}",
                        if zh {
                            "配置失败"
                        } else {
                            "Configuration failed"
                        }
                    );
                    self.log(self.status_text.clone());
                }
                AppEvent::OAuthAccountsLoaded(mut accounts) => {
                    self.oauth_loading = false;
                    self.oauth_error.clear();
                    for account in &mut accounts {
                        if !account.error.trim().is_empty() {
                            account.error =
                                runtime_logs::summarize_error_for_display(&account.error);
                        }
                    }
                    let mut selected = self.config.oauth_account_ids.clone().unwrap_or_default();
                    let mut seen = self.config.oauth_seen_account_ids.clone();
                    let bound_account_ids = accounts
                        .iter()
                        .filter(|account| account.bound_to_router)
                        .map(|account| account.id)
                        .collect::<Vec<_>>();
                    let auto_added = logic::enroll_unseen_oauth_accounts(
                        &mut selected,
                        &mut seen,
                        &bound_account_ids,
                    );
                    self.config.oauth_account_ids = Some(selected);
                    self.config.oauth_seen_account_ids = seen;
                    if self.grok_sso_auto_select_pending {
                        self.grok_sso_auto_select_pending = false;
                    }
                    self.oauth_accounts = accounts;
                    if auto_added > 0 {
                        let selected = self.config.oauth_account_ids.clone();
                        let seen = self.config.oauth_seen_account_ids.clone();
                        let config_path = self.router_root.join("codex-router-config.json");
                        let persisted = (|| -> anyhow::Result<()> {
                            let mut saved = if config_path.is_file() {
                                RouterConfig::load(&config_path)?
                            } else {
                                self.config.clone()
                            };
                            saved.oauth_account_ids = selected.clone();
                            saved.oauth_seen_account_ids = seen.clone();
                            saved.save(&config_path)?;
                            if !self.active_profile_id.trim().is_empty() {
                                profiles::update_profile_oauth_selection(
                                    &self.router_root,
                                    &self.active_profile_id,
                                    selected,
                                    seen,
                                )?;
                            }
                            Ok(())
                        })();
                        if let Err(error) = persisted {
                            self.log(format!(
                                "OAuth 自动同步保存失败：{}",
                                runtime_logs::summarize_error_for_display(&error.to_string())
                            ));
                        }
                        self.schedule_usage_refresh();
                        self.status_text = if zh {
                            format!(
                                "已自动把 {auto_added} 个新 OAuth 账号加入当前配置，并刷新用量统计"
                            )
                        } else {
                            format!(
                                "Added {auto_added} new OAuth account(s) to this profile and refreshed usage statistics"
                            )
                        };
                    }
                }
                AppEvent::OAuthAccountsError(error) => {
                    self.oauth_loading = false;
                    let detail = runtime_logs::summarize_error_for_display(&error);
                    self.oauth_error.clone_from(&detail);
                    self.log(format!(
                        "{}: {detail}",
                        if zh {
                            "OAuth 账号加载失败"
                        } else {
                            "Could not load OAuth accounts"
                        }
                    ));
                }
                AppEvent::ProviderOAuthFinished => {
                    self.provider_oauth_running = false;
                    self.status_text = if zh {
                        "OAuth 登录成功，正在自动同步到当前配置与用量统计…"
                    } else {
                        "OAuth login succeeded. Syncing it to this profile and usage statistics…"
                    }
                    .to_owned();
                    self.refresh_oauth_accounts();
                }
                AppEvent::ProviderOAuthError(error) => {
                    self.provider_oauth_running = false;
                    let detail = runtime_logs::summarize_error_for_display(&error);
                    self.status_text = if zh {
                        format!("OAuth 登录未完成：{detail}")
                    } else {
                        format!("OAuth login did not complete: {detail}")
                    }
                    .to_owned();
                    self.log(self.status_text.clone());
                    self.log(self.status_text.clone());
                }
                AppEvent::UsageLoaded {
                    profile_key,
                    snapshot,
                } => {
                    self.usage_loading = false;
                    self.usage_error.clear();
                    if profile_key != self.active_route_profile_key() {
                        continue;
                    }
                    let mut snapshot = *snapshot;
                    for account in snapshot
                        .subscriptions
                        .iter_mut()
                        .chain(snapshot.api_channels.iter_mut())
                    {
                        if !account.status_detail.trim().is_empty() {
                            account.status_detail =
                                runtime_logs::summarize_error_for_display(&account.status_detail);
                        }
                        if !account.query_note.trim().is_empty() {
                            account.query_note =
                                runtime_logs::summarize_error_for_display(&account.query_note);
                        }
                    }
                    Self::sort_usage_accounts(
                        &mut snapshot.subscriptions,
                        &self.monitor_subscription_order,
                    );
                    Self::sort_usage_accounts(&mut snapshot.api_channels, &self.monitor_api_order);
                    if let Err(error) = self.save_usage_monitor_cache(&profile_key, &snapshot) {
                        self.log(format!(
                            "用量监控缓存保存失败：{}",
                            runtime_logs::summarize_error_for_display(&error.to_string())
                        ));
                    }
                    self.usage_snapshot = Some(snapshot);
                    self.usage_snapshot_profile_key = profile_key;
                }
                AppEvent::UsageError(error) => {
                    self.usage_loading = false;
                    let detail = usage_error_for_display(&error);
                    self.usage_error = if self.usage_snapshot.is_some()
                        && self.usage_snapshot_profile_key == self.active_route_profile_key()
                    {
                        if zh {
                            format!("刷新失败，下面保留上次成功数据；{detail}")
                        } else {
                            format!("Refresh failed; the last successful data is retained below. {detail}")
                        }
                    } else {
                        detail.clone()
                    };
                    self.log(format!(
                        "{}: {detail}",
                        if zh {
                            "用量查询失败"
                        } else {
                            "Usage query failed"
                        }
                    ));
                }
                AppEvent::RouterModeDisabled(outcome) => {
                    self.router_mode_switching = false;
                    self.router_mode_enabled = false;
                    self.oauth_recovery_due = None;
                    self.health_probe_due = None;
                    self.health_probe_failures = 0;
                    self.active_profile_id.clear();
                    self.pending_profile_activation = None;
                    self.persist_ui_preferences();
                    self.status_text = if zh {
                        if outcome.shared_state_preserved && outcome.auth_available {
                            "已切回 Codex 官方路由并停止本地转发；当前账号、会话与设置保持共享。请完全退出并重新打开 Codex。"
                        } else if outcome.auth_available {
                            "已切回 Codex 官方路由并停止本地转发。请完全退出并重新打开 Codex。"
                        } else {
                            "已切回 Codex 官方路由并停止本地转发；官方登录快照缺失或无效，请重新打开 Codex 后登录。"
                        }
                    } else if outcome.shared_state_preserved && outcome.auth_available {
                        "Restored the official Codex route and stopped local forwarding while preserving the current account, tasks, and settings. Fully restart Codex."
                    } else if outcome.auth_available {
                        "Restored the official Codex route and stopped local forwarding. Fully restart Codex."
                    } else {
                        "Restored the official Codex route and stopped local forwarding. The official login snapshot was unavailable; sign in after restarting Codex."
                    }
                    .to_owned();
                    self.log(if zh {
                        "已关闭 Router 路由，Codex 已恢复官方配置"
                    } else {
                        "Router mode disabled; Codex official configuration restored"
                    });
                }
                AppEvent::RouterModeSwitchError(error) => {
                    self.router_mode_switching = false;
                    self.router_mode_enabled = logic::codex_router_mode_active(&self.config);
                    let detail = runtime_logs::summarize_error_for_display(&error);
                    self.status_text = if zh {
                        format!("路由切换失败：{detail}")
                    } else {
                        format!("Could not switch routes: {detail}")
                    };
                    self.log(self.status_text.clone());
                }
                AppEvent::OAuthRecoveryFinished => {
                    self.oauth_recovery_running = false;
                    self.oauth_recovery_due =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(60 * 60));
                    self.log(if zh {
                        "OAuth 每小时恢复探测完成"
                    } else {
                        "Hourly OAuth recovery probe completed"
                    });
                }
                AppEvent::OAuthRecoveryError(error) => {
                    self.oauth_recovery_running = false;
                    self.oauth_recovery_due =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(60 * 60));
                    let detail = runtime_logs::summarize_error_for_display(&error);
                    self.log(if zh {
                        format!("OAuth 每小时恢复探测未成功：{detail}")
                    } else {
                        format!("Hourly OAuth recovery probe did not succeed: {detail}")
                    });
                }
                AppEvent::GrokSsoImported => {
                    self.grok_sso_importing = false;
                    self.grok_sso_dialog_open = false;
                    self.grok_sso_draft.clear();
                    self.grok_sso_error.clear();
                    self.grok_sso_auto_select_pending = true;
                    self.status_text = if zh {
                        "Grok 授权码已导入；账号已加入当前路由配置".to_owned()
                    } else {
                        "Grok authorization imported and added to this route profile".to_owned()
                    };
                    self.log(self.status_text.clone());
                    self.refresh_oauth_accounts();
                }
                AppEvent::GrokSsoImportError(error) => {
                    self.grok_sso_importing = false;
                    let detail = runtime_logs::summarize_error_for_display(&error);
                    self.grok_sso_error.clone_from(&detail);
                    self.log(format!(
                        "{}: {detail}",
                        if zh {
                            "Grok 授权导入失败"
                        } else {
                            "Grok authorization import failed"
                        }
                    ));
                }
                AppEvent::OAuthAccountRevoked {
                    account_id,
                    account_name,
                } => {
                    self.oauth_revoking = false;
                    self.oauth_revoke_target = None;
                    logic::remove_oauth_account_references(&mut self.config, account_id);
                    self.schedule_usage_refresh();
                    let mut cleanup_errors = Vec::new();
                    if let Err(error) = self
                        .config
                        .save(&self.router_root.join("codex-router-config.json"))
                    {
                        cleanup_errors.push(format!(
                            "无法保存当前配置: {}",
                            runtime_logs::summarize_error_for_display(&error.to_string())
                        ));
                    }
                    if let Err(error) =
                        profiles::purge_oauth_account_references(&self.router_root, account_id)
                    {
                        cleanup_errors.push(format!(
                            "无法清理隔离配置: {}",
                            runtime_logs::summarize_error_for_display(&error.to_string())
                        ));
                    }
                    self.oauth_accounts
                        .retain(|account| account.id != account_id);
                    if cleanup_errors.is_empty() {
                        self.oauth_error.clear();
                        self.status_text = if zh {
                            format!("已撤销 {account_name} 的本机 OAuth，并清理所有配置引用")
                        } else {
                            format!(
                                "Revoked local OAuth for {account_name} and removed it from all profiles"
                            )
                        };
                    } else {
                        self.oauth_error = cleanup_errors.join("；");
                        self.status_text = if zh {
                            format!("{account_name} 的本机 OAuth 已删除，但部分配置引用清理失败")
                        } else {
                            format!(
                                "Local OAuth for {account_name} was deleted, but some profile references could not be cleaned"
                            )
                        };
                        self.log(self.oauth_error.clone());
                    }
                    self.refresh_oauth_accounts();
                }
                AppEvent::OAuthAccountRevokeError(error) => {
                    self.oauth_revoking = false;
                    self.oauth_revoke_target = None;
                    let detail = runtime_logs::summarize_error_for_display(&error);
                    self.oauth_error.clone_from(&detail);
                    self.status_text = if zh {
                        format!("撤销 OAuth 失败：{detail}")
                    } else {
                        format!("Could not revoke OAuth: {detail}")
                    };
                    self.log(self.status_text.clone());
                }
                AppEvent::UpdateResult(info) => {
                    self.update_checking = false;
                    self.update_downloading = false;
                    self.update_info = Some(*info);
                    self.update_dialog_open = true;
                }
                AppEvent::UpdateError(error) => {
                    self.update_checking = false;
                    self.update_downloading = false;
                    let detail = runtime_logs::summarize_error_for_display(&error);
                    self.update_info = Some(GitHubUpdateInfo {
                        status: "error".into(),
                        current_version: APP_VERSION.into(),
                        release_url: OFFICIAL_GITHUB_URL.into(),
                        message: detail.clone(),
                        ..Default::default()
                    });
                    self.update_dialog_open = true;
                    self.log(format!(
                        "{}: {}",
                        if zh {
                            "更新检查失败"
                        } else {
                            "Update check failed"
                        },
                        detail
                    ));
                }
                AppEvent::RouterHealthProbeFinished(result) => {
                    self.health_probe_running = false;
                    if !self.router_mode_enabled {
                        self.health_probe_due = None;
                        self.health_probe_failures = 0;
                        continue;
                    }
                    match result {
                        Ok(()) => {
                            self.health_probe_failures = 0;
                            self.health_probe_due =
                                Some(std::time::Instant::now() + HEALTHY_PROBE_INTERVAL);
                        }
                        Err(error) => {
                            let detail = runtime_logs::summarize_error_for_display(&error);
                            if !router_health_failure_recoverable(&error) {
                                self.health_probe_failures = 0;
                                self.health_probe_due =
                                    Some(std::time::Instant::now() + RECOVERY_RETRY_INTERVAL);
                                self.log(if zh {
                                    format!("转发保护无法探测本机 Router 配置：{detail}")
                                } else {
                                    format!("Forwarding protection could not probe the local Router configuration: {detail}")
                                });
                                continue;
                            }
                            self.health_probe_failures =
                                self.health_probe_failures.saturating_add(1);
                            self.log(if zh {
                                format!(
                                    "本机 Router 健康探测失败（{}/3）：{detail}",
                                    self.health_probe_failures
                                )
                            } else {
                                format!(
                                    "Local Router health probe failed ({}/3): {detail}",
                                    self.health_probe_failures
                                )
                            });
                            if self.health_probe_failures >= 3 {
                                self.start_router_health_recovery(ctx);
                            } else {
                                self.health_probe_due =
                                    Some(std::time::Instant::now() + FAILED_PROBE_RETRY_INTERVAL);
                            }
                        }
                    }
                }
                AppEvent::RouterHealthRecoveryFinished(result) => {
                    self.health_recovery_running = false;
                    self.health_probe_failures = 0;
                    if !self.router_mode_enabled {
                        self.health_probe_due = None;
                        continue;
                    }
                    match result {
                        Ok(()) => {
                            self.health_probe_due =
                                Some(std::time::Instant::now() + HEALTHY_PROBE_INTERVAL);
                            self.status_text = if zh {
                                "已自动恢复本机转发，连接保护继续运行".to_owned()
                            } else {
                                "Local forwarding recovered automatically; connection protection remains active".to_owned()
                            };
                            self.log(self.status_text.clone());
                        }
                        Err(error) => {
                            let detail = runtime_logs::summarize_error_for_display(&error);
                            self.health_probe_due =
                                Some(std::time::Instant::now() + RECOVERY_RETRY_INTERVAL);
                            self.status_text = if zh {
                                format!("自动恢复本机转发未成功，稍后重试：{detail}")
                            } else {
                                format!("Automatic local forwarding recovery did not succeed; retrying later: {detail}")
                            };
                            self.log(self.status_text.clone());
                        }
                    }
                }
                AppEvent::Tray(action) => match action {
                    TrayAction::RestoreWindow => self.restore_from_tray(ctx),
                    TrayAction::HideWindow => self.minimize_to_tray(ctx),
                    TrayAction::OpenConsole => {
                        self.restore_from_tray(ctx);
                        self.page = Page::Dashboard;
                    }
                    TrayAction::ChooseProfile => {
                        self.restore_from_tray(ctx);
                        self.open_profiles();
                    }
                    TrayAction::ApplyCurrent => self.apply_all(),
                    TrayAction::StartForwarding => {
                        self.enable_router_mode();
                    }
                    TrayAction::StopForwarding => {
                        self.disable_router_mode();
                    }
                    TrayAction::Exit => self.request_exit(ctx),
                },
            }
        }
        while let Ok(batch) = self.runtime_log_rx.try_recv() {
            for record in batch.into_records() {
                if self.router_mode_enabled
                    && runtime_logs::signals_router_health_failure(&record)
                    && !self.health_probe_running
                    && !self.health_recovery_running
                {
                    self.health_probe_due = Some(std::time::Instant::now());
                }
                self.log(record);
            }
        }
    }

    fn load_logo_texture(&mut self, ctx: &egui::Context) {
        if self.logo_texture.is_some() {
            return;
        }
        if let Ok((pixels, width, height)) = decode_icon() {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [width as usize, height as usize],
                &pixels,
            );
            self.logo_texture =
                Some(ctx.load_texture("codex-router-logo", image, egui::TextureOptions::LINEAR));
        }
    }

    fn log(&mut self, message: impl AsRef<str>) {
        let message = runtime_logs::redact_for_display(message.as_ref());
        if message.trim().is_empty() {
            return;
        }
        append_bounded_log(&mut self.logs, &message);
        self.log_scroll_to_bottom = self.log_follow_latest;
    }

    fn report_error(&mut self, message: impl Into<String>) {
        self.status_text = localized_error_summary(self.ui_language == "zh", &message.into());
        self.log(self.status_text.clone());
    }

    fn export_logs(&mut self) {
        let timestamp = chrono::Local::now();
        let default_name = format!(
            "codex-router-diagnostics-{}.log",
            timestamp.format("%Y%m%d-%H%M%S")
        );
        let Some(path) = rfd::FileDialog::new()
            .set_title(if self.ui_language == "zh" {
                "下载脱敏运行日志"
            } else {
                "Save redacted runtime log"
            })
            .set_file_name(&default_name)
            .add_filter("Log", &["log", "txt"])
            .save_file()
        else {
            return;
        };

        let content = runtime_logs::redact_for_display(&format!(
            "Codex-Router diagnostics v{APP_VERSION}\nExported: {}\nSecrets and request/response payloads are omitted.\n\n{}",
            timestamp.to_rfc3339(),
            self.logs
        ));
        match std::fs::write(path, content) {
            Ok(()) => {
                self.status_text = if self.ui_language == "zh" {
                    "脱敏运行日志已下载".to_owned()
                } else {
                    "Redacted runtime log saved".to_owned()
                };
            }
            Err(error) => {
                let message = if self.ui_language == "zh" {
                    format!("下载日志失败：{error}")
                } else {
                    format!("Could not save the runtime log: {error}")
                };
                self.report_error(message);
            }
        }
    }

    fn open_oauth_manager(&mut self) {
        if self.page != Page::OAuth {
            self.oauth_return_page = self.page;
        }
        self.page = Page::OAuth;
        self.refresh_oauth_accounts();
    }

    fn refresh_oauth_accounts(&mut self) {
        if self.oauth_loading {
            return;
        }
        self.oauth_loading = true;
        self.oauth_error.clear();
        let root = self.router_root.clone();
        let tx = self.event_tx.clone();
        std::thread::spawn(move || match logic::load_oauth_accounts(&root) {
            Ok(accounts) => {
                tx.send(AppEvent::OAuthAccountsLoaded(accounts)).ok();
            }
            Err(error) => {
                tx.send(AppEvent::OAuthAccountsError(error.to_string()))
                    .ok();
            }
        });
    }

    fn open_usage_monitor(&mut self) {
        if self.page != Page::Monitor {
            self.usage_return_page = self.page;
        }
        self.usage_refresh_due = None;
        let profile_key = self.active_route_profile_key();
        if self.usage_snapshot_profile_key != profile_key {
            self.usage_snapshot = None;
            self.usage_snapshot_profile_key.clear();
            self.load_usage_monitor_cache();
        }
        self.page = Page::Monitor;
        self.refresh_usage_monitor();
    }

    fn active_route_profile_key(&self) -> String {
        if !self.active_profile_id.trim().is_empty() {
            return format!("profile:{}", self.active_profile_id.trim());
        }
        if self.config.deploy.cc_switch_sync {
            let id = self.config.deploy.cc_switch_profile_id.trim();
            if !id.is_empty() {
                return format!("cc-switch:{id}");
            }
            let name = self.config.deploy.cc_switch_profile_name.trim();
            if !name.is_empty() {
                return format!("cc-switch-name:{name}");
            }
        }
        "default".to_owned()
    }

    fn usage_monitor_cache_path(&self) -> PathBuf {
        self.router_root
            .join("data")
            .join("ui")
            .join("usage-monitor-cache.json")
    }

    fn load_usage_monitor_cache(&mut self) {
        let path = self.usage_monitor_cache_path();
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(mut cache) = serde_json::from_str::<UsageMonitorCache>(&text) else {
            return;
        };
        if cache.profile_key != self.active_route_profile_key() {
            return;
        }
        Self::sort_usage_accounts(
            &mut cache.snapshot.subscriptions,
            &self.monitor_subscription_order,
        );
        Self::sort_usage_accounts(&mut cache.snapshot.api_channels, &self.monitor_api_order);
        self.usage_snapshot = Some(cache.snapshot);
        self.usage_snapshot_profile_key = cache.profile_key;
    }

    fn save_usage_monitor_cache(
        &self,
        profile_key: &str,
        snapshot: &UsageSnapshot,
    ) -> anyhow::Result<()> {
        let path = self.usage_monitor_cache_path();
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("监控缓存路径无效"))?;
        std::fs::create_dir_all(parent)?;
        let cache = UsageMonitorCache {
            profile_key: profile_key.to_owned(),
            snapshot: snapshot.clone(),
        };
        config::atomic_write(&path, &serde_json::to_vec_pretty(&cache)?)?;
        Ok(())
    }

    fn schedule_usage_refresh_after_close(&mut self, ctx: &egui::Context) {
        if self.tray_lightweight_mode {
            return;
        }
        let delay = std::time::Duration::from_secs(30 * 60);
        self.usage_refresh_due = Some(std::time::Instant::now() + delay);
        ctx.request_repaint_after(delay);
    }

    fn process_scheduled_usage_refresh(&mut self, ctx: &egui::Context) {
        if self.tray_lightweight_mode {
            self.usage_refresh_due = None;
            return;
        }
        let Some(due) = self.usage_refresh_due else {
            return;
        };
        let now = std::time::Instant::now();
        if now >= due {
            if self.usage_loading {
                ctx.request_repaint_after(std::time::Duration::from_secs(1));
            } else {
                self.usage_refresh_due = None;
                self.refresh_usage_monitor();
            }
        } else {
            ctx.request_repaint_after(due.saturating_duration_since(now));
        }
    }

    fn schedule_usage_refresh(&mut self) {
        if self.tray_lightweight_mode {
            return;
        }
        self.usage_refresh_due = Some(std::time::Instant::now());
    }

    fn process_router_health_protection(&mut self, ctx: &egui::Context) {
        if !self.router_mode_enabled {
            self.health_probe_due = None;
            self.health_probe_failures = 0;
            return;
        }
        if self.applying || self.router_mode_switching {
            self.health_probe_due = Some(std::time::Instant::now() + HEALTHY_PROBE_INTERVAL);
            ctx.request_repaint_after(HEALTHY_PROBE_INTERVAL);
            return;
        }
        if self.health_probe_running || self.health_recovery_running {
            return;
        }

        let now = std::time::Instant::now();
        let due = self
            .health_probe_due
            .get_or_insert_with(|| now + HEALTHY_PROBE_INTERVAL);
        if now < *due {
            ctx.request_repaint_after(due.saturating_duration_since(now));
            return;
        }

        self.health_probe_due = None;
        self.health_probe_running = true;
        let base_uri = self.config.deploy.sub2api_host.clone();
        let tx = self.event_tx.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let result = router_deep_health(&base_uri, HEALTH_PROBE_TIMEOUT);
            tx.send(AppEvent::RouterHealthProbeFinished(result)).ok();
            repaint.request_repaint();
        });
    }

    fn start_router_health_recovery(&mut self, ctx: &egui::Context) {
        if self.health_recovery_running || !self.router_mode_enabled {
            return;
        }
        self.health_probe_due = None;
        self.health_recovery_running = true;
        self.health_recovery_cancel.store(false, Ordering::Relaxed);
        self.status_text = if self.ui_language == "zh" {
            "连续 3 次健康探测失败，正在无窗口恢复本机转发…".to_owned()
        } else {
            "Three health probes failed; recovering local forwarding without a console window…"
                .to_owned()
        };
        self.log(self.status_text.clone());

        let root = self.router_root.clone();
        let base_uri = self.config.deploy.sub2api_host.clone();
        let tx = self.event_tx.clone();
        let repaint = ctx.clone();
        let cancel = self.health_recovery_cancel.clone();
        std::thread::spawn(move || {
            let repaired = run_hidden_powershell(
                &root,
                "Start-Router.ps1",
                &["-RepairUnhealthy"],
                std::time::Duration::from_secs(240),
                &cancel,
            );
            let result = if repaired {
                router_deep_health(&base_uri, HEALTH_PROBE_TIMEOUT)
            } else {
                Err("The verified recovery process failed or timed out".to_owned())
            };
            tx.send(AppEvent::RouterHealthRecoveryFinished(result)).ok();
            repaint.request_repaint();
        });
    }

    fn process_scheduled_oauth_recovery(&mut self, ctx: &egui::Context) {
        if self.tray_lightweight_mode {
            self.oauth_recovery_due = None;
            return;
        }
        if !self.router_mode_enabled
            || !self
                .config
                .models
                .iter()
                .any(|model| model.source == "oauth")
        {
            self.oauth_recovery_due = None;
            return;
        }
        let due = self.oauth_recovery_due.get_or_insert_with(|| {
            std::time::Instant::now() + std::time::Duration::from_secs(60 * 60)
        });
        let now = std::time::Instant::now();
        if now < *due {
            ctx.request_repaint_after(due.saturating_duration_since(now));
            return;
        }
        if self.oauth_recovery_running {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
            return;
        }
        self.oauth_recovery_due = None;
        self.oauth_recovery_running = true;
        let script = self
            .router_root
            .join("scripts")
            .join("Invoke-OAuthRecovery.ps1");
        let cwd = self.router_root.clone();
        let tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let result = std::process::Command::new("powershell.exe")
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ])
                .arg(script)
                .current_dir(cwd)
                .creation_flags(0x08000000)
                .output();
            match result {
                Ok(output) if output.status.success() => {
                    tx.send(AppEvent::OAuthRecoveryFinished).ok();
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let details = if stderr.trim().is_empty() {
                        stdout.trim()
                    } else {
                        stderr.trim()
                    };
                    tx.send(AppEvent::OAuthRecoveryError(if details.is_empty() {
                        format!("process_exit={}", output.status.code().unwrap_or(-1))
                    } else {
                        runtime_logs::summarize_error_for_display(details)
                    }))
                    .ok();
                }
                Err(error) => {
                    tx.send(AppEvent::OAuthRecoveryError(
                        runtime_logs::summarize_error_for_display(&error.to_string()),
                    ))
                    .ok();
                }
            }
        });
    }

    fn sort_usage_accounts(accounts: &mut [UsageAccount], order: &[i64]) {
        accounts.sort_by_key(|account| {
            order
                .iter()
                .position(|account_id| *account_id == account.id)
                .unwrap_or(usize::MAX)
        });
    }

    fn refresh_usage_monitor(&mut self) {
        if self.usage_loading {
            return;
        }
        self.usage_loading = true;
        self.usage_error.clear();
        let root = self.router_root.clone();
        let profile_name = self.active_route_config_name(self.ui_language == "zh");
        let profile_key = self.active_route_profile_key();
        let config = self.config.clone();
        let tx = self.event_tx.clone();
        std::thread::spawn(move || {
            match logic::load_usage_snapshot(&root, &profile_name, &config) {
                Ok(snapshot) => {
                    tx.send(AppEvent::UsageLoaded {
                        profile_key,
                        snapshot: Box::new(snapshot),
                    })
                    .ok();
                }
                Err(error) => {
                    tx.send(AppEvent::UsageError(error.to_string())).ok();
                }
            }
        });
    }

    fn import_grok_sso(&mut self) {
        if self.grok_sso_importing || self.grok_sso_draft.trim().is_empty() {
            return;
        }
        self.grok_sso_importing = true;
        self.grok_sso_error.clear();
        let root = self.router_root.clone();
        let authorization = self.grok_sso_draft.clone();
        let tx = self.event_tx.clone();
        std::thread::spawn(
            move || match logic::import_grok_sso(&root, &authorization) {
                Ok(_) => {
                    tx.send(AppEvent::GrokSsoImported).ok();
                }
                Err(error) => {
                    tx.send(AppEvent::GrokSsoImportError(error.to_string()))
                        .ok();
                }
            },
        );
    }

    fn revoke_oauth_account(&mut self, account: OAuthAccountSummary) {
        if self.oauth_revoking {
            return;
        }
        self.oauth_revoking = true;
        self.oauth_error.clear();
        let root = self.router_root.clone();
        let tx = self.event_tx.clone();
        std::thread::spawn(
            move || match logic::revoke_oauth_account(&root, account.id) {
                Ok(()) => {
                    tx.send(AppEvent::OAuthAccountRevoked {
                        account_id: account.id,
                        account_name: account.name,
                    })
                    .ok();
                }
                Err(error) => {
                    tx.send(AppEvent::OAuthAccountRevokeError(error.to_string()))
                        .ok();
                }
            },
        );
    }

    fn enable_router_mode(&mut self) {
        if self.router_mode_switching || self.applying {
            return;
        }
        if self.router_mode_enabled && logic::codex_router_mode_active(&self.config) {
            self.status_text = if self.ui_language == "zh" {
                "当前已在使用 Codex-Router 路由"
            } else {
                "Codex-Router routing is already active"
            }
            .to_owned();
            return;
        }
        self.health_recovery_cancel.store(true, Ordering::Relaxed);
        self.router_mode_switching = true;
        self.log(if self.ui_language == "zh" {
            "正在启用 Router 路由并应用当前配置…"
        } else {
            "Enabling Router mode and applying the current configuration…"
        });
        if !self.apply_all_with_backup(true, None) {
            self.router_mode_switching = false;
        }
    }

    fn disable_router_mode(&mut self) {
        if self.router_mode_switching || self.applying {
            return;
        }
        if !self.router_mode_enabled && !logic::codex_router_mode_active(&self.config) {
            self.status_text = if self.ui_language == "zh" {
                "当前已在使用 Codex 官方路由"
            } else {
                "The official Codex route is already active"
            }
            .to_owned();
            return;
        }
        self.health_recovery_cancel.store(true, Ordering::Relaxed);
        self.router_mode_switching = true;
        let root = self.router_root.clone();
        let config = self.config.clone();
        let share_codex_state = self.share_codex_state;
        let tx = self.event_tx.clone();
        self.status_text = if self.ui_language == "zh" {
            "正在恢复 Codex 官方配置，然后停止本地转发…"
        } else {
            "Restoring the official Codex configuration, then stopping local forwarding…"
        }
        .to_owned();
        self.log(self.status_text.clone());
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<profiles::RestoreOutcome> {
                let _config_lock =
                    profiles::acquire_config_apply_lock(&root, std::time::Duration::from_secs(10))?;
                profiles::capture_restore_point(
                    &root,
                    &config,
                    "关闭 Router 路由并恢复 Codex 官方配置之前",
                )?;
                let outcome = profiles::restore_original_codex(&root, &config, share_codex_state)?;
                logic::run_stop_router_script(&root)?;
                Ok(outcome)
            })();
            match result {
                Ok(outcome) => {
                    tx.send(AppEvent::RouterModeDisabled(outcome)).ok();
                }
                Err(error) => {
                    tx.send(AppEvent::RouterModeSwitchError(error.to_string()))
                        .ok();
                }
            }
        });
    }

    fn apply_all(&mut self) {
        let active_profile_id =
            (!self.active_profile_id.trim().is_empty()).then(|| self.active_profile_id.clone());
        if self.apply_all_with_backup(true, None) && self.pending_profile_activation.is_none() {
            // A normal save made while an isolated profile is active must also
            // refresh that profile's RouterConfig snapshot. This keeps its
            // OAuth account bindings, imported models, and fallback policy
            // independent when the user switches away and back later.
            self.pending_profile_activation = active_profile_id;
        }
    }

    fn apply_all_with_backup(
        &mut self,
        capture_before_apply: bool,
        config_lock: Option<profiles::ConfigApplyLock>,
    ) -> bool {
        if self.applying {
            return false;
        }
        let zh = self.ui_language == "zh";
        if !self.config.accept_compliance
            || self.config.accepted_terms_version != CURRENT_TERMS_VERSION
        {
            let message = if zh {
                "请先完整阅读并同意当前版本的 Codex-Router 使用与分发承诺"
            } else {
                "Read and accept the current Codex-Router terms before deployment"
            };
            self.report_error(message);
            return false;
        }
        if self.config.models.is_empty() {
            let message = if zh {
                "请至少添加一个模型"
            } else {
                "Add at least one model"
            };
            self.report_error(message);
            return false;
        }
        if self.config.deploy.cc_switch_sync {
            if self.config.deploy.cc_switch_db.trim().is_empty() {
                if let Some(path) = logic::detect_cc_switch_db() {
                    self.config.deploy.cc_switch_db = path.display().to_string();
                }
            }
            logic::ensure_cc_switch_profile_id(&mut self.config);
            if let Err(error) = logic::validate_cc_switch_target(&self.config) {
                self.report_error(error.to_string());
                return false;
            }
        }
        logic::normalize_default_model(&mut self.config);
        self.applying = true;
        self.configured = false;
        self.status_text = if zh {
            "正在安全保存凭据并配置 Sub2API..."
        } else {
            "Saving credentials securely and configuring Sub2API..."
        }
        .into();
        let mut cfg = self.config.clone();
        let root = self.router_root.clone();
        let share_codex_state = self.share_codex_state;
        let tx = self.event_tx.clone();
        let credential_log = if zh {
            "API Key 已安全保存到 Windows 凭据管理器"
        } else {
            "API keys were stored securely in Windows Credential Manager"
        }
        .to_owned();
        let files_log = if zh {
            "无密钥配置和模型目录已写入"
        } else {
            "Secret-free configuration and model catalog were written"
        }
        .to_owned();
        let cc_switch_log = if zh {
            format!(
                "CC Switch 独立配置“{}”已同步（原有配置未覆盖，数据库已备份）",
                self.config.deploy.cc_switch_profile_name.trim()
            )
        } else {
            format!(
                "The isolated CC Switch profile \"{}\" was synced without overwriting existing profiles (database backed up)",
                self.config.deploy.cc_switch_profile_name.trim()
            )
        };
        for model in &mut self.config.models {
            model.api_key.clear();
        }
        self.config.proxy.password.clear();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                let _config_lock = match config_lock {
                    Some(lock) => lock,
                    None => profiles::acquire_config_apply_lock(
                        &root,
                        std::time::Duration::from_secs(10),
                    )?,
                };
                profiles::ensure_original_codex_snapshot(&root, &cfg)?;
                if capture_before_apply {
                    profiles::capture_restore_point(&root, &cfg, "应用 Router 配置之前")?;
                }
                logic::store_credentials(&mut cfg, &root)?;
                tx.send(AppEvent::Log(credential_log)).ok();
                logic::write_all_files(&cfg, &root)?;
                tx.send(AppEvent::Log(files_log)).ok();
                logic::run_apply_script(&root, |line| {
                    tx.send(AppEvent::Log(localized_deployment_line(zh, line)))
                        .ok();
                })?;
                if cfg.deploy.cc_switch_sync {
                    logic::sync_cc_switch(&cfg, share_codex_state)?;
                    tx.send(AppEvent::Log(cc_switch_log)).ok();
                }
                Ok(())
            })();
            match result {
                Ok(()) => {
                    tx.send(AppEvent::Complete).ok();
                }
                Err(error) => {
                    tx.send(AppEvent::Error(error.to_string())).ok();
                }
            }
        });
        true
    }

    fn run_script_hidden(&self, relative: &str) {
        let script = self.router_root.join("scripts").join(relative);
        let cwd = self.router_root.clone();
        std::thread::spawn(move || {
            let _ = std::process::Command::new("powershell.exe")
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ])
                .arg(script)
                .current_dir(cwd)
                .creation_flags(0x08000000)
                .output();
        });
    }

    fn check_for_updates(&mut self) {
        if self.update_checking || self.update_downloading {
            return;
        }
        self.update_checking = true;
        self.update_dialog_open = false;
        let script = self.router_root.join("scripts").join("GitHub-Update.ps1");
        let cwd = self.router_root.clone();
        let tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let result = std::process::Command::new("powershell.exe")
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ])
                .arg(script)
                .args(["-Action", "Check", "-CurrentVersion", APP_VERSION])
                .current_dir(cwd)
                .creation_flags(0x08000000)
                .output()
                .map_err(anyhow::Error::from)
                .and_then(parse_update_script_output);
            match result {
                Ok(info) => tx.send(AppEvent::UpdateResult(Box::new(info))).ok(),
                Err(error) => tx.send(AppEvent::UpdateError(error.to_string())).ok(),
            };
        });
    }

    fn download_update(&mut self, info: &GitHubUpdateInfo) {
        if self.update_downloading || info.download_url.is_empty() || info.asset_name.is_empty() {
            return;
        }
        self.update_downloading = true;
        let script = self.router_root.join("scripts").join("GitHub-Update.ps1");
        let cwd = self.router_root.clone();
        let tx = self.event_tx.clone();
        let download_url = info.download_url.clone();
        let asset_name = info.asset_name.clone();
        let expected_size = info.asset_size.to_string();
        std::thread::spawn(move || {
            let result = std::process::Command::new("powershell.exe")
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ])
                .arg(script)
                .args(["-Action", "Download", "-DownloadUrl"])
                .arg(download_url)
                .args(["-FileName"])
                .arg(asset_name)
                .args(["-ExpectedSize"])
                .arg(expected_size)
                .current_dir(cwd)
                .creation_flags(0x08000000)
                .output()
                .map_err(anyhow::Error::from)
                .and_then(parse_update_script_output);
            match result {
                Ok(info) => tx.send(AppEvent::UpdateResult(Box::new(info))).ok(),
                Err(error) => tx.send(AppEvent::UpdateError(error.to_string())).ok(),
            };
        });
    }

    fn open_update_url(&self, requested_url: &str) {
        let url = if requested_url.starts_with(OFFICIAL_GITHUB_URL) {
            requested_url
        } else {
            OFFICIAL_GITHUB_URL
        };
        let _ = std::process::Command::new("explorer.exe").arg(url).spawn();
    }

    fn open_download_location(&self, requested_path: &str) {
        let path = PathBuf::from(requested_path);
        if requested_path.is_empty() || !path.starts_with(self.router_root.join("updates")) {
            return;
        }
        let _ = std::process::Command::new("explorer.exe")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }

    fn open_sub2api_accounts(&self) {
        let url = format!("{}/admin/accounts", self.local_sub2api_base_url());
        let _ = std::process::Command::new("explorer.exe")
            .arg(url)
            .creation_flags(0x08000000)
            .spawn();
    }

    fn local_sub2api_base_url(&self) -> String {
        let fallback = "http://127.0.0.1:18080";
        let candidate = self.config.deploy.sub2api_host.trim().trim_end_matches('/');
        let Ok(mut url) = url::Url::parse(candidate) else {
            return fallback.to_owned();
        };
        if url.scheme() != "http"
            || !matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port_or_known_default().is_none()
        {
            return fallback.to_owned();
        }
        url.set_path("");
        url.set_query(None);
        url.set_fragment(None);
        url.as_str().trim_end_matches('/').to_owned()
    }

    fn start_provider_oauth(&mut self, provider: &str) {
        if self.provider_oauth_running {
            self.status_text = if self.ui_language == "zh" {
                "已有 OAuth 登录正在进行，请先在浏览器中完成或等待其结束".to_owned()
            } else {
                "An OAuth login is already in progress. Complete it in the browser or wait for it to finish"
                    .to_owned()
            };
            return;
        }
        if !matches!(
            provider,
            "openai" | "anthropic" | "gemini" | "antigravity" | "grok"
        ) {
            self.report_error("不支持的 OAuth 平台".to_owned());
            return;
        }
        if !self.config.accept_compliance
            || self.config.accepted_terms_version != CURRENT_TERMS_VERSION
        {
            self.pending_oauth_provider = Some(provider.to_owned());
            self.terms_open = true;
            self.terms_scroll_complete = false;
            self.status_text = if self.ui_language == "zh" {
                "首次 OAuth 前请先阅读并同意本地部署与使用条例".to_owned()
            } else {
                "Read and accept the local deployment and use terms before the first OAuth login"
                    .to_owned()
            };
            return;
        }
        let script = self
            .router_root
            .join("scripts")
            .join("Start-ProviderOAuth.ps1");
        let cwd = self.router_root.clone();
        let provider = provider.to_owned();
        let interactive_console = matches!(provider.as_str(), "anthropic" | "gemini");
        let tx = self.event_tx.clone();
        self.provider_oauth_running = true;
        self.status_text = if self.ui_language == "zh" {
            format!("正在打开 {provider} 官方授权页；请按新窗口提示完成登录")
        } else {
            format!("Opening the official {provider} authorization page. Follow the new window")
        };
        std::thread::spawn(move || {
            let mut command = std::process::Command::new("powershell.exe");
            command.args(["-NoLogo", "-NoProfile"]);
            if !interactive_console {
                command.arg("-NonInteractive");
            }
            command
                .args(["-ExecutionPolicy", "Bypass", "-File"])
                .arg(script)
                .args(["-Provider", &provider])
                .arg("-ComplianceAccepted")
                .current_dir(cwd);
            if interactive_console {
                command.creation_flags(0x00000010);
            } else {
                command
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .creation_flags(CREATE_NO_WINDOW);
            }
            match command.status() {
                Ok(status) if status.success() => {
                    tx.send(AppEvent::ProviderOAuthFinished).ok();
                }
                Ok(status) => {
                    tx.send(AppEvent::ProviderOAuthError(format!(
                        "{provider} OAuth process exited with {status}"
                    )))
                    .ok();
                }
                Err(error) => {
                    tx.send(AppEvent::ProviderOAuthError(format!(
                        "could not start {provider} OAuth: {error}"
                    )))
                    .ok();
                }
            }
        });
    }
}

impl Drop for CodexRouterApp {
    fn drop(&mut self) {
        self.runtime_log_stop.store(true, Ordering::Relaxed);
        self.health_recovery_cancel.store(true, Ordering::Relaxed);
    }
}

#[cfg(any())]
impl eframe::App for CodexRouterApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.load_logo_texture(ctx);
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                AppEvent::Log(message) => self.log(message),
                AppEvent::Complete => {
                    self.applying = false;
                    self.configured = true;
                    self.status_text = "配置完成：模型渠道、Codex 和所选集成均已生效".into();
                    self.log("配置完成");
                }
                AppEvent::Error(error) => {
                    self.applying = false;
                    self.status_text = format!("配置失败：{error}");
                    self.log(format!("错误: {error}"));
                }
            }
        }
        egui::CentralPanel::default().show(ctx, |ui| match self.page {
            Page::Welcome => self.show_welcome(ui),
            Page::Project => self.show_project(ui),
            Page::Auth => self.show_auth(ui),
            Page::Model => self.show_model(ui),
            Page::Proxy => self.show_proxy(ui),
            Page::Finish => self.show_finish(ui),
            Page::Dashboard => self.show_dashboard(ui),
        });
        if self.applying {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }
}

#[cfg(any())]
impl CodexRouterApp {
    fn header(&self, ui: &mut egui::Ui, title: &str) {
        ui.horizontal(|ui| {
            if let Some(texture) = &self.logo_texture {
                ui.image((texture.id(), egui::vec2(56.0, 56.0)));
            }
            ui.heading(title);
        });
        ui.separator();
        ui.add_space(8.0);
    }

    fn show_welcome(&mut self, ui: &mut egui::Ui) {
        self.header(ui, "欢迎使用 Codex-Router");
        ui.label("本向导会配置单用户、多模型、多 API 渠道与自动兜底。所有操作都在本程序中完成。");
        ui.add_space(12.0);
        ui.label("第三方 API Key 和代理密码只进入当前 Windows 用户的凭据管理器，不会写入项目 JSON、日志或 EXE。");
        ui.label(
            "Sub2API、PostgreSQL 与 Redis 随便携包提供，无需单独安装 Python、Node.js 或 Rust。",
        );
        ui.add_space(28.0);
        if ui.button("开始配置").clicked() {
            self.page = Page::Project;
        }
    }

    fn show_project(&mut self, ui: &mut egui::Ui) {
        self.header(ui, "1 / 5  确认项目目录");
        ui.label("通常无需修改：把 EXE 放在 Codex-Router 根目录即可自动识别。");
        ui.horizontal(|ui| {
            ui.label("项目目录:");
            let mut value = self.router_root.to_string_lossy().to_string();
            if ui.text_edit_singleline(&mut value).changed() {
                self.router_root = PathBuf::from(value);
            }
            if ui.button("浏览...").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.router_root = path;
                }
            }
        });
        let valid = self
            .router_root
            .join("scripts")
            .join("Start-Router.ps1")
            .exists()
            && self.router_root.join("app").join("sub2api.exe").exists();
        if valid {
            ui.colored_label(egui::Color32::from_rgb(22, 163, 74), "已识别完整运行环境");
        } else {
            ui.colored_label(
                egui::Color32::from_rgb(220, 38, 38),
                "目录中缺少 scripts/Start-Router.ps1 或 app/sub2api.exe",
            );
        }
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            if ui.button("上一步").clicked() {
                self.page = Page::Welcome;
            }
            if ui.add_enabled(valid, egui::Button::new("下一步")).clicked() {
                self.page = Page::Auth;
            }
        });
    }

    fn show_auth(&mut self, ui: &mut egui::Ui) {
        self.header(ui, "2 / 5  选择上游登录方式");
        ui.label("Codex 始终通过本机路由访问；这里决定是否额外接入 ChatGPT OAuth 渠道。");
        ui.radio_value(
            &mut self.config.auth_mode,
            "chatgpt_oauth".into(),
            "接入 ChatGPT 账号 OAuth，并允许同名第三方模型兜底",
        );
        ui.radio_value(
            &mut self.config.auth_mode,
            "local_api_key".into(),
            "只使用下面配置的第三方 API 渠道",
        );
        ui.checkbox(
            &mut self.config.oauth_fallback.enabled,
            "官方 OAuth 不可用时自动回退到第三方同名模型",
        );
        if self.config.oauth_fallback.enabled {
            ui.horizontal(|ui| {
                ui.label("OAuth 优先级");
                ui.add(
                    egui::DragValue::new(&mut self.config.oauth_fallback.official_priority)
                        .range(1..=999),
                );
                ui.label("第三方兜底优先级");
                ui.add(
                    egui::DragValue::new(&mut self.config.oauth_fallback.fallback_priority)
                        .range(1..=999),
                );
            });
        }
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            if ui.button("上一步").clicked() {
                self.page = Page::Project;
            }
            if ui.button("下一步").clicked() {
                self.temp_model = ModelConfig::default();
                self.editing_model = None;
                self.model_from_wizard = true;
                self.page = Page::Model;
            }
        });
    }

    fn show_model(&mut self, ui: &mut egui::Ui) {
        self.header(
            ui,
            if self.model_from_wizard {
                "3 / 5  配置第一个模型"
            } else {
                "模型渠道设置"
            },
        );
        egui::Grid::new("model-form")
            .num_columns(2)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                ui.label("模型名称 *");
                ui.text_edit_singleline(&mut self.temp_model.model);
                ui.end_row();
                ui.label("显示别名");
                ui.text_edit_singleline(&mut self.temp_model.alias);
                ui.end_row();
                ui.label("Base URL *");
                ui.text_edit_singleline(&mut self.temp_model.base_url);
                ui.end_row();
                ui.label("API Key");
                ui.add(
                    egui::TextEdit::singleline(&mut self.temp_model.api_key)
                        .password(true)
                        .hint_text(if self.temp_model.credential_name.is_empty() {
                            "输入 API Key"
                        } else {
                            "留空则保留已安全保存的 Key"
                        }),
                );
                ui.end_row();
                ui.label("优先级");
                ui.add(egui::DragValue::new(&mut self.temp_model.priority).range(1..=999));
                ui.end_row();
                ui.label("权重");
                ui.add(egui::DragValue::new(&mut self.temp_model.weight).range(1..=100));
                ui.end_row();
                ui.label("多模态");
                egui::ComboBox::from_id_salt("multimodal")
                    .selected_text(match self.temp_model.multimodal.as_str() {
                        "true" => "手动：支持",
                        "false" => "手动：不支持",
                        _ => "自动判断",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.temp_model.multimodal,
                            "auto".into(),
                            "自动判断",
                        );
                        ui.selectable_value(
                            &mut self.temp_model.multimodal,
                            "true".into(),
                            "支持图片/多模态",
                        );
                        ui.selectable_value(
                            &mut self.temp_model.multimodal,
                            "false".into(),
                            "不支持图片/多模态",
                        );
                    });
                ui.end_row();
                ui.label("其它参数 JSON");
                ui.text_edit_multiline(&mut self.temp_model.extra);
                ui.end_row();
            });
        ui.label(format!(
            "当前判定：{}",
            if logic::resolve_multimodal(&self.temp_model) {
                "支持多模态（图片将透传）"
            } else {
                "纯文本模型"
            }
        ));
        let json_valid = serde_json::from_str::<serde_json::Value>(&self.temp_model.extra)
            .map(|v| v.is_object())
            .unwrap_or(false);
        let valid = !self.temp_model.model.trim().is_empty()
            && !self.temp_model.base_url.trim().is_empty()
            && json_valid
            && (!self.temp_model.api_key.trim().is_empty()
                || !self.temp_model.credential_name.is_empty());
        if !json_valid {
            ui.colored_label(egui::Color32::RED, "其它参数必须是 JSON 对象，例如 {}");
        }
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if ui
                .button(if self.model_from_wizard {
                    "上一步"
                } else {
                    "取消"
                })
                .clicked()
            {
                self.page = if self.model_from_wizard {
                    Page::Auth
                } else {
                    Page::Dashboard
                };
            }
            if ui
                .add_enabled(
                    valid,
                    egui::Button::new(if self.model_from_wizard {
                        "下一步"
                    } else {
                        "保存模型"
                    }),
                )
                .clicked()
            {
                if self.model_from_wizard {
                    self.config.models = vec![self.temp_model.clone()];
                    self.proxy_from_wizard = true;
                    self.page = Page::Proxy;
                } else {
                    match self.editing_model {
                        Some(index) => self.config.models[index] = self.temp_model.clone(),
                        None => self.config.models.push(self.temp_model.clone()),
                    }
                    self.page = Page::Dashboard;
                }
            }
        });
    }

    fn show_proxy(&mut self, ui: &mut egui::Ui) {
        self.header(
            ui,
            if self.proxy_from_wizard {
                "4 / 5  网络代理与可选 CC Switch"
            } else {
                "网络代理与可选 CC Switch"
            },
        );
        ui.checkbox(
            &mut self.config.proxy.enabled,
            "让 Sub2API 的上游请求使用代理（兼容 Clash / V2Ray / SSR）",
        );
        if self.config.proxy.enabled {
            egui::Grid::new("proxy-form")
                .num_columns(2)
                .spacing([16.0, 10.0])
                .show(ui, |ui| {
                    ui.label("协议");
                    egui::ComboBox::from_id_salt("proxy-type")
                        .selected_text(&self.config.proxy.proxy_type)
                        .show_ui(ui, |ui| {
                            for value in ["http", "https", "socks5", "socks5h"] {
                                ui.selectable_value(
                                    &mut self.config.proxy.proxy_type,
                                    value.into(),
                                    value,
                                );
                            }
                        });
                    ui.end_row();
                    ui.label("地址");
                    ui.text_edit_singleline(&mut self.config.proxy.host);
                    ui.end_row();
                    ui.label("端口");
                    ui.text_edit_singleline(&mut self.config.proxy.port);
                    ui.end_row();
                    ui.label("用户名（可选）");
                    ui.text_edit_singleline(&mut self.config.proxy.username);
                    ui.end_row();
                    ui.label("密码（可选）");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.config.proxy.password)
                            .password(true)
                            .hint_text("留空则保留已保存密码"),
                    );
                    ui.end_row();
                });
        }
        ui.separator();
        ui.checkbox(
            &mut self.config.deploy.cc_switch_sync,
            "同步为 CC Switch 独立隔离配置（可选；不了解可跳过）",
        );
        if self.config.deploy.cc_switch_sync {
            ui.horizontal(|ui| {
                ui.label("数据库:");
                ui.text_edit_singleline(&mut self.config.deploy.cc_switch_db);
                if ui.button("选择...").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("SQLite", &["db"])
                        .pick_file()
                    {
                        self.config.deploy.cc_switch_db = path.display().to_string();
                    }
                }
            });
            ui.label("CC Switch 只是额外的配置隔离工具，不是必需组件；不了解可直接关闭。同步前会自动备份数据库，关闭时 Codex-Router 仍可独立使用。");
        }
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            if ui
                .button(if self.proxy_from_wizard {
                    "上一步"
                } else {
                    "取消"
                })
                .clicked()
            {
                self.page = if self.proxy_from_wizard {
                    Page::Model
                } else {
                    Page::Dashboard
                };
            }
            if ui
                .button(if self.proxy_from_wizard {
                    "下一步"
                } else {
                    "保存"
                })
                .clicked()
            {
                self.page = if self.proxy_from_wizard {
                    Page::Finish
                } else {
                    Page::Dashboard
                };
            }
        });
    }

    fn show_finish(&mut self, ui: &mut egui::Ui) {
        self.header(ui, "5 / 5  一键完成配置");
        ui.label("将自动初始化本地运行环境、配置真实 Sub2API 渠道并写入 Codex。代理与 CC Switch 都是可选项；不使用 CC Switch 不影响 Codex-Router 独立运行。");
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.config.accept_compliance, "我已阅读、理解并同意 Sub2API 部署与运营合规承诺");
            if ui.link("查看中文承诺原文").clicked() {
                let _ = std::process::Command::new("cmd.exe").args(["/C", "start", "", "https://github.com/Wei-Shaw/sub2api/blob/main/docs/legal/admin-compliance.zh.md"]).spawn();
            }
        });
        if !self.config.accept_compliance {
            ui.colored_label(
                egui::Color32::from_rgb(180, 83, 9),
                "首次使用必须由你本人确认合规承诺；程序不会替你静默接受。 ",
            );
        }
        ui.label(&self.status_text);
        if self.applying {
            ui.spinner();
        } else if ui
            .add_enabled(
                self.config.accept_compliance,
                egui::Button::new("一键完成配置"),
            )
            .clicked()
        {
            self.apply_all();
        }
        ui.add_space(14.0);
        egui::ScrollArea::vertical()
            .max_height(280.0)
            .show(ui, |ui| {
                ui.monospace(&self.logs);
            });
        if self.configured && ui.button("进入控制台").clicked() {
            self.page = Page::Dashboard;
        }
    }

    fn show_dashboard(&mut self, ui: &mut egui::Ui) {
        self.header(ui, "Codex-Router 控制台");
        ui.label(format!("项目目录: {}", self.router_root.display()));
        ui.horizontal(|ui| {
            if ui.button("启动路由").clicked() {
                self.run_script_new_console("Start-Router.ps1");
                self.log("正在启动路由...");
            }
            if ui.button("停止路由").clicked() {
                self.stop_router();
                self.log("正在停止路由...");
            }
            if ui.button("打开 Sub2API 管理页").clicked() {
                let _ = std::process::Command::new("explorer.exe")
                    .arg(self.local_sub2api_base_url())
                    .spawn();
            }
            if self.config.auth_mode == "chatgpt_oauth"
                && ui.button("登录 / 更新 ChatGPT OAuth").clicked()
            {
                self.run_script_new_console("Start-ChatGPTOAuth.ps1");
            }
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.applying, egui::Button::new("保存并应用全部配置"))
                .clicked()
            {
                self.apply_all();
            }
            if ui.button("代理 / CC Switch 设置").clicked() {
                self.proxy_from_wizard = false;
                self.page = Page::Proxy;
            }
            if ui.button("重新运行首次向导").clicked() {
                self.page = Page::Welcome;
            }
        });
        if self.applying {
            ui.spinner();
        }
        if !self.status_text.is_empty() {
            ui.label(&self.status_text);
        }
        ui.separator();
        ui.heading("模型渠道");
        let mut edit = None;
        let mut delete = None;
        for (index, model) in self.config.models.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{}  |  {}  |  优先级 {}  |  {}",
                    model.model,
                    model.base_url,
                    model.priority,
                    if logic::resolve_multimodal(model) {
                        "多模态"
                    } else {
                        "文本"
                    }
                ));
                if ui.button("编辑").clicked() {
                    edit = Some(index);
                }
                if ui.button("删除").clicked() {
                    delete = Some(index);
                }
            });
        }
        if let Some(index) = delete {
            self.config.models.remove(index);
        }
        if let Some(index) = edit {
            self.temp_model = self.config.models[index].clone();
            self.editing_model = Some(index);
            self.model_from_wizard = false;
            self.page = Page::Model;
        }
        if ui.button("+ 添加模型").clicked() {
            self.temp_model = ModelConfig::default();
            self.editing_model = None;
            self.model_from_wizard = false;
            self.page = Page::Model;
        }
        ui.separator();
        ui.heading("运行日志");
        egui::ScrollArea::vertical()
            .max_height(220.0)
            .show(ui, |ui| {
                ui.monospace(&self.logs);
            });
    }
}

fn window_icon() -> egui::IconData {
    let (rgba, width, height) = decode_icon().expect("embedded logo is invalid");
    egui::IconData {
        rgba,
        width,
        height,
    }
}

fn run_hidden_powershell(
    router_root: &std::path::Path,
    script_name: &str,
    arguments: &[&str],
    timeout: std::time::Duration,
    cancel: &AtomicBool,
) -> bool {
    let script = router_root.join("scripts").join(script_name);
    let mut child = match std::process::Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(script)
        .args(arguments)
        .current_dir(router_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };

    let started = std::time::Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(windows)]
fn process_name_from_entry(executable: &[u16]) -> String {
    let end = executable
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(executable.len());
    String::from_utf16_lossy(&executable[..end])
}

#[cfg(windows)]
fn should_take_over_process(current_pid: u32, candidate_pid: u32, executable: &str) -> bool {
    candidate_pid != current_pid && executable.eq_ignore_ascii_case("Codex-Router.exe")
}

#[cfg(windows)]
fn take_over_older_gui_instances() {
    use windows_sys::Win32::Foundation::{
        CloseHandle, INVALID_HANDLE_VALUE, WAIT_ABANDONED, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        CreateMutexW, GetCurrentProcessId, OpenProcess, ReleaseMutex, TerminateProcess,
        WaitForSingleObject, INFINITE, PROCESS_TERMINATE,
    };
    const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;

    let mutex_name: Vec<u16> = "Local\\CodexRouterStartupTakeoverV1"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let mutex = CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr());
        if mutex.is_null() {
            return;
        }
        let wait = WaitForSingleObject(mutex, INFINITE);
        if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
            CloseHandle(mutex);
            return;
        }

        let current_pid = GetCurrentProcessId();
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot != INVALID_HANDLE_VALUE {
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut has_entry = Process32FirstW(snapshot, &mut entry) != 0;
            while has_entry {
                let executable = process_name_from_entry(&entry.szExeFile);
                if should_take_over_process(current_pid, entry.th32ProcessID, &executable) {
                    let process = OpenProcess(
                        PROCESS_TERMINATE | SYNCHRONIZE_ACCESS,
                        0,
                        entry.th32ProcessID,
                    );
                    if !process.is_null() {
                        if TerminateProcess(process, 0) != 0 {
                            WaitForSingleObject(process, 5_000);
                        }
                        CloseHandle(process);
                    }
                }
                has_entry = Process32NextW(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
        }
        ReleaseMutex(mutex);
        CloseHandle(mutex);
    }
}

fn main() -> eframe::Result<()> {
    #[cfg(windows)]
    take_over_older_gui_instances();
    let start_in_background =
        std::env::args_os().any(|argument| argument == "--background" || argument == "--watchdog");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([840.0, 680.0])
            .with_visible(!start_in_background)
            .with_icon(window_icon()),
        centered: true,
        renderer: eframe::Renderer::Glow,
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native(
        "Codex-Router",
        options,
        Box::new(move |cc| Ok(Box::new(CodexRouterApp::new(cc, start_in_background)))),
    )
}

#[cfg(test)]
mod main_tests {
    use super::{
        append_bounded_log, localized_deployment_line, localized_error_summary,
        usage_error_for_display, MAX_LOG_BYTES, RETAIN_LOG_BYTES,
    };

    #[cfg(windows)]
    use super::should_take_over_process;

    #[cfg(windows)]
    #[test]
    fn only_an_older_router_gui_is_selected_for_takeover() {
        assert!(should_take_over_process(20, 10, "Codex-Router.exe"));
        assert!(should_take_over_process(20, 10, "codex-router.EXE"));
        assert!(!should_take_over_process(20, 20, "Codex-Router.exe"));
        assert!(!should_take_over_process(20, 10, "sub2api.exe"));
    }

    #[test]
    fn deployment_output_is_allowlisted_before_reaching_the_log() {
        let safe_stage =
            localized_deployment_line(false, "[2/7] secret=must-not-survive".to_owned());
        assert_eq!(safe_stage, "[2/7] Starting PostgreSQL, Redis, and Sub2API…");

        let safe_error =
            localized_deployment_line(false, "upstream failed api_key=must-not-survive".to_owned());
        assert!(safe_error.contains("class="));
        assert!(!safe_error.contains("must-not-survive"));
    }

    #[test]
    fn install_root_conflicts_are_actionable_in_both_languages() {
        let marker = "ROUTER_INSTALL_ROOT_CONFLICT: Sub2API port 18080 is owned elsewhere";
        assert!(localized_error_summary(true, marker).contains("另一份 Codex-Router"));
        assert!(localized_error_summary(false, marker).contains("Another Codex-Router"));
    }

    #[test]
    fn preclassified_usage_errors_are_not_classified_a_second_time() {
        assert_eq!(
            usage_error_for_display("class=connection_refused | status=503"),
            "class=connection_refused | status=503"
        );
        assert_eq!(
            usage_error_for_display("unsafe\napi_key=must-not-survive"),
            "class=unclassified_error"
        );
    }

    #[test]
    fn gui_log_rebuilds_at_256_kib_and_keeps_complete_recent_records() {
        let mut logs = String::with_capacity(2 * 1024 * 1024);
        let original_capacity = logs.capacity();
        let mut index = 0;
        while logs.len() <= MAX_LOG_BYTES {
            logs.push_str(&format!("record-{index:05}|{}|end\n", "界🙂".repeat(12)));
            index += 1;
        }

        append_bounded_log(&mut logs, "record-latest|界🙂|end");
        assert!(logs.len() <= RETAIN_LOG_BYTES);
        assert!(logs.ends_with("record-latest|界🙂|end\n"));
        assert!(logs
            .lines()
            .all(|line| line.starts_with("record-") && line.ends_with("|end")));
        assert!(logs.capacity() < original_capacity);
        assert!(logs.capacity() <= MAX_LOG_BYTES);
    }
}
