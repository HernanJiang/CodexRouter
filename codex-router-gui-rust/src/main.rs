#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod logic;
mod theme;
mod ui;

use config::{ModelConfig, RouterConfig};
use eframe::egui;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

const LOGO_PNG: &[u8] = include_bytes!("../assets/logo.png");
const CURRENT_TERMS_VERSION: &str = "codex-router-terms-v1.0-2026-08-01";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    Welcome,
    Project,
    Auth,
    Model,
    Proxy,
    Finish,
    Dashboard,
}

enum AppEvent {
    Log(String),
    Complete,
    Error(String),
}

struct CodexRouterApp {
    page: Page,
    router_root: PathBuf,
    project_path_input: String,
    config: RouterConfig,
    temp_model: ModelConfig,
    editing_model: Option<usize>,
    model_page: usize,
    model_from_wizard: bool,
    proxy_from_wizard: bool,
    status_text: String,
    logs: String,
    event_rx: Receiver<AppEvent>,
    event_tx: Sender<AppEvent>,
    applying: bool,
    configured: bool,
    logo_texture: Option<egui::TextureHandle>,
    _tray_icon: Option<tray_icon::TrayIcon>,
    last_page: Page,
    page_changed_at: std::time::Instant,
    installed_theme: String,
    installed_compact_layout: bool,
    ui_language: String,
    terms_open: bool,
    terms_scroll_complete: bool,
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
    if !zh {
        return line;
    }
    let localized = [
        ("[1/7]", "[1/7] 正在初始化本地凭据与数据库…"),
        ("[2/7]", "[2/7] 正在启动 PostgreSQL、Redis 与 Sub2API…"),
        ("[3/7]", "[3/7] 本地服务已就绪，正在登录管理接口…"),
        ("[4/7]", "[4/7] 正在确认 Sub2API 合规状态…"),
        ("[5/7]", "[5/7] 正在创建或更新模型渠道…"),
        ("[6/7]", "[6/7] 正在写入 Codex 配置与本地访问密钥…"),
        ("[7/7]", "[7/7] 部署完成。"),
    ];
    localized
        .into_iter()
        .find_map(|(prefix, text)| line.starts_with(prefix).then(|| text.to_owned()))
        .unwrap_or(line)
}

impl CodexRouterApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (event_tx, event_rx) = channel();
        let router_root = RouterConfig::find_router_root();
        let config_path = router_root.join("codex-router-config.json");
        let (mut config, page, configured) = match RouterConfig::load(&config_path) {
            Ok(cfg) => (cfg, Page::Dashboard, true),
            Err(_) => (RouterConfig::default(), Page::Welcome, false),
        };
        if config.deploy.cc_switch_db.trim().is_empty() {
            if let Some(path) = logic::detect_cc_switch_db() {
                config.deploy.cc_switch_db = path.display().to_string();
            }
        }
        if config.accepted_terms_version != CURRENT_TERMS_VERSION {
            config.accept_compliance = false;
            config.accepted_terms_version.clear();
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
        let mut fonts = egui::FontDefinitions::default();
        let font_specs = [
            ("msyh", "C:/Windows/Fonts/msyh.ttc"),
            ("segoe", "C:/Windows/Fonts/segoeui.ttf"),
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
        cc.egui_ctx.set_fonts(fonts);
        theme::install(&cc.egui_ctx, &theme::palette(&config.ui_theme));
        let installed_theme = config.ui_theme.clone();
        let installed_compact_layout = cc.egui_ctx.content_rect().height() < 700.0;
        let tray = decode_icon().ok().and_then(|(rgba, width, height)| {
            tray_icon::Icon::from_rgba(rgba, width, height)
                .ok()
                .and_then(|icon| {
                    tray_icon::TrayIconBuilder::new()
                        .with_tooltip("Codex-Router")
                        .with_icon(icon)
                        .build()
                        .ok()
                })
        });
        let project_path_input = router_root.to_string_lossy().to_string();
        Self {
            page,
            router_root,
            project_path_input,
            config,
            temp_model: ModelConfig::default(),
            editing_model: None,
            model_page: 0,
            model_from_wizard: true,
            proxy_from_wizard: true,
            status_text: String::new(),
            logs: String::new(),
            event_rx,
            event_tx,
            applying: false,
            configured,
            logo_texture: None,
            _tray_icon: tray,
            last_page: page,
            page_changed_at: std::time::Instant::now(),
            installed_theme,
            installed_compact_layout,
            ui_language,
            terms_open: false,
            terms_scroll_complete: false,
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
        self.logs.push_str(message.as_ref());
        self.logs.push('\n');
    }

    fn apply_all(&mut self) {
        let zh = self.ui_language == "zh";
        if !self.config.accept_compliance
            || self.config.accepted_terms_version != CURRENT_TERMS_VERSION
        {
            self.status_text = if zh {
                "请先完整阅读并同意当前版本的 Codex-Router 使用与分发承诺"
            } else {
                "Read and accept the current Codex-Router terms before deployment"
            }
            .into();
            return;
        }
        if self.config.models.is_empty() {
            self.status_text = if zh {
                "请至少添加一个模型"
            } else {
                "Add at least one model"
            }
            .into();
            return;
        }
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
            "CC Switch 隔离 Provider 已同步（数据库已备份）"
        } else {
            "The isolated CC Switch provider was synced after database backup"
        }
        .to_owned();
        for model in &mut self.config.models {
            model.api_key.clear();
        }
        self.config.proxy.password.clear();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                logic::store_credentials(&mut cfg, &root)?;
                tx.send(AppEvent::Log(credential_log)).ok();
                logic::write_all_files(&cfg, &root)?;
                tx.send(AppEvent::Log(files_log)).ok();
                logic::run_apply_script(&root, |line| {
                    tx.send(AppEvent::Log(localized_deployment_line(zh, line)))
                        .ok();
                })?;
                if cfg.deploy.cc_switch_sync {
                    let local_key = logic::read_credential(&root, "LocalApiKey")?;
                    logic::sync_cc_switch(&cfg, &local_key)?;
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
    }

    fn run_script_new_console(&self, relative: &str) {
        let script = self.router_root.join("scripts").join(relative);
        let cwd = self.router_root.clone();
        std::thread::spawn(move || {
            let _ = std::process::Command::new("powershell.exe")
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ])
                .arg(script)
                .current_dir(cwd)
                .creation_flags(0x00000010)
                .spawn();
        });
    }

    fn stop_router(&self) {
        let script = self.router_root.join("scripts").join("Stop-Router.ps1");
        let cwd = self.router_root.clone();
        std::thread::spawn(move || {
            let _ = std::process::Command::new("powershell.exe")
                .args([
                    "-NoLogo",
                    "-NoProfile",
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
                "4 / 5  网络代理与 CC Switch"
            } else {
                "网络代理与 CC Switch"
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
            "同步为 CC Switch 独立隔离配置（可选）",
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
            ui.label("同步前会自动备份数据库；关闭本开关时不会写入 CC Switch。");
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
        ui.label("将自动初始化本地运行环境、配置真实 Sub2API 渠道、写入 Codex，并执行你选择的代理与 CC Switch 设置。");
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
                let _ = std::process::Command::new("cmd.exe")
                    .args(["/C", "start", "", "http://127.0.0.1:18080"])
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

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([840.0, 680.0])
            .with_icon(window_icon()),
        centered: true,
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native(
        "Codex-Router",
        options,
        Box::new(|cc| Ok(Box::new(CodexRouterApp::new(cc)))),
    )
}
