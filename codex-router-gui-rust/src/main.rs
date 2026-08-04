#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod logic;
mod profiles;
mod runtime_logs;
mod theme;
mod ui;
mod user_data;

use anyhow::Context;
use config::{CloseBehavior, ModelConfig, RouterConfig, UiPreferences};
use eframe::egui;
use profiles::{IsolationKind, IsolationProfile};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

const APP_ICON_ICO: &[u8] = include_bytes!("../assets/logo.ico");
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const APP_TITLE: &str = concat!("Codex-Router v", env!("CARGO_PKG_VERSION"));
const TRAY_TOOLTIP_ZH: &str = concat!(
    "Codex-Router v",
    env!("CARGO_PKG_VERSION"),
    " - 轻量托盘模式（仅保留转发保护）"
);
const TRAY_TOOLTIP_EN: &str = concat!(
    "Codex-Router v",
    env!("CARGO_PKG_VERSION"),
    " - lightweight tray mode (forwarding protection only)"
);
const CURRENT_CONFIG_VERSION: &str = APP_VERSION;
const CURRENT_TERMS_VERSION: &str = "codex-router-terms-v1.2.1-2026-08-04";
const OFFICIAL_GITHUB_URL: &str = "https://github.com/HernanJiang/Codex-Router";
const MAX_LOG_BYTES: usize = 256 * 1024;
const RETAIN_LOG_BYTES: usize = 192 * 1024;
const CREATE_NO_WINDOW: u32 = 0x08000000;
const CREATE_NEW_CONSOLE: u32 = 0x00000010;
const HEALTHY_PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
const FAILED_PROBE_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
const RECOVERY_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
const HEALTH_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);
const EXIT_CONFIG_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const EXIT_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);
const EXIT_HELPER_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
const EXIT_PROCESS_KILL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const EXIT_HELPER_OUTPUT_LIMIT: usize = 64 * 1024;
const EXIT_TAKEOVER_GRACE: std::time::Duration = std::time::Duration::from_secs(15);
const STARTUP_TAKEOVER_MUTEX_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
// Logical points, not physical pixels. Startup is fitted to the primary
// monitor's work area, so ordinary displays retain the comfortable desktop
// size while 200%-scaled displays receive a usable compact window.
const DEFAULT_WINDOW_LOGICAL_SIZE: [f32; 2] = [1280.0, 820.0];
const COMPACT_WINDOW_LOGICAL_SIZE: [f32; 2] = [900.0, 600.0];
const MIN_WINDOW_LOGICAL_SIZE: [f32; 2] = [800.0, 400.0];
const WINDOWS_1080P_200_WORK_AREA_LOGICAL_SIZE: [f32; 2] = [960.0, 492.0];
const WINDOWS_NON_CLIENT_LOGICAL_ALLOWANCE: [f32; 2] = [16.0, 40.0];
// `ViewportInfo::monitor_size` includes the taskbar area. Reserve both normal
// window chrome and a 48-point taskbar when clamping after a DPI transition.
const WINDOWS_RUNTIME_MONITOR_ALLOWANCE: [f32; 2] = [16.0, 88.0];

fn fit_window_to_work_area(preferred: [f32; 2], work_area: [f32; 2]) -> [f32; 2] {
    let maximum = [
        (work_area[0] - WINDOWS_NON_CLIENT_LOGICAL_ALLOWANCE[0]).max(1.0),
        (work_area[1] - WINDOWS_NON_CLIENT_LOGICAL_ALLOWANCE[1]).max(1.0),
    ];
    [preferred[0].min(maximum[0]), preferred[1].min(maximum[1])]
}

fn fit_window_to_monitor(current: [f32; 2], monitor: [f32; 2]) -> [f32; 2] {
    let maximum = [
        (monitor[0] - WINDOWS_RUNTIME_MONITOR_ALLOWANCE[0]).max(1.0),
        (monitor[1] - WINDOWS_RUNTIME_MONITOR_ALLOWANCE[1]).max(1.0),
    ];
    [current[0].min(maximum[0]), current[1].min(maximum[1])]
}

pub(crate) fn clamp_window_to_current_monitor(ctx: &egui::Context) {
    let viewport = ctx.input(|input| {
        let viewport = input.viewport();
        (
            viewport.monitor_size,
            viewport.inner_rect.map(|rect| rect.size()),
            viewport.maximized == Some(true),
            viewport.fullscreen == Some(true),
            viewport.minimized == Some(true),
        )
    });
    let (Some(monitor), Some(current), false, false, false) = viewport else {
        return;
    };
    if monitor.x <= 0.0 || monitor.y <= 0.0 || current.x <= 0.0 || current.y <= 0.0 {
        return;
    }
    let fitted = fit_window_to_monitor([current.x, current.y], [monitor.x, monitor.y]);
    if fitted[0] + 1.0 < current.x || fitted[1] + 1.0 < current.y {
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
            fitted[0], fitted[1],
        )));
    }
}

#[cfg(windows)]
fn primary_work_area_logical_size() -> Option<[f32; 2]> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETWORKAREA};

    let mut work_area: RECT = unsafe { std::mem::zeroed() };
    let available = unsafe {
        SystemParametersInfoW(SPI_GETWORKAREA, 0, (&mut work_area as *mut RECT).cast(), 0)
    } != 0;
    let dpi = unsafe { GetDpiForSystem() };
    let width = work_area.right.saturating_sub(work_area.left);
    let height = work_area.bottom.saturating_sub(work_area.top);
    if !available || dpi == 0 || width <= 0 || height <= 0 {
        return None;
    }
    let points_per_pixel = 96.0 / dpi as f32;
    Some([
        width as f32 * points_per_pixel,
        height as f32 * points_per_pixel,
    ])
}

fn initial_window_logical_size(compact: bool) -> [f32; 2] {
    let preferred = if compact {
        COMPACT_WINDOW_LOGICAL_SIZE
    } else {
        DEFAULT_WINDOW_LOGICAL_SIZE
    };
    let work_area =
        primary_work_area_logical_size().unwrap_or(WINDOWS_1080P_200_WORK_AREA_LOGICAL_SIZE);
    fit_window_to_work_area(preferred, work_area)
}

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

#[derive(Clone, Debug)]
struct UiAuditOptions {
    scenario: String,
    language: String,
    theme: String,
    compact: bool,
}

impl UiAuditOptions {
    fn from_args() -> Option<Self> {
        let arguments = std::env::args().collect::<Vec<_>>();
        let encoded = std::env::current_exe()
            .ok()
            .and_then(|path| path.file_stem().map(|value| value.to_owned()))
            .and_then(|value| value.to_str().map(str::to_owned))
            .and_then(|name| {
                name.to_ascii_lowercase()
                    .strip_prefix("codex-router-ui-audit__")
                    .map(|suffix| suffix.split("__").map(str::to_owned).collect::<Vec<_>>())
            });
        let scenario = arguments
            .iter()
            .find_map(|argument| {
                argument
                    .strip_prefix("--ui-audit=")
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            })
            .or_else(|| encoded.as_ref().and_then(|items| items.first().cloned()))?;
        let value = |prefix: &str| {
            arguments.iter().find_map(|argument| {
                argument
                    .strip_prefix(prefix)
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(str::to_owned)
            })
        };
        let language = value("--ui-language=")
            .or_else(|| encoded.as_ref().and_then(|items| items.get(1).cloned()))
            .filter(|item| matches!(item.as_str(), "zh" | "en"))
            .unwrap_or_else(|| "zh".to_owned());
        let theme = value("--ui-theme=")
            .or_else(|| encoded.as_ref().and_then(|items| items.get(2).cloned()))
            .filter(|item| matches!(item.as_str(), "sky" | "coffee"))
            .unwrap_or_else(|| "sky".to_owned());
        let compact = arguments.iter().any(|argument| argument == "--ui-compact")
            || encoded
                .as_ref()
                .is_some_and(|items| items.iter().any(|item| item == "compact"));
        Some(Self {
            scenario,
            language,
            theme,
            compact,
        })
    }
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
struct OAuthRecoverySchedule {
    #[serde(default = "default_oauth_recovery_seconds")]
    next_check_seconds: u64,
    #[serde(default)]
    summary: String,
}

fn default_oauth_recovery_seconds() -> u64 {
    60 * 60
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

#[derive(Clone)]
struct ApplyUiRollback {
    config: RouterConfig,
    active_profile_id: String,
    pending_profile_activation: Option<String>,
    configured: bool,
    router_mode_enabled: bool,
}

#[derive(Clone)]
struct ApplyTransactionBackup {
    point: profiles::RestorePoint,
    config: RouterConfig,
}

enum AppEvent {
    Log(String),
    ExitShutdownFinished(Result<(), String>),
    AutostartFinished {
        enabled: bool,
        rollback_to: Option<bool>,
        result: Result<(), String>,
    },
    Complete,
    Error(String),
    Tray(TrayAction),
    OAuthAccountsLoaded(Vec<OAuthAccountSummary>),
    OAuthAccountsError(String),
    ProviderOAuthPrepared {
        provider: String,
        generation: u64,
    },
    ProviderOAuthPrepareError {
        provider: String,
        generation: u64,
        error: String,
    },
    ProviderOAuthFinished,
    ProviderOAuthError(String),
    UsageLoaded {
        profile_key: String,
        generation: u64,
        snapshot: Box<UsageSnapshot>,
    },
    UsageError {
        profile_key: String,
        generation: u64,
        error: String,
    },
    PreviousConfigurationRestored {
        config: Box<RouterConfig>,
        outcome: profiles::RestoreOutcome,
        label: String,
    },
    PreviousConfigurationRestoreError(String),
    RouterModeDisabled(profiles::RestoreOutcome),
    RouterModeSwitchError(String),
    CodexAccountModeChanged(profiles::CodexAccountModeStatus),
    CodexAccountModeSwitchError(String),
    OAuthRecoveryFinished(OAuthRecoverySchedule),
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
    ui_audit_mode: bool,
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
    apply_cancel: Arc<AtomicBool>,
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
    terms_scroll_reset_pending: bool,
    advanced_json_open: bool,
    advanced_json_draft: String,
    reasoning_open: bool,
    reasoning_mode_draft: String,
    reasoning_levels_draft: String,
    reasoning_default_draft: String,
    reasoning_fast_supported_draft: bool,
    reasoning_fast_mode_draft: bool,
    close_behavior: CloseBehavior,
    autostart_switching: bool,
    close_prompt_open: bool,
    apply_success_dialog_open: bool,
    remember_close_choice: bool,
    exit_after_prompt: bool,
    exit_shutdown_in_progress: bool,
    exit_shutdown_error: String,
    local_profile_name_input: String,
    isolation_profiles: Vec<IsolationProfile>,
    active_profile_id: String,
    pending_profile_activation: Option<String>,
    pending_apply_rollback: Option<ApplyUiRollback>,
    oauth_accounts: Vec<OAuthAccountSummary>,
    oauth_loading: bool,
    oauth_error: String,
    oauth_return_page: Page,
    oauth_provider_draft: String,
    provider_oauth_running: bool,
    pending_oauth_provider: Option<String>,
    provider_oauth_preparing: bool,
    provider_oauth_preparing_provider: Option<String>,
    provider_oauth_prepare_generation: u64,
    provider_oauth_prepared_provider: Option<String>,
    provider_oauth_prepare_error: String,
    provider_oauth_prepare_cancel: Arc<AtomicBool>,
    provider_oauth_cancel: Arc<AtomicBool>,
    oauth_revoke_target: Option<OAuthAccountSummary>,
    oauth_revoking: bool,
    oauth_manual_model_target: Option<OAuthAccountSummary>,
    oauth_manual_model_id_draft: String,
    oauth_manual_model_alias_draft: String,
    oauth_fallback_picker_target: Option<OAuthAccountSummary>,
    oauth_fallback_picker_draft: BTreeMap<String, Option<Vec<String>>>,
    usage_snapshot: Option<UsageSnapshot>,
    usage_snapshot_profile_key: String,
    usage_loading: bool,
    usage_request_generation: u64,
    usage_error: String,
    usage_return_page: Page,
    usage_refresh_due: Option<std::time::Instant>,
    monitor_subscription_order: Vec<i64>,
    monitor_api_order: Vec<i64>,
    share_codex_state: bool,
    router_mode_enabled: bool,
    router_mode_switching: bool,
    codex_account_mode_status: profiles::CodexAccountModeStatus,
    codex_account_mode_switching: bool,
    oauth_recovery_due: Option<std::time::Instant>,
    oauth_recovery_running: bool,
    oauth_recovery_cancel: Arc<AtomicBool>,
    grok_sso_dialog_open: bool,
    grok_sso_draft: String,
    grok_sso_importing: bool,
    grok_sso_error: String,
    grok_sso_auto_select_pending: bool,
    channel_preset_dialog_open: bool,
    recommended_platform_dialog_open: bool,
    sub2api_intro_open: bool,
    log_scroll_to_bottom: bool,
    log_follow_latest: bool,
    log_dialog_open: bool,
    runtime_log_stop: Arc<AtomicBool>,
    runtime_log_paused: Arc<AtomicBool>,
    tray_lightweight_mode: bool,
    background_hide_until: Option<std::time::Instant>,
    tray_restore_guard_until: Option<std::time::Instant>,
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

fn drain_bounded_output<R: Read>(mut reader: R) -> std::io::Result<Vec<u8>> {
    let mut retained = Vec::with_capacity(EXIT_HELPER_OUTPUT_LIMIT);
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        if read >= EXIT_HELPER_OUTPUT_LIMIT {
            retained.clear();
            retained.extend_from_slice(&buffer[read - EXIT_HELPER_OUTPUT_LIMIT..read]);
            continue;
        }
        let overflow = retained
            .len()
            .saturating_add(read)
            .saturating_sub(EXIT_HELPER_OUTPUT_LIMIT);
        if overflow > 0 {
            retained.drain(..overflow);
        }
        retained.extend_from_slice(&buffer[..read]);
    }
    Ok(retained)
}

fn finish_output_drain(
    receiver: Receiver<std::io::Result<Vec<u8>>>,
    stream_name: &str,
) -> anyhow::Result<Vec<u8>> {
    receiver
        .recv_timeout(EXIT_HELPER_DRAIN_TIMEOUT)
        .map_err(|error| {
            anyhow::anyhow!(
                "Router shutdown {stream_name} did not close within its output-drain budget: {error}"
            )
        })?
        .with_context(|| format!("could not read Router shutdown {stream_name}"))
}

fn spawn_output_drain<R>(reader: R) -> Receiver<std::io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(drain_bounded_output(reader));
    });
    receiver
}

fn wait_for_child_exit(child: &mut std::process::Child, timeout: std::time::Duration) -> bool {
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Ok(None) | Err(_) => return false,
        }
    }
}

fn terminate_child_process_tree(child: &mut std::process::Child) {
    let taskkill = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .map(|root| root.join("System32").join("taskkill.exe"))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("taskkill.exe"));
    if let Ok(mut killer) = std::process::Command::new(taskkill)
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        if !wait_for_child_exit(&mut killer, EXIT_PROCESS_KILL_TIMEOUT) {
            let _ = killer.kill();
        }
    }
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    // Never turn a bounded shutdown into an unbounded GUI exit. Dropping a
    // still-running Child handle is safe; the tree-kill request above remains
    // the primary cleanup mechanism.
    let _ = wait_for_child_exit(child, EXIT_PROCESS_KILL_TIMEOUT);
}

fn stop_router_for_exit_with_timeout(
    router_root: &Path,
    timeout: std::time::Duration,
) -> anyhow::Result<()> {
    let script = router_root.join("scripts").join("Stop-Router.ps1");
    let mut child = std::process::Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(script)
        .arg("-Force")
        .arg("-AdoptActivePortableOwner")
        .current_dir(router_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .context("could not start the Router shutdown helper")?;

    let stdout = child
        .stdout
        .take()
        .context("Router shutdown helper did not expose stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("Router shutdown helper did not expose stderr")?;
    let stdout_drain = spawn_output_drain(stdout);
    let stderr_drain = spawn_output_drain(stderr);

    let started = std::time::Instant::now();
    let status_result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok(None) => {
                terminate_child_process_tree(&mut child);
                break Err(anyhow::anyhow!(
                    "Router shutdown exceeded its {} second time budget",
                    timeout.as_secs_f32()
                ));
            }
            Err(error) => {
                terminate_child_process_tree(&mut child);
                break Err(error).context("could not monitor the Router shutdown helper");
            }
        }
    };
    let stdout = finish_output_drain(stdout_drain, "output")?;
    let stderr = finish_output_drain(stderr_drain, "error output")?;
    let status = status_result?;
    if status.success() {
        return Ok(());
    }
    let detail = if stderr.is_empty() {
        String::from_utf8_lossy(&stdout)
    } else {
        String::from_utf8_lossy(&stderr)
    };
    let detail = runtime_logs::summarize_error_for_display(detail.trim());
    anyhow::bail!("Router shutdown failed: {detail}")
}

fn stop_router_for_exit(router_root: &Path) -> anyhow::Result<()> {
    stop_router_for_exit_with_timeout(router_root, EXIT_SHUTDOWN_TIMEOUT)
}

struct ExitTransactionMarker {
    path: PathBuf,
}

impl ExitTransactionMarker {
    fn create(router_root: &Path) -> anyhow::Result<Self> {
        let directory = user_data::data_root(router_root).join("pids");
        std::fs::create_dir_all(&directory)?;
        let path = directory.join("gui-exit.pid");
        std::fs::write(&path, std::process::id().to_string())?;
        Ok(Self { path })
    }
}

impl Drop for ExitTransactionMarker {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn restore_codex_and_stop_router_for_exit(
    router_root: &Path,
    share_codex_state: bool,
) -> anyhow::Result<()> {
    let restore_result = (|| -> anyhow::Result<()> {
        let _config_lock =
            profiles::acquire_config_apply_lock(router_root, EXIT_CONFIG_LOCK_TIMEOUT)
                .context("could not acquire the Router configuration lock during exit")?;
        let config_path = user_data::config_path(router_root);
        if config_path.is_file() {
            let applied_config = RouterConfig::load(&config_path)
                .context("could not load the last applied Router configuration during exit")?;
            let router_mode_configured = logic::codex_router_mode_configured(&applied_config);
            restore_codex_for_exit(
                router_root,
                &applied_config,
                share_codex_state,
                router_mode_configured,
            )?;
        }
        Ok(())
    })();

    // A broken/stale Codex snapshot must not leave the forwarding stack alive
    // after the user explicitly selected Exit. Always attempt both halves and
    // retain both errors when cleanup also fails.
    let stop_result = stop_router_for_exit(router_root);
    match (restore_result, stop_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(restore_error), Ok(())) => Err(restore_error),
        (Ok(()), Err(stop_error)) => Err(stop_error),
        (Err(restore_error), Err(stop_error)) => Err(anyhow::anyhow!(
            "Codex restore failed: {restore_error}; Router shutdown also failed: {stop_error}"
        )),
    }
}

fn restore_codex_for_exit(
    router_root: &Path,
    config: &RouterConfig,
    share_codex_state: bool,
    router_mode_configured: bool,
) -> anyhow::Result<()> {
    if router_mode_configured {
        profiles::restore_original_codex(router_root, config, share_codex_state)
            .map(|_| ())
            .context("could not restore the Codex configuration used before Router")?;
    }
    Ok(())
}

fn update_autostart_registration(router_root: &Path, enabled: bool) -> anyhow::Result<()> {
    let script_name = if enabled {
        "Register-Autostart.ps1"
    } else {
        "Unregister-Autostart.ps1"
    };
    let script = router_root.join("scripts").join(script_name);
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(script)
        .current_dir(router_root)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .context("could not start the autostart helper")?;
    if output.status.success() {
        return Ok(());
    }
    let detail = if output.stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout)
    } else {
        String::from_utf8_lossy(&output.stderr)
    };
    let detail = runtime_logs::summarize_error_for_display(detail.trim());
    anyhow::bail!("autostart helper failed: {detail}")
}

fn legacy_autostart_shortcut_exists() -> bool {
    std::env::var_os("APPDATA").is_some_and(|app_data| {
        PathBuf::from(app_data)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup")
            .join("Codex Router.lnk")
            .is_file()
    })
}

#[cfg(windows)]
fn hide_current_process_windows() {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, ShowWindow, SW_HIDE,
    };

    unsafe extern "system" fn hide_owned_window(hwnd: HWND, owner_pid: LPARAM) -> i32 {
        let mut window_pid = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut window_pid);
            if window_pid == owner_pid as u32 {
                ShowWindow(hwnd, SW_HIDE);
            }
        }
        1
    }

    unsafe {
        EnumWindows(Some(hide_owned_window), GetCurrentProcessId() as LPARAM);
    }
}

fn decode_icon() -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let image =
        image::load_from_memory_with_format(APP_ICON_ICO, image::ImageFormat::Ico)?.to_rgba8();
    let (width, height) = image.dimensions();

    // Use the compact Windows icon artwork for title-bar and tray sizes, then
    // trim its transparent border so the robot remains recognizable at 16 px.
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0;
    let mut max_y = 0;
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel[3] >= 16 {
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
        .saturating_add((content_width.max(content_height) as f32 * 0.04) as u32)
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
    for (marker, chinese, english) in [
        (
            "ROUTER_OAUTH_PREPARE_LIFECYCLE_BUSY",
            "另一项 Router 操作占用时间过长。程序已自动重试，请等待片刻后再次准备。",
            "Another Router operation remained busy after automatic retries. Wait a moment and prepare again.",
        ),
        (
            "ROUTER_OAUTH_PREPARE_ROUTER_START",
            "本地 Router 未能稳定启动。请查看活动日志中的启动阶段，再点击重新准备环境。",
            "The local Router did not start reliably. Check the startup stage in the activity log, then prepare again.",
        ),
        (
            "ROUTER_OAUTH_PREPARE_ADMIN_LOGIN",
            "本地 Router 已启动，但管理会话未就绪。程序不会打开浏览器，请稍后重新准备。",
            "The local Router started, but its admin session was not ready. No browser was opened; prepare again shortly.",
        ),
        (
            "ROUTER_OAUTH_PREPARE_COMPLIANCE",
            "本地 Router 已启动，但授权前检查未完成。请重新准备环境。",
            "The local Router started, but the pre-authorization check did not finish. Prepare the environment again.",
        ),
        (
            "ROUTER_OAUTH_PREPARE_COMPONENTS",
            "OAuth 本地组件未能加载。请确认发布包完整后重新准备。",
            "The local OAuth components could not be loaded. Verify the release package and prepare again.",
        ),
        (
            "ROUTER_OAUTH_PREPARE_TIMEOUT",
            "首次启动超过了安全准备时限，已清理残留进程。请再次准备；后续启动通常会更快。",
            "First startup exceeded the preparation budget and leftover processes were cleaned up. Prepare again; later starts are usually faster.",
        ),
        (
            "ROUTER_OAUTH_PREPARE_PROCESS",
            "OAuth 准备进程未正常完成，且没有返回有效阶段信息。请重新准备环境。",
            "The OAuth preparation process did not finish and returned no valid stage information. Prepare again.",
        ),
    ] {
        if text.contains(marker) {
            return if zh { chinese } else { english }.to_owned();
        }
    }
    summary
}

fn oauth_prepare_error_from_output(output: &std::process::Output) -> String {
    let raw = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    let text = String::from_utf8_lossy(raw);
    for line in text
        .lines()
        .rev()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let code = value
            .get("code")
            .and_then(serde_json::Value::as_str)
            .filter(|code| code.starts_with("ROUTER_OAUTH_PREPARE_"))
            .unwrap_or("ROUTER_OAUTH_PREPARE_PROCESS");
        let stage = value
            .get("stage")
            .and_then(serde_json::Value::as_str)
            .filter(|stage| {
                !stage.is_empty()
                    && stage.len() <= 48
                    && stage
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            })
            .unwrap_or("unknown");
        return format!("{code} stage={stage}");
    }
    "ROUTER_OAUTH_PREPARE_PROCESS stage=unknown".to_owned()
}

fn oauth_prepare_error_is_retryable(error: &str) -> bool {
    error.contains("ROUTER_OAUTH_PREPARE_LIFECYCLE_BUSY")
        || error.contains("ROUTER_LIFECYCLE_DEFERRED")
}

fn usage_error_for_display(zh: bool, text: &str) -> String {
    let trimmed = text.trim();
    let summary = if trimmed.starts_with("class=")
        && !trimmed.contains('\r')
        && !trimmed.contains('\n')
        && trimmed.len() <= 512
        && trimmed.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'=' | b'_' | b'+' | b'|' | b' ' | b'-' | b'.')
        }) {
        trimmed.to_owned()
    } else {
        runtime_logs::summarize_error_for_display(trimmed)
    };
    let class = summary
        .split('|')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("class="))
        .unwrap_or("unclassified_error");
    match (zh, class) {
        (true, "configuration") => {
            "本地用量配置正在更新，请稍后重新查询。已保留上次成功数据。".to_owned()
        }
        (false, "configuration") => {
            "The local usage configuration is being updated. Retry shortly; the last successful data is retained."
                .to_owned()
        }
        (true, "connection_refused" | "connection_closed" | "process_failure") => {
            "本地 Router 正在启动或恢复，请稍后重新查询。".to_owned()
        }
        (false, "connection_refused" | "connection_closed" | "process_failure") => {
            "The local Router is starting or recovering. Retry shortly.".to_owned()
        }
        (true, "timeout" | "network" | "dns" | "proxy" | "tls") => {
            "上游额度服务暂时不可用，已保留上次成功数据。".to_owned()
        }
        (false, "timeout" | "network" | "dns" | "proxy" | "tls") => {
            "The upstream quota service is temporarily unavailable; the last successful data is retained."
                .to_owned()
        }
        (true, "authentication" | "permission") => {
            "额度服务拒绝了当前授权，请在授权页面重新登录或检查凭据。".to_owned()
        }
        (false, "authentication" | "permission") => {
            "The quota service rejected the current authorization. Sign in again or check the credential."
                .to_owned()
        }
        (true, _) => "用量查询暂时失败，已保留上次成功数据，请稍后重试。".to_owned(),
        (false, _) => {
            "The usage query temporarily failed. The last successful data is retained; retry shortly."
                .to_owned()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestResultDisposition {
    Ignore,
    RefreshCurrent,
    Apply,
}

fn request_result_disposition(
    current_generation: u64,
    current_key: &str,
    event_generation: u64,
    event_key: &str,
) -> RequestResultDisposition {
    if event_generation != current_generation {
        RequestResultDisposition::Ignore
    } else if event_key != current_key {
        RequestResultDisposition::RefreshCurrent
    } else {
        RequestResultDisposition::Apply
    }
}

fn next_request_generation(current: &mut u64) -> u64 {
    *current = current.wrapping_add(1).max(1);
    *current
}

fn restore_apply_ui_fields(
    config: &mut RouterConfig,
    active_profile_id: &mut String,
    pending_profile_activation: &mut Option<String>,
    configured: &mut bool,
    router_mode_enabled: &mut bool,
    rollback: ApplyUiRollback,
) {
    *config = rollback.config;
    *active_profile_id = rollback.active_profile_id;
    *pending_profile_activation = rollback.pending_profile_activation;
    *configured = rollback.configured;
    *router_mode_enabled = rollback.router_mode_enabled;
}

fn deploy_router_config<F>(
    cfg: &mut RouterConfig,
    router_root: &Path,
    cancel: &AtomicBool,
    zh: bool,
    mut on_log: F,
) -> anyhow::Result<()>
where
    F: FnMut(String),
{
    if cancel.load(Ordering::Acquire) {
        anyhow::bail!("deployment cancelled because Codex-Router is exiting");
    }
    logic::store_credentials(cfg, router_root)?;
    on_log(
        if zh {
            "API Key 已安全保存到 Windows 凭据管理器"
        } else {
            "API keys were stored securely in Windows Credential Manager"
        }
        .to_owned(),
    );
    logic::write_all_files(cfg, router_root)?;
    on_log(
        if zh {
            "无密钥配置和模型目录已写入"
        } else {
            "Secret-free configuration and model catalog were written"
        }
        .to_owned(),
    );
    logic::run_apply_script_with_cancel(router_root, cancel, |line| {
        on_log(localized_deployment_line(zh, line));
    })
}

fn rollback_failed_deployment<F>(
    router_root: &Path,
    backup: &ApplyTransactionBackup,
    share_codex_state: bool,
    zh: bool,
    mut on_log: F,
) -> anyhow::Result<()>
where
    F: FnMut(String),
{
    on_log(
        if zh {
            "部署失败，正在恢复应用前配置…"
        } else {
            "Deployment failed; restoring the previously applied configuration…"
        }
        .to_owned(),
    );
    let rollback_cancel = AtomicBool::new(false);
    profiles::restore_point_config_and_deploy(
        router_root,
        &backup.point,
        &backup.config,
        share_codex_state,
        |restored| {
            let mut restored = restored.clone();
            deploy_router_config(
                &mut restored,
                router_root,
                &rollback_cancel,
                zh,
                &mut on_log,
            )
        },
    )?;
    on_log(
        if zh {
            "已恢复应用前配置"
        } else {
            "The previously applied configuration was restored"
        }
        .to_owned(),
    );
    Ok(())
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
    fn new(
        cc: &eframe::CreationContext<'_>,
        start_in_background: bool,
        ui_audit: Option<UiAuditOptions>,
    ) -> Self {
        if let Some(options) = ui_audit {
            return Self::new_ui_audit(cc, options);
        }
        let (event_tx, event_rx) = channel();
        let (runtime_log_tx, runtime_log_rx) = runtime_logs::bounded_channel();
        let router_root = RouterConfig::find_router_root();
        let _ = user_data::prepare(&router_root);
        let ui_preferences_path = user_data::preferences_path(&router_root);
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
        let config_path = user_data::config_path(&router_root);
        let (mut config, mut page, configured) = match RouterConfig::load(&config_path) {
            Ok(cfg) => (cfg, Page::Dashboard, true),
            Err(_) => (RouterConfig::default(), Page::Welcome, false),
        };
        config.version = CURRENT_CONFIG_VERSION.to_owned();
        logic::normalize_default_model(&mut config);
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
                        .with_tooltip(APP_TITLE)
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
                        tray_ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        tray_ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        tray_ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
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
        let router_mode_enabled = logic::codex_router_mode_configured(&config);
        let codex_account_mode_status = profiles::codex_account_mode_status(&router_root, &config);
        let has_selected_oauth = config
            .oauth_account_ids
            .as_ref()
            .is_some_and(|accounts| !accounts.is_empty())
            || config.models.iter().any(|model| model.source == "oauth");
        let oauth_recovery_due = (router_mode_enabled && has_selected_oauth)
            .then(|| std::time::Instant::now() + std::time::Duration::from_secs(5));
        let runtime_log_paused = Arc::new(AtomicBool::new(start_in_background));
        let mut app = Self {
            ui_audit_mode: false,
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
            apply_cancel: Arc::new(AtomicBool::new(false)),
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
            terms_scroll_reset_pending: false,
            advanced_json_open: false,
            advanced_json_draft: String::new(),
            reasoning_open: false,
            reasoning_mode_draft: "auto".to_owned(),
            reasoning_levels_draft: String::new(),
            reasoning_default_draft: String::new(),
            reasoning_fast_supported_draft: false,
            reasoning_fast_mode_draft: false,
            close_behavior,
            autostart_switching: false,
            close_prompt_open: false,
            apply_success_dialog_open: false,
            remember_close_choice: false,
            exit_after_prompt: false,
            exit_shutdown_in_progress: false,
            exit_shutdown_error: String::new(),
            local_profile_name_input: String::new(),
            isolation_profiles,
            active_profile_id,
            pending_profile_activation: None,
            pending_apply_rollback: None,
            oauth_accounts: Vec::new(),
            oauth_loading: false,
            oauth_error: String::new(),
            oauth_return_page: Page::Dashboard,
            oauth_provider_draft: "openai".to_owned(),
            provider_oauth_running: false,
            pending_oauth_provider: None,
            provider_oauth_preparing: false,
            provider_oauth_preparing_provider: None,
            provider_oauth_prepare_generation: 0,
            provider_oauth_prepared_provider: None,
            provider_oauth_prepare_error: String::new(),
            provider_oauth_prepare_cancel: Arc::new(AtomicBool::new(false)),
            provider_oauth_cancel: Arc::new(AtomicBool::new(false)),
            oauth_revoke_target: None,
            oauth_revoking: false,
            oauth_manual_model_target: None,
            oauth_manual_model_id_draft: String::new(),
            oauth_manual_model_alias_draft: String::new(),
            oauth_fallback_picker_target: None,
            oauth_fallback_picker_draft: BTreeMap::new(),
            usage_snapshot: None,
            usage_snapshot_profile_key: String::new(),
            usage_loading: false,
            usage_request_generation: 0,
            usage_error: String::new(),
            usage_return_page: Page::Dashboard,
            usage_refresh_due: None,
            monitor_subscription_order,
            monitor_api_order,
            share_codex_state,
            router_mode_enabled,
            router_mode_switching: false,
            codex_account_mode_status,
            codex_account_mode_switching: false,
            oauth_recovery_due,
            oauth_recovery_running: false,
            oauth_recovery_cancel: Arc::new(AtomicBool::new(false)),
            grok_sso_dialog_open: false,
            grok_sso_draft: String::new(),
            grok_sso_importing: false,
            grok_sso_error: String::new(),
            grok_sso_auto_select_pending: false,
            channel_preset_dialog_open: false,
            recommended_platform_dialog_open: false,
            sub2api_intro_open: false,
            log_scroll_to_bottom: true,
            log_follow_latest: true,
            log_dialog_open: false,
            runtime_log_stop: Arc::new(AtomicBool::new(false)),
            runtime_log_paused,
            tray_lightweight_mode: start_in_background,
            background_hide_until: start_in_background
                .then(|| std::time::Instant::now() + std::time::Duration::from_secs(2)),
            tray_restore_guard_until: None,
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
                TRAY_TOOLTIP_ZH
            } else {
                TRAY_TOOLTIP_EN
            }));
        }
        runtime_logs::spawn(
            app.router_root.clone(),
            runtime_log_tx,
            cc.egui_ctx.clone(),
            app.runtime_log_stop.clone(),
            app.runtime_log_paused.clone(),
        );
        if configured || legacy_autostart_shortcut_exists() {
            app.reconcile_autostart_registration();
        }
        app
    }

    fn new_ui_audit(cc: &eframe::CreationContext<'_>, options: UiAuditOptions) -> Self {
        let (event_tx, event_rx) = channel();
        let (runtime_log_tx, runtime_log_rx) = runtime_logs::bounded_channel();
        drop(runtime_log_tx);

        install_app_fonts(&cc.egui_ctx);
        let mut config = RouterConfig {
            version: CURRENT_CONFIG_VERSION.to_owned(),
            ui_theme: options.theme.clone(),
            accept_compliance: true,
            accepted_terms_version: CURRENT_TERMS_VERSION.to_owned(),
            oauth_account_ids: Some(vec![101, 202]),
            oauth_seen_account_ids: vec![101, 202, 303],
            models: vec![
                ModelConfig {
                    model: "gpt-5.6-sol".to_owned(),
                    alias: "ChatGPT-5.6-Sol".to_owned(),
                    base_url: "Sub2API OAuth / openai".to_owned(),
                    priority: 1,
                    reasoning_mode: "manual".to_owned(),
                    reasoning_levels: vec![
                        "low".to_owned(),
                        "medium".to_owned(),
                        "high".to_owned(),
                        "xhigh".to_owned(),
                        "max".to_owned(),
                        "ultra".to_owned(),
                    ],
                    default_reasoning_level: "max".to_owned(),
                    fast_supported: true,
                    fast_mode: true,
                    source: "oauth".to_owned(),
                    oauth_account_id: 101,
                    oauth_platform: "openai".to_owned(),
                    ..Default::default()
                },
                ModelConfig {
                    model: "gpt-5.6-sol".to_owned(),
                    alias: "ChatGPT-5.6-Sol".to_owned(),
                    base_url: "https://api.430123.xyz/v1".to_owned(),
                    credential_name: "OpenAiAuditBackupKey".to_owned(),
                    priority: 10,
                    source: "apikey".to_owned(),
                    ..Default::default()
                },
                ModelConfig {
                    model: "claude-opus-4-6".to_owned(),
                    alias: "Claude Opus 4.6 / API backup".to_owned(),
                    base_url: "https://api.anthropic.com".to_owned(),
                    credential_name: "AnthropicAuditKey".to_owned(),
                    priority: 100,
                    weight: 2,
                    source: "apikey".to_owned(),
                    ..Default::default()
                },
                ModelConfig {
                    model: "kimi-for-coding".to_owned(),
                    alias: "Kimi for Coding / fallback".to_owned(),
                    base_url: "https://api.kimi.com/coding/v1".to_owned(),
                    credential_name: "KimiAuditKey".to_owned(),
                    priority: 120,
                    source: "apikey".to_owned(),
                    ..Default::default()
                },
            ],
            default_model: "gpt-5.6-sol".to_owned(),
            ..Default::default()
        };
        theme::install(&cc.egui_ctx, &theme::palette(&config.ui_theme));

        let oauth_accounts = vec![
            OAuthAccountSummary {
                id: 101,
                name: "OpenAI Pro / primary".to_owned(),
                platform: "openai".to_owned(),
                status: "active".to_owned(),
                email: "router.qa@example.com".to_owned(),
                plan: "pro".to_owned(),
                priority: 1,
                bound_to_router: true,
                error: String::new(),
                expires_at: "2026-08-31T16:00:00Z".to_owned(),
                models: vec![
                    OAuthModelSummary {
                        id: "gpt-5.6-sol".to_owned(),
                        display_name: "ChatGPT-5.6-Sol".to_owned(),
                        suggested: true,
                    },
                    OAuthModelSummary {
                        id: "gpt-5.6-terra".to_owned(),
                        display_name: "ChatGPT-5.6-Terra".to_owned(),
                        suggested: false,
                    },
                    OAuthModelSummary {
                        id: "gpt-5.6-luna".to_owned(),
                        display_name: "ChatGPT-5.6-Luna".to_owned(),
                        suggested: true,
                    },
                ],
            },
            OAuthAccountSummary {
                id: 202,
                name: "Gemini workspace".to_owned(),
                platform: "gemini".to_owned(),
                status: "warning".to_owned(),
                email: "very-long-account-name-for-layout@example.com".to_owned(),
                plan: "workspace enterprise".to_owned(),
                priority: 20,
                bound_to_router: false,
                error: "Token refresh will be retried automatically".to_owned(),
                expires_at: "2026-08-04T03:15:00Z".to_owned(),
                models: vec![OAuthModelSummary {
                    id: "gemini-3.6-pro-preview".to_owned(),
                    display_name: "Gemini 3.6 Pro Preview".to_owned(),
                    suggested: true,
                }],
            },
        ];

        let usage_snapshot = UsageSnapshot {
            profile_name: "Production / long configuration name".to_owned(),
            queried_at: "2026-08-03T03:45:00Z".to_owned(),
            total_tokens: 12_845_930,
            total_requests: 842,
            total_cost: 18.4271,
            subscriptions: vec![
                UsageAccount {
                    id: 101,
                    name: "OpenAI Pro / primary".to_owned(),
                    kind: "subscription".to_owned(),
                    platform: "openai".to_owned(),
                    status: "active".to_owned(),
                    health: "quotaExhausted".to_owned(),
                    status_detail: "The account quota has been exhausted.".to_owned(),
                    query_note: "Live quota returned by provider".to_owned(),
                    updated_at: "2026-08-03T03:45:00Z".to_owned(),
                    totals: UsageTotals {
                        requests: 512,
                        total_tokens: 9_842_331,
                        cost: 0.0,
                        models: Vec::new(),
                    },
                    windows: vec![
                        UsageWindow {
                            kind: "fiveHour".to_owned(),
                            display_name: String::new(),
                            used_percent: Some(62.0),
                            reset_at: "2026-08-03T08:30:00Z".to_owned(),
                            remaining_seconds: 14_400,
                            requests: 148,
                            tokens: 2_300_400,
                        },
                        UsageWindow {
                            kind: "weekly".to_owned(),
                            display_name: String::new(),
                            used_percent: Some(100.0),
                            reset_at: "2026-08-08T03:45:00Z".to_owned(),
                            remaining_seconds: 432_000,
                            requests: 512,
                            tokens: 9_842_331,
                        },
                    ],
                    ..Default::default()
                },
                UsageAccount {
                    id: 202,
                    name: "Gemini workspace / quota exhausted sample".to_owned(),
                    kind: "subscription".to_owned(),
                    platform: "gemini".to_owned(),
                    status: "rate_limited".to_owned(),
                    health: "quotaExhausted".to_owned(),
                    status_detail: "This account is temporarily skipped until its quota resets."
                        .to_owned(),
                    query_note: "Fallback routes remain available.".to_owned(),
                    updated_at: "2026-08-03T03:45:00Z".to_owned(),
                    totals: UsageTotals {
                        requests: 74,
                        total_tokens: 1_540_000,
                        cost: 0.0,
                        models: Vec::new(),
                    },
                    windows: vec![UsageWindow {
                        kind: "monthly".to_owned(),
                        display_name: String::new(),
                        used_percent: Some(100.0),
                        reset_at: "2026-08-08T03:34:00Z".to_owned(),
                        remaining_seconds: 431_340,
                        requests: 74,
                        tokens: 1_540_000,
                    }],
                    ..Default::default()
                },
            ],
            api_channels: vec![UsageAccount {
                id: 303,
                name: "Anthropic API backup channel".to_owned(),
                kind: "api".to_owned(),
                platform: "anthropic".to_owned(),
                status: "active".to_owned(),
                health: "healthy".to_owned(),
                updated_at: "2026-08-03T03:45:00Z".to_owned(),
                totals: UsageTotals {
                    requests: 256,
                    total_tokens: 1_463_599,
                    cost: 18.4271,
                    models: vec![UsageModelSummary {
                        name: "claude-opus-4-6".to_owned(),
                        requests: 256,
                        input_tokens: 1_100_000,
                        output_tokens: 240_000,
                        cache_read_tokens: 100_000,
                        cache_creation_tokens: 23_599,
                        total_tokens: 1_463_599,
                        cost: 18.4271,
                    }],
                },
                ..Default::default()
            }],
        };

        let page = match options.scenario.as_str() {
            "welcome" => Page::Welcome,
            "project" => Page::Project,
            "auth" => Page::Auth,
            "model"
            | "advanced-json"
            | "reasoning"
            | "channel-preset"
            | "recommended-platforms" => Page::Model,
            "proxy-auto" | "proxy-manual" => Page::Proxy,
            "finish"
            | "terms"
            | "oauth-terms-preparing"
            | "oauth-terms-ready"
            | "oauth-terms-error" => Page::Finish,
            "profiles" | "profiles-empty" => Page::Profiles,
            "oauth" | "oauth-loading" | "oauth-error" | "oauth-revoke" | "oauth-manual"
            | "oauth-fallback" | "grok-sso" | "sub2api" => Page::OAuth,
            "monitor" | "monitor-loading" | "monitor-error" => Page::Monitor,
            _ => Page::Dashboard,
        };
        if options.scenario == "proxy-manual" {
            config.proxy.auto_detect = false;
            config.proxy.enabled = true;
            config.proxy.proxy_type = "socks5".to_owned();
            config.proxy.host = "proxy.office.example".to_owned();
            config.proxy.port = "1080".to_owned();
            config.proxy.username = "domain\\router-user".to_owned();
        }
        if matches!(
            options.scenario.as_str(),
            "terms" | "oauth-terms-preparing" | "oauth-terms-ready" | "oauth-terms-error"
        ) {
            config.accept_compliance = false;
            config.accepted_terms_version.clear();
        }
        if options.scenario == "dashboard-empty" {
            config.models.clear();
            config.default_model.clear();
        }

        let first_oauth_account = oauth_accounts.first().cloned();
        let mut app = Self {
            ui_audit_mode: true,
            page,
            router_root: std::env::temp_dir().join("codex-router-ui-audit-read-only"),
            project_path_input: r"C:\Tools\Codex-Router-Portable".to_owned(),
            temp_model: config.models.first().cloned().unwrap_or_default(),
            config,
            editing_model: None,
            model_from_wizard: true,
            proxy_from_wizard: true,
            status_text: "UI audit mode: no system configuration will be changed.".to_owned(),
            logs: "[03:44:58] Router health check: healthy\n[03:45:00] Usage data refreshed\n[03:45:02] OAuth fallback route is ready\n[03:45:04] Unicode check: 中文、模型、连接稳定\n".repeat(8),
            event_rx,
            event_tx,
            runtime_log_rx,
            applying: false,
            apply_cancel: Arc::new(AtomicBool::new(false)),
            configured: true,
            logo_texture: None,
            fonts_loaded: true,
            tray_icon: None,
            last_page: page,
            profiles_return_page: Page::Dashboard,
            page_changed_at: std::time::Instant::now() - std::time::Duration::from_secs(1),
            installed_theme: options.theme,
            installed_compact_layout: options.compact,
            ui_language: options.language,
            terms_open: false,
            terms_scroll_complete: false,
            terms_scroll_reset_pending: false,
            advanced_json_open: false,
            advanced_json_draft: "{\n  \"temperature\": 0.2,\n  \"metadata\": {\"audit\": true}\n}".to_owned(),
            reasoning_open: false,
            reasoning_mode_draft: "manual".to_owned(),
            reasoning_levels_draft: "low, medium, high, xhigh, max, ultra".to_owned(),
            reasoning_default_draft: "max".to_owned(),
            reasoning_fast_supported_draft: true,
            reasoning_fast_mode_draft: true,
            close_behavior: CloseBehavior::Ask,
            autostart_switching: false,
            close_prompt_open: false,
            apply_success_dialog_open: false,
            remember_close_choice: false,
            exit_after_prompt: false,
            exit_shutdown_in_progress: false,
            exit_shutdown_error: String::new(),
            local_profile_name_input: "工作 / 独立账号配置".to_owned(),
            isolation_profiles: vec![
                IsolationProfile {
                    id: "profile-work".to_owned(),
                    name: "工作账号 / 稳定路由".to_owned(),
                    kind: IsolationKind::Local,
                    created_at: "2026-08-01T09:00:00Z".to_owned(),
                    updated_at: "2026-08-03T03:00:00Z".to_owned(),
                },
                IsolationProfile {
                    id: "profile-lab".to_owned(),
                    name: "实验室 / Very long profile name for compact layout".to_owned(),
                    kind: IsolationKind::Local,
                    created_at: "2026-08-02T09:00:00Z".to_owned(),
                    updated_at: "2026-08-03T03:00:00Z".to_owned(),
                },
            ],
            active_profile_id: "profile-work".to_owned(),
            pending_profile_activation: None,
            pending_apply_rollback: None,
            oauth_accounts,
            oauth_loading: false,
            oauth_error: String::new(),
            oauth_return_page: Page::Dashboard,
            oauth_provider_draft: "openai".to_owned(),
            provider_oauth_running: false,
            pending_oauth_provider: None,
            provider_oauth_preparing: false,
            provider_oauth_preparing_provider: None,
            provider_oauth_prepare_generation: 0,
            provider_oauth_prepared_provider: None,
            provider_oauth_prepare_error: String::new(),
            provider_oauth_prepare_cancel: Arc::new(AtomicBool::new(false)),
            provider_oauth_cancel: Arc::new(AtomicBool::new(false)),
            oauth_revoke_target: None,
            oauth_revoking: false,
            oauth_manual_model_target: None,
            oauth_manual_model_id_draft: "gemini-3.6-flash".to_owned(),
            oauth_manual_model_alias_draft: "Gemini 3.6 Flash / 手动".to_owned(),
            oauth_fallback_picker_target: None,
            oauth_fallback_picker_draft: BTreeMap::new(),
            usage_snapshot: Some(usage_snapshot),
            usage_snapshot_profile_key: "profile-work".to_owned(),
            usage_loading: false,
            usage_request_generation: 0,
            usage_error: String::new(),
            usage_return_page: Page::Dashboard,
            usage_refresh_due: None,
            monitor_subscription_order: vec![101, 202],
            monitor_api_order: vec![303],
            share_codex_state: true,
            router_mode_enabled: true,
            router_mode_switching: false,
            codex_account_mode_status: profiles::CodexAccountModeStatus {
                mode: profiles::CodexAccountMode::Official,
                official_snapshot_available: true,
            },
            codex_account_mode_switching: false,
            oauth_recovery_due: None,
            oauth_recovery_running: false,
            oauth_recovery_cancel: Arc::new(AtomicBool::new(false)),
            grok_sso_dialog_open: false,
            grok_sso_draft: "audit-token-hidden".to_owned(),
            grok_sso_importing: false,
            grok_sso_error: String::new(),
            grok_sso_auto_select_pending: false,
            channel_preset_dialog_open: false,
            recommended_platform_dialog_open: false,
            sub2api_intro_open: false,
            log_scroll_to_bottom: true,
            log_follow_latest: true,
            log_dialog_open: false,
            runtime_log_stop: Arc::new(AtomicBool::new(false)),
            runtime_log_paused: Arc::new(AtomicBool::new(true)),
            tray_lightweight_mode: false,
            background_hide_until: None,
            tray_restore_guard_until: None,
            health_probe_due: None,
            health_probe_running: false,
            health_probe_failures: 0,
            health_recovery_running: false,
            health_recovery_cancel: Arc::new(AtomicBool::new(false)),
            update_checking: false,
            update_downloading: false,
            update_dialog_open: false,
            update_info: None,
        };

        match options.scenario.as_str() {
            "profiles-empty" => {
                app.isolation_profiles.clear();
                app.active_profile_id.clear();
                app.local_profile_name_input.clear();
            }
            "oauth-loading" => {
                app.oauth_accounts.clear();
                app.oauth_loading = true;
            }
            "oauth-error" => {
                app.oauth_accounts.clear();
                app.oauth_error =
                    "class=connection_refused | status=503 | retryable=true".to_owned();
            }
            "monitor-loading" => {
                app.usage_snapshot = None;
                app.usage_loading = true;
            }
            "monitor-error" => {
                app.usage_snapshot = None;
                app.usage_error = "class=upstream_timeout | status=504 | retryable=true".to_owned();
            }
            "close" => app.close_prompt_open = true,
            "apply-success" => app.apply_success_dialog_open = true,
            "logs" => app.log_dialog_open = true,
            "sub2api" => app.sub2api_intro_open = true,
            "channel-preset" => app.channel_preset_dialog_open = true,
            "recommended-platforms" => app.recommended_platform_dialog_open = true,
            "grok-sso" => app.grok_sso_dialog_open = true,
            "terms" => {
                app.terms_open = true;
                app.terms_scroll_complete = false;
                app.terms_scroll_reset_pending = true;
            }
            "oauth-terms-preparing" => {
                app.terms_open = true;
                app.terms_scroll_complete = false;
                app.terms_scroll_reset_pending = true;
                app.pending_oauth_provider = Some("openai".to_owned());
                app.provider_oauth_preparing = true;
            }
            "oauth-terms-ready" => {
                app.terms_open = true;
                app.terms_scroll_complete = true;
                app.terms_scroll_reset_pending = true;
                app.pending_oauth_provider = Some("openai".to_owned());
                app.provider_oauth_prepared_provider = Some("openai".to_owned());
            }
            "oauth-terms-error" => {
                app.terms_open = true;
                app.terms_scroll_complete = true;
                app.terms_scroll_reset_pending = true;
                app.pending_oauth_provider = Some("openai".to_owned());
                app.provider_oauth_prepare_error =
                    "The local Router did not become ready within 120 seconds".to_owned();
            }
            "advanced-json" => app.advanced_json_open = true,
            "reasoning" => app.reasoning_open = true,
            "oauth-revoke" => app.oauth_revoke_target = first_oauth_account.clone(),
            "oauth-manual" => app.oauth_manual_model_target = first_oauth_account,
            "oauth-fallback" => {
                app.oauth_fallback_picker_target = first_oauth_account;
                app.oauth_fallback_picker_draft
                    .insert("gpt-5.6-sol".to_owned(), None);
            }
            "update-current" => {
                app.update_dialog_open = true;
                app.update_info = Some(GitHubUpdateInfo {
                    status: "current".to_owned(),
                    current_version: APP_VERSION.to_owned(),
                    latest_version: APP_VERSION.to_owned(),
                    release_url: OFFICIAL_GITHUB_URL.to_owned(),
                    ..Default::default()
                });
            }
            "update-available" => {
                app.update_dialog_open = true;
                app.update_info = Some(GitHubUpdateInfo {
                    status: "update_available".to_owned(),
                    current_version: APP_VERSION.to_owned(),
                    latest_version: "1.0.1".to_owned(),
                    release_name: "Codex-Router 1.0.1".to_owned(),
                    release_notes: "- Improve connection recovery\n- Reduce tray CPU usage\n- 修复中文布局与代理检测".to_owned(),
                    release_url: OFFICIAL_GITHUB_URL.to_owned(),
                    asset_name: "Codex-Router-Portable-1.0.1-windows-x64.zip".to_owned(),
                    download_url: "https://example.invalid/release.zip".to_owned(),
                    asset_size: 84_000_000,
                    message: "Signed portable release".to_owned(),
                    ..Default::default()
                });
            }
            "update-error" => {
                app.update_dialog_open = true;
                app.update_info = Some(GitHubUpdateInfo {
                    status: "error".to_owned(),
                    current_version: APP_VERSION.to_owned(),
                    message: "class=network_unavailable | retryable=true".to_owned(),
                    release_url: OFFICIAL_GITHUB_URL.to_owned(),
                    ..Default::default()
                });
            }
            "update-downloaded" => {
                app.update_dialog_open = true;
                app.update_info = Some(GitHubUpdateInfo {
                    status: "downloaded".to_owned(),
                    current_version: APP_VERSION.to_owned(),
                    latest_version: "1.0.1".to_owned(),
                    release_url: OFFICIAL_GITHUB_URL.to_owned(),
                    download_path: r"C:\Downloads\Codex-Router-Portable-1.0.1.zip".to_owned(),
                    ..Default::default()
                });
            }
            _ => {}
        }
        app
    }

    fn persist_close_behavior(&mut self) -> bool {
        if self.ui_audit_mode {
            return true;
        }
        let path = user_data::preferences_path(&self.router_root);
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

    fn start_autostart_update(&mut self, enabled: bool, rollback_to: Option<bool>) {
        if self.ui_audit_mode || self.autostart_switching {
            return;
        }
        self.autostart_switching = true;
        let router_root = self.router_root.clone();
        let tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let result = update_autostart_registration(&router_root, enabled)
                .map_err(|error| error.to_string());
            tx.send(AppEvent::AutostartFinished {
                enabled,
                rollback_to,
                result,
            })
            .ok();
        });
    }

    fn set_start_with_windows(&mut self, enabled: bool) {
        if self.autostart_switching || self.config.deploy.start_with_windows == enabled {
            return;
        }
        let previous = self.config.deploy.start_with_windows;
        self.config.deploy.start_with_windows = enabled;
        let config_path = user_data::config_path(&self.router_root);
        if let Err(error) = self.config.save(&config_path) {
            self.config.deploy.start_with_windows = previous;
            self.report_error(if self.ui_language == "zh" {
                format!("无法保存开机自启设置：{error}")
            } else {
                format!("Could not save the autostart setting: {error}")
            });
            return;
        }
        self.start_autostart_update(enabled, Some(previous));
    }

    fn reconcile_autostart_registration(&mut self) {
        let enabled = self.config.deploy.start_with_windows;
        self.start_autostart_update(enabled, None);
    }

    fn refresh_isolation_profiles(&mut self) {
        if self.ui_audit_mode {
            return;
        }
        match profiles::list_profiles(&self.router_root) {
            Ok(items) => self.isolation_profiles = items,
            Err(error) => self.report_error(format!("无法读取隔离配置：{error}")),
        }
    }

    fn capture_apply_ui_rollback(&self) -> ApplyUiRollback {
        ApplyUiRollback {
            config: self.config.clone(),
            active_profile_id: self.active_profile_id.clone(),
            pending_profile_activation: self.pending_profile_activation.clone(),
            configured: self.configured,
            router_mode_enabled: self.router_mode_enabled,
        }
    }

    fn restore_pending_apply_ui_rollback(&mut self) -> bool {
        let Some(rollback) = self.pending_apply_rollback.take() else {
            return false;
        };
        restore_apply_ui_fields(
            &mut self.config,
            &mut self.active_profile_id,
            &mut self.pending_profile_activation,
            &mut self.configured,
            &mut self.router_mode_enabled,
            rollback,
        );
        true
    }

    fn open_profiles(&mut self) {
        if self.ui_audit_mode {
            self.profiles_return_page = self.page;
            self.page = Page::Profiles;
            return;
        }
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
        if self.ui_audit_mode {
            self.status_text = "UI audit mode: profile creation is disabled.".to_owned();
            return;
        }
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
        let ui_rollback = self.capture_apply_ui_rollback();
        match profiles::create_profile(&self.router_root, &requested_name, kind, &self.config) {
            Ok((profile, isolated_config)) => {
                let profile_id = profile.id.clone();
                self.pending_apply_rollback = Some(ui_rollback);
                self.config = isolated_config;
                self.pending_profile_activation = Some(profile_id);
                self.refresh_isolation_profiles();
                if self.apply_all_with_backup(true, None, None) {
                    if self.local_profile_name_input.trim() == requested_name {
                        self.local_profile_name_input.clear();
                    }
                    self.status_text = format!("正在创建并应用本地隔离配置“{}”…", profile.name);
                } else {
                    self.restore_pending_apply_ui_rollback();
                }
            }
            Err(error) => self.report_error(error.to_string()),
        }
    }

    fn apply_isolation_profile(&mut self, profile: &IsolationProfile) {
        if self.ui_audit_mode {
            self.status_text = "UI audit mode: profile switching is disabled.".to_owned();
            return;
        }
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
        let (rollback_point, rollback_config) = match profiles::capture_applied_restore_point(
            &self.router_root,
            &self.config,
            &format!("切换到“{}”之前", profile.name),
        ) {
            Ok(backup) => backup,
            Err(error) => {
                self.report_error(format!("切换前备份失败，已停止操作：{error}"));
                return;
            }
        };
        let transaction_backup = ApplyTransactionBackup {
            point: rollback_point,
            config: rollback_config,
        };
        let ui_rollback = self.capture_apply_ui_rollback();
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
        self.pending_apply_rollback = Some(ui_rollback);
        self.config = target_config;
        self.pending_profile_activation = Some(profile.id.clone());
        if self.apply_all_with_backup(false, Some(config_lock), Some(transaction_backup.clone())) {
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
            let _ = profiles::restore_point_config(
                &self.router_root,
                &transaction_backup.point,
                &transaction_backup.config,
                self.share_codex_state,
            );
            self.restore_pending_apply_ui_rollback();
        }
    }

    fn restore_original_codex(&mut self) {
        if self.ui_audit_mode {
            self.status_text = "UI audit mode: Codex restore is disabled.".to_owned();
            return;
        }
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
        if self.ui_audit_mode {
            self.status_text = "UI audit mode: restore is disabled.".to_owned();
            return;
        }
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
        self.pending_apply_rollback = Some(self.capture_apply_ui_rollback());
        self.applying = true;
        self.configured = false;
        let zh = self.ui_language == "zh";
        self.status_text = if zh {
            format!("正在恢复并重新部署上一次配置（{}）…", target.label)
        } else {
            format!(
                "Restoring and redeploying the previous configuration ({})…",
                target.label
            )
        };
        self.log(self.status_text.clone());

        let root = self.router_root.clone();
        let fallback = self.config.clone();
        let share_codex_state = self.share_codex_state;
        let tx = self.event_tx.clone();
        let apply_cancel = self.apply_cancel.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<(RouterConfig, profiles::RestoreOutcome)> {
                let _config_lock =
                    profiles::acquire_config_apply_lock(&root, std::time::Duration::from_secs(10))?;
                let (rollback_point, rollback_config) = profiles::capture_applied_restore_point(
                    &root,
                    &fallback,
                    "执行返回上一次配置之前",
                )?;
                let transaction_backup = ApplyTransactionBackup {
                    point: rollback_point,
                    config: rollback_config,
                };
                let restored = profiles::restore_point_config_and_deploy(
                    &root,
                    &target,
                    &fallback,
                    share_codex_state,
                    |restored| {
                        let mut restored = restored.clone();
                        deploy_router_config(&mut restored, &root, &apply_cancel, zh, |line| {
                            tx.send(AppEvent::Log(line)).ok();
                        })
                    },
                );
                match restored {
                    Ok(value) => Ok(value),
                    Err(error) if apply_cancel.load(Ordering::Acquire) => Err(error),
                    Err(error) => {
                        let rollback = rollback_failed_deployment(
                            &root,
                            &transaction_backup,
                            share_codex_state,
                            zh,
                            |line| {
                                tx.send(AppEvent::Log(line)).ok();
                            },
                        );
                        match rollback {
                            Ok(()) => Err(error.context("已恢复还原操作前的配置")),
                            Err(rollback_error) => Err(error.context(format!(
                                "还原失败，且无法恢复还原前配置：{rollback_error}"
                            ))),
                        }
                    }
                }
            })();
            match result {
                Ok((config, outcome)) => {
                    tx.send(AppEvent::PreviousConfigurationRestored {
                        config: Box::new(config),
                        outcome,
                        label: target.label,
                    })
                    .ok();
                }
                Err(error) => {
                    tx.send(AppEvent::PreviousConfigurationRestoreError(
                        error.to_string(),
                    ))
                    .ok();
                }
            }
        });
    }

    fn minimize_to_tray(&mut self, ctx: &egui::Context) {
        self.close_prompt_open = false;
        self.remember_close_choice = false;
        if self.tray_icon.is_some() {
            self.tray_lightweight_mode = true;
            self.runtime_log_paused.store(true, Ordering::Relaxed);
            self.usage_refresh_due = None;
            if self.router_mode_enabled && self.oauth_recovery_due.is_none() {
                self.oauth_recovery_due =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
            }
            if self.fonts_loaded {
                install_lightweight_fonts(ctx);
                self.fonts_loaded = false;
                self.logo_texture = None;
            }
            if let Some(tray) = &self.tray_icon {
                let _ = tray.set_tooltip(Some(if self.ui_language == "zh" {
                    TRAY_TOOLTIP_ZH
                } else {
                    TRAY_TOOLTIP_EN
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
        self.background_hide_until = None;
        let was_lightweight = self.tray_lightweight_mode;
        if !self.fonts_loaded {
            install_app_fonts(ctx);
            self.fonts_loaded = true;
            self.installed_theme.clear();
        }
        self.tray_lightweight_mode = false;
        self.tray_restore_guard_until =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(1));
        self.load_logo_texture(ctx);
        self.runtime_log_paused.store(false, Ordering::Relaxed);
        if self.router_mode_enabled
            && (self
                .config
                .oauth_account_ids
                .as_ref()
                .is_some_and(|accounts| !accounts.is_empty())
                || self
                    .config
                    .models
                    .iter()
                    .any(|model| model.source == "oauth"))
            && self.oauth_recovery_due.is_none()
        {
            self.oauth_recovery_due =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
        }
        if let Some(tray) = &self.tray_icon {
            let _ = tray.set_tooltip(Some(APP_TITLE));
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
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    fn enforce_background_start_hidden(&mut self, ctx: &egui::Context) {
        let Some(deadline) = self.background_hide_until else {
            return;
        };
        if std::time::Instant::now() >= deadline || !self.tray_lightweight_mode {
            self.background_hide_until = None;
            return;
        }
        hide_current_process_windows();
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }

    fn request_exit(&mut self, ctx: &egui::Context) {
        if self.exit_shutdown_in_progress {
            return;
        }
        if self.ui_audit_mode {
            self.exit_after_prompt = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        self.close_prompt_open = true;
        self.remember_close_choice = false;
        let router_root = self.router_root.clone();
        let exit_marker = match ExitTransactionMarker::create(&router_root) {
            Ok(marker) => marker,
            Err(error) => {
                self.exit_shutdown_error = if self.ui_language == "zh" {
                    format!("无法建立安全退出事务，程序仍保持运行：{error}")
                } else {
                    format!("Could not establish the safe exit transaction; the app remains open: {error}")
                };
                self.status_text.clone_from(&self.exit_shutdown_error);
                ctx.request_repaint();
                return;
            }
        };
        self.exit_shutdown_in_progress = true;
        self.exit_shutdown_error.clear();
        self.apply_cancel.store(true, Ordering::Release);
        self.runtime_log_paused.store(true, Ordering::Relaxed);
        self.health_recovery_cancel.store(true, Ordering::Relaxed);
        self.provider_oauth_prepare_cancel
            .store(true, Ordering::Relaxed);
        self.provider_oauth_cancel.store(true, Ordering::Relaxed);
        self.oauth_recovery_cancel.store(true, Ordering::Relaxed);
        self.health_probe_due = None;
        self.oauth_recovery_due = None;
        self.status_text = if self.ui_language == "zh" {
            "正在恢复原 Codex 配置并彻底停止本地转发服务…".to_owned()
        } else {
            "Restoring the previous Codex configuration and stopping local forwarding services…"
                .to_owned()
        };
        if let Some(tray) = &self.tray_icon {
            let _ = tray.set_visible(false);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        let share_codex_state = self.share_codex_state;
        let tx = self.event_tx.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let _exit_marker = exit_marker;
            let result = restore_codex_and_stop_router_for_exit(&router_root, share_codex_state)
                .map_err(|error| error.to_string());
            tx.send(AppEvent::ExitShutdownFinished(result)).ok();
            repaint.request_repaint();
        });
        ctx.request_repaint();
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|input| input.viewport().close_requested()) {
            return;
        }
        if self.exit_after_prompt {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if self.exit_shutdown_in_progress {
            return;
        }
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
        if self
            .tray_restore_guard_until
            .is_some_and(|deadline| std::time::Instant::now() < deadline)
        {
            return;
        }
        self.tray_restore_guard_until = None;
        if !self.tray_lightweight_mode
            && self.tray_icon.is_some()
            && ctx.input(|input| input.viewport().minimized == Some(true))
        {
            self.minimize_to_tray(ctx);
        }
    }

    fn process_app_events(&mut self, ctx: &egui::Context) {
        let zh = self.ui_language == "zh";
        let discard_background_events = self.exit_shutdown_in_progress;
        while let Ok(event) = self.event_rx.try_recv() {
            if discard_background_events && !matches!(&event, AppEvent::ExitShutdownFinished(_)) {
                continue;
            }
            match event {
                AppEvent::Log(message) => self.log(message),
                AppEvent::ExitShutdownFinished(result) => {
                    self.exit_shutdown_in_progress = false;
                    match result {
                        Ok(()) => {
                            self.exit_shutdown_error.clear();
                            self.router_mode_enabled = false;
                            self.exit_after_prompt = true;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        Err(error) => {
                            self.runtime_log_paused.store(false, Ordering::Relaxed);
                            self.health_probe_due = None;
                            if let Some(tray) = &self.tray_icon {
                                let _ = tray.set_visible(true);
                            }
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                            let detail = runtime_logs::summarize_error_for_display(&error);
                            self.exit_shutdown_error = if zh {
                                format!("未能彻底关闭本地服务：{detail}")
                            } else {
                                format!("Could not stop all local services: {detail}")
                            };
                            self.status_text.clone_from(&self.exit_shutdown_error);
                            self.close_prompt_open = true;
                            self.log(self.status_text.clone());
                        }
                    }
                }
                AppEvent::AutostartFinished {
                    enabled,
                    rollback_to,
                    result,
                } => {
                    self.autostart_switching = false;
                    match result {
                        Ok(()) => {
                            self.status_text = if zh {
                                if enabled {
                                    "已开启开机静默启动；下次登录后将直接进入轻量托盘模式"
                                } else {
                                    "已关闭开机自启"
                                }
                            } else if enabled {
                                "Silent startup enabled; the app will enter lightweight tray mode after the next sign-in"
                            } else {
                                "Startup with Windows disabled"
                            }
                            .to_owned();
                        }
                        Err(error) => {
                            if let Some(previous) = rollback_to {
                                self.config.deploy.start_with_windows = previous;
                                let _ =
                                    self.config.save(&user_data::config_path(&self.router_root));
                            }
                            let detail = runtime_logs::summarize_error_for_display(&error);
                            self.report_error(if zh {
                                format!("无法更新开机自启设置：{detail}")
                            } else {
                                format!("Could not update the autostart setting: {detail}")
                            });
                        }
                    }
                }
                AppEvent::Complete => {
                    self.applying = false;
                    if self.exit_shutdown_in_progress {
                        continue;
                    }
                    self.apply_success_dialog_open = true;
                    self.configured = true;
                    self.router_mode_enabled = true;
                    self.codex_account_mode_status =
                        profiles::codex_account_mode_status(&self.router_root, &self.config);
                    self.router_mode_switching = false;
                    self.oauth_recovery_due =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
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
                                // Deployment already committed the selected profile. Keep the
                                // active id aligned with the running route even if refreshing the
                                // profile snapshot itself failed.
                                self.active_profile_id = profile_id;
                                self.persist_ui_preferences();
                                self.refresh_isolation_profiles();
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
                    self.pending_apply_rollback = None;
                }
                AppEvent::Error(error) => {
                    self.applying = false;
                    if self.exit_shutdown_in_progress {
                        continue;
                    }
                    self.router_mode_switching = false;
                    if !self.restore_pending_apply_ui_rollback() {
                        self.router_mode_enabled = logic::codex_router_mode_active(&self.config);
                        self.pending_profile_activation = None;
                    }
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
                AppEvent::PreviousConfigurationRestored {
                    config,
                    outcome,
                    label,
                } => {
                    self.applying = false;
                    self.pending_apply_rollback = None;
                    self.config = *config;
                    self.configured = true;
                    self.active_profile_id.clear();
                    self.pending_profile_activation = None;
                    self.router_mode_enabled = logic::codex_router_mode_active(&self.config);
                    self.codex_account_mode_status =
                        profiles::codex_account_mode_status(&self.router_root, &self.config);
                    self.oauth_recovery_due = self
                        .router_mode_enabled
                        .then(|| std::time::Instant::now() + std::time::Duration::from_secs(5));
                    self.persist_ui_preferences();
                    self.schedule_usage_refresh();
                    self.status_text = if outcome.shared_state_preserved {
                        if zh {
                            format!(
                                "已恢复并重新部署上一次配置（{label}）；当前账号、会话记录和个人设置保持共享。请完全退出并重新打开 Codex。"
                            )
                        } else {
                            format!(
                                "Restored and redeployed the previous configuration ({label}) while preserving the current account, tasks, and settings. Fully restart Codex."
                            )
                        }
                    } else if zh {
                        format!(
                            "已恢复并重新部署上一次配置（{label}）；检测到不同 Codex 账号，已使用该账号自己的完整快照。请完全退出并重新打开 Codex。"
                        )
                    } else {
                        format!(
                            "Restored and redeployed the previous configuration ({label}) with its own account snapshot. Fully restart Codex."
                        )
                    };
                    self.log(self.status_text.clone());
                }
                AppEvent::PreviousConfigurationRestoreError(error) => {
                    self.applying = false;
                    self.restore_pending_apply_ui_rollback();
                    let detail = localized_error_summary(zh, &error);
                    self.status_text = if zh {
                        format!("无法恢复并重新部署上一次配置：{detail}")
                    } else {
                        format!(
                            "Could not restore and redeploy the previous configuration: {detail}"
                        )
                    };
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
                        let config_path = user_data::config_path(&self.router_root);
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
                        if self.page != Page::OAuth {
                            self.schedule_usage_refresh();
                        }
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
                    if self.page == Page::OAuth {
                        self.refresh_usage_monitor();
                    }
                }
                AppEvent::OAuthAccountsError(error) => {
                    self.oauth_loading = false;
                    let detail = localized_error_summary(zh, &error);
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
                AppEvent::ProviderOAuthPrepared {
                    provider,
                    generation,
                } => {
                    if request_result_disposition(
                        self.provider_oauth_prepare_generation,
                        self.provider_oauth_preparing_provider
                            .as_deref()
                            .unwrap_or_default(),
                        generation,
                        &provider,
                    ) != RequestResultDisposition::Apply
                    {
                        continue;
                    }
                    self.provider_oauth_preparing = false;
                    self.provider_oauth_preparing_provider = None;
                    self.provider_oauth_prepared_provider = Some(provider.clone());
                    self.provider_oauth_prepare_error.clear();
                    if self.pending_oauth_provider.as_deref() == Some(provider.as_str()) {
                        self.status_text = if zh {
                            "安全登录环境已准备好；阅读完条例后即可开始 OAuth".to_owned()
                        } else {
                            "The secure sign-in environment is ready. Finish reading the terms to start OAuth"
                                .to_owned()
                        };
                    }
                }
                AppEvent::ProviderOAuthPrepareError {
                    provider,
                    generation,
                    error,
                } => {
                    if request_result_disposition(
                        self.provider_oauth_prepare_generation,
                        self.provider_oauth_preparing_provider
                            .as_deref()
                            .unwrap_or_default(),
                        generation,
                        &provider,
                    ) != RequestResultDisposition::Apply
                    {
                        continue;
                    }
                    self.provider_oauth_preparing = false;
                    self.provider_oauth_preparing_provider = None;
                    self.provider_oauth_prepared_provider = None;
                    let detail = localized_error_summary(zh, &error);
                    self.provider_oauth_prepare_error.clone_from(&detail);
                    self.status_text = if zh {
                        format!("安全登录环境准备失败：{detail}")
                    } else {
                        format!("Could not prepare the secure sign-in environment: {detail}")
                    };
                    self.log(format!(
                        "{}: {detail}",
                        if zh {
                            "OAuth 后台准备未完成"
                        } else {
                            "OAuth background preparation did not finish"
                        },
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
                }
                AppEvent::UsageLoaded {
                    profile_key,
                    generation,
                    snapshot,
                } => {
                    match request_result_disposition(
                        self.usage_request_generation,
                        &self.active_route_profile_key(),
                        generation,
                        &profile_key,
                    ) {
                        RequestResultDisposition::Ignore => continue,
                        RequestResultDisposition::RefreshCurrent => {
                            self.usage_loading = false;
                            self.schedule_usage_refresh();
                            continue;
                        }
                        RequestResultDisposition::Apply => {}
                    }
                    self.usage_loading = false;
                    self.usage_error.clear();
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
                    if !snapshot.subscriptions.is_empty() && self.router_mode_enabled {
                        self.oauth_recovery_due = Some(std::time::Instant::now());
                    }
                    if let Err(error) = self.save_usage_monitor_cache(&profile_key, &snapshot) {
                        self.log(format!(
                            "用量监控缓存保存失败：{}",
                            runtime_logs::summarize_error_for_display(&error.to_string())
                        ));
                    }
                    self.usage_snapshot = Some(snapshot);
                    self.usage_snapshot_profile_key = profile_key;
                }
                AppEvent::UsageError {
                    profile_key,
                    generation,
                    error,
                } => {
                    match request_result_disposition(
                        self.usage_request_generation,
                        &self.active_route_profile_key(),
                        generation,
                        &profile_key,
                    ) {
                        RequestResultDisposition::Ignore => continue,
                        RequestResultDisposition::RefreshCurrent => {
                            self.usage_loading = false;
                            self.schedule_usage_refresh();
                            continue;
                        }
                        RequestResultDisposition::Apply => {}
                    }
                    self.usage_loading = false;
                    let detail = usage_error_for_display(zh, &error);
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
                    self.codex_account_mode_status =
                        profiles::codex_account_mode_status(&self.router_root, &self.config);
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
                AppEvent::CodexAccountModeChanged(status) => {
                    self.codex_account_mode_switching = false;
                    self.codex_account_mode_status = status;
                    self.apply_success_dialog_open = true;
                    self.status_text = match status.mode {
                        profiles::CodexAccountMode::ApiOnly if zh => {
                            "已切换到 API 模式；第三方模型保持可用。完全退出并重开 Codex 后，官方额度提示通常会消失。"
                        }
                        profiles::CodexAccountMode::ApiOnly => {
                            "Switched to API-only mode; third-party models remain available. Fully restart Codex; the official quota banner will usually disappear."
                        }
                        profiles::CodexAccountMode::Official if zh => {
                            "已恢复官方账号登录；Codex 云端、远程控制和工作区能力可继续使用。请完全退出并重开 Codex。"
                        }
                        profiles::CodexAccountMode::Official => {
                            "Restored the official account login, including Codex cloud, remote, and workspace capabilities. Fully restart Codex."
                        }
                        _ if zh => "Codex 账号模式已更新，请完全退出并重开 Codex。",
                        _ => "Codex account mode updated. Fully restart Codex.",
                    }
                    .to_owned();
                    self.log(self.status_text.clone());
                }
                AppEvent::CodexAccountModeSwitchError(error) => {
                    self.codex_account_mode_switching = false;
                    self.codex_account_mode_status =
                        profiles::codex_account_mode_status(&self.router_root, &self.config);
                    let detail = runtime_logs::summarize_error_for_display(&error);
                    self.status_text = if zh {
                        format!("Codex 账号模式切换失败：{detail}")
                    } else {
                        format!("Could not switch the Codex account mode: {detail}")
                    };
                    self.log(self.status_text.clone());
                }
                AppEvent::OAuthRecoveryFinished(schedule) => {
                    self.oauth_recovery_running = false;
                    let delay = std::time::Duration::from_secs(
                        schedule.next_check_seconds.clamp(30, 7 * 24 * 60 * 60),
                    );
                    self.oauth_recovery_due = Some(std::time::Instant::now() + delay);
                    self.log(if zh {
                        format!(
                            "OAuth 恢复调度已更新：{}；下次检查约 {} 秒后",
                            schedule.summary,
                            delay.as_secs()
                        )
                    } else {
                        format!(
                            "OAuth recovery schedule updated: {}; next check in about {} seconds",
                            schedule.summary,
                            delay.as_secs()
                        )
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
                    if let Err(error) = self.config.save(&user_data::config_path(&self.router_root))
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
                    if self.exit_shutdown_in_progress {
                        continue;
                    }
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
                    if self.exit_shutdown_in_progress {
                        continue;
                    }
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
                AppEvent::Tray(action) if self.exit_shutdown_in_progress => {
                    let _ = action;
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
                if !self.exit_shutdown_in_progress
                    && self.router_mode_enabled
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
        if self.ui_audit_mode {
            self.status_text = "UI audit mode: log export is disabled.".to_owned();
            return;
        }
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
        self.usage_refresh_due = None;
        let profile_key = self.active_route_profile_key();
        if self.usage_snapshot_profile_key != profile_key {
            self.usage_snapshot = None;
            self.usage_snapshot_profile_key.clear();
            self.load_usage_monitor_cache();
        }
        self.page = Page::OAuth;
        if self.ui_audit_mode {
            return;
        }
        self.refresh_oauth_accounts();
    }

    fn refresh_oauth_accounts(&mut self) {
        if self.ui_audit_mode {
            return;
        }
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
        if self.ui_audit_mode {
            return;
        }
        self.refresh_usage_monitor();
    }

    fn active_route_profile_key(&self) -> String {
        if !self.active_profile_id.trim().is_empty() {
            return format!("profile:{}", self.active_profile_id.trim());
        }
        "default".to_owned()
    }

    fn usage_monitor_cache_path(&self) -> PathBuf {
        user_data::data_root(&self.router_root)
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
        if self.ui_audit_mode {
            return Ok(());
        }
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
        if self.exit_shutdown_in_progress || !self.router_mode_enabled {
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
        if self.exit_shutdown_in_progress
            || self.health_recovery_running
            || !self.router_mode_enabled
        {
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
        if self.exit_shutdown_in_progress {
            self.oauth_recovery_cancel.store(true, Ordering::Relaxed);
            self.oauth_recovery_due = None;
            return;
        }
        if !self.router_mode_enabled
            || !(self
                .config
                .oauth_account_ids
                .as_ref()
                .is_some_and(|accounts| !accounts.is_empty())
                || self
                    .config
                    .models
                    .iter()
                    .any(|model| model.source == "oauth"))
        {
            self.oauth_recovery_due = None;
            return;
        }
        let due = self
            .oauth_recovery_due
            .get_or_insert_with(|| std::time::Instant::now() + std::time::Duration::from_secs(5));
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
        self.oauth_recovery_cancel.store(false, Ordering::Relaxed);
        let cwd = self.router_root.clone();
        let tx = self.event_tx.clone();
        let cancel = self.oauth_recovery_cancel.clone();
        std::thread::spawn(move || {
            let result = run_hidden_powershell_output(
                &cwd,
                "Invoke-OAuthRecovery.ps1",
                &[],
                std::time::Duration::from_secs(15 * 60),
                &cancel,
            );
            match result {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let schedule = stdout
                        .lines()
                        .rev()
                        .find_map(|line| serde_json::from_str::<OAuthRecoverySchedule>(line).ok())
                        .unwrap_or_else(|| OAuthRecoverySchedule {
                            next_check_seconds: default_oauth_recovery_seconds(),
                            summary: "completed".to_owned(),
                        });
                    tx.send(AppEvent::OAuthRecoveryFinished(schedule)).ok();
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
        if self.ui_audit_mode {
            self.usage_loading = false;
            return;
        }
        if self.usage_loading {
            return;
        }
        self.usage_loading = true;
        let generation = next_request_generation(&mut self.usage_request_generation);
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
                        generation,
                        snapshot: Box::new(snapshot),
                    })
                    .ok();
                }
                Err(error) => {
                    tx.send(AppEvent::UsageError {
                        profile_key,
                        generation,
                        error: error.to_string(),
                    })
                    .ok();
                }
            }
        });
    }

    fn import_grok_sso(&mut self) {
        if self.ui_audit_mode {
            self.grok_sso_error = "UI audit mode: import is disabled.".to_owned();
            return;
        }
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
        if self.ui_audit_mode {
            self.oauth_error = "UI audit mode: account revocation is disabled.".to_owned();
            return;
        }
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
        if self.ui_audit_mode {
            self.status_text = "UI audit mode: Router switching is disabled.".to_owned();
            return;
        }
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
        if !self.apply_all_with_backup(true, None, None) {
            self.router_mode_switching = false;
        }
    }

    fn disable_router_mode(&mut self) {
        if self.ui_audit_mode {
            self.status_text = "UI audit mode: Router switching is disabled.".to_owned();
            return;
        }
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

    fn switch_codex_account_mode(&mut self, target: profiles::CodexAccountMode) {
        if self.ui_audit_mode {
            self.status_text = "UI audit mode: account switching is disabled.".to_owned();
            return;
        }
        if self.codex_account_mode_switching || self.applying || self.router_mode_switching {
            return;
        }
        if target == self.codex_account_mode_status.mode {
            return;
        }
        if target == profiles::CodexAccountMode::ApiOnly
            && (!self.router_mode_enabled || !logic::codex_router_mode_active(&self.config))
        {
            self.report_error(if self.ui_language == "zh" {
                "请先开启 Codex 本地 Router 路由，再切换到 API 模式"
            } else {
                "Enable the local Codex Router route before switching to API-only mode"
            });
            return;
        }
        self.codex_account_mode_switching = true;
        self.status_text = if self.ui_language == "zh" {
            "正在校验加密快照并切换 Codex 账号模式…"
        } else {
            "Verifying the encrypted snapshot and switching the Codex account mode…"
        }
        .to_owned();
        self.log(self.status_text.clone());
        let root = self.router_root.clone();
        let config = self.config.clone();
        let tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<profiles::CodexAccountModeStatus> {
                let _lock =
                    profiles::acquire_config_apply_lock(&root, std::time::Duration::from_secs(10))?;
                match target {
                    profiles::CodexAccountMode::ApiOnly => {
                        profiles::switch_to_api_only_mode(&root, &config)?
                    }
                    profiles::CodexAccountMode::Official => {
                        profiles::restore_official_account_mode(&root, &config)?
                    }
                    profiles::CodexAccountMode::Unavailable => {
                        anyhow::bail!("Unsupported Codex account mode")
                    }
                }
                Ok(profiles::codex_account_mode_status(&root, &config))
            })();
            match result {
                Ok(status) => {
                    tx.send(AppEvent::CodexAccountModeChanged(status)).ok();
                }
                Err(error) => {
                    tx.send(AppEvent::CodexAccountModeSwitchError(error.to_string()))
                        .ok();
                }
            }
        });
    }

    fn apply_all(&mut self) {
        if self.ui_audit_mode {
            self.status_text = "UI audit mode: configuration apply is disabled.".to_owned();
            return;
        }
        let active_profile_id =
            (!self.active_profile_id.trim().is_empty()).then(|| self.active_profile_id.clone());
        if self.apply_all_with_backup(true, None, None) && self.pending_profile_activation.is_none()
        {
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
        transaction_backup: Option<ApplyTransactionBackup>,
    ) -> bool {
        if self.applying || self.exit_shutdown_in_progress {
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
        logic::normalize_default_model(&mut self.config);
        self.apply_cancel.store(false, Ordering::Release);
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
        let tx = self.event_tx.clone();
        let apply_cancel = self.apply_cancel.clone();
        let share_codex_state = self.share_codex_state;
        for model in &mut self.config.models {
            model.api_key.clear();
        }
        self.config.proxy.password.clear();
        std::thread::spawn(move || {
            let mut transaction_backup = transaction_backup;
            let result = (|| -> anyhow::Result<()> {
                if apply_cancel.load(Ordering::Acquire) {
                    anyhow::bail!("deployment cancelled because Codex-Router is exiting");
                }
                let _config_lock = match config_lock {
                    Some(lock) => lock,
                    None => profiles::acquire_config_apply_lock(
                        &root,
                        std::time::Duration::from_secs(10),
                    )?,
                };
                if apply_cancel.load(Ordering::Acquire) {
                    anyhow::bail!("deployment cancelled because Codex-Router is exiting");
                }
                profiles::ensure_original_codex_snapshot(&root, &cfg)?;
                if capture_before_apply {
                    let (point, config) = profiles::capture_applied_restore_point(
                        &root,
                        &cfg,
                        "应用 Router 配置之前",
                    )?;
                    transaction_backup = Some(ApplyTransactionBackup { point, config });
                }
                deploy_router_config(&mut cfg, &root, &apply_cancel, zh, |line| {
                    tx.send(AppEvent::Log(line)).ok();
                })
            })();
            match result {
                Ok(()) => {
                    tx.send(AppEvent::Complete).ok();
                }
                Err(error) => {
                    let error = if apply_cancel.load(Ordering::Acquire) {
                        error
                    } else if let Some(backup) = transaction_backup.as_ref() {
                        match rollback_failed_deployment(
                            &root,
                            backup,
                            share_codex_state,
                            zh,
                            |line| {
                                tx.send(AppEvent::Log(line)).ok();
                            },
                        ) {
                            Ok(()) => error.context("已自动恢复应用前配置"),
                            Err(rollback_error) => {
                                error.context(format!("自动恢复应用前配置失败：{rollback_error}"))
                            }
                        }
                    } else {
                        error
                    };
                    tx.send(AppEvent::Error(error.to_string())).ok();
                }
            }
        });
        true
    }

    fn run_script_hidden(&self, relative: &str) {
        if self.ui_audit_mode {
            return;
        }
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
        if self.ui_audit_mode {
            self.status_text = "UI audit mode: update checks are disabled.".to_owned();
            return;
        }
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
        if self.ui_audit_mode {
            self.status_text = "UI audit mode: downloads are disabled.".to_owned();
            return;
        }
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
        if self.ui_audit_mode {
            return;
        }
        let url = if requested_url.starts_with(OFFICIAL_GITHUB_URL) {
            requested_url
        } else {
            OFFICIAL_GITHUB_URL
        };
        let _ = std::process::Command::new("explorer.exe").arg(url).spawn();
    }

    fn open_download_location(&self, requested_path: &str) {
        if self.ui_audit_mode {
            return;
        }
        let path = PathBuf::from(requested_path);
        if requested_path.is_empty() || !path.starts_with(self.router_root.join("updates")) {
            return;
        }
        let _ = std::process::Command::new("explorer.exe")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }

    fn open_sub2api_accounts(&self) {
        if self.ui_audit_mode {
            return;
        }
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
        if self.ui_audit_mode {
            self.status_text = "UI audit mode: OAuth launch is disabled.".to_owned();
            return;
        }
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
            self.terms_scroll_reset_pending = true;
            self.prewarm_provider_oauth(provider);
            self.status_text = if self.ui_language == "zh" {
                "首次 OAuth 前请先阅读并同意本地部署与使用条例；登录环境正在后台准备".to_owned()
            } else {
                "Read and accept the local deployment and use terms before the first OAuth login. The sign-in environment is preparing in the background".to_owned()
            };
            return;
        }
        self.launch_provider_oauth(provider);
    }

    fn prewarm_provider_oauth(&mut self, provider: &str) {
        if self.ui_audit_mode || self.provider_oauth_running {
            return;
        }
        if self.provider_oauth_prepared_provider.as_deref() == Some(provider) {
            if self.provider_oauth_preparing {
                self.cancel_provider_oauth_preparation(false);
            }
            return;
        }
        if self.provider_oauth_preparing
            && self.provider_oauth_preparing_provider.as_deref() == Some(provider)
        {
            return;
        }

        self.provider_oauth_prepare_cancel
            .store(true, Ordering::Release);
        let generation = next_request_generation(&mut self.provider_oauth_prepare_generation);
        let cancel = Arc::new(AtomicBool::new(false));
        self.provider_oauth_prepare_cancel = cancel.clone();
        let cwd = self.router_root.clone();
        let provider = provider.to_owned();
        let event_provider = provider.clone();
        let tx = self.event_tx.clone();
        self.provider_oauth_preparing = true;
        self.provider_oauth_preparing_provider = Some(provider.clone());
        self.provider_oauth_prepared_provider = None;
        self.provider_oauth_prepare_error.clear();
        std::thread::spawn(move || {
            let arguments = ["-Provider", provider.as_str(), "-PrepareOnly"];
            let mut final_error = "ROUTER_OAUTH_PREPARE_PROCESS stage=unknown".to_owned();
            for attempt in 0..3 {
                match run_hidden_powershell_output(
                    &cwd,
                    "Start-ProviderOAuth.ps1",
                    &arguments,
                    std::time::Duration::from_secs(300),
                    &cancel,
                ) {
                    Ok(output) if output.status.success() => {
                        tx.send(AppEvent::ProviderOAuthPrepared {
                            provider: event_provider,
                            generation,
                        })
                        .ok();
                        return;
                    }
                    Ok(output) => final_error = oauth_prepare_error_from_output(&output),
                    Err(error) => {
                        final_error = if error.to_string().contains("time budget") {
                            "ROUTER_OAUTH_PREPARE_TIMEOUT stage=router_start".to_owned()
                        } else {
                            "ROUTER_OAUTH_PREPARE_PROCESS stage=launcher".to_owned()
                        };
                    }
                }
                if attempt < 2 && oauth_prepare_error_is_retryable(&final_error) {
                    std::thread::sleep(std::time::Duration::from_secs(1 << attempt));
                    continue;
                }
                break;
            }
            tx.send(AppEvent::ProviderOAuthPrepareError {
                provider: event_provider,
                generation,
                error: final_error,
            })
            .ok();
        });
    }

    fn cancel_provider_oauth_preparation(&mut self, clear_prepared: bool) {
        self.provider_oauth_prepare_cancel
            .store(true, Ordering::Release);
        next_request_generation(&mut self.provider_oauth_prepare_generation);
        self.provider_oauth_prepare_cancel = Arc::new(AtomicBool::new(false));
        self.provider_oauth_preparing = false;
        self.provider_oauth_preparing_provider = None;
        self.provider_oauth_prepare_error.clear();
        if clear_prepared {
            self.provider_oauth_prepared_provider = None;
        }
    }

    fn continue_provider_oauth_after_terms(&mut self, provider: String) {
        if !self.provider_oauth_preparing
            && self.provider_oauth_prepared_provider.as_deref() == Some(provider.as_str())
        {
            self.launch_provider_oauth(&provider);
            return;
        }

        self.pending_oauth_provider = Some(provider.clone());
        self.terms_open = true;
        self.prewarm_provider_oauth(&provider);
        self.status_text = if self.ui_language == "zh" {
            "安全登录环境尚未准备完成；准备成功后才能开始 OAuth".to_owned()
        } else {
            "The secure sign-in environment is not ready yet. OAuth can start after preparation succeeds"
                .to_owned()
        };
    }

    fn launch_provider_oauth(&mut self, provider: &str) {
        let script = self
            .router_root
            .join("scripts")
            .join("Start-ProviderOAuth.ps1");
        let cwd = self.router_root.clone();
        let provider = provider.to_owned();
        let interactive_console = matches!(provider.as_str(), "anthropic" | "gemini");
        let tx = self.event_tx.clone();
        let cancel = self.provider_oauth_cancel.clone();
        cancel.store(false, Ordering::Relaxed);
        self.provider_oauth_running = true;
        self.status_text = if self.ui_language == "zh" {
            format!("正在打开 {provider} 官方授权页；请按新窗口提示完成登录")
        } else {
            format!("Opening the official {provider} authorization page. Follow the new window")
        };
        std::thread::spawn(move || {
            match run_provider_oauth_process(&cwd, &script, &provider, interactive_console, &cancel)
            {
                Ok(()) => {
                    tx.send(AppEvent::ProviderOAuthFinished).ok();
                }
                Err(error) => {
                    tx.send(AppEvent::ProviderOAuthError(format!(
                        "{provider} OAuth process did not complete: {error}"
                    )))
                    .ok();
                }
            }
        });
    }
}

impl Drop for CodexRouterApp {
    fn drop(&mut self) {
        self.apply_cancel.store(true, Ordering::Release);
        self.runtime_log_stop.store(true, Ordering::Relaxed);
        self.health_recovery_cancel.store(true, Ordering::Relaxed);
        self.provider_oauth_prepare_cancel
            .store(true, Ordering::Relaxed);
        self.provider_oauth_cancel.store(true, Ordering::Relaxed);
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
                self.temp_model.priority = logic::next_api_channel_priority(&self.config);
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
                "4 / 5  网络代理"
            } else {
                "网络代理"
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
        ui.label("将自动初始化本地运行环境、配置真实 Sub2API 渠道并写入 Codex。网络代理为可选项。");
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
            if ui.button("网络代理设置").clicked() {
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
            self.temp_model.priority = logic::next_api_channel_priority(&self.config);
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

fn run_provider_oauth_process(
    router_root: &Path,
    script: &Path,
    provider: &str,
    interactive_console: bool,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    let mut command = std::process::Command::new("powershell.exe");
    command.args(["-NoLogo", "-NoProfile"]);
    if !interactive_console {
        command.arg("-NonInteractive");
    }
    command
        .args(["-ExecutionPolicy", "Bypass", "-File"])
        .arg(script)
        .args(["-Provider", provider])
        .arg("-ComplianceAccepted")
        .current_dir(router_root);
    if interactive_console {
        command.creation_flags(CREATE_NEW_CONSOLE);
    } else {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("could not start {provider} OAuth"))?;
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("cancelled because Codex-Router is exiting");
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => anyhow::bail!("exited with {status}"),
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(200)),
            Err(error) => anyhow::bail!("could not monitor the OAuth process: {error}"),
        }
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

fn run_hidden_powershell_output(
    router_root: &Path,
    script_name: &str,
    arguments: &[&str],
    timeout: std::time::Duration,
    cancel: &AtomicBool,
) -> anyhow::Result<std::process::Output> {
    let script = router_root.join("scripts").join(script_name);
    let mut child = std::process::Command::new("powershell.exe")
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .with_context(|| format!("could not start {script_name}"))?;

    let stdout = child
        .stdout
        .take()
        .with_context(|| format!("{script_name} did not expose stdout"))?;
    let stderr = child
        .stderr
        .take()
        .with_context(|| format!("{script_name} did not expose stderr"))?;
    let stdout_drain = spawn_output_drain(stdout);
    let stderr_drain = spawn_output_drain(stderr);
    let started = std::time::Instant::now();
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            terminate_child_process_tree(&mut child);
            anyhow::bail!("{script_name} was cancelled because Codex-Router is exiting");
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Ok(None) => {
                terminate_child_process_tree(&mut child);
                anyhow::bail!("{script_name} exceeded its time budget");
            }
            Err(error) => {
                terminate_child_process_tree(&mut child);
                return Err(error).with_context(|| format!("could not monitor {script_name}"));
            }
        }
    };
    let stdout = finish_output_drain(stdout_drain, "background helper output")?;
    let stderr = finish_output_drain(stderr_drain, "background helper error output")?;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
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
fn exit_marker_matches_executable(executable: &Path, candidate_pid: u32) -> bool {
    let Some(router_root) = executable.parent() else {
        return false;
    };
    let marker = user_data::data_root(router_root)
        .join("pids")
        .join("gui-exit.pid");
    std::fs::read_to_string(marker)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        == Some(candidate_pid)
}

#[cfg(windows)]
unsafe fn process_has_exit_transaction_marker(
    process: windows_sys::Win32::Foundation::HANDLE,
    candidate_pid: u32,
) -> bool {
    use windows_sys::Win32::System::Threading::QueryFullProcessImageNameW;

    let mut path = vec![0_u16; 32_768];
    let mut path_len = path.len() as u32;
    if QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut path_len) == 0 {
        return false;
    }
    path.truncate(path_len as usize);
    let executable = PathBuf::from(String::from_utf16_lossy(&path));
    exit_marker_matches_executable(&executable, candidate_pid)
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
        WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
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
        let wait = WaitForSingleObject(
            mutex,
            STARTUP_TAKEOVER_MUTEX_TIMEOUT
                .as_millis()
                .min(u32::MAX as u128) as u32,
        );
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
                        PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE_ACCESS,
                        0,
                        entry.th32ProcessID,
                    );
                    if !process.is_null() {
                        if process_has_exit_transaction_marker(process, entry.th32ProcessID)
                            && WaitForSingleObject(
                                process,
                                EXIT_TAKEOVER_GRACE.as_millis().min(u32::MAX as u128) as u32,
                            ) == WAIT_OBJECT_0
                        {
                            CloseHandle(process);
                            has_entry = Process32NextW(snapshot, &mut entry) != 0;
                            continue;
                        }
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
    let ui_audit = UiAuditOptions::from_args();
    #[cfg(windows)]
    if ui_audit.is_none() {
        take_over_older_gui_instances();
    }
    let start_in_background =
        std::env::args_os().any(|argument| argument == "--background" || argument == "--watchdog");
    let compact = ui_audit.as_ref().is_some_and(|options| options.compact);
    // Keep every size in logical points and leave pixels_per_point unset. With
    // the PerMonitorV2 manifest, winit forwards live scale-factor changes to
    // eframe/egui whenever the window moves between monitors.
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(initial_window_logical_size(compact))
            .with_min_inner_size(MIN_WINDOW_LOGICAL_SIZE)
            .with_resizable(true)
            .with_clamp_size_to_monitor_size(true)
            .with_visible(!start_in_background)
            .with_icon(window_icon()),
        centered: true,
        renderer: eframe::Renderer::Glow,
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native(
        APP_TITLE,
        options,
        Box::new(move |cc| {
            Ok(Box::new(CodexRouterApp::new(
                cc,
                start_in_background,
                ui_audit,
            )))
        }),
    )
}

#[cfg(test)]
mod main_tests {
    use super::{
        append_bounded_log, decode_icon, drain_bounded_output, fit_window_to_monitor,
        fit_window_to_work_area, initial_window_logical_size, localized_deployment_line,
        localized_error_summary, next_request_generation, oauth_prepare_error_from_output,
        oauth_prepare_error_is_retryable, request_result_disposition, restore_apply_ui_fields,
        restore_codex_and_stop_router_for_exit, restore_codex_for_exit, usage_error_for_display,
        ApplyUiRollback, RequestResultDisposition, RouterConfig, APP_TITLE, APP_VERSION,
        COMPACT_WINDOW_LOGICAL_SIZE, DEFAULT_WINDOW_LOGICAL_SIZE, EXIT_HELPER_OUTPUT_LIMIT,
        MAX_LOG_BYTES, MIN_WINDOW_LOGICAL_SIZE, RETAIN_LOG_BYTES,
        WINDOWS_1080P_200_WORK_AREA_LOGICAL_SIZE, WINDOWS_NON_CLIENT_LOGICAL_ALLOWANCE,
    };

    #[test]
    fn native_window_title_contains_the_exact_package_version() {
        assert_eq!(APP_TITLE, format!("Codex-Router v{APP_VERSION}"));
    }

    #[test]
    fn native_window_policy_fits_a_1080p_work_area_at_200_percent() {
        for preferred in [DEFAULT_WINDOW_LOGICAL_SIZE, COMPACT_WINDOW_LOGICAL_SIZE] {
            let size = fit_window_to_work_area(preferred, WINDOWS_1080P_200_WORK_AREA_LOGICAL_SIZE);
            assert!(MIN_WINDOW_LOGICAL_SIZE[0] <= preferred[0]);
            assert!(MIN_WINDOW_LOGICAL_SIZE[1] <= preferred[1]);
            assert!(
                size[0] + WINDOWS_NON_CLIENT_LOGICAL_ALLOWANCE[0]
                    <= WINDOWS_1080P_200_WORK_AREA_LOGICAL_SIZE[0]
            );
            assert!(
                size[1] + WINDOWS_NON_CLIENT_LOGICAL_ALLOWANCE[1]
                    <= WINDOWS_1080P_200_WORK_AREA_LOGICAL_SIZE[1]
            );
        }
        let normal = initial_window_logical_size(false);
        let compact = initial_window_logical_size(true);
        assert!(normal[0] > 0.0 && normal[0] <= DEFAULT_WINDOW_LOGICAL_SIZE[0]);
        assert!(normal[1] > 0.0 && normal[1] <= DEFAULT_WINDOW_LOGICAL_SIZE[1]);
        assert!(compact[0] > 0.0 && compact[0] <= COMPACT_WINDOW_LOGICAL_SIZE[0]);
        assert!(compact[1] > 0.0 && compact[1] <= COMPACT_WINDOW_LOGICAL_SIZE[1]);
        assert_eq!(
            fit_window_to_work_area(DEFAULT_WINDOW_LOGICAL_SIZE, [1920.0, 1032.0]),
            DEFAULT_WINDOW_LOGICAL_SIZE
        );
    }

    #[test]
    fn cross_screen_move_clamps_only_when_the_target_monitor_is_smaller() {
        assert_eq!(
            fit_window_to_monitor([1280.0, 820.0], [1920.0, 1080.0]),
            [1280.0, 820.0]
        );
        assert_eq!(
            fit_window_to_monitor([1280.0, 820.0], [960.0, 540.0]),
            [944.0, 452.0]
        );
        assert_eq!(
            fit_window_to_monitor([800.0, 400.0], [960.0, 540.0]),
            [800.0, 400.0]
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_resource_embeds_per_monitor_v2_dpi_awareness() {
        let manifest = include_str!(concat!(env!("OUT_DIR"), "/codex-router.manifest"));
        assert!(manifest.contains(
            "<dpiAwareness xmlns=\"http://schemas.microsoft.com/SMI/2016/WindowsSettings\">PerMonitorV2, PerMonitor</dpiAwareness>"
        ));
        assert!(manifest.contains(
            "<dpiAware xmlns=\"http://schemas.microsoft.com/SMI/2005/WindowsSettings\">true/pm</dpiAware>"
        ));
        assert!(manifest.contains("requestedExecutionLevel level=\"asInvoker\""));

        let resource = include_str!(concat!(env!("OUT_DIR"), "/codex-router.rc"));
        assert!(resource.lines().any(|line| line.starts_with("1 24 \"")));
    }

    #[cfg(windows)]
    use super::{
        exit_marker_matches_executable, should_take_over_process, stop_router_for_exit,
        stop_router_for_exit_with_timeout, ExitTransactionMarker,
    };

    #[cfg(windows)]
    #[test]
    fn only_an_older_router_gui_is_selected_for_takeover() {
        assert!(should_take_over_process(20, 10, "Codex-Router.exe"));
        assert!(should_take_over_process(20, 10, "codex-router.EXE"));
        assert!(!should_take_over_process(20, 20, "Codex-Router.exe"));
        assert!(!should_take_over_process(20, 10, "sub2api.exe"));
    }

    #[cfg(windows)]
    #[test]
    fn exit_transaction_marker_is_pid_scoped_and_removed_on_drop() {
        let root = std::env::temp_dir().join(format!(
            "codex-router-exit-marker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let executable = root.join("Codex-Router.exe");
        std::fs::create_dir_all(&root).unwrap();
        let marker = ExitTransactionMarker::create(&root).unwrap();
        assert!(exit_marker_matches_executable(
            &executable,
            std::process::id()
        ));
        assert!(!exit_marker_matches_executable(
            &executable,
            std::process::id().saturating_add(1)
        ));
        let marker_path = marker.path.clone();
        drop(marker);
        assert!(!marker_path.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn exit_transaction_marker_creation_failure_is_reported() {
        let root = std::env::temp_dir().join(format!(
            "codex-router-exit-marker-failure-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("data"), b"not a directory").unwrap();
        assert!(ExitTransactionMarker::create(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shutdown_helper_output_is_drained_and_tail_bounded() {
        let input = (0..EXIT_HELPER_OUTPUT_LIMIT + 4096)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let retained = drain_bounded_output(std::io::Cursor::new(&input)).unwrap();
        assert_eq!(retained.len(), EXIT_HELPER_OUTPUT_LIMIT);
        assert_eq!(
            retained,
            input[input.len() - EXIT_HELPER_OUTPUT_LIMIT..].to_vec()
        );
    }

    #[cfg(windows)]
    #[test]
    fn shutdown_helper_large_output_does_not_deadlock() {
        let root = std::env::temp_dir().join(format!(
            "codex-router-exit-output-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let scripts = root.join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(
            scripts.join("Stop-Router.ps1"),
            "param([switch]$Force,[switch]$AdoptActivePortableOwner)\n\
             [Console]::Out.Write(('x' * 1048576))\nexit 0\n",
        )
        .unwrap();

        let started = std::time::Instant::now();
        stop_router_for_exit(&root).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn shutdown_timeout_terminates_the_helper_process_tree() {
        let root = std::env::temp_dir().join(format!(
            "codex-router-exit-tree-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let scripts = root.join("scripts");
        let child_started = root.join("child-started.txt");
        let child_survived = root.join("child-survived.txt");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(
            scripts.join("Exit-Tree-Child.ps1"),
            format!(
                "Set-Content -LiteralPath '{}' -Value started\n\
                 Start-Sleep -Seconds 2\n\
                 Set-Content -LiteralPath '{}' -Value survived\n",
                child_started.display(),
                child_survived.display()
            ),
        )
        .unwrap();
        std::fs::write(
            scripts.join("Stop-Router.ps1"),
            "param([switch]$Force,[switch]$AdoptActivePortableOwner)\n\
             $child = Join-Path $PSScriptRoot 'Exit-Tree-Child.ps1'\n\
             Start-Process powershell.exe -ArgumentList @('-NoProfile','-File',$child) -WindowStyle Hidden\n\
             Start-Sleep -Seconds 30\n",
        )
        .unwrap();

        let started = std::time::Instant::now();
        let result =
            stop_router_for_exit_with_timeout(&root, std::time::Duration::from_millis(1500));
        assert!(result.is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(
            child_started.exists(),
            "the descendant test process never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(2200));
        assert!(
            !child_survived.exists(),
            "the timed-out shutdown helper left a descendant process running"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn small_system_icon_uses_most_of_its_canvas() {
        let (rgba, width, height) = decode_icon().expect("embedded icon should decode");
        assert_eq!(width, height);
        assert_eq!(rgba.len(), width as usize * height as usize * 4);

        let mut min_x = width;
        let mut max_x = 0;
        for (index, pixel) in rgba.chunks_exact(4).enumerate() {
            if pixel[3] >= 16 {
                let x = index as u32 % width;
                min_x = min_x.min(x);
                max_x = max_x.max(x);
            }
        }
        let occupied_width = max_x.saturating_sub(min_x).saturating_add(1);
        assert!(occupied_width as f32 / width as f32 >= 0.9);
    }

    #[test]
    fn full_exit_restores_the_codex_route_used_before_router() {
        let root = std::env::temp_dir().join(format!(
            "codex-router-exit-restore-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let codex_home = root.join("codex-home");
        std::fs::create_dir_all(&codex_home).unwrap();
        let mut config = RouterConfig::default();
        config.deploy.codex_home = codex_home.to_string_lossy().into_owned();
        std::fs::write(
            codex_home.join("config.toml"),
            "model = \"official-before-router\"\napproval_policy = \"never\"\n",
        )
        .unwrap();
        super::profiles::ensure_original_codex_snapshot(&root, &config).unwrap();
        std::fs::write(
            codex_home.join("config.toml"),
            "model_provider = \"sub2api\"\nmodel = \"router-model\"\n\
             [model_providers.sub2api]\nname = \"Codex-Router\"\nbase_url = \"http://127.0.0.1:18080/v1\"\n",
        )
        .unwrap();

        restore_codex_for_exit(&root, &config, true, true).unwrap();
        let restored = std::fs::read_to_string(codex_home.join("config.toml")).unwrap();
        assert!(restored.contains("official-before-router"));
        assert!(!restored.contains("sub2api"));
        assert!(!restored.contains("router-model"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exit_waits_for_an_inflight_apply_before_deciding_whether_to_restore() {
        let root = std::env::temp_dir().join(format!(
            "codex-router-exit-apply-race-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let codex_home = root.join("codex-home");
        let scripts = root.join("scripts");
        std::fs::create_dir_all(&codex_home).unwrap();
        std::fs::create_dir_all(&scripts).unwrap();
        let mut config = RouterConfig::default();
        config.deploy.codex_home = codex_home.to_string_lossy().into_owned();
        config.deploy.sub2api_host = "http://127.0.0.1:18080".into();
        config.save(&root.join("codex-router-config.json")).unwrap();
        std::fs::write(
            codex_home.join("config.toml"),
            "model = \"official-before-router\"\napproval_policy = \"never\"\n",
        )
        .unwrap();
        super::profiles::ensure_original_codex_snapshot(&root, &config).unwrap();
        std::fs::write(
            scripts.join("Stop-Router.ps1"),
            "param([switch]$Force,[switch]$AdoptActivePortableOwner)\nexit 0\n",
        )
        .unwrap();

        let apply_lock =
            super::profiles::acquire_config_apply_lock(&root, std::time::Duration::from_secs(1))
                .unwrap();
        let worker_root = root.clone();
        let exit =
            std::thread::spawn(move || restore_codex_and_stop_router_for_exit(&worker_root, true));
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::write(
            codex_home.join("config.toml"),
            "model_provider = \"custom\"\nmodel = \"router-model\"\n\
             [model_providers.custom]\nbase_url = \"http://127.0.0.1:18080/v1\"\n",
        )
        .unwrap();
        drop(apply_lock);

        exit.join().unwrap().unwrap();
        let restored = std::fs::read_to_string(codex_home.join("config.toml")).unwrap();
        assert!(restored.contains("official-before-router"));
        assert!(!restored.contains("router-model"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn exit_still_stops_router_when_codex_restore_input_is_invalid() {
        let root = std::env::temp_dir().join(format!(
            "codex-router-exit-invalid-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let scripts = root.join("scripts");
        let stopped = root.join("stop-called.txt");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(root.join("codex-router-config.json"), b"{invalid-json").unwrap();
        std::fs::write(
            scripts.join("Stop-Router.ps1"),
            format!(
                "param([switch]$Force,[switch]$AdoptActivePortableOwner)\n\
                 Set-Content -LiteralPath '{}' -Value stopped\n",
                stopped.display()
            ),
        )
        .unwrap();

        let result = restore_codex_and_stop_router_for_exit(&root, true);
        assert!(
            result.is_err(),
            "the invalid restore input must still be reported"
        );
        assert!(
            stopped.exists(),
            "a restore failure skipped the mandatory Router shutdown"
        );
        std::fs::remove_dir_all(root).unwrap();
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
            usage_error_for_display(true, "class=connection_refused | status=503"),
            "本地 Router 正在启动或恢复，请稍后重新查询。"
        );
        assert_eq!(
            usage_error_for_display(false, "unsafe\napi_key=must-not-survive"),
            "The usage query temporarily failed. The last successful data is retained; retry shortly."
        );
        assert!(!usage_error_for_display(true, "class=configuration").contains("class="));
    }

    #[test]
    fn failed_profile_apply_restores_every_field_needed_for_a_safe_retry() {
        let mut config = RouterConfig {
            auth_mode: "profile-b".to_owned(),
            ..RouterConfig::default()
        };
        let mut active_profile_id = "profile-a".to_owned();
        let mut pending_profile_activation = Some("profile-b".to_owned());
        let mut configured = false;
        let mut router_mode_enabled = false;

        let previous_config = RouterConfig {
            auth_mode: "profile-a".to_owned(),
            ..RouterConfig::default()
        };
        restore_apply_ui_fields(
            &mut config,
            &mut active_profile_id,
            &mut pending_profile_activation,
            &mut configured,
            &mut router_mode_enabled,
            ApplyUiRollback {
                config: previous_config,
                active_profile_id: "profile-a".to_owned(),
                pending_profile_activation: None,
                configured: true,
                router_mode_enabled: true,
            },
        );

        assert_eq!(config.auth_mode, "profile-a");
        assert_eq!(active_profile_id, "profile-a");
        assert_eq!(pending_profile_activation, None);
        assert!(configured);
        assert!(router_mode_enabled);
    }

    #[test]
    fn oauth_prepare_results_are_scoped_to_provider_and_generation() {
        let current_generation = 42;
        assert_eq!(
            request_result_disposition(current_generation, "openai", 41, "grok"),
            RequestResultDisposition::Ignore
        );
        assert_eq!(
            request_result_disposition(current_generation, "openai", 42, "grok"),
            RequestResultDisposition::RefreshCurrent
        );
        assert_eq!(
            request_result_disposition(current_generation, "openai", 42, "openai"),
            RequestResultDisposition::Apply
        );

        let mut closed_generation = current_generation;
        next_request_generation(&mut closed_generation);
        assert_eq!(
            request_result_disposition(closed_generation, "", 42, "openai"),
            RequestResultDisposition::Ignore
        );
    }

    #[test]
    fn oauth_prepare_errors_keep_only_safe_structured_stage_codes() {
        use std::os::windows::process::ExitStatusExt;

        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(1),
            stdout: br#"{"status":"error","provider":"openai","stage":"admin_login","code":"ROUTER_OAUTH_PREPARE_ADMIN_LOGIN","unsafe":"token-value"}"#
                .to_vec(),
            stderr: Vec::new(),
        };
        let safe = oauth_prepare_error_from_output(&output);
        assert_eq!(safe, "ROUTER_OAUTH_PREPARE_ADMIN_LOGIN stage=admin_login");
        assert!(!safe.contains("token-value"));
        assert!(localized_error_summary(true, &safe).contains("管理会话"));
        assert!(localized_error_summary(false, &safe).contains("admin session"));
    }

    #[test]
    fn lifecycle_busy_is_the_only_automatically_retried_prepare_failure() {
        assert!(oauth_prepare_error_is_retryable(
            "ROUTER_OAUTH_PREPARE_LIFECYCLE_BUSY stage=lifecycle_lock"
        ));
        assert!(!oauth_prepare_error_is_retryable(
            "ROUTER_OAUTH_PREPARE_ADMIN_LOGIN stage=admin_login"
        ));
    }

    #[test]
    fn usage_results_from_profile_a_cannot_mutate_profile_b() {
        assert_eq!(
            request_result_disposition(8, "profile:b", 7, "profile:a"),
            RequestResultDisposition::Ignore,
            "an older A result must not clear B's loading state"
        );
        assert_eq!(
            request_result_disposition(8, "profile:b", 8, "profile:a"),
            RequestResultDisposition::RefreshCurrent,
            "a current request whose profile changed must schedule B"
        );
        assert_eq!(
            request_result_disposition(8, "profile:b", 8, "profile:b"),
            RequestResultDisposition::Apply
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
