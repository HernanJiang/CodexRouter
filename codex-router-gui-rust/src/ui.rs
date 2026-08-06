use super::{
    ensure_full_ui_fonts, theme, CloseBehavior, CodexRouterApp, IsolationKind, IsolationProfile,
    ModelConfig, OAuthAccountSummary, Page, UsageAccount, UsageWindow, APP_VERSION,
    OFFICIAL_GITHUB_URL,
};
use eframe::egui;

const TERMS_ZH: &str = include_str!("../../TERMS.zh-CN.md");
const TERMS_EN: &str = include_str!("../../TERMS.en.md");
const TOPBAR_CONTROL_WIDTH: f32 = 112.0;
const TOPBAR_CONTROL_HEIGHT: f32 = 48.0;
const COMPACT_TOPBAR_CONTROL_WIDTH: f32 = 92.0;
const COMPACT_TOPBAR_CONTROL_HEIGHT: f32 = 42.0;
const DIALOG_VIEWPORT_GUTTER: f32 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq)]
struct ShellMetrics {
    topbar_height: f32,
    topbar_horizontal_margin: i8,
    topbar_vertical_margin: i8,
    content_top_space: f32,
    footer_height: f32,
}

fn shell_metrics(viewport_height: f32) -> ShellMetrics {
    if viewport_height < 700.0 {
        ShellMetrics {
            topbar_height: 80.0,
            topbar_horizontal_margin: 12,
            topbar_vertical_margin: 10,
            content_top_space: 12.0,
            footer_height: 24.0,
        }
    } else {
        ShellMetrics {
            topbar_height: 104.0,
            topbar_horizontal_margin: 28,
            topbar_vertical_margin: 18,
            content_top_space: 24.0,
            footer_height: 44.0,
        }
    }
}

fn topbar_control_size(viewport_width: f32, viewport_height: f32) -> egui::Vec2 {
    if viewport_width < 1060.0 || viewport_height < 700.0 {
        egui::vec2(COMPACT_TOPBAR_CONTROL_WIDTH, COMPACT_TOPBAR_CONTROL_HEIGHT)
    } else {
        egui::vec2(TOPBAR_CONTROL_WIDTH, TOPBAR_CONTROL_HEIGHT)
    }
}

fn fit_dialog_size(viewport: egui::Vec2, desired: egui::Vec2, minimum: egui::Vec2) -> egui::Vec2 {
    let maximum = egui::vec2(
        (viewport.x - DIALOG_VIEWPORT_GUTTER).max(240.0),
        (viewport.y - DIALOG_VIEWPORT_GUTTER).max(220.0),
    );
    egui::vec2(
        desired.x.max(minimum.x).min(maximum.x).min(viewport.x),
        desired.y.max(minimum.y).min(maximum.y).min(viewport.y),
    )
}

fn page_scroll_min_height(page: Page) -> Option<f32> {
    match page {
        Page::Welcome | Page::Project | Page::Auth | Page::Model | Page::Proxy | Page::Finish => {
            Some(540.0)
        }
        // Monitor packs its own header + scrollable card grid; forcing a tall
        // min height here only leaves a large empty band under the cards.
        Page::Profiles => Some(620.0),
        Page::Monitor => None,
        Page::OAuth => Some(720.0),
        Page::Dashboard => None,
    }
}

fn dashboard_uses_wide_layout(available_width: f32, available_height: f32) -> bool {
    available_width >= 980.0 && available_height >= 500.0
}

fn usage_monitor_uses_two_columns(available_width: f32) -> bool {
    available_width >= 920.0
}

fn paint_chiral_mark(ui: &mut egui::Ui, size: f32, palette: &theme::Palette) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        egui::CornerRadius::same((size * 0.22).round() as u8),
        palette.action,
    );
    painter.rect_stroke(
        rect.shrink(1.0),
        egui::CornerRadius::same((size * 0.22).round() as u8),
        egui::Stroke::new(1.0, palette.background_light),
        egui::StrokeKind::Inside,
    );

    let point =
        |x: f32, y: f32| egui::pos2(rect.left() + size * x / 24.0, rect.top() + size * y / 24.0);
    let upper = vec![
        point(19.2, 7.7),
        point(16.7, 4.5),
        point(15.7, 4.0),
        point(8.6, 4.0),
        point(7.7, 4.3),
        point(4.2, 7.7),
        point(4.8, 9.9),
        point(14.4, 15.9),
    ];
    let lower = vec![
        point(4.8, 16.3),
        point(7.3, 19.5),
        point(8.3, 20.0),
        point(15.4, 20.0),
        point(16.3, 19.7),
        point(19.8, 16.3),
        point(19.2, 14.1),
        point(9.6, 8.1),
    ];
    let stroke_width = (size * 0.09).max(3.0);
    painter.add(egui::Shape::line(
        upper,
        egui::Stroke::new(stroke_width, palette.background_light),
    ));
    painter.add(egui::Shape::line(
        lower,
        egui::Stroke::new(stroke_width, palette.accent),
    ));
}

#[derive(Clone, Copy, Debug)]
struct ModelOrderDrag {
    source_index: usize,
}

#[cfg(test)]
mod ordering_tests {
    use super::egui;
    use super::{
        dashboard_panel_heights, dashboard_uses_wide_layout, fit_dialog_size, friendly_error,
        move_list_item, oauth_terms_confirmation_ready, remaining_quota_percent, shell_metrics,
        topbar_control_size, usage_monitor_uses_two_columns, APP_VERSION, DIALOG_VIEWPORT_GUTTER,
        TERMS_EN, TERMS_ZH, TOPBAR_CONTROL_HEIGHT, TOPBAR_CONTROL_WIDTH,
    };

    #[test]
    fn embedded_terms_identify_the_release_in_both_languages() {
        assert!(TERMS_ZH.contains(&format!("软件版本：v{APP_VERSION}")));
        assert!(TERMS_EN.contains(&format!("Software version: v{APP_VERSION}")));

        let zh_date = TERMS_ZH
            .lines()
            .find_map(|line| line.strip_prefix("发布日期："))
            .expect("Chinese terms should contain a release date");
        let en_date = TERMS_EN
            .lines()
            .find_map(|line| line.strip_prefix("Release date: "))
            .expect("English terms should contain a release date");
        assert_eq!(zh_date, en_date);
        assert_eq!(zh_date.len(), 10);
        assert!(zh_date
            .bytes()
            .enumerate()
            .all(|(index, byte)| if index == 4 || index == 7 {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }));
    }

    #[test]
    fn dashboard_keeps_two_and_a_half_model_rows_and_a_compact_log() {
        for available_height in [480.0, 560.0, 720.0] {
            let (model_panel, model_list, log) = dashboard_panel_heights(available_height);
            assert!((312.0..=320.0).contains(&model_panel));
            assert!((216.0..=224.0).contains(&model_list));
            assert_eq!(log, 90.0);
        }
    }

    #[test]
    fn short_windows_shrink_the_model_panel_so_the_log_stays_visible() {
        // On a short window the model panel must give way; otherwise the
        // activity log is pushed below the taskbar.
        for available_height in [300.0_f32, 360.0, 420.0] {
            let (model_panel, model_list, log) = dashboard_panel_heights(available_height);
            assert!(
                model_panel + log + 72.0 <= available_height + 0.5,
                "panels overflow at {available_height}: {model_panel} + {log}"
            );
            assert!(model_list <= model_panel);
        }
    }

    #[test]
    fn topbar_controls_share_stable_regular_and_compact_sizes() {
        assert_eq!(TOPBAR_CONTROL_WIDTH, 112.0);
        assert_eq!(TOPBAR_CONTROL_HEIGHT, 48.0);
        assert_eq!(topbar_control_size(1280.0, 820.0), egui::vec2(112.0, 48.0));
        assert_eq!(topbar_control_size(944.0, 452.0), egui::vec2(92.0, 42.0));
    }

    #[test]
    fn short_screen_shell_never_reserves_more_height_than_it_has() {
        let metrics = shell_metrics(654.0);
        assert_eq!(metrics.topbar_height, 80.0);
        assert!(metrics.topbar_height + metrics.content_top_space + metrics.footer_height < 654.0);
    }

    #[test]
    fn dialogs_fit_small_high_dpi_and_short_wide_viewports() {
        let square = fit_dialog_size(
            egui::vec2(654.0, 654.0),
            egui::vec2(820.0, 620.0),
            egui::vec2(520.0, 360.0),
        );
        assert_eq!(square, egui::vec2(630.0, 620.0));

        let short = fit_dialog_size(
            egui::vec2(960.0, 492.0),
            egui::vec2(820.0, 620.0),
            egui::vec2(520.0, 360.0),
        );
        assert_eq!(short, egui::vec2(820.0, 468.0));

        let minimum_window = egui::vec2(800.0, 400.0);
        for desired in [
            egui::vec2(560.0, 250.0),
            egui::vec2(560.0, 360.0),
            egui::vec2(600.0, 360.0),
            egui::vec2(620.0, 390.0),
            egui::vec2(600.0, 430.0),
            egui::vec2(720.0, 560.0),
            egui::vec2(920.0, 560.0),
        ] {
            let fitted = fit_dialog_size(minimum_window, desired, egui::vec2(360.0, 220.0));
            assert!(fitted.x <= minimum_window.x - DIALOG_VIEWPORT_GUTTER);
            assert!(fitted.y <= minimum_window.y - DIALOG_VIEWPORT_GUTTER);
            assert!(fitted.x >= 360.0);
            assert!(fitted.y >= 220.0);
        }
    }

    #[test]
    fn dashboard_wide_mode_requires_width_and_height() {
        assert!(dashboard_uses_wide_layout(1280.0, 620.0));
        assert!(dashboard_uses_wide_layout(1000.0, 520.0));
        assert!(!dashboard_uses_wide_layout(1280.0, 480.0));
        assert!(!dashboard_uses_wide_layout(900.0, 620.0));
        assert!(usage_monitor_uses_two_columns(960.0));
        assert!(!usage_monitor_uses_two_columns(800.0));
    }

    #[test]
    fn quota_plan_accounts_include_oauth_and_windowed_coding_plans() {
        let oauth = super::UsageAccount {
            kind: "oauth".into(),
            ..Default::default()
        };
        assert!(super::CodexRouterApp::usage_account_is_quota_plan(&oauth));

        let coding_plan = super::UsageAccount {
            kind: "apikey".into(),
            windows: vec![super::UsageWindow {
                kind: "weekly".into(),
                used_percent: Some(20.0),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(super::CodexRouterApp::usage_account_is_quota_plan(
            &coding_plan
        ));

        let five_hour_plan = super::UsageAccount {
            kind: "apikey".into(),
            windows: vec![super::UsageWindow {
                kind: "fiveHour".into(),
                used_percent: Some(0.0),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(super::CodexRouterApp::usage_account_is_quota_plan(
            &five_hour_plan
        ));

        let metered = super::UsageAccount {
            kind: "apikey".into(),
            ..Default::default()
        };
        assert!(!super::CodexRouterApp::usage_account_is_quota_plan(
            &metered
        ));

        let model_window_only = super::UsageAccount {
            kind: "apikey".into(),
            windows: vec![super::UsageWindow {
                kind: "model".into(),
                used_percent: Some(50.0),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(!super::CodexRouterApp::usage_account_is_quota_plan(
            &model_window_only
        ));
    }

    #[test]
    fn first_oauth_stays_locked_until_terms_and_preparation_are_both_complete() {
        assert!(!oauth_terms_confirmation_ready(
            false,
            false,
            Some("openai"),
            Some("openai")
        ));
        assert!(!oauth_terms_confirmation_ready(
            true,
            true,
            None,
            Some("openai")
        ));
        assert!(!oauth_terms_confirmation_ready(
            true,
            false,
            Some("grok"),
            Some("openai")
        ));
        assert!(oauth_terms_confirmation_ready(
            true,
            false,
            Some("openai"),
            Some("openai")
        ));
    }

    #[test]
    fn list_items_can_move_in_both_directions() {
        let mut items = vec![1, 2, 3, 4];
        assert!(move_list_item(&mut items, 0, 3));
        assert_eq!(items, vec![2, 3, 4, 1]);
        assert!(move_list_item(&mut items, 3, 1));
        assert_eq!(items, vec![2, 1, 3, 4]);
    }

    #[test]
    fn invalid_or_same_position_moves_are_ignored() {
        let mut items = vec![1, 2, 3];
        assert!(!move_list_item(&mut items, 1, 1));
        assert!(!move_list_item(&mut items, 5, 0));
        assert_eq!(items, vec![1, 2, 3]);
    }

    #[test]
    fn structured_connection_errors_are_localized_for_people() {
        let raw = "class=connection_refused | status=503 | retryable=true";
        assert!(friendly_error(raw, true).contains("本地服务未响应"));
        assert!(friendly_error(raw, false).contains("local service"));
        assert!(friendly_error("class=network_unavailable", true).contains("无法连接网络"));
        assert!(
            friendly_error("class=network_unavailable", false).contains("network is unavailable")
        );
        assert_eq!(friendly_error("custom error", true), "custom error");
    }

    #[test]
    fn quota_percentages_are_presented_as_remaining_capacity() {
        assert_eq!(remaining_quota_percent(Some(0.0)), Some(100.0));
        assert_eq!(remaining_quota_percent(Some(62.0)), Some(38.0));
        assert_eq!(remaining_quota_percent(Some(100.0)), Some(0.0));
        assert_eq!(remaining_quota_percent(None), None);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UsageOrderSection {
    Subscription,
    Api,
}

#[derive(Clone, Copy, Debug)]
struct UsageOrderDrag {
    section: UsageOrderSection,
    account_id: i64,
}

fn move_list_item<T>(items: &mut Vec<T>, source_index: usize, target_index: usize) -> bool {
    if source_index >= items.len() || target_index >= items.len() || source_index == target_index {
        return false;
    }
    let item = items.remove(source_index);
    items.insert(target_index.min(items.len()), item);
    true
}

fn step_number(page: Page) -> usize {
    match page {
        Page::Welcome => 0,
        Page::Project => 1,
        Page::Auth => 2,
        Page::Model => 3,
        Page::Proxy => 4,
        Page::Finish => 5,
        Page::Dashboard => 6,
        Page::Profiles => 6,
        Page::OAuth => 6,
        Page::Monitor => 6,
    }
}

fn t<'a>(zh: bool, chinese: &'a str, english: &'a str) -> &'a str {
    if zh {
        chinese
    } else {
        english
    }
}

fn oauth_terms_confirmation_ready(
    scroll_complete: bool,
    preparing: bool,
    prepared_provider: Option<&str>,
    pending_provider: Option<&str>,
) -> bool {
    scroll_complete
        && !preparing
        && prepared_provider.is_some()
        && prepared_provider == pending_provider
}

fn remaining_quota_percent(used_percent: Option<f32>) -> Option<f32> {
    used_percent.map(|value| 100.0 - value.clamp(0.0, 100.0))
}

fn friendly_error(raw: &str, zh: bool) -> String {
    let normalized = raw.to_ascii_lowercase();
    if normalized.contains("router_oauth_accounts_unavailable")
        || normalized.contains("connection_refused")
        || normalized.contains("connection_closed")
        || normalized.contains("lifecycle_busy")
        || normalized.contains("lifecycle_deferred")
    {
        t(
            zh,
            "本地服务未响应。请确认 Codex-Router 正在后台运行，然后点击「刷新」。",
            "The local service did not respond. Keep Codex-Router running, then click Refresh.",
        )
        .to_owned()
    } else if normalized.contains("router_oauth_accounts_parse") {
        t(
            zh,
            "OAuth 账号清单暂时无法解析。请点击「刷新」；若仍失败，请重启后再试。",
            "The OAuth account list could not be parsed. Click Refresh; if it still fails, restart and try again.",
        )
        .to_owned()
    } else if normalized.contains("authentication")
        || normalized.contains("status=401")
        || normalized.contains("unauthorized")
    {
        t(
            zh,
            "管理会话未就绪或已过期。请稍候再点「刷新」；若仍失败，请重启 Codex-Router。",
            "The admin session is not ready or has expired. Wait a moment and click Refresh; if it still fails, restart Codex-Router.",
        )
        .to_owned()
    } else if normalized.contains("upstream_timeout") || normalized.contains("timeout") {
        t(
            zh,
            "上游响应超时。当前配置未被更改，请稍后重试。",
            "The upstream request timed out. Your configuration was not changed; try again later.",
        )
        .to_owned()
    } else if normalized.contains("network_unavailable") || normalized.contains("class=network") {
        t(
            zh,
            "当前无法连接网络。请检查系统代理或网络连接后重试。",
            "The network is unavailable. Check the system proxy or connection, then try again.",
        )
        .to_owned()
    } else if normalized.contains("status=503") {
        t(
            zh,
            "上游服务暂时不可用。当前配置未被更改，请稍后重试。",
            "The upstream service is temporarily unavailable. Your configuration was not changed; try again later.",
        )
        .to_owned()
    } else if normalized.contains("class=request_failure")
        || normalized.contains("class=unclassified_error")
    {
        t(
            zh,
            "暂时无法读取 OAuth 账号。请确认本地服务已启动，然后点击「刷新」。",
            "Could not load OAuth accounts right now. Make sure the local service is running, then click Refresh.",
        )
        .to_owned()
    } else {
        raw.to_owned()
    }
}

fn oauth_model_docs_url(platform: &str) -> &'static str {
    match platform {
        "openai" => "https://developers.openai.com/api/docs/models",
        "anthropic" => "https://platform.claude.com/docs/en/about-claude/models/overview",
        "gemini" | "antigravity" => "https://ai.google.dev/gemini-api/docs/models",
        "grok" => "https://docs.x.ai/docs/models",
        _ => "https://github.com/HernanJiang/Codex-Router",
    }
}

fn page_label(page: Page, zh: bool) -> &'static str {
    match page {
        Page::Welcome => t(zh, "首页", "INTRO"),
        Page::Project => t(zh, "项目", "PROJECT"),
        Page::Auth => t(zh, "登录", "ACCESS"),
        Page::Model => t(zh, "模型", "MODEL"),
        Page::Proxy => t(zh, "网络", "NETWORK"),
        Page::Finish => t(zh, "部署", "DEPLOY"),
        Page::Dashboard => t(zh, "控制台", "CONSOLE"),
        Page::Profiles => t(zh, "切换配置分组", "SWITCH GROUPS"),
        Page::OAuth => t(zh, "OAuth 账号", "OAUTH ACCOUNTS"),
        Page::Monitor => t(zh, "实时用量统计", "LIVE USAGE"),
    }
}

fn log_excerpt(text: &str, max_lines: usize, max_chars: usize) -> String {
    let lines: Vec<_> = text.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..]
        .iter()
        .map(|line| line.chars().take(max_chars).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn dashboard_panel_heights(layout_height: f32) -> (f32, f32, f32) {
    const LOG_CONTENT_HEIGHT: f32 = 90.0;
    // Chrome around both panels: log frame, spacing, and the footer strip.
    const RESERVED: f32 = 72.0;
    let available_for_models = layout_height - LOG_CONTENT_HEIGHT - RESERVED;
    // Prefer ~2.5 model rows, but never claim more than the window can show or
    // the activity log gets pushed under the taskbar.
    let model_content_height = available_for_models.clamp(0.0, 320.0);
    let model_list_height = (model_content_height - 96.0).clamp(0.0, 224.0);
    (model_content_height, model_list_height, LOG_CONTENT_HEIGHT)
}

impl eframe::App for CodexRouterApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        super::clamp_window_to_current_monitor(ctx);
        if self.ui_audit_mode {
            return;
        }
        self.enforce_background_start_hidden(ctx);
        self.process_app_events(ctx);
        if !self.exit_shutdown_in_progress {
            self.process_router_health_protection(ctx);
            self.process_codex_binding_watch(ctx);
            self.process_scheduled_usage_refresh(ctx);
            self.process_scheduled_oauth_recovery(ctx);
            self.process_scheduled_oauth_account_refresh(ctx);
        }
        self.handle_close_request(ctx);
        self.handle_native_minimize(ctx);
    }

    fn ui(&mut self, root_ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root_ui.ctx().clone();
        // Any visible dialog while the process is still marked lightweight must
        // restore CJK fonts first. Otherwise the UI becomes tofu boxes.
        let needs_full_fonts = self.close_prompt_open
            || self.apply_success_dialog_open
            || self.oauth_priority_target.is_some()
            || self.oauth_revoke_target.is_some()
            || self.update_dialog_open
            || self.terms_open
            || !self.exit_shutdown_error.is_empty()
            || (!self.tray_lightweight_mode && ctx.input(|i| i.viewport().minimized != Some(true)));
        if needs_full_fonts && !self.fonts_loaded {
            ensure_full_ui_fonts(self, &ctx);
        }
        let palette = theme::palette(&self.config.ui_theme);
        let viewport_height = ctx.content_rect().height();
        let compact_layout = viewport_height < 700.0;
        let shell = shell_metrics(viewport_height);
        if self.installed_theme != self.config.ui_theme
            || self.installed_compact_layout != compact_layout
        {
            theme::install(&ctx, &palette);
            self.installed_theme.clone_from(&self.config.ui_theme);
            self.installed_compact_layout = compact_layout;
        }
        if !self.tray_lightweight_mode {
            self.load_logo_texture(&ctx);
        }
        if self.page != self.last_page {
            let previous_page = self.last_page;
            self.last_page = self.page;
            self.page_changed_at = std::time::Instant::now();
            if previous_page == Page::Monitor && self.page != Page::Monitor {
                self.schedule_usage_refresh_after_close(&ctx);
            }
        }
        egui::Panel::top("editorial-nav")
            .exact_size(shell.topbar_height)
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_unmultiplied(
                        palette.background_light.r(),
                        palette.background_light.g(),
                        palette.background_light.b(),
                        178,
                    ))
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 112),
                    ))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 8],
                        blur: 24,
                        spread: 0,
                        color: egui::Color32::from_rgba_unmultiplied(25, 18, 12, 34),
                    })
                    .inner_margin(egui::Margin::symmetric(
                        shell.topbar_horizontal_margin,
                        shell.topbar_vertical_margin,
                    )),
            )
            .show(root_ui, |ui| self.show_topbar(ui, &palette));

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(palette.background))
            .show(root_ui, |ui| {
                let rect = ui.max_rect();
                theme::paint_background(ui.painter(), rect, &palette);
                ui.add_space(shell.content_top_space);
                let max_width = if matches!(
                    self.page,
                    Page::Dashboard | Page::Profiles | Page::OAuth | Page::Monitor
                ) {
                    1480.0
                } else {
                    1320.0
                };
                let container_width = ui.available_width();
                let horizontal_gutter = if compact_layout { 24.0 } else { 48.0 };
                let width = (container_width - horizontal_gutter)
                    .max(0.0)
                    .min(max_width)
                    .min(container_width);
                let side = ((container_width - width) * 0.5).max(0.0);
                // Reserve a dedicated footer strip below the wizard cards so the
                // signature always sits on the solid theme color, never across a
                // pale card where its leading characters lose contrast.
                let content_height = (ui.available_height() - shell.footer_height).max(0.0);
                ui.horizontal_top(|ui| {
                    ui.add_space(side);
                    ui.allocate_ui_with_layout(
                        egui::vec2(width, content_height),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            let elapsed = self.page_changed_at.elapsed().as_secs_f32();
                            let opacity = (elapsed / 0.24).clamp(0.22, 1.0);
                            ui.set_opacity(opacity);
                            ui.add_space((1.0 - opacity) * 10.0);
                            let viewport = ui.available_size();
                            if let Some(min_height) = page_scroll_min_height(self.page)
                                .filter(|height| *height > viewport.y)
                            {
                                egui::ScrollArea::vertical()
                                    .id_salt(("page-height-scroll", page_label(self.page, false)))
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        ui.set_min_width(viewport.x);
                                        ui.allocate_ui_with_layout(
                                            egui::vec2(viewport.x, min_height),
                                            egui::Layout::top_down(egui::Align::Min),
                                            |ui| self.show_current_page(ui, &palette),
                                        );
                                    });
                            } else {
                                self.show_current_page(ui, &palette);
                            }
                        },
                    );
                });
                ui.painter().text(
                    rect.right_bottom() - egui::vec2(20.0, 8.0),
                    egui::Align2::RIGHT_BOTTOM,
                    "~By Hernan_Jiang",
                    egui::FontId::new(11.0, theme::serif_family()),
                    egui::Color32::from_rgba_unmultiplied(
                        palette.paper.r(),
                        palette.paper.g(),
                        palette.paper.b(),
                        108,
                    ),
                );
            });
        if self.close_prompt_open {
            self.show_close_prompt(&ctx, &palette);
        }
        if self.apply_success_dialog_open {
            self.show_apply_success_dialog(&ctx, &palette);
        }
        if self.sub2api_intro_open {
            self.show_sub2api_intro(&ctx, &palette);
        }
        if self.update_dialog_open {
            self.show_update_dialog(&ctx, &palette);
        }
        if self.oauth_revoke_target.is_some() {
            self.show_oauth_revoke_dialog(&ctx, &palette);
        }
        if self.oauth_priority_target.is_some() {
            self.show_oauth_priority_dialog(&ctx, &palette);
        }
        if self.oauth_manual_model_target.is_some() {
            self.show_oauth_manual_model_dialog(&ctx, &palette);
        }
        if self.oauth_fallback_picker_target.is_some() {
            self.show_oauth_fallback_picker_dialog(&ctx, &palette);
        }
        if self.channel_preset_dialog_open {
            self.show_channel_preset_dialog(&ctx, &palette);
        }
        if self.recommended_platform_dialog_open {
            self.show_recommended_platform_dialog(&ctx, &palette);
        }
        if self.grok_sso_dialog_open {
            self.show_grok_sso_dialog(&ctx, &palette);
        }
        if self.terms_open {
            self.show_terms_modal(&ctx, &palette);
        }
        if self.log_dialog_open {
            self.show_log_dialog(&ctx, &palette);
        }
        if self.tray_lightweight_mode {
            // Health protection schedules its own low-frequency wakeup.
        } else if self.page_changed_at.elapsed().as_secs_f32() < 0.3 {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        } else if self.applying
            || self.exit_shutdown_in_progress
            || self.router_mode_switching
            || self.codex_account_mode_switching
            || self.usage_loading
            || self.oauth_loading
            || self.provider_oauth_running
            || self.update_checking
            || self.update_downloading
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if self.ui_audit_mode {
            return;
        }
        storage.set_string("codex-router-ui-theme-v3", self.config.ui_theme.clone());
        storage.set_string("codex-router-ui-language-v1", self.ui_language.clone());
    }
}

impl CodexRouterApp {
    fn show_current_page(&mut self, ui: &mut egui::Ui, palette: &theme::Palette) {
        match self.page {
            Page::Welcome => self.show_welcome(ui, palette),
            Page::Project => self.show_project(ui, palette),
            Page::Auth => self.show_auth(ui, palette),
            Page::Model => self.show_model(ui, palette),
            Page::Proxy => self.show_proxy(ui, palette),
            Page::Finish => self.show_finish(ui, palette),
            Page::Dashboard => self.show_dashboard(ui, palette),
            Page::Profiles => self.show_profiles(ui, palette),
            Page::OAuth => {
                ui.push_id("oauth-page", |ui| self.show_oauth_accounts(ui, palette));
            }
            Page::Monitor => self.show_usage_monitor(ui, palette),
        }
    }

    fn topbar_button(
        ui: &mut egui::Ui,
        label: &str,
        enabled: bool,
        palette: &theme::Palette,
    ) -> egui::Response {
        let viewport = ui.ctx().content_rect().size();
        let control_size = topbar_control_size(viewport.x, viewport.y);
        ui.add_enabled(
            enabled,
            egui::Button::new(
                egui::RichText::new(label)
                    .size(14.0)
                    .strong()
                    .color(palette.ink),
            )
            .fill(palette.paper)
            .stroke(egui::Stroke::new(1.0, palette.line))
            .corner_radius(egui::CornerRadius::same(10))
            .min_size(control_size),
        )
    }

    fn topbar_language_switch(
        ui: &mut egui::Ui,
        zh: bool,
        palette: &theme::Palette,
    ) -> egui::Response {
        let viewport = ui.ctx().content_rect().size();
        let control_size = topbar_control_size(viewport.x, viewport.y);
        let (rect, response) = ui.allocate_exact_size(control_size, egui::Sense::click());
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(10), palette.paper);
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(10),
            egui::Stroke::new(1.0, palette.line),
            egui::StrokeKind::Inside,
        );

        let track = egui::Rect::from_center_size(rect.center(), egui::vec2(38.0, 22.0));
        ui.painter()
            .rect_filled(track, egui::CornerRadius::same(11), palette.action);
        let position = ui.ctx().animate_bool(response.id, !zh);
        let knob_x = egui::lerp((track.left() + 11.0)..=(track.right() - 11.0), position);
        ui.painter().circle_filled(
            egui::pos2(knob_x, track.center().y),
            8.0,
            egui::Color32::WHITE,
        );
        let font = egui::FontId::new(13.0, egui::FontFamily::Proportional);
        ui.painter().text(
            egui::pos2(rect.left() + 22.0, rect.center().y),
            egui::Align2::CENTER_CENTER,
            "中",
            font.clone(),
            if zh { palette.ink } else { palette.muted },
        );
        ui.painter().text(
            egui::pos2(rect.right() - 24.0, rect.center().y),
            egui::Align2::CENTER_CENTER,
            "EN",
            font,
            if zh { palette.muted } else { palette.ink },
        );
        response
    }

    fn topbar_theme_switch(
        &mut self,
        ui: &mut egui::Ui,
        zh: bool,
        palette: &theme::Palette,
    ) -> egui::Response {
        let viewport = ui.ctx().content_rect().size();
        let control_size = topbar_control_size(viewport.x, viewport.y);
        let compact = control_size.x < TOPBAR_CONTROL_WIDTH;
        ui.add_sized(
            control_size,
            egui::Button::new(
                egui::RichText::new(if self.config.ui_theme == "sky" {
                    if compact {
                        t(zh, "雾蓝", "MIST")
                    } else {
                        t(zh, "主题 · 雾蓝", "THEME · MIST")
                    }
                } else if compact {
                    t(zh, "陶土", "CLAY")
                } else {
                    t(zh, "主题 · 陶土", "THEME · CLAY")
                })
                .size(13.0)
                .strong()
                .color(palette.ink),
            )
            .fill(palette.paper)
            .stroke(egui::Stroke::new(1.0, palette.line))
            .corner_radius(egui::CornerRadius::same(10)),
        )
    }

    fn show_apply_success_dialog(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let compact = ctx.content_rect().height() < 500.0;
        let mut acknowledged = false;
        // Keep enough height for the title, two body paragraphs, and the primary
        // action button on short high-DPI windows. fixed_size alone used to clip
        // the "Got it" button under the window chrome.
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(580.0, if compact { 360.0 } else { 320.0 }),
            egui::vec2(420.0, 300.0),
        );
        egui::Window::new(t(zh, "配置已应用", "Configuration applied"))
            .id(egui::Id::new("apply-success-dialog"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .default_size(dialog_size)
            .min_size(dialog_size)
            .max_size(dialog_size)
            .show(ctx, |ui| {
                ui.set_min_size(dialog_size);
                ui.set_max_size(dialog_size);
                let button_reserve = 64.0;
                let body_height = (ui.available_height() - button_reserve).max(140.0);
                egui::ScrollArea::vertical()
                    .id_salt("apply-success-body")
                    .max_height(body_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(t(
                                zh,
                                "建议流程：Codex 先登录 → 再应用 Router → 完全重启 Codex",
                                "Recommended: sign in to Codex first → apply Router → fully restart Codex",
                            ))
                            .size(if compact { 16.0 } else { 18.0 })
                            .strong()
                            .color(palette.ink),
                        );
                        ui.add_space(if compact { 6.0 } else { 10.0 });
                        ui.label(
                            egui::RichText::new(t(
                                zh,
                                "请完全退出并重新启动 ChatGPT / Codex。未登录时也会写入本地 Router（第三方 API）；已登录账号会保留，模型列表来自本机路由目录。",
                                "Fully quit and restart ChatGPT / Codex. Without a login, local Router API mode is still written; an existing login is kept, and models come from the local catalog.",
                            ))
                            .color(palette.ink_soft),
                        );
                        ui.add_space(if compact { 4.0 } else { 8.0 });
                        ui.label(
                            egui::RichText::new(if compact {
                                t(
                                    zh,
                                    "请保持 Codex-Router 在系统托盘运行，否则本地转发将停止。",
                                    "Keep Codex-Router running in the tray, otherwise local forwarding stops.",
                                )
                            } else {
                                t(
                                    zh,
                                    "使用过程中请保持 Codex-Router 在后台或系统托盘运行，否则本地转发将不可用。",
                                    "Keep Codex-Router running in the background or system tray while using it, otherwise local forwarding will be unavailable.",
                                )
                            })
                            .color(palette.ink_soft),
                        );
                        ui.add_space(if compact { 4.0 } else { 8.0 });
                        ui.label(
                            egui::RichText::new(if compact {
                                t(
                                    zh,
                                    "托盘轻量模式会暂停界面、日志和用量刷新，仅保留低频健康检查、OAuth 必要恢复与连接恢复。",
                                    "Lightweight tray mode pauses UI, logs, and usage refresh, keeping only low-frequency health checks, essential OAuth recovery, and connection recovery.",
                                )
                            } else {
                                t(
                                    zh,
                                    "进入托盘后会自动启用轻量模式，暂停界面刷新、日志跟随和用量刷新，只保留低频健康检查、OAuth 必要恢复与连接恢复，不会持续占用计算资源。",
                                    "Tray mode automatically pauses UI updates, log following, and usage refresh. Only low-frequency health checks, essential OAuth recovery, and connection recovery remain, so it does not continuously consume computing resources.",
                                )
                            })
                            .small()
                            .color(palette.muted),
                        );
                    });
                ui.add_space(10.0);
                ui.vertical_centered(|ui| {
                    if ui
                        .add_sized(
                            [160.0, 46.0],
                            egui::Button::new(
                                egui::RichText::new(t(zh, "知道了", "Got it"))
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(palette.action)
                            .stroke(egui::Stroke::NONE)
                            .corner_radius(egui::CornerRadius::same(7)),
                        )
                        .clicked()
                    {
                        acknowledged = true;
                    }
                });
            });
        if acknowledged {
            self.apply_success_dialog_open = false;
        }
    }

    fn show_log_dialog(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let mut open = self.log_dialog_open;
        let mut clear_requested = false;
        let mut export_requested = false;
        let mut scroll_requested = false;
        let mut scroll_consumed = false;
        let available = ctx.content_rect().size();
        let default_size = fit_dialog_size(
            available,
            egui::vec2(920.0, 560.0),
            egui::vec2(480.0, 280.0),
        );
        let minimum_size = fit_dialog_size(
            available,
            egui::vec2(560.0, 320.0),
            egui::vec2(360.0, 240.0),
        );
        let maximum_size = fit_dialog_size(available, available, egui::vec2(360.0, 240.0));
        egui::Window::new(t(zh, "运行日志", "Runtime log"))
            .id(egui::Id::new("runtime-log-dialog"))
            .default_size(default_size)
            .min_size(minimum_size)
            .max_size(maximum_size)
            .resizable(true)
            .collapsible(false)
            .open(&mut open)
            .frame(
                egui::Frame::new()
                    .fill(palette.paper)
                    .stroke(egui::Stroke::new(1.0, palette.line))
                    .inner_margin(egui::Margin::same(18)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let previous_follow = self.log_follow_latest;
                    ui.checkbox(
                        &mut self.log_follow_latest,
                        t(zh, "跟随最新", "Follow latest"),
                    );
                    if self.log_follow_latest && !previous_follow {
                        scroll_requested = true;
                    }
                    if ui.button(t(zh, "清空", "Clear")).clicked() {
                        clear_requested = true;
                    }
                    if ui.button(t(zh, "下载", "Download")).clicked() {
                        export_requested = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{:.1} KiB",
                                self.logs.len() as f32 / 1024.0
                            ))
                            .small()
                            .color(palette.muted),
                        );
                    });
                });
                ui.add_space(8.0);
                egui::Frame::new()
                    .fill(palette.background_dark)
                    .stroke(egui::Stroke::new(1.0, palette.line))
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        let content = if self.logs.is_empty() {
                            t(
                                zh,
                                "等待错误或配置事件…",
                                "Waiting for errors or configuration events…",
                            )
                        } else {
                            self.logs.as_str()
                        };
                        egui::ScrollArea::vertical()
                            .id_salt("runtime-log-dialog-scroll")
                            .auto_shrink([false, false])
                            .stick_to_bottom(self.log_follow_latest)
                            .max_height(ui.available_height().max(96.0))
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.label(
                                    egui::RichText::new(content)
                                        .monospace()
                                        .small()
                                        .color(egui::Color32::WHITE),
                                );
                                if self.log_scroll_to_bottom || scroll_requested {
                                    ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
                                    scroll_consumed = true;
                                }
                            });
                    });
            });
        self.log_dialog_open = open;
        if clear_requested {
            self.logs.clear();
            self.log_scroll_to_bottom = false;
        } else if scroll_consumed {
            self.log_scroll_to_bottom = false;
        }
        if export_requested {
            self.export_logs();
        }
    }

    fn show_close_prompt(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let compact = ctx.content_rect().height() < 500.0;
        let mut action = None;
        let mut cancel = false;
        let mut close_ui_only = false;
        let mut window_open = true;
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(560.0, 360.0),
            egui::vec2(420.0, 280.0),
        );
        egui::Window::new(t(zh, "关闭 Codex-Router", "Close Codex-Router"))
            .id(egui::Id::new("close-behavior-prompt"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size(dialog_size)
            .collapsible(false)
            .resizable(false)
            .vscroll(true)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .open(&mut window_open)
            .frame(
                egui::Frame::new()
                    .fill(palette.paper)
                    .stroke(egui::Stroke::new(1.0, palette.line))
                    .inner_margin(egui::Margin::same(if compact { 14 } else { 22 })),
            )
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(t(
                        zh,
                        "建议最小化到系统托盘",
                        "Minimizing to the system tray is recommended",
                    ))
                    .size(if compact { 17.0 } else { 19.0 })
                    .strong()
                    .color(palette.ink),
                );
                ui.label(
                    egui::RichText::new(if compact {
                        t(
                            zh,
                            "彻底退出会恢复原 Codex 配置并停止本地转发。",
                            "A full exit restores the previous Codex configuration and stops local forwarding.",
                        )
                    } else {
                        t(
                            zh,
                            "彻底退出会先恢复 Codex-Router 接管前的官方、API 或外部切换配置，再停止当前便携目录中的本地服务；退出后不再保留 Router 转发。",
                            "A full exit first restores the official, API, or external-switch configuration used before Codex-Router, then stops this portable directory's local services. Router forwarding is removed after exit.",
                        )
                    })
                    .small()
                    .color(palette.muted),
                );
                ui.add_space(if compact { 3.0 } else { 6.0 });
                ui.label(
                    egui::RichText::new(if compact {
                        t(
                            zh,
                            "托盘轻量模式暂停界面、日志和用量刷新，保留低频健康检查、OAuth 必要恢复与连接恢复。",
                            "Tray mode pauses UI, logs, and usage refresh while retaining low-frequency health checks, essential OAuth recovery, and connection recovery.",
                        )
                    } else {
                        t(
                            zh,
                            "最小化后自动进入轻量模式：暂停日志跟随、用量刷新和界面刷新，保留低频健康检查、OAuth 必要恢复与连接恢复。",
                            "Minimizing enables lightweight mode: log following, usage refresh, and UI refresh pause while low-frequency health checks, essential OAuth recovery, and connection recovery remain active.",
                        )
                    })
                    .small()
                    .color(palette.muted),
                );
                ui.add_space(if compact { 6.0 } else { 12.0 });
                if self.exit_shutdown_in_progress {
                    ui.label(
                        egui::RichText::new(t(
                            zh,
                            "正在恢复原 Codex 配置并彻底停止本地转发，请稍候…",
                            "Restoring the previous Codex configuration and stopping local forwarding…",
                        ))
                        .strong()
                        .color(palette.accent),
                    );
                } else {
                    ui.checkbox(
                        &mut self.remember_close_choice,
                        t(zh, "记住我的选择", "Remember my choice"),
                    );
                }
                if !self.exit_shutdown_error.is_empty() {
                    ui.label(
                        egui::RichText::new(&self.exit_shutdown_error)
                            .small()
                            .color(palette.danger),
                    );
                    if theme::secondary_button(
                        ui,
                        t(
                            zh,
                            "仅关闭界面（保持转发）",
                            "Close UI only (keep forwarding)",
                        ),
                        palette,
                    )
                    .on_hover_text(t(
                        zh,
                        "关闭 Codex-Router 窗口，但保留当前后台转发服务",
                        "Close the Codex-Router window while keeping the current forwarding services",
                    ))
                    .clicked()
                    {
                        close_ui_only = true;
                    }
                }
                ui.add_space(if compact { 6.0 } else { 12.0 });
                ui.horizontal(|ui| {
                    let minimize = ui.add_enabled_ui(!self.exit_shutdown_in_progress, |ui| {
                        theme::primary_button(
                            ui,
                            egui::RichText::new(t(zh, "最小化到托盘", "Minimize to tray"))
                                .strong()
                                .color(egui::Color32::WHITE),
                            palette,
                        )
                    });
                    if minimize.inner.clicked() {
                        action = Some(CloseBehavior::MinimizeToTray);
                    }
                    let exit = ui.add_enabled_ui(!self.exit_shutdown_in_progress, |ui| {
                        theme::secondary_button(ui, t(zh, "彻底退出", "Exit completely"), palette)
                    });
                    if exit.inner.clicked() {
                        action = Some(CloseBehavior::Exit);
                    }
                    let cancel_response = ui.add_enabled_ui(!self.exit_shutdown_in_progress, |ui| {
                        theme::secondary_button(ui, t(zh, "取消", "Cancel"), palette)
                    });
                    if cancel_response.inner.clicked() {
                        cancel = true;
                    }
                });
            });

        if close_ui_only {
            self.close_prompt_open = false;
            self.exit_shutdown_error.clear();
            self.exit_after_prompt = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if let Some(choice) = action {
            if self.remember_close_choice {
                self.close_behavior = choice;
                if !self.persist_close_behavior() {
                    self.close_behavior = CloseBehavior::Ask;
                }
            }
            match choice {
                CloseBehavior::MinimizeToTray => self.minimize_to_tray(ctx),
                CloseBehavior::Exit => self.request_exit(ctx),
                CloseBehavior::Ask => {}
            }
        } else if (cancel || !window_open) && !self.exit_shutdown_in_progress {
            self.close_prompt_open = false;
            self.remember_close_choice = false;
            self.exit_shutdown_error.clear();
        }
    }

    fn show_topbar(&mut self, ui: &mut egui::Ui, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let viewport = ui.ctx().content_rect().size();
        let viewport_width = viewport.x;
        let compact_chrome = viewport.y < 700.0 || viewport_width < 1060.0;
        let compact_brand = viewport_width < 1000.0;
        let logo_size = if compact_chrome { 34.0 } else { 38.0 };
        ui.horizontal(|ui| {
            egui::Frame::new()
                .fill(palette.paper)
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 170),
                ))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::symmetric(
                    if compact_brand { 10 } else { 20 },
                    if compact_chrome { 7 } else { 12 },
                ))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 6],
                    blur: 18,
                    spread: 0,
                    color: egui::Color32::from_rgba_unmultiplied(32, 22, 16, 42),
                })
                .show(ui, |ui| {
                    ui.set_min_width(if compact_brand { logo_size } else { 230.0 });
                    ui.horizontal(|ui| {
                        if let Some(texture) = &self.logo_texture {
                            ui.image((texture.id(), egui::vec2(logo_size, logo_size)));
                        }
                        if !compact_brand {
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new("CODEX-ROUTER")
                                    .font(egui::FontId::new(17.0, theme::display_family()))
                                    .color(palette.ink),
                            );
                        }
                    });
                });

            ui.add_space(8.0);
            let current = step_number(self.page);
            let full_step_navigation = viewport_width >= if zh { 1180.0 } else { 1420.0 };
            if full_step_navigation {
                theme::elevated_control_frame(palette, false).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 3.0;
                        ui.spacing_mut().button_padding.x = 10.0;
                        let labels = if zh {
                            ["项目", "登录", "模型", "网络", "部署"]
                        } else {
                            ["PROJECT", "ACCESS", "MODEL", "NETWORK", "DEPLOY"]
                        };
                        for (index, label) in labels.iter().enumerate() {
                            let active = current == index + 1;
                            let complete = current > index + 1
                                || matches!(
                                    self.page,
                                    Page::Dashboard | Page::Profiles | Page::OAuth | Page::Monitor
                                );
                            let color = if active || complete {
                                palette.ink
                            } else {
                                palette.ink_soft
                            };
                            let text = if complete {
                                format!("✅ {label}")
                            } else {
                                (*label).to_string()
                            };
                            let response = ui.add(
                                egui::Button::new(
                                    egui::RichText::new(text).small().strong().color(color),
                                )
                                .fill(if active {
                                    palette.paper
                                } else {
                                    egui::Color32::TRANSPARENT
                                })
                                .stroke(if active {
                                    egui::Stroke::new(1.0_f32, palette.line)
                                } else {
                                    egui::Stroke::NONE
                                })
                                .corner_radius(egui::CornerRadius::same(7)),
                            );
                            if response.clicked() && complete {
                                self.page = match index {
                                    0 => Page::Project,
                                    1 => Page::Auth,
                                    2 => Page::Model,
                                    3 => Page::Proxy,
                                    _ => Page::Finish,
                                };
                            }
                        }
                    });
                });
            } else if viewport_width >= 1060.0 {
                theme::elevated_control_frame(palette, true).show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{}  {} / 5",
                            page_label(self.page, zh),
                            current.min(5)
                        ))
                        .small()
                        .strong()
                        .color(palette.ink),
                    );
                });
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let show_skip_guide = !matches!(
                    self.page,
                    Page::Dashboard | Page::Profiles | Page::OAuth | Page::Monitor
                );
                let skip_guide_clicked = show_skip_guide
                    && Self::topbar_button(
                        ui,
                        if compact_chrome {
                            t(zh, "跳过", "SKIP")
                        } else {
                            t(zh, "跳过引导", "SKIP GUIDE")
                        },
                        true,
                        palette,
                    )
                    .on_hover_text(t(zh, "直接进入控制台", "Open the console directly"))
                    .clicked();
                if skip_guide_clicked {
                    self.page = Page::Dashboard;
                }
                if self
                    .topbar_theme_switch(ui, zh, palette)
                    .on_hover_text(t(zh, "切换界面主题", "Switch interface theme"))
                    .clicked()
                {
                    self.config.ui_theme = if self.config.ui_theme == "sky" {
                        "coffee".to_owned()
                    } else {
                        "sky".to_owned()
                    };
                }
                if Self::topbar_language_switch(ui, zh, palette)
                    .on_hover_text(t(zh, "切换为英文", "Switch to Chinese"))
                    .clicked()
                {
                    self.ui_language = if zh { "en" } else { "zh" }.to_owned();
                }
                let label = if self.update_checking {
                    if compact_chrome {
                        t(zh, "检查中…", "WAIT…")
                    } else {
                        t(zh, "检查中…", "CHECKING…")
                    }
                } else if compact_chrome {
                    t(zh, "更新", "UPDATE")
                } else {
                    t(zh, "检查更新", "CHECK UPDATE")
                };
                if Self::topbar_button(ui, label, !self.update_checking, palette)
                    .on_hover_text(t(
                        zh,
                        "从官方 GitHub Releases 检查新版本",
                        "Check official GitHub Releases for a new version",
                    ))
                    .clicked()
                {
                    self.check_for_updates();
                }
                if self.page == Page::Dashboard
                    && Self::topbar_button(
                        ui,
                        if compact_chrome {
                            t(zh, "代理", "PROXY")
                        } else {
                            t(zh, "网络代理", "NETWORK PROXY")
                        },
                        true,
                        palette,
                    )
                    .on_hover_text(t(
                        zh,
                        "配置系统代理、环境变量代理和直连规则",
                        "Configure system proxies, environment proxies, and direct-connect rules",
                    ))
                    .clicked()
                {
                    self.proxy_from_wizard = false;
                    self.page = Page::Proxy;
                }
            });
        });
    }

    fn show_update_dialog(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let info = self.update_info.clone().unwrap_or_default();
        let mut open = true;
        let mut close = false;
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(600.0, 360.0),
            egui::vec2(440.0, 280.0),
        );
        egui::Window::new(t(zh, "Codex-Router 更新", "Codex-Router update"))
            .id(egui::Id::new("github-update-dialog"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size(dialog_size)
            .collapsible(false)
            .resizable(false)
            .vscroll(true)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .open(&mut open)
            .frame(
                egui::Frame::new()
                    .fill(palette.paper)
                    .stroke(egui::Stroke::new(1.0, palette.line))
                    .inner_margin(egui::Margin::same(22)),
            )
            .show(ctx, |ui| {
                let (title, detail) = match info.status.as_str() {
                    "update_available" => (
                        format!(
                            "{} {}",
                            t(zh, "发现新版本", "New version available"),
                            info.latest_version
                        ),
                        t(
                            zh,
                            "是否从官方 GitHub Release 下载更新包？下载后不会自动覆盖当前程序。",
                            "Download the package from the official GitHub Release? It will not overwrite the running app automatically.",
                        )
                        .to_owned(),
                    ),
                    "current" => (
                        t(zh, "当前已是最新版本", "You are up to date").to_owned(),
                        t(
                            zh,
                            "当前安装版本与 GitHub 最新 Release 一致。",
                            "The installed version matches the latest GitHub Release.",
                        )
                        .to_owned(),
                    ),
                    "no_release" => (
                        t(zh, "GitHub 暂无可下载版本", "No GitHub Release yet").to_owned(),
                        t(
                            zh,
                            "官方仓库目前还没有发布 Release；源代码仓库仍可正常访问。",
                            "The official repository has not published a Release yet; the source repository is available.",
                        )
                        .to_owned(),
                    ),
                    "private_auth_required" => (
                        t(zh, "需要 GitHub 私有仓库授权", "Private GitHub access required")
                            .to_owned(),
                        t(
                            zh,
                            "当前仓库处于私有发布阶段。请安装 GitHub CLI 并登录后重试；仓库公开后普通用户无需 GitHub CLI。",
                            "This repository is currently private. Install GitHub CLI and sign in, then retry. Public releases do not require GitHub CLI.",
                        )
                        .to_owned(),
                    ),
                    "downloaded" => (
                        t(zh, "更新包已下载", "Update downloaded").to_owned(),
                        t(
                            zh,
                            "请关闭 Codex-Router 后解压或运行更新包。现有配置和数据不会被自动删除。",
                            "Close Codex-Router before extracting or running the package. Existing configuration and data are not deleted automatically.",
                        )
                        .to_owned(),
                    ),
                    _ => (
                        t(zh, "无法检查更新", "Update check failed").to_owned(),
                        if info.message.is_empty() {
                            t(
                                zh,
                                "无法连接官方 GitHub。",
                                "Could not reach the official GitHub repository.",
                            )
                            .to_owned()
                        } else {
                            friendly_error(&info.message, zh)
                        },
                    ),
                };
                ui.label(
                    egui::RichText::new(title)
                        .font(egui::FontId::new(23.0, theme::display_family()))
                        .color(palette.ink),
                );
                ui.label(egui::RichText::new(detail).color(palette.ink_soft));
                ui.add_space(10.0);
                ui.horizontal_wrapped(|ui| {
                    let current_version = if info.current_version.is_empty() {
                        APP_VERSION
                    } else {
                        &info.current_version
                    };
                    ui.label(format!(
                        "{}: {}",
                        t(zh, "当前版本", "Current"),
                        current_version
                    ));
                    if !info.latest_version.is_empty() {
                        ui.separator();
                        ui.label(format!(
                            "{}: {}",
                            t(zh, "最新版本", "Latest"),
                            info.latest_version
                        ));
                    }
                });
                if !info.release_notes.trim().is_empty() {
                    ui.add_space(8.0);
                    theme::field_label(
                        ui,
                        t(zh, "发布说明", "RELEASE NOTES"),
                        &info.release_name,
                        palette,
                    );
                    egui::ScrollArea::vertical()
                        .id_salt("github-release-notes")
                        .max_height(145.0)
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(&info.release_notes)
                                    .small()
                                    .color(palette.ink_soft),
                            );
                        });
                } else {
                    ui.add_space(78.0);
                }
                if !info.message.is_empty() && info.status == "update_available" {
                    ui.label(
                        egui::RichText::new(&info.message)
                            .small()
                            .color(palette.muted),
                    );
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if info.status == "update_available" && !info.download_url.is_empty() {
                        let label = if self.update_downloading {
                            t(zh, "正在下载…", "Downloading…")
                        } else {
                            t(zh, "下载更新", "Download update")
                        };
                        if ui
                            .add_enabled(
                                !self.update_downloading,
                                egui::Button::new(
                                    egui::RichText::new(label).color(egui::Color32::WHITE),
                                )
                                .fill(palette.action),
                            )
                            .clicked()
                        {
                            self.download_update(&info);
                        }
                    }
                    if info.status == "downloaded"
                        && theme::primary_button(
                            ui,
                            egui::RichText::new(t(zh, "打开下载位置", "Open download location"))
                                .color(egui::Color32::WHITE),
                            palette,
                        )
                        .clicked()
                    {
                        self.open_download_location(&info.download_path);
                    }
                    if theme::secondary_button(
                        ui,
                        t(zh, "打开官方 GitHub", "Open official GitHub"),
                        palette,
                    )
                    .clicked()
                    {
                        self.open_update_url(&info.release_url);
                    }
                    if theme::secondary_button(ui, t(zh, "关闭", "Close"), palette).clicked() {
                        close = true;
                    }
                });
            });
        if close || !open {
            self.update_dialog_open = false;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn magazine_cover(
        &mut self,
        ui: &mut egui::Ui,
        step: &str,
        title: &str,
        italic: &str,
        summary: &str,
        palette: &theme::Palette,
        compact: bool,
        target_height: f32,
    ) {
        theme::paper_frame(palette).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height((target_height - 56.0).max(if compact { 220.0 } else { 550.0 }));
            ui.vertical_centered(|ui| {
                ui.add_space(if compact { 8.0 } else { 48.0 });
                ui.label(
                    egui::RichText::new("▪ ▪ ▪")
                        .font(egui::FontId::new(18.0, theme::display_family()))
                        .color(palette.ink),
                );
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(title)
                        .font(egui::FontId::new(
                            if compact { 31.0 } else { 43.0 },
                            theme::display_family(),
                        ))
                        .color(palette.ink),
                );
                ui.label(
                    egui::RichText::new(italic)
                        .font(egui::FontId::new(
                            if compact { 21.0 } else { 29.0 },
                            theme::serif_family(),
                        ))
                        .italics()
                        .color(palette.ink_soft),
                );
                ui.add_space(16.0);
                ui.label(
                    egui::RichText::new(summary)
                        .size(14.0)
                        .color(palette.ink_soft),
                );
                if self.page == Page::Model {
                    ui.add_space(if compact { 12.0 } else { 38.0 });
                    theme::field_label(
                        ui,
                        t(
                            self.ui_language == "zh",
                            "常见 API 渠道",
                            "COMMON API CHANNELS",
                        ),
                        t(
                            self.ui_language == "zh",
                            "选择后自动填写，右侧仍可修改",
                            "Auto-fill now; edit any field on the right",
                        ),
                        palette,
                    );
                    let zh = self.ui_language == "zh";
                    let current_preset = super::logic::channel_presets()
                        .iter()
                        .find(|preset| {
                            self.temp_model.base_url.trim_end_matches('/')
                                == preset.base_url.trim_end_matches('/')
                                && self.temp_model.model == preset.model
                        });
                    if let Some(preset) = current_preset {
                        let provider_name = if zh {
                            preset.label_zh
                        } else {
                            preset.label_en
                        };
                        ui.hyperlink_to(
                            format!(
                                "{}: {provider_name} ↗",
                                t(zh, "1. 访问渠道官网", "1. Provider website")
                            ),
                            preset.website_url,
                        );
                        ui.hyperlink_to(
                            format!(
                                "{} ↗",
                                t(zh, "2. 打开渠道配置说明文档", "2. Configuration guide")
                            ),
                            preset.docs_url,
                        );
                        ui.add_space(8.0);
                    } else {
                        ui.label(
                            egui::RichText::new(t(
                                zh,
                                "当前是自定义渠道；请以渠道提供方的官网和文档为准",
                                "Custom provider: follow the provider's own website and documentation",
                            ))
                            .small()
                            .color(palette.muted),
                        );
                        ui.add_space(8.0);
                    }
                    let selected_text = current_preset
                        .map(|preset| if zh { preset.label_zh } else { preset.label_en })
                        .unwrap_or(t(zh, "选择渠道…", "Choose a provider…"));
                    let mut selected_preset = None;
                    egui::ComboBox::from_id_salt("model-channel-preset")
                        .selected_text(selected_text)
                        .width(ui.available_width().max(180.0))
                        .show_ui(ui, |ui| {
                            for preset in super::logic::common_channel_presets() {
                                let label = if zh { preset.label_zh } else { preset.label_en };
                                if ui
                                    .selectable_label(
                                        self.temp_model.base_url.trim_end_matches('/')
                                            == preset.base_url.trim_end_matches('/')
                                            && self.temp_model.model == preset.model,
                                        label,
                                    )
                                    .on_hover_text(format!("{}\n{}", preset.model, preset.base_url))
                                    .clicked()
                                {
                                    selected_preset = Some(preset.id);
                                }
                            }
                        });
                    if let Some(preset_id) = selected_preset {
                        super::logic::apply_channel_preset(&mut self.temp_model, preset_id);
                        self.status_text = t(
                            zh,
                            "已应用渠道推荐配置；请填写 API Key 后保存",
                            "Provider preset applied. Enter the API key, then save.",
                        )
                        .into();
                    }
                    ui.add_space(8.0);
                    if ui
                        .add_sized(
                            [ui.available_width(), 42.0],
                            egui::Button::new(
                                egui::RichText::new(t(
                                    zh,
                                    "API 推荐平台",
                                    "Recommended API platforms",
                                ))
                                .strong()
                                .color(egui::Color32::WHITE),
                            )
                            .fill(palette.accent)
                            .stroke(egui::Stroke::NONE)
                            .corner_radius(egui::CornerRadius::same(6)),
                        )
                        .on_hover_text(t(
                            zh,
                            "查看 Codex-Router 推荐并已适配的平台",
                            "Browse platforms recommended and integrated by Codex-Router",
                        ))
                        .clicked()
                    {
                        self.recommended_platform_dialog_open = true;
                    }
                }
            });
            if !compact {
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    theme::eyebrow(ui, step, palette.muted);
                });
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn wizard_layout<F>(
        &mut self,
        ui: &mut egui::Ui,
        step: &str,
        title: &str,
        italic: &str,
        summary: &str,
        palette: &theme::Palette,
        form: F,
    ) where
        F: FnOnce(&mut Self, &mut egui::Ui, &theme::Palette, f32),
    {
        let wide = ui.available_width() >= 1000.0;
        let height = ui.available_height();
        if wide {
            let width = ui.available_width();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(width * 0.34, height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        self.magazine_cover(
                            ui,
                            step,
                            title,
                            italic,
                            summary,
                            palette,
                            height < 600.0,
                            height,
                        )
                    },
                );
                ui.add_space(16.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(width * 0.66 - 28.0, height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| form(self, ui, palette, height),
                );
            });
        } else {
            form(self, ui, palette, height);
        }
    }

    fn panel_heading(ui: &mut egui::Ui, kicker: &str, title: &str, palette: &theme::Palette) {
        theme::eyebrow(ui, kicker, palette.background_dark);
        ui.label(
            egui::RichText::new(title)
                .font(egui::FontId::new(25.0, theme::display_family()))
                .color(palette.ink),
        );
        ui.add_space(5.0);
    }

    fn navigation_row(
        ui: &mut egui::Ui,
        back: &str,
        next: &str,
        next_enabled: bool,
        palette: &theme::Palette,
    ) -> (bool, bool) {
        let mut back_clicked = false;
        let mut next_clicked = false;
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.horizontal(|ui| {
                back_clicked = theme::secondary_button(ui, back, palette).clicked();
                let response = ui.add_enabled_ui(next_enabled, |ui| {
                    theme::primary_button(
                        ui,
                        egui::RichText::new(next)
                            .strong()
                            .color(egui::Color32::WHITE),
                        palette,
                    )
                });
                next_clicked = response.inner.clicked();
            });
        });
        (back_clicked, next_clicked)
    }

    fn show_welcome(&mut self, ui: &mut egui::Ui, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let wide = ui.available_width() >= 1000.0;
        let short = ui.ctx().content_rect().height() < 840.0;
        let card_height = ui.available_height().max(if short { 520.0 } else { 620.0 });
        if wide {
            let width = ui.available_width();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(width * 0.44, card_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        theme::paper_frame(palette).show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.set_min_height(card_height - 56.0);
                            ui.vertical_centered(|ui| {
                                ui.add_space(if short { 24.0 } else { 58.0 });
                                ui.label(egui::RichText::new("▪ ▪ ▪").size(18.0).color(palette.ink));
                                ui.add_space(12.0);
                                ui.label(
                                    egui::RichText::new("CODEX-ROUTER")
                                        .font(egui::FontId::new(
                                            if short { 41.0 } else { 49.0 },
                                            theme::display_family(),
                                        ))
                                        .color(palette.ink),
                                );
                                ui.label(
                                    egui::RichText::new(t(zh, "为你的模型而生，", "for your models,"))
                                        .font(egui::FontId::new(31.0, theme::serif_family()))
                                        .italics()
                                        .color(palette.ink_soft),
                                );
                                ui.add_space(8.0);
                                ui.label(
                                    egui::RichText::new(t(
                                        zh,
                                        "每个 API，一条本地路由。",
                                        "EVERY API. ONE LOCAL ROUTE.",
                                    ))
                                        .font(egui::FontId::new(19.0, theme::display_family()))
                                        .color(palette.ink),
                                );
                                ui.add_space(if short { 12.0 } else { 24.0 });
                                ui.label(
                                    egui::RichText::new(t(
                                        zh,
                                        "单用户、多模型、多 API 与自动兜底，\n全部由一个安全的本地控制面管理。",
                                        "Multiple models, APIs, and automatic fallback,\nmanaged by one secure local control plane.",
                                    ))
                                        .size(14.0)
                                        .color(palette.ink_soft),
                                );
                                ui.add_space(if short { 16.0 } else { 26.0 });
                                if theme::primary_button(
                                    ui,
                                    egui::RichText::new(t(zh, "开始配置", "START TO CONFIGURE")).strong().color(egui::Color32::WHITE),
                                    palette,
                                )
                                .clicked()
                                {
                                    self.page = Page::Project;
                                }
                            });
                            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                                theme::eyebrow(ui, t(zh, "RUST 原生 · 本地优先 · 密钥安全", "RUST NATIVE · LOCAL FIRST · SECRET SAFE"), palette.muted);
                            });
                        });
                    },
                );
                ui.add_space(16.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(width * 0.56 - 28.0, card_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| self.welcome_glass(ui, palette, false, short, card_height),
                );
            });
        } else {
            let card_height = ui.available_height();
            self.welcome_glass(ui, palette, true, true, card_height);
        }
    }

    fn welcome_glass(
        &mut self,
        ui: &mut egui::Ui,
        palette: &theme::Palette,
        show_action: bool,
        compact: bool,
        target_height: f32,
    ) {
        let zh = self.ui_language == "zh";
        theme::glass_frame(palette).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height((target_height - 52.0).max(if compact { 468.0 } else { 568.0 }));
            ui.horizontal(|ui| {
                if let Some(texture) = &self.logo_texture {
                    ui.image((texture.id(), egui::vec2(60.0, 60.0)));
                }
                ui.vertical(|ui| {
                    theme::eyebrow(ui, t(zh, "本地 AI 控制中心", "THE LOCAL AI CONTROL PLANE"), palette.background_dark);
                    ui.label(
                        egui::RichText::new(if show_action {
                            "CODEX-ROUTER"
                        } else {
                            t(zh, "连接每一个模型", "ASK EVERY MODEL.")
                        })
                        .strong()
                        .color(palette.ink),
                    );
                });
                if show_action {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::primary_button(
                            ui,
                            egui::RichText::new(t(zh, "开始配置", "START"))
                                .strong()
                                .color(egui::Color32::WHITE),
                            palette,
                        )
                        .clicked()
                        {
                            self.page = Page::Project;
                        }
                    });
                }
            });
            ui.add_space(if compact { 12.0 } else { 24.0 });
            theme::dark_glass_frame(palette).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(
                    egui::RichText::new(t(
                        zh,
                        "你好，我会引导你完成第一个模型。\n密钥只进入 Windows 凭据管理器。",
                        "I'll guide you through your first model.\nSecrets only enter Windows Credential Manager.",
                    ))
                    .size(15.0)
                    .color(egui::Color32::WHITE),
                );
            });
            ui.add_space(if compact { 12.0 } else { 22.0 });
            let features = if zh {
                [
                    ("01", "模型路由", "多模型、多 Base URL 与优先级兜底"),
                    ("02", "多模态就绪", "默认开放图片输入，可逐模型关闭"),
                    ("03", "兼容代理", "Clash / V2Ray / SOCKS5 一键接入"),
                    ("04", "本地隔离", "可逆切换配置，凭据按配置独立保存"),
                ]
            } else {
                [
                    ("01", "MODEL ROUTING", "Multiple models, URLs, and priority fallback"),
                    ("02", "VISION READY", "Image input is enabled by default and can be disabled per model"),
                    ("03", "PROXY COMPATIBLE", "One-click Clash / V2Ray / SOCKS5 support"),
                    ("04", "LOCAL PROFILES", "Reversible switching with isolated credentials"),
                ]
            };
            for (number, title, detail) in features {
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_unmultiplied(
                        palette.paper.r(),
                        palette.paper.g(),
                        palette.paper.b(),
                        164,
                    ))
                    .stroke(egui::Stroke::new(1.0_f32, palette.line))
                    .corner_radius(egui::CornerRadius::same(8))
                    .shadow(theme::soft_card_shadow())
                    .inner_margin(egui::Margin::symmetric(16, if compact { 8 } else { 12 }))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(number)
                                    .font(egui::FontId::new(18.0, theme::display_family()))
                                    .color(palette.accent),
                            );
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new(title).strong().color(palette.ink));
                                ui.label(egui::RichText::new(detail).small().color(palette.muted));
                            });
                        });
                    });
            }
        });
    }

    fn show_project(&mut self, ui: &mut egui::Ui, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        self.wizard_layout(
            ui,
            t(zh, "01 / 项目目录", "01 / PROJECT DIRECTORY"),
            t(zh, "选择项目目录", "CHOOSE THE PROJECT"),
            t(zh, "路由从这里开始，", "where routing begins,"),
            t(
                zh,
                "确认便携运行时的位置。程序会从这里管理所有本地服务。",
                "Choose the portable runtime location used to manage local services.",
            ),
            palette,
            |this, ui, palette, form_height| {
                theme::glass_frame(palette).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.set_min_height((form_height - 52.0).max(470.0));
                    Self::panel_heading(ui, t(zh, "第 01 步", "STEP 01"), t(zh, "项目目录", "Project directory"), palette);
                    ui.label(
                        egui::RichText::new(t(
                            zh,
                            "通常无需修改；EXE 放在完整便携包根目录即可自动识别。",
                            "Usually no change is needed when the EXE is in the portable package root.",
                        ))
                        .color(palette.muted),
                    );
                    ui.add_space(18.0);
                    theme::field_label(ui, t(zh, "项目根目录", "PROJECT ROOT"), t(zh, "必填", "Required"), palette);
                    ui.horizontal(|ui| {
                        let available = ui.available_width();
                        ui.allocate_ui_with_layout(
                            egui::vec2((available - 112.0).max(180.0), 46.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                if theme::input(
                                    ui,
                                    &mut this.project_path_input,
                                    t(zh, "选择 Codex-Router 根目录", "Choose the Codex-Router root"),
                                    false,
                                    palette,
                                )
                                .changed()
                                {
                                    this.router_root =
                                        std::path::PathBuf::from(this.project_path_input.trim());
                                }
                            },
                        );
                        if theme::secondary_button(ui, t(zh, "浏览…", "Browse…"), palette).clicked() {
                            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                this.project_path_input = path.display().to_string();
                                this.router_root = path;
                            }
                        }
                    });
                    let valid = this
                        .router_root
                        .join("scripts")
                        .join("Start-Router.ps1")
                        .exists()
                        && this.router_root.join("app").join("sub2api.exe").exists();
                    ui.add_space(14.0);
                    egui::Frame::new()
                        .fill(if valid {
                            egui::Color32::from_rgba_unmultiplied(87, 174, 126, 36)
                        } else {
                            egui::Color32::from_rgba_unmultiplied(190, 54, 51, 30)
                        })
                        .inner_margin(egui::Margin::same(14))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(if valid {
                                    t(zh, "✓ 已识别完整运行环境", "✓ Complete runtime detected")
                                } else {
                                    t(zh, "目录缺少 Start-Router.ps1 或 sub2api.exe", "Start-Router.ps1 or sub2api.exe is missing")
                                })
                                .strong()
                                .color(if valid {
                                    palette.success
                                } else {
                                    palette.danger
                                }),
                            );
                        });
                    ui.add_space(28.0);
                    let (back, next) = Self::navigation_row(
                        ui,
                        t(zh, "返回", "Back"),
                        t(zh, "继续到登录方式 →", "Continue to access →"),
                        valid,
                        palette,
                    );
                    if back {
                        this.page = Page::Welcome;
                    }
                    if next {
                        this.page = Page::Auth;
                    }
                });
            },
        );
    }

    fn show_auth(&mut self, ui: &mut egui::Ui, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        self.wizard_layout(
            ui,
            t(zh, "02 / 登录策略", "02 / ACCESS STRATEGY"),
            t(zh, "选择登录方式", "SELECT THE ACCESS"),
            t(zh, "官方或独立渠道，", "official or independent,"),
            t(
                zh,
                "Codex 始终走本机路由；你只需决定上游渠道的组合。",
                "Codex always uses the local router; choose your upstream channels.",
            ),
            palette,
            |this, ui, palette, form_height| {
                theme::glass_frame(palette).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.set_min_height((form_height - 52.0).max(470.0));
                    Self::panel_heading(
                        ui,
                        t(zh, "第 02 步", "STEP 02"),
                        t(zh, "上游登录方式", "Upstream access"),
                        palette,
                    );
                    let auth_choices = if zh {
                        [
                            (
                                "chatgpt_oauth",
                                "CHATGPT OAUTH + API",
                                "官方账号优先，第三方同名模型自动兜底",
                            ),
                            (
                                "local_api_key",
                                "API CHANNELS ONLY",
                                "只使用你接下来添加的第三方 API 渠道",
                            ),
                        ]
                    } else {
                        [
                            (
                                "chatgpt_oauth",
                                "CHATGPT OAUTH + API",
                                "Official account first, API models as fallback",
                            ),
                            (
                                "local_api_key",
                                "API CHANNELS ONLY",
                                "Only use the third-party API channels you add next",
                            ),
                        ]
                    };
                    let mut login_provider = None;
                    let mut manage_oauth = false;
                    ui.columns(2, |columns| {
                        for (value, title, detail) in auth_choices {
                            let selected = this.config.auth_mode == value;
                            let response = egui::Frame::new()
                                .fill(if selected {
                                    palette.paper
                                } else {
                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 82)
                                })
                                .stroke(egui::Stroke::new(
                                    if selected { 2.0_f32 } else { 1.0_f32 },
                                    if selected {
                                        palette.background_dark
                                    } else {
                                        palette.line
                                    },
                                ))
                                .inner_margin(egui::Margin::same(16))
                                .show(&mut columns[0], |ui| {
                                    ui.horizontal(|ui| {
                                        ui.radio_value(
                                            &mut this.config.auth_mode,
                                            value.into(),
                                            "",
                                        );
                                        ui.vertical(|ui| {
                                            ui.label(
                                                egui::RichText::new(title)
                                                    .strong()
                                                    .color(palette.ink),
                                            );
                                            ui.label(
                                                egui::RichText::new(detail)
                                                    .small()
                                                    .color(palette.muted),
                                            );
                                        });
                                    });
                                });
                            if response.response.clicked() {
                                this.config.auth_mode = value.into();
                            }
                            columns[0].add_space(10.0);
                        }

                        theme::field_label(
                            &mut columns[1],
                            t(zh, "可用 OAuth 登录", "AVAILABLE OAUTH SIGN-INS"),
                            t(
                                zh,
                                "可依次登录多个平台，并在同一配置中同时启用",
                                "Sign in to multiple providers and enable them in one profile",
                            ),
                            palette,
                        );
                        let providers = [
                            ("openai", "OpenAI / ChatGPT"),
                            ("anthropic", "Anthropic / Claude"),
                            ("gemini", "Google / Gemini"),
                            ("antigravity", "Google / Antigravity"),
                            ("grok", "xAI / Grok"),
                        ];
                        let selected_provider = providers
                            .iter()
                            .find(|(id, _)| *id == this.oauth_provider_draft)
                            .map(|(_, label)| *label)
                            .unwrap_or("OpenAI / ChatGPT");
                        egui::ComboBox::from_id_salt("auth-provider-picker")
                            .selected_text(selected_provider)
                            .width(columns[1].available_width())
                            .show_ui(&mut columns[1], |ui| {
                                for (id, label) in providers {
                                    ui.selectable_value(
                                        &mut this.oauth_provider_draft,
                                        id.to_owned(),
                                        label,
                                    );
                                }
                            });
                        columns[1].add_space(10.0);
                        if this.provider_oauth_running {
                            columns[1].horizontal(|ui| {
                                ui.spinner();
                                ui.label(t(zh, "正在等待 OAuth…", "Waiting for OAuth…"));
                            });
                            if theme::secondary_button(
                                &mut columns[1],
                                t(zh, "取消 OAuth", "Cancel OAuth"),
                                palette,
                            )
                            .clicked()
                            {
                                this.cancel_provider_oauth();
                            }
                        } else {
                            let login_response = theme::primary_button(
                                &mut columns[1],
                                egui::RichText::new(t(
                                    zh,
                                    "登录选中平台",
                                    "Sign in to selected provider",
                                ))
                                .strong()
                                .color(egui::Color32::WHITE),
                                palette,
                            );
                            if login_response.clicked() {
                                login_provider = Some(this.oauth_provider_draft.clone());
                            }
                        }
                        let oauth_count =
                            this.config.oauth_account_ids.as_ref().map_or(0, Vec::len);
                        if theme::secondary_button(
                            &mut columns[1],
                            format!(
                                "{} ({oauth_count})",
                                t(zh, "管理已登录账号", "Manage signed-in accounts")
                            ),
                            palette,
                        )
                        .clicked()
                        {
                            manage_oauth = true;
                        }
                    });
                    if let Some(provider) = login_provider {
                        this.config.auth_mode = "chatgpt_oauth".to_owned();
                        this.start_provider_oauth(&provider);
                    }
                    if manage_oauth {
                        this.open_oauth_manager();
                    }
                    ui.add_space(12.0);
                    ui.checkbox(
                        &mut this.config.oauth_fallback.enabled,
                        t(
                            zh,
                            "OAuth 不可用时自动回退到第三方同名模型",
                            "Fall back to an API model when OAuth is unavailable",
                        ),
                    );
                    if this.config.oauth_fallback.enabled {
                        ui.columns(2, |columns| {
                            theme::field_label(
                                &mut columns[0],
                                t(zh, "OAUTH 优先级", "OAUTH PRIORITY"),
                                t(zh, "数值越小越优先", "Lower values run first"),
                                palette,
                            );
                            let official_priority_response = columns[0].add(
                                egui::DragValue::new(
                                    &mut this.config.oauth_fallback.official_priority,
                                )
                                .range(1..=999),
                            );
                            theme::ascii_response(&mut columns[0], &official_priority_response);
                            theme::field_label(
                                &mut columns[1],
                                t(zh, "兜底优先级", "FALLBACK PRIORITY"),
                                t(zh, "兜底顺序", "Fallback order"),
                                palette,
                            );
                            let fallback_priority_response = columns[1].add(
                                egui::DragValue::new(
                                    &mut this.config.oauth_fallback.fallback_priority,
                                )
                                .range(1..=999),
                            );
                            theme::ascii_response(&mut columns[1], &fallback_priority_response);
                        });
                    }
                    ui.add_space(22.0);
                    let (back, next) = Self::navigation_row(
                        ui,
                        t(zh, "← 项目目录", "← Project"),
                        t(zh, "配置第一个模型 →", "Configure first model →"),
                        true,
                        palette,
                    );
                    if back {
                        this.page = Page::Project;
                    }
                    if next {
                        this.temp_model = ModelConfig::default();
                        this.temp_model.priority =
                            super::logic::next_api_channel_priority(&this.config);
                        this.editing_model = None;
                        this.model_from_wizard = true;
                        this.advanced_json_open = false;
                        this.page = Page::Model;
                    }
                });
            },
        );
    }

    fn show_model(&mut self, ui: &mut egui::Ui, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let title = if self.model_from_wizard {
            t(zh, "配置第一个模型", "DESIGN THE FIRST MODEL")
        } else {
            t(zh, "编辑模型", "EDIT THE MODEL")
        };
        self.wizard_layout(
            ui,
            t(zh, "03 / 模型渠道", "03 / MODEL CHANNEL"),
            title,
            t(zh, "一次配置一个模型，", "one model at a time,"),
            t(
                zh,
                "名称决定 Codex 如何选择模型；Base URL 与 Key 决定请求去哪。",
                "The name selects the model; Base URL and key select the destination.",
            ),
            palette,
            |this, ui, palette, form_height| {
                theme::glass_frame(palette).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.set_min_height((form_height - 52.0).max(470.0));
                    egui::ScrollArea::vertical()
                        .id_salt("model-form-scroll")
                        .auto_shrink([false, false])
                        .max_height((form_height - 82.0).max(440.0))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            let json_valid =
                                serde_json::from_str::<serde_json::Value>(&this.temp_model.extra)
                                    .map(|value| value.is_object())
                                    .unwrap_or(false);
                            let oauth_model = this.temp_model.source == "oauth";
                            let valid = !this.temp_model.model.trim().is_empty()
                                && json_valid
                                && (oauth_model
                                    || (!this.temp_model.base_url.trim().is_empty()
                                        && (!this.temp_model.api_key.trim().is_empty()
                                            || !this.temp_model.credential_name.is_empty())));
                            let mut back = false;
                            let mut next = false;
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    Self::panel_heading(
                                        ui,
                                        t(zh, "第 03 步", "STEP 03"),
                                        if this.model_from_wizard {
                                            t(zh, "第一个模型", "First model")
                                        } else {
                                            t(zh, "模型渠道", "Model channel")
                                        },
                                        palette,
                                    );
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let next_label = if this.model_from_wizard {
                                            t(zh, "网络代理 →", "Network proxy →")
                                        } else {
                                            t(zh, "保存模型", "Save model")
                                        };
                                        let response = ui.add_enabled_ui(valid, |ui| {
                                            theme::primary_button(
                                                ui,
                                                egui::RichText::new(next_label)
                                                    .strong()
                                                    .color(egui::Color32::WHITE),
                                                palette,
                                            )
                                        });
                                        next = response.inner.clicked();
                                        back = theme::secondary_button(
                                            ui,
                                            if this.model_from_wizard {
                                                t(zh, "← 登录方式", "← Access")
                                            } else {
                                                t(zh, "取消", "Cancel")
                                            },
                                            palette,
                                        )
                                        .clicked();
                                    },
                                );
                            });
                            let two_columns = ui.available_width() > 610.0;
                            if two_columns {
                                ui.columns(2, |columns| {
                                    theme::field_label(
                                        &mut columns[0],
                                        t(zh, "模型 ID", "MODEL ID"),
                                        t(zh, "用于实际 API 请求", "Used for API requests"),
                                        palette,
                                    );
                                    let model_response = theme::input_ascii(
                                        &mut columns[0],
                                        &mut this.temp_model.model,
                                        t(zh, "例如 kimi-k3", "e.g. kimi-k3"),
                                        false,
                                        palette,
                                    );
                                    if model_response.changed()
                                        && this.temp_model.alias_customized != Some(true)
                                    {
                                        this.temp_model.alias =
                                            super::logic::recommended_model_display_name(
                                                &this.temp_model.model,
                                            );
                                        this.temp_model.alias_customized = Some(false);
                                    }
                                    theme::field_label(
                                        &mut columns[1],
                                        t(zh, "模型名称", "MODEL NAME"),
                                        t(zh, "显示给用户", "Shown to the user"),
                                        palette,
                                    );
                                    let alias_response = theme::input(
                                        &mut columns[1],
                                        &mut this.temp_model.alias,
                                        t(zh, "例如 ChatGPT-5.6-Sol", "e.g. ChatGPT-5.6-Sol"),
                                        false,
                                        palette,
                                    );
                                    if alias_response.changed() {
                                        this.temp_model.alias_customized = Some(true);
                                    }
                                });
                            } else {
                                theme::field_label(
                                    ui,
                                    t(zh, "模型 ID", "MODEL ID"),
                                    t(zh, "用于实际 API 请求", "Used for API requests"),
                                    palette,
                                );
                                let model_response = theme::input_ascii(
                                    ui,
                                    &mut this.temp_model.model,
                                    t(zh, "例如 kimi-k3", "e.g. kimi-k3"),
                                    false,
                                    palette,
                                );
                                if model_response.changed()
                                    && this.temp_model.alias_customized != Some(true)
                                {
                                    this.temp_model.alias =
                                        super::logic::recommended_model_display_name(
                                            &this.temp_model.model,
                                        );
                                    this.temp_model.alias_customized = Some(false);
                                }
                                theme::field_label(
                                    ui,
                                    t(zh, "模型名称", "MODEL NAME"),
                                    t(zh, "显示给用户", "Shown to the user"),
                                    palette,
                                );
                                let alias_response = theme::input(
                                    ui,
                                    &mut this.temp_model.alias,
                                    t(zh, "例如 ChatGPT-5.6-Sol", "e.g. ChatGPT-5.6-Sol"),
                                    false,
                                    palette,
                                );
                                if alias_response.changed() {
                                    this.temp_model.alias_customized = Some(true);
                                }
                            }
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(super::logic::model_routing_explanation(
                                    &this.config,
                                    &this.temp_model,
                                    zh,
                                ))
                                .small()
                                .color(palette.ink_soft),
                            );
                            if two_columns {
                                ui.columns(2, |columns| {
                                    theme::field_label(
                                        &mut columns[0],
                                        "BASE URL",
                                        t(zh, "OpenAI 兼容地址", "OpenAI-compatible endpoint"),
                                        palette,
                                    );
                                    theme::input_ascii(
                                        &mut columns[0],
                                        &mut this.temp_model.base_url,
                                        "https://api.example.com/v1",
                                        false,
                                        palette,
                                    );
                                    theme::field_label(
                                        &mut columns[1],
                                        "API KEY",
                                        t(zh, "写入凭据管理器", "Stored in Credential Manager"),
                                        palette,
                                    );
                                    theme::input_ascii(
                                        &mut columns[1],
                                        &mut this.temp_model.api_key,
                                        if this.temp_model.credential_name.is_empty() {
                                            t(zh, "输入 API Key", "Enter API key")
                                        } else {
                                            t(
                                                zh,
                                                "留空则保留已保存的 Key",
                                                "Leave blank to keep the saved key",
                                            )
                                        },
                                        true,
                                        palette,
                                    );
                                });
                            } else {
                                theme::field_label(
                                    ui,
                                    "BASE URL",
                                    t(zh, "必填 · OpenAI 兼容地址", "Required · OpenAI-compatible"),
                                    palette,
                                );
                                theme::input_ascii(
                                    ui,
                                    &mut this.temp_model.base_url,
                                    "https://api.example.com/v1",
                                    false,
                                    palette,
                                );
                                theme::field_label(
                                    ui,
                                    "API KEY",
                                    t(
                                        zh,
                                        "安全写入 Windows 凭据管理器",
                                        "Stored securely in Windows Credential Manager",
                                    ),
                                    palette,
                                );
                                theme::input_ascii(
                                    ui,
                                    &mut this.temp_model.api_key,
                                    if this.temp_model.credential_name.is_empty() {
                                        t(zh, "输入 API Key", "Enter API key")
                                    } else {
                                        t(
                                            zh,
                                            "留空则保留已保存的 Key",
                                            "Leave blank to keep the saved key",
                                        )
                                    },
                                    true,
                                    palette,
                                );
                            }
                            ui.columns(2, |columns| {
                                theme::field_label(
                                    &mut columns[0],
                                    t(zh, "优先级", "PRIORITY"),
                                    t(zh, "数字越小越优先", "Smaller numbers route first"),
                                    palette,
                                );
                                let priority_response = columns[0].add(
                                    egui::DragValue::new(&mut this.temp_model.priority)
                                        .range(1..=999),
                                );
                                theme::ascii_response(&mut columns[0], &priority_response);
                                theme::field_label(
                                    &mut columns[1],
                                    t(zh, "多模态", "MULTIMODAL"),
                                    t(zh, "图片支持", "Image support"),
                                    palette,
                                );
                                egui::ComboBox::from_id_salt("multimodal")
                                    .selected_text(match this.temp_model.multimodal.as_str() {
                                        "true" => t(zh, "手动支持", "Enabled"),
                                        "false" => t(zh, "手动关闭", "Disabled"),
                                        _ => {
                                            if super::logic::detect_multimodal_defaults(
                                                &this.temp_model.model,
                                            )
                                            .supported
                                            {
                                                t(zh, "自动：支持图片", "Auto: images enabled")
                                            } else {
                                                t(zh, "自动：纯文本", "Auto: text only")
                                            }
                                        }
                                    })
                                    .show_ui(&mut columns[1], |ui| {
                                        ui.selectable_value(
                                            &mut this.temp_model.multimodal,
                                            "auto".into(),
                                            t(
                                                zh,
                                                "按模型文档自动判断",
                                                "Detect from model documentation",
                                            ),
                                        );
                                        ui.selectable_value(
                                            &mut this.temp_model.multimodal,
                                            "true".into(),
                                            t(zh, "支持图片/多模态", "Enable image/multimodal"),
                                        );
                                        ui.selectable_value(
                                            &mut this.temp_model.multimodal,
                                            "false".into(),
                                            t(zh, "不支持图片/多模态", "Disable image/multimodal"),
                                        );
                                    });
                            });
                            let context_defaults =
                                super::logic::detect_context_defaults(&this.temp_model.model);
                            ui.columns(2, |columns| {
                                let source = if zh {
                                    context_defaults.source_zh
                                } else {
                                    context_defaults.source_en
                                };
                                theme::stacked_field_label(
                                    &mut columns[0],
                                    t(zh, "上下文窗口", "CONTEXT WINDOW"),
                                    &format!("{}: {} tokens", source, context_defaults.window),
                                    palette,
                                );
                                let mut documented_default = this.temp_model.context_window <= 0;
                                if columns[0]
                                    .checkbox(
                                        &mut documented_default,
                                        t(zh, "使用模型文档默认值", "Use documented model default"),
                                    )
                                    .changed()
                                {
                                    this.temp_model.context_window = if documented_default {
                                        0
                                    } else if this.temp_model.context_window <= 0 {
                                        context_defaults.window
                                    } else {
                                        this.temp_model.context_window
                                    };
                                }
                                if documented_default {
                                    columns[0].label(format!(
                                        "{} tokens ({})",
                                        context_defaults.window,
                                        t(zh, "文档默认", "documented default")
                                    ));
                                } else {
                                    if this.temp_model.context_window <= 0 {
                                        this.temp_model.context_window = context_defaults.window;
                                    }
                                    let context_response = columns[0].add(
                                        egui::DragValue::new(&mut this.temp_model.context_window)
                                            .range(16_000..=4_000_000)
                                            .speed(1_000.0)
                                            .suffix(" tokens"),
                                    );
                                    theme::ascii_response(&mut columns[0], &context_response);
                                    columns[0].label(t(
                                        zh,
                                        "点击数字可直接键盘输入自定义上下文窗口",
                                        "Click the number to type a custom context window",
                                    ));
                                }

                                theme::stacked_field_label(
                                    &mut columns[1],
                                    t(zh, "自动压缩", "AUTO COMPACTION"),
                                    t(
                                        zh,
                                        "默认 80%，提前保留输出余量",
                                        "80% default leaves conservative output headroom",
                                    ),
                                    palette,
                                );
                                columns[1].add(
                                    egui::Slider::new(
                                        &mut this.temp_model.auto_compact_percent,
                                        60..=90,
                                    )
                                    .suffix("%"),
                                );
                                columns[1].label(format!(
                                    "{} tokens",
                                    super::logic::resolve_auto_compact_token_limit(
                                        &this.temp_model
                                    )
                                ));
                            });
                            let vision = super::logic::resolve_multimodal(&this.temp_model);
                            let multimodal_defaults =
                                super::logic::detect_multimodal_defaults(&this.temp_model.model);
                            theme::pill(
                                ui,
                                if vision {
                                    t(zh, "视觉  多模态已启用", "VISION  Multimodal enabled")
                                } else {
                                    t(zh, "文本  当前按纯文本处理", "TEXT  Text-only mode")
                                },
                                if vision {
                                    egui::Color32::from_rgba_unmultiplied(75, 154, 111, 36)
                                } else {
                                    palette.paper_alt
                                },
                                if vision {
                                    palette.success
                                } else {
                                    palette.muted
                                },
                            );
                            if this.temp_model.multimodal == "auto" {
                                ui.label(
                                    egui::RichText::new(if zh {
                                        multimodal_defaults.source_zh
                                    } else {
                                        multimodal_defaults.source_en
                                    })
                                    .small()
                                    .color(palette.muted),
                                );
                            }
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    theme::field_label(
                                        ui,
                                        t(zh, "高级 JSON", "ADVANCED JSON"),
                                        t(
                                            zh,
                                            "可选；仅在服务要求额外参数时使用",
                                            "Optional; only for provider-specific parameters",
                                        ),
                                        palette,
                                    );
                                    ui.label(
                                        egui::RichText::new(
                                            if this.temp_model.extra.trim() == "{}" {
                                                t(zh, "当前未配置", "Not configured")
                                            } else if json_valid {
                                                t(zh, "已配置 JSON 对象", "JSON object configured")
                                            } else {
                                                t(zh, "当前 JSON 无效", "Current JSON is invalid")
                                            },
                                        )
                                        .small()
                                        .color(
                                            if json_valid {
                                                palette.muted
                                            } else {
                                                palette.danger
                                            },
                                        ),
                                    );
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if theme::secondary_button(
                                            ui,
                                            t(zh, "编辑高级 JSON", "Edit advanced JSON"),
                                            palette,
                                        )
                                        .clicked()
                                        {
                                            this.advanced_json_draft =
                                                this.temp_model.extra.clone();
                                            this.advanced_json_open = true;
                                        }
                                        if theme::secondary_button(
                                            ui,
                                            t(zh, "思考与 Fast", "Reasoning & Fast"),
                                            palette,
                                        )
                                        .clicked()
                                        {
                                            let detected = super::logic::detect_reasoning(
                                                &this.temp_model.model,
                                            );
                                            let manual = this.temp_model.reasoning_mode == "manual";
                                            this.reasoning_mode_draft = if manual {
                                                "manual".to_owned()
                                            } else {
                                                "auto".to_owned()
                                            };
                                            this.reasoning_levels_draft = if manual {
                                                this.temp_model.reasoning_levels.join(", ")
                                            } else {
                                                detected.levels.join(", ")
                                            };
                                            this.reasoning_default_draft = if manual {
                                                this.temp_model.default_reasoning_level.clone()
                                            } else {
                                                detected.default_level.clone()
                                            };
                                            this.reasoning_fast_supported_draft = if manual {
                                                this.temp_model.fast_supported
                                            } else {
                                                detected.supports_fast
                                            };
                                            this.reasoning_fast_mode_draft =
                                                this.temp_model.fast_mode;
                                            this.reasoning_open = true;
                                        }
                                    },
                                );
                            });
                            if !json_valid {
                                ui.label(
                                    egui::RichText::new(t(
                                        zh,
                                        "其它参数必须是 JSON 对象，例如 {}",
                                        "Advanced parameters must be a JSON object, e.g. {}",
                                    ))
                                    .color(palette.danger),
                                );
                            }
                            if back {
                                this.page = if this.model_from_wizard {
                                    Page::Auth
                                } else {
                                    Page::Dashboard
                                };
                            }
                            if next {
                                if this.model_from_wizard {
                                    this.config.models = vec![this.temp_model.clone()];
                                    super::logic::normalize_default_model(&mut this.config);
                                    this.proxy_from_wizard = true;
                                    this.page = Page::Proxy;
                                } else {
                                    match this.editing_model {
                                        Some(index) => {
                                            let was_default = this.config.default_model
                                                == this.config.models[index].model;
                                            this.config.models[index] = this.temp_model.clone();
                                            if was_default {
                                                this.config.default_model =
                                                    this.temp_model.model.clone();
                                            }
                                        }
                                        None => this.config.models.push(this.temp_model.clone()),
                                    }
                                    super::logic::normalize_default_model(&mut this.config);
                                    this.page = Page::Dashboard;
                                }
                            }
                            ui.add_space(8.0);
                        });
                });
            },
        );
        if self.advanced_json_open {
            self.show_advanced_json_modal(ui.ctx(), palette);
        }
        if self.reasoning_open {
            self.show_reasoning_modal(ui.ctx(), palette);
        }
    }

    fn show_reasoning_modal(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let detected = super::logic::detect_reasoning(&self.temp_model.model);
        let parse_levels = |raw: &str| {
            raw.split(|character: char| {
                character == ',' || character == '，' || character.is_whitespace()
            })
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
        };
        let levels = parse_levels(&self.reasoning_levels_draft);
        let default_level = self.reasoning_default_draft.trim().to_ascii_lowercase();
        let invalid_levels = levels
            .iter()
            .filter(|value| !super::logic::is_valid_reasoning_level(value))
            .cloned()
            .collect::<Vec<_>>();
        let manual_valid =
            !levels.is_empty() && invalid_levels.is_empty() && levels.contains(&default_level);
        let manual = self.reasoning_mode_draft == "manual";
        let supports_fast = if manual {
            self.reasoning_fast_supported_draft
        } else {
            detected.supports_fast
        };
        let modal_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(720.0, 560.0),
            egui::vec2(480.0, 300.0),
        );
        let mut cancel_clicked = false;
        let mut apply_clicked = false;
        let response = egui::Modal::new(egui::Id::new("codex-router-reasoning-modal"))
            .backdrop_color(egui::Color32::from_black_alpha(150))
            .frame(
                egui::Frame::new()
                    .fill(palette.paper)
                    .stroke(egui::Stroke::new(1.0, palette.line))
                    .corner_radius(egui::CornerRadius::same(10))
                    .inner_margin(egui::Margin::same(22))
                    .shadow(theme::soft_card_shadow()),
            )
            .show(ctx, |ui| {
                ui.set_width((modal_size.x - 44.0).max(360.0));
                egui::ScrollArea::vertical()
                    .id_salt("reasoning-modal-scroll")
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(
                        egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                    )
                    .max_height((modal_size.y - 44.0).max(220.0))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(
                    egui::RichText::new(t(zh, "思考强度与 Fast", "Reasoning effort & Fast"))
                        .font(egui::FontId::new(24.0, theme::display_family()))
                        .color(palette.ink),
                );
                ui.label(
                    egui::RichText::new(t(
                        zh,
                        "默认按模型名称匹配官方档位；只有上游文档明确不同时才需要手动配置。",
                        "Official levels are matched by model name; use manual settings only when your provider differs.",
                    ))
                    .color(palette.muted),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut self.reasoning_mode_draft,
                        "auto".to_owned(),
                        t(zh, "按官方文档自动匹配", "Official automatic preset"),
                    );
                    ui.selectable_value(
                        &mut self.reasoning_mode_draft,
                        "manual".to_owned(),
                        t(zh, "手动配置", "Manual"),
                    );
                });
                ui.add_space(10.0);
                if self.reasoning_mode_draft == "auto" {
                    theme::field_label(
                        ui,
                        if zh { detected.family_zh } else { detected.family_en },
                        if zh { detected.source_zh } else { detected.source_en },
                        palette,
                    );
                    ui.label(format!(
                        "{}: {}",
                        t(zh, "支持档位", "Supported levels"),
                        detected.levels.join(", ")
                    ));
                    ui.label(format!(
                        "{}: {}",
                        t(zh, "默认档位", "Default level"),
                        detected.default_level
                    ));
                } else {
                    theme::field_label(
                        ui,
                        t(zh, "支持档位", "SUPPORTED LEVELS"),
                        t(
                            zh,
                            "可手填，例如 minimal / low / medium / high / xhigh；以模型或渠道文档为准",
                            "Enter values such as minimal / low / medium / high / xhigh; follow the model or provider docs",
                        ),
                        palette,
                    );
                    theme::input_ascii(
                        ui,
                        &mut self.reasoning_levels_draft,
                        "e.g. minimal, low, medium, high, xhigh",
                        false,
                        palette,
                    );
                    theme::field_label(
                        ui,
                        t(zh, "默认档位", "DEFAULT LEVEL"),
                        t(
                            zh,
                            "手填一个支持档位，例如 minimal 或 xhigh",
                            "Enter one supported value, for example minimal or xhigh",
                        ),
                        palette,
                    );
                    theme::input_ascii(
                        ui,
                        &mut self.reasoning_default_draft,
                        "e.g. medium",
                        false,
                        palette,
                    );
                    ui.checkbox(
                        &mut self.reasoning_fast_supported_draft,
                        t(zh, "此模型/渠道支持 Fast", "This model/provider supports Fast"),
                    );
                    if !manual_valid {
                        let message = if !invalid_levels.is_empty() {
                            format!(
                                "{}: {}",
                                t(zh, "Codex 无法识别", "Not recognized by Codex"),
                                invalid_levels.join(", ")
                            )
                        } else {
                            t(
                                zh,
                                "请填写至少一个档位，并确保默认档位包含在其中。",
                                "Enter at least one level and include the default in that list.",
                            )
                            .to_owned()
                        };
                        ui.label(egui::RichText::new(message).color(palette.danger));
                    }
                }
                ui.add_space(10.0);
                let fast = ui.add_enabled_ui(supports_fast, |ui| {
                    ui.checkbox(
                        &mut self.reasoning_fast_mode_draft,
                        t(zh, "新窗口默认开启 Fast", "Enable Fast for new tasks by default"),
                    )
                });
                if !supports_fast {
                    fast.response.on_hover_text(t(
                        zh,
                        "当前模型档案不支持 Fast",
                        "The current model preset does not support Fast",
                    ));
                }
                ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            cancel_clicked = theme::secondary_button(
                                ui,
                                t(zh, "取消", "Cancel"),
                                palette,
                            )
                            .clicked();
                            let can_apply =
                                self.reasoning_mode_draft == "auto" || manual_valid;
                            let apply = ui.add_enabled_ui(can_apply, |ui| {
                                theme::primary_button(
                                    ui,
                                    egui::RichText::new(t(zh, "应用", "Apply"))
                                        .strong()
                                        .color(egui::Color32::WHITE),
                                    palette,
                                )
                            });
                            apply_clicked = apply.inner.clicked();
                        });
                    });
            });

        if apply_clicked {
            self.temp_model.reasoning_mode = self.reasoning_mode_draft.clone();
            if manual {
                let mut normalized = Vec::new();
                for value in levels {
                    if !normalized.contains(&value) {
                        normalized.push(value);
                    }
                }
                self.temp_model.reasoning_levels = normalized;
                self.temp_model.default_reasoning_level = default_level;
                self.temp_model.fast_supported = self.reasoning_fast_supported_draft;
            } else {
                self.temp_model.reasoning_levels.clear();
                self.temp_model.default_reasoning_level.clear();
                self.temp_model.fast_supported = false;
            }
            self.temp_model.fast_mode = supports_fast && self.reasoning_fast_mode_draft;
            self.reasoning_open = false;
        } else if cancel_clicked || response.should_close() {
            self.reasoning_open = false;
        }
    }

    fn show_advanced_json_modal(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let bounds = ctx.content_rect();
        let modal_size = fit_dialog_size(
            bounds.size(),
            egui::vec2(760.0, 520.0),
            egui::vec2(480.0, 300.0),
        );
        let json_valid = serde_json::from_str::<serde_json::Value>(&self.advanced_json_draft)
            .map(|value| value.is_object())
            .unwrap_or(false);
        let mut cancel_clicked = false;
        let mut apply_clicked = false;
        let mut format_clicked = false;
        let response = egui::Modal::new(egui::Id::new("codex-router-advanced-json-modal"))
            .backdrop_color(egui::Color32::from_black_alpha(150))
            .frame(
                egui::Frame::new()
                    .fill(palette.paper)
                    .stroke(egui::Stroke::new(1.0, palette.line))
                    .corner_radius(egui::CornerRadius::same(10))
                    .inner_margin(egui::Margin::same(22))
                    .shadow(theme::soft_card_shadow()),
            )
            .show(ctx, |ui| {
                ui.set_width((modal_size.x - 44.0).max(360.0));
                egui::ScrollArea::vertical()
                    .id_salt("advanced-json-modal-scroll")
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                    .max_height((modal_size.y - 44.0).max(220.0))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(
                            egui::RichText::new(t(zh, "编辑高级 JSON", "Edit advanced JSON"))
                                .font(egui::FontId::new(24.0, theme::display_family()))
                                .color(palette.ink),
                        );
                        ui.label(
                            egui::RichText::new(t(
                                zh,
                                "可选。仅填写上游服务明确要求的额外参数，内容必须是 JSON 对象。",
                                "Optional. Add only provider-required parameters as a JSON object.",
                            ))
                            .color(palette.muted),
                        );
                        ui.add_space(12.0);
                        theme::multiline_ascii(
                            ui,
                            &mut self.advanced_json_draft,
                            "{}",
                            12,
                            palette,
                        );
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(if json_valid {
                                t(zh, "JSON 对象格式有效", "Valid JSON object")
                            } else {
                                t(
                                    zh,
                                    "请输入有效的 JSON 对象，例如 {}",
                                    "Enter a valid JSON object, e.g. {}",
                                )
                            })
                            .color(if json_valid {
                                palette.success
                            } else {
                                palette.danger
                            }),
                        );
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            cancel_clicked =
                                theme::secondary_button(ui, t(zh, "取消", "Cancel"), palette)
                                    .clicked();
                            let format = ui.add_enabled_ui(json_valid, |ui| {
                                theme::secondary_button(ui, t(zh, "格式化", "Format"), palette)
                            });
                            format_clicked = format.inner.clicked();
                            let apply = ui.add_enabled_ui(json_valid, |ui| {
                                theme::primary_button(
                                    ui,
                                    egui::RichText::new(t(zh, "应用", "Apply"))
                                        .strong()
                                        .color(egui::Color32::WHITE),
                                    palette,
                                )
                            });
                            apply_clicked = apply.inner.clicked();
                        });
                    });
            });

        if format_clicked {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&self.advanced_json_draft)
            {
                if let Ok(formatted) = serde_json::to_string_pretty(&value) {
                    self.advanced_json_draft = formatted;
                }
            }
        }
        if apply_clicked {
            self.temp_model.extra = self.advanced_json_draft.clone();
            self.advanced_json_open = false;
        } else if cancel_clicked || response.should_close() {
            self.advanced_json_open = false;
        }
    }

    fn show_proxy(&mut self, ui: &mut egui::Ui, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        self.wizard_layout(
            ui,
            t(zh, "04 / 网络代理", "04 / NETWORK PROXY"),
            t(zh, "连接网络", "CONNECT THE NETWORK"),
            t(zh, "你的路由，你的规则，", "your route, your rules,"),
            t(zh, "默认自动遵循当前电脑的系统代理与分流规则；也可手动指定通用 HTTP、HTTPS 或 SOCKS 代理。", "By default, Router follows this computer's system proxy and routing rules. You can also provide a standard HTTP, HTTPS, or SOCKS proxy."),
            palette,
            |this, ui, palette, form_height| {
                theme::glass_frame(palette).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.set_min_height((form_height - 52.0).max(470.0));
                    egui::ScrollArea::vertical()
                        .id_salt("proxy-form-scroll")
                        .auto_shrink([false, false])
                        .max_height((form_height - 82.0).max(440.0))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            Self::panel_heading(
                                ui,
                                t(zh, "第 04 步", "STEP 04"),
                                t(zh, "网络代理", "Network proxy"),
                                palette,
                            );
                            egui::Frame::new()
                                .fill(palette.paper)
                                .stroke(egui::Stroke::new(1.0_f32, palette.line))
                                .shadow(theme::soft_card_shadow())
                                .inner_margin(egui::Margin::same(16))
                                .show(ui, |ui| {
                            let mut proxy_mode = if this.config.proxy.enabled {
                                "manual"
                            } else if this.config.proxy.auto_detect {
                                "auto"
                            } else {
                                "direct"
                            };
                            ui.horizontal(|ui| {
                                ui.selectable_value(
                                    &mut proxy_mode,
                                    "auto",
                                    t(zh, "自动", "Auto"),
                                );
                                ui.selectable_value(
                                    &mut proxy_mode,
                                    "manual",
                                    t(zh, "手动", "Manual"),
                                );
                                ui.selectable_value(
                                    &mut proxy_mode,
                                    "direct",
                                    t(zh, "直连", "Direct"),
                                );
                            });
                            this.config.proxy.enabled = proxy_mode == "manual";
                            this.config.proxy.auto_detect = proxy_mode == "auto";
                            ui.label(
                                egui::RichText::new(t(zh, "自动模式遵循当前用户的系统代理与分流规则", "Auto follows the current user's system proxy and routing rules"))
                                    .small()
                                    .color(palette.muted),
                            );
                            if this.config.proxy.enabled {
                                ui.add_space(10.0);
                                ui.columns(3, |columns| {
                                    theme::field_label(&mut columns[0], t(zh, "协议", "PROTOCOL"), "", palette);
                                    egui::ComboBox::from_id_salt("proxy-type")
                                        .selected_text(&this.config.proxy.proxy_type)
                                        .show_ui(&mut columns[0], |ui| {
                                            for value in ["http", "https", "socks5", "socks5h"] {
                                                ui.selectable_value(
                                                    &mut this.config.proxy.proxy_type,
                                                    value.into(),
                                                    value,
                                                );
                                            }
                                        });
                                    theme::field_label(&mut columns[1], t(zh, "主机", "HOST"), "", palette);
                                    theme::input_ascii(
                                        &mut columns[1],
                                        &mut this.config.proxy.host,
                                        "127.0.0.1",
                                        false,
                                        palette,
                                    );
                                    theme::field_label(&mut columns[2], t(zh, "端口", "PORT"), "", palette);
                                    theme::input_ascii(
                                        &mut columns[2],
                                        &mut this.config.proxy.port,
                                        "7890",
                                        false,
                                        palette,
                                    );
                                });
                                ui.columns(2, |columns| {
                                    theme::stacked_field_label(
                                        &mut columns[0],
                                        t(zh, "用户名", "USERNAME"),
                                        t(zh, "可选", "Optional"),
                                        palette,
                                    );
                                    theme::input_ascii(
                                        &mut columns[0],
                                        &mut this.config.proxy.username,
                                        t(zh, "代理用户名", "Proxy username"),
                                        false,
                                        palette,
                                    );
                                    theme::stacked_field_label(
                                        &mut columns[1],
                                        t(zh, "密码", "PASSWORD"),
                                        t(zh, "可选", "Optional"),
                                        palette,
                                    );
                                    theme::input_ascii(
                                        &mut columns[1],
                                        &mut this.config.proxy.password,
                                        t(zh, "留空保留已保存密码", "Leave blank to keep saved password"),
                                        true,
                                        palette,
                                    );
                                });
                            }
                                });
                            let back_page = if this.proxy_from_wizard {
                                Page::Model
                            } else {
                                Page::Dashboard
                            };
                            let next_page = if this.proxy_from_wizard {
                                Page::Finish
                            } else {
                                Page::Dashboard
                            };
                            let mut back = false;
                            let mut next = false;
                            ui.add_space(12.0);
                            ui.horizontal_wrapped(|ui| {
                                back = theme::secondary_button(
                                    ui,
                                    if this.proxy_from_wizard {
                                        t(zh, "← 模型", "← Model")
                                    } else {
                                        t(zh, "取消", "Cancel")
                                    },
                                    palette,
                                )
                                .clicked();
                                next = theme::primary_button(
                                    ui,
                                    egui::RichText::new(if this.proxy_from_wizard {
                                        t(zh, "完成配置 →", "Finish setup →")
                                    } else {
                                        t(zh, "保存设置", "Save settings")
                                    })
                                    .strong()
                                    .color(egui::Color32::WHITE),
                                    palette,
                                )
                                .clicked();
                            });
                            if back {
                                this.page = back_page;
                            }
                            if next {
                                this.page = next_page;
                            }
                            ui.add_space(4.0);
                        });
                });
            },
        );
    }

    fn show_finish(&mut self, ui: &mut egui::Ui, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        self.wizard_layout(
            ui,
            t(zh, "05 / 部署", "05 / DEPLOY"),
            t(zh, "让配置正式生效", "MAKE IT OPERATIONAL"),
            t(zh, "一键完成，", "one click to finish,"),
            t(zh, "保存凭据、创建渠道并写入 Codex。部署完成后请重启 ChatGPT / Codex，并保持 Codex-Router 在后台或托盘运行。", "Save credentials, create channels, and configure Codex. Restart ChatGPT / Codex after deployment and keep Codex-Router running in the background or tray."),
            palette,
            |this, ui, palette, form_height| {
                theme::glass_frame(palette).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.set_min_height((form_height - 52.0).max(470.0));
                    Self::panel_heading(ui, t(zh, "最后一步", "FINAL STEP"), t(zh, "一键完成配置", "Complete setup"), palette);
                    egui::Frame::new()
                        .fill(palette.paper)
                        .stroke(egui::Stroke::new(1.0_f32, palette.line))
                        .shadow(theme::soft_card_shadow())
                        .inner_margin(egui::Margin::same(18))
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                if this.config.accept_compliance
                                    && this.config.accepted_terms_version
                                        == super::CURRENT_TERMS_VERSION
                                {
                                    ui.label(
                                        egui::RichText::new(t(
                                            zh,
                                            "✓ 已同意《Codex-Router 使用与分发承诺》",
                                            "✓ Codex-Router terms accepted",
                                        ))
                                        .strong()
                                        .color(palette.success),
                                    );
                                } else {
                                    ui.label(
                                        egui::RichText::new(t(
                                            zh,
                                            "尚未同意《Codex-Router 使用与分发承诺》",
                                            "Codex-Router terms have not been accepted",
                                        ))
                                        .strong()
                                        .color(palette.accent),
                                    );
                                }
                            });
                            ui.add_space(8.0);
                            if theme::secondary_button(
                                ui,
                                if this.config.accept_compliance {
                                    t(zh, "查看完整条例", "View full terms")
                                } else {
                                    t(zh, "阅读并同意条例", "Read and accept")
                                },
                                palette,
                            )
                            .clicked()
                            {
                                this.terms_open = true;
                                this.terms_scroll_complete = false;
                                this.terms_scroll_reset_pending = true;
                            }
                            ui.label(
                                egui::RichText::new(t(
                                    zh,
                                    "包含禁止商用、允许保留署名与官方 GitHub 发布地址的分发，以及 Sub2API 专项合规条款。",
                                    "Includes non-commercial use, redistribution with attribution and the official GitHub release URL, and Sub2API requirements.",
                                ))
                                .small()
                                .color(palette.muted),
                            );
                        });
                    if !this.config.accept_compliance {
                        ui.label(egui::RichText::new(t(zh, "请打开条例并滚动到底部，由你本人点击同意；程序不会替你接受。", "Open the terms, scroll to the end, and accept them yourself.")).color(palette.accent));
                    }
                    if !this.status_text.is_empty() {
                        ui.label(egui::RichText::new(&this.status_text).strong().color(if this.status_text.contains("失败") || this.status_text.contains("failed") { palette.danger } else { palette.ink_soft }));
                    }
                    ui.add_space(8.0);
                    if this.applying {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(t(zh, "正在配置本地路由，请稍候…", "Configuring the local router…"));
                        });
                    } else {
                        let response = ui.add_enabled_ui(this.config.accept_compliance, |ui| {
                            theme::primary_button(
                                ui,
                                egui::RichText::new(t(zh, "一键完成配置", "Complete setup")).strong().color(egui::Color32::WHITE),
                                palette,
                            )
                        });
                        if response.inner.clicked() {
                            this.apply_all();
                        }
                    }
                    if !this.logs.is_empty() {
                        ui.add_space(12.0);
                        theme::dark_glass_frame(palette).show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.label(
                                egui::RichText::new(log_excerpt(&this.logs, 6, 110))
                                    .monospace()
                                    .small()
                                    .color(egui::Color32::WHITE),
                            );
                        });
                    }
                    ui.add_space(12.0);
                    ui.horizontal_wrapped(|ui| {
                        if theme::secondary_button(ui, t(zh, "← 网络代理", "← Network proxy"), palette).clicked() { this.page = Page::Proxy; }
                        if this.configured && theme::primary_button(
                            ui,
                            egui::RichText::new(t(zh, "进入控制台 →", "Open console →")).strong().color(egui::Color32::WHITE),
                            palette,
                        ).clicked() { this.page = Page::Dashboard; }
                    });
                });
            },
        );
    }

    fn show_terms_modal(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let terms = if zh { TERMS_ZH } else { TERMS_EN };
        let bounds = ctx.content_rect();
        let modal_size = fit_dialog_size(
            bounds.size(),
            egui::vec2(880.0, 720.0),
            egui::vec2(520.0, 360.0),
        );
        let modal_width = (modal_size.x - 44.0).max(260.0);
        let preparation_error_space = if self.pending_oauth_provider.is_some()
            && !self.provider_oauth_prepare_error.is_empty()
        {
            100.0
        } else {
            0.0
        };
        let scroll_height = (modal_size.y - 190.0 - preparation_error_space).clamp(120.0, 640.0);
        let mut close_clicked = false;
        let mut accept_clicked = false;
        let mut retry_preparation = false;
        let response = egui::Modal::new(egui::Id::new("codex-router-terms-modal"))
            .backdrop_color(egui::Color32::from_black_alpha(150))
            .frame(
                egui::Frame::new()
                    .fill(palette.paper)
                    .stroke(egui::Stroke::new(1.0, palette.line))
                    .corner_radius(egui::CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(22))
                    .shadow(theme::soft_card_shadow()),
            )
            .show(ctx, |ui| {
                ui.set_width(modal_width);
                ui.label(
                    egui::RichText::new(t(
                        zh,
                        "Codex-Router 使用与分发承诺",
                        "Codex-Router Use and Distribution Commitment",
                    ))
                    .font(egui::FontId::new(24.0, theme::display_family()))
                    .color(palette.ink),
                );
                ui.label(
                    egui::RichText::new(t(
                        zh,
                        "请滚动阅读全部条例；到达底部后才能确认。",
                        "Read all terms. Confirmation unlocks at the end.",
                    ))
                    .color(palette.muted),
                );
                ui.add_space(10.0);
                let reset_scroll = self.terms_scroll_reset_pending;
                let scroll = egui::Frame::new()
                    .fill(palette.paper_alt)
                    .stroke(egui::Stroke::new(1.0, palette.line))
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::same(14))
                    .show(ui, |ui| {
                        let mut scroll_area = egui::ScrollArea::vertical()
                            .id_salt("codex-router-terms-scroll")
                            .max_height(scroll_height)
                            .min_scrolled_height(scroll_height)
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                            );
                        if reset_scroll {
                            scroll_area = scroll_area.vertical_scroll_offset(0.0);
                        }
                        scroll_area.show(ui, |ui| {
                                ui.set_width((modal_width - 28.0).max(240.0));
                                for line in terms.lines() {
                                    if let Some(heading) = line.strip_prefix("# ") {
                                        ui.label(
                                            egui::RichText::new(heading)
                                                .size(20.0)
                                                .strong()
                                                .color(palette.ink),
                                        );
                                    } else if let Some(heading) = line.strip_prefix("## ") {
                                        ui.add_space(8.0);
                                        ui.label(
                                            egui::RichText::new(heading)
                                                .size(16.0)
                                                .strong()
                                                .color(palette.accent),
                                        );
                                    } else if line.trim().is_empty() {
                                        ui.add_space(5.0);
                                    } else {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(line)
                                                    .size(14.0)
                                                    .color(palette.ink_soft),
                                            )
                                            .wrap(),
                                        );
                                    }
                                }
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(t(
                                        zh,
                                        "— 条例已到底 —",
                                        "— End of terms —",
                                    ))
                                    .strong()
                                    .color(palette.success),
                                );
                            })
                    })
                    .inner;
                let max_offset = (scroll.content_size.y - scroll.inner_rect.height()).max(0.0);
                if reset_scroll {
                    self.terms_scroll_complete = false;
                    if scroll.content_size.y > 1.0 && scroll.state.offset.y <= 1.0 {
                        self.terms_scroll_reset_pending = false;
                        if max_offset <= 1.0 {
                            self.terms_scroll_complete = true;
                        }
                    }
                } else if max_offset <= 1.0 || scroll.state.offset.y >= max_offset - 12.0 {
                    self.terms_scroll_complete = true;
                }
                ui.add_space(12.0);
                if self.pending_oauth_provider.is_some() {
                    ui.horizontal(|ui| {
                        if self.provider_oauth_preparing {
                            ui.spinner();
                        }
                        ui.label(
                            egui::RichText::new(if self.provider_oauth_preparing {
                                t(
                                    zh,
                                    "正在后台准备安全登录环境；不会提前打开浏览器或发起授权。",
                                    "Preparing the secure sign-in environment in the background. No browser or authorization starts yet.",
                                )
                            } else if self.provider_oauth_prepared_provider.as_deref()
                                == self.pending_oauth_provider.as_deref()
                            {
                                t(
                                    zh,
                                    "安全登录环境已准备好，确认条例后将立即打开官方授权页。",
                                    "The secure sign-in environment is ready. The official authorization page will open after acceptance.",
                                )
                            } else {
                                t(
                                    zh,
                                    "确认条例后将继续准备并打开官方授权页。",
                                    "Preparation will continue after acceptance before opening the official authorization page.",
                                )
                            })
                            .small()
                            .color(palette.muted),
                        );
                    });
                    ui.add_space(6.0);
                    if !self.provider_oauth_prepare_error.is_empty() {
                        ui.label(
                            egui::RichText::new(if zh {
                                format!(
                                    "安全登录环境准备失败：{}",
                                    self.provider_oauth_prepare_error
                                )
                            } else {
                                format!(
                                    "Secure sign-in preparation failed: {}",
                                    self.provider_oauth_prepare_error
                                )
                            })
                            .small()
                            .color(palette.danger),
                        );
                        if theme::secondary_button(
                            ui,
                            t(zh, "重新准备环境", "Retry preparation"),
                            palette,
                        )
                        .clicked()
                        {
                            retry_preparation = true;
                        }
                        ui.add_space(6.0);
                    }
                }
                if !self.terms_scroll_complete {
                    ui.label(
                        egui::RichText::new(t(
                            zh,
                            "继续向下滚动，阅读完整条例后即可确认。",
                            "Continue scrolling to unlock confirmation.",
                        ))
                        .color(palette.accent),
                    );
                }
                ui.horizontal(|ui| {
                    if theme::secondary_button(ui, t(zh, "暂不接受", "Not now"), palette).clicked()
                    {
                        close_clicked = true;
                    }
                    let oauth_ready = if self.pending_oauth_provider.is_some() {
                        oauth_terms_confirmation_ready(
                            self.terms_scroll_complete,
                            self.provider_oauth_preparing,
                            self.provider_oauth_prepared_provider.as_deref(),
                            self.pending_oauth_provider.as_deref(),
                        )
                    } else {
                        self.terms_scroll_complete
                    };
                    let confirm = ui.add_enabled_ui(oauth_ready, |ui| {
                        theme::primary_button(
                            ui,
                            egui::RichText::new(if self.pending_oauth_provider.is_some() {
                                t(
                                    zh,
                                    "我已阅读并同意，开始 OAuth",
                                    "I agree and start OAuth",
                                )
                            } else {
                                t(zh, "我已阅读并同意", "I have read and agree")
                            })
                            .strong()
                            .color(egui::Color32::WHITE),
                            palette,
                        )
                    });
                    if confirm.inner.clicked() {
                        accept_clicked = true;
                    }
                });
            });
        if retry_preparation {
            if let Some(provider) = self.pending_oauth_provider.clone() {
                self.prewarm_provider_oauth(&provider);
            }
        } else if accept_clicked {
            self.config.accept_compliance = true;
            self.config.accepted_terms_version = super::CURRENT_TERMS_VERSION.to_owned();
            self.terms_open = false;
            if let Some(provider) = self.pending_oauth_provider.take() {
                self.continue_provider_oauth_after_terms(provider);
            }
        } else if close_clicked || response.should_close() {
            self.terms_open = false;
            if self.pending_oauth_provider.take().is_some() {
                self.cancel_provider_oauth_preparation(true);
            }
        }
    }

    fn show_codex_account_mode_card(&mut self, ui: &mut egui::Ui, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        theme::paper_frame(palette).show(ui, |ui| {
            theme::eyebrow(ui, "01 / CODEX LOGIN", palette.muted);
            ui.heading(t(zh, "Codex 登录状态", "Codex sign-in"));
            ui.label(
                egui::RichText::new(t(
                    zh,
                    "当前：保持你的 Codex 账号登录",
                    "Current: keep your Codex account sign-in",
                ))
                .strong()
                .color(palette.ink_soft),
            );
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(t(
                        zh,
                        "保存并应用只会增量写入本地 Router 与模型目录，不会切换成第三方 API 登录，也不会删除 auth.json 或聊天记录。",
                        "Save & Apply only writes the local Router route and model catalog. It never switches to third-party API sign-in and never deletes auth.json or chats.",
                    ))
                    .color(palette.ink),
                )
                .wrap(),
            );
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(t(
                    zh,
                    "请先在 Codex 中完成账号登录，再应用 Router；应用后完全重启 Codex 即可看到全部模型。",
                    "Sign in to Codex first, then apply Router. Fully restart Codex afterward to load the full model list.",
                ))
                .small()
                .color(palette.muted),
            );
        });
    }

    fn show_profiles(&mut self, ui: &mut egui::Ui, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let mut restore_original = false;
        let mut restore_previous = false;
        let mut create_request = None;
        let mut apply_profile: Option<IsolationProfile> = None;
        let restore_points =
            super::profiles::list_restore_points(&self.router_root).unwrap_or_default();
        let compact_header = ui.available_width() < 1000.0;
        let mut back_to_console = false;
        let mut back_to_previous = false;
        let mut open_sub2api = false;
        let show_title = |ui: &mut egui::Ui| {
            ui.vertical(|ui| {
                theme::eyebrow(
                    ui,
                    t(
                        zh,
                        "可逆切换 / 多配置",
                        "REVERSIBLE SWITCHING / MULTI-PROFILE",
                    ),
                    palette.paper,
                );
                ui.label(
                    egui::RichText::new(t(zh, "切换配置分组", "SWITCH CONFIGURATION GROUPS"))
                        .font(egui::FontId::new(36.0, theme::display_family()))
                        .color(egui::Color32::WHITE),
                );
            });
        };
        if compact_header {
            ui.vertical(|ui| {
                show_title(ui);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        back_to_console = theme::secondary_button(
                            ui,
                            t(zh, "返回控制台", "Back to console"),
                            palette,
                        )
                        .clicked();
                        back_to_previous =
                            theme::secondary_button(ui, t(zh, "返回上一页", "Back"), palette)
                                .clicked();
                        open_sub2api = theme::secondary_button(
                            ui,
                            t(zh, "Sub2API 高级管理", "Sub2API admin"),
                            palette,
                        )
                        .clicked();
                    });
                });
            });
        } else {
            ui.horizontal(|ui| {
                show_title(ui);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    back_to_console = theme::secondary_button(
                        ui,
                        t(zh, "返回控制台", "Back to console"),
                        palette,
                    )
                    .clicked();
                    back_to_previous =
                        theme::secondary_button(ui, t(zh, "返回上一页", "Back"), palette).clicked();
                    open_sub2api = theme::secondary_button(
                        ui,
                        t(zh, "Sub2API 高级管理", "Sub2API admin"),
                        palette,
                    )
                    .clicked();
                });
            });
        }
        if open_sub2api {
            self.sub2api_intro_open = true;
        }
        if back_to_console {
            self.page = Page::Dashboard;
        } else if back_to_previous {
            self.page = self.profiles_return_page;
        }
        ui.label(
            egui::RichText::new(t(
                zh,
                "每次应用或还原前都会先保存当前 Codex 状态；OAuth 快照使用 Windows 当前用户 DPAPI 加密。",
                "The current Codex state is saved before every apply or restore; OAuth snapshots are protected with Windows per-user DPAPI.",
            ))
            .color(palette.paper),
        );
        if !self.status_text.is_empty() {
            ui.label(
                egui::RichText::new(&self.status_text)
                    .small()
                    .color(palette.paper),
            );
        }
        ui.add_space(10.0);
        self.show_share_codex_state_toggle(ui, palette, zh);
        ui.add_space(10.0);

        egui::ScrollArea::vertical()
            .id_salt("configuration-profiles-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let content_width = ui.available_width();
                let content_height = (ui.ctx().content_rect().height() - 190.0).max(540.0);
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(content_width * 0.39, content_height),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                    // First card: name + create profile (required before first apply).
                    theme::paper_frame(palette).show(ui, |ui| {
                        theme::eyebrow(ui, "01 / LOCAL", palette.muted);
                        ui.heading(t(zh, "应用前配置与本地隔离", "Previous & local"));
                        ui.label(t(
                            zh,
                            "首次保存前请先命名配置分组。建议：先在 Codex 登录账号 → 在此命名并应用 → 完全重启 Codex。未登录 Codex 时也会写入本地 Router（第三方 API 模式）。",
                            "Name a profile before the first save. Recommended: sign in to Codex first → name and apply here → fully restart Codex. Without a Codex login, Router still writes local API mode.",
                        ));
                        ui.add_space(8.0);
                        let response = ui.add_enabled_ui(
                            !self.applying && !restore_points.is_empty(),
                            |ui| {
                                theme::secondary_button(
                                    ui,
                                    if restore_points.is_empty() {
                                        t(zh, "暂无应用前备份", "No restore point")
                                    } else {
                                        t(zh, "返回上一次配置", "Restore previous")
                                    },
                                    palette,
                                )
                            },
                        );
                        if response.inner.clicked() {
                            restore_previous = true;
                        }
                        ui.label(
                            egui::RichText::new(format!(
                                "{} {}",
                                restore_points.len(),
                                t(zh, "个可逆还原点", "reversible restore point(s)")
                            ))
                            .small()
                            .color(palette.muted),
                        );
                        ui.add_space(8.0);
                        theme::input(
                            ui,
                            &mut self.local_profile_name_input,
                            t(zh, "输入新配置名称（例如 工作）", "New profile name (e.g. Work)"),
                            false,
                            palette,
                        );
                        let response = ui.add_enabled_ui(
                            !self.applying && !self.local_profile_name_input.trim().is_empty(),
                            |ui| {
                                theme::primary_button(
                                    ui,
                                    egui::RichText::new(t(
                                        zh,
                                        "新建并应用本地隔离配置",
                                        "Create & apply local profile",
                                    ))
                                    .strong()
                                    .color(egui::Color32::WHITE),
                                    palette,
                                )
                            },
                        );
                        if response.inner.clicked() {
                            create_request = Some((
                                IsolationKind::Local,
                                self.local_profile_name_input.clone(),
                            ));
                        }
                    });

                    self.show_codex_account_mode_card(ui, palette);

                    theme::paper_frame(palette).show(ui, |ui| {
                        theme::eyebrow(ui, "03 / CODEX", palette.muted);
                        ui.heading(t(zh, "初始化 Codex 默认配置", "Reset Codex defaults"));
                        ui.label(t(
                            zh,
                            "移除 Codex-Router 写入的模型提供方、模型目录与推理默认值，让 Codex 回到自己的默认配置与登录流程。你的登录状态、聊天记录、插件、MCP 与权限设置不会被改动，也不会写入任何 Windows 沙箱设置。",
                            "Remove the model provider, model catalog, and reasoning defaults written by Codex-Router so Codex returns to its own defaults and login flow. Your sign-in, chats, plugins, MCP servers, and permissions are left untouched, and no Windows sandbox value is written.",
                        ));
                        ui.add_space(10.0);
                        let response = ui.add_enabled_ui(!self.applying, |ui| {
                            theme::primary_button(
                                ui,
                                egui::RichText::new(t(
                                    zh,
                                    "初始化 Codex 默认配置",
                                    "Reset Codex defaults",
                                ))
                                .strong()
                                .color(egui::Color32::WHITE),
                                palette,
                            )
                        });
                        if response.inner.clicked() {
                            restore_original = true;
                        }
                    });
                        },
                    );
                    ui.add_space(14.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(content_width * 0.61 - 14.0, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                theme::glass_frame(palette).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            theme::eyebrow(
                                ui,
                                t(zh, "已保存配置", "SAVED PROFILES"),
                                palette.background_dark,
                            );
                            ui.label(
                                egui::RichText::new(t(
                                    zh,
                                    "直接采用任意隔离配置",
                                    "APPLY ANY ISOLATED PROFILE",
                                ))
                                .font(egui::FontId::new(23.0, theme::display_family()))
                                .color(palette.ink),
                            );
                        });
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                theme::pill(
                                    ui,
                                    &format!("{:02}", self.isolation_profiles.len()),
                                    palette.paper,
                                    palette.ink,
                                );
                            },
                        );
                    });
                    if self.isolation_profiles.is_empty() {
                        ui.label(t(
                            zh,
                            "还没有隔离配置。请在左侧输入名称并创建本地隔离配置。",
                            "No isolated profiles yet. Enter a name on the left to create one.",
                        ));
                    }
                    for profile in &self.isolation_profiles {
                        let active = profile.id == self.active_profile_id;
                        egui::Frame::new()
                            .fill(palette.paper)
                            .stroke(egui::Stroke::new(1.0, palette.line))
                            .corner_radius(egui::CornerRadius::same(8))
                            .inner_margin(egui::Margin::symmetric(14, 10))
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                // Reserve the action button first, then let the
                                // name wrap in the remaining width. Both sides get
                                // an explicit height so the row cannot stretch to
                                // fill the panel.
                                let action_width = if active { 150.0_f32 } else { 104.0_f32 };
                                let row_height = 44.0_f32;
                                ui.horizontal(|ui| {
                                    // Text takes the left side; the action button is
                                    // pushed to the right edge of the card.
                                    let full_width = ui.available_width();
                                    let text_width =
                                        (full_width - action_width - 12.0).max(120.0);
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(text_width, row_height),
                                        egui::Layout::top_down(egui::Align::Min),
                                        |ui| {
                                            ui.set_max_width(text_width);
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(&profile.name)
                                                        .strong()
                                                        .color(palette.ink),
                                                )
                                                .truncate(),
                                            );
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(t(
                                                        zh,
                                                        "Codex-Router 本地隔离",
                                                        "Codex-Router local isolation",
                                                    ))
                                                    .small()
                                                    .color(palette.muted),
                                                )
                                                .truncate(),
                                            );
                                        },
                                    );
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(
                                            (ui.available_width()).max(action_width),
                                            row_height,
                                        ),
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let response = ui.add_enabled_ui(
                                                !self.applying && !active,
                                                |ui| {
                                                    theme::secondary_button(
                                                        ui,
                                                        if active {
                                                            t(zh, "上次由本软件应用", "Last applied")
                                                        } else {
                                                            t(zh, "直接应用", "Apply")
                                                        },
                                                        palette,
                                                    )
                                                },
                                            );
                                            if response.inner.clicked() {
                                                apply_profile = Some(profile.clone());
                                            }
                                        },
                                    );
                                });
                            });
                        ui.add_space(6.0);
                    }
                });
                        },
                    );
                });
                ui.add_space(12.0);
            });

        if restore_original {
            self.restore_original_codex();
        } else if restore_previous {
            self.restore_previous_codex();
        } else if let Some((kind, name)) = create_request {
            self.create_isolation_profile(kind, name);
        } else if let Some(profile) = apply_profile {
            self.apply_isolation_profile(&profile);
        }
    }

    fn show_sub2api_intro(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let mut open = true;
        let mut close = false;
        let mut launch = false;
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(520.0, 340.0),
            egui::vec2(400.0, 280.0),
        );
        egui::Window::new(t(zh, "Sub2API 本地管理", "Sub2API local administration"))
            .id(egui::Id::new("sub2api-introduction"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size(dialog_size)
            .collapsible(false)
            .resizable(false)
            .vscroll(true)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .open(&mut open)
            .frame(
                egui::Frame::new()
                    .fill(palette.paper)
                    .stroke(egui::Stroke::new(1.0, palette.line))
                    .inner_margin(egui::Margin::same(22)),
            )
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(t(
                        zh,
                        "高级渠道管理与故障排查入口",
                        "Advanced channel administration and troubleshooting",
                    ))
                    .size(19.0)
                    .strong()
                    .color(palette.ink),
                );
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(t(
                        zh,
                        "管理员邮箱：admin@admin.com",
                        "Administrator email: admin@admin.com",
                    ))
                    .monospace()
                    .strong(),
                );
                ui.label(
                    egui::RichText::new(t(
                        zh,
                        "管理员密码：随机生成并保存在 Windows 凭据管理器",
                        "Administrator password: randomly generated and stored in Windows Credential Manager",
                    ))
                    .monospace()
                    .strong(),
                );
                ui.label(
                    egui::RichText::new(t(
                        zh,
                        "管理页仅监听 127.0.0.1。需要登录时，请使用下方按钮复制当前凭据。",
                        "The admin page only listens on 127.0.0.1. Use the button below to copy the current credentials.",
                    ))
                    .small()
                    .color(palette.muted),
                );
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(t(
                        zh,
                        "日常 OAuth 登录、账号状态和模型导入请优先使用 Codex-Router 的 OAuth 账号页；此入口用于查看 Sub2API 的完整账户、分组、用量和错误详情。",
                        "Use Codex-Router's OAuth page for normal sign-in, account status, and model import. This console exposes Sub2API's full account, group, usage, and error details.",
                    ))
                    .color(palette.muted),
                );
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if theme::secondary_button(
                        ui,
                        t(zh, "复制登录信息", "Copy login"),
                        palette,
                    )
                    .clicked()
                    {
                        self.run_script_hidden("Copy-Sub2ApiLogin.ps1");
                        self.status_text = t(
                            zh,
                            "Sub2API 登录信息已复制",
                            "Sub2API login copied",
                        )
                        .into();
                    }
                    if theme::primary_button(
                        ui,
                        egui::RichText::new(t(zh, "打开管理页", "Open admin console"))
                            .strong()
                            .color(egui::Color32::WHITE),
                        palette,
                    )
                    .clicked()
                    {
                        launch = true;
                    }
                    if theme::secondary_button(ui, t(zh, "取消", "Cancel"), palette).clicked() {
                        close = true;
                    }
                });
            });
        if launch {
            self.open_sub2api_accounts();
            self.sub2api_intro_open = false;
        } else if close || !open {
            self.sub2api_intro_open = false;
        }
    }

    fn show_channel_preset_dialog(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(820.0, 620.0),
            egui::vec2(520.0, 360.0),
        );
        let mut open = true;
        let mut close = false;
        let mut open_recommended = false;
        let mut selected_preset = None;
        egui::Window::new("")
            .id(egui::Id::new("channel-preset-dialog"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size(dialog_size)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .frame(
                egui::Frame::new()
                    .fill(palette.background_dark)
                    .stroke(egui::Stroke::new(1.0, palette.background_light))
                    .inner_margin(egui::Margin::ZERO)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 14],
                        blur: 38,
                        spread: 0,
                        color: egui::Color32::from_rgba_unmultiplied(18, 24, 28, 82),
                    }),
            )
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(palette.glass_dark)
                    .inner_margin(egui::Margin::symmetric(22, 13))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(t(
                                    zh,
                                    "常见 API 渠道",
                                    "Common API channels",
                                ))
                                .size(24.0)
                                .strong()
                                .color(palette.paper),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add_sized(
                                            [34.0, 30.0],
                                            egui::Button::new(
                                                egui::RichText::new("×")
                                                    .size(21.0)
                                                    .color(palette.paper),
                                            )
                                            .fill(egui::Color32::TRANSPARENT)
                                            .stroke(egui::Stroke::NONE),
                                        )
                                        .on_hover_text(t(zh, "关闭", "Close"))
                                        .clicked()
                                    {
                                        close = true;
                                    }
                                    if ui
                                        .add_sized(
                                            [170.0, 34.0],
                                            egui::Button::new(
                                                egui::RichText::new(t(
                                                    zh,
                                                    "API 推荐平台",
                                                    "Recommended platforms",
                                                ))
                                                .strong()
                                                .color(palette.paper),
                                            )
                                            .fill(palette.accent)
                                            .stroke(egui::Stroke::new(1.0, palette.background_light))
                                            .corner_radius(egui::CornerRadius::same(6)),
                                        )
                                        .clicked()
                                    {
                                        open_recommended = true;
                                    }
                                },
                            );
                        });
                    });

                egui::Frame::new()
                    .fill(palette.background)
                    .inner_margin(egui::Margin::same(22))
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .fill(palette.glass)
                            .stroke(egui::Stroke::new(1.0, palette.background_light))
                            .corner_radius(egui::CornerRadius::same(6))
                            .inner_margin(egui::Margin::same(16))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(t(
                                        zh,
                                        "选择常见渠道，再检查自动填写的模型配置",
                                        "Choose a common channel, then review the auto-filled model configuration",
                                    ))
                                    .size(20.0)
                                    .strong()
                                    .color(palette.ink),
                                );
                                ui.add_space(9.0);
                                ui.columns(2, |columns| {
                                    theme::stacked_field_label(
                                        &mut columns[0],
                                        "BASE URL",
                                        t(
                                            zh,
                                            "接口根地址，通常以 /v1 结尾；不要填写控制台网页地址",
                                            "API root, usually ending in /v1; do not enter a console webpage",
                                        ),
                                        palette,
                                    );
                                    theme::stacked_field_label(
                                        &mut columns[1],
                                        "API KEY",
                                        t(
                                            zh,
                                            "只填写渠道生成的密钥；将安全保存到 Windows 凭据管理器",
                                            "Use the secret issued by the provider; it is stored in Windows Credential Manager",
                                        ),
                                        palette,
                                    );
                                    theme::field_label(
                                        &mut columns[0],
                                        t(zh, "模型 ID", "MODEL ID"),
                                        t(
                                            zh,
                                            "必须使用渠道接口接受的准确 ID，实际请求会发送此值",
                                            "Use the exact ID accepted by the provider API; requests send this value",
                                        ),
                                        palette,
                                    );
                                    theme::field_label(
                                        &mut columns[1],
                                        t(zh, "模型名称", "MODEL NAME"),
                                        t(
                                            zh,
                                            "仅用于 Codex-Router 界面显示，可以使用更易读的名称",
                                            "Display-only name in Codex-Router; a readable name is fine",
                                        ),
                                        palette,
                                    );
                                });
                            });
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(t(
                                    zh,
                                    "一键选择渠道",
                                    "CHOOSE A PROVIDER",
                                ))
                                .strong()
                                .color(palette.paper),
                            );
                            ui.label(
                                egui::RichText::new(t(
                                    zh,
                                    "API Key 不会由预设填写；进入下一页后由你输入",
                                    "Presets never fill an API key; enter yours on the next page",
                                ))
                                .small()
                                .color(palette.paper_alt),
                            );
                        });
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical()
                            .id_salt("channel-preset-selection-scroll")
                            .max_height((dialog_size.y - 350.0).clamp(96.0, 220.0))
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let common_presets =
                                    super::logic::common_channel_presets().collect::<Vec<_>>();
                                for pair in common_presets.chunks(2) {
                                    ui.columns(2, |columns| {
                                        for (index, preset) in pair.iter().enumerate() {
                                            let label = if zh {
                                                preset.label_zh
                                            } else {
                                                preset.label_en
                                            };
                                            egui::Frame::new()
                                                .fill(palette.background_light)
                                                .stroke(egui::Stroke::new(
                                                    1.0,
                                                    palette.background_dark,
                                                ))
                                                .corner_radius(egui::CornerRadius::same(5))
                                                .inner_margin(egui::Margin::same(12))
                                                .show(&mut columns[index], |ui| {
                                                    ui.set_min_width(ui.available_width());
                                                    ui.set_min_height(116.0);
                                                    if ui
                                                        .add_sized(
                                                            [ui.available_width(), 38.0],
                                                            egui::Button::new(
                                                                egui::RichText::new(label)
                                                                    .strong()
                                                                    .color(palette.paper),
                                                            )
                                                            .fill(palette.background_dark)
                                                            .stroke(egui::Stroke::new(
                                                                1.0,
                                                                palette.paper_alt,
                                                            )),
                                                        )
                                                        .clicked()
                                                    {
                                                        selected_preset = Some(preset.id);
                                                    }
                                                    ui.label(
                                                        egui::RichText::new(preset.model)
                                                            .monospace()
                                                            .small()
                                                            .color(palette.action),
                                                    );
                                                    ui.label(
                                                        egui::RichText::new(preset.base_url)
                                                            .small()
                                                            .color(palette.ink_soft),
                                                    );
                                                });
                                        }
                                    });
                                    ui.add_space(8.0);
                                }
                            });
                        ui.add_space(10.0);
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(t(zh, "取消", "Cancel"))
                                        .color(palette.paper),
                                )
                                .fill(palette.background_dark)
                                .stroke(egui::Stroke::new(1.0, palette.background_light))
                                .corner_radius(egui::CornerRadius::same(6))
                                .min_size(egui::vec2(112.0, 42.0)),
                            )
                            .clicked()
                        {
                            close = true;
                        }
                    });
            });
        if open_recommended {
            self.channel_preset_dialog_open = false;
            self.recommended_platform_dialog_open = true;
        } else if let Some(preset_id) = selected_preset {
            self.temp_model = ModelConfig::default();
            self.temp_model.priority = super::logic::next_api_channel_priority(&self.config);
            super::logic::apply_channel_preset(&mut self.temp_model, preset_id);
            self.temp_model.priority = super::logic::next_api_channel_priority(&self.config);
            self.editing_model = None;
            self.model_from_wizard = false;
            self.advanced_json_open = false;
            self.channel_preset_dialog_open = false;
            self.page = Page::Model;
        } else if close || !open {
            self.channel_preset_dialog_open = false;
        }
    }

    fn show_recommended_platform_dialog(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let available = ctx.content_rect().size();
        let dialog_size = fit_dialog_size(
            available,
            egui::vec2(880.0, 650.0),
            egui::vec2(520.0, 360.0),
        );
        let mut open = true;
        let mut close = false;
        let mut back_to_common = false;
        let mut select_chiral = false;
        let chiral_preset = super::logic::recommended_channel_presets()
            .next()
            .expect("Chiral-API recommended preset must exist");

        egui::Window::new("")
            .id(egui::Id::new("recommended-platform-dialog"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size(dialog_size)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .frame(
                egui::Frame::new()
                    .fill(palette.background_dark)
                    .stroke(egui::Stroke::new(1.0, palette.background_light))
                    .inner_margin(egui::Margin::ZERO)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 14],
                        blur: 38,
                        spread: 0,
                        color: egui::Color32::from_rgba_unmultiplied(18, 24, 28, 82),
                    }),
            )
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(palette.glass_dark)
                    .inner_margin(egui::Margin::symmetric(22, 13))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new(t(
                                        zh,
                                        "API 推荐平台",
                                        "Recommended API platforms",
                                    ))
                                    .size(24.0)
                                    .strong()
                                    .color(palette.paper),
                                );
                                ui.label(
                                    egui::RichText::new(t(
                                        zh,
                                        "经过适配的合作平台与渠道",
                                        "Integrated partner platforms and channels",
                                    ))
                                    .small()
                                    .color(palette.paper_alt),
                                );
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add_sized(
                                            [34.0, 30.0],
                                            egui::Button::new(
                                                egui::RichText::new("×")
                                                    .size(21.0)
                                                    .color(palette.paper),
                                            )
                                            .fill(egui::Color32::TRANSPARENT)
                                            .stroke(egui::Stroke::NONE),
                                        )
                                        .on_hover_text(t(zh, "关闭", "Close"))
                                        .clicked()
                                    {
                                        close = true;
                                    }
                                },
                            );
                        });
                    });

                egui::Frame::new()
                    .fill(palette.background)
                    .inner_margin(egui::Margin::same(20))
                    .show(ui, |ui| {
                        let scroll_height = (dialog_size.y - 150.0).clamp(120.0, 500.0);
                        egui::ScrollArea::vertical()
                            .id_salt("recommended-platform-scroll")
                            .max_height(scroll_height)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(t(
                                        zh,
                                        "选择平台后会自动填写推荐的 Base URL、模型 ID 与显示名称，API Key 仍由你自己输入。",
                                        "Choosing a platform fills the recommended Base URL, model ID, and display name. You still enter your own API key.",
                                    ))
                                    .size(13.5)
                                    .color(palette.paper),
                                );
                                ui.add_space(12.0);

                                egui::Frame::new()
                                    .fill(palette.paper)
                                    .stroke(egui::Stroke::new(2.0, palette.accent))
                                    .corner_radius(egui::CornerRadius::same(7))
                                    .inner_margin(egui::Margin::same(18))
                                    .shadow(theme::soft_card_shadow())
                                    .show(ui, |ui| {
                                        ui.set_min_width(ui.available_width());
                                        ui.set_min_height(164.0);
                                        let compact_card = ui.available_width() < 620.0;
                                        if compact_card {
                                            ui.vertical(|ui| {
                                                ui.horizontal(|ui| {
                                                    paint_chiral_mark(ui, 56.0, palette);
                                                    ui.vertical(|ui| {
                                                        ui.label(
                                                            egui::RichText::new("Chiral-API")
                                                                .size(22.0)
                                                                .strong()
                                                                .color(palette.ink),
                                                        );
                                                        ui.add(
                                                            egui::Label::new(
                                                                egui::RichText::new(t(
                                                                    zh,
                                                                    "科研与开发工作流的大模型 API 枢纽",
                                                                    "LLM APIs for research and development",
                                                                ))
                                                                .size(13.0)
                                                                .strong()
                                                                .color(palette.accent),
                                                            )
                                                            .wrap(),
                                                        );
                                                    });
                                                });
                                                ui.add_space(5.0);
                                                ui.add(
                                                    egui::Label::new(
                                                        egui::RichText::new(t(
                                                            zh,
                                                            "提供加密转接、分布式智能路由与稳定汇聚，可接入终端、IDE 和自动化 Agent。",
                                                            "Encrypted relay, distributed routing, stable aggregation, and support for terminals, IDEs, and automated agents.",
                                                        ))
                                                        .size(13.0)
                                                        .color(palette.ink_soft),
                                                    )
                                                    .wrap(),
                                                );
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "{}  ·  {}",
                                                        chiral_preset.model, chiral_preset.base_url
                                                    ))
                                                    .monospace()
                                                    .small()
                                                    .color(palette.muted),
                                                );
                                                ui.add_space(5.0);
                                                ui.horizontal_wrapped(|ui| {
                                                    if ui
                                                        .add_sized(
                                                            [150.0, 40.0],
                                                            egui::Button::new(
                                                                egui::RichText::new(t(
                                                                    zh,
                                                                    "使用此平台",
                                                                    "Use Chiral-API",
                                                                ))
                                                                .strong()
                                                                .color(egui::Color32::WHITE),
                                                            )
                                                            .fill(palette.action)
                                                            .stroke(egui::Stroke::NONE)
                                                            .corner_radius(
                                                                egui::CornerRadius::same(6),
                                                            ),
                                                        )
                                                        .clicked()
                                                    {
                                                        select_chiral = true;
                                                    }
                                                    ui.hyperlink_to(
                                                        t(
                                                            zh,
                                                            "访问平台网站 ↗",
                                                            "Visit platform ↗",
                                                        ),
                                                        chiral_preset.website_url,
                                                    );
                                                });
                                            });
                                        } else {
                                            ui.horizontal(|ui| {
                                            paint_chiral_mark(ui, 72.0, palette);
                                            ui.add_space(4.0);
                                            let action_width = if zh { 136.0 } else { 164.0 };
                                            let text_width =
                                                (ui.available_width() - action_width - 100.0)
                                                    .max(160.0);
                                            ui.allocate_ui_with_layout(
                                                egui::vec2(text_width, 150.0),
                                                egui::Layout::top_down(egui::Align::Min),
                                                |ui| {
                                                    ui.label(
                                                        egui::RichText::new("Chiral-API")
                                                            .size(23.0)
                                                            .strong()
                                                            .color(palette.ink),
                                                    );
                                                    ui.label(
                                                        egui::RichText::new(t(
                                                            zh,
                                                            "科研与开发工作流的大模型 API 枢纽",
                                                            "An LLM API hub for research and development workflows",
                                                        ))
                                                        .size(14.0)
                                                        .strong()
                                                        .color(palette.accent),
                                                    );
                                                    ui.add_space(5.0);
                                                    ui.add(
                                                        egui::Label::new(
                                                            egui::RichText::new(t(
                                                                zh,
                                                                "提供加密转接、分布式智能路由与稳定汇聚，可接入终端、IDE 和自动化 Agent。",
                                                                "Encrypted relay, distributed intelligent routing, stable aggregation, and support for terminals, IDEs, and automated agents.",
                                                            ))
                                                            .size(13.0)
                                                            .color(palette.ink_soft),
                                                        )
                                                        .wrap(),
                                                    );
                                                    ui.add_space(6.0);
                                                    ui.label(
                                                        egui::RichText::new(
                                                            format!(
                                                                "{}  ·  {}",
                                                                chiral_preset.model,
                                                                chiral_preset.base_url
                                                            ),
                                                        )
                                                        .monospace()
                                                        .small()
                                                        .color(palette.muted),
                                                    );
                                                },
                                            );
                                            ui.vertical(|ui| {
                                                ui.set_width(action_width);
                                                ui.add_space(20.0);
                                                if ui
                                                    .add_sized(
                                                        [action_width, 42.0],
                                                        egui::Button::new(
                                                            egui::RichText::new(t(
                                                                zh,
                                                                "使用此平台",
                                                                "Use Chiral-API",
                                                            ))
                                                            .strong()
                                                            .color(egui::Color32::WHITE),
                                                        )
                                                        .fill(palette.action)
                                                        .stroke(egui::Stroke::NONE)
                                                        .corner_radius(egui::CornerRadius::same(6)),
                                                    )
                                                    .clicked()
                                                {
                                                    select_chiral = true;
                                                }
                                                ui.add_space(7.0);
                                                ui.vertical_centered(|ui| {
                                                    ui.hyperlink_to(
                                                        t(
                                                            zh,
                                                            "访问平台网站 ↗",
                                                            "Visit platform ↗",
                                                        ),
                                                        chiral_preset.website_url,
                                                    );
                                                });
                                            });
                                            });
                                        }
                                    });

                                ui.add_space(15.0);
                                theme::eyebrow(
                                    ui,
                                    t(zh, "更多平台", "MORE PLATFORMS"),
                                    palette.paper_alt,
                                );
                                ui.add_space(7.0);
                                for _ in 0..2 {
                                    ui.columns(2, |columns| {
                                        for column in columns {
                                            egui::Frame::new()
                                                .fill(palette.glass)
                                                .stroke(egui::Stroke::new(1.0, palette.background_light))
                                                .corner_radius(egui::CornerRadius::same(6))
                                                .inner_margin(egui::Margin::same(14))
                                                .show(column, |ui| {
                                                    ui.set_min_width(ui.available_width());
                                                    ui.set_min_height(66.0);
                                                    ui.with_layout(
                                                        egui::Layout::centered_and_justified(
                                                            egui::Direction::TopDown,
                                                        ),
                                                        |ui| {
                                                            ui.add(
                                                                egui::Hyperlink::from_label_and_url(
                                                                    egui::RichText::new(t(
                                                                        zh,
                                                                        "寻求合作请在 GitHub 联系作者",
                                                                        "For partnerships, contact the author on GitHub",
                                                                    ))
                                                                    .size(12.5)
                                                                    .color(palette.ink_soft),
                                                                    OFFICIAL_GITHUB_URL,
                                                                ),
                                                            );
                                                        },
                                                    );
                                                });
                                        }
                                    });
                                    ui.add_space(8.0);
                                }
                            });

                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if theme::secondary_button(
                                ui,
                                t(zh, "返回常见 API", "Back to common APIs"),
                                palette,
                            )
                            .clicked()
                            {
                                back_to_common = true;
                            }
                            if theme::secondary_button(ui, t(zh, "关闭", "Close"), palette)
                                .clicked()
                            {
                                close = true;
                            }
                        });
                    });
            });

        if select_chiral {
            self.temp_model = ModelConfig::default();
            self.temp_model.priority = super::logic::next_api_channel_priority(&self.config);
            super::logic::apply_channel_preset(&mut self.temp_model, chiral_preset.id);
            self.editing_model = None;
            self.model_from_wizard = false;
            self.advanced_json_open = false;
            self.recommended_platform_dialog_open = false;
            self.channel_preset_dialog_open = false;
            self.status_text = t(
                zh,
                "已应用 Chiral-API 推荐配置；请填写 API Key 后保存",
                "Chiral-API preset applied. Enter the API key, then save.",
            )
            .into();
            self.page = Page::Model;
        } else if back_to_common {
            self.recommended_platform_dialog_open = false;
            self.channel_preset_dialog_open = true;
        } else if close || !open {
            self.recommended_platform_dialog_open = false;
        }
    }

    fn show_oauth_revoke_dialog(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let Some(account) = self.oauth_revoke_target.clone() else {
            return;
        };
        let mut open = true;
        let mut confirm = false;
        let mut cancel = false;
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(560.0, 360.0),
            egui::vec2(420.0, 280.0),
        );
        egui::Window::new("")
            .id(egui::Id::new("oauth-revoke-confirmation"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size(dialog_size)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .vscroll(true)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .open(&mut open)
            .frame(
                egui::Frame::new()
                    .fill(palette.background_dark)
                    .stroke(egui::Stroke::new(1.0, palette.background_light))
                    .inner_margin(egui::Margin::ZERO)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 14],
                        blur: 38,
                        spread: 0,
                        color: egui::Color32::from_rgba_unmultiplied(18, 24, 28, 82),
                    }),
            )
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(palette.glass_dark)
                    .inner_margin(egui::Margin::symmetric(22, 13))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(t(zh, "撤销 OAuth", "Revoke OAuth"))
                                    .size(24.0)
                                    .strong()
                                    .color(palette.paper),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add_sized(
                                            [34.0, 30.0],
                                            egui::Button::new(
                                                egui::RichText::new("×")
                                                    .size(21.0)
                                                    .color(palette.paper),
                                            )
                                            .fill(egui::Color32::TRANSPARENT)
                                            .stroke(egui::Stroke::NONE),
                                        )
                                        .on_hover_text(t(zh, "关闭", "Close"))
                                        .clicked()
                                    {
                                        cancel = true;
                                    }
                                },
                            );
                        });
                    });
                egui::Frame::new()
                    .fill(palette.background)
                    .inner_margin(egui::Margin::same(22))
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .fill(palette.glass)
                            .stroke(egui::Stroke::new(1.0, palette.background_light))
                            .corner_radius(egui::CornerRadius::same(6))
                            .inner_margin(egui::Margin::same(16))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(if account.email.is_empty() {
                                        account.name.clone()
                                    } else {
                                        format!("{} · {}", account.name, account.email)
                                    })
                                    .size(20.0)
                                    .strong()
                                    .color(palette.ink),
                                );
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} · ID {}",
                                        account.platform, account.id
                                    ))
                                    .small()
                                    .color(palette.muted),
                                );
                                ui.add_space(12.0);
                                ui.label(
                                    egui::RichText::new(t(
                                        zh,
                                        "确认后将永久删除本机 Sub2API 中保存的 OAuth 令牌和账号，并从所有路由配置中移除该账号导入的模型。",
                                        "This permanently deletes the OAuth tokens and account stored in local Sub2API, and removes models imported from it from every route profile.",
                                    ))
                                    .color(palette.danger),
                                );
                                ui.add_space(8.0);
                                ui.label(
                                    egui::RichText::new(t(
                                        zh,
                                        "此操作不会自动撤回远端平台中的第三方应用授权。如需彻底撤回，请同时前往对应平台的账号安全设置。",
                                        "This does not automatically revoke third-party app access at the provider. To revoke it completely, also use the provider's account security settings.",
                                    ))
                                    .small()
                                    .color(palette.ink_soft),
                                );
                            });
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    !self.oauth_revoking,
                                    egui::Button::new(
                                        egui::RichText::new(t(
                                            zh,
                                            "确认撤销并删除",
                                            "Revoke and delete",
                                        ))
                                        .strong()
                                        .color(egui::Color32::WHITE),
                                    )
                                    .fill(palette.danger)
                                    .corner_radius(egui::CornerRadius::same(7)),
                                )
                                .clicked()
                            {
                                confirm = true;
                            }
                            if theme::secondary_button(ui, t(zh, "取消", "Cancel"), palette)
                                .clicked()
                            {
                                cancel = true;
                            }
                        });
                    });
            });
        if confirm {
            self.oauth_revoke_target = None;
            self.revoke_oauth_account(account);
        } else if cancel || !open {
            self.oauth_revoke_target = None;
        }
    }

    fn show_oauth_priority_dialog(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let Some(account) = self.oauth_priority_target.clone() else {
            return;
        };
        let peers = self
            .oauth_accounts
            .iter()
            .filter(|peer| peer.platform.eq_ignore_ascii_case(&account.platform))
            .cloned()
            .collect::<Vec<_>>();
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(560.0, 420.0),
            egui::vec2(420.0, 320.0),
        );
        egui::Window::new(t(
            zh,
            "同平台 OAuth 调度优先级",
            "Same-platform OAuth priority",
        ))
        .id(egui::Id::new("oauth-priority-dialog"))
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .default_size(dialog_size)
        .min_size(dialog_size)
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.set_min_width(dialog_size.x - 24.0);
            ui.label(
                egui::RichText::new(if zh {
                    format!(
                        "账号：{}（{}）\n数值越小，同平台调度时越优先。修改后立即写入 Sub2API，无需重新部署。",
                        account.name, account.platform
                    )
                } else {
                    format!(
                        "Account: {} ({})\nLower numbers are preferred when the same platform has multiple accounts. Changes are written to Sub2API immediately.",
                        account.name, account.platform
                    )
                })
                .color(palette.ink_soft),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(t(zh, "优先级", "Priority"));
                ui.add(
                    egui::DragValue::new(&mut self.oauth_priority_draft)
                        .range(1..=999)
                        .speed(1.0)
                        .prefix("P"),
                );
            });
            ui.add_space(8.0);
            if peers.len() > 1 {
                ui.label(
                    egui::RichText::new(t(
                        zh,
                        "同平台账号一览",
                        "Accounts on this platform",
                    ))
                    .strong()
                    .color(palette.ink),
                );
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .max_height(160.0)
                    .show(ui, |ui| {
                        for peer in &peers {
                            let marker = if peer.id == account.id { "→ " } else { "  " };
                            let label = if peer.email.is_empty() {
                                format!(
                                    "{marker}P{}  {}{}",
                                    peer.priority.max(1),
                                    peer.name,
                                    if peer.id == account.id {
                                        if zh {
                                            "（当前）"
                                        } else {
                                            " (current)"
                                        }
                                    } else {
                                        ""
                                    }
                                )
                            } else {
                                format!(
                                    "{marker}P{}  {} · {}{}",
                                    peer.priority.max(1),
                                    peer.name,
                                    peer.email,
                                    if peer.id == account.id {
                                        if zh {
                                            "（当前）"
                                        } else {
                                            " (current)"
                                        }
                                    } else {
                                        ""
                                    }
                                )
                            };
                            ui.label(
                                egui::RichText::new(label)
                                    .small()
                                    .color(if peer.id == account.id {
                                        palette.action
                                    } else {
                                        palette.muted
                                    }),
                            );
                        }
                    });
            }
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                let save_response = ui.add_enabled_ui(!self.oauth_priority_saving, |ui| {
                    theme::primary_button(
                        ui,
                        egui::RichText::new(t(zh, "保存优先级", "Save priority"))
                            .strong()
                            .color(egui::Color32::WHITE),
                        palette,
                    )
                });
                if save_response.inner.clicked() {
                    save = true;
                }
                if theme::secondary_button(ui, t(zh, "取消", "Cancel"), palette).clicked() {
                    cancel = true;
                }
                if self.oauth_priority_saving {
                    ui.spinner();
                }
            });
        });
        if save {
            self.save_oauth_account_priority();
        } else if (cancel || !open) && !self.oauth_priority_saving {
            self.oauth_priority_target = None;
        }
    }

    fn show_grok_sso_dialog(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let mut open = true;
        let mut cancel = false;
        let mut import = false;
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(620.0, 390.0),
            egui::vec2(440.0, 300.0),
        );
        egui::Window::new("")
            .id(egui::Id::new("grok-sso-import-dialog"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size(dialog_size)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .vscroll(true)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .open(&mut open)
            .frame(
                egui::Frame::new()
                    .fill(palette.paper)
                    .stroke(egui::Stroke::new(1.0, palette.line))
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::same(24))
                    .shadow(theme::soft_card_shadow()),
            )
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(t(
                        zh,
                        "导入 Grok 授权码 / SSO Token",
                        "Import Grok authorization / SSO token",
                    ))
                    .font(egui::FontId::new(25.0, theme::display_family()))
                    .color(palette.ink),
                );
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(t(
                        zh,
                        "适用于没有账号密码、由 xAI 或账号提供方直接发放授权码的登录方式。每行可填写一个授权码。",
                        "For accounts without password login where xAI or the account provider supplies an authorization code. Enter one token per line.",
                    ))
                    .color(palette.ink_soft),
                );
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(t(zh, "授权码 / SSO TOKEN", "AUTHORIZATION / SSO TOKEN"))
                        .small()
                        .strong()
                        .color(palette.muted),
                );
                ui.add_sized(
                    [ui.available_width(), 105.0],
                    egui::TextEdit::multiline(&mut self.grok_sso_draft)
                        .password(true)
                        .hint_text(t(
                            zh,
                            "粘贴授权码；多个账号时每行一个",
                            "Paste authorization; one token per line",
                        )),
                );
                ui.label(
                    egui::RichText::new(t(
                        zh,
                        "授权码只通过标准输入交给本机 Sub2API，不写入配置、日志或命令行。",
                        "Tokens are sent to local Sub2API over standard input and are never written to config, logs, or command-line arguments.",
                    ))
                    .small()
                    .color(palette.muted),
                );
                if !self.grok_sso_error.is_empty() {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(&self.grok_sso_error)
                            .small()
                            .color(palette.danger),
                    );
                }
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let submit = ui.add_enabled_ui(
                        !self.grok_sso_importing && !self.grok_sso_draft.trim().is_empty(),
                        |ui| {
                            theme::primary_button(
                                ui,
                                egui::RichText::new(t(zh, "验证并导入", "Validate & import"))
                                    .strong()
                                    .color(egui::Color32::WHITE),
                                palette,
                            )
                        },
                    );
                    if submit.inner.clicked() {
                        import = true;
                    }
                    if theme::secondary_button(ui, t(zh, "取消", "Cancel"), palette).clicked() {
                        cancel = true;
                    }
                    if self.grok_sso_importing {
                        ui.spinner();
                    }
                });
            });
        if import {
            self.import_grok_sso();
        } else if cancel || !open {
            self.grok_sso_dialog_open = false;
            self.grok_sso_draft.clear();
            self.grok_sso_error.clear();
        }
    }

    fn oauth_fallback_base_urls(&self, account_id: i64) -> Vec<String> {
        let oauth_model_ids = self
            .config
            .models
            .iter()
            .filter(|model| model.source == "oauth" && model.oauth_account_id == account_id)
            .map(|model| model.model.clone())
            .collect::<Vec<_>>();
        let mut base_urls = Vec::new();
        for model in self
            .config
            .models
            .iter()
            .filter(|model| model.source != "oauth")
        {
            if !oauth_model_ids
                .iter()
                .any(|oauth_id| super::logic::same_model_identity(oauth_id, &model.model))
            {
                continue;
            }
            if !super::logic::is_fallback_channel_selected(&self.config, model) {
                continue;
            }
            let base_url = model.base_url.trim().trim_end_matches('/');
            if !base_url.is_empty() && !base_urls.iter().any(|item| item == base_url) {
                base_urls.push(base_url.to_owned());
            }
        }
        base_urls
    }

    fn show_oauth_routing_status(
        &self,
        ui: &mut egui::Ui,
        account: &OAuthAccountSummary,
        usage: Option<&UsageAccount>,
        backup_urls: &[String],
        palette: &theme::Palette,
        zh: bool,
    ) -> bool {
        let most_used_window = usage.and_then(|usage| {
            usage
                .windows
                .iter()
                .filter(|window| window.used_percent.is_some())
                .max_by(|left, right| {
                    left.used_percent
                        .unwrap_or_default()
                        .total_cmp(&right.used_percent.unwrap_or_default())
                })
        });
        let used_percent = most_used_window.and_then(|window| window.used_percent);
        let quota_exhausted = usage.is_some_and(|usage| usage.health == "quotaExhausted")
            || used_percent.is_some_and(|value| value >= 99.95);
        let unavailable = usage.is_some_and(|usage| {
            !usage.health.is_empty() && usage.health != "healthy" && !quota_exhausted
        });
        let recovery = if quota_exhausted {
            most_used_window.map(|window| Self::usage_reset_label(window, zh))
        } else {
            None
        };
        let urls = backup_urls.join(if zh { "、" } else { ", " });
        let has_backup = !backup_urls.is_empty();
        let prefer_oauth = self.config.oauth_fallback.prefer_oauth;

        let (title, mut details, color) = if !account.bound_to_router {
            (
                t(
                    zh,
                    "等待保存并应用：本账号尚未接入当前路由",
                    "Pending save: this account is not yet attached to the active route",
                )
                .to_owned(),
                Vec::new(),
                palette.muted,
            )
        } else if self.usage_loading && usage.is_none() {
            (
                t(
                    zh,
                    "正在读取账号额度与路由状态",
                    "Loading quota and routing state",
                )
                .to_owned(),
                Vec::new(),
                palette.muted,
            )
        } else if prefer_oauth && usage.is_none() {
            (
                t(
                    zh,
                    "暂时无法读取账号额度；仍按已配置优先级路由",
                    "Account quota is temporarily unavailable; configured priorities remain active",
                )
                .to_owned(),
                if has_backup {
                    vec![if zh {
                        format!("备用 Base URL：{urls}")
                    } else {
                        format!("Fallback Base URL: {urls}")
                    }]
                } else {
                    Vec::new()
                },
                palette.muted,
            )
        } else if !prefer_oauth {
            if has_backup {
                let mut detail = vec![if zh {
                    format!("第一优先级 Base URL：{urls}；OAuth 作为备用渠道")
                } else {
                    format!("First-priority Base URL: {urls}; OAuth is the backup channel")
                }];
                if quota_exhausted {
                    detail.push(t(
                        zh,
                        "本 OAuth 账号额度已用尽；恢复后仍按当前设置作为备用",
                        "This OAuth quota is exhausted; after recovery it remains the backup by preference",
                    ).to_owned());
                }
                (
                    t(
                        zh,
                        "同名模型正在优先使用其他 API / Base URL 额度",
                        "Matching models currently prefer other API / Base URL quota",
                    )
                    .to_owned(),
                    detail,
                    if quota_exhausted {
                        palette.danger
                    } else {
                        palette.success
                    },
                )
            } else {
                (
                    t(
                        zh,
                        "已选择 API / Base URL 优先，但没有同名备用渠道",
                        "API / Base URL priority is selected, but no matching channel exists",
                    )
                    .to_owned(),
                    Vec::new(),
                    palette.danger,
                )
            }
        } else if quota_exhausted {
            if has_backup {
                (
                    t(
                        zh,
                        "本账号额度已用尽，已路由至其他 Base URL",
                        "This account quota is exhausted; routing has moved to other Base URLs",
                    )
                    .to_owned(),
                    vec![if zh {
                        format!("当前备用 Base URL：{urls}")
                    } else {
                        format!("Current fallback Base URL: {urls}")
                    }],
                    palette.danger,
                )
            } else {
                (
                    t(
                        zh,
                        "本账号额度已用尽，且没有同名 Base URL 可接续",
                        "This account quota is exhausted and no matching Base URL can take over",
                    )
                    .to_owned(),
                    Vec::new(),
                    palette.danger,
                )
            }
        } else if unavailable {
            if has_backup {
                (
                    t(
                        zh,
                        "本 OAuth 账号暂不可调度，已使用同名备用渠道",
                        "This OAuth account is temporarily unavailable; a matching fallback is active",
                    )
                    .to_owned(),
                    vec![if zh {
                        format!("当前备用 Base URL：{urls}")
                    } else {
                        format!("Current fallback Base URL: {urls}")
                    }],
                    palette.danger,
                )
            } else {
                (
                    t(
                        zh,
                        "本 OAuth 账号暂不可调度，且没有同名备用渠道",
                        "This OAuth account is temporarily unavailable with no matching fallback",
                    )
                    .to_owned(),
                    Vec::new(),
                    palette.danger,
                )
            }
        } else {
            let detail = if has_backup {
                vec![if zh {
                    format!("备用 Base URL：{urls}")
                } else {
                    format!("Fallback Base URL: {urls}")
                }]
            } else {
                Vec::new()
            };
            (
                t(
                    zh,
                    "本账号额度可用，正在以第一优先级使用这些模型",
                    "This account has quota and its models are using first priority",
                )
                .to_owned(),
                detail,
                palette.success,
            )
        };

        if let Some(percent) = used_percent {
            details.push(if zh {
                format!("账号额度已用 {percent:.0}%")
            } else {
                format!("Account quota used: {percent:.0}%")
            });
        }
        if let Some(recovery) = recovery {
            details.push(if prefer_oauth {
                if zh {
                    format!("{recovery}，届时自动恢复第一优先级")
                } else {
                    format!("{recovery}; OAuth automatically returns to first priority")
                }
            } else if zh {
                format!("{recovery}，届时 OAuth 可再次作为备用")
            } else {
                format!("{recovery}; OAuth then becomes available as backup again")
            });
        }
        if unavailable {
            let usage = usage.expect("unavailable state requires usage data");
            if !usage.status_detail.trim().is_empty() {
                details.push(usage.status_detail.clone());
            }
        }

        ui.add_space(8.0);
        let mut open_picker = false;
        egui::Frame::new()
            .fill(palette.paper_alt)
            .stroke(egui::Stroke::new(1.0, color))
            .corner_radius(egui::CornerRadius::same(6))
            .inner_margin(egui::Margin::symmetric(12, 9))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(egui::RichText::new(title).strong().color(color));
                for detail in details {
                    ui.label(egui::RichText::new(detail).small().color(palette.ink_soft));
                }
                ui.add_space(5.0);
                if ui
                    .button(t(
                        zh,
                        "手动选择备用条目",
                        "Choose fallback entries",
                    ))
                    .on_hover_text(t(
                        zh,
                        "按已激活的 OAuth 模型选择同名 API 条目；默认按名称自动匹配",
                        "Choose same-name API entries for each active OAuth model; automatic name matching is the default",
                    ))
                    .clicked()
                {
                    open_picker = true;
                }
            });
        open_picker
    }

    fn show_oauth_accounts(&mut self, ui: &mut egui::Ui, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let active_config_name = self.active_route_config_name(zh);
        let oauth_count = self.config.oauth_account_ids.as_ref().map_or(0, Vec::len);
        let compact_header = ui.available_width() < 1000.0;
        let applying = self.applying;
        let mut back_to_console = false;
        let mut apply_current_profile = false;
        let mut refresh_accounts = false;
        let show_title = |ui: &mut egui::Ui| {
            ui.vertical(|ui| {
                theme::eyebrow(ui, "ROUTE PROFILE / OAUTH", palette.paper);
                ui.label(
                    egui::RichText::new(t(zh, "当前配置的 OAuth 授权", "PROFILE OAUTH ACCESS"))
                        .font(egui::FontId::new(34.0, theme::display_family()))
                        .color(palette.paper),
                );
                ui.label(
                    egui::RichText::new(if zh {
                        format!("当前配置：{active_config_name} · 已启用 {oauth_count} 个账号")
                    } else {
                        format!(
                            "Current config: {active_config_name} · {oauth_count} account(s) enabled"
                        )
                    })
                    .small()
                    .strong()
                    .color(palette.paper),
                );
            });
        };
        let mut show_actions = |ui: &mut egui::Ui| {
            back_to_console =
                theme::secondary_button(ui, t(zh, "返回控制台", "Back to console"), palette)
                    .clicked();
            let response = ui.add_enabled_ui(!applying, |ui| {
                theme::primary_button(
                    ui,
                    egui::RichText::new(t(zh, "保存并应用当前配置", "Save & apply profile"))
                        .strong()
                        .color(egui::Color32::WHITE),
                    palette,
                )
            });
            apply_current_profile = response.inner.clicked();
            refresh_accounts =
                theme::secondary_button(ui, t(zh, "刷新", "Refresh"), palette).clicked();
        };
        if compact_header {
            ui.vertical(|ui| {
                show_title(ui);
                ui.add_space(7.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        show_actions(ui);
                    });
                });
            });
        } else {
            ui.horizontal(|ui| {
                show_title(ui);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    show_actions(ui);
                });
            });
        }
        if back_to_console {
            self.page = self.oauth_return_page;
        }
        if apply_current_profile {
            self.apply_all();
        }
        if refresh_accounts {
            self.refresh_oauth_accounts();
        }
        ui.add_space(14.0);
        let accounts = self.oauth_accounts.clone();
        let connected_count = accounts.len();
        let mut selected_ids = self.config.oauth_account_ids.clone().unwrap_or_default();
        let mut selection_changed = false;
        let mut import_model = None;
        let mut revoke_account = None;
        let mut priority_account = None;
        let mut manual_model_account = None;
        let mut fallback_picker_account = None;
        let content_height = ui.available_height().max(320.0);
        ui.horizontal_top(|ui| {
            let total_width = ui.available_width();
            let gap = 14.0;
            let left_width = (total_width * 0.29).clamp(248.0, 340.0);
            ui.allocate_ui_with_layout(
                egui::vec2(left_width, content_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(left_width);
                    theme::glass_frame(palette).show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        egui::ScrollArea::vertical()
                            .id_salt("oauth-provider-panel")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
            theme::stacked_field_label(
                ui,
                t(zh, "添加登录平台", "ADD A LOGIN PROVIDER"),
                t(
                    zh,
                    "登录账号由 Sub2API 安全保管；本页的启用账号、模型和回退策略只属于当前配置",
                    "Sub2API securely stores sign-ins; enabled accounts, models, and fallback policy on this page belong only to the current profile",
                ),
                palette,
            );
            ui.vertical(|ui| {
                for (platform, label) in [
                    ("openai", "OpenAI / ChatGPT"),
                    ("anthropic", "Anthropic / Claude"),
                    ("gemini", "Google / Gemini"),
                    ("antigravity", "Google / Antigravity"),
                    ("grok", "xAI / Grok"),
                ] {
                    let button_width = ui.available_width();
                    let response = ui
                        .add_enabled_ui(!self.provider_oauth_running, |ui| {
                            ui.add_sized(
                                [button_width, 42.0],
                                egui::Button::new(
                                    egui::RichText::new(label).strong().color(palette.ink),
                                )
                                .fill(palette.paper)
                                .stroke(egui::Stroke::new(1.0, palette.line))
                                .corner_radius(egui::CornerRadius::same(7)),
                            )
                        })
                        .inner;
                    if response
                        .on_hover_text(format!(
                            "{}: {platform}",
                            t(
                                zh,
                                "直接打开此平台的官方登录授权页",
                                "Open this provider's official authorization page"
                            )
                        ))
                        .clicked()
                    {
                        self.start_provider_oauth(platform);
                    }
                }
                if self.provider_oauth_running {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(t(
                            zh,
                            "正在等待当前 OAuth 完成",
                            "Waiting for the current OAuth login",
                        ));
                    });
                    if ui
                        .add_sized(
                            [ui.available_width(), 34.0],
                            egui::Button::new(
                                egui::RichText::new(t(zh, "取消 OAuth", "Cancel OAuth"))
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(palette.danger)
                            .corner_radius(egui::CornerRadius::same(6)),
                        )
                        .on_hover_text(t(
                            zh,
                            "关闭浏览器标签后点此取消，避免一直卡在等待状态",
                            "Cancel after closing the browser tab so the UI does not stay stuck",
                        ))
                        .clicked()
                    {
                        self.cancel_provider_oauth();
                    }
                }
                if ui
                    .add_sized(
                        [ui.available_width(), 34.0],
                        egui::Button::new(
                            egui::RichText::new(t(
                                zh,
                                "Grok Web SSO（可选）",
                                "Grok Web SSO (optional)",
                            ))
                            .color(palette.ink_soft),
                        )
                        .fill(palette.paper_alt)
                        .stroke(egui::Stroke::new(1.0, palette.line))
                        .corner_radius(egui::CornerRadius::same(6)),
                    )
                    .on_hover_text(t(
                        zh,
                        "没有账号密码时，使用 xAI 提供的授权码或 SSO Token 导入",
                        "Import an xAI authorization code or SSO token when no password login is available",
                    ))
                    .clicked()
                {
                    self.grok_sso_dialog_open = true;
                    self.grok_sso_error.clear();
                }
                if ui
                    .add_sized(
                        [ui.available_width(), 34.0],
                        egui::Button::new(t(zh, "Sub2API 高级管理", "Sub2API admin"))
                            .fill(palette.paper_alt)
                            .stroke(egui::Stroke::new(1.0, palette.line))
                            .corner_radius(egui::CornerRadius::same(6)),
                    )
                    .clicked()
                {
                    self.sub2api_intro_open = true;
                }
            });
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(6.0);
            ui.horizontal_top(|ui| {
                let fallback_response = ui.checkbox(&mut self.config.oauth_fallback.enabled, "");
                ui.add(
                    egui::Label::new(t(
                        zh,
                        "启用 OAuth 与同名 API / Base URL 之间的自动接续",
                        "Enable automatic continuity between OAuth and matching API / Base URL channels",
                    ))
                    .wrap(),
                );
                if fallback_response.changed() {
                    self.status_text = t(
                        zh,
                        "当前配置的 OAuth 兜底策略已修改；保存并应用后生效",
                        "This profile's OAuth fallback policy changed. Save & apply to activate it.",
                    )
                    .into();
                }
            });
            if self.config.oauth_fallback.enabled {
                ui.add_space(6.0);
                egui::Frame::new()
                    .fill(palette.paper_alt)
                    .stroke(egui::Stroke::new(1.0, palette.line))
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        let prefer_oauth = self.config.oauth_fallback.prefer_oauth;
                        let mut toggle = false;
                        ui.vertical(|ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(if prefer_oauth {
                                        t(zh, "同名模型首选：OAuth 账号额度", "Matching model priority: OAuth quota")
                                    } else {
                                        t(zh, "同名模型首选：其他 API / Base URL", "Matching model priority: other API / Base URL")
                                    })
                                    .strong()
                                    .color(palette.ink),
                                )
                                .wrap(),
                            );
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(t(
                                        zh,
                                        "开启为 OAuth 优先；关闭为其他额度优先",
                                        "On prefers OAuth; off prefers other API quota",
                                    ))
                                    .small()
                                    .color(palette.muted),
                                )
                                .wrap(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    toggle = Self::dashboard_route_switch(
                                        ui,
                                        prefer_oauth,
                                        false,
                                        palette,
                                    )
                                    .clicked();
                                },
                            );
                        });
                        if toggle {
                            self.config.oauth_fallback.prefer_oauth = !prefer_oauth;
                            self.status_text = t(
                                zh,
                                "同名模型的首选额度已修改；保存并应用后生效",
                                "The preferred quota for matching models changed. Save & apply to activate it.",
                            )
                            .to_owned();
                        }
                    });
                let (oauth_priority, api_priority) = if self.config.oauth_fallback.prefer_oauth {
                    (
                        self.config.oauth_fallback.official_priority,
                        self.config.oauth_fallback.fallback_priority,
                    )
                } else {
                    (
                        self.config.oauth_fallback.fallback_priority,
                        self.config.oauth_fallback.official_priority,
                    )
                };
                ui.label(
                    egui::RichText::new(format!(
                        "{}: OAuth P{} → API Key P{}",
                        t(zh, "路由优先级", "Routing priority"),
                        oauth_priority,
                        api_priority
                    ))
                    .small()
                    .color(palette.ink_soft),
                );
            }
            ui.add_space(6.0);
            let router_base_url = format!(
                "{}/v1",
                self.local_sub2api_base_url()
            );
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(t(
                        zh,
                        "Codex 接口 / Router Base URL",
                        "Codex endpoint / Router Base URL",
                    ))
                        .small()
                        .color(palette.ink_soft),
                );
                ui.label(
                    egui::RichText::new(router_base_url)
                        .monospace()
                        .small()
                        .color(palette.ink),
                );
                ui.label(
                    egui::RichText::new(t(
                        zh,
                        "API Key  本机 Router Key（已脱敏）",
                        "API key  local Router key (masked)",
                    ))
                    .small()
                    .color(palette.ink_soft),
                );
                ui.horizontal(|ui| {
                    if ui.small_button(t(zh, "复制 Key", "Copy key")).clicked() {
                        self.run_script_hidden("Copy-LocalApiKey.ps1");
                        self.status_text =
                            t(zh, "本机 Router Key 已复制", "Local Router key copied").into();
                    }
                });
            });
                            });
                    });
                },
            );
            ui.add_space(gap);
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), content_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    theme::glass_frame(palette).show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new(t(
                                    zh,
                                    "已添加的 OAuth 账号",
                                    "CONNECTED OAUTH ACCOUNTS",
                                ))
                                .size(18.0)
                                .strong()
                                .color(palette.ink),
                            );
                            theme::pill(
                                ui,
                                &format!("{connected_count}"),
                                palette.paper_alt,
                                palette.ink_soft,
                            );
                        });
                        ui.add_space(8.0);
                        if self.oauth_loading {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(t(
                                    zh,
                                    "正在读取登录账号…",
                                    "Loading signed-in accounts…",
                                ));
                            });
                        }
                        if !self.oauth_error.is_empty() {
                            egui::Frame::new()
                                .fill(palette.paper)
                                .stroke(egui::Stroke::new(1.0, palette.danger))
                                .corner_radius(egui::CornerRadius::same(6))
                                .inner_margin(egui::Margin::same(12))
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(friendly_error(&self.oauth_error, zh))
                                            .color(palette.danger),
                                    );
                                });
                        }

        let account_height = (content_height - 82.0).max(220.0);
        egui::ScrollArea::vertical()
            .id_salt("oauth-account-list")
            .max_height(account_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for account in &accounts {
                    let usage_account = self
                        .usage_snapshot
                        .as_ref()
                        .and_then(|snapshot| {
                            snapshot
                                .subscriptions
                                .iter()
                                .find(|usage| usage.id == account.id)
                        })
                        .cloned();
                    let backup_urls = self.oauth_fallback_base_urls(account.id);
                    egui::Frame::new()
                        .fill(palette.paper)
                        .stroke(egui::Stroke::new(1.0, palette.line))
                        .inner_margin(egui::Margin::symmetric(18, 14))
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(
                                        egui::RichText::new(&account.name)
                                            .size(18.0)
                                            .strong()
                                            .color(palette.ink),
                                    );
                                    let identity = if account.email.is_empty() {
                                        account.platform.clone()
                                    } else if account.plan.is_empty() {
                                        format!("{} · {}", account.platform, account.email)
                                    } else {
                                        format!(
                                            "{} · {} · {}",
                                            account.platform, account.email, account.plan
                                        )
                                    };
                                    ui.label(
                                        egui::RichText::new(identity).small().color(palette.muted),
                                    );
                                });
                                theme::pill(
                                    ui,
                                    &account.status,
                                    palette.paper_alt,
                                    if account.status == "active" {
                                        palette.success
                                    } else {
                                        palette.danger
                                    },
                                );
                                let peer_count = accounts
                                    .iter()
                                    .filter(|peer| {
                                        peer.platform.eq_ignore_ascii_case(&account.platform)
                                    })
                                    .count();
                                let priority_label = format!("P{}", account.priority.max(1));
                                let priority_response = ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new(&priority_label)
                                                .small()
                                                .strong()
                                                .color(palette.ink),
                                        )
                                        .fill(palette.paper_alt)
                                        .stroke(egui::Stroke::new(1.0, palette.line))
                                        .corner_radius(egui::CornerRadius::same(99)),
                                    )
                                    .on_hover_text(if zh {
                                        if peer_count > 1 {
                                            format!(
                                                "点击调整同平台（{}）多账号调度优先级。数值越小越优先，当前同平台 {} 个账号。",
                                                account.platform, peer_count
                                            )
                                        } else {
                                            "点击设置此 OAuth 账号的调度优先级（数值越小越优先）"
                                                .to_owned()
                                        }
                                    } else if peer_count > 1 {
                                        format!(
                                            "Click to set scheduling priority among {} {} accounts. Lower numbers are preferred.",
                                            peer_count, account.platform
                                        )
                                    } else {
                                        "Click to set this OAuth account's scheduling priority (lower numbers are preferred)"
                                            .to_owned()
                                    });
                                if priority_response.clicked() {
                                    priority_account = Some(account.clone());
                                }
                            });
                            ui.add_space(6.0);
                            let mut selected = selected_ids.contains(&account.id);
                            ui.horizontal_wrapped(|ui| {
                                if ui
                                    .checkbox(
                                        &mut selected,
                                        t(zh, "本配置启用", "Enabled in this profile"),
                                    )
                                    .changed()
                                {
                                    selection_changed = true;
                                    if selected {
                                        selected_ids.push(account.id);
                                    } else {
                                        selected_ids.retain(|id| *id != account.id);
                                    }
                                }
                                if ui
                                    .add_enabled(
                                        !self.oauth_revoking,
                                        egui::Button::new(t(
                                            zh,
                                            "撤销 OAuth",
                                            "Revoke OAuth",
                                        ))
                                        .fill(palette.paper_alt)
                                        .stroke(egui::Stroke::new(1.0, palette.danger))
                                        .corner_radius(egui::CornerRadius::same(6)),
                                    )
                                    .on_hover_text(t(
                                        zh,
                                        "删除本机保存的 OAuth 令牌、账号和所有配置引用",
                                        "Delete locally stored OAuth tokens, account, and all profile references",
                                    ))
                                    .clicked()
                                {
                                    revoke_account = Some(account.clone());
                                }
                            });
                            if !account.error.is_empty() {
                                ui.label(
                                    egui::RichText::new(&account.error)
                                        .small()
                                        .color(palette.danger),
                                );
                            }
                            if !account.expires_at.is_empty() {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{}: {}",
                                        t(zh, "凭据到期", "Credential expiry"),
                                        account.expires_at
                                    ))
                                    .small()
                                    .color(palette.muted),
                                );
                            }
                            if self.config.oauth_fallback.enabled
                                && selected
                                && self.show_oauth_routing_status(
                                    ui,
                                    account,
                                    usage_account.as_ref(),
                                    &backup_urls,
                                    palette,
                                    zh,
                                )
                            {
                                    fallback_picker_account = Some(account.clone());
                            }
                            ui.add_space(8.0);
                            ui.horizontal_wrapped(|ui| {
                                for model in &account.models {
                                    let already_added = self
                                        .config
                                        .models
                                        .iter()
                                        .any(|item| {
                                            item.source == "oauth"
                                                && item.oauth_account_id == account.id
                                                && item.model == model.id
                                        });
                                    let unsupported =
                                        model.id.to_ascii_lowercase().contains("image");
                                    let label = if already_added {
                                        format!(
                                            "{} · {}",
                                            model.display_name,
                                            t(zh, "已加入", "Added")
                                        )
                                    } else if model.suggested {
                                        format!(
                                            "＋ {} · {}",
                                            model.display_name,
                                            t(zh, "文档新增", "Docs update")
                                        )
                                    } else {
                                        format!("＋ {}", model.display_name)
                                    };
                                    let mut response = ui.add_enabled(
                                        !already_added && !unsupported,
                                        egui::Button::new(label)
                                            .fill(palette.paper_alt)
                                            .corner_radius(egui::CornerRadius::same(6)),
                                    );
                                    if model.suggested {
                                        response = response.on_hover_text(if account.platform == "openai" {
                                            t(
                                                zh,
                                                "Codex 已知模型目录包含此模型，但当前 OpenAI 账号尚未通过接口发现；能否使用取决于账号权限，失败时会按配置走备用路由。",
                                                "The known Codex catalog includes this model, but the OpenAI account has not advertised it through discovery. Access depends on the account and failures use the configured fallback route.",
                                            )
                                        } else {
                                            t(
                                                zh,
                                                "平台已知模型目录包含此模型，但当前账号尚未通过接口发现；能否使用取决于账号权限，失败时会按配置走备用路由。",
                                                "The known provider catalog includes this model, but the account has not advertised it through discovery. Access depends on the account and failures use the configured fallback route.",
                                            )
                                        });
                                    }
                                    if response.clicked() {
                                        import_model = Some((account.clone(), model.clone()));
                                    }
                                }
                                if ui
                                    .button(t(zh, "手动补填模型…", "Add model manually…"))
                                    .on_hover_text(t(
                                        zh,
                                        "接口列表缺少新模型时，按平台官方文档填写准确模型 ID",
                                        "Enter the exact model ID from the provider documentation when discovery is incomplete",
                                    ))
                                    .clicked()
                                {
                                    manual_model_account = Some(account.clone());
                                }
                            });
                        });
                    ui.add_space(8.0);
                }
            });
                    });
                },
            );
        });
        if let Some(account) = revoke_account {
            self.oauth_revoke_target = Some(account);
        }
        if let Some(account) = priority_account {
            self.open_oauth_priority_editor(account);
        }
        if let Some(account) = manual_model_account {
            self.oauth_manual_model_target = Some(account);
            self.oauth_manual_model_id_draft.clear();
            self.oauth_manual_model_alias_draft.clear();
        }
        if let Some(account) = fallback_picker_account {
            self.open_oauth_fallback_picker(account);
        }
        if selection_changed {
            selected_ids.sort_unstable();
            selected_ids.dedup();
            self.config.oauth_account_ids = Some(selected_ids);
            self.schedule_usage_refresh();
            self.status_text = t(
                zh,
                "OAuth 账号选择已保存到当前配置；点击“保存并应用”后生效",
                "OAuth account selection is stored in this profile. Save & apply to activate it.",
            )
            .into();
        }
        if let Some((account, model)) = import_model {
            let imported_model_id =
                if account.platform == "openai" && model.id.eq_ignore_ascii_case("gpt-5.6") {
                    "gpt-5.6-sol".to_owned()
                } else {
                    model.id.clone()
                };
            let mut item = ModelConfig {
                model: imported_model_id,
                alias: model.display_name,
                alias_customized: Some(false),
                base_url: format!("Sub2API OAuth / {}", account.platform),
                priority: self.config.oauth_fallback.official_priority,
                source: "oauth".into(),
                oauth_account_id: account.id,
                oauth_platform: account.platform,
                ..Default::default()
            };
            item.multimodal = "auto".into();
            self.config.models.push(item);
            let ids = self.config.oauth_account_ids.get_or_insert_with(Vec::new);
            if !ids.contains(&account.id) {
                ids.push(account.id);
            }
            super::logic::normalize_default_model(&mut self.config);
            self.schedule_usage_refresh();
            self.status_text = t(
                zh,
                "OAuth 模型已加入当前配置；点击“保存并应用”后可在 Codex 中使用",
                "The OAuth model was added to this profile. Save & apply to use it in Codex.",
            )
            .into();
        }
    }

    fn open_oauth_fallback_picker(&mut self, account: OAuthAccountSummary) {
        self.oauth_fallback_picker_draft.clear();
        for model in self
            .config
            .models
            .iter()
            .filter(|model| model.source == "oauth" && model.oauth_account_id == account.id)
        {
            let canonical = super::logic::canonical_route_model_id(&model.model);
            if self.oauth_fallback_picker_draft.contains_key(&canonical) {
                continue;
            }
            let selection = self
                .config
                .fallback_channel_selections
                .get(&canonical)
                .cloned()
                .map(Some)
                .unwrap_or(None);
            self.oauth_fallback_picker_draft
                .insert(canonical, selection);
        }
        self.oauth_fallback_picker_target = Some(account);
    }

    fn show_oauth_fallback_picker_dialog(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let Some(account) = self.oauth_fallback_picker_target.clone() else {
            return;
        };
        let mut active_models = Vec::<(String, String, String)>::new();
        for model in self
            .config
            .models
            .iter()
            .filter(|model| model.source == "oauth" && model.oauth_account_id == account.id)
        {
            let canonical = super::logic::canonical_route_model_id(&model.model);
            if active_models.iter().any(|(id, _, _)| id == &canonical) {
                continue;
            }
            let display_name = if model.alias.trim().is_empty() {
                model.model.clone()
            } else {
                model.alias.clone()
            };
            active_models.push((canonical, model.model.clone(), display_name));
        }

        let mut candidates = std::collections::BTreeMap::new();
        for (canonical, oauth_model_id, _) in &active_models {
            let matching = self
                .config
                .models
                .iter()
                .enumerate()
                .filter(|(_, model)| {
                    model.source != "oauth"
                        && super::logic::same_model_identity(&model.model, oauth_model_id)
                })
                .map(|(index, model)| {
                    (
                        index,
                        model.clone(),
                        super::logic::fallback_channel_key(&model.model, &model.base_url),
                    )
                })
                .collect::<Vec<_>>();
            candidates.insert(canonical.clone(), matching);
        }

        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(760.0, 610.0),
            egui::vec2(520.0, 360.0),
        );
        egui::Window::new("")
            .id(egui::Id::new("oauth-fallback-picker-dialog"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size(dialog_size)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .frame(
                egui::Frame::new()
                    .fill(palette.background_dark)
                    .stroke(egui::Stroke::new(1.0, palette.background_light))
                    .inner_margin(egui::Margin::ZERO)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 14],
                        blur: 38,
                        spread: 0,
                        color: egui::Color32::from_rgba_unmultiplied(18, 24, 28, 82),
                    }),
            )
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(palette.glass_dark)
                    .inner_margin(egui::Margin::symmetric(22, 13))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(t(
                                    zh,
                                    "手动选择备用条目",
                                    "Choose fallback entries",
                                ))
                                .size(24.0)
                                .strong()
                                .color(palette.paper),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add_sized(
                                            [34.0, 30.0],
                                            egui::Button::new(
                                                egui::RichText::new("×")
                                                    .size(21.0)
                                                    .color(palette.paper),
                                            )
                                            .fill(egui::Color32::TRANSPARENT)
                                            .stroke(egui::Stroke::NONE),
                                        )
                                        .on_hover_text(t(zh, "关闭", "Close"))
                                        .clicked()
                                    {
                                        cancel = true;
                                    }
                                },
                            );
                        });
                    });
                egui::Frame::new()
                    .fill(palette.background)
                    .inner_margin(egui::Margin::same(20))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(format!("{} · {}", account.name, account.platform))
                                .strong()
                                .color(palette.ink),
                        );
                        ui.label(
                            egui::RichText::new(t(
                                zh,
                                "每个已激活 OAuth 模型只列出模型 ID 相同的 API 条目。自动模式使用全部同名条目，并按条目优先级路由。",
                                "Each active OAuth model lists only API entries with the same model ID. Automatic mode uses every match and routes by entry priority.",
                            ))
                            .small()
                            .color(palette.ink_soft),
                        );
                        ui.add_space(10.0);
                        egui::ScrollArea::vertical()
                            .id_salt("oauth-fallback-picker-scroll")
                            .max_height((dialog_size.y - 180.0).clamp(120.0, 430.0))
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if active_models.is_empty() {
                                    ui.label(
                                        egui::RichText::new(t(
                                            zh,
                                            "当前账号还没有加入本配置的 OAuth 模型。请先在账号卡片中加入模型。",
                                            "This account has no OAuth model added to the profile yet. Add a model from the account card first.",
                                        ))
                                        .color(palette.muted),
                                    );
                                }
                                for (canonical, _, display_name) in &active_models {
                                    let matching = candidates
                                        .get(canonical)
                                        .map(Vec::as_slice)
                                        .unwrap_or_default();
                                    egui::Frame::new()
                                        .fill(palette.paper)
                                        .stroke(egui::Stroke::new(1.0, palette.line))
                                        .corner_radius(egui::CornerRadius::same(6))
                                        .inner_margin(egui::Margin::same(14))
                                        .show(ui, |ui| {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "{} · {}",
                                                    display_name, canonical
                                                ))
                                                .strong()
                                                .color(palette.ink),
                                            );
                                            let state = self
                                                .oauth_fallback_picker_draft
                                                .entry(canonical.clone())
                                                .or_insert(None);
                                            let mut automatic = state.is_none();
                                            if ui
                                                .checkbox(
                                                    &mut automatic,
                                                    t(
                                                        zh,
                                                        "自动匹配展示候选与真实 ID 均一致的条目",
                                                        "Automatically match entries whose display candidate and real ID both agree",
                                                    ),
                                                )
                                                .changed()
                                            {
                                                *state = if automatic {
                                                    None
                                                } else {
                                                    Some(
                                                        matching
                                                            .iter()
                                                            .map(|(_, _, key)| key.clone())
                                                            .collect(),
                                                    )
                                                };
                                            }
                                            ui.add_space(5.0);
                                            if matching.is_empty() {
                                                ui.label(
                                                    egui::RichText::new(t(
                                                        zh,
                                                        "没有找到展示候选与真实 ID 均一致的 API 条目",
                                                        "No API entry matched both the display candidate and real ID",
                                                    ))
                                                    .small()
                                                    .color(palette.muted),
                                                );
                                            }
                                            for (index, model, key) in matching {
                                                let selected = state
                                                    .as_ref()
                                                    .is_none_or(|items| {
                                                        items.iter().any(|item| {
                                                            item.eq_ignore_ascii_case(key)
                                                        })
                                                    });
                                                let mut checked = selected;
                                                let alias = if model.alias.trim().is_empty() {
                                                    model.model.as_str()
                                                } else {
                                                    model.alias.as_str()
                                                };
                                                let label = format!(
                                                    "{:02}  {} · {} · P{}",
                                                    index + 1,
                                                    alias,
                                                    model.model,
                                                    model.priority
                                                );
                                                let response = ui.add_enabled(
                                                    state.is_some(),
                                                    egui::Checkbox::new(&mut checked, label),
                                                );
                                                if response.changed() {
                                                    if let Some(items) = state.as_mut() {
                                                        if checked {
                                                            if !items.iter().any(|item| {
                                                                item.eq_ignore_ascii_case(key)
                                                            }) {
                                                                items.push(key.clone());
                                                            }
                                                        } else {
                                                            items.retain(|item| {
                                                                !item.eq_ignore_ascii_case(key)
                                                            });
                                                        }
                                                    }
                                                }
                                                ui.label(
                                                    egui::RichText::new(
                                                        model.base_url.trim().trim_end_matches('/'),
                                                    )
                                                    .monospace()
                                                    .small()
                                                    .color(palette.muted),
                                                );
                                            }
                                        });
                                    ui.add_space(8.0);
                                }
                            });
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            let response = ui.add_enabled_ui(!active_models.is_empty(), |ui| {
                                theme::primary_button(
                                    ui,
                                    egui::RichText::new(t(
                                        zh,
                                        "保存备用选择",
                                        "Save fallback selection",
                                    ))
                                    .strong()
                                    .color(egui::Color32::WHITE),
                                    palette,
                                )
                            });
                            if response.inner.clicked() {
                                save = true;
                            }
                            if theme::secondary_button(ui, t(zh, "取消", "Cancel"), palette)
                                .clicked()
                            {
                                cancel = true;
                            }
                        });
                    });
            });

        if save {
            for (canonical, _, _) in &active_models {
                match self
                    .oauth_fallback_picker_draft
                    .get(canonical)
                    .cloned()
                    .flatten()
                {
                    Some(mut selected) => {
                        selected.sort();
                        selected.dedup();
                        self.config
                            .fallback_channel_selections
                            .insert(canonical.clone(), selected);
                    }
                    None => {
                        self.config.fallback_channel_selections.remove(canonical);
                    }
                }
            }
            self.status_text = t(
                zh,
                "备用条目选择已保存到当前配置；点击“保存并应用”后写入实际路由",
                "Fallback entry selection is stored in this profile. Save & apply to update the actual route.",
            )
            .into();
            self.oauth_fallback_picker_target = None;
            self.oauth_fallback_picker_draft.clear();
        } else if cancel || !open {
            self.oauth_fallback_picker_target = None;
            self.oauth_fallback_picker_draft.clear();
        }
    }

    fn show_oauth_manual_model_dialog(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let Some(account) = self.oauth_manual_model_target.clone() else {
            return;
        };
        let mut open = true;
        let mut add = false;
        let mut cancel = false;
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(600.0, 430.0),
            egui::vec2(440.0, 300.0),
        );
        egui::Window::new("")
            .id(egui::Id::new("oauth-manual-model-dialog"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size(dialog_size)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .vscroll(true)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .open(&mut open)
            .frame(
                egui::Frame::new()
                    .fill(palette.background_dark)
                    .stroke(egui::Stroke::new(1.0, palette.background_light))
                    .inner_margin(egui::Margin::ZERO)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 14],
                        blur: 38,
                        spread: 0,
                        color: egui::Color32::from_rgba_unmultiplied(18, 24, 28, 82),
                    }),
            )
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(palette.glass_dark)
                    .inner_margin(egui::Margin::symmetric(22, 13))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(t(
                                    zh,
                                    "手动补填 OAuth 模型",
                                    "Add OAuth model manually",
                                ))
                                .size(24.0)
                                .strong()
                                .color(palette.paper),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add_sized(
                                            [34.0, 30.0],
                                            egui::Button::new(
                                                egui::RichText::new("×")
                                                    .size(21.0)
                                                    .color(palette.paper),
                                            )
                                            .fill(egui::Color32::TRANSPARENT)
                                            .stroke(egui::Stroke::NONE),
                                        )
                                        .on_hover_text(t(zh, "关闭", "Close"))
                                        .clicked()
                                    {
                                        cancel = true;
                                    }
                                },
                            );
                        });
                    });

                egui::Frame::new()
                    .fill(palette.background)
                    .inner_margin(egui::Margin::same(22))
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .fill(palette.glass)
                            .stroke(egui::Stroke::new(1.0, palette.background_light))
                            .corner_radius(egui::CornerRadius::same(6))
                            .inner_margin(egui::Margin::same(16))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} · {}",
                                        account.name, account.platform
                                    ))
                                    .size(18.0)
                                    .strong()
                                    .color(palette.ink),
                                );
                                ui.add_space(6.0);
                                ui.label(
                                    egui::RichText::new(t(
                                        zh,
                                        "模型发现列表可能滞后。请从平台官方文档复制准确的接口模型 ID；手动加入不代表当前账号套餐一定拥有该模型权限。",
                                        "Model discovery can lag behind. Copy the exact API model ID from the provider documentation; adding it does not guarantee that the account plan has access.",
                                    ))
                                    .small()
                                    .color(palette.ink_soft),
                                );
                                ui.add(
                                    egui::Hyperlink::from_label_and_url(
                                        egui::RichText::new(t(
                                            zh,
                                            "查看该平台模型文档 ↗",
                                            "Open provider model documentation ↗",
                                        ))
                                        .strong()
                                        .color(palette.action),
                                        oauth_model_docs_url(&account.platform),
                                    ),
                                );
                                ui.add_space(14.0);
                                theme::field_label(
                                    ui,
                                    t(zh, "模型 ID", "MODEL ID"),
                                    t(
                                        zh,
                                        "必填，例如 gemini-3.6-flash；请求会原样发送此值",
                                        "Required, for example gemini-3.6-flash; requests use this exact value",
                                    ),
                                    palette,
                                );
                                ui.add(
                                    egui::TextEdit::singleline(
                                        &mut self.oauth_manual_model_id_draft,
                                    )
                                    .hint_text("gemini-3.6-flash")
                                    .desired_width(f32::INFINITY),
                                );
                                ui.add_space(10.0);
                                theme::field_label(
                                    ui,
                                    t(zh, "模型名称（可选）", "DISPLAY NAME (OPTIONAL)"),
                                    t(
                                        zh,
                                        "仅用于 Codex-Router 中显示；留空则使用模型 ID",
                                        "Only shown in Codex-Router; leave blank to use the model ID",
                                    ),
                                    palette,
                                );
                                ui.add(
                                    egui::TextEdit::singleline(
                                        &mut self.oauth_manual_model_alias_draft,
                                    )
                                    .hint_text(t(
                                        zh,
                                        "例如 Gemini 3.6 Flash",
                                        "e.g. Gemini 3.6 Flash",
                                    ))
                                    .desired_width(f32::INFINITY),
                                );
                            });
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            let response = ui.add_enabled_ui(
                                !self.oauth_manual_model_id_draft.trim().is_empty(),
                                |ui| {
                                    theme::primary_button(
                                        ui,
                                        egui::RichText::new(t(
                                            zh,
                                            "加入当前配置",
                                            "Add to profile",
                                        ))
                                        .strong()
                                        .color(egui::Color32::WHITE),
                                        palette,
                                    )
                                },
                            );
                            if response.inner.clicked() {
                                add = true;
                            }
                            if theme::secondary_button(
                                ui,
                                t(zh, "取消", "Cancel"),
                                palette,
                            )
                            .clicked()
                            {
                                cancel = true;
                            }
                        });
                    });
            });

        if add {
            let mut model_id = self.oauth_manual_model_id_draft.trim().to_owned();
            if account.platform == "openai" && model_id.eq_ignore_ascii_case("gpt-5.6") {
                model_id = "gpt-5.6-sol".to_owned();
            }
            if self.config.models.iter().any(|item| {
                item.source == "oauth"
                    && item.oauth_account_id == account.id
                    && item.model == model_id
            }) {
                self.status_text = t(
                    zh,
                    "这个模型 ID 已在当前配置中，无需重复加入",
                    "That model ID is already in the current profile",
                )
                .into();
            } else {
                let alias = self.oauth_manual_model_alias_draft.trim();
                self.config.models.push(ModelConfig {
                    model: model_id.clone(),
                    alias: if alias.is_empty() {
                        super::logic::recommended_model_display_name(&model_id)
                    } else {
                        alias.to_owned()
                    },
                    alias_customized: Some(!alias.is_empty()),
                    base_url: format!("Sub2API OAuth / {}", account.platform),
                    priority: self.config.oauth_fallback.official_priority,
                    source: "oauth".into(),
                    oauth_account_id: account.id,
                    oauth_platform: account.platform,
                    multimodal: "auto".into(),
                    ..Default::default()
                });
                let ids = self.config.oauth_account_ids.get_or_insert_with(Vec::new);
                if !ids.contains(&account.id) {
                    ids.push(account.id);
                }
                super::logic::normalize_default_model(&mut self.config);
                self.schedule_usage_refresh();
                self.status_text = t(
                    zh,
                    "手动模型已加入当前配置；点击“保存并应用”后可在 Codex 中使用",
                    "The manual model was added. Save & apply to use it in Codex.",
                )
                .into();
            }
            self.oauth_manual_model_target = None;
        } else if cancel || !open {
            self.oauth_manual_model_target = None;
        }
    }

    fn compact_number(value: i64) -> String {
        let absolute = value.unsigned_abs() as f64;
        if absolute >= 1_000_000_000.0 {
            format!("{:.2}B", value as f64 / 1_000_000_000.0)
        } else if absolute >= 1_000_000.0 {
            format!("{:.2}M", value as f64 / 1_000_000.0)
        } else if absolute >= 1_000.0 {
            format!("{:.1}K", value as f64 / 1_000.0)
        } else {
            value.to_string()
        }
    }

    fn usage_window_label(window: &UsageWindow, zh: bool) -> String {
        if !window.display_name.trim().is_empty() {
            return window.display_name.clone();
        }
        match window.kind.as_str() {
            "fiveHour" => t(zh, "5 小时额度", "5-hour limit").to_owned(),
            "weekly" => t(zh, "周额度", "Weekly limit").to_owned(),
            "monthly" => t(zh, "月额度", "Monthly limit").to_owned(),
            "model" => t(zh, "模型额度", "Model limit").to_owned(),
            _ => t(zh, "其他额度窗口", "Other quota window").to_owned(),
        }
    }

    fn usage_timestamp(value: &str) -> String {
        chrono::DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|timestamp| {
                timestamp
                    .with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_else(|| value.to_owned())
    }

    fn usage_reset_label(window: &UsageWindow, zh: bool) -> String {
        let remaining = chrono::DateTime::parse_from_rfc3339(&window.reset_at)
            .ok()
            .map(|reset| {
                reset
                    .signed_duration_since(chrono::Utc::now())
                    .num_seconds()
            })
            .unwrap_or(window.remaining_seconds);
        if window.reset_at.is_empty() && remaining <= 0 {
            return t(zh, "平台未提供重置时间", "Reset time not provided").to_owned();
        }
        if remaining < 0 {
            return if window.reset_at.is_empty() {
                t(zh, "平台未提供重置时间", "Reset time not provided").to_owned()
            } else {
                t(zh, "等待平台刷新", "Awaiting provider refresh").to_owned()
            };
        }
        if remaining == 0 {
            return t(zh, "正在重置", "Resetting now").to_owned();
        }
        let days = remaining / 86_400;
        let hours = (remaining % 86_400) / 3_600;
        let minutes = (remaining % 3_600) / 60;
        if days > 0 {
            if zh {
                format!("{days} 天 {hours} 小时后重置")
            } else {
                format!("Resets in {days}d {hours}h")
            }
        } else if hours > 0 {
            if zh {
                format!("{hours} 小时 {minutes} 分后重置")
            } else {
                format!("Resets in {hours}h {minutes}m")
            }
        } else if zh {
            format!("{minutes} 分钟后重置")
        } else {
            format!("Resets in {minutes}m")
        }
    }

    fn usage_health(
        account: &UsageAccount,
        palette: &theme::Palette,
        zh: bool,
    ) -> (&'static str, egui::Color32) {
        match account.health.as_str() {
            "healthy" => (t(zh, "正常", "HEALTHY"), palette.success),
            "quotaExhausted" => (t(zh, "额度已用尽", "QUOTA EXHAUSTED"), palette.danger),
            "cooldown" => (t(zh, "限流冷却中", "COOLING DOWN"), palette.muted),
            _ => (t(zh, "上游异常", "UPSTREAM ERROR"), palette.danger),
        }
    }

    fn show_usage_window(
        ui: &mut egui::Ui,
        window: &UsageWindow,
        palette: &theme::Palette,
        zh: bool,
    ) {
        let label = Self::usage_window_label(window, zh);
        let reset = Self::usage_reset_label(window, zh);
        let remaining = remaining_quota_percent(window.used_percent);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(label).strong().color(palette.ink));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(match remaining {
                        Some(value) if zh => format!("剩余 {value:.0}%"),
                        Some(value) => format!("{value:.0}% remaining"),
                        None => t(zh, "额度不可用", "Quota unavailable").to_owned(),
                    })
                    .small()
                    .strong()
                    .color(palette.ink_soft),
                );
            });
        });
        let progress = remaining.unwrap_or(0.0) / 100.0;
        let (bar_rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 10.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(bar_rect, egui::CornerRadius::same(5), palette.paper_alt);
        if progress > 0.0 {
            let fill_rect = egui::Rect::from_min_size(
                bar_rect.min,
                egui::vec2(bar_rect.width() * progress, bar_rect.height()),
            );
            ui.painter().rect_filled(
                fill_rect,
                egui::CornerRadius::same(5),
                if remaining.unwrap_or(0.0) <= 5.0 {
                    palette.danger
                } else {
                    palette.action
                },
            );
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(reset).small().color(palette.muted));
            if window.tokens > 0 || window.requests > 0 {
                ui.label(
                    egui::RichText::new(format!(
                        "{} · {} {} · {} {}",
                        "•",
                        Self::compact_number(window.tokens),
                        t(zh, "tokens", "tokens"),
                        window.requests,
                        t(zh, "次请求", "requests")
                    ))
                    .small()
                    .color(palette.muted),
                );
            }
        });
    }

    fn show_usage_account(
        ui: &mut egui::Ui,
        account: &UsageAccount,
        palette: &theme::Palette,
        zh: bool,
        subscription: bool,
    ) -> (egui::Response, egui::Response) {
        let card = egui::Frame::new()
            .fill(palette.paper)
            .stroke(egui::Stroke::new(1.0, palette.line))
            .corner_radius(egui::CornerRadius::same(7))
            .inner_margin(egui::Margin::symmetric(20, 17))
            .shadow(theme::soft_card_shadow())
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.set_max_width(ui.available_width());
                let drag_handle = ui
                    .horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.set_max_width((ui.available_width() - 96.0).max(120.0));
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&account.name)
                                        .font(egui::FontId::new(17.0, theme::display_family()))
                                        .color(palette.ink),
                                )
                                .wrap(),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} · #{} · {} · {}",
                                    account.platform.to_uppercase(),
                                    account.id,
                                    account.kind.to_uppercase(),
                                    account.status.to_uppercase(),
                                ))
                                .small()
                                .color(palette.muted),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            let drag_handle = ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("≡").size(18.0).color(palette.ink_soft),
                                    )
                                    .fill(palette.paper_alt)
                                    .stroke(egui::Stroke::new(1.0, palette.line))
                                    .corner_radius(egui::CornerRadius::same(5))
                                    .sense(egui::Sense::drag()),
                                )
                                .on_hover_text(t(
                                    zh,
                                    "长按此手柄并拖到其他卡片上排序",
                                    "Hold this handle and drag onto another card to reorder",
                                ));
                            let (health, health_color) = Self::usage_health(account, palette, zh);
                            theme::pill(ui, health, palette.paper_alt, health_color);
                            drag_handle
                        })
                        .inner
                    })
                    .inner;
                ui.add_space(10.0);

                ui.columns(3, |columns| {
                    let values = [
                        (
                            t(zh, "31 天 TOKENS", "31-DAY TOKENS"),
                            Self::compact_number(account.totals.total_tokens),
                        ),
                        (
                            t(zh, "请求", "REQUESTS"),
                            account.totals.requests.to_string(),
                        ),
                        (
                            t(zh, "估算费用", "EST. COST"),
                            format!("${:.4}", account.totals.cost),
                        ),
                    ];
                    for (column, (label, value)) in columns.iter_mut().zip(values) {
                        column.label(egui::RichText::new(label).small().color(palette.muted));
                        column.label(egui::RichText::new(value).strong().color(palette.ink));
                    }
                });

                if subscription || !account.windows.is_empty() {
                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(8.0);
                    let readable_windows = account
                        .windows
                        .iter()
                        .filter(|window| remaining_quota_percent(window.used_percent).is_some())
                        .collect::<Vec<_>>();
                    if readable_windows.is_empty() {
                        ui.label(
                            egui::RichText::new(t(
                                zh,
                                "平台未提供可读取的 5 小时 / 周 / 月额度窗口",
                                "The provider did not expose readable 5-hour, weekly, or monthly quota windows",
                            ))
                            .small()
                            .color(palette.muted),
                        );
                    } else {
                        for (index, window) in readable_windows.into_iter().enumerate() {
                            if index > 0 {
                                ui.add_space(10.0);
                            }
                            Self::show_usage_window(ui, window, palette, zh);
                        }
                    }
                } else if !account.totals.models.is_empty() {
                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(8.0);
                    for model in &account.totals.models {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new(&model.name)
                                    .strong()
                                    .color(palette.ink_soft),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} total · {} in · {} out · {} cache · {} req · ${:.4}",
                                    Self::compact_number(model.total_tokens),
                                    Self::compact_number(model.input_tokens),
                                    Self::compact_number(model.output_tokens),
                                    Self::compact_number(
                                        model.cache_read_tokens + model.cache_creation_tokens
                                    ),
                                    model.requests,
                                    model.cost
                                ))
                                .small()
                                .color(palette.muted),
                            );
                        });
                    }
                }

                if !account.status_detail.trim().is_empty() {
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new(&account.status_detail)
                            .small()
                            .color(palette.danger),
                    );
                }
                if !account.query_note.trim().is_empty() {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(&account.query_note)
                            .small()
                            .color(palette.muted),
                    );
                }
                let source_time = if account.updated_at.is_empty() {
                    &account.last_used_at
                } else {
                    &account.updated_at
                };
                if !source_time.is_empty() {
                    ui.add_space(5.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{}: {}",
                            t(zh, "数据时间", "Data timestamp"),
                            Self::usage_timestamp(source_time)
                        ))
                        .small()
                        .color(palette.muted),
                    );
                }
                drag_handle
            });
        (card.response, card.inner)
    }

    /// Quota-window accounts (OAuth subscriptions and Coding Plan style API
    /// channels) share the 5-hour / weekly / monthly window cards. Plain
    /// pay-as-you-go API channels have no such windows and are listed separately.
    fn usage_account_is_quota_plan(account: &UsageAccount) -> bool {
        account.kind == "oauth"
            || account
                .windows
                .iter()
                .any(|window| matches!(window.kind.as_str(), "fiveHour" | "weekly" | "monthly"))
    }

    fn show_usage_account_grid(
        ui: &mut egui::Ui,
        accounts: &[&UsageAccount],
        palette: &theme::Palette,
        zh: bool,
        subscription: bool,
        two_columns: bool,
    ) -> Option<(UsageOrderSection, i64, i64)> {
        let mut usage_reorder = None;
        let gap = 10.0;
        // Edge gutter keeps the right column off the scrollbar; never force a
        // floor width that would overflow and wrap into a blank second row.
        let edge_gutter = 8.0;
        let usable_width = (ui.available_width() - edge_gutter).max(0.0);
        let columns = if two_columns && usable_width >= 560.0 {
            2usize
        } else {
            1usize
        };
        let card_width = if columns == 2 {
            ((usable_width - gap) / 2.0).max(0.0)
        } else {
            usable_width.max(0.0)
        };
        let mut index = 0usize;
        while index < accounts.len() {
            ui.horizontal_top(|ui| {
                for offset in 0..columns {
                    let Some(account) = accounts.get(index + offset) else {
                        break;
                    };
                    if offset > 0 {
                        ui.add_space(gap);
                    }
                    ui.allocate_ui_with_layout(
                        egui::vec2(card_width, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            let account_section = if account.kind == "oauth" {
                                UsageOrderSection::Subscription
                            } else {
                                UsageOrderSection::Api
                            };
                            let (card, handle) = Self::show_usage_account(
                                ui,
                                account,
                                palette,
                                zh,
                                subscription || account.kind == "oauth",
                            );
                            handle.dnd_set_drag_payload(UsageOrderDrag {
                                section: account_section,
                                account_id: account.id,
                            });
                            if let Some(payload) = card.dnd_hover_payload::<UsageOrderDrag>() {
                                if payload.section == account_section
                                    && payload.account_id != account.id
                                {
                                    ui.painter().rect_stroke(
                                        card.rect,
                                        egui::CornerRadius::same(7),
                                        egui::Stroke::new(2.0, palette.action),
                                        egui::StrokeKind::Outside,
                                    );
                                }
                            }
                            if let Some(payload) = card.dnd_release_payload::<UsageOrderDrag>() {
                                if payload.section == account_section {
                                    usage_reorder =
                                        Some((account_section, payload.account_id, account.id));
                                }
                            }
                        },
                    );
                }
            });
            index += columns;
            if index < accounts.len() {
                ui.add_space(gap);
            }
        }
        usage_reorder
    }

    fn show_usage_monitor(&mut self, ui: &mut egui::Ui, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                theme::eyebrow(
                    ui,
                    t(
                        zh,
                        "当前配置 / 用量与额度",
                        "ACTIVE PROFILE / USAGE & QUOTAS",
                    ),
                    palette.paper,
                );
                ui.label(
                    egui::RichText::new(t(zh, "实时用量统计", "LIVE USAGE"))
                        .font(egui::FontId::new(36.0, theme::display_family()))
                        .color(egui::Color32::WHITE),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::secondary_button(ui, t(zh, "返回控制台", "Back to console"), palette)
                    .clicked()
                {
                    self.page = Page::Dashboard;
                }
                let refresh = ui.add_enabled_ui(!self.usage_loading, |ui| {
                    theme::primary_button(
                        ui,
                        egui::RichText::new(t(zh, "↻ 刷新查询", "↻ Refresh"))
                            .strong()
                            .color(egui::Color32::WHITE),
                        palette,
                    )
                });
                if refresh.inner.clicked() {
                    self.refresh_usage_monitor();
                }
            });
        });
        ui.add_space(10.0);

        if self.usage_loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    egui::RichText::new(t(
                        zh,
                        "正在查询订阅额度与 API token 用量…",
                        "Querying subscription quotas and API token usage…",
                    ))
                    .color(palette.paper),
                );
            });
            ui.add_space(6.0);
        }
        if !self.usage_error.is_empty() {
            egui::Frame::new()
                .fill(palette.paper)
                .stroke(egui::Stroke::new(1.0, palette.danger))
                .corner_radius(egui::CornerRadius::same(7))
                .inner_margin(egui::Margin::same(14))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{}：{}",
                            t(zh, "查询失败", "Query failed"),
                            friendly_error(&self.usage_error, zh)
                        ))
                        .color(palette.danger),
                    );
                });
            ui.add_space(8.0);
        }

        let Some(snapshot) = self.usage_snapshot.as_ref() else {
            if self.usage_loading {
                egui::Frame::new()
                    .fill(palette.glass)
                    .stroke(egui::Stroke::new(1.0, palette.line))
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::same(20))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(t(
                                zh,
                                "正在准备当前配置的用量概览",
                                "Preparing usage for the active profile",
                            ))
                            .strong()
                            .color(palette.ink),
                        );
                        ui.add_space(12.0);
                        for width_factor in [0.92_f32, 0.68_f32, 0.81_f32] {
                            let width = ui.available_width() * width_factor;
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(width.max(120.0), 14.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().rect_filled(
                                rect,
                                4.0,
                                egui::Color32::from_rgba_unmultiplied(
                                    palette.muted.r(),
                                    palette.muted.g(),
                                    palette.muted.b(),
                                    42,
                                ),
                            );
                            ui.add_space(10.0);
                        }
                    });
            } else {
                theme::paper_frame(palette).show(ui, |ui| {
                    ui.label(t(
                        zh,
                        "暂无监控数据，请刷新查询。",
                        "No usage data yet. Refresh to query.",
                    ));
                });
            }
            return;
        };

        let quota_plan_count = snapshot.subscriptions.len()
            + snapshot
                .api_channels
                .iter()
                .filter(|account| Self::usage_account_is_quota_plan(account))
                .count();
        let metered_count =
            snapshot.api_channels.len() + snapshot.subscriptions.len() - quota_plan_count;
        let compact_summary = ui.available_width() < 1000.0;
        egui::Frame::new()
            .fill(palette.glass)
            .stroke(egui::Stroke::new(1.0, palette.line))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(t(zh, "当前配置", "ACTIVE PROFILE"))
                                .small()
                                .color(palette.muted),
                        );
                        ui.label(
                            egui::RichText::new(&snapshot.profile_name)
                                .strong()
                                .color(palette.ink),
                        );
                    });
                    ui.separator();
                    ui.label(format!(
                        "{} {} · {} {}",
                        quota_plan_count,
                        t(zh, "订阅/套餐", "plan"),
                        metered_count,
                        t(zh, "按量", "metered")
                    ));
                    ui.separator();
                    ui.label(format!(
                        "{} tokens · {} {} · ${:.4}",
                        Self::compact_number(snapshot.total_tokens),
                        snapshot.total_requests,
                        t(zh, "次请求", "requests"),
                        snapshot.total_cost
                    ));
                    if !compact_summary {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}: {}",
                                    t(zh, "上次查询", "Last queried"),
                                    Self::usage_timestamp(&snapshot.queried_at)
                                ))
                                .small()
                                .color(palette.muted),
                            );
                        });
                    }
                });
                if compact_summary {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}: {}",
                                    t(zh, "上次查询", "Last queried"),
                                    Self::usage_timestamp(&snapshot.queried_at)
                                ))
                                .small()
                                .color(palette.muted),
                            );
                        });
                    });
                }
            });
        ui.add_space(8.0);

        let mut usage_reorder = None;
        let two_columns = usage_monitor_uses_two_columns(ui.available_width());
        // Merge OAuth subscriptions and Coding Plan style API channels into one
        // quota-window group; plain pay-as-you-go channels form the second group.
        let mut quota_accounts: Vec<&UsageAccount> = snapshot.subscriptions.iter().collect();
        quota_accounts.extend(
            snapshot
                .api_channels
                .iter()
                .filter(|account| Self::usage_account_is_quota_plan(account)),
        );
        let metered_accounts: Vec<&UsageAccount> = snapshot
            .api_channels
            .iter()
            .filter(|account| !Self::usage_account_is_quota_plan(account))
            .collect();
        // Shrink to content so short snapshots do not leave a tall empty band;
        // still scroll when the card stack exceeds the remaining viewport.
        let cards_budget = ui.available_height().max(96.0);
        egui::ScrollArea::vertical()
            .id_salt("usage-monitor-scroll")
            .auto_shrink([false, true])
            .max_height(cards_budget)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                theme::eyebrow(
                    ui,
                    t(zh, "订阅 / 套餐额度", "SUBSCRIPTIONS / PLAN QUOTAS"),
                    palette.paper,
                );
                ui.add_space(4.0);
                if quota_accounts.is_empty() {
                    ui.label(
                        egui::RichText::new(t(
                            zh,
                            "当前配置没有订阅或套餐窗口账号。",
                            "The active profile has no subscription or quota-window accounts.",
                        ))
                        .color(palette.paper),
                    );
                } else {
                    usage_reorder = usage_reorder.or(Self::show_usage_account_grid(
                        ui,
                        &quota_accounts,
                        palette,
                        zh,
                        true,
                        two_columns,
                    ));
                }

                ui.add_space(10.0);
                theme::eyebrow(
                    ui,
                    t(zh, "按量付费 API", "PAY-AS-YOU-GO API"),
                    palette.paper,
                );
                ui.add_space(4.0);
                if metered_accounts.is_empty() {
                    ui.label(
                        egui::RichText::new(t(
                            zh,
                            "当前配置没有按量付费的 API Key 渠道。",
                            "The active profile has no pay-as-you-go API-key channels.",
                        ))
                        .color(palette.paper),
                    );
                } else {
                    usage_reorder = usage_reorder.or(Self::show_usage_account_grid(
                        ui,
                        &metered_accounts,
                        palette,
                        zh,
                        false,
                        two_columns,
                    ));
                }
                ui.add_space(8.0);
            });
        if let Some((section, source_id, target_id)) = usage_reorder {
            let accounts = match (section, self.usage_snapshot.as_mut()) {
                (UsageOrderSection::Subscription, Some(snapshot)) => &mut snapshot.subscriptions,
                (UsageOrderSection::Api, Some(snapshot)) => &mut snapshot.api_channels,
                (_, None) => return,
            };
            let source_index = accounts.iter().position(|account| account.id == source_id);
            let target_index = accounts.iter().position(|account| account.id == target_id);
            if let (Some(source_index), Some(target_index)) = (source_index, target_index) {
                if move_list_item(accounts, source_index, target_index) {
                    let order = accounts
                        .iter()
                        .map(|account| account.id)
                        .collect::<Vec<_>>();
                    match section {
                        UsageOrderSection::Subscription => {
                            self.monitor_subscription_order = order;
                        }
                        UsageOrderSection::Api => self.monitor_api_order = order,
                    }
                    self.persist_ui_preferences();
                }
            }
        }
    }

    pub(crate) fn active_route_config_name(&self, zh: bool) -> String {
        if self.applying || self.router_mode_switching {
            return t(zh, "正在应用…", "Applying…").to_owned();
        }
        self.isolation_profiles
            .iter()
            .find(|profile| profile.id == self.active_profile_id)
            .map(|profile| profile.name.clone())
            .unwrap_or_else(|| {
                t(
                    zh,
                    if self.configured {
                        "当前配置"
                    } else {
                        "未保存配置"
                    },
                    if self.configured {
                        "Current config"
                    } else {
                        "Unsaved config"
                    },
                )
                .to_owned()
            })
    }

    fn show_dashboard(&mut self, ui: &mut egui::Ui, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let header_width = ui.available_width();
        let compact_header = header_width < 1080.0;
        let narrow_header = header_width < 900.0;
        ui.horizontal(|ui| {
            // English labels are noticeably wider, so the title column needs more
            // room before the share/action controls begin.
            ui.allocate_ui_with_layout(
                egui::vec2(
                    if narrow_header {
                        if zh { 180.0 } else { 200.0 }
                    } else if zh {
                        230.0
                    } else if compact_header {
                        250.0
                    } else {
                        275.0
                    },
                    72.0,
                ),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    theme::eyebrow(
                        ui,
                        if narrow_header {
                            t(zh, "本地路由", "LOCAL ROUTER")
                        } else {
                            t(zh, "本地模型控制中心", "LOCAL MODEL CONTROL PLANE")
                        },
                        palette.paper,
                    );
                    ui.label(
                        egui::RichText::new(if zh {
                            "路由控制台"
                        } else if narrow_header {
                            "CONSOLE"
                        } else {
                            "ROUTER CONSOLE"
                        })
                        .font(egui::FontId::new(
                            if narrow_header {
                                27.0
                            } else if zh {
                                34.0
                            } else {
                                30.0
                            },
                            theme::display_family(),
                        ))
                        .color(egui::Color32::WHITE),
                    );
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let action_width = if narrow_header {
                    104.0
                } else if compact_header {
                    142.0
                } else {
                    180.0
                };
                if ui
                    .add_sized(
                        [action_width, 42.0],
                        egui::Button::new(if narrow_header {
                            t(zh, "配置分组", "Groups")
                        } else {
                            t(zh, "切换配置分组", "Switch groups")
                        })
                            .fill(palette.paper)
                            .stroke(egui::Stroke::new(1.0, palette.line))
                            .corner_radius(egui::CornerRadius::same(8)),
                    )
                    .clicked()
                {
                    self.open_profiles();
                }
                if ui
                    .add_sized(
                        [action_width, 42.0],
                        egui::Button::new(if narrow_header {
                            t(zh, "用量", "Usage")
                        } else {
                            t(zh, "实时用量统计", "Live usage")
                        })
                            .fill(palette.paper)
                            .stroke(egui::Stroke::new(1.0, palette.line))
                            .corner_radius(egui::CornerRadius::same(8)),
                    )
                    .on_hover_text(t(
                        zh,
                        "查看当前配置的订阅额度、重置时间与 API token 用量",
                        "View subscription quotas, reset times, and API token usage for this profile",
                    ))
                    .clicked()
                {
                    self.open_usage_monitor();
                }
            });
        });
        ui.add_space(14.0);
        let panel_height = ui.available_height();
        let wide = dashboard_uses_wide_layout(ui.available_width(), panel_height);
        if wide {
            let width = ui.available_width();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(width * 0.25, (panel_height - 24.0).max(440.0)),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| self.dashboard_sidebar(ui, palette, panel_height - 24.0),
                );
                ui.add_space(16.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(width * 0.75 - 28.0, panel_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| self.dashboard_models(ui, palette, panel_height),
                );
            });
        } else {
            egui::ScrollArea::vertical()
                .id_salt("dashboard-compact-scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    self.dashboard_sidebar(ui, palette, 560.0);
                    ui.add_space(16.0);
                    self.dashboard_models(ui, palette, 720.0);
                });
        }
    }

    fn show_share_codex_state_toggle(
        &mut self,
        ui: &mut egui::Ui,
        palette: &theme::Palette,
        zh: bool,
    ) {
        let mut switch_clicked = false;
        let card = egui::Frame::new()
            .fill(palette.paper)
            .stroke(egui::Stroke::new(1.0, palette.line))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::symmetric(16, 12))
            .shadow(theme::soft_card_shadow())
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.set_max_width((ui.available_width() - 80.0).max(180.0));
                        ui.label(
                            egui::RichText::new(t(zh, "共享会话与设置", "Share tasks & settings"))
                                .strong()
                                .color(palette.ink),
                        );
                        ui.label(
                            egui::RichText::new(t(
                                zh,
                                "默认开启。同一 Codex 账号下切换官方路由或 Router 本地配置时，共享会话记录、登录状态和个人设置；检测到不同账号时自动隔离。",
                                "On by default. Official and local Router profiles share tasks, login state, and personal settings for the same Codex account. Different accounts stay isolated.",
                            ))
                            .small()
                            .color(palette.muted),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        switch_clicked = Self::dashboard_route_switch(
                            ui,
                            self.share_codex_state,
                            false,
                            palette,
                        )
                        .clicked();
                    });
                });
            })
            .response
            .interact(egui::Sense::click())
            .on_hover_text(t(
                zh,
                "在切换配置分组时生效：控制是否在配置之间共享 Codex 会话与设置",
                "Applies when switching configuration groups: controls whether Codex tasks and settings are shared across profiles",
            ));
        if switch_clicked || card.clicked() {
            let previous = self.share_codex_state;
            self.share_codex_state = !self.share_codex_state;
            if self.persist_ui_preferences() {
                self.status_text = if zh {
                    if self.share_codex_state {
                        "已开启共享：同一 Codex 账号的会话、登录状态与个人设置会在配置间保持一致"
                    } else {
                        "已关闭共享：配置切换将恢复各自的完整 Codex 快照"
                    }
                } else if self.share_codex_state {
                    "Sharing enabled: tasks, login state, and personal settings stay consistent for the same Codex account"
                } else {
                    "Sharing disabled: switching profiles restores each complete Codex snapshot"
                }
                .to_owned();
            } else {
                self.share_codex_state = previous;
            }
        }
    }

    fn dashboard_route_switch(
        ui: &mut egui::Ui,
        enabled: bool,
        switching: bool,
        palette: &theme::Palette,
    ) -> egui::Response {
        let desired_size = egui::vec2(58.0, 30.0);
        let sense = if switching {
            egui::Sense::hover()
        } else {
            egui::Sense::click()
        };
        let (rect, response) = ui.allocate_exact_size(desired_size, sense);
        let position = ui.ctx().animate_bool(response.id, enabled);
        let track = if enabled {
            palette.action
        } else {
            palette.line
        };
        ui.painter().rect_filled(rect, rect.height() * 0.5, track);
        let radius = 11.0;
        let left = rect.left() + rect.height() * 0.5;
        let right = rect.right() - rect.height() * 0.5;
        let center_x = egui::lerp(left..=right, position);
        ui.painter().circle_filled(
            egui::pos2(center_x, rect.center().y),
            radius,
            if switching {
                palette.paper_alt
            } else {
                egui::Color32::WHITE
            },
        );
        response
    }

    fn dashboard_setting_toggle_row(
        ui: &mut egui::Ui,
        title: &str,
        subtitle: &str,
        enabled: bool,
        changing: bool,
        palette: &theme::Palette,
    ) -> bool {
        let mut switch_clicked = false;
        let row = egui::Frame::new()
            .fill(palette.paper_alt)
            .stroke(egui::Stroke::new(1.0, palette.line))
            .corner_radius(egui::CornerRadius::same(7))
            .inner_margin(egui::Margin::symmetric(10, 3))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(title).strong().color(palette.ink));
                        ui.label(egui::RichText::new(subtitle).small().color(palette.muted));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        switch_clicked =
                            Self::dashboard_route_switch(ui, enabled, changing, palette).clicked();
                    });
                });
            })
            .response
            .interact(if changing {
                egui::Sense::hover()
            } else {
                egui::Sense::click()
            });
        !changing && (switch_clicked || row.clicked())
    }

    fn dashboard_sidebar(
        &mut self,
        ui: &mut egui::Ui,
        palette: &theme::Palette,
        target_height: f32,
    ) {
        let zh = self.ui_language == "zh";
        theme::paper_frame(palette)
            .inner_margin(egui::Margin::same(14))
            .outer_margin(egui::Margin {
                left: 0,
                right: 0,
                top: 0,
                bottom: 16,
            })
            .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height((target_height - 40.0).max(440.0));
            theme::eyebrow(ui, t(zh, "系统 / 概览", "SYSTEM / OVERVIEW"), palette.muted);
            let active_config_name = self.active_route_config_name(zh);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(format!("{:02}", self.config.models.len()))
                            .font(egui::FontId::new(44.0, theme::display_family()))
                            .color(palette.ink),
                    );
                    ui.label(
                        egui::RichText::new(t(zh, "已配置模型", "configured models"))
                            .font(egui::FontId::new(18.0, theme::serif_family()))
                            .italics()
                            .color(palette.ink_soft),
                    );
                });
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    ui.set_max_width((ui.available_width()).clamp(96.0, 160.0));
                    ui.label(
                        egui::RichText::new(t(zh, "当前配置", "CURRENT CONFIG"))
                            .small()
                            .strong()
                            .color(palette.muted),
                    );
                    if self.applying {
                        // Show the apply progress here instead of adding a row to
                        // the models panel, which used to push the activity log
                        // below the window edge.
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(t(zh, "正在应用…", "Applying…"))
                                        .size(13.0)
                                        .strong()
                                        .color(palette.ink),
                                )
                                .truncate(),
                            );
                        });
                    } else {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&active_config_name)
                                    .size(14.0)
                                    .strong()
                                    .color(palette.ink),
                            )
                            .wrap(),
                        );
                    }
                });
            });
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(t(zh, "项目根目录", "PROJECT ROOT"))
                    .small()
                    .strong()
                    .color(palette.muted),
            );
            ui.add(
                egui::Label::new(
                    egui::RichText::new(self.router_root.display().to_string())
                        .small()
                        .color(palette.ink_soft),
                )
                .wrap(),
            );
            ui.add_space(8.0);
            let route_switch = egui::Frame::new()
                .fill(palette.paper_alt)
                .stroke(egui::Stroke::new(1.0, palette.line))
                .corner_radius(egui::CornerRadius::same(7))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new(t(zh, "Codex 路由", "CODEX ROUTE"))
                                    .strong()
                                    .color(palette.ink),
                            );
                            ui.label(
                                egui::RichText::new(if self.router_mode_switching {
                                    t(zh, "正在切换…", "Switching…")
                                } else if self.router_mode_enabled {
                                    t(zh, "已开启 · 本地 Router", "ON · Local Router")
                                } else {
                                    t(zh, "已关闭 · 官方路由", "OFF · Official route")
                                })
                                .small()
                                .color(palette.muted),
                            );
                        });
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                Self::dashboard_route_switch(
                                    ui,
                                    self.router_mode_enabled,
                                    self.router_mode_switching || self.applying,
                                    palette,
                                )
                            },
                        )
                        .inner
                    })
                    .inner
                })
                .inner
                .on_hover_text(t(
                    zh,
                    "开启会启动 Router 并应用当前配置；关闭会先恢复 Codex 官方配置，再停止转发。切换后请完全重启 Codex。",
                    "ON starts Router and applies this profile. OFF restores the official Codex configuration before stopping forwarding. Fully restart Codex after switching.",
                ));
            if route_switch.clicked() {
                if self.router_mode_enabled {
                    self.disable_router_mode();
                } else {
                    self.enable_router_mode();
                }
            }
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            let minimize_on_close = self.close_behavior == CloseBehavior::MinimizeToTray;
            let close_subtitle = match self.close_behavior {
                CloseBehavior::Ask => t(zh, "首次关闭时询问", "Ask on first close"),
                CloseBehavior::MinimizeToTray => {
                    t(zh, "关闭窗口后保持转发", "Keep forwarding after closing")
                }
                CloseBehavior::Exit => t(zh, "关闭窗口时停止转发", "Stop forwarding on close"),
            };
            if Self::dashboard_setting_toggle_row(
                ui,
                t(zh, "按 X 最小化到托盘", "Minimize on X"),
                close_subtitle,
                minimize_on_close,
                false,
                palette,
            ) {
                let previous = self.close_behavior;
                self.close_behavior = if minimize_on_close {
                    CloseBehavior::Exit
                } else {
                    CloseBehavior::MinimizeToTray
                };
                if self.persist_close_behavior() {
                    self.status_text = t(
                        zh,
                        "窗口关闭设置已保存",
                        "Window close behavior saved",
                    )
                    .into();
                } else {
                    self.close_behavior = previous;
                }
            }
            ui.add_space(5.0);
            let autostart_enabled = self.config.deploy.start_with_windows;
            if Self::dashboard_setting_toggle_row(
                ui,
                t(zh, "开机静默启动", "Silent startup"),
                if self.autostart_switching {
                    t(zh, "正在更新…", "Updating…")
                } else if autostart_enabled {
                    t(zh, "登录后进入轻量托盘", "Open in lightweight tray mode")
                } else {
                    t(zh, "不开机自启", "Do not start with Windows")
                },
                autostart_enabled,
                self.autostart_switching,
                palette,
            ) {
                self.set_start_with_windows(!autostart_enabled);
            }
            ui.add_space(6.0);
        });
    }

    fn dashboard_models(
        &mut self,
        ui: &mut egui::Ui,
        palette: &theme::Palette,
        target_height: f32,
    ) {
        let zh = self.ui_language == "zh";
        // Keep the split within both the requested allocation and the current
        // clip rect. On scaled Windows displays the parent allocation can be
        // taller than the visible region, which otherwise clips the log.
        let dashboard_bottom_space = 24.0;
        let visible_height = (ui.clip_rect().bottom() - ui.cursor().top()).max(0.0);
        let layout_height = target_height.min(visible_height);
        // Cap the model panel at about 2.5 rows, then reserve a compact
        // three-line log preview and a visible footer gap.
        let (model_content_height, list_height, log_content_height) =
            dashboard_panel_heights(layout_height);
        theme::glass_frame(palette).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height(model_content_height);
            ui.horizontal(|ui| {
                // Reserve the action row first so the heading can never be
                // overlapped by it. English action labels are wider than Chinese.
                let actions_width = if zh { 430.0_f32 } else { 520.0_f32 };
                let title_width = (ui.available_width() - actions_width).max(168.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(title_width, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_max_width(title_width);
                        theme::eyebrow(
                            ui,
                            t(zh, "模型渠道", "MODEL CHANNELS"),
                            palette.background_dark,
                        );
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(t(zh, "你的路由配置", "Router Configuration"))
                                    .font(egui::FontId::new(
                                        if zh { 24.0 } else { 18.0 },
                                        theme::display_family(),
                                    ))
                                    .color(palette.ink),
                            )
                            .truncate(),
                        );
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::accent_button(
                        ui,
                        egui::RichText::new("＋")
                            .size(22.0)
                            .color(egui::Color32::WHITE),
                        palette,
                    )
                    .on_hover_text(t(zh, "新增模型渠道", "Add model channel"))
                    .clicked()
                    {
                        self.temp_model = ModelConfig::default();
                        self.temp_model.priority =
                            super::logic::next_api_channel_priority(&self.config);
                        self.editing_model = None;
                        self.model_from_wizard = false;
                        self.advanced_json_open = false;
                        self.page = Page::Model;
                    }
                    let response = ui.add_enabled_ui(!self.applying, |ui| {
                        theme::primary_button(
                            ui,
                            egui::RichText::new(t(zh, "保存并应用", "Save & apply"))
                                .strong()
                                .color(egui::Color32::WHITE),
                            palette,
                        )
                    });
                    if response.inner.clicked() {
                        self.apply_all();
                    }
                    let oauth_count = self.config.oauth_account_ids.as_ref().map_or(0, Vec::len);
                    let oauth_response = ui
                        .add_sized(
                            [178.0, 44.0],
                            egui::Button::new(
                                egui::RichText::new(format!(
                                    "{} ({oauth_count})",
                                    t(zh, "当前配置 OAuth", "Profile OAuth")
                                ))
                                .strong()
                                .color(egui::Color32::WHITE),
                            )
                            .fill(palette.accent)
                            .stroke(egui::Stroke::NONE)
                            .corner_radius(egui::CornerRadius::same(7)),
                        )
                        .on_hover_text(t(
                            zh,
                            "管理只属于当前配置的 OAuth 账号、模型和回退策略",
                            "Manage OAuth accounts, models, and fallback policy for this profile",
                        ));
                    if oauth_response.clicked() {
                        self.open_oauth_manager();
                    }
                    if ui
                        .add_sized(
                            [142.0, 44.0],
                            egui::Button::new(
                                egui::RichText::new(t(
                                    zh,
                                    "常见渠道快速配置",
                                    "Common provider setup",
                                ))
                                    .strong()
                                    .color(palette.ink),
                            )
                            .fill(palette.paper)
                            .stroke(egui::Stroke::new(1.0, palette.line))
                            .corner_radius(egui::CornerRadius::same(7)),
                        )
                        .on_hover_text(t(
                            zh,
                            "查看 OpenAI、Claude、OpenRouter、Kimi、MiMo 等常见 API 渠道；推荐平台单独展示",
                            "Choose OpenAI, Claude, OpenRouter, Kimi, MiMo, and other common APIs; recommended platforms are listed separately",
                        ))
                        .clicked()
                    {
                        self.channel_preset_dialog_open = true;
                    }
                });
            });
            // Apply progress lives in the sidebar / activity log only. Do not
            // render a status line under the model-channel heading — it left a
            // leftover subtitle and pushed the card list down.
            if self
                .status_expires_at
                .is_some_and(|deadline| std::time::Instant::now() > deadline)
            {
                self.status_text.clear();
                self.status_expires_at = None;
            }
            ui.add_space(10.0);
            let mut edit = None;
            let mut delete = None;
            let mut set_default = None;
            let mut reorder = None;
            let current_default = super::logic::resolve_default_model(&self.config)
                .unwrap_or_default()
                .to_owned();
            if !self.config.models.is_empty() {
                egui::ScrollArea::vertical()
                    .id_salt("dashboard-model-list-scroll")
                    .max_height(list_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                    for (index, model) in self.config.models.iter().enumerate() {
                        let vision = super::logic::resolve_multimodal(model);
                        let is_default = model.model == current_default;
                        let response = egui::Frame::new()
                            .fill(palette.paper)
                            .stroke(egui::Stroke::new(1.0_f32, palette.line))
                            .shadow(theme::soft_card_shadow())
                            .inner_margin(egui::Margin::symmetric(18, 15))
                            .show(ui, |ui| {
                                ui.vertical(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(format!("{:02}", index + 1))
                                                .font(egui::FontId::new(
                                                    23.0,
                                                    theme::display_family(),
                                                ))
                                                .color(palette.accent),
                                        );
                                        ui.label(
                                            egui::RichText::new(if model.alias.is_empty() {
                                                &model.model
                                            } else {
                                                &model.alias
                                            })
                                            .font(egui::FontId::new(19.0, theme::display_family()))
                                            .color(palette.ink)
                                            .strong(),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                let drag_handle = ui
                                                    .add(
                                                        egui::Button::new(
                                                            egui::RichText::new("≡")
                                                                .size(18.0)
                                                                .color(palette.ink_soft),
                                                        )
                                                        .fill(palette.paper_alt)
                                                        .stroke(egui::Stroke::new(
                                                            1.0,
                                                            palette.line,
                                                        ))
                                                        .corner_radius(egui::CornerRadius::same(5))
                                                        .sense(egui::Sense::drag()),
                                                    )
                                                    .on_hover_text(t(
                                                        zh,
                                                        "长按此手柄并拖到其他模型卡片上排序",
                                                        "Hold this handle and drag onto another model card to reorder",
                                                    ));
                                                drag_handle.dnd_set_drag_payload(ModelOrderDrag {
                                                    source_index: index,
                                                });
                                                if ui
                                                    .small_button(t(zh, "删除", "Delete"))
                                                    .clicked()
                                                {
                                                    delete = Some(index);
                                                }
                                                if ui
                                                    .small_button(t(zh, "编辑", "Edit"))
                                                    .clicked()
                                                {
                                                    edit = Some(index);
                                                }
                                                if ui
                                                    .selectable_label(
                                                        is_default,
                                                        if is_default {
                                                            t(zh, "默认模型", "Default model")
                                                        } else {
                                                            t(zh, "设为默认", "Make default")
                                                        },
                                                    )
                                                    .on_hover_text(t(
                                                        zh,
                                                        "新建 Codex 窗口和任务时默认使用此模型",
                                                        "Use this model for new Codex windows and threads",
                                                    ))
                                                    .clicked()
                                                {
                                                    set_default = Some(model.model.clone());
                                                }
                                            },
                                        );
                                    });
                                    ui.add_space(6.0);
                                    ui.horizontal_wrapped(|ui| {
                                        theme::pill(
                                            ui,
                                            &format!(
                                                "{}K / {}%",
                                                super::logic::resolve_context_window(model) / 1000,
                                                model.auto_compact_percent.clamp(60, 90)
                                            ),
                                            palette.paper_alt,
                                            palette.ink_soft,
                                        );
                                        theme::pill(
                                            ui,
                                            if model.source == "oauth" {
                                                "OAUTH"
                                            } else if super::logic::is_oauth_fallback_model(
                                                &self.config,
                                                model,
                                            ) {
                                                "FALLBACK"
                                            } else {
                                                "API KEY"
                                            },
                                            palette.paper_alt,
                                            palette.ink_soft,
                                        );
                                        theme::pill(
                                            ui,
                                            if vision {
                                                t(zh, "视觉", "VISION")
                                            } else {
                                                t(zh, "文本", "TEXT")
                                            },
                                            palette.paper_alt,
                                            if vision {
                                                palette.success
                                            } else {
                                                palette.muted
                                            },
                                        );
                                        ui.label(
                                            egui::RichText::new(
                                                super::logic::model_routing_explanation(
                                                    &self.config,
                                                    model,
                                                    zh,
                                                ),
                                            )
                                            .small()
                                            .color(palette.ink_soft),
                                        );
                                        // Endpoint belongs on the same line as the
                                        // route description so the card stays compact.
                                        ui.label(
                                            egui::RichText::new(if model.source == "oauth" {
                                                t(
                                                    zh,
                                                    "Sub2API 托管 OAuth",
                                                    "Sub2API-managed OAuth",
                                                )
                                                .to_owned()
                                            } else {
                                                model.base_url.clone()
                                            })
                                            .small()
                                            .color(palette.background_dark),
                                        );
                                    });
                                });
                            });
                        if response.response.hovered() {
                            ui.painter().rect_stroke(
                                response.response.rect,
                                egui::CornerRadius::same(2),
                                egui::Stroke::new(1.5_f32, palette.background_dark),
                                egui::StrokeKind::Outside,
                            );
                        }
                        if let Some(payload) =
                            response.response.dnd_hover_payload::<ModelOrderDrag>()
                        {
                            if payload.source_index != index {
                                ui.painter().rect_stroke(
                                    response.response.rect,
                                    egui::CornerRadius::same(2),
                                    egui::Stroke::new(2.0, palette.action),
                                    egui::StrokeKind::Outside,
                                );
                            }
                        }
                        if let Some(payload) =
                            response.response.dnd_release_payload::<ModelOrderDrag>()
                        {
                            reorder = Some((payload.source_index, index));
                        }
                    }
                    });
            }
            if let Some((source_index, target_index)) = reorder {
                if move_list_item(&mut self.config.models, source_index, target_index) {
                    edit = None;
                    delete = None;
                    set_default = None;
                    if self.ui_audit_mode {
                        self.status_text = t(
                            zh,
                            "审计预览中的模型顺序已更新；未写入配置",
                            "Model order updated in the audit preview; configuration was not written",
                        )
                        .to_owned();
                    } else {
                        match self
                            .config
                            .save(&crate::user_data::config_path(&self.router_root))
                        {
                            Ok(()) => {
                                self.status_text = t(
                                    zh,
                                    "路由模型顺序已保存；转发优先级保持不变",
                                    "Model display order saved; routing priorities are unchanged",
                                )
                                .to_owned();
                            }
                            Err(error) => {
                                let message = if zh {
                                    format!("模型已排序，但保存失败：{error}")
                                } else {
                                    format!("Models were reordered, but saving failed: {error}")
                                };
                                self.report_error(message);
                            }
                        }
                    }
                }
            }
            if let Some(index) = delete {
                self.config.models.remove(index);
                super::logic::normalize_default_model(&mut self.config);
            }
            if let Some(model) = set_default {
                self.config.default_model = model.clone();
                self.status_text = if zh {
                    format!("默认模型已选择：{model}；点击“保存并应用”后生效")
                } else {
                    format!("Default model selected: {model}. Save & apply to activate it.")
                };
            }
            if let Some(index) = edit {
                self.temp_model = self.config.models[index].clone();
                self.editing_model = Some(index);
                self.model_from_wizard = false;
                self.advanced_json_open = false;
                self.page = Page::Model;
            }
            if self.config.models.is_empty() {
                egui::Frame::new()
                    .fill(palette.paper_alt)
                    .shadow(theme::soft_card_shadow())
                    .inner_margin(egui::Margin::same(24))
                    .show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new(t(zh, "尚未添加模型", "NO MODEL YET"))
                                    .font(egui::FontId::new(20.0, theme::display_family()))
                                    .color(palette.ink),
                            );
                            ui.label(
                                egui::RichText::new(t(
                                    zh,
                                    "点击模型配置卡片右上角的 ＋ 添加第一个模型",
                                    "Click ＋ in the model card to add your first model",
                                ))
                                .color(palette.muted),
                            );
                        });
                    });
            }
        });
        ui.add_space(12.0);
        let mut clear_log = false;
        let mut export_log = false;
        theme::dark_glass_frame(palette).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), log_content_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.horizontal(|ui| {
                        theme::eyebrow(
                            ui,
                            t(zh, "活动日志", "ACTIVITY LOG"),
                            egui::Color32::from_rgb(220, 220, 215),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button("↗")
                                .on_hover_text(t(zh, "展开运行日志", "Open runtime log"))
                                .clicked()
                            {
                                self.log_dialog_open = true;
                            }
                            if ui
                                .small_button("↓")
                                .on_hover_text(t(zh, "下载脱敏日志", "Download redacted log"))
                                .clicked()
                            {
                                export_log = true;
                            }
                            if ui
                                .small_button("×")
                                .on_hover_text(t(zh, "清空日志", "Clear log"))
                                .clicked()
                            {
                                clear_log = true;
                            }
                            let previous_follow = self.log_follow_latest;
                            ui.checkbox(&mut self.log_follow_latest, t(zh, "跟随最新", "Follow"));
                            if self.log_follow_latest && !previous_follow {
                                self.log_scroll_to_bottom = true;
                            }
                        });
                    });
                    let content = if self.logs.is_empty() {
                        t(zh, "等待操作…", "Waiting for an action…").to_owned()
                    } else {
                        log_excerpt(&self.logs, 200, 320)
                    };
                    let scroll = egui::ScrollArea::vertical()
                        .id_salt("dashboard-activity-log")
                        .max_height(log_content_height - 34.0)
                        .auto_shrink([false, false])
                        .stick_to_bottom(self.log_follow_latest)
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(&content)
                                    .monospace()
                                    .small()
                                    .color(egui::Color32::WHITE),
                            );
                            if self.log_scroll_to_bottom {
                                ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
                            }
                        });
                    let at_bottom = scroll.state.offset.y + scroll.inner_rect.height()
                        >= scroll.content_size.y - 4.0;
                    // Keep "Follow latest" on by default. Only drop it when the
                    // user intentionally scrolls this log away from the bottom.
                    let pointer_over_log = ui.rect_contains_pointer(scroll.inner_rect);
                    let scrolled_up = ui.input(|input| input.smooth_scroll_delta.y > 0.5);
                    if self.log_follow_latest
                        && !at_bottom
                        && pointer_over_log
                        && scrolled_up
                        && !self.log_scroll_to_bottom
                    {
                        self.log_follow_latest = false;
                    }
                    self.log_scroll_to_bottom = false;
                },
            );
        });
        if clear_log {
            self.logs.clear();
            self.log_scroll_to_bottom = false;
        }
        if export_log {
            self.export_logs();
        }
        ui.add_space(dashboard_bottom_space);
    }
}
