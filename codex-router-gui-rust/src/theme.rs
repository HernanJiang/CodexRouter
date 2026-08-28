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
    pub success: egui::Color32,
    pub danger: egui::Color32,
    pub warning: egui::Color32,
}

pub fn palette(name: &str) -> Palette {
    if name == "sky" {
        // Clear sky blue with opaque dialog surfaces.
        Palette {
            background: egui::Color32::from_rgb(142, 186, 214),
            background_dark: egui::Color32::from_rgb(78, 128, 162),
            background_light: egui::Color32::from_rgb(198, 224, 240),
            paper: egui::Color32::from_rgb(248, 252, 255),
            paper_alt: egui::Color32::from_rgb(232, 243, 250),
            ink: egui::Color32::from_rgb(14, 28, 42),
            ink_soft: egui::Color32::from_rgb(48, 72, 92),
            muted: egui::Color32::from_rgb(96, 122, 140),
            line: egui::Color32::from_rgb(196, 218, 232),
            action: egui::Color32::from_rgb(28, 72, 108),
            accent: egui::Color32::from_rgb(46, 118, 158),
            glass: egui::Color32::from_rgb(248, 252, 255),
            success: egui::Color32::from_rgb(29, 130, 89),
            danger: egui::Color32::from_rgb(190, 54, 51),
            warning: egui::Color32::from_rgb(212, 148, 28),
        }
    } else {
        // Coffee theme with the same opaque dialog treatment.
        Palette {
            background: egui::Color32::from_rgb(168, 136, 114),
            background_dark: egui::Color32::from_rgb(102, 74, 58),
            background_light: egui::Color32::from_rgb(214, 184, 156),
            paper: egui::Color32::from_rgb(252, 246, 236),
            paper_alt: egui::Color32::from_rgb(242, 230, 214),
            ink: egui::Color32::from_rgb(42, 29, 21),
            ink_soft: egui::Color32::from_rgb(83, 64, 52),
            muted: egui::Color32::from_rgb(125, 101, 84),
            line: egui::Color32::from_rgb(220, 200, 178),
            action: egui::Color32::from_rgb(61, 39, 27),
            accent: egui::Color32::from_rgb(82, 91, 67),
            glass: egui::Color32::from_rgb(252, 246, 236),
            success: egui::Color32::from_rgb(58, 119, 83),
            danger: egui::Color32::from_rgb(166, 66, 52),
            warning: egui::Color32::from_rgb(201, 155, 61),
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
    style.visuals.window_stroke = egui::Stroke::new(1.0, palette.line);
    style.visuals.window_corner_radius = egui::CornerRadius::same(14);
    style.visuals.window_shadow = soft_card_shadow();
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
    // Soft acrylic-style light wells: translucent orbs that read as frosted glass.
    let orbs = [
        (
            rect.left_top() + egui::vec2(rect.width() * 0.18, rect.height() * 0.22),
            rect.width().max(rect.height()) * 0.42,
            palette.background_light,
            58,
        ),
        (
            rect.right_top() + egui::vec2(-rect.width() * 0.12, rect.height() * 0.48),
            rect.width().max(rect.height()) * 0.36,
            egui::Color32::WHITE,
            34,
        ),
        (
            rect.center_bottom() + egui::vec2(-rect.width() * 0.08, -rect.height() * 0.08),
            rect.width().max(rect.height()) * 0.48,
            palette.background_dark,
            28,
        ),
    ];
    for (center, radius, color, alpha) in orbs {
        painter.circle_filled(
            center,
            radius,
            egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha),
        );
    }
    // Top sheen to sell the acrylic depth without heavy blur cost.
    let sheen = egui::Rect::from_min_max(
        rect.left_top(),
        egui::pos2(rect.right(), rect.top() + rect.height() * 0.34),
    );
    painter.rect_filled(
        sheen,
        0.0,
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 22),
    );
}

pub fn paper_frame(palette: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(palette.paper)
        .stroke(egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 196),
        ))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::same(28))
        .shadow(egui::epaint::Shadow {
            offset: [0, 12],
            blur: 36,
            spread: 0,
            color: egui::Color32::from_rgba_unmultiplied(20, 28, 40, 48),
        })
}

pub fn glass_frame(palette: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(palette.paper)
        .stroke(egui::Stroke::new(1.0_f32, palette.line))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(egui::Margin::same(26))
        .shadow(soft_card_shadow())
}

pub fn dialog_window_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(egui::Color32::TRANSPARENT)
        .inner_margin(egui::Margin::ZERO)
}

pub fn dialog_shell<R>(
    ui: &mut egui::Ui,
    palette: &Palette,
    header: impl FnOnce(&mut egui::Ui),
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let mut body_result = None;
    egui::Frame::new()
        .fill(palette.paper)
        .stroke(egui::Stroke::new(1.0, palette.line))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(egui::Margin::ZERO)
        .shadow(soft_card_shadow())
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
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
                    ui.set_min_width(ui.available_width());
                    header(ui);
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
                    ui.set_min_width(ui.available_width());
                    body_result = Some(body(ui));
                });
        });
    body_result.expect("dialog body should be rendered")
}

pub fn dialog_title(ui: &mut egui::Ui, title: &str) {
    ui.label(
        egui::RichText::new(title)
            .font(egui::FontId::new(22.0, display_family()))
            .strong()
            .color(egui::Color32::WHITE),
    );
}

pub fn dark_glass_frame(palette: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(palette.background_dark)
        .stroke(egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 58),
        ))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::same(22))
        .shadow(egui::epaint::Shadow {
            offset: [0, 10],
            blur: 34,
            spread: 0,
            color: egui::Color32::from_rgba_unmultiplied(16, 22, 30, 46),
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
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(label).strong().color(palette.ink));
        if !detail.is_empty() {
            ui.label(egui::RichText::new(detail).small().color(palette.muted));
        }
    });
}

pub fn stacked_field_label(ui: &mut egui::Ui, label: &str, detail: &str, palette: &Palette) {
    ui.label(egui::RichText::new(label).strong().color(palette.ink));
    if !detail.is_empty() {
        ui.label(egui::RichText::new(detail).small().color(palette.muted));
    }
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
