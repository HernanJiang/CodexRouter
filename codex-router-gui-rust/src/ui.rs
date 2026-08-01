use super::{theme, AppEvent, CodexRouterApp, ModelConfig, Page};
use eframe::egui;

const TERMS_ZH: &str = include_str!("../../TERMS.zh-CN.md");
const TERMS_EN: &str = include_str!("../../TERMS.en.md");

fn step_number(page: Page) -> usize {
    match page {
        Page::Welcome => 0,
        Page::Project => 1,
        Page::Auth => 2,
        Page::Model => 3,
        Page::Proxy => 4,
        Page::Finish => 5,
        Page::Dashboard => 6,
    }
}

fn t<'a>(zh: bool, chinese: &'a str, english: &'a str) -> &'a str {
    if zh {
        chinese
    } else {
        english
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

impl eframe::App for CodexRouterApp {
    fn ui(&mut self, root_ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root_ui.ctx().clone();
        let zh = self.ui_language == "zh";
        let palette = theme::palette(&self.config.ui_theme);
        let compact_layout = ctx.content_rect().height() < 700.0;
        if self.installed_theme != self.config.ui_theme
            || self.installed_compact_layout != compact_layout
        {
            theme::install(&ctx, &palette);
            self.installed_theme.clone_from(&self.config.ui_theme);
            self.installed_compact_layout = compact_layout;
        }
        self.load_logo_texture(&ctx);
        if self.page != self.last_page {
            self.last_page = self.page;
            self.page_changed_at = std::time::Instant::now();
        }
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                AppEvent::Log(message) => self.log(message),
                AppEvent::Complete => {
                    self.applying = false;
                    self.configured = true;
                    self.status_text = t(
                        zh,
                        "配置完成：模型渠道、Codex 和所选集成均已生效",
                        "Configuration complete: model channels, Codex, and integrations are active",
                    )
                    .into();
                    self.log(t(zh, "配置完成", "Configuration complete"));
                }
                AppEvent::Error(error) => {
                    self.applying = false;
                    self.status_text =
                        format!("{}: {error}", t(zh, "配置失败", "Configuration failed"));
                    self.log(format!("{}: {error}", t(zh, "错误", "Error")));
                }
            }
        }

        egui::Panel::top("editorial-nav")
            .exact_size(104.0)
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
                    .inner_margin(egui::Margin::symmetric(28, 18)),
            )
            .show(root_ui, |ui| self.show_topbar(ui, &palette));

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(palette.background))
            .show(root_ui, |ui| {
                let rect = ui.max_rect();
                theme::paint_background(ui.painter(), rect, &palette);
                ui.add_space(24.0);
                let max_width = if self.page == Page::Dashboard {
                    1480.0
                } else {
                    1320.0
                };
                let container_width = ui.available_width();
                let width = (container_width - 48.0).max(320.0).min(max_width);
                let side = ((container_width - width) * 0.5).max(0.0);
                // Reserve a dedicated footer strip below the wizard cards so the
                // signature always sits on the solid theme color, never across a
                // pale card where its leading characters lose contrast.
                let content_height = (ui.available_height() - 44.0).max(400.0);
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
                            match self.page {
                                Page::Welcome => self.show_welcome(ui, &palette),
                                Page::Project => self.show_project(ui, &palette),
                                Page::Auth => self.show_auth(ui, &palette),
                                Page::Model => self.show_model(ui, &palette),
                                Page::Proxy => self.show_proxy(ui, &palette),
                                Page::Finish => self.show_finish(ui, &palette),
                                Page::Dashboard => self.show_dashboard(ui, &palette),
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
        if ctx.egui_wants_keyboard_input()
            || self.applying
            || self.page_changed_at.elapsed().as_secs_f32() < 0.3
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("codex-router-ui-theme-v3", self.config.ui_theme.clone());
        storage.set_string("codex-router-ui-language-v1", self.ui_language.clone());
    }
}

impl CodexRouterApp {
    fn show_topbar(&mut self, ui: &mut egui::Ui, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        ui.horizontal(|ui| {
            egui::Frame::new()
                .fill(palette.paper)
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 170),
                ))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::symmetric(20, 12))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 6],
                    blur: 18,
                    spread: 0,
                    color: egui::Color32::from_rgba_unmultiplied(32, 22, 16, 42),
                })
                .show(ui, |ui| {
                    ui.set_min_width(230.0);
                    ui.horizontal(|ui| {
                        if let Some(texture) = &self.logo_texture {
                            ui.image((texture.id(), egui::vec2(38.0, 38.0)));
                        }
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("CODEX-ROUTER")
                                .font(egui::FontId::new(17.0, theme::display_family()))
                                .color(palette.ink),
                        );
                    });
                });

            ui.add_space(8.0);
            let current = step_number(self.page);
            if ui.available_width() > 760.0 {
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
                            let complete = current > index + 1 || self.page == Page::Dashboard;
                            let color = if active || complete {
                                palette.ink
                            } else {
                                palette.ink_soft
                            };
                            let text = if complete {
                                format!("✓ {label}")
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
                            if response.clicked() && complete && self.page != Page::Dashboard {
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
            } else {
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
                if self.page != Page::Dashboard {
                    theme::elevated_control_frame(palette, true).show(ui, |ui| {
                        if ui
                            .button(t(zh, "跳过引导", "SKIP GUIDE"))
                            .on_hover_text(t(zh, "直接进入控制台", "Open the console directly"))
                            .clicked()
                        {
                            self.page = Page::Dashboard;
                        }
                    });
                }
                theme::elevated_control_frame(palette, true).show(ui, |ui| {
                    if ui
                        .button(if zh { "中文 / EN" } else { "EN / 中文" })
                        .on_hover_text(t(zh, "切换为英文", "Switch to Chinese"))
                        .clicked()
                    {
                        self.ui_language = if zh { "en" } else { "zh" }.to_owned();
                    }
                });
                theme::elevated_control_frame(palette, true).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(t(zh, "主题", "THEME"))
                                .small()
                                .strong()
                                .color(palette.ink_soft),
                        );
                        egui::ComboBox::from_id_salt("theme-switch")
                            .selected_text(if self.config.ui_theme == "sky" {
                                t(zh, "雾蓝 / 白", "MIST / WHITE")
                            } else {
                                t(zh, "陶土 / 米白", "CLAY / CREAM")
                            })
                            .width(104.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.config.ui_theme,
                                    "coffee".into(),
                                    t(zh, "陶土 / 米白", "CLAY / CREAM"),
                                );
                                ui.selectable_value(
                                    &mut self.config.ui_theme,
                                    "sky".into(),
                                    t(zh, "雾蓝 / 白", "MIST / WHITE"),
                                );
                            });
                    });
                });
            });
        });
    }

    fn magazine_cover(
        &self,
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
            });
            if !compact {
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    theme::eyebrow(ui, step, palette.muted);
                    ui.add_space(6.0);
                    ui.separator();
                });
            }
        });
    }

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
                    ("02", "多模态就绪", "自动识别 Kimi K3 等多模态模型"),
                    ("03", "兼容代理", "Clash / V2Ray / SOCKS5 一键接入"),
                    ("04", "CC SWITCH", "可选、隔离、同步前自动备份"),
                ]
            } else {
                [
                    ("01", "MODEL ROUTING", "Multiple models, URLs, and priority fallback"),
                    ("02", "VISION READY", "Auto-detect multimodal models such as Kimi K3"),
                    ("03", "PROXY COMPATIBLE", "One-click Clash / V2Ray / SOCKS5 support"),
                    ("04", "CC SWITCH", "Optional isolation with backup before sync"),
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
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.radio_value(&mut this.config.auth_mode, value.into(), "");
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new(title).strong().color(palette.ink),
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
                        this.editing_model = None;
                        this.model_from_wizard = true;
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
                    let two_columns = ui.available_width() > 610.0;
                    if two_columns {
                        ui.columns(2, |columns| {
                            theme::field_label(
                                &mut columns[0],
                                t(zh, "模型名称", "MODEL NAME"),
                                t(zh, "必填", "Required"),
                                palette,
                            );
                            theme::input_ascii(
                                &mut columns[0],
                                &mut this.temp_model.model,
                                t(zh, "例如 kimi-k3", "e.g. kimi-k3"),
                                false,
                                palette,
                            );
                            theme::field_label(
                                &mut columns[1],
                                t(zh, "显示别名", "DISPLAY ALIAS"),
                                t(zh, "可选", "Optional"),
                                palette,
                            );
                            theme::input(
                                &mut columns[1],
                                &mut this.temp_model.alias,
                                t(zh, "例如 Kimi K3", "e.g. Kimi K3"),
                                false,
                                palette,
                            );
                        });
                    } else {
                        theme::field_label(
                            ui,
                            t(zh, "模型名称", "MODEL NAME"),
                            t(zh, "必填", "Required"),
                            palette,
                        );
                        theme::input_ascii(
                            ui,
                            &mut this.temp_model.model,
                            t(zh, "例如 kimi-k3", "e.g. kimi-k3"),
                            false,
                            palette,
                        );
                        theme::field_label(
                            ui,
                            t(zh, "显示别名", "DISPLAY ALIAS"),
                            t(zh, "可选", "Optional"),
                            palette,
                        );
                        theme::input(
                            ui,
                            &mut this.temp_model.alias,
                            t(zh, "例如 Kimi K3", "e.g. Kimi K3"),
                            false,
                            palette,
                        );
                    }
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
                    ui.columns(3, |columns| {
                        theme::field_label(
                            &mut columns[0],
                            t(zh, "优先级", "PRIORITY"),
                            "1–999",
                            palette,
                        );
                        let priority_response = columns[0].add(
                            egui::DragValue::new(&mut this.temp_model.priority).range(1..=999),
                        );
                        theme::ascii_response(&mut columns[0], &priority_response);
                        theme::field_label(
                            &mut columns[1],
                            t(zh, "权重", "WEIGHT"),
                            "1–100",
                            palette,
                        );
                        let weight_response = columns[1]
                            .add(egui::DragValue::new(&mut this.temp_model.weight).range(1..=100));
                        theme::ascii_response(&mut columns[1], &weight_response);
                        theme::field_label(
                            &mut columns[2],
                            t(zh, "多模态", "MULTIMODAL"),
                            t(zh, "图片支持", "Image support"),
                            palette,
                        );
                        egui::ComboBox::from_id_salt("multimodal")
                            .selected_text(match this.temp_model.multimodal.as_str() {
                                "true" => t(zh, "手动支持", "Enabled"),
                                "false" => t(zh, "手动关闭", "Disabled"),
                                _ => t(zh, "自动判断", "Auto detect"),
                            })
                            .show_ui(&mut columns[2], |ui| {
                                ui.selectable_value(
                                    &mut this.temp_model.multimodal,
                                    "auto".into(),
                                    t(zh, "自动判断", "Auto detect"),
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
                    let vision = super::logic::resolve_multimodal(&this.temp_model);
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
                    theme::field_label(
                        ui,
                        t(zh, "高级 JSON", "ADVANCED JSON"),
                        t(zh, "必须是 JSON 对象", "Must be a JSON object"),
                        palette,
                    );
                    theme::multiline_ascii(ui, &mut this.temp_model.extra, "{}", 2, palette);
                    let json_valid =
                        serde_json::from_str::<serde_json::Value>(&this.temp_model.extra)
                            .map(|value| value.is_object())
                            .unwrap_or(false);
                    let valid = !this.temp_model.model.trim().is_empty()
                        && !this.temp_model.base_url.trim().is_empty()
                        && json_valid
                        && (!this.temp_model.api_key.trim().is_empty()
                            || !this.temp_model.credential_name.is_empty());
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
                    let back_label = if this.model_from_wizard {
                        t(zh, "← 登录方式", "← Access")
                    } else {
                        t(zh, "取消", "Cancel")
                    };
                    let next_label = if this.model_from_wizard {
                        t(zh, "网络与集成 →", "Network & integrations →")
                    } else {
                        t(zh, "保存模型", "Save model")
                    };
                    let (back, next) =
                        Self::navigation_row(ui, back_label, next_label, valid, palette);
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
                            this.proxy_from_wizard = true;
                            this.page = Page::Proxy;
                        } else {
                            match this.editing_model {
                                Some(index) => this.config.models[index] = this.temp_model.clone(),
                                None => this.config.models.push(this.temp_model.clone()),
                            }
                            this.page = Page::Dashboard;
                        }
                    }
                });
            },
        );
    }

    fn show_proxy(&mut self, ui: &mut egui::Ui, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        self.wizard_layout(
            ui,
            t(zh, "04 / 网络与集成", "04 / NETWORK & INTEGRATION"),
            t(zh, "连接网络", "CONNECT THE NETWORK"),
            t(zh, "你的路由，你的规则，", "your route, your rules,"),
            t(zh, "代理与 CC Switch 都是可选项；关闭时不会写入任何额外配置。", "Proxy and CC Switch are optional and write nothing when disabled."),
            palette,
            |this, ui, palette, form_height| {
                theme::glass_frame(palette).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.set_min_height((form_height - 52.0).max(470.0));
                    Self::panel_heading(ui, t(zh, "第 04 步", "STEP 04"), t(zh, "网络与集成", "Network & integrations"), palette);
                    egui::Frame::new()
                        .fill(palette.paper)
                        .stroke(egui::Stroke::new(1.0_f32, palette.line))
                        .shadow(theme::soft_card_shadow())
                        .inner_margin(egui::Margin::same(16))
                        .show(ui, |ui| {
                            ui.checkbox(&mut this.config.proxy.enabled, t(zh, "启用上游网络代理", "Enable upstream network proxy"));
                            ui.label(
                                egui::RichText::new(t(zh, "兼容 Clash、V2Ray、SSR、HTTP 与 SOCKS5", "Compatible with Clash, V2Ray, SSR, HTTP, and SOCKS5"))
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
                                    theme::field_label(
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
                                    theme::field_label(
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
                    egui::Frame::new()
                        .fill(palette.paper)
                        .stroke(egui::Stroke::new(1.0_f32, palette.line))
                        .shadow(theme::soft_card_shadow())
                        .inner_margin(egui::Margin::same(16))
                        .show(ui, |ui| {
                            let sync_response = ui.checkbox(
                                &mut this.config.deploy.cc_switch_sync,
                                t(zh, "同步为 CC Switch 独立隔离配置", "Sync as an isolated CC Switch profile"),
                            );
                            if sync_response.changed()
                                && this.config.deploy.cc_switch_sync
                                && this.config.deploy.cc_switch_db.trim().is_empty()
                            {
                                if let Some(path) = super::logic::detect_cc_switch_db() {
                                    this.config.deploy.cc_switch_db = path.display().to_string();
                                }
                            }
                            ui.label(
                                egui::RichText::new(t(zh, "勾选后才会写入；每次同步前自动备份数据库。", "Nothing is written unless enabled; the database is backed up before every sync."))
                                    .small()
                                    .color(palette.muted),
                            );
                            if this.config.deploy.cc_switch_sync {
                                ui.add_space(8.0);
                                theme::field_label(
                                    ui,
                                    t(zh, "CC SWITCH 数据库", "CC SWITCH DATABASE"),
                                    t(zh, "自动检测 · 可手动覆盖", "Auto-detected · manual override"),
                                    palette,
                                );
                                ui.horizontal(|ui| {
                                    let available = ui.available_width();
                                    ui.allocate_ui_with_layout(
                                        egui::vec2((available - 232.0).max(180.0), 46.0),
                                        egui::Layout::top_down(egui::Align::Min),
                                        |ui| {
                                            theme::input(
                                                ui,
                                                &mut this.config.deploy.cc_switch_db,
                                                t(zh, "未检测到 cc-switch.db", "cc-switch.db was not detected"),
                                                false,
                                                palette,
                                            );
                                        },
                                    );
                                    if theme::secondary_button(
                                        ui,
                                        t(zh, "重新检测", "Detect"),
                                        palette,
                                    )
                                    .clicked()
                                    {
                                        if let Some(path) = super::logic::detect_cc_switch_db() {
                                            this.config.deploy.cc_switch_db =
                                                path.display().to_string();
                                        }
                                    }
                                    if theme::secondary_button(
                                        ui,
                                        t(zh, "手动选择…", "Browse…"),
                                        palette,
                                    )
                                    .clicked()
                                    {
                                        if let Some(path) = rfd::FileDialog::new()
                                            .add_filter("SQLite", &["db"])
                                            .pick_file()
                                        {
                                            this.config.deploy.cc_switch_db =
                                                path.display().to_string();
                                        }
                                    }
                                });
                                let selected_path =
                                    std::path::Path::new(this.config.deploy.cc_switch_db.trim());
                                if selected_path.is_file() {
                                    let auto_detected = super::logic::detect_cc_switch_db()
                                        .is_some_and(|path| path == selected_path);
                                    ui.label(
                                        egui::RichText::new(if auto_detected {
                                            t(
                                                zh,
                                                "✓ 已自动检测到 CC Switch 数据库",
                                                "✓ CC Switch database detected automatically",
                                            )
                                        } else {
                                            t(zh, "✓ 数据库路径有效", "✓ Database path is valid")
                                        })
                                        .strong()
                                        .color(palette.success),
                                    );
                                } else {
                                    ui.label(
                                        egui::RichText::new(t(
                                            zh,
                                            "未检测到数据库；请先运行一次 CC Switch，或手动选择 cc-switch.db。",
                                            "Database not detected. Run CC Switch once or choose cc-switch.db manually.",
                                        ))
                                        .color(palette.danger),
                                    );
                                }
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
                    let (back, next) = Self::navigation_row(
                        ui,
                        if this.proxy_from_wizard {
                            t(zh, "← 模型", "← Model")
                        } else {
                            t(zh, "取消", "Cancel")
                        },
                        if this.proxy_from_wizard {
                            t(zh, "完成配置 →", "Finish setup →")
                        } else {
                            t(zh, "保存设置", "Save settings")
                        },
                        true,
                        palette,
                    );
                    if back {
                        this.page = back_page;
                    }
                    if next {
                        this.page = next_page;
                    }
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
            t(zh, "保存凭据、创建渠道、写入 Codex，并按你的选择同步集成。", "Save credentials, create channels, configure Codex, and sync integrations."),
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
                                this.terms_scroll_complete = this.config.accept_compliance
                                    && this.config.accepted_terms_version
                                        == super::CURRENT_TERMS_VERSION;
                            }
                            ui.label(
                                egui::RichText::new(t(
                                    zh,
                                    "包含禁止商用、禁止二次分发、官方 GitHub 唯一下载渠道、署名与 Sub2API 专项合规条款。",
                                    "Includes non-commercial, no-redistribution, official-download, attribution, and Sub2API requirements.",
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
                        if response.inner.clicked() { this.apply_all(); }
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
                    ui.horizontal(|ui| {
                        if theme::secondary_button(ui, t(zh, "← 网络与集成", "← Network & integrations"), palette).clicked() { this.page = Page::Proxy; }
                        if this.configured && theme::primary_button(
                            ui,
                            egui::RichText::new(t(zh, "进入控制台 →", "Open console →")).strong().color(egui::Color32::WHITE),
                            palette,
                        ).clicked() { this.page = Page::Dashboard; }
                    });
                });
            },
        );
        if self.terms_open {
            self.show_terms_modal(ui.ctx(), palette);
        }
    }

    fn show_terms_modal(&mut self, ctx: &egui::Context, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        let terms = if zh { TERMS_ZH } else { TERMS_EN };
        let bounds = ctx.content_rect();
        let modal_width = (bounds.width() * 0.68).clamp(620.0, 880.0);
        let scroll_height = (bounds.height() * 0.58).clamp(330.0, 640.0);
        let mut close_clicked = false;
        let mut accept_clicked = false;
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
                let scroll = egui::Frame::new()
                    .fill(palette.paper_alt)
                    .stroke(egui::Stroke::new(1.0, palette.line))
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::same(14))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("codex-router-terms-scroll")
                            .max_height(scroll_height)
                            .min_scrolled_height(scroll_height)
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                            )
                            .show(ui, |ui| {
                                ui.set_width((modal_width - 52.0).max(480.0));
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
                if max_offset <= 1.0 || scroll.state.offset.y >= max_offset - 12.0 {
                    self.terms_scroll_complete = true;
                }
                ui.add_space(12.0);
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
                    let confirm = ui.add_enabled_ui(self.terms_scroll_complete, |ui| {
                        theme::primary_button(
                            ui,
                            egui::RichText::new(t(zh, "我已阅读并同意", "I have read and agree"))
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
        if accept_clicked {
            self.config.accept_compliance = true;
            self.config.accepted_terms_version = super::CURRENT_TERMS_VERSION.to_owned();
            self.terms_open = false;
        } else if close_clicked || response.should_close() {
            self.terms_open = false;
        }
    }

    fn show_dashboard(&mut self, ui: &mut egui::Ui, palette: &theme::Palette) {
        let zh = self.ui_language == "zh";
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                theme::eyebrow(
                    ui,
                    t(zh, "本地模型控制中心", "LOCAL MODEL CONTROL PLANE"),
                    palette.paper,
                );
                ui.label(
                    egui::RichText::new(t(zh, "路由控制台", "ROUTER CONSOLE"))
                        .font(egui::FontId::new(38.0, theme::display_family()))
                        .color(egui::Color32::WHITE),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::accent_button(
                    ui,
                    egui::RichText::new("＋")
                        .size(22.0)
                        .color(egui::Color32::WHITE),
                    palette,
                )
                .clicked()
                {
                    self.temp_model = ModelConfig::default();
                    self.editing_model = None;
                    self.model_from_wizard = false;
                    self.page = Page::Model;
                }
                theme::pill(
                    ui,
                    if self.configured {
                        t(zh, "已配置", "CONFIGURED")
                    } else {
                        t(zh, "草稿", "DRAFT")
                    },
                    palette.paper,
                    palette.ink,
                );
            });
        });
        ui.add_space(14.0);
        let wide = ui.available_width() >= 760.0;
        let panel_height = ui.available_height();
        if wide {
            let width = ui.available_width();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(width * 0.25, panel_height),
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
            self.dashboard_sidebar(ui, palette, panel_height);
            ui.add_space(16.0);
            self.dashboard_models(ui, palette, panel_height);
        }
    }

    fn dashboard_action_button(
        ui: &mut egui::Ui,
        label: &str,
        primary: bool,
        palette: &theme::Palette,
    ) -> egui::Response {
        ui.add_sized(
            [ui.available_width(), 38.0],
            egui::Button::new(
                egui::RichText::new(label)
                    .size(12.5)
                    .strong()
                    .color(if primary {
                        egui::Color32::WHITE
                    } else {
                        palette.ink
                    }),
            )
            .fill(if primary {
                palette.action
            } else {
                egui::Color32::WHITE
            })
            .stroke(if primary {
                egui::Stroke::NONE
            } else {
                egui::Stroke::new(1.0_f32, palette.line)
            })
            .corner_radius(egui::CornerRadius::same(7)),
        )
    }

    fn dashboard_sidebar(
        &mut self,
        ui: &mut egui::Ui,
        palette: &theme::Palette,
        target_height: f32,
    ) {
        let zh = self.ui_language == "zh";
        theme::paper_frame(palette).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height((target_height - 56.0).max(480.0));
            theme::eyebrow(ui, t(zh, "系统 / 概览", "SYSTEM / OVERVIEW"), palette.muted);
            ui.label(
                egui::RichText::new(format!("{:02}", self.config.models.len()))
                    .font(egui::FontId::new(58.0, theme::display_family()))
                    .color(palette.ink),
            );
            ui.label(
                egui::RichText::new(t(zh, "已配置模型", "configured models"))
                    .font(egui::FontId::new(20.0, theme::serif_family()))
                    .italics()
                    .color(palette.ink_soft),
            );
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(t(zh, "项目根目录", "PROJECT ROOT"))
                    .small()
                    .strong()
                    .color(palette.muted),
            );
            ui.label(
                egui::RichText::new(self.router_root.display().to_string())
                    .small()
                    .color(palette.ink_soft),
            );
            ui.add_space(12.0);
            ui.columns(2, |columns| {
                if Self::dashboard_action_button(
                    &mut columns[0],
                    t(zh, "▶ 启动", "▶ Start"),
                    true,
                    palette,
                )
                .clicked()
                {
                    self.run_script_new_console("Start-Router.ps1");
                    self.log(t(zh, "正在启动路由...", "Starting router..."));
                }
                if Self::dashboard_action_button(
                    &mut columns[1],
                    t(zh, "停止", "Stop"),
                    false,
                    palette,
                )
                .clicked()
                {
                    self.stop_router();
                    self.log(t(zh, "正在停止路由...", "Stopping router..."));
                }
            });
            ui.columns(2, |columns| {
                if Self::dashboard_action_button(
                    &mut columns[0],
                    t(zh, "Sub2API ↗", "Sub2API ↗"),
                    false,
                    palette,
                )
                .clicked()
                {
                    let _ = std::process::Command::new("cmd.exe")
                        .args(["/C", "start", "", "http://127.0.0.1:18080"])
                        .spawn();
                }
                if self.config.auth_mode == "chatgpt_oauth"
                    && Self::dashboard_action_button(&mut columns[1], "OAuth", false, palette)
                        .clicked()
                {
                    self.run_script_new_console("Start-ChatGPTOAuth.ps1");
                }
            });
            ui.separator();
            ui.columns(2, |columns| {
                if Self::dashboard_action_button(
                    &mut columns[0],
                    t(zh, "网络 / CC", "Network / CC"),
                    false,
                    palette,
                )
                .clicked()
                {
                    self.proxy_from_wizard = false;
                    self.page = Page::Proxy;
                }
                if Self::dashboard_action_button(
                    &mut columns[1],
                    t(zh, "重新引导", "Run guide"),
                    false,
                    palette,
                )
                .clicked()
                {
                    self.page = Page::Welcome;
                }
            });
        });
    }

    fn dashboard_models(
        &mut self,
        ui: &mut egui::Ui,
        palette: &theme::Palette,
        target_height: f32,
    ) {
        let zh = self.ui_language == "zh";
        theme::glass_frame(palette).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height((target_height - 45.0).max(480.0));
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    theme::eyebrow(
                        ui,
                        t(zh, "模型渠道", "MODEL CHANNELS"),
                        palette.background_dark,
                    );
                    ui.label(
                        egui::RichText::new(t(zh, "你的路由配置", "YOUR ROUTING EDITION"))
                            .font(egui::FontId::new(25.0, theme::display_family()))
                            .color(palette.ink),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                });
            });
            if self.applying {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(t(zh, "正在应用配置…", "Applying configuration…"));
                });
            }
            if !self.status_text.is_empty() {
                ui.label(
                    egui::RichText::new(&self.status_text)
                        .small()
                        .color(palette.ink_soft),
                );
            }
            ui.add_space(12.0);
            let mut edit = None;
            let mut delete = None;
            let per_page = if ui.ctx().content_rect().height() < 840.0 {
                3
            } else {
                4
            };
            let page_count = self.config.models.len().div_ceil(per_page).max(1);
            self.model_page = self.model_page.min(page_count - 1);
            let page_start = self.model_page * per_page;
            for (index, model) in self
                .config
                .models
                .iter()
                .enumerate()
                .skip(page_start)
                .take(per_page)
            {
                let vision = super::logic::resolve_multimodal(model);
                let response = egui::Frame::new()
                    .fill(palette.paper)
                    .stroke(egui::Stroke::new(1.0_f32, palette.line))
                    .shadow(theme::soft_card_shadow())
                    .inner_margin(egui::Margin::symmetric(18, 15))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!("{:02}", index + 1))
                                    .font(egui::FontId::new(23.0, theme::display_family()))
                                    .color(palette.accent),
                            );
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new(if model.alias.is_empty() {
                                        &model.model
                                    } else {
                                        &model.alias
                                    })
                                    .font(egui::FontId::new(19.0, theme::display_family()))
                                    .color(palette.ink),
                                );
                                ui.label(
                                    egui::RichText::new(&model.base_url)
                                        .small()
                                        .color(palette.background_dark),
                                );
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button(t(zh, "删除", "Delete")).clicked() {
                                        delete = Some(index);
                                    }
                                    if ui.small_button(t(zh, "编辑", "Edit")).clicked() {
                                        edit = Some(index);
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
                                    theme::pill(
                                        ui,
                                        &format!("P{}", model.priority),
                                        palette.paper_alt,
                                        palette.ink_soft,
                                    );
                                },
                            );
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
            }
            if page_count > 1 {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            self.model_page > 0,
                            egui::Button::new(t(zh, "← 上一页", "← Previous")),
                        )
                        .clicked()
                    {
                        self.model_page -= 1;
                    }
                    ui.label(format!("{} / {}", self.model_page + 1, page_count));
                    if ui
                        .add_enabled(
                            self.model_page + 1 < page_count,
                            egui::Button::new(t(zh, "下一页 →", "Next →")),
                        )
                        .clicked()
                    {
                        self.model_page += 1;
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
                                    "点击右上角的 ＋ 添加第一个模型",
                                    "Click ＋ in the upper-right to add your first model",
                                ))
                                .color(palette.muted),
                            );
                        });
                    });
            }
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                theme::dark_glass_frame(palette).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), 70.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            theme::eyebrow(
                                ui,
                                t(zh, "活动日志", "ACTIVITY LOG"),
                                egui::Color32::from_rgb(220, 220, 215),
                            );
                            let excerpt = if self.logs.is_empty() {
                                t(zh, "等待操作…", "Waiting for an action…").to_owned()
                            } else {
                                log_excerpt(&self.logs, 4, 100)
                            };
                            ui.label(
                                egui::RichText::new(excerpt)
                                    .monospace()
                                    .small()
                                    .color(egui::Color32::WHITE),
                            );
                        },
                    );
                });
            });
        });
    }
}
