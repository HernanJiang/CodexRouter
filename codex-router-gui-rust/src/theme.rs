use eframe::egui;

#[derive(Clone, Copy)]
pub struct Palette {
    pub background: egui::Color32,
    pub background_dark: egui::Color32,
    pub background_light: egui::Color32,
    pub paper: egui::Color32,
    pub paper_alt: egui::Color32,
    pub ink: egui::Color32,
    pub ink_soft: egui::Color32,
    pub muted: egui::Color32,
    pub line: egui::Color32,
    pub action: egui::Color32,
    pub accent: egui::Color32,
    pub glass: egui::Color32,
    pub glass_dark: egui::Color32,
    pub success: egui::Color32,
    pub danger: egui::Color32,
}

pub fn palette(name: &str) -> Palette {
    if name == "sky" {
        Palette {
            background: egui::Color32::from_rgb(111, 143, 163),
            background_dark: egui::Color32::from_rgb(64, 94, 112),
            background_light: egui::Color32::from_rgb(168, 194, 208),
            paper: egui::Color32::from_rgb(247, 250, 251),
            paper_alt: egui::Color32::from_rgb(234, 240, 243),
            ink: egui::Color32::from_rgb(12, 16, 24),
            ink_soft: egui::Color32::from_rgb(45, 58, 67),
            muted: egui::Color32::from_rgb(99, 113, 122),
            line: egui::Color32::from_rgb(202, 215, 222),
            action: egui::Color32::from_rgb(23, 43, 58),
            accent: egui::Color32::from_rgb(230, 110, 70),
            glass: egui::Color32::from_rgba_unmultiplied(235, 243, 246, 224),
            glass_dark: egui::Color32::from_rgba_unmultiplied(36, 55, 65, 218),
            success: egui::Color32::from_rgb(29, 130, 89),
            danger: egui::Color32::from_rgb(190, 54, 51),
        }
    } else {
        Palette {
            background: egui::Color32::from_rgb(154, 120, 98),
            background_dark: egui::Color32::from_rgb(90, 64, 49),
            background_light: egui::Color32::from_rgb(203, 169, 139),
            paper: egui::Color32::from_rgb(251, 245, 234),
            paper_alt: egui::Color32::from_rgb(239, 226, 208),
            ink: egui::Color32::from_rgb(42, 29, 21),
            ink_soft: egui::Color32::from_rgb(83, 64, 52),
            muted: egui::Color32::from_rgb(125, 101, 84),
            line: egui::Color32::from_rgb(216, 194, 169),
            action: egui::Color32::from_rgb(61, 39, 27),
            accent: egui::Color32::from_rgb(183, 101, 70),
            glass: egui::Color32::from_rgba_unmultiplied(248, 238, 222, 224),
            glass_dark: egui::Color32::from_rgba_unmultiplied(63, 47, 38, 218),
            success: egui::Color32::from_rgb(58, 119, 83),
            danger: egui::Color32::from_rgb(166, 66, 52),
        }
    }
}

pub fn display_family() -> egui::FontFamily {
    egui::FontFamily::Name("editorial-display".into())
}

pub fn serif_family() -> egui::FontFamily {
    egui::FontFamily::Name("editorial-serif".into())
}

pub fn install(ctx: &egui::Context, palette: &Palette) {
    let compact = ctx.content_rect().height() < 700.0;
    let mut style = (*ctx.global_style()).clone();
    style.visuals = egui::Visuals::light();
    style.visuals.panel_fill = egui::Color32::TRANSPARENT;
    style.visuals.window_fill = palette.paper;
    style.visuals.extreme_bg_color = egui::Color32::WHITE;
    style.visuals.faint_bg_color = palette.paper_alt;
    style.visuals.override_text_color = Some(palette.ink);
    style.visuals.selection.bg_fill = palette.background_light;
    style.visuals.selection.stroke = egui::Stroke::new(1.0_f32, palette.ink);
    style.visuals.hyperlink_color = palette.background_dark;
    style.visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, palette.ink_soft);
    style.visuals.widgets.inactive.bg_fill = palette.paper_alt;
    style.visuals.widgets.inactive.weak_bg_fill = palette.paper_alt;
    style.visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, palette.ink_soft);
    style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, palette.line);
    style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(2);
    style.visuals.widgets.hovered.bg_fill = egui::Color32::WHITE;
    style.visuals.widgets.hovered.weak_bg_fill = egui::Color32::WHITE;
    style.visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.5_f32, palette.ink);
    style.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.5_f32, palette.background_dark);
    style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(2);
    style.visuals.widgets.active.bg_fill = egui::Color32::WHITE;
    style.visuals.widgets.active.weak_bg_fill = egui::Color32::WHITE;
    style.visuals.widgets.active.fg_stroke = egui::Stroke::new(1.5_f32, palette.ink);
    style.visuals.widgets.active.bg_stroke = egui::Stroke::new(2.0_f32, palette.background_dark);
    style.visuals.widgets.active.corner_radius = egui::CornerRadius::same(2);
    style.visuals.widgets.open.bg_fill = egui::Color32::WHITE;
    style.visuals.widgets.open.fg_stroke = egui::Stroke::new(1.5_f32, palette.ink);
    style.visuals.widgets.open.bg_stroke = egui::Stroke::new(2.0_f32, palette.background_dark);
    style.visuals.widgets.open.corner_radius = egui::CornerRadius::same(2);
    style.spacing.item_spacing = egui::vec2(12.0, if compact { 8.0 } else { 12.0 });
    style.spacing.button_padding = egui::vec2(18.0, if compact { 8.0 } else { 11.0 });
    style.spacing.interact_size = egui::vec2(44.0, if compact { 38.0 } else { 42.0 });
    style.spacing.combo_width = 180.0;
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::new(30.0, display_family()),
    );
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::new(15.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::new(14.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        egui::FontId::new(12.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Monospace,
        egui::FontId::new(13.5, egui::FontFamily::Monospace),
    );
    ctx.set_global_style(style);
}

pub fn paint_background(painter: &egui::Painter, rect: egui::Rect, palette: &Palette) {
    painter.rect_filled(rect, 0.0, palette.background);
}

pub fn paper_frame(palette: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(palette.paper)
        .stroke(egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 180),
        ))
        .corner_radius(egui::CornerRadius::same(2))
        .inner_margin(egui::Margin::same(28))
        .shadow(egui::epaint::Shadow {
            offset: [0, 10],
            blur: 28,
            spread: 0,
            color: egui::Color32::from_rgba_unmultiplied(26, 20, 16, 42),
        })
}

pub fn glass_frame(palette: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(palette.glass)
        .stroke(egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 188),
        ))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(egui::Margin::same(26))
        .shadow(egui::epaint::Shadow {
            offset: [0, 12],
            blur: 40,
            spread: 0,
            color: egui::Color32::from_rgba_unmultiplied(25, 18, 12, 54),
        })
}

pub fn dark_glass_frame(palette: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(palette.glass_dark)
        .stroke(egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 46),
        ))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::same(22))
        .shadow(egui::epaint::Shadow {
            offset: [0, 8],
            blur: 28,
            spread: 0,
            color: egui::Color32::from_rgba_unmultiplied(25, 18, 12, 40),
        })
}

pub fn input(
    ui: &mut egui::Ui,
    value: &mut String,
    hint: &str,
    password: bool,
    palette: &Palette,
) -> egui::Response {
    input_with_ime(ui, value, hint, password, true, palette)
}

pub fn input_ascii(
    ui: &mut egui::Ui,
    value: &mut String,
    hint: &str,
    password: bool,
    palette: &Palette,
) -> egui::Response {
    input_with_ime(ui, value, hint, password, false, palette)
}

fn input_with_ime(
    ui: &mut egui::Ui,
    value: &mut String,
    hint: &str,
    password: bool,
    allow_ime: bool,
    palette: &Palette,
) -> egui::Response {
    let height = if ui.ctx().content_rect().height() < 700.0 {
        38.0
    } else {
        44.0
    };
    let response = ui.add_sized(
        [ui.available_width(), height],
        egui::TextEdit::singleline(value)
            .password(password)
            .hint_text(egui::RichText::new(hint).color(palette.muted))
            .font(egui::TextStyle::Body)
            .text_color(palette.ink)
            .background_color(egui::Color32::WHITE)
            .margin(egui::Margin::symmetric(12, 10)),
    );
    if response.has_focus() || response.changed() {
        ui.ctx().request_repaint();
    }
    if response.has_focus() && !allow_ime {
        // Technical configuration fields are ASCII by definition. Disabling
        // the native IME for them prevents Chinese IMEs such as Sogou from
        // holding latin keystrokes in an external candidate window until the
        // user presses Enter. Human-facing aliases and filesystem paths keep
        // using `input`, so Chinese text input remains available there.
        ui.output_mut(|output| output.ime = None);
    }
    response
}

pub fn multiline(
    ui: &mut egui::Ui,
    value: &mut String,
    hint: &str,
    rows: usize,
    palette: &Palette,
) -> egui::Response {
    let compact = ui.ctx().content_rect().height() < 700.0;
    let response = ui.add_sized(
        [
            ui.available_width(),
            rows as f32 * if compact { 19.0 } else { 23.0 } + if compact { 16.0 } else { 24.0 },
        ],
        egui::TextEdit::multiline(value)
            .hint_text(egui::RichText::new(hint).color(palette.muted))
            .font(egui::TextStyle::Monospace)
            .text_color(palette.ink)
            .background_color(egui::Color32::WHITE)
            .margin(egui::Margin::same(12))
            .desired_rows(rows),
    );
    if response.has_focus() || response.changed() {
        ui.ctx().request_repaint();
    }
    response
}

pub fn multiline_ascii(
    ui: &mut egui::Ui,
    value: &mut String,
    hint: &str,
    rows: usize,
    palette: &Palette,
) -> egui::Response {
    let response = multiline(ui, value, hint, rows, palette);
    if response.has_focus() {
        ui.output_mut(|output| output.ime = None);
    }
    response
}

pub fn ascii_response(ui: &mut egui::Ui, response: &egui::Response) {
    if response.has_focus() {
        ui.output_mut(|output| output.ime = None);
        ui.ctx().request_repaint();
    }
}

pub fn primary_button(
    ui: &mut egui::Ui,
    label: impl Into<egui::WidgetText>,
    palette: &Palette,
) -> egui::Response {
    ui.add(
        egui::Button::new(label)
            .fill(palette.action)
            .stroke(egui::Stroke::NONE)
            .corner_radius(egui::CornerRadius::same(1))
            .min_size(egui::vec2(142.0, 46.0)),
    )
}

pub fn accent_button(
    ui: &mut egui::Ui,
    label: impl Into<egui::WidgetText>,
    palette: &Palette,
) -> egui::Response {
    ui.add(
        egui::Button::new(label)
            .fill(palette.accent)
            .stroke(egui::Stroke::NONE)
            .corner_radius(egui::CornerRadius::same(24))
            .min_size(egui::vec2(48.0, 48.0)),
    )
}

pub fn secondary_button(
    ui: &mut egui::Ui,
    label: impl Into<egui::WidgetText>,
    palette: &Palette,
) -> egui::Response {
    ui.add(
        egui::Button::new(label)
            .fill(egui::Color32::WHITE)
            .stroke(egui::Stroke::new(1.0_f32, palette.line))
            .corner_radius(egui::CornerRadius::same(8))
            .min_size(egui::vec2(112.0, 42.0)),
    )
}

pub fn elevated_control_frame(palette: &Palette, strong: bool) -> egui::Frame {
    egui::Frame::new()
        .fill(if strong {
            palette.paper
        } else {
            egui::Color32::from_rgba_unmultiplied(
                palette.paper.r(),
                palette.paper.g(),
                palette.paper.b(),
                126,
            )
        })
        .stroke(egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 156),
        ))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(6, 4))
        .shadow(egui::epaint::Shadow {
            offset: [0, if strong { 8 } else { 5 }],
            blur: if strong { 24 } else { 16 },
            spread: 0,
            color: egui::Color32::from_rgba_unmultiplied(32, 22, 16, if strong { 68 } else { 46 }),
        })
}

pub fn soft_card_shadow() -> egui::epaint::Shadow {
    egui::epaint::Shadow {
        offset: [0, 5],
        blur: 14,
        spread: 0,
        color: egui::Color32::from_rgba_unmultiplied(30, 21, 16, 34),
    }
}

pub fn eyebrow(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .font(egui::FontId::new(11.0, egui::FontFamily::Proportional))
            .strong()
            .color(color),
    );
}

pub fn field_label(ui: &mut egui::Ui, label: &str, detail: &str, palette: &Palette) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).strong().color(palette.ink));
        if !detail.is_empty() {
            ui.label(egui::RichText::new(detail).small().color(palette.muted));
        }
    });
}

pub fn pill(ui: &mut egui::Ui, text: &str, fill: egui::Color32, color: egui::Color32) {
    egui::Frame::new()
        .fill(fill)
        .corner_radius(egui::CornerRadius::same(16))
        .inner_margin(egui::Margin::symmetric(11, 5))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).small().strong().color(color));
        });
}
