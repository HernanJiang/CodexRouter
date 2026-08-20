use super::{
    ensure_full_ui_fonts, theme, CloseBehavior, CodexRouterApp, IsolationKind, IsolationProfile,
    ModelConfig, OAuthAccountSummary, OAuthModelSummary, Page, RouterConfig, UsageAccount, UsageSnapshot,
    UsageWindow, APP_VERSION,
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
const SUBSCRIPTION_CARD_HEIGHT: f32 = 430.0;
const SUBSCRIPTION_MODEL_LIST_HEIGHT: f32 = 160.0;

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
    if viewport_width < 1120.0 || viewport_height < 700.0 {
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

fn dashboard_uses_wide_layout(available_width: f32, _available_height: f32) -> bool {
    available_width >= 860.0
}

fn usage_monitor_uses_two_columns(available_width: f32) -> bool {
    available_width >= 920.0
}

fn usage_column_indices(account_count: usize, column_count: usize) -> Vec<Vec<usize>> {
    let column_count = column_count.max(1);
    let mut columns = vec![Vec::new(); column_count];
    for index in 0..account_count {
        columns[index % column_count].push(index);
    }
    columns
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

fn subscription_provider_key(platform: &str) -> String {
    match platform.trim().to_ascii_lowercase().as_str() {
        "openai" | "chatgpt" => "chatgpt".to_owned(),
        "grok" | "xai" | "x-ai" => "grok".to_owned(),
        "anthropic" | "claude" => "claude".to_owned(),
        "gemini" | "google" | "google_one" | "google-one" => "gemini".to_owned(),
        "antigravity" => "antigravity".to_owned(),
        "" => "subscription".to_owned(),
        value => value.to_owned(),
    }
}

fn subscription_provider_title(platform: &str) -> &'static str {
    match subscription_provider_key(platform).as_str() {
        "chatgpt" => "ChatGPT",
        "grok" => "Grok",
        "claude" => "Claude",
        "gemini" => "Gemini",
        "antigravity" => "Antigravity",
        _ => "Subscription",
    }
}

fn subscription_account_identifier(account: &OAuthAccountSummary) -> String {
    let identity = if !account.email.trim().is_empty() {
        account.email.trim().to_owned()
    } else if !account.name.trim().is_empty() {
        account.name.trim().to_owned()
    } else {
        format!("#{}", account.id)
    };
    if account.plan.trim().is_empty() {
        identity
    } else {
        format!("{identity} ({})", account.plan.trim())
    }
}

fn priority_endpoint_labels(
    model: &ModelConfig,
    accounts: &[OAuthAccountSummary],
    zh: bool,
) -> (String, String, String) {
    let is_oauth = model.source == "oauth";
    let profile = crate::logic::classify_channel_route(model);
    let vendor = profile.vendor;
    let source_label = if is_oauth {
        format!("{} · {}", t(zh, "订阅", "Subscription"), vendor)
    } else {
        format!("API · {}", vendor)
    };
    let value = if is_oauth {
        accounts
            .iter()
            .find(|account| account.id == model.oauth_account_id)
            .map(subscription_account_identifier)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                format!(
                    "{} #{}",
                    subscription_provider_title(&model.oauth_platform),
                    model.oauth_account_id
                )
            })
    } else if model.base_url.trim().is_empty() {
        "—".to_owned()
    } else {
        model.base_url.trim().to_owned()
    };
    let full = if is_oauth {
        format!(
            "{}\n{}: #{}\n{}: {}",
            source_label,
            t(zh, "账号 ID", "Account ID"),
            model.oauth_account_id,
            t(zh, "登录账号", "Signed-in account"),
            value
        )
    } else {
        format!("{source_label}\nBase URL: {value}")
    };
    let chars = value.chars().collect::<Vec<_>>();
    let short = if chars.len() > 42 {
        format!("{}…", chars[..42].iter().collect::<String>())
    } else {
        value
    };
    (source_label, short, full)
}

#[cfg(test)]
mod ordering_tests {
    use super::egui;
    use super::{
        dashboard_panel_heights, dashboard_sidebar_inner_height, dashboard_uses_wide_layout,
        apply_model_priority_order, fit_dialog_size, friendly_error, move_list_item,
        oauth_account_error, oauth_terms_confirmation_ready, priority_endpoint_labels,
        remaining_quota_percent, shell_metrics, skip_guide_visible, subscription_provider_key,
        subscription_provider_title, topbar_control_size, usage_column_indices,
        usage_monitor_uses_two_columns, APP_VERSION, DIALOG_VIEWPORT_GUTTER, Page,
        SUBSCRIPTION_CARD_HEIGHT, SUBSCRIPTION_MODEL_LIST_HEIGHT, TERMS_EN, TERMS_ZH,
        TOPBAR_CONTROL_HEIGHT, TOPBAR_CONTROL_WIDTH,
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
    fn dashboard_model_list_uses_height_above_the_compact_log() {
        let compact = dashboard_panel_heights(480.0);
        let regular = dashboard_panel_heights(560.0);
        let tall = dashboard_panel_heights(720.0);

        assert_eq!(compact, (308.0, 262.0, 116.0));
        assert_eq!(regular, (388.0, 342.0, 116.0));
        assert_eq!(tall, (548.0, 502.0, 116.0));
    }

    #[test]
    fn short_windows_shrink_the_model_panel_so_the_log_stays_visible() {
        // On a short window the model panel must give way; otherwise the
        // activity log is pushed below the taskbar.
        for available_height in [300.0_f32, 360.0, 420.0] {
            let (model_panel, model_list, log) = dashboard_panel_heights(available_height);
            assert!(
                model_panel + log + 56.0 <= available_height + 0.5,
                "panels overflow at {available_height}: {model_panel} + {log}"
            );
            assert!(model_list <= model_panel);
        }
    }

    #[test]
    fn skip_guide_only_appears_after_a_finished_setup() {
        assert!(!skip_guide_visible(false, Page::Welcome));
        assert!(!skip_guide_visible(false, Page::Finish));
        assert!(!skip_guide_visible(true, Page::Dashboard));
        assert!(skip_guide_visible(true, Page::Welcome));
        assert!(skip_guide_visible(true, Page::Finish));
    }

    #[test]
    fn topbar_controls_share_stable_regular_and_compact_sizes() {
        assert_eq!(TOPBAR_CONTROL_WIDTH, 112.0);
        assert_eq!(TOPBAR_CONTROL_HEIGHT, 48.0);
        assert_eq!(topbar_control_size(1280.0, 820.0), egui::vec2(112.0, 48.0));
        assert_eq!(topbar_control_size(1064.0, 820.0), egui::vec2(92.0, 42.0));
        assert_eq!(topbar_control_size(944.0, 452.0), egui::vec2(92.0, 42.0));
    }

    #[test]
    fn subscription_cards_keep_stable_dimensions_and_provider_names() {
        assert_eq!(SUBSCRIPTION_CARD_HEIGHT, 430.0);
        assert_eq!(SUBSCRIPTION_MODEL_LIST_HEIGHT, 160.0);
        assert_eq!(subscription_provider_key("openai"), "chatgpt");
        assert_eq!(subscription_provider_key("x-ai"), "grok");
        assert_eq!(subscription_provider_title("antigravity"), "Antigravity");
    }

    #[test]
    fn priority_rows_identify_subscription_accounts_and_api_endpoints() {
        let account = super::OAuthAccountSummary {
            id: 24,
            name: "Grok OAuth".into(),
            platform: "grok".into(),
            status: "active".into(),
            email: "user@example.com".into(),
            plan: String::new(),
            priority: 1,
            bound_to_router: true,
            error: String::new(),
            expires_at: String::new(),
            models: Vec::new(),
            models_error: String::new(),
        };
        let oauth = super::ModelConfig {
            model: "grok-4.5".into(),
            source: "oauth".into(),
            oauth_platform: "grok".into(),
            oauth_account_id: 24,
            ..Default::default()
        };
        let (source, short, full) = priority_endpoint_labels(&oauth, &[account], true);
        assert!(source.contains("订阅"));
        assert!(source.contains("x-ai"));
        assert!(short.contains("user@example.com"));
        assert!(full.contains("账号 ID: #24"));

        let api = super::ModelConfig {
            model: "grok-4.5".into(),
            base_url: "https://api.example.com/a/very/long/provider/path/that/is/truncated".into(),
            ..Default::default()
        };
        let (source, short, full) = priority_endpoint_labels(&api, &[], false);
        assert!(source.starts_with("API ·"));
        assert!(short.ends_with('…'));
        assert!(full.contains("Base URL: https://api.example.com"));
    }

    #[test]
    fn priority_order_updates_models_priorities_and_route_policy() {
        let mut config = super::RouterConfig {
            models: vec![
                super::ModelConfig {
                    model: "grok-4.5".into(),
                    source: "oauth".into(),
                    oauth_account_id: 24,
                    priority: 10,
                    ..Default::default()
                },
                super::ModelConfig {
                    model: "other-model".into(),
                    priority: 70,
                    ..Default::default()
                },
                super::ModelConfig {
                    model: "grok-4.5".into(),
                    base_url: "https://api.example.com/v1".into(),
                    priority: 20,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        assert!(apply_model_priority_order(&mut config, "grok-4.5", &[2, 0]));
        assert_eq!(config.models[0].base_url, "https://api.example.com/v1");
        assert_eq!(config.models[0].priority, 10);
        assert_eq!(config.models[1].model, "other-model");
        assert_eq!(config.models[1].priority, 70);
        assert_eq!(config.models[2].source, "oauth");
        assert_eq!(config.models[2].priority, 20);
        assert_eq!(
            crate::logic::model_route_policy(&config, "grok-4.5"),
            crate::logic::ModelRoutePolicy::ApiFirst
        );
        assert!(config.oauth_fallback.enabled);
    }

    #[test]
    fn priority_order_rejects_partial_or_foreign_indices() {
        let mut config = super::RouterConfig {
            models: vec![
                super::ModelConfig {
                    model: "grok-4.5".into(),
                    source: "oauth".into(),
                    ..Default::default()
                },
                super::ModelConfig {
                    model: "grok-4.5".into(),
                    ..Default::default()
                },
                super::ModelConfig {
                    model: "other-model".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let original = config.models.clone();

        assert!(!apply_model_priority_order(&mut config, "grok-4.5", &[1]));
        assert!(!apply_model_priority_order(&mut config, "grok-4.5", &[1, 2]));
        assert_eq!(config.models[0].source, original[0].source);
        assert_eq!(config.models[1].source, original[1].source);
    }

    #[test]
    fn priority_drag_source_carries_payload_over_target() {
        fn render(
            context: &egui::Context,
            input: egui::RawInput,
        ) -> (egui::Rect, egui::Rect, Option<usize>) {
            let mut result = None;
            let output = context.run_ui(input, |ui| {
                let source = ui.dnd_drag_source(
                    egui::Id::new("priority-drag-test-source"),
                    super::ModelOrderDrag { source_index: 7 },
                    |ui| ui.allocate_response(egui::vec2(80.0, 32.0), egui::Sense::hover()),
                );
                ui.add_space(40.0);
                let target = ui.allocate_response(egui::vec2(180.0, 48.0), egui::Sense::hover());
                let payload = target
                    .dnd_hover_payload::<super::ModelOrderDrag>()
                    .map(|payload| payload.source_index);
                result = Some((source.response.rect, target.rect, payload));
            });
            drop(output);
            result.expect("drag test should render")
        }

        let context = egui::Context::default();
        let (source, target, _) = render(&context, egui::RawInput::default());
        let source_pos = source.center();
        let target_pos = target.center();
        let pressed = egui::RawInput {
            events: vec![
                egui::Event::PointerMoved(source_pos),
                egui::Event::PointerButton {
                    pos: source_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };
        render(&context, pressed);
        let moved = egui::RawInput {
            events: vec![egui::Event::PointerMoved(target_pos)],
            ..Default::default()
        };
        let (_, _, payload) = render(&context, moved);

        assert_eq!(payload, Some(7));
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
        assert!(dashboard_uses_wide_layout(860.0, 400.0));
        assert!(dashboard_uses_wide_layout(980.0, 360.0));
        assert!(!dashboard_uses_wide_layout(800.0, 620.0));
        assert!(usage_monitor_uses_two_columns(960.0));
        assert!(!usage_monitor_uses_two_columns(800.0));
    }

    #[test]
    fn usage_cards_stack_independently_in_two_columns() {
        assert_eq!(usage_column_indices(5, 2), vec![vec![0, 2, 4], vec![1, 3]]);
        assert_eq!(usage_column_indices(3, 1), vec![vec![0, 1, 2]]);
    }

    #[test]
    fn balance_windows_keep_provider_amounts() {
        let window: super::UsageWindow = serde_json::from_str(
            r#"{"kind":"balance","displayName":"Balance","remainingAmount":9.5,"limitAmount":20.0,"usedAmount":10.5,"currency":"USD"}"#,
        )
        .expect("balance window should deserialize");
        assert_eq!(window.remaining_amount, Some(9.5));
        assert_eq!(window.limit_amount, Some(20.0));
        assert_eq!(window.used_amount, Some(10.5));
        assert_eq!(window.currency, "USD");
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
    fn same_coding_plan_accounts_share_one_group_key() {
        let grok_a = super::UsageAccount {
            id: 1,
            kind: "oauth".into(),
            platform: "grok".into(),
            name: "Grok One".into(),
            ..Default::default()
        };
        let grok_b = super::UsageAccount {
            id: 2,
            kind: "oauth".into(),
            platform: "grok".into(),
            name: "Grok Two".into(),
            ..Default::default()
        };
        let kimi = super::UsageAccount {
            id: 3,
            kind: "apikey".into(),
            platform: "kimi".into(),
            windows: vec![super::UsageWindow {
                kind: "weekly".into(),
                used_percent: Some(10.0),
                ..Default::default()
            }],
            ..Default::default()
        };
        let volcengine = super::UsageAccount {
            id: 4,
            kind: "apikey".into(),
            platform: "Volcengine Coding Plan".into(),
            windows: vec![super::UsageWindow {
                kind: "weekly".into(),
                used_percent: Some(20.0),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            super::CodexRouterApp::usage_plan_group_key(&grok_a),
            super::CodexRouterApp::usage_plan_group_key(&grok_b)
        );
        let groups = super::CodexRouterApp::group_usage_accounts(&[&grok_a, &kimi, &grok_b]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0][0].id, 1);
        assert_eq!(groups[0][1].id, 2);
        assert_eq!(groups[1][0].id, 3);
        assert_ne!(
            super::CodexRouterApp::usage_plan_group_key(&kimi),
            super::CodexRouterApp::usage_plan_group_key(&volcengine)
        );
    }

    #[test]
    fn model_row_usage_matches_oauth_account_id_then_provider() {
        let snapshot = super::UsageSnapshot {
            subscriptions: vec![super::UsageAccount {
                id: 24,
                kind: "oauth".into(),
                platform: "grok".into(),
                name: "Grok A".into(),
                ..Default::default()
            }],
            api_channels: vec![super::UsageAccount {
                id: 90,
                kind: "apikey".into(),
                platform: "kimi".into(),
                name: "Codex-Router / Kimi".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let grok = super::ModelConfig {
            source: "oauth".into(),
            model: "grok-4.6".into(),
            oauth_account_id: 24,
            oauth_platform: "grok".into(),
            ..Default::default()
        };
        let kimi = super::ModelConfig {
            source: "apikey".into(),
            model: "kimi-for-coding".into(),
            base_url: "https://api.kimi.com/coding/v1".into(),
            ..Default::default()
        };
        assert_eq!(
            super::CodexRouterApp::usage_account_for_model(&snapshot, &grok)
                .map(|account| account.id),
            Some(24)
        );
        assert_eq!(
            super::CodexRouterApp::usage_account_for_model(&snapshot, &kimi)
                .map(|account| account.id),
            Some(90)
        );
        let chiral = super::ModelConfig {
            source: "apikey".into(),
            model: "gpt-5.6-sol".into(),
            alias: "ChatGPT-5.6-Sol".into(),
            base_url: "https://api.430123.xyz/v1".into(),
            ..Default::default()
        };
        let snapshot = super::UsageSnapshot {
            api_channels: vec![super::UsageAccount {
                id: 12,
                kind: "apikey".into(),
                platform: "chiral".into(),
                name: "Codex-Router / ChatGPT-5.6-Sol".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            super::CodexRouterApp::usage_account_for_model(&snapshot, &chiral)
                .map(|account| account.id),
            Some(12)
        );
    }

    #[test]
    fn model_row_usage_prefers_subscription_quota_then_api_tokens() {
        let grok_oauth = super::ModelConfig {
            source: "oauth".into(),
            model: "grok-4.5".into(),
            oauth_account_id: 24,
            oauth_platform: "grok".into(),
            ..Default::default()
        };
        let grok_api = super::ModelConfig {
            source: "apikey".into(),
            model: "x-ai/grok-4.5".into(),
            ..Default::default()
        };
        let snapshot = super::UsageSnapshot {
            subscriptions: vec![super::UsageAccount {
                id: 24,
                kind: "oauth".into(),
                platform: "grok".into(),
                name: "Grok A".into(),
                windows: vec![super::UsageWindow {
                    kind: "weekly".into(),
                    used_percent: Some(20.0),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            api_channels: vec![super::UsageAccount {
                id: 91,
                kind: "apikey".into(),
                platform: "xai".into(),
                name: "Codex-Router / Grok 4.5".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut cfg = super::RouterConfig {
            models: vec![grok_oauth.clone(), grok_api.clone()],
            ..Default::default()
        };
        let (account, quota) = super::CodexRouterApp::usage_for_model_row(
            &snapshot,
            &cfg,
            &cfg.models,
            &grok_oauth,
        )
        .expect("subscription usage");
        assert_eq!(account.id, 24);
        assert!(quota);
        crate::logic::set_model_route_policy(
            &mut cfg,
            "grok-4.5",
            crate::logic::ModelRoutePolicy::ApiFirst,
        );
        let (account, quota) = super::CodexRouterApp::usage_for_model_row(
            &snapshot,
            &cfg,
            &cfg.models,
            &grok_oauth,
        )
        .expect("api usage");
        assert_eq!(account.id, 91);
        assert!(!quota);
    }

    #[test]
    fn coding_plan_model_row_uses_smallest_quota_window() {
        let kimi = super::ModelConfig {
            source: "apikey".into(),
            model: "kimi-for-coding".into(),
            base_url: "https://api.kimi.com/coding/v1".into(),
            ..Default::default()
        };
        let snapshot = super::UsageSnapshot {
            api_channels: vec![super::UsageAccount {
                id: 90,
                kind: "apikey".into(),
                platform: "kimi".into(),
                name: "Codex-Router / Kimi".into(),
                windows: vec![
                    super::UsageWindow {
                        kind: "weekly".into(),
                        used_percent: Some(40.0),
                        ..Default::default()
                    },
                    super::UsageWindow {
                        kind: "fiveHour".into(),
                        used_percent: Some(10.0),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let cfg = super::RouterConfig {
            models: vec![kimi.clone()],
            ..Default::default()
        };
        let (account, quota) = super::CodexRouterApp::usage_for_model_row(
            &snapshot, &cfg, &cfg.models, &kimi,
        )
        .expect("coding plan usage");
        assert_eq!(account.id, 90);
        assert!(quota);
        assert_eq!(
            super::CodexRouterApp::smallest_readable_quota_window(account)
                .map(|window| window.kind.as_str()),
            Some("fiveHour")
        );
    }

    #[test]
    fn dashboard_sidebar_bottom_matches_log_bottom() {
        let layout = 560.0;
        let (model_content, _, log_content) = dashboard_panel_heights(layout);
        let right_outer = model_content + 24.0 + 8.0 + log_content + 20.0;
        let left_outer = dashboard_sidebar_inner_height(layout) + 28.0;
        assert!((left_outer - right_outer).abs() < 0.5);
        assert!(left_outer <= layout);
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
    fn oauth_account_errors_are_actionable_instead_of_raw_classes() {
        let request = oauth_account_error("class=request_failure", true);
        assert!(request.contains("上游探测暂时失败"));
        assert!(!request.contains("本地服务"));
        assert!(!request.contains("class="));

        let authentication = oauth_account_error("class=authentication", true);
        assert!(authentication.contains("重新授权"));
        assert!(!authentication.contains("管理会话"));
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

fn apply_model_priority_order(
    config: &mut RouterConfig,
    route_id: &str,
    order: &[usize],
) -> bool {
    let mut slots = config
        .models
        .iter()
        .enumerate()
        .filter(|(_, model)| super::logic::same_model_identity(&model.model, route_id))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    slots.sort_unstable();
    let mut ordered_indices = order.to_vec();
    ordered_indices.sort_unstable();
    if slots.len() < 2 || slots != ordered_indices {
        return false;
    }

    let old_models = config.models.clone();
    for (position, (slot, source)) in slots.iter().zip(order.iter()).enumerate() {
        let mut model = old_models[*source].clone();
        model.priority = (position as i32 + 1) * 10;
        config.models[*slot] = model;
    }
    let policy = if config.models[slots[0]].source == "oauth" {
        super::logic::ModelRoutePolicy::SubscriptionFirst
    } else {
        super::logic::ModelRoutePolicy::ApiFirst
    };
    super::logic::set_model_route_policy(config, route_id, policy);
    config.oauth_fallback.enabled = true;
    true
}

fn skip_guide_visible(configured: bool, page: Page) -> bool {
    configured && page.is_setup_wizard()
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

fn oauth_account_error(raw: &str, zh: bool) -> String {
    let normalized = raw.to_ascii_lowercase();
    if normalized.contains("authentication")
        || normalized.contains("unauthenticated")
        || normalized.contains("status=401")
        || normalized.contains("invalid_grant")
    {
        t(
            zh,
            "OAuth 凭据已失效。请撤销此账号后重新授权。",
            "The OAuth credential has expired. Revoke this account, then authorize it again.",
        )
        .to_owned()
    } else if normalized.contains("permission")
        || normalized.contains("forbidden")
        || normalized.contains("status=403")
    {
        t(
            zh,
            "上游拒绝了此账号的访问。请完成账号验证或重新授权后再刷新。",
            "The provider denied this account. Complete account verification or authorize it again, then refresh.",
        )
        .to_owned()
    } else if normalized.contains("rate_limit") || normalized.contains("status=429") {
        t(
            zh,
            "此账号当前受额度或频率限制。到重置时间后点击「刷新」。",
            "This account is currently quota or rate limited. Click Refresh after its reset time.",
        )
        .to_owned()
    } else if normalized.contains("class=request_failure")
        || normalized.contains("class=unclassified_error")
    {
        t(
            zh,
            "该账号的上游探测暂时失败。自检系统会自动重试，也可点击「刷新」。",
            "This account's upstream probe temporarily failed. Self-check will retry automatically; you can also click Refresh.",
        )
        .to_owned()
    } else {
        friendly_error(raw, zh)
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
        Page::OAuth => t(zh, "订阅账号", "SUBSCRIPTIONS"),
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

fn dashboard_sidebar_inner_height(layout_height: f32) -> f32 {
    let (model_content, _, log_content) = dashboard_panel_heights(layout_height);
    (model_content + 24.0 + 8.0 + log_content + 20.0 - 28.0).max(0.0)
}

fn dashboard_panel_heights(layout_height: f32) -> (f32, f32, f32) {
    const LOG_CONTENT_HEIGHT: f32 = 116.0;
    const PANEL_GAP: f32 = 8.0;
    const BOTTOM_GAP: f32 = 4.0;
    const MODEL_FRAME_VERTICAL_MARGIN: f32 = 24.0;
    const LOG_FRAME_VERTICAL_MARGIN: f32 = 20.0;
    const MODEL_HEADER_HEIGHT: f32 = 46.0;
    // `model_content_height` and `log_content_height` are inner allocations.
    // Account for both custom frame margins here so the activity log cannot be
    // pushed beneath the footer on scaled Windows displays.
    let reserved = LOG_CONTENT_HEIGHT
        + PANEL_GAP
        + BOTTOM_GAP
        + MODEL_FRAME_VERTICAL_MARGIN
        + LOG_FRAME_VERTICAL_MARGIN;
    let model_content_height = (layout_height - reserved).max(0.0);
    let model_list_height = (model_content_height - MODEL_HEADER_HEIGHT).max(0.0);
    (model_content_height, model_list_height, LOG_CONTENT_HEIGHT)
}

impl eframe::App for CodexRouterApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        super::clamp_window_to_current_monitor(ctx);
        if self.ui_audit_mode {
            let screenshot = ctx.input(|input| {
                input.events.iter().find_map(|event| match event {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let (Some(path), Some(image)) = (self.ui_audit_screenshot_path.as_ref(), screenshot)
            {
                let result = (|| -> anyhow::Result<()> {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let rgba = image
                        .pixels
                        .iter()
                        .flat_map(|pixel| pixel.to_array())
                        .collect::<Vec<_>>();
                    image::save_buffer(
                        path,
                        &rgba,
                        image.size[0] as u32,
                        image.size[1] as u32,
                        image::ColorType::Rgba8,
                    )?;
                    Ok(())
                })();
                match result {
                    Ok(()) => eprintln!("UI_AUDIT_SCREENSHOT={}", path.display()),
                    Err(error) => eprintln!("UI_AUDIT_ERROR={error:#}"),
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        self.enforce_background_start_hidden(ctx);
        self.process_app_events(ctx);
        if !self.exit_shutdown_in_progress {
            if self.runtime_probes_allowed() {
                self.process_router_health_protection(ctx);
                self.process_scheduled_usage_refresh(ctx);
                self.process_scheduled_oauth_recovery(ctx);
            } else {
                self.health_probe_due = None;
                self.health_probe_failures = 0;
            }
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
            || self.codex_overwrite_prompt_open
            || self.oauth_post_login_prompt_open
            || self.profile_delete_target.is_some()
            || self.oauth_priority_target.is_some()
            || self.model_route_policy_target.is_some()
            || self.channel_preset_dialog_open
            || self.recommended_platform_dialog_open
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
        let maximized = ctx.input(|input| {
            input.viewport().maximized == Some(true) || input.viewport().fullscreen == Some(true)
        });
        let compact_layout = !maximized && viewport_height < 700.0;
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
                    .fill(palette.background_dark)
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
        if self.oauth_post_login_prompt_open {
            self.show_oauth_post_login_prompt(&ctx, &palette);
        }
        if self.codex_overwrite_prompt_open {
            self.show_codex_overwrite_dialog(&ctx, &palette);
        }
        if self.update_dialog_open {
            self.show_update_dialog(&ctx, &palette);
        }
        if self.profile_create_open {
            self.show_profile_create_dialog(&ctx, &palette);
        }
        if self.profile_delete_target.is_some() {
            self.show_profile_delete_dialog(&ctx, &palette);
        }
        if self.oauth_revoke_target.is_some() {
            self.show_oauth_revoke_dialog(&ctx, &palette);
        }
        if self.oauth_priority_target.is_some() {
            self.show_oauth_priority_dialog(&ctx, &palette);
        }
        if self.oauth_fallback_picker_target.is_some() {
            self.show_oauth_fallback_picker_dialog(&ctx, &palette);
        }
        if self.model_route_policy_target.is_some() {
            self.show_model_route_policy_dialog(&ctx, &palette);
        }
        if self.model_priority_dialog_target.is_some() {
            self.show_model_priority_dialog(&ctx, &palette);
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
        if self.provider_oauth_prompt.is_some() {
            self.show_provider_oauth_prompt(&ctx, &palette);
        }
        if self.terms_open {
            self.show_terms_modal(&ctx, &palette);
        }
        if self.log_dialog_open {
            self.show_log_dialog(&ctx, &palette);
        }
        if self.ui_audit_screenshot_path.is_some() && !self.ui_audit_screenshot_requested {
            self.ui_audit_frame_count = self.ui_audit_frame_count.saturating_add(1);
            if self.ui_audit_frame_count >= 3 {
                self.ui_audit_screenshot_requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            } else {
                ctx.request_repaint();
            }
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
                    .color(palette.action),
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
        let left = egui::Rect::from_min_max(rect.min, egui::pos2(rect.center().x, rect.max.y));
        let right = egui::Rect::from_min_max(egui::pos2(rect.center().x, rect.min.y), rect.max);
        let active = if zh { left } else { right };
        ui.painter().rect_filled(
            active.shrink(3.0),
            egui::CornerRadius::same(7),
            palette.action,
        );
        ui.painter().line_segment(
            [
                egui::pos2(rect.center().x, rect.top() + 9.0),
                egui::pos2(rect.center().x, rect.bottom() - 9.0),
            ],
            egui::Stroke::new(1.0, palette.line),
        );
        let font = egui::FontId::new(13.0, egui::FontFamily::Proportional);
        ui.painter().text(
            left.center(),
            egui::Align2::CENTER_CENTER,
            "中",
            font.clone(),
            if zh {
                egui::Color32::WHITE
            } else {
                palette.ink_soft
            },
        );
        ui.painter().text(
            right.center(),
            egui::Align2::CENTER_CENTER,
            "EN",
            font,
            if zh {
                palette.ink_soft
            } else {
                egui::Color32::WHITE
            },
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
                .color(palette.action),
            )
            .fill(palette.paper)
            .stroke(egui::Stroke::new(1.0, palette.line))
            .corner_radius(egui::CornerRadius::same(10)),
        )
    }

    fn show_oauth_post_login_prompt(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let mut open_oauth = false;
        let mut dismissed = false;
        let mut header_closed = false;
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(520.0, 340.0),
            egui::vec2(400.0, 280.0),
        );
        egui::Window::new("")
            .id(egui::Id::new("oauth-post-login-prompt"))
            .title_bar(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .frame(theme::dialog_window_frame())
            .default_size(dialog_size)
            .min_size(dialog_size)
            .max_size(dialog_size)
            .show(ctx, |ui| {
                ui.set_width(dialog_size.x);
                theme::dialog_shell(ui, palette, |ui| {
                    ui.horizontal(|ui| {
                        theme::dialog_title(ui, t(zh, "订阅登录成功", "Subscription signed in"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                            egui::RichText::new("×")
                                                .size(18.0)
                                                .color(egui::Color32::WHITE),
                                    )
                                    .fill(egui::Color32::TRANSPARENT)
                                    .stroke(egui::Stroke::NONE),
                                )
                                .on_hover_text(t(zh, "关闭", "Close"))
                                .clicked()
                            {
                                header_closed = true;
                            }
                        });
                    });
                }, |ui| {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(t(zh, "登录成功", "Signed in"))
                            .font(egui::FontId::new(28.0, theme::display_family()))
                            .color(palette.ink),
                    );
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new(t(
                            zh,
                            "请在本页选择要使用的模型，并加入当前配置的模型列表。",
                            "Choose the models you want on this page and add them to the active profile list.",
                        ))
                        .size(15.5)
                        .strong()
                        .color(palette.ink),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(t(
                            zh,
                            "仅完成账号认证不会改写路由。加入模型并保存应用后，才会按 OAuth 优先、第三方兜底生效；若暂不加入，将继续使用模型列表中的第三方渠道。",
                            "Signing in alone does not change routing. After you add models and Save & apply, OAuth is preferred with third-party fallback. Skip for now and existing API channels keep serving.",
                        ))
                        .color(palette.ink_soft),
                    );
                    ui.add_space(20.0);
                    ui.horizontal(|ui| {
                        if theme::primary_button(
                            ui,
                            egui::RichText::new(t(zh, "去选择模型", "Choose models"))
                                .strong()
                                .color(egui::Color32::WHITE),
                            palette,
                        )
                        .clicked()
                        {
                            open_oauth = true;
                        }
                        ui.add_space(8.0);
                        if theme::secondary_button(ui, t(zh, "知道了", "Got it"), palette).clicked()
                        {
                            dismissed = true;
                        }
                    });
                });
            });
        dismissed |= header_closed;
        if open_oauth || dismissed {
            self.oauth_post_login_prompt_open = false;
            if !self.oauth_model_hint_seen {
                self.oauth_model_hint_seen = true;
                let _ = self.persist_ui_preferences();
            }
            if open_oauth {
                self.open_oauth_manager();
            }
        }
    }

    fn show_codex_overwrite_dialog(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let running = self.codex_overwrite_action_running || self.applying;
        let mut apply_router = false;
        let mut keep_current = false;
        let mut restore_factory = false;
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(520.0, 320.0),
            egui::vec2(400.0, 260.0),
        );
        egui::Window::new("")
            .id(egui::Id::new("codex-overwrite-dialog"))
            .title_bar(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .frame(theme::dialog_window_frame())
            .default_size(dialog_size)
            .min_size(dialog_size)
            .max_size(dialog_size)
            .show(ctx, |ui| {
                ui.set_width(dialog_size.x);
                theme::dialog_shell(ui, palette, |ui| {
                    theme::dialog_title(ui, t(zh, "Codex 路由绑定", "Codex route binding"));
                }, |ui| {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(t(zh, "路由绑定已变化", "Router binding changed"))
                            .font(egui::FontId::new(26.0, theme::display_family()))
                            .color(palette.ink),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(t(
                            zh,
                            "Codex 不再指向当前本地 Router。可写回现有设置、保持现状，或恢复官方默认。",
                            "Codex is no longer pointed at this local Router. Write back current settings, keep the file, or restore official defaults.",
                        ))
                        .color(palette.ink_soft),
                    );
                    if running {
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(t(zh, "正在处理，请稍候…", "Working, please wait…"))
                                .color(palette.muted),
                        );
                    }
                    ui.add_space(18.0);
                    ui.add_enabled_ui(!running, |ui| {
                        ui.horizontal(|ui| {
                            if theme::primary_button(
                                ui,
                                egui::RichText::new(t(zh, "写回设置", "Write settings"))
                                    .strong()
                                    .color(egui::Color32::WHITE),
                                palette,
                            )
                            .on_hover_text(t(
                                zh,
                                "写入当前 CodexRouter 配置并重启 Codex",
                                "Write the current CodexRouter config and restart Codex",
                            ))
                            .clicked()
                            {
                                apply_router = true;
                            }
                            ui.add_space(8.0);
                            if theme::secondary_button(ui, t(zh, "保持现状", "Keep current"), palette)
                                .clicked()
                            {
                                keep_current = true;
                            }
                            ui.add_space(8.0);
                            if theme::secondary_button(ui, t(zh, "恢复默认", "Restore defaults"), palette)
                                .clicked()
                            {
                                restore_factory = true;
                            }
                                                });
                                            });
                                        });
            });
        if apply_router {
            self.codex_overwrite_apply_router_config();
        }
        if keep_current {
            self.codex_overwrite_keep_current();
        }
        if restore_factory {
            self.codex_overwrite_restore_factory();
        }
    }

    fn show_apply_success_dialog(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let compact = ctx.content_rect().height() < 500.0;
        let mut acknowledged = false;
        let subscription = self.apply_success_is_subscription;
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(640.0, if compact { 400.0 } else { 420.0 }),
            egui::vec2(480.0, 360.0),
        );
        egui::Window::new("")
            .id(egui::Id::new("apply-success-dialog"))
            .title_bar(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .frame(theme::dialog_window_frame())
            .default_size(dialog_size)
            .min_size(dialog_size)
            .max_size(dialog_size)
            .show(ctx, |ui| {
                ui.set_width(dialog_size.x);
                theme::dialog_shell(
                    ui,
                    palette,
                    |ui| {
                        theme::dialog_title(
                            ui,
                            if subscription {
                                t(zh, "订阅配置成功", "Subscription configured")
                            } else {
                                t(zh, "配置已应用", "Configuration applied")
                            },
                        );
                        ui.label(
                            egui::RichText::new(t(
                                zh,
                                if subscription {
                                    "首个订阅模型已启用，Router 路由已写入 Codex。"
                                } else {
                                    "Router 已接管本机 Codex 路由。"
                                },
                                if subscription {
                                    "The first subscription model is enabled and the Router route is written to Codex."
                                } else {
                                    "Router now owns the local Codex route."
                                },
                            ))
                            .color(egui::Color32::from_white_alpha(215)),
                        );
                    },
                    |ui| {
                        ui.label(
                            egui::RichText::new(t(
                                zh,
                                "建议流程：完全退出并重新启动 ChatGPT / Codex",
                                "Recommended: fully quit and restart ChatGPT / Codex",
                            ))
                            .size(if compact { 16.0 } else { 18.0 })
                            .strong()
                            .color(palette.ink),
                        );
                        ui.add_space(if compact { 6.0 } else { 10.0 });
                        ui.label(
                            egui::RichText::new(t(
                                zh,
                                "请完全退出并重新启动 ChatGPT / Codex。应用后会继续使用原有 ChatGPT 登录；若尚未登录，请先按 Codex 官方流程登录。模型列表来自本机路由目录。",
                                "Fully quit and restart ChatGPT / Codex. Applying keeps the existing ChatGPT sign-in; if you are signed out, use the official Codex sign-in flow first. Models come from the local Router catalog.",
                            ))
                            .color(palette.ink_soft),
                        );
                        ui.add_space(if compact { 4.0 } else { 8.0 });
                        ui.label(
                            egui::RichText::new(t(
                                zh,
                                "使用过程中请保持 Codex-Router 在后台或系统托盘运行，否则本地转发将不可用。",
                                "Keep Codex-Router running in the background or system tray while using it, otherwise local forwarding will be unavailable.",
                            ))
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
                        ui.add_space(18.0);
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
                    },
                );
            });
        if acknowledged {
            self.apply_success_dialog_open = false;
            self.apply_success_is_subscription = false;
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
        egui::Window::new("")
            .id(egui::Id::new("runtime-log-dialog"))
            .title_bar(false)
            .default_size(default_size)
            .min_size(minimum_size)
            .max_size(maximum_size)
            .resizable(true)
            .collapsible(false)
            .open(&mut open)
            .frame(theme::dialog_window_frame())
            .show(ctx, |ui| {
                ui.set_min_width(minimum_size.x);
                theme::dialog_shell(
                    ui,
                    palette,
                    |ui| theme::dialog_title(ui, t(zh, "运行日志", "Runtime log")),
                    |ui| {
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
                    },
                );
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
            egui::vec2(640.0, 480.0),
            egui::vec2(480.0, 360.0),
        );
        egui::Window::new("")
            .id(egui::Id::new("close-behavior-prompt"))
            .title_bar(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size(dialog_size)
            .collapsible(false)
            .resizable(false)
            .open(&mut window_open)
            .frame(theme::dialog_window_frame())
            .show(ctx, |ui| {
                ui.set_width(dialog_size.x);
                theme::dialog_shell(
                    ui,
                    palette,
                    |ui| theme::dialog_title(ui, t(zh, "关闭 Codex-Router", "Close Codex-Router")),
                    |ui| {
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
                    },
                );
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
        let compact_chrome =
            topbar_control_size(viewport_width, viewport.y).x < TOPBAR_CONTROL_WIDTH;
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
            if let Some(page) =
                Self::tutorial_progress(ui, step_number(self.page), zh, viewport_width, palette)
            {
                self.page = page;
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let show_skip_guide = skip_guide_visible(self.configured, self.page);
                if show_skip_guide
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
                    .on_hover_text(t(
                        zh,
                        "已有配置，返回控制台",
                        "Return to the console; this profile is already configured",
                    ))
                    .clicked()
                {
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

    fn tutorial_progress(
        ui: &mut egui::Ui,
        current: usize,
        zh: bool,
        viewport_width: f32,
        palette: &theme::Palette,
    ) -> Option<Page> {
        let width = if viewport_width < 900.0 {
            176.0
        } else if viewport_width < 1040.0 {
            236.0
        } else {
            260.0
        };
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, 58.0), egui::Sense::click());
        let painter = ui.painter_at(rect);
        painter.rect(
            rect,
            egui::CornerRadius::same(10),
            palette.paper,
            egui::Stroke::new(1.0, palette.line),
            egui::StrokeKind::Inside,
        );
        painter.text(
            egui::pos2(rect.center().x, rect.top() + 6.0),
            egui::Align2::CENTER_TOP,
            t(zh, "新手教学配置", "QUICK START SETUP"),
            egui::FontId::new(10.5, theme::display_family()),
            palette.ink,
        );
        let stages = if zh {
            ["项目", "登录", "首个模型", "网络代理", "完成"]
        } else {
            ["Project", "Access", "1st model", "Proxy", "Finish"]
        };
        let pages = [
            Page::Project,
            Page::Auth,
            Page::Model,
            Page::Proxy,
            Page::Finish,
        ];
        let track_left = rect.left() + 18.0;
        let track_right = rect.right() - 18.0;
        let track_y = rect.top() + 26.0;
        painter.line_segment(
            [
                egui::pos2(track_left, track_y),
                egui::pos2(track_right, track_y),
            ],
            egui::Stroke::new(2.0, palette.line),
        );
        let complete_all = current > 5;
        for (index, label) in stages.iter().enumerate() {
            let progress = index as f32 / 4.0;
            let x = egui::lerp(track_left..=track_right, progress);
            let complete = complete_all || current > index + 1;
            let active = current == index + 1;
            let dot_color = if complete || active {
                palette.action
            } else {
                palette.paper_alt
            };
            painter.circle_filled(
                egui::pos2(x, track_y),
                if active { 6.0 } else { 5.0 },
                dot_color,
            );
            painter.circle_stroke(
                egui::pos2(x, track_y),
                if active { 6.0 } else { 5.0 },
                egui::Stroke::new(
                    1.0,
                    if complete || active {
                        palette.action
                    } else {
                        palette.line
                    },
                ),
            );
            painter.text(
                egui::pos2(x, rect.bottom() - 8.0),
                egui::Align2::CENTER_BOTTOM,
                label,
                egui::FontId::new(if zh { 9.5 } else { 8.5 }, egui::FontFamily::Proportional),
                if complete || active {
                    palette.ink
                } else {
                    palette.ink_soft
                },
            );
        }
        response.clone().on_hover_text(t(
            zh,
            "点击已完成的阶段返回对应设置",
            "Open any completed setup stage",
        ));
        if response.clicked() {
            let progress = ((response.interact_pointer_pos()?.x - track_left)
                / (track_right - track_left))
                .clamp(0.0, 1.0);
            let index = (progress * 4.0).round() as usize;
            if complete_all || current > index {
                return Some(pages[index]);
            }
        }
        None
    }

    fn show_update_dialog(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let info = self.update_info.clone().unwrap_or_default();
        let mut open = true;
        let mut close = false;
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(680.0, 560.0),
            egui::vec2(500.0, 380.0),
        );
        egui::Window::new("")
            .id(egui::Id::new("github-update-dialog"))
            .title_bar(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size(dialog_size)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .frame(theme::dialog_window_frame())
            .show(ctx, |ui| {
                ui.set_width(dialog_size.x);
                theme::dialog_shell(
                    ui,
                    palette,
                    |ui| theme::dialog_title(ui, t(zh, "Codex-Router 更新", "Codex-Router update")),
                    |ui| {
                let (title, detail) = match info.status.as_str() {
                    "update_available" => (
                        format!(
                            "{} {}",
                            t(zh, "发现新版本", "New version available"),
                            info.latest_version
                        ),
                        t(
                            zh,
                            "从官方 GitHub Release 下载并校验后，程序会安全退出、自动覆盖当前版本并重新启动。",
                            "After the official GitHub Release is downloaded and verified, the app will exit safely, replace the current version, and restart automatically.",
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
                    "ready_to_install" => (
                        t(zh, "更新包校验完成", "Update verified").to_owned(),
                        t(
                            zh,
                            "正在安全退出本地服务；退出后会自动完成替换并重新启动。",
                            "Local services are shutting down safely; replacement and restart will continue automatically after exit.",
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
                if self.update_downloading {
                    ui.add_space(10.0);
                    let total = self.update_total_bytes.max(info.asset_size);
                    let fraction = if total == 0 {
                        0.0
                    } else {
                        (self.update_downloaded_bytes as f32 / total as f32).clamp(0.0, 1.0)
                    };
                    let downloaded_mib = self.update_downloaded_bytes as f64 / 1_048_576.0;
                    let total_mib = total as f64 / 1_048_576.0;
                    ui.add(
                        egui::ProgressBar::new(fraction)
                            .desired_width(ui.available_width())
                            .show_percentage()
                            .text(format!("{downloaded_mib:.1} / {total_mib:.1} MiB")),
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
                            self.download_update(&info, ctx);
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
                    },
                );
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
                    let valid = RouterConfig::is_router_root(&this.router_root);
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
                                    t(zh, "目录缺少完整的本机运行组件", "The native runtime is incomplete")
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
                            t(zh, "可用订阅登录", "AVAILABLE PLAN SIGN-INS"),
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
                                ui.label(t(zh, "正在等待订阅授权…", "Waiting for subscription…"));
                            });
                            if theme::secondary_button(
                                &mut columns[1],
                                t(zh, "取消授权", "Cancel sign-in"),
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
                            "Fall back to an API model when the subscription is unavailable",
                        ),
                    );
                    if this.config.oauth_fallback.enabled {
                        ui.columns(2, |columns| {
                            theme::field_label(
                                &mut columns[0],
                                t(zh, "订阅优先级", "PLAN PRIORITY"),
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
                    let has_first_model = !this.config.models.is_empty();
                    let (back, next) = Self::navigation_row(
                        ui,
                        t(zh, "← 项目目录", "← Project"),
                        if has_first_model {
                            t(zh, "网络代理 →", "Network proxy →")
                        } else {
                            t(zh, "配置第一个模型 →", "Configure first model →")
                        },
                        true,
                        palette,
                    );
                    if back {
                        this.page = Page::Project;
                    }
                    if next {
                        if has_first_model {
                            this.proxy_from_wizard = true;
                            this.page = Page::Proxy;
                        } else {
                            this.temp_model = ModelConfig::default();
                            this.temp_model.priority =
                                super::logic::next_api_channel_priority(&this.config);
                            this.editing_model = None;
                            this.model_from_wizard = true;
                            this.advanced_json_open = false;
                            this.page = Page::Model;
                        }
                    }
                });
            },
        );
    }

    pub(crate) fn commit_model_draft(
        &mut self,
        mut model: ModelConfig,
        editing_model: Option<usize>,
        model_from_wizard: bool,
    ) {
        let zh = self.ui_language == "zh";
        if model.source == "oauth" {
            model.user_selected = true;
        }
        let key_update_pending = !model.api_key.trim().is_empty();
        if model_from_wizard {
            self.config.models = vec![model];
            super::logic::normalize_default_model(&mut self.config);
            self.proxy_from_wizard = true;
            self.page = Page::Proxy;
            return;
        }
        match editing_model {
            Some(index) if index < self.config.models.len() => {
                let was_default = self.config.default_model == self.config.models[index].model;
                self.config.models[index] = model.clone();
                if was_default {
                    self.config.default_model = model.model.clone();
                }
            }
            Some(_) => {
                self.status_text = t(
                    zh,
                    "模型列表已变化，请重新打开模型后再保存",
                    "The model list changed. Reopen the model before saving.",
                )
                .to_owned();
                return;
            }
            None => self.config.models.push(model),
        }
        super::logic::normalize_default_model(&mut self.config);
        if key_update_pending {
            self.status_text = t(
                zh,
                "API 渠道已验证并暂存；点击“保存并应用”后将安全写入 Windows 凭据",
                "The API channel was verified and staged. Save & apply to store the key securely.",
            )
            .to_owned();
        }
        self.page = Page::Dashboard;
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
                            let volcengine_coding_plan =
                                super::logic::is_volcengine_plan_url(&this.temp_model.base_url);
                            let valid = !this.api_model_validation_running
                                && !this.temp_model.model.trim().is_empty()
                                && json_valid
                                && (oauth_model
                                    || (!this.temp_model.base_url.trim().is_empty()
                                        && (!this.temp_model.api_key.trim().is_empty()
                                            || !this.temp_model.credential_name.is_empty())));
                            let mut back = false;
                            let mut next = false;
                            let mut open_oauth_models = false;
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
                                        let key_update_pending =
                                            !this.temp_model.api_key.trim().is_empty();
                                        let next_label = if this.api_model_validation_running {
                                            t(zh, "正在测试连接…", "Testing connection…")
                                        } else if this.model_from_wizard {
                                            t(zh, "网络代理 →", "Network proxy →")
                                        } else if key_update_pending {
                                            t(zh, "保存新 Key 与模型", "Save new key & model")
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
                                        open_oauth_models = theme::secondary_button(
                                            ui,
                                            t(
                                                zh,
                                                "添加订阅账号模型",
                                                "Add subscription model",
                                            ),
                                            palette,
                                        )
                                        .on_hover_text(t(
                                            zh,
                                            "打开 OAuth 账号页，选择账号实时声明的可用模型",
                                            "Open OAuth accounts and choose from live declared models",
                                        ))
                                        .clicked();
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
                            if volcengine_coding_plan {
                                ui.add_space(8.0);
                                ui.separator();
                                ui.label(
                                    egui::RichText::new(t(
                                        zh,
                                        "火山方舟 Coding Plan 管控面凭据（可选）",
                                        "Volcengine Coding Plan control-plane credentials (optional)",
                                    ))
                                    .strong()
                                    .color(palette.ink),
                                );
                                ui.label(
                                    egui::RichText::new(t(
                                        zh,
                                        "用于读取官方 5 小时、周、月额度；仅写入 Windows 凭据管理器，不会保存到配置文件。留空则保留已保存凭据。",
                                        "Used for official 5-hour, weekly, and monthly quota. Stored only in Windows Credential Manager, never in the config file. Leave blank to keep saved credentials.",
                                    ))
                                    .small()
                                    .color(palette.muted),
                                );
                                ui.columns(2, |columns| {
                                    theme::field_label(
                                        &mut columns[0],
                                        "ACCESS KEY ID",
                                        t(zh, "火山控制面 AK", "Volcengine control-plane AK"),
                                        palette,
                                    );
                                    theme::input_ascii(
                                        &mut columns[0],
                                        &mut this.temp_model.volcengine_access_key_id,
                                        t(zh, "输入 Access Key ID", "Enter Access Key ID"),
                                        true,
                                        palette,
                                    );
                                    theme::field_label(
                                        &mut columns[1],
                                        "SECRET ACCESS KEY",
                                        t(zh, "火山控制面 SK", "Volcengine control-plane SK"),
                                        palette,
                                    );
                                    theme::input_ascii(
                                        &mut columns[1],
                                        &mut this.temp_model.volcengine_secret_access_key,
                                        t(zh, "输入 Secret Access Key", "Enter Secret Access Key"),
                                        true,
                                        palette,
                                    );
                                });
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
                            if open_oauth_models {
                                this.open_oauth_manager();
                            } else if back {
                                this.page = if this.model_from_wizard {
                                    Page::Auth
                                } else {
                                    Page::Dashboard
                                };
                            }
                            if next && !open_oauth_models {
                                if this.temp_model.source != "oauth"
                                    && this.editing_model.is_none()
                                {
                                    this.api_model_validation_running = true;
                                    this.status_text = t(
                                        zh,
                                        "正在测试 API 渠道和模型可用性，通过后才会正式添加",
                                        "Testing the API channel and model before adding it",
                                    )
                                    .to_owned();
                                    let config = this.config.clone();
                                    let model = this.temp_model.clone();
                                    let model_from_wizard = this.model_from_wizard;
                                    let tx = this.event_tx.clone();
                                    std::thread::spawn(move || {
                                        let result = super::validate_api_model_connection(
                                            &config, &model,
                                        );
                                        tx.send(super::AppEvent::ApiModelValidationFinished {
                                            model: Box::new(model),
                                            editing_model: None,
                                            model_from_wizard,
                                            result,
                                        })
                                        .ok();
                                    });
                                } else {
                                    this.commit_model_draft(
                                        this.temp_model.clone(),
                                        this.editing_model,
                                        this.model_from_wizard,
                                    );
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
            egui::vec2(760.0, 620.0),
            egui::vec2(520.0, 360.0),
        );
        let mut cancel_clicked = false;
        let mut apply_clicked = false;
        let response = egui::Modal::new(egui::Id::new("codex-router-reasoning-modal"))
            .backdrop_color(egui::Color32::from_black_alpha(150))
            .frame(
                theme::dialog_window_frame(),
            )
            .show(ctx, |ui| {
                ui.set_width(modal_size.x.max(360.0));
                egui::Frame::new()
                    .fill(palette.background_dark)
                    .corner_radius(egui::CornerRadius {
                        nw: 13,
                        ne: 13,
                        sw: 0,
                        se: 0,
                    })
                    .inner_margin(egui::Margin::symmetric(22, 14))
                    .show(ui, |ui| {
                        theme::dialog_title(
                            ui,
                            t(zh, "思考强度与 Fast", "Reasoning effort & Fast"),
                        );
                    });
                egui::Frame::new()
                    .fill(palette.paper)
                    .corner_radius(egui::CornerRadius {
                        nw: 0,
                        ne: 0,
                        sw: 13,
                        se: 13,
                    })
                    .inner_margin(egui::Margin::same(22))
                    .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("reasoning-modal-scroll")
                    .auto_shrink([false, false])
                    .max_height((modal_size.y - 100.0).max(220.0))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
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
            egui::vec2(800.0, 590.0),
            egui::vec2(520.0, 360.0),
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
                theme::dialog_window_frame(),
            )
            .show(ctx, |ui| {
                ui.set_width(modal_size.x.max(360.0));
                egui::Frame::new()
                    .fill(palette.background_dark)
                    .corner_radius(egui::CornerRadius {
                        nw: 13,
                        ne: 13,
                        sw: 0,
                        se: 0,
                    })
                    .inner_margin(egui::Margin::symmetric(22, 14))
                    .show(ui, |ui| {
                        theme::dialog_title(ui, t(zh, "编辑高级 JSON", "Edit advanced JSON"));
                    });
                egui::Frame::new()
                    .fill(palette.paper)
                    .corner_radius(egui::CornerRadius {
                        nw: 0,
                        ne: 0,
                        sw: 13,
                        se: 13,
                    })
                    .inner_margin(egui::Margin::same(22))
                    .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("advanced-json-modal-scroll")
                    .auto_shrink([false, false])
                    .max_height((modal_size.y - 100.0).max(220.0))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
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
                                            "✓ 已同意《Codex-Router 条款与合规说明》",
                                            "✓ Codex-Router terms accepted",
                                        ))
                                        .strong()
                                        .color(palette.success),
                                    );
                                } else {
                                    ui.label(
                                        egui::RichText::new(t(
                                            zh,
                                            "尚未同意《Codex-Router 条款与合规说明》",
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
                                    "包含禁止商用、允许保留署名与官方 GitHub 发布地址的分发，以及上游组件的许可条款。",
                                    "Includes non-commercial use, redistribution with attribution and the official GitHub release URL, plus the license terms of bundled upstream components.",
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
                        if this.terms_are_current()
                            && (this.configured || !this.config.models.is_empty())
                            && theme::primary_button(
                            ui,
                            egui::RichText::new(t(zh, "进入控制台 →", "Open console →")).strong().color(egui::Color32::WHITE),
                            palette,
                        ).clicked()
                        {
                            this.page = Page::Dashboard;
                        }
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
            egui::vec2(940.0, 760.0),
            egui::vec2(600.0, 420.0),
        );
        let modal_width = modal_size.x.max(320.0);
        let preparation_error_space = if self.pending_oauth_provider.is_some()
            && !self.provider_oauth_prepare_error.is_empty()
        {
            100.0
        } else {
            0.0
        };
        let scroll_height =
            (modal_size.y - 250.0 - preparation_error_space).clamp(160.0, 600.0);
        let mut close_clicked = false;
        let mut accept_clicked = false;
        let mut retry_preparation = false;
        let response = egui::Modal::new(egui::Id::new("codex-router-terms-modal"))
            .backdrop_color(egui::Color32::from_black_alpha(150))
            .frame(
                theme::dialog_window_frame(),
            )
            .show(ctx, |ui| {
                ui.set_width(modal_width);
                theme::dialog_shell(
                    ui,
                    palette,
                    |ui| {
                        theme::dialog_title(
                            ui,
                            t(
                                zh,
                                "Codex-Router 条款与合规说明",
                                "Codex-Router Terms and Compliance",
                            ),
                        );
                        ui.label(
                            egui::RichText::new(t(
                                zh,
                                "请完整阅读；滚动到正文底部后即可确认。",
                                "Read the full terms. Confirmation unlocks at the end.",
                            ))
                            .color(egui::Color32::from_white_alpha(215)),
                        );
                    },
                    |ui| {
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
                        let max_offset =
                            (scroll.content_size.y - scroll.inner_rect.height()).max(0.0);
                        if reset_scroll {
                            self.terms_scroll_complete = false;
                            if scroll.content_size.y > 1.0 && scroll.state.offset.y <= 1.0 {
                                self.terms_scroll_reset_pending = false;
                                if max_offset <= 1.0 {
                                    self.terms_scroll_complete = true;
                                }
                            }
                        } else if max_offset <= 1.0
                            || scroll.state.offset.y >= max_offset - 12.0
                        {
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
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if theme::secondary_button(
                                ui,
                                t(zh, "暂不接受", "Not now"),
                                palette,
                            )
                            .clicked()
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
                    }
                );
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

    fn show_profiles(&mut self, ui: &mut egui::Ui, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let mut restore_original = false;
        let mut restore_previous = false;
        let mut apply_profile: Option<IsolationProfile> = None;
        let mut delete_profile: Option<IsolationProfile> = None;
        let restore_points =
            super::profiles::list_restore_points(&self.router_root).unwrap_or_default();
        let compact_header = ui.available_width() < 1000.0;
        let mut back_to_console = false;
        let mut back_to_previous = false;
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
                        self.share_session_toggle_button(ui, palette, zh);
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
                    self.share_session_toggle_button(ui, palette, zh);
                });
            });
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

        egui::ScrollArea::vertical()
            .id_salt("configuration-profiles-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
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
                                if ui
                                    .add_enabled(
                                        !self.applying,
                                        egui::Button::new(
                                            egui::RichText::new(t(
                                                zh,
                                                "＋ 新建配置",
                                                "＋ New profile",
                                            ))
                                            .strong()
                                            .color(egui::Color32::WHITE),
                                        )
                                        .fill(palette.action)
                                        .stroke(egui::Stroke::NONE)
                                        .corner_radius(egui::CornerRadius::same(7)),
                                    )
                                    .on_hover_text(t(
                                        zh,
                                        "新建本地隔离配置",
                                        "Create a local isolated profile",
                                    ))
                                    .clicked()
                                {
                                    self.local_profile_name_input.clear();
                                    self.profile_create_open = true;
                                }
                            },
                        );
                    });
                    if self.isolation_profiles.is_empty() {
                        ui.label(t(
                            zh,
                            "还没有隔离配置。点击右上角「新建配置」创建本地隔离配置。",
                            "No isolated profiles yet. Click New profile to create one.",
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
                                let apply_width = if active { 150.0_f32 } else { 104.0_f32 };
                                let delete_width = 66.0_f32;
                                let action_width = apply_width + delete_width + 8.0;
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
                                            let delete_response = ui.add_enabled(
                                                !self.applying && !active,
                                                egui::Button::new(
                                                    egui::RichText::new(t(zh, "删除", "Delete"))
                                                        .strong()
                                                        .color(if active {
                                                            palette.muted
                                                        } else {
                                                            palette.danger
                                                        }),
                                                )
                                                .fill(palette.paper)
                                                .stroke(egui::Stroke::new(1.0, palette.danger))
                                                .corner_radius(egui::CornerRadius::same(7)),
                                            );
                                            let delete_response = if active {
                                                delete_response.on_disabled_hover_text(t(
                                                    zh,
                                                    "当前正在使用；请先应用另一个配置或初始化 Codex",
                                                    "Currently active; apply another profile or reset Codex first",
                                                ))
                                            } else {
                                                delete_response.on_hover_text(t(
                                                    zh,
                                                    "删除这个已保存配置",
                                                    "Delete this saved profile",
                                                ))
                                            };
                                            if delete_response.clicked() {
                                                delete_profile = Some(profile.clone());
                                            }
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
                ui.add_space(12.0);
                ui.columns(2, |columns| {
                    theme::paper_frame(palette).show(&mut columns[0], |ui| {
                        theme::eyebrow(ui, "01 / LOCAL", palette.muted);
                        ui.heading(t(zh, "返回上一次配置", "Restore previous"));
                        ui.label(t(
                            zh,
                            "把 Codex 恢复到最近一次应用前自动保存的状态。登录状态、聊天记录、插件、MCP 与权限按当时快照还原。",
                            "Restore Codex to the last automatically saved pre-apply snapshot. Sign-in, chats, plugins, MCP, and permissions follow that snapshot.",
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
                    });
                    theme::paper_frame(palette).show(&mut columns[1], |ui| {
                        theme::eyebrow(ui, "02 / CODEX", palette.muted);
                        ui.heading(t(zh, "初始化 Codex 默认配置", "Reset Codex defaults"));
                        ui.label(t(
                            zh,
                            "移除 Codex-Router 写入的模型提供方、模型目录与推理默认值，让 Codex 回到自己的默认配置与登录流程。登录状态、聊天记录、插件、MCP 与权限不会被改动。",
                            "Remove the model provider, catalog, and reasoning defaults written by Codex-Router. Sign-in, chats, plugins, MCP, and permissions stay untouched.",
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
                });
                ui.add_space(12.0);
            });

        if restore_original {
            self.restore_original_codex();
        } else if restore_previous {
            self.restore_previous_codex();
        } else if let Some(profile) = apply_profile {
            self.apply_isolation_profile(&profile);
        }
        if let Some(profile) = delete_profile {
            self.profile_delete_target = Some(profile);
        }
    }

    fn show_profile_create_dialog(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(520.0, 280.0),
            egui::vec2(400.0, 240.0),
        );
        egui::Window::new("")
            .id(egui::Id::new("profile-create-dialog"))
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
                    .corner_radius(egui::CornerRadius::same(14))
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
                    .fill(palette.background_dark)
                    .inner_margin(egui::Margin::symmetric(22, 13))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(t(zh, "新建隔离配置", "New isolated profile"))
                                .size(24.0)
                                .strong()
                                .color(palette.paper),
                        );
                    });
                egui::Frame::new()
                    .fill(palette.paper)
                    .inner_margin(egui::Margin::same(22))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(t(
                                zh,
                                "为这组模型、渠道和路由策略起一个名称。保存后会立即应用并完成本地隔离。",
                                "Name this set of models, channels, and routes. Saving applies it and isolates it locally.",
                            ))
                            .color(palette.ink_soft),
                        );
                        ui.add_space(12.0);
                        theme::input(
                            ui,
                            &mut self.local_profile_name_input,
                            t(zh, "例如 工作", "e.g. Work"),
                            false,
                            palette,
                        );
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            let can_save =
                                !self.applying && !self.local_profile_name_input.trim().is_empty();
                            if ui
                                .add_enabled(
                                    can_save,
                                    egui::Button::new(
                                        egui::RichText::new(t(zh, "保存", "Save"))
                                            .strong()
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(palette.action)
                                    .corner_radius(egui::CornerRadius::same(7)),
                                )
                                .clicked()
                            {
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
            let name = self.local_profile_name_input.clone();
            self.profile_create_open = false;
            self.create_isolation_profile(IsolationKind::Local, name);
        } else if cancel || !open {
            self.profile_create_open = false;
        }
    }

    fn show_profile_delete_dialog(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let Some(profile) = self.profile_delete_target.clone() else {
            return;
        };
        let mut open = true;
        let mut confirm = false;
        let mut cancel = false;
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(560.0, 330.0),
            egui::vec2(420.0, 280.0),
        );
        egui::Window::new("")
            .id(egui::Id::new("profile-delete-confirmation"))
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
                    .corner_radius(egui::CornerRadius::same(14))
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
                    .fill(palette.background_dark)
                    .inner_margin(egui::Margin::symmetric(22, 13))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(t(zh, "删除已保存配置", "Delete saved profile"))
                                .size(24.0)
                                .strong()
                                .color(palette.paper),
                        );
                    });
                egui::Frame::new()
                    .fill(palette.paper)
                    .inner_margin(egui::Margin::same(22))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(&profile.name)
                                .size(20.0)
                                .strong()
                                .color(palette.ink),
                        );
                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new(t(
                                zh,
                                "将永久删除这个配置快照及其隔离保存的 API 凭据。不会删除 OAuth 账号，也不会修改当前 Codex 配置。",
                                "This permanently deletes the saved profile and its isolated API credentials. OAuth accounts and the current Codex configuration are not changed.",
                            ))
                            .color(palette.danger),
                        );
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    !self.applying,
                                    egui::Button::new(
                                        egui::RichText::new(t(zh, "确认删除", "Delete profile"))
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
            self.profile_delete_target = None;
            self.delete_isolation_profile(profile);
        } else if cancel || !open {
            self.profile_delete_target = None;
        }
    }

    fn show_channel_preset_dialog(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(720.0, 560.0),
            egui::vec2(480.0, 360.0),
        );
        let mut close = false;
        let mut header_close = false;
        let mut open_recommended = false;
        let mut selected_preset = None;
        egui::Window::new("")
            .id(egui::Id::new("channel-preset-dialog"))
            .title_bar(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .default_size(dialog_size)
            .min_size(dialog_size)
            .max_size(dialog_size)
            .frame(theme::dialog_window_frame())
            .show(ctx, |ui| {
                ui.set_width(dialog_size.x);
                theme::dialog_shell(ui, palette, |ui| {
                    ui.horizontal(|ui| {
                        theme::dialog_title(ui, t(zh, "选择一个渠道", "Choose a channel"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                            egui::RichText::new("×")
                                                .size(18.0)
                                                .color(egui::Color32::WHITE),
                                    )
                                    .fill(egui::Color32::TRANSPARENT)
                                    .stroke(egui::Stroke::NONE),
                                )
                                .on_hover_text(t(zh, "关闭", "Close"))
                                .clicked()
                            {
                                header_close = true;
                            }
                            ui.add_space(6.0);
                            if theme::secondary_button(
                                ui,
                                t(zh, "推荐平台", "Recommended"),
                                palette,
                            )
                            .clicked()
                            {
                                open_recommended = true;
                            }
                        });
                    });
                }, |ui| {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(t(
                            zh,
                            "点卡片即可带入模型 ID 和地址。API Key 下一页再填，不会预写。",
                            "Click a card to fill the model ID and base URL. Enter the API key on the next page.",
                        ))
                        .color(palette.ink_soft),
                    );
                    ui.add_space(12.0);
                    egui::ScrollArea::vertical()
                        .id_salt("channel-preset-selection-scroll")
                        .max_height((dialog_size.y - 180.0).clamp(160.0, 360.0))
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
                                        let response = egui::Frame::new()
                                            .fill(palette.paper)
                                            .stroke(egui::Stroke::new(1.0, palette.line))
                                            .corner_radius(egui::CornerRadius::same(10))
                                            .inner_margin(egui::Margin::symmetric(14, 12))
                                            .show(&mut columns[index], |ui| {
                                                ui.set_width(ui.available_width());
                                                ui.label(
                                                    egui::RichText::new(label)
                                                        .size(15.0)
                                                        .strong()
                                                        .color(palette.ink),
                                                );
                                                ui.add_space(6.0);
                                                ui.label(
                                                    egui::RichText::new(preset.model)
                                                        .monospace()
                                                        .small()
                                                        .color(palette.action),
                                                );
                                                ui.label(
                                                    egui::RichText::new(preset.base_url)
                                                        .small()
                                                        .color(palette.muted),
                                                );
                                            })
                                            .response
                                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                                            .interact(egui::Sense::click());
                                        if response.clicked() {
                                            selected_preset = Some(preset.id);
                                        }
                                    }
                                });
                                ui.add_space(8.0);
                            }
                        });
                    ui.add_space(10.0);
                    if theme::secondary_button(ui, t(zh, "取消", "Cancel"), palette).clicked() {
                        close = true;
                    }
                });
            });
        close |= header_close;
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
        } else if close {
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
                    .corner_radius(egui::CornerRadius::same(14))
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
                    .fill(palette.background_dark)
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
                    .fill(palette.paper)
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
                    .corner_radius(egui::CornerRadius::same(14))
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
                    .fill(palette.background_dark)
                    .inner_margin(egui::Margin::symmetric(22, 13))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(t(zh, "撤销订阅", "Revoke plan"))
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
                    .fill(palette.paper)
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
                                        "确认后将永久删除本机 Router 中保存的 OAuth 令牌和账号，并从所有路由配置中移除该账号导入的模型。",
                                        "This permanently deletes the OAuth tokens and account stored in the local Router, and removes models imported from it from every route profile.",
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
            .filter(|peer| {
                subscription_provider_key(&peer.platform)
                    == subscription_provider_key(&account.platform)
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        let mut edit_peer = None;
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(560.0, 420.0),
            egui::vec2(420.0, 320.0),
        );
        egui::Window::new("")
        .id(egui::Id::new("oauth-priority-dialog"))
        .title_bar(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .default_size(dialog_size)
        .min_size(dialog_size)
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .frame(theme::dialog_window_frame())
        .show(ctx, |ui| {
            ui.set_width(dialog_size.x);
            theme::dialog_shell(
                ui,
                palette,
                |ui| {
                    theme::dialog_title(
                        ui,
                        t(zh, "订阅账号池优先级", "Subscription pool priority"),
                    );
                },
                |ui| {
            ui.label(
                egui::RichText::new(if zh {
                    format!(
                        "服务商：{} · 当前账号：{}\n数值越小越优先。下方可切换编辑该服务商账号池中的任意账号。",
                        subscription_provider_title(&account.platform), account.name
                    )
                } else {
                    format!(
                        "Provider: {} · Current account: {}\nLower values run first. Select any account below to edit the provider pool order.",
                        subscription_provider_title(&account.platform), account.name
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
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(label)
                                            .small()
                                            .color(if peer.id == account.id {
                                                palette.action
                                            } else {
                                                palette.muted
                                            }),
                                    )
                                    .truncate(),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if peer.id != account.id
                                            && ui
                                                .small_button(t(zh, "编辑", "Edit"))
                                                .clicked()
                                        {
                                            edit_peer = Some(peer.clone());
                                        }
                                    },
                                );
                            });
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
                },
            );
        });
        if save {
            self.save_oauth_account_priority();
        } else if let Some(peer) = edit_peer {
            self.open_oauth_priority_editor(peer);
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
            egui::vec2(680.0, 470.0),
            egui::vec2(480.0, 360.0),
        );
        egui::Window::new("")
            .id(egui::Id::new("grok-sso-import-dialog"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size(dialog_size)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .frame(theme::dialog_window_frame())
            .show(ctx, |ui| {
                ui.set_width(dialog_size.x);
                theme::dialog_shell(ui, palette, |ui| {
                    theme::dialog_title(ui, t(zh, "导入 Grok 授权码 / SSO Token", "Import Grok authorization / SSO token"));
                }, |ui| {
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
                        "授权码只通过标准输入交给本机 Router，不写入配置、日志或命令行。",
                        "Tokens are sent to the local Router over standard input and are never written to config, logs, or command-line arguments.",
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
            });
        if import {
            self.import_grok_sso();
        } else if cancel || !open {
            self.grok_sso_dialog_open = false;
            self.grok_sso_draft.clear();
            self.grok_sso_error.clear();
        }
    }

    fn show_provider_oauth_prompt(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let Some(prompt) = self
            .provider_oauth_prompt
            .as_ref()
            .map(|state| state.prompt.clone())
        else {
            return;
        };
        let zh = self.ui_language == "zh";
        let mut open = true;
        let mut submit = false;
        let mut cancel = false;
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(680.0, 470.0),
            egui::vec2(480.0, 360.0),
        );
        egui::Window::new("")
            .id(egui::Id::new("provider-oauth-prompt"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size(dialog_size)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .frame(theme::dialog_window_frame())
            .show(ctx, |ui| {
                ui.set_width(dialog_size.x);
                let title = match &prompt {
                    crate::logic::oauth::Prompt::GeminiConfiguration { .. } => {
                        t(zh, "选择 Gemini 额度类型", "Choose Gemini quota type").to_owned()
                    }
                    crate::logic::oauth::Prompt::AuthorizationCode { provider, .. } => format!(
                        "{} {}",
                        provider.as_str(),
                        t(zh, "授权码", "authorization code")
                    ),
                };
                theme::dialog_shell(ui, palette, |ui| theme::dialog_title(ui, &title), |ui| {
                match &prompt {
                    crate::logic::oauth::Prompt::GeminiConfiguration {
                        detected_project_id: _,
                    } => {
                        ui.add_space(18.0);
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut self.provider_oauth_gemini_code_assist,
                                false,
                                t(zh, "Google One / 个人", "Google One / personal"),
                            );
                            ui.selectable_value(
                                &mut self.provider_oauth_gemini_code_assist,
                                true,
                                t(zh, "GCP Code Assist", "GCP Code Assist"),
                            );
                        });
                        ui.add_space(20.0);
                        ui.label(
                            egui::RichText::new(t(
                                zh,
                                "GOOGLE CLOUD PROJECT ID（可选）",
                                "GOOGLE CLOUD PROJECT ID (OPTIONAL)",
                            ))
                            .small()
                            .strong()
                            .color(palette.muted),
                        );
                        ui.add_sized(
                            [ui.available_width(), 34.0],
                            egui::TextEdit::singleline(&mut self.provider_oauth_project_draft),
                        );
                    }
                    crate::logic::oauth::Prompt::AuthorizationCode {
                        provider: _,
                        manual,
                    } => {
                        ui.add_space(18.0);
                        if *manual {
                            ui.label(
                                egui::RichText::new(t(
                                    zh,
                                    "本机回调端口正被其他程序占用。网页确认后即使页面没有响应，也请复制浏览器地址栏中的完整 localhost 回调 URL，并粘贴到下方。",
                                    "The local callback port is in use by another program. After confirming in the browser, copy the complete localhost callback URL from the address bar even if the page does not respond, then paste it below.",
                                ))
                                .color(palette.danger),
                            );
                            ui.add_space(12.0);
                        }
                        ui.label(
                            egui::RichText::new(t(
                                zh,
                                "一次性授权码或完整回调 URL",
                                "ONE-TIME CODE OR FULL CALLBACK URL",
                            ))
                                .small()
                                .strong()
                                .color(palette.muted),
                        );
                        ui.add_sized(
                            [ui.available_width(), 36.0],
                            egui::TextEdit::singleline(&mut self.provider_oauth_code_draft)
                                .password(true),
                        );
                    }
                }
                ui.add_space(24.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let enabled = match prompt {
                        crate::logic::oauth::Prompt::GeminiConfiguration { .. } => true,
                        crate::logic::oauth::Prompt::AuthorizationCode { .. } => {
                            !self.provider_oauth_code_draft.trim().is_empty()
                        }
                    };
                    let button = ui.add_enabled_ui(enabled, |ui| {
                        theme::primary_button(
                            ui,
                            egui::RichText::new(t(zh, "继续", "Continue"))
                                .strong()
                                .color(egui::Color32::WHITE),
                            palette,
                        )
                    });
                    if button.inner.clicked() {
                        submit = true;
                    }
                    if theme::secondary_button(ui, t(zh, "取消", "Cancel"), palette).clicked() {
                        cancel = true;
                    }
                });
                });
            });

        if submit {
            if let Some(state) = self.provider_oauth_prompt.take() {
                let response = match state.prompt {
                    crate::logic::oauth::Prompt::GeminiConfiguration { .. } => {
                        crate::logic::oauth::PromptResponse::GeminiConfiguration {
                            oauth_type: if self.provider_oauth_gemini_code_assist {
                                "code_assist"
                            } else {
                                "google_one"
                            }
                            .to_owned(),
                            tier_id: if self.provider_oauth_gemini_code_assist {
                                "gcp_standard"
                            } else {
                                "google_one_free"
                            }
                            .to_owned(),
                            project_id: self.provider_oauth_project_draft.trim().to_owned(),
                        }
                    }
                    crate::logic::oauth::Prompt::AuthorizationCode { .. } => {
                        crate::logic::oauth::PromptResponse::AuthorizationCode(
                            zeroize::Zeroizing::new(
                                self.provider_oauth_code_draft.trim().to_owned(),
                            ),
                        )
                    }
                };
                let _ = state.response.send(response);
            }
            self.provider_oauth_code_draft.clear();
            self.provider_oauth_project_draft.clear();
        } else if cancel || !open {
            self.cancel_provider_oauth();
        }
    }

    #[allow(dead_code)]
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

    #[allow(dead_code)]
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

    #[allow(clippy::too_many_arguments)]
    fn show_subscription_provider_card(
        &mut self,
        ui: &mut egui::Ui,
        accounts: &[OAuthAccountSummary],
        selected_ids: &mut Vec<i64>,
        selection_changed: &mut bool,
        import_model: &mut Option<(OAuthAccountSummary, OAuthModelSummary)>,
        remove_model: &mut Option<(i64, String, String)>,
        revoke_account: &mut Option<OAuthAccountSummary>,
        priority_account: &mut Option<OAuthAccountSummary>,
        palette: &theme::Palette,
        zh: bool,
    ) {
        let Some(representative) = accounts.first() else {
            return;
        };
        let provider = subscription_provider_title(&representative.platform);
        let identifiers = accounts
            .iter()
            .map(subscription_account_identifier)
            .collect::<Vec<_>>()
            .join(" · ");
        let mut enabled = accounts
            .iter()
            .all(|account| selected_ids.contains(&account.id));

        let mut models: Vec<(&OAuthAccountSummary, &OAuthModelSummary)> = Vec::new();
        let mut seen_models = std::collections::HashSet::new();
        for account in accounts {
            for model in &account.models {
                if seen_models.insert(model.id.to_ascii_lowercase()) {
                    models.push((account, model));
                }
            }
        }

        egui::Frame::new()
            .fill(palette.paper)
            .stroke(egui::Stroke::new(1.0, palette.line))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.set_min_height(SUBSCRIPTION_CARD_HEIGHT);
                ui.set_max_height(SUBSCRIPTION_CARD_HEIGHT);
                ui.label(
                    egui::RichText::new(provider)
                        .font(egui::FontId::new(24.0, theme::display_family()))
                        .strong()
                        .color(palette.ink),
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(if zh {
                            format!("该服务商下共有 {} 个账号：{identifiers}", accounts.len())
                        } else {
                            format!("{} account(s): {identifiers}", accounts.len())
                        })
                        .small()
                        .color(palette.muted),
                    )
                    .truncate(),
                )
                .on_hover_text(&identifiers);
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(t(zh, "本配置已启用", "Enabled"))
                            .small()
                            .strong()
                            .color(palette.ink),
                    );
                    if Self::dashboard_route_switch(ui, enabled, false, palette).clicked() {
                        enabled = !enabled;
                        *selection_changed = true;
                        for account in accounts {
                            if enabled {
                                if !selected_ids.contains(&account.id) {
                                    selected_ids.push(account.id);
                                }
                            } else {
                                selected_ids.retain(|id| *id != account.id);
                            }
                        }
                    }
                    if ui
                        .add(
                            egui::Button::new(t(zh, "优先级", "Priority"))
                                .fill(palette.paper_alt)
                                .stroke(egui::Stroke::new(1.0, palette.line))
                                .corner_radius(egui::CornerRadius::same(6)),
                        )
                        .on_hover_text(t(
                            zh,
                            "调整当前服务商全部账号池的调度顺序",
                            "Adjust scheduling order for all accounts in this provider pool",
                        ))
                        .clicked()
                    {
                        *priority_account = Some(representative.clone());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let revoke = ui
                            .add_enabled(
                                !self.oauth_revoking,
                                egui::Button::new(t(zh, "撤销此订阅", "Revoke subscription"))
                                    .fill(palette.paper)
                                    .stroke(egui::Stroke::new(1.0, palette.danger))
                                    .corner_radius(egui::CornerRadius::same(6)),
                            )
                            .on_hover_text(if accounts.len() == 1 {
                                t(
                                    zh,
                                    "删除本机保存的订阅令牌、账号和所有配置引用",
                                    "Delete the local subscription token, account, and profile references",
                                )
                                .to_owned()
                            } else if zh {
                                format!("先撤销账号 {}；其余账号仍保留", subscription_account_identifier(representative))
                            } else {
                                format!("Revoke {} first; other accounts remain", subscription_account_identifier(representative))
                            });
                        if revoke.clicked() {
                            *revoke_account = Some(representative.clone());
                        }
                    });
                });
                if let Some(account) = accounts.iter().find(|account| !account.error.is_empty()) {
                    ui.label(
                        egui::RichText::new(oauth_account_error(&account.error, zh))
                            .small()
                            .color(palette.danger),
                    );
                } else if let Some(account) = accounts
                    .iter()
                    .find(|account| !account.expires_at.trim().is_empty())
                {
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
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(t(zh, "账号可用模型", "Available models"))
                        .strong()
                        .color(palette.ink),
                );
                egui::Frame::new()
                    .fill(palette.paper_alt)
                    .stroke(egui::Stroke::new(1.0, palette.line))
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_min_height(SUBSCRIPTION_MODEL_LIST_HEIGHT);
                        ui.set_max_height(SUBSCRIPTION_MODEL_LIST_HEIGHT);
                        egui::ScrollArea::vertical()
                            .id_salt(("subscription-models", provider))
                            .max_height(SUBSCRIPTION_MODEL_LIST_HEIGHT - 16.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if models.is_empty() {
                                    ui.label(
                                        egui::RichText::new(t(
                                            zh,
                                            "暂无可用模型；请点击上方刷新。",
                                            "No models yet; click Refresh above.",
                                        ))
                                        .small()
                                        .color(palette.muted),
                                    );
                                }
                                for (account, model) in &models {
                                    let already_added = self.config.models.iter().any(|item| {
                                        item.source == "oauth"
                                            && item.oauth_account_id == account.id
                                            && item.model == model.id
                                    });
                                    let unsupported = model.id.to_ascii_lowercase().contains("image");
                                    let label = if already_added {
                                        format!("{} · {}", model.display_name, t(zh, "已加入", "Added"))
                                    } else {
                                        format!("＋ {}", model.display_name)
                                    };
                                    let response = ui.add_enabled(
                                        already_added || !unsupported,
                                        egui::Button::new(
                                            egui::RichText::new(label).color(palette.ink),
                                        )
                                        .fill(palette.paper)
                                        .stroke(egui::Stroke::new(1.0, palette.line))
                                        .corner_radius(egui::CornerRadius::same(5))
                                        .min_size(egui::vec2(ui.available_width(), 30.0)),
                                    );
                                    if already_added {
                                        response.clone().context_menu(|ui| {
                                            if ui
                                                .button(t(
                                                    zh,
                                                    "从当前配置删除",
                                                    "Remove from this profile",
                                                ))
                                                .clicked()
                                            {
                                                *remove_model = Some((
                                                    account.id,
                                                    model.id.clone(),
                                                    model.display_name.clone(),
                                                ));
                                                ui.close();
                                            }
                                        });
                                    } else if !unsupported && response.clicked() {
                                        *import_model = Some(((*account).clone(), (*model).clone()));
                                    }
                                }
                            });
                    });
            });
    }

    fn show_oauth_accounts(&mut self, ui: &mut egui::Ui, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let active_config_name = self.active_route_config_name(zh);
        let oauth_count = self.config.oauth_account_ids.as_ref().map_or(0, Vec::len);
        let compact_header = ui.available_width() < 1000.0;
        let applying = self.applying;
        let return_to_model_editor = matches!(self.oauth_return_page, Page::Model);
        let mut back_to_console = false;
        let mut apply_current_profile = false;
        let mut refresh_accounts = false;
        let show_title = |ui: &mut egui::Ui| {
            ui.vertical(|ui| {
                theme::eyebrow(ui, t(zh, "订阅套餐", "SUBSCRIPTION PLANS"), palette.paper);
                ui.label(
                    egui::RichText::new(t(zh, "当前配置的订阅授权", "PROFILE SUBSCRIPTIONS"))
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
            back_to_console = theme::secondary_button(
                ui,
                if return_to_model_editor {
                    t(zh, "返回模型编辑", "Back to model editor")
                } else {
                    t(zh, "返回控制台", "Back to console")
                },
                palette,
            )
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
            self.trigger_self_check();
        }
        ui.add_space(14.0);
        let accounts = self.oauth_accounts.clone();
        let connected_count = accounts.len();
        let mut selected_ids = self.config.oauth_account_ids.clone().unwrap_or_default();
        let mut selection_changed = false;
        let mut import_model = None;
        let mut remove_model = None;
        let mut revoke_account = None;
        let mut priority_account = None;
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
                    "登录账号由本机 Router 安全保管；本页的启用账号、模型和回退策略只属于当前配置",
                    "The local Router securely stores sign-ins; enabled accounts, models, and fallback policy on this page belong only to the current profile",
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
                            "正在等待当前订阅授权完成",
                            "Waiting for the current subscription login",
                        ));
                    });
                    if ui
                        .add_sized(
                            [ui.available_width(), 34.0],
                            egui::Button::new(
                                egui::RichText::new(t(zh, "取消授权", "Cancel sign-in"))
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
            });
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(6.0);
            ui.horizontal_top(|ui| {
                let fallback_response = ui.checkbox(&mut self.config.oauth_fallback.enabled, "");
                ui.add(
                    egui::Label::new(t(
                        zh,
                        "启用订阅套餐与同名 API / Base URL 之间的自动接续",
                        "Enable automatic continuity between subscriptions and matching API / Base URL channels",
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
                                        t(zh, "同名模型首选：订阅额度", "Matching model priority: subscription quota")
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
                                        "开启为订阅优先；关闭为其他额度优先",
                                        "On prefers subscription; off prefers other API quota",
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
                        "{}: {} P{} → API Key P{}",
                        t(zh, "路由优先级", "Routing priority"),
                        t(zh, "订阅", "Sub"),
                        oauth_priority,
                        api_priority
                    ))
                    .small()
                    .color(palette.ink_soft),
                );
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let retries_response = ui.add(
                    egui::DragValue::new(&mut self.config.rate_limit_max_retries).range(0..=32),
                );
                theme::ascii_response(ui, &retries_response);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(t(
                            zh,
                            "上游 429 / 断网自动重试次数（2s / 10s / 30s / 1min / 3min / 5min）",
                            "Automatic retries on 429 or network errors (2s / 10s / 30s / 1min / 3min / 5min)",
                        ))
                        .small()
                        .color(palette.ink_soft),
                    )
                    .wrap(),
                );
                if retries_response.changed() {
                    self.status_text = t(
                        zh,
                        "429 限流重试次数已修改；保存并应用后生效",
                        "The 429 retry count changed. Save & apply to activate it.",
                    )
                    .into();
                }
            });
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
                        self.copy_local_api_key();
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
                                    "已添加的订阅账号",
                                    "CONNECTED SUBSCRIPTIONS",
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

        let mut provider_groups: Vec<(String, Vec<OAuthAccountSummary>)> = Vec::new();
        for account in &accounts {
            let key = subscription_provider_key(&account.platform);
            if let Some((_, grouped)) = provider_groups
                .iter_mut()
                .find(|(existing, _)| existing == &key)
            {
                grouped.push(account.clone());
            } else {
                provider_groups.push((key, vec![account.clone()]));
            }
        }
        let account_height = (content_height - 82.0).max(220.0);
        egui::ScrollArea::vertical()
            .id_salt("oauth-account-list")
            .max_height(account_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let columns_count = if ui.available_width() >= 760.0 { 2 } else { 1 };
                for pair in provider_groups.chunks(columns_count) {
                    ui.columns(columns_count, |columns| {
                        for (index, (_, group)) in pair.iter().enumerate() {
                            self.show_subscription_provider_card(
                                &mut columns[index],
                                group,
                                &mut selected_ids,
                                &mut selection_changed,
                                &mut import_model,
                                &mut remove_model,
                                &mut revoke_account,
                                &mut priority_account,
                                palette,
                                zh,
                            );
                        }
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
        if selection_changed {
            selected_ids.sort_unstable();
            selected_ids.dedup();
            self.config.oauth_account_ids = Some(selected_ids);
            self.schedule_usage_refresh();
            self.status_text = t(
                zh,
                "订阅账号选择已保存到当前配置；点击“保存并应用”后生效",
                "Subscription selection is stored in this profile. Save & apply to activate it.",
            )
            .into();
        }
        if let Some((account_id, model_id, display_name)) = remove_model {
            if super::logic::remove_oauth_model_reference(&mut self.config, account_id, &model_id) {
                self.schedule_usage_refresh();
                self.status_text = if zh {
                    format!("已从当前配置删除 {display_name}；点击“保存并应用”后生效")
                } else {
                    format!(
                        "Removed {display_name} from this profile. Save & apply to activate it."
                    )
                };
            }
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
                base_url: format!("Router OAuth / {}", account.platform),
                priority: self.config.oauth_fallback.official_priority,
                source: "oauth".into(),
                oauth_account_id: account.id,
                oauth_platform: account.platform,
                user_selected: true,
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
                "订阅模型已加入当前配置；点击“保存并应用”后可在 Codex 中使用",
                "The subscription model was added to this profile. Save & apply to use it in Codex.",
            )
            .into();
        }
    }

    fn show_model_route_policy_dialog(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let Some(index) = self.model_route_policy_target else {
            return;
        };
        let Some(model) = self.config.models.get(index) else {
            self.model_route_policy_target = None;
            return;
        };
        let model_id = model.model.clone();
        let display_name = if model.alias.trim().is_empty() {
            model.model.clone()
        } else {
            model.alias.clone()
        };
        let has_api = !super::logic::matching_api_fallback_models(&self.config, &model_id).is_empty();
        let mut save = false;
        let mut cancel = false;
        let mut header_cancel = false;
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(520.0, 340.0),
            egui::vec2(400.0, 280.0),
        );
        egui::Window::new("")
            .id(egui::Id::new("model-route-policy-dialog"))
            .title_bar(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .default_size(dialog_size)
            .min_size(dialog_size)
            .max_size(dialog_size)
            .collapsible(false)
            .resizable(false)
            .frame(theme::dialog_window_frame())
            .show(ctx, |ui| {
                ui.set_width(dialog_size.x);
                theme::dialog_shell(ui, palette, |ui| {
                    ui.horizontal(|ui| {
                        theme::dialog_title(ui, t(zh, "路由策略", "Routing policy"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                            egui::RichText::new("×")
                                                .size(18.0)
                                                .color(egui::Color32::WHITE),
                                    )
                                    .fill(egui::Color32::TRANSPARENT)
                                    .stroke(egui::Stroke::NONE),
                                )
                                .clicked()
                            {
                                header_cancel = true;
                            }
                        });
                    });
                }, |ui| {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(display_name)
                            .font(egui::FontId::new(24.0, theme::display_family()))
                            .color(palette.ink),
                    );
                    ui.add_space(10.0);
                    let options = [
                        (
                            super::logic::ModelRoutePolicy::SubscriptionFirst,
                            t(zh, "优先订阅", "Subscription first"),
                            t(
                                zh,
                                "订阅额度用完后自动切到第三方 API",
                                "Switch to a third-party API when the subscription is exhausted",
                            ),
                        ),
                        (
                            super::logic::ModelRoutePolicy::ApiFirst,
                            t(zh, "优先 API", "API first"),
                            t(
                                zh,
                                "第三方 API 用完后再回到订阅",
                                "Fall back to the subscription when the API is exhausted",
                            ),
                        ),
                        (
                            super::logic::ModelRoutePolicy::SubscriptionOnly,
                            t(zh, "仅订阅", "Subscription only"),
                            t(
                                zh,
                                "只走订阅账号，不使用第三方 API",
                                "Use subscription accounts only",
                            ),
                        ),
                    ];
                    for (policy, label, detail) in options {
                        let api_needed = policy != super::logic::ModelRoutePolicy::SubscriptionOnly;
                        let enabled = has_api || !api_needed;
                        let selected = self.model_route_policy_draft == policy;
                        ui.add_enabled_ui(enabled, |ui| {
                            let fill = if selected {
                                palette.background_light
                            } else {
                                palette.paper
                            };
                            let response = egui::Frame::new()
                                .fill(fill)
                                .stroke(egui::Stroke::new(
                                    1.0,
                                    if selected {
                                        palette.action
                                    } else {
                                        palette.line
                                    },
                                ))
                                .corner_radius(egui::CornerRadius::same(8))
                                .inner_margin(egui::Margin::symmetric(12, 8))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.label(
                                        egui::RichText::new(label)
                                            .strong()
                                            .color(palette.ink),
                                    );
                                    ui.label(
                                        egui::RichText::new(if enabled {
                                            detail
                                        } else {
                                            t(
                                                zh,
                                                "当前没有可用的第三方 API 渠道",
                                                "No third-party API channel is available",
                                            )
                                        })
                                        .small()
                                        .color(palette.muted),
                                    );
                                })
                                .response
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .interact(egui::Sense::click());
                            if response.clicked() {
                                self.model_route_policy_draft = policy;
                            }
                        });
                        ui.add_space(6.0);
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if theme::primary_button(
                            ui,
                            egui::RichText::new(t(zh, "保存", "Save"))
                                .strong()
                                .color(egui::Color32::WHITE),
                            palette,
                        )
                        .clicked()
                        {
                            save = true;
                        }
                        ui.add_space(8.0);
                        if theme::secondary_button(ui, t(zh, "取消", "Cancel"), palette).clicked()
                        {
                            cancel = true;
                        }
                    });
                });
            });
        cancel |= header_cancel;
        if save {
            if !has_api {
                self.model_route_policy_draft = super::logic::ModelRoutePolicy::SubscriptionOnly;
            }
            super::logic::set_model_route_policy(
                &mut self.config,
                &model_id,
                self.model_route_policy_draft,
            );
            if self.model_route_policy_draft != super::logic::ModelRoutePolicy::SubscriptionOnly {
                self.config.oauth_fallback.enabled = true;
            }
            if !self.ui_audit_mode {
                let _ = self
                    .config
                    .save(&crate::user_data::config_path(&self.router_root));
            }
            self.status_text = t(
                zh,
                "同名模型路由策略已保存；点击“保存并应用”后生效",
                "Same-model routing policy saved. Save & apply to activate it.",
            )
            .to_owned();
            self.model_route_policy_target = None;
        } else if cancel {
            self.model_route_policy_target = None;
        }
    }

    fn show_model_priority_dialog(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let Some(route_id) = self.model_priority_dialog_target.clone() else {
            return;
        };
        if self.model_priority_order.is_empty()
            || self
                .model_priority_order
                .iter()
                .any(|idx| self.config.models.get(*idx).is_none())
        {
            self.model_priority_dialog_target = None;
            return;
        }
        let display_name = self
            .config
            .models
            .iter()
            .find(|m| super::logic::same_model_identity(&m.model, &route_id))
            .and_then(|m| if m.alias.trim().is_empty() { None } else { Some(m.alias.clone()) })
            .unwrap_or_else(|| route_id.clone());
        let mut save = false;
        let mut cancel = false;
        let mut header_cancel = false;
        let mut new_order = self.model_priority_order.clone();
        let dialog_size = fit_dialog_size(
            ctx.content_rect().size(),
            egui::vec2(560.0, 420.0),
            egui::vec2(420.0, 320.0),
        );
        egui::Window::new("")
            .id(egui::Id::new("model-priority-dialog"))
            .title_bar(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .default_size(dialog_size)
            .min_size(dialog_size)
            .max_size(dialog_size)
            .collapsible(false)
            .resizable(false)
            .frame(theme::dialog_window_frame())
            .show(ctx, |ui| {
                ui.set_width(dialog_size.x);
                let mut reorder = None;
                theme::dialog_shell(
                    ui,
                    palette,
                    |ui| {
                        ui.horizontal(|ui| {
                            theme::dialog_title(ui, &format!("{} - {}", t(zh, "优先级", "Priority"), display_name));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new("×")
                                                .size(18.0)
                                                .color(egui::Color32::WHITE),
                                        )
                                        .fill(egui::Color32::TRANSPARENT)
                                        .stroke(egui::Stroke::NONE),
                                    )
                                    .clicked()
                                {
                                    header_cancel = true;
                                }
                            });
                        });
                        ui.label(
                            egui::RichText::new(t(
                                zh,
                                "拖动左侧手柄调整同名模型的调用顺序，订阅默认靠前",
                                "Drag the left handle to reorder same-name models; subscription first by default",
                            ))
                            .color(egui::Color32::from_white_alpha(215))
                            .small(),
                        );
                    },
                    |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("model-priority-list")
                            .max_height((dialog_size.y - 140.0).max(160.0))
                            .show(ui, |ui| {
                                for (pos, &idx) in new_order.clone().iter().enumerate() {
                                    let model = match self.config.models.get(idx) {
                                        Some(m) => m.clone(),
                                        None => continue,
                                    };
                                    let is_oauth = model.source == "oauth";
                                    let (source_label, endpoint_short, endpoint_detail) =
                                        priority_endpoint_labels(&model, &self.oauth_accounts, zh);
                                    let card = egui::Frame::new()
                                        .fill(palette.paper_alt)
                                        .stroke(egui::Stroke::new(1.0, palette.line))
                                        .corner_radius(egui::CornerRadius::same(8))
                                        .inner_margin(egui::Margin::symmetric(10, 8))
                                        .show(ui, |ui| {
                                            ui.set_min_width(ui.available_width());
                                            ui.horizontal_wrapped(|ui| {
                                                let drag_id = ui.make_persistent_id((
                                                    "model-priority-drag",
                                                    &route_id,
                                                    idx,
                                                ));
                                                let drag_source = ui.dnd_drag_source(
                                                    drag_id,
                                                    ModelOrderDrag { source_index: idx },
                                                    |ui| {
                                                        egui::Frame::new()
                                                            .fill(palette.paper)
                                                            .stroke(egui::Stroke::new(
                                                                1.0,
                                                                palette.line,
                                                            ))
                                                            .corner_radius(
                                                                egui::CornerRadius::same(4),
                                                            )
                                                            .inner_margin(egui::Margin::symmetric(
                                                                8, 3,
                                                            ))
                                                            .show(ui, |ui| {
                                                                ui.label(
                                                                    egui::RichText::new("≡")
                                                                        .size(16.0)
                                                                        .color(palette.ink_soft),
                                                                );
                                                            });
                                                    },
                                                );
                                                drag_source.response.on_hover_text(t(
                                                    zh,
                                                    "按住并拖到另一条渠道上排序",
                                                    "Hold and drag onto another channel to reorder",
                                                ));
                                                ui.label(
                                                    egui::RichText::new(format!("{:02}", pos + 1))
                                                        .strong()
                                                        .color(palette.accent),
                                                );
                                                ui.label(
                                                    egui::RichText::new(format!("P{}", model.priority))
                                                        .small()
                                                        .color(palette.muted),
                                                );
                                                theme::pill(
                                                    ui,
                                                    &source_label,
                                                    palette.paper,
                                                    if is_oauth {
                                                        palette.success
                                                    } else {
                                                        palette.muted
                                                    },
                                                );
                                                ui.add(
                                                    egui::Label::new(
                                                        egui::RichText::new(&endpoint_short)
                                                            .small()
                                                            .strong()
                                                            .color(palette.ink_soft),
                                                    )
                                                    .wrap(),
                                                )
                                                .on_hover_text(&endpoint_detail);
                                            });
                                        });
                                    let response = card.response;
                                    if let Some(payload) = response.dnd_hover_payload::<ModelOrderDrag>() {
                                        if payload.source_index != idx {
                                            ui.painter().rect_stroke(
                                                response.rect,
                                                egui::CornerRadius::same(8),
                                                egui::Stroke::new(2.0, palette.action),
                                                egui::StrokeKind::Outside,
                                            );
                                            if let Some(source_pos) = new_order
                                                .iter()
                                                .position(|&i| i == payload.source_index)
                                            {
                                                if source_pos != pos {
                                                    reorder = Some((source_pos, pos));
                                                }
                                            }
                                        }
                                    }
                                }
                                if let Some((from, to)) = reorder.take() {
                                    let item = new_order.remove(from);
                                    new_order.insert(to, item);
                                }
                            });
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if theme::primary_button(
                                ui,
                                egui::RichText::new(t(zh, "保存", "Save"))
                                    .strong()
                                    .color(egui::Color32::WHITE),
                                palette,
                            )
                            .clicked()
                            {
                                save = true;
                            }
                            if theme::secondary_button(ui, t(zh, "取消", "Cancel"), palette).clicked() {
                                cancel = true;
                            }
                        });
                    },
                );
            });
        // Keep the released drag across frames so a later Save uses the order
        // the user can see instead of cloning the pre-drag order again.
        self.model_priority_order = new_order;
        if save {
            let order = self.model_priority_order.clone();
            let route_id = self.model_priority_dialog_target.clone().unwrap_or_default();
            let changed = apply_model_priority_order(&mut self.config, &route_id, &order);
            if changed && !self.ui_audit_mode {
                let _ = self
                    .config
                    .save(&crate::user_data::config_path(&self.router_root));
                let _ = super::logic::write_model_catalog(&self.config, &self.router_root);
            }
            self.status_text = if changed {
                t(
                    zh,
                    "同名模型优先级已保存；点击“保存并应用”后生效",
                    "Same-model priority saved. Save & apply to activate it.",
                )
            } else {
                t(
                    zh,
                    "优先级顺序已变化，请重新打开弹窗后再试",
                    "The priority set changed. Reopen the dialog and try again.",
                )
            }
            .to_owned();
            self.model_priority_dialog_target = None;
            self.model_priority_order.clear();
        } else if cancel || header_cancel {
            self.model_priority_dialog_target = None;
            self.model_priority_order.clear();
        }
    }

    #[allow(dead_code)]
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
                    .corner_radius(egui::CornerRadius::same(14))
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
                    .fill(palette.background_dark)
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
                    .fill(palette.paper)
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
        if window.kind == "sharedPool" {
            return match window.display_name.as_str() {
                "Gemini shared quota" => t(zh, "Gemini 共享额度", "Gemini shared quota"),
                "Claude shared quota" => t(zh, "Claude 共享额度", "Claude shared quota"),
                "GPT shared quota" => t(zh, "GPT 共享额度", "GPT shared quota"),
                "Claude / GPT shared quota" => {
                    t(zh, "Claude / GPT 共享额度", "Claude / GPT shared quota")
                }
                _ => t(zh, "Antigravity 共享额度", "Antigravity shared quota"),
            }
            .to_owned();
        }
        if !window.display_name.trim().is_empty() {
            return window.display_name.clone();
        }
        match window.kind.as_str() {
            "fiveHour" => t(zh, "5 小时额度", "5-hour limit").to_owned(),
            "daily" => t(zh, "每日额度", "Daily limit").to_owned(),
            "weekly" => t(zh, "周额度", "Weekly limit").to_owned(),
            "monthly" => t(zh, "月额度", "Monthly limit").to_owned(),
            "model" => t(zh, "模型额度", "Model limit").to_owned(),
            "balance" => t(zh, "账户余额", "Account balance").to_owned(),
            _ => t(zh, "其他额度窗口", "Other quota window").to_owned(),
        }
    }

    fn usage_amount(value: f64, currency: &str) -> String {
        let currency = currency.trim().to_uppercase();
        if currency == "USD" || currency.is_empty() {
            format!("${value:.2}")
        } else {
            format!("{currency} {value:.2}")
        }
    }

    fn usage_window_is_readable(window: &UsageWindow) -> bool {
        if window.kind == "balance" {
            window.remaining_amount.is_some()
        } else {
            remaining_quota_percent(window.used_percent).is_some()
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
        if window.kind == "balance" {
            let remaining = window.remaining_amount.unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(label).strong().color(palette.ink));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(Self::usage_amount(remaining, &window.currency))
                            .strong()
                            .color(palette.ink_soft),
                    );
                });
            });
            if let Some(used_percent) = window.used_percent {
                let progress = (100.0 - used_percent.clamp(0.0, 100.0)) / 100.0;
                let (bar_rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 7.0),
                    egui::Sense::hover(),
                );
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
                        if progress <= 0.05 {
                            palette.danger
                        } else {
                            palette.action
                        },
                    );
                }
            }
            let details = match (window.used_amount, window.limit_amount) {
                (Some(used), Some(limit)) => Some(format!(
                    "{} {} · {} {}",
                    t(zh, "已用", "Used"),
                    Self::usage_amount(used, &window.currency),
                    t(zh, "总额", "Total"),
                    Self::usage_amount(limit, &window.currency)
                )),
                (Some(used), None) => Some(format!(
                    "{} {}",
                    t(zh, "已用", "Used"),
                    Self::usage_amount(used, &window.currency)
                )),
                _ => None,
            };
            if let Some(details) = details {
                ui.label(egui::RichText::new(details).small().color(palette.muted));
            }
            return;
        }
        let reset = Self::usage_reset_label(window, zh);
        let remaining = remaining_quota_percent(window.used_percent);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(egui::RichText::new(label).small().strong().color(palette.ink));
            ui.label(
                egui::RichText::new(match remaining {
                    Some(value) if zh => format!("{value:.0}%"),
                    Some(value) => format!("{value:.0}%"),
                    None => "—".to_owned(),
                })
                .small()
                .strong()
                .color(palette.ink_soft),
            );
            let bar_width = (ui.available_width() - 132.0).clamp(56.0, 220.0);
            let progress = remaining.unwrap_or(0.0) / 100.0;
            let (bar_rect, _) =
                ui.allocate_exact_size(egui::vec2(bar_width, 6.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(bar_rect, egui::CornerRadius::same(3), palette.paper_alt);
            if progress > 0.0 {
                let fill_rect = egui::Rect::from_min_size(
                    bar_rect.min,
                    egui::vec2(bar_rect.width() * progress, bar_rect.height()),
                );
                ui.painter().rect_filled(
                    fill_rect,
                    egui::CornerRadius::same(3),
                    if remaining.unwrap_or(0.0) <= 5.0 {
                        palette.danger
                    } else {
                        palette.action
                    },
                );
            }
            ui.label(egui::RichText::new(reset).small().color(palette.muted));
            if window.tokens > 0 || window.requests > 0 {
                ui.label(
                    egui::RichText::new(format!(
                        "{}·{}",
                        Self::compact_number(window.tokens),
                        window.requests
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
            .inner_margin(egui::Margin::symmetric(8, 5))
            .shadow(theme::soft_card_shadow())
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.set_min_width(ui.available_width());
                ui.set_max_width(ui.available_width());
                let drag_handle = ui
                    .horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.set_max_width((ui.available_width() - 96.0).max(120.0));
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&account.name)
                                        .font(egui::FontId::new(14.0, theme::display_family()))
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
                ui.label(
                    egui::RichText::new(format!(
                        "{} tok · {} {} · ${:.2}",
                        Self::compact_number(account.totals.total_tokens),
                        account.totals.requests,
                        t(zh, "次", "req"),
                        account.totals.cost
                    ))
                    .small()
                    .color(palette.ink_soft),
                );

                if subscription || !account.windows.is_empty() {
                    let readable_windows = account
                        .windows
                        .iter()
                        .filter(|window| Self::usage_window_is_readable(window))
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
                                ui.add_space(4.0);
                            }
                            Self::show_usage_window(ui, window, palette, zh);
                        }
                    }
                } else if !account.totals.models.is_empty() {
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
                    ui.label(
                        egui::RichText::new(&account.status_detail)
                            .small()
                            .color(palette.danger),
                    );
                }
                if !account.query_note.trim().is_empty() {
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
            || account.windows.iter().any(|window| {
                matches!(
                    window.kind.as_str(),
                    "fiveHour" | "daily" | "weekly" | "monthly" | "other"
                )
            })
    }

    fn usage_plan_group_key(account: &UsageAccount) -> String {
        let platform = if account.platform.trim().is_empty() {
            account
                .name
                .split(['/', '·', '-'])
                .next()
                .unwrap_or("plan")
                .trim()
                .to_ascii_lowercase()
        } else {
            account.platform.trim().to_ascii_lowercase()
        };
        let platform = subscription_provider_key(&platform);
        if account.kind == "oauth" {
            format!("oauth:{platform}")
        } else if Self::usage_account_is_quota_plan(account) {
            format!("plan:{platform}")
        } else {
            format!("id:{}", account.id)
        }
    }

    fn group_usage_accounts<'a>(accounts: &[&'a UsageAccount]) -> Vec<Vec<&'a UsageAccount>> {
        let mut groups: Vec<(String, Vec<&'a UsageAccount>)> = Vec::new();
        for account in accounts {
            let key = Self::usage_plan_group_key(account);
            if let Some((_, group)) = groups.iter_mut().find(|(existing, _)| existing == &key) {
                group.push(*account);
            } else {
                groups.push((key, vec![*account]));
            }
        }
        groups.into_iter().map(|(_, accounts)| accounts).collect()
    }

    fn usage_token_breakdown(account: &UsageAccount) -> (i64, i64, i64) {
        let mut input = 0;
        let mut output = 0;
        let mut cache_read = 0;
        for model in &account.totals.models {
            input += model.input_tokens;
            output += model.output_tokens;
            cache_read += model.cache_read_tokens;
        }
        (input, output, cache_read)
    }

    fn usage_cache_hit_rate(input: i64, cache_read: i64) -> f32 {
        let denom = input + cache_read;
        if denom <= 0 {
            0.0
        } else {
            cache_read as f32 / denom as f32 * 100.0
        }
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
        let gap = 6.0;
        let grouped = Self::group_usage_accounts(accounts);
        // Edge gutter keeps the right column off the scrollbar; never force a
        // floor width that would overflow and wrap into a blank second row.
        let edge_gutter = 8.0;
        let usable_width = (ui.available_width() - edge_gutter).max(0.0);
        let columns = if two_columns && usable_width >= 560.0 {
            2usize
        } else {
            1usize
        };
        let assignments = usage_column_indices(grouped.len(), columns);
        ui.allocate_ui_with_layout(
            egui::vec2(usable_width, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.columns(columns, |column_uis| {
                    for (column, indices) in column_uis.iter_mut().zip(assignments) {
                        for (position, index) in indices.into_iter().enumerate() {
                            if position > 0 {
                                column.add_space(gap);
                            }
                            let group = &grouped[index];
                            let account = group[0];
                            let account_section = if account.kind == "oauth" {
                                UsageOrderSection::Subscription
                            } else {
                                UsageOrderSection::Api
                            };
                            let (card, handle) = if group.len() == 1 {
                                Self::show_usage_account(
                                    column,
                                    account,
                                    palette,
                                    zh,
                                    subscription || account.kind == "oauth",
                                )
                            } else {
                                Self::show_usage_plan_group(
                                    column,
                                    group,
                                    palette,
                                    zh,
                                    subscription,
                                )
                            };
                            handle.dnd_set_drag_payload(UsageOrderDrag {
                                section: account_section,
                                account_id: account.id,
                            });
                            if let Some(payload) = card.dnd_hover_payload::<UsageOrderDrag>() {
                                if payload.section == account_section
                                    && payload.account_id != account.id
                                {
                                    column.painter().rect_stroke(
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
                        }
                    }
                });
            },
        );
        usage_reorder
    }

    fn show_usage_plan_group(
        ui: &mut egui::Ui,
        accounts: &[&UsageAccount],
        palette: &theme::Palette,
        zh: bool,
        subscription: bool,
    ) -> (egui::Response, egui::Response) {
        let title = accounts
            .first()
            .map(|account| {
                if account.kind == "oauth" {
                    subscription_provider_title(&account.platform).to_owned()
                } else if account.platform.trim().is_empty() {
                    account.name.clone()
                } else {
                    account.platform.clone()
                }
            })
            .unwrap_or_else(|| t(zh, "套餐", "Plan").to_owned());
        let card = egui::Frame::new()
            .fill(palette.paper)
            .stroke(egui::Stroke::new(1.0, palette.line))
            .corner_radius(egui::CornerRadius::same(7))
            .inner_margin(egui::Margin::symmetric(8, 5))
            .shadow(theme::soft_card_shadow())
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.set_min_width(ui.available_width());
                let drag_handle = ui
                    .horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} · {} {}",
                                title,
                                accounts.len(),
                                t(zh, "个账号", "accounts")
                            ))
                            .font(egui::FontId::new(14.0, theme::display_family()))
                            .color(palette.ink),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add(
                                egui::Button::new(
                                    egui::RichText::new("≡").size(16.0).color(palette.ink_soft),
                                )
                                .fill(palette.paper_alt)
                                .stroke(egui::Stroke::new(1.0, palette.line))
                                .corner_radius(egui::CornerRadius::same(5))
                                .sense(egui::Sense::drag()),
                            )
                        })
                        .inner
                    })
                    .inner;
                for (index, account) in accounts.iter().enumerate() {
                    if index > 0 {
                        ui.separator();
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(
                            egui::RichText::new(&account.name)
                                .small()
                                .strong()
                                .color(palette.ink),
                        );
                        ui.label(
                            egui::RichText::new(format!("#{} · {}", account.id, account.status))
                                .small()
                                .color(palette.muted),
                        );
                        let (health, health_color) = Self::usage_health(account, palette, zh);
                        theme::pill(ui, health, palette.paper_alt, health_color);
                        ui.label(
                            egui::RichText::new(format!(
                                "{} tok · {} {} · ${:.2}",
                                Self::compact_number(account.totals.total_tokens),
                                account.totals.requests,
                                t(zh, "次", "req"),
                                account.totals.cost
                            ))
                            .small()
                            .color(palette.ink_soft),
                        );
                    });
                    if !account.status_detail.trim().is_empty() {
                        ui.label(
                            egui::RichText::new(&account.status_detail)
                                .small()
                                .color(palette.danger),
                        );
                    }
                    if !account.query_note.trim().is_empty() {
                        ui.label(
                            egui::RichText::new(&account.query_note)
                                .small()
                                .color(palette.muted),
                        );
                    }
                    if subscription || !account.windows.is_empty() {
                        for window in account
                            .windows
                            .iter()
                            .filter(|window| Self::usage_window_is_readable(window))
                            .take(3)
                        {
                            Self::show_usage_window(ui, window, palette, zh);
                        }
                    }
                }
                drag_handle
            });
        (card.response, card.inner)
    }

    fn usage_account_for_model<'a>(
        snapshot: &'a UsageSnapshot,
        model: &ModelConfig,
    ) -> Option<&'a UsageAccount> {
        if model.source.eq_ignore_ascii_case("oauth") && model.oauth_account_id > 0 {
            return snapshot
                .subscriptions
                .iter()
                .find(|account| account.id == model.oauth_account_id);
        }
        let hay = format!(
            "{} {} {} {}",
            model.model, model.alias, model.base_url, model.oauth_platform
        )
        .to_ascii_lowercase();
        let alias = model.alias.trim().to_ascii_lowercase();
        snapshot
            .api_channels
            .iter()
            .chain(snapshot.subscriptions.iter())
            .find(|account| {
                let name = account.name.to_ascii_lowercase();
                let platform = account.platform.trim().to_ascii_lowercase();
                if !alias.is_empty() && name.contains(&alias) {
                    return true;
                }
                if !platform.is_empty()
                    && platform.len() >= 3
                    && (hay.contains(&platform)
                        || (platform == "chiral" && hay.contains("430123"))
                        || (platform.contains("openai") && hay.contains("gpt")))
                {
                    return true;
                }
                [
                    "kimi",
                    "deepseek",
                    "glm",
                    "zhipu",
                    "ark",
                    "volcengine",
                    "openrouter",
                    "mimo",
                    "moonshot",
                    "grok",
                    "xai",
                    "x-ai",
                    "gemini",
                    "claude",
                    "chiral",
                    "430123",
                ]
                .iter()
                .any(|keyword| {
                    hay.contains(keyword)
                        && (name.contains(keyword)
                            || platform.contains(keyword)
                            || (keyword == &"grok"
                                && (name.contains("xai") || platform.contains("xai")))
                            || (keyword == &"xai" && (name.contains("grok") || hay.contains("grok"))))
                })
            })
    }

    fn usage_for_model_row<'a>(
        snapshot: &'a UsageSnapshot,
        cfg: &RouterConfig,
        models: &[ModelConfig],
        representative: &ModelConfig,
    ) -> Option<(&'a UsageAccount, bool)> {
        let related = models
            .iter()
            .filter(|candidate| {
                super::logic::same_model_identity(&candidate.model, &representative.model)
            })
            .collect::<Vec<_>>();
        let oauth_ids = related
            .iter()
            .filter(|model| model.source == "oauth" && model.oauth_account_id > 0)
            .map(|model| model.oauth_account_id)
            .collect::<Vec<_>>();
        let mut oauth_accounts = snapshot
            .subscriptions
            .iter()
            .filter(|account| oauth_ids.contains(&account.id))
            .collect::<Vec<_>>();
        if oauth_accounts.is_empty() {
            oauth_accounts = snapshot
                .subscriptions
                .iter()
                .filter(|account| {
                    account.kind == "oauth"
                        && related.iter().any(|model| {
                            model.source == "oauth"
                                && Self::usage_account_for_model(snapshot, model)
                                    .is_some_and(|matched| matched.id == account.id)
                        })
                })
                .collect();
        }
        if oauth_accounts.is_empty() && representative.source == "oauth" {
            if let Some(account) = Self::usage_account_for_model(snapshot, representative) {
                if account.kind == "oauth" || Self::usage_account_is_quota_plan(account) {
                    oauth_accounts.push(account);
                }
            }
        }
        let oauth = oauth_accounts
            .iter()
            .copied()
            .find(|account| {
                account
                    .windows
                    .iter()
                    .any(Self::usage_window_is_readable)
            })
            .or_else(|| oauth_accounts.first().copied());
        let api = related
            .iter()
            .filter(|model| model.source != "oauth")
            .find_map(|model| Self::usage_account_for_model(snapshot, model))
            .or_else(|| {
                if representative.source != "oauth" {
                    Self::usage_account_for_model(snapshot, representative)
                } else {
                    None
                }
            });
        let coding_plan = related.iter().any(|model| {
            super::logic::classify_channel_route(model).source_type
                == super::logic::ChannelSourceType::CodingPlan
        }) || super::logic::classify_channel_route(representative)
            .source_type
            == super::logic::ChannelSourceType::CodingPlan;
        let prefer_api =
            super::logic::model_route_policy(cfg, &representative.model)
                == super::logic::ModelRoutePolicy::ApiFirst;
        let shows_quota = |account: &UsageAccount| {
            coding_plan || Self::usage_account_is_quota_plan(account)
        };
        if prefer_api {
            if let Some(account) = api {
                return Some((account, shows_quota(account)));
            }
            return oauth.map(|account| (account, true));
        }
        if let Some(account) = oauth {
            if account
                .windows
                .iter()
                .any(Self::usage_window_is_readable)
            {
                return Some((account, true));
            }
        }
        if let Some(account) = api {
            return Some((account, shows_quota(account)));
        }
        oauth.map(|account| (account, true))
    }

    fn usage_window_rank(kind: &str) -> u8 {
        match kind {
            "fiveHour" => 0,
            "daily" => 1,
            "weekly" => 2,
            "monthly" => 3,
            "other" => 4,
            "balance" => 5,
            _ => 9,
        }
    }

    fn smallest_readable_quota_window(account: &UsageAccount) -> Option<&UsageWindow> {
        account
            .windows
            .iter()
            .filter(|window| {
                window.kind != "model" && Self::usage_window_is_readable(window)
            })
            .min_by_key(|window| Self::usage_window_rank(&window.kind))
    }

    fn show_model_row_usage(
        ui: &mut egui::Ui,
        account: Option<(&UsageAccount, bool)>,
        palette: &theme::Palette,
        zh: bool,
    ) {
        let Some((account, show_quota)) = account else {
            ui.label(
                egui::RichText::new(t(zh, "暂无用量", "No usage"))
                    .small()
                    .color(palette.muted),
            );
            return;
        };
        ui.spacing_mut().item_spacing.x = 10.0;
        if show_quota {
            if let Some(window) = Self::smallest_readable_quota_window(account) {
                if window.kind == "balance" {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} {}",
                            t(zh, "余额", "Balance"),
                            Self::usage_amount(
                                window.remaining_amount.unwrap_or_default(),
                                &window.currency
                            )
                        ))
                        .small()
                        .strong()
                        .color(palette.ink_soft),
                    );
                } else {
                    let remaining = remaining_quota_percent(window.used_percent);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(
                            egui::RichText::new(format!(
                                "{} {}",
                                Self::usage_window_label(window, zh),
                                match remaining {
                                    Some(value) => format!("{value:.0}%"),
                                    None => "—".to_owned(),
                                }
                            ))
                            .small()
                            .strong()
                            .color(palette.ink_soft),
                        );
                        let progress = remaining.unwrap_or(0.0) / 100.0;
                        let (bar_rect, _) =
                            ui.allocate_exact_size(egui::vec2(56.0, 6.0), egui::Sense::hover());
                        ui.painter().rect_filled(
                            bar_rect,
                            egui::CornerRadius::same(3),
                            palette.paper_alt,
                        );
                        if progress > 0.0 {
                            ui.painter().rect_filled(
                                egui::Rect::from_min_size(
                                    bar_rect.min,
                                    egui::vec2(bar_rect.width() * progress, bar_rect.height()),
                                ),
                                egui::CornerRadius::same(3),
                                if remaining.unwrap_or(0.0) <= 5.0 {
                                    palette.danger
                                } else {
                                    palette.action
                                },
                            );
                        }
                    });
                }
            } else {
                ui.label(
                    egui::RichText::new(t(zh, "暂无额度", "No quota yet"))
                        .small()
                        .color(palette.muted),
                )
                .on_hover_text(if account.query_note.trim().is_empty() {
                    t(
                        zh,
                        "订阅额度尚未返回。打开实时用量或点刷新后再看。",
                        "Subscription quota is not back yet. Open Live usage or refresh.",
                    )
                    .to_owned()
                } else {
                    account.query_note.clone()
                });
            }
            return;
        }
        let (input, output, cache_read) = Self::usage_token_breakdown(account);
        let input = if input > 0 {
            input
        } else {
            account.totals.total_tokens
        };
        let hit = Self::usage_cache_hit_rate(input, cache_read);
        ui.label(
            egui::RichText::new(format!(
                "{} {}  {} {}  {} {}  {} {:.0}%",
                t(zh, "入", "in"),
                Self::compact_number(input),
                t(zh, "出", "out"),
                Self::compact_number(output),
                t(zh, "缓存", "cache"),
                Self::compact_number(cache_read),
                t(zh, "命中", "hit"),
                hit
            ))
            .small()
            .color(palette.ink_soft),
        );
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
                        .font(egui::FontId::new(24.0, theme::display_family()))
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
                    self.trigger_self_check();
                }
            });
        });
        ui.add_space(6.0);

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
        let narrow_header = header_width < 900.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(if narrow_header { 4.0 } else { 6.0 }, 6.0);
            ui.allocate_ui_with_layout(
                egui::vec2(
                    if narrow_header {
                        if zh {
                            118.0
                        } else {
                            126.0
                        }
                    } else {
                        if zh {
                            158.0
                        } else {
                            172.0
                        }
                    },
                    54.0,
                ),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    theme::eyebrow(ui, t(zh, "本地模型控制", "LOCAL ROUTER"), palette.paper);
                    ui.label(
                        egui::RichText::new(if zh {
                            "路由控制台"
                        } else {
                            "ROUTER CONSOLE"
                        })
                        .font(egui::FontId::new(
                            if narrow_header { 22.0 } else { 25.0 },
                            theme::display_family(),
                        ))
                        .color(egui::Color32::WHITE),
                    );
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let button = |ui: &mut egui::Ui, width: f32, label: &str| {
                    ui.add_sized(
                        [width, 40.0],
                        egui::Button::new(egui::RichText::new(label).size(13.0).strong())
                            .fill(palette.paper)
                            .stroke(egui::Stroke::new(1.0, palette.line))
                            .corner_radius(egui::CornerRadius::same(7)),
                    )
                };
                let oauth_count = self.config.oauth_account_ids.as_ref().map_or(0, Vec::len);
                if ui
                    .add_sized(
                        [if narrow_header { 88.0 } else { 122.0 }, 40.0],
                        egui::Button::new(
                            egui::RichText::new(if narrow_header {
                                format!("{} ({oauth_count})", t(zh, "订阅", "Plans"))
                            } else {
                                format!(
                                    "{} ({oauth_count})",
                                    t(zh, "当前配置订阅", "Profile plans")
                                )
                            })
                            .size(13.0)
                            .strong()
                            .color(egui::Color32::WHITE),
                        )
                        .fill(palette.action)
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(egui::CornerRadius::same(7)),
                    )
                    .on_hover_text(t(
                        zh,
                        "管理当前配置的订阅账号、模型和回退策略",
                        "Manage subscription accounts, models, and fallback policy for this profile",
                    ))
                    .clicked()
                {
                    self.open_oauth_manager();
                }
                if button(
                    ui,
                    if narrow_header { 86.0 } else { 116.0 },
                    if narrow_header {
                        t(zh, "配置分组", "Groups")
                    } else {
                        t(zh, "切换配置分组", "Switch groups")
                    },
                )
                .clicked()
                {
                    self.open_profiles();
                }
                if button(
                    ui,
                    if narrow_header { 88.0 } else { 126.0 },
                    if narrow_header {
                        t(zh, "常见渠道", "Providers")
                    } else {
                        t(zh, "常见渠道快速配置", "Provider setup")
                    },
                )
                .on_hover_text(t(
                    zh,
                    "快速添加常见 API 渠道",
                    "Quickly add a common API provider",
                ))
                .clicked()
                {
                    self.channel_preset_dialog_open = true;
                }
                if button(
                    ui,
                    if narrow_header { 78.0 } else { 116.0 },
                    if narrow_header {
                        t(zh, "用量", "Usage")
                    } else {
                        t(zh, "实时用量统计", "Live usage")
                    },
                )
                .on_hover_text(t(
                    zh,
                    "查看订阅额度、重置时间与 API token 用量",
                    "View subscription quotas, reset times, and API token usage",
                ))
                .clicked()
                {
                    self.open_usage_monitor();
                }
            });
        });
        ui.add_space(8.0);
        let panel_height = ui.available_height();
        let wide = dashboard_uses_wide_layout(ui.available_width(), panel_height);
        if wide {
            let width = ui.available_width();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(width * 0.25, panel_height.max(1.0)),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| self.dashboard_sidebar(ui, palette, panel_height),
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

    fn share_session_toggle_button(
        &mut self,
        ui: &mut egui::Ui,
        palette: &theme::Palette,
        zh: bool,
    ) {
        let clicked = egui::Frame::new()
            .fill(palette.paper)
            .stroke(egui::Stroke::new(1.0, palette.line))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::symmetric(12, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(t(zh, "共享配置", "Shared config"))
                            .strong()
                            .color(palette.ink),
                    );
                    ui.add_space(10.0);
                    Self::dashboard_route_switch(ui, self.share_codex_state, false, palette)
                        .on_hover_text(t(
                            zh,
                            "共享配置：开=同一 Codex 账号在配置间共用会话和设置；关=各配置独立快照。",
                            "Shared config: on keeps tasks and settings for the same Codex account; off restores each profile snapshot.",
                        ))
                        .clicked()
                })
                .inner
            })
            .inner;
        if clicked {
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
                bottom: 0,
            })
            .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            let visible_height = (ui.clip_rect().bottom() - ui.cursor().top()).max(0.0);
            let aligned = dashboard_sidebar_inner_height(target_height.min(visible_height));
            ui.set_min_height(aligned);
            ui.set_max_height(aligned);
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
        let dashboard_bottom_space = 4.0;
        let visible_height = (ui.clip_rect().bottom() - ui.cursor().top()).max(0.0);
        let layout_height = target_height.min(visible_height);
        // Give the model list all remaining height above the compact log and
        // footer gap so large windows do not end with an unused blank band.
        let (model_content_height, list_height, log_content_height) =
            dashboard_panel_heights(layout_height);
        theme::glass_frame(palette)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height(model_content_height);
            ui.horizontal(|ui| {
                theme::eyebrow(
                    ui,
                    t(zh, "模型渠道", "MODEL CHANNELS"),
                    palette.background_dark,
                );
                ui.label(
                    egui::RichText::new(t(zh, "路由配置", "Router configuration"))
                        .font(egui::FontId::new(18.0, theme::display_family()))
                        .color(palette.ink),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_sized(
                            [if zh { 116.0 } else { 126.0 }, 36.0],
                            egui::Button::new(
                                egui::RichText::new(t(zh, "＋ 添加新模型", "＋ Add model"))
                                    .size(13.0)
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(palette.action)
                            .stroke(egui::Stroke::NONE)
                            .corner_radius(egui::CornerRadius::same(7)),
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
                    let apply = ui.add_enabled_ui(!self.applying, |ui| {
                        ui.add_sized(
                            [if zh { 102.0 } else { 110.0 }, 36.0],
                            egui::Button::new(
                                egui::RichText::new(t(zh, "保存并应用", "Save & apply"))
                                    .size(13.0)
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(palette.background_dark)
                            .stroke(egui::Stroke::NONE)
                            .corner_radius(egui::CornerRadius::same(7)),
                        )
                    });
                    if apply.inner.clicked() {
                        self.apply_all();
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
            ui.add_space(3.0);
            let mut edit = None;
            let mut delete = None;
            let mut set_default = None;
            let mut reorder = None;
            let mut configure = None;
            let mut priority_dialog = None;
            let current_default = super::logic::resolve_default_model(&self.config)
                .unwrap_or_default()
                .to_owned();
            let route_plan = super::logic::catalog::build_route_plan(&self.config);
            let usage_snapshot = self.usage_snapshot.clone();
            if !self.config.models.is_empty() {
                egui::ScrollArea::vertical()
                    .id_salt("dashboard-model-list-scroll")
                    .max_height(list_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                    for (display_index, row) in super::logic::dashboard_model_rows(&self.config.models)
                        .into_iter()
                        .enumerate()
                    {
                        let index = row.index;
                        let account_count = row.account_count;
                        let Some(model) = self.config.models.get(index) else {
                            continue;
                        };
                        let vision = super::logic::resolve_multimodal(model);
                        let route = route_plan.iter().find(|route| route.index == index);
                        let route_id = route
                            .map(|route| route.public_model_id.as_str())
                            .unwrap_or(model.model.as_str());
                        let is_default = route.is_some_and(|route| {
                            route.include_in_catalog && route.public_model_id == current_default
                        });
                        let response = egui::Frame::new()
                            .fill(palette.paper)
                            .stroke(egui::Stroke::new(1.0_f32, palette.line))
                            .shadow(theme::soft_card_shadow())
                            .inner_margin(egui::Margin::symmetric(12, 6))
                            .show(ui, |ui| {
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 2.0;
                                    ui.horizontal(|ui| {
                                        let action_width = if zh { 268.0 } else { 318.0 };
                                        let title_width =
                                            (ui.available_width() - action_width - 8.0).max(120.0);
                                        let model_label = if model.alias.is_empty() {
                                            &model.model
                                        } else {
                                            &model.alias
                                        };
                                        ui.allocate_ui_with_layout(
                                            egui::vec2(title_width, 32.0),
                                            egui::Layout::left_to_right(egui::Align::Center),
                                            |ui| {
                                                ui.label(
                                                    egui::RichText::new(format!("{:02}", display_index + 1))
                                                        .font(egui::FontId::new(
                                                            21.0,
                                                            theme::display_family(),
                                                        ))
                                                        .color(palette.accent),
                                                );
                                                ui.add(
                                                    egui::Label::new(
                                                        egui::RichText::new(model_label)
                                                            .font(egui::FontId::new(
                                                                18.0,
                                                                theme::display_family(),
                                                            ))
                                                            .color(palette.ink)
                                                            .strong(),
                                                    )
                                                    .truncate(),
                                                )
                                                .on_hover_text(model_label);
                                            },
                                        );
                                        ui.allocate_ui_with_layout(
                                            egui::vec2(ui.available_width().max(action_width), 32.0),
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
                                                if account_count > 1
                                                    && ui
                                                        .small_button(t(zh, "优先级", "Priority"))
                                                        .on_hover_text(t(
                                                            zh,
                                                            "同名模型的调用顺序，订阅默认靠前，可拖动自定义",
                                                            "Call order for same-name models; subscription first by default, drag to reorder",
                                                        ))
                                                        .clicked()
                                                {
                                                    priority_dialog = Some(route_id.to_owned());
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
                                                    let label = if model.alias.trim().is_empty() {
                                                        model.model.clone()
                                                    } else {
                                                        model.alias.clone()
                                                    };
                                                    set_default =
                                                        Some((route_id.to_owned(), label));
                                                }
                                            },
                                        );
                                    });
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
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
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    egui::RichText::new(
                                                        super::logic::model_route_chip(
                                                            &self.config,
                                                            model,
                                                            account_count,
                                                            zh,
                                                        ),
                                                    )
                                                    .small()
                                                    .strong()
                                                    .color(palette.action),
                                                )
                                                .fill(palette.background_light)
                                                .stroke(egui::Stroke::new(1.0, palette.line))
                                                .corner_radius(egui::CornerRadius::same(16))
                                                .min_size(egui::vec2(0.0, 22.0)),
                                            )
                                            .on_hover_text(t(
                                                zh,
                                                "数字是可调用渠道数。点击选择：优先订阅、优先 API，或仅订阅",
                                                "The number is callable channels. Click to choose subscription first, API first, or subscription only",
                                            ))
                                            .clicked()
                                        {
                                            configure = Some(index);
                                        }
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
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                let account = usage_snapshot.as_ref().and_then(
                                                    |snapshot| {
                                                        Self::usage_for_model_row(
                                                            snapshot,
                                                            &self.config,
                                                            &self.config.models,
                                                            model,
                                                        )
                                                    },
                                                );
                                                Self::show_model_row_usage(
                                                    ui, account, palette, zh,
                                                );
                                            },
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
                                let catalog_note =
                                    match super::logic::write_model_catalog(&self.config, &self.router_root)
                                    {
                                        Ok(()) => t(
                                            zh,
                                            "路由模型顺序已保存，并已同步写入 Codex 目录",
                                            "Model order saved and written to the Codex catalog",
                                        ),
                                        Err(_) => t(
                                            zh,
                                            "路由模型顺序已保存；Codex 目录将在下次保存并应用时更新",
                                            "Model order saved; the Codex catalog updates on Save & apply",
                                        ),
                                    };
                                self.status_text = catalog_note.to_owned();
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
            if let Some((model, label)) = set_default {
                self.config.default_model = model;
                self.status_text = if zh {
                    format!("默认模型已选择：{label}；点击“保存并应用”后生效")
                } else {
                    format!("Default model selected: {label}. Save & apply to activate it.")
                };
            }
            if let Some(index) = edit {
                self.temp_model = self.config.models[index].clone();
                self.editing_model = Some(index);
                self.model_from_wizard = false;
                self.advanced_json_open = false;
                self.page = Page::Model;
            }
            if let Some(index) = configure {
                if let Some(model) = self.config.models.get(index) {
                    self.model_route_policy_draft =
                        super::logic::model_route_policy(&self.config, &model.model);
                    self.model_route_policy_target = Some(index);
                }
            }
            if let Some(route_id) = priority_dialog {
                let indices: Vec<usize> = self
                    .config
                    .models
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| super::logic::same_model_identity(&m.model, &route_id))
                    .map(|(i, _)| i)
                    .collect();
                let mut ordered = indices;
                ordered.sort_by(|&a, &b| {
                    let ma = &self.config.models[a];
                    let mb = &self.config.models[b];
                    let a_is_oauth = ma.source == "oauth";
                    let b_is_oauth = mb.source == "oauth";
                    b_is_oauth
                        .cmp(&a_is_oauth)
                        .then_with(|| ma.priority.cmp(&mb.priority))
                        .then_with(|| a.cmp(&b))
                });
                self.model_priority_dialog_target = Some(route_id);
                self.model_priority_order = ordered;
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
        ui.add_space(8.0);
        let mut clear_log = false;
        let mut export_log = false;
        theme::dark_glass_frame(palette)
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
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
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .small_button("↗")
                                        .on_hover_text(t(zh, "展开运行日志", "Open runtime log"))
                                        .clicked()
                                    {
                                        self.log_dialog_open = true;
                                    }
                                    if ui
                                        .small_button("↓")
                                        .on_hover_text(t(
                                            zh,
                                            "下载脱敏日志",
                                            "Download redacted log",
                                        ))
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
                                    ui.checkbox(
                                        &mut self.log_follow_latest,
                                        t(zh, "跟随最新", "Follow"),
                                    );
                                    if self.log_follow_latest && !previous_follow {
                                        self.log_scroll_to_bottom = true;
                                    }
                                },
                            );
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
