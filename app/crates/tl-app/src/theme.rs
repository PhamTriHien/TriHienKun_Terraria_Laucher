use egui::{
    epaint::Shadow, style::WidgetVisuals, Color32, CornerRadius, FontData, FontDefinitions,
    FontFamily, FontId, Frame, IconData, Margin, RichText, Stroke, TextStyle, Visuals,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

pub const BG_VOID: Color32 = Color32::from_rgb(13, 17, 23);
pub const BG_PANEL: Color32 = Color32::from_rgb(18, 24, 33);
pub const BG_CARD: Color32 = Color32::from_rgb(24, 32, 44);
pub const BG_ELEVATED: Color32 = Color32::from_rgb(31, 41, 56);
pub const BG_CONSOLE: Color32 = Color32::from_rgb(11, 15, 20);
pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(48, 63, 84);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(72, 95, 126);

pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(236, 242, 248);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(148, 165, 186);

pub const ACCENT_EMERALD: Color32 = Color32::from_rgb(24, 188, 108);
pub const ACCENT_EMERALD_GLOW: Color32 = Color32::from_rgb(72, 224, 148);
pub const ACCENT_CYAN: Color32 = Color32::from_rgb(38, 178, 238);
pub const ACCENT_AMBER: Color32 = Color32::from_rgb(238, 162, 32);
pub const ACCENT_RED: Color32 = Color32::from_rgb(225, 64, 84);

pub fn install_unicode_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut installed = false;

    for path_str in [
        r"C:\Windows\Fonts\seguisb.ttf",
        r"C:\Windows\Fonts\segoeui.ttf",
        r"C:\Windows\Fonts\arial.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(Path::new(path_str)) {
            fonts
                .font_data
                .insert("tl_ui".to_owned(), Arc::new(FontData::from_owned(bytes)));
            if let Some(prop) = fonts.families.get_mut(&FontFamily::Proportional) {
                prop.insert(0, "tl_ui".to_owned());
            }
            if let Some(mono) = fonts.families.get_mut(&FontFamily::Monospace) {
                mono.push("tl_ui".to_owned());
            }
            installed = true;
            break;
        }
    }

    for path_str in [
        r"C:\Windows\Fonts\CascadiaMono.ttf",
        r"C:\Windows\Fonts\consola.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(Path::new(path_str)) {
            fonts
                .font_data
                .insert("tl_mono".to_owned(), Arc::new(FontData::from_owned(bytes)));
            if let Some(mono) = fonts.families.get_mut(&FontFamily::Monospace) {
                mono.insert(0, "tl_mono".to_owned());
            }
            installed = true;
            break;
        }
    }

    if installed {
        ctx.set_fonts(fonts);
    }
}

pub fn apply_theme(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();

    let mut text_styles = BTreeMap::new();
    text_styles.insert(
        TextStyle::Heading,
        FontId::new(19.0, FontFamily::Proportional),
    );
    text_styles.insert(
        TextStyle::Name("Title".into()),
        FontId::new(22.0, FontFamily::Proportional),
    );
    text_styles.insert(
        TextStyle::Name("Subheading".into()),
        FontId::new(15.0, FontFamily::Proportional),
    );
    text_styles.insert(TextStyle::Body, FontId::new(13.5, FontFamily::Proportional));
    text_styles.insert(
        TextStyle::Button,
        FontId::new(13.5, FontFamily::Proportional),
    );
    text_styles.insert(
        TextStyle::Monospace,
        FontId::new(12.5, FontFamily::Monospace),
    );
    text_styles.insert(TextStyle::Small, FontId::new(11.5, FontFamily::Proportional));
    style.text_styles = text_styles;

    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.spacing.window_margin = Margin::same(14);
    style.spacing.indent = 16.0;
    style.spacing.scroll.bar_width = 7.0;
    style.spacing.scroll.floating = false;

    let cr_small = CornerRadius::same(7);
    let cr_med = CornerRadius::same(9);
    let cr_card = CornerRadius::same(11);

    let mut v = Visuals::dark();
    v.override_text_color = Some(TEXT_PRIMARY);
    v.panel_fill = BG_PANEL;
    v.window_fill = BG_CARD;
    v.extreme_bg_color = BG_CONSOLE;
    v.faint_bg_color = Color32::from_rgb(28, 37, 50);
    v.code_bg_color = BG_CONSOLE;
    v.window_corner_radius = cr_card;
    v.menu_corner_radius = cr_med;
    v.window_stroke = Stroke::new(1.2, BORDER_STRONG);
    v.window_shadow = Shadow {
        offset: [0, 6],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(110),
    };
    v.selection.bg_fill = Color32::from_rgba_unmultiplied(24, 188, 108, 72);
    v.selection.stroke = Stroke::new(1.4, ACCENT_EMERALD_GLOW);
    v.text_cursor.stroke = Stroke::new(2.0, ACCENT_EMERALD_GLOW);
    v.hyperlink_color = ACCENT_CYAN;
    v.warn_fg_color = ACCENT_AMBER;
    v.error_fg_color = ACCENT_RED;
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);

    v.widgets.noninteractive = WidgetVisuals {
        bg_fill: BG_CARD,
        weak_bg_fill: BG_PANEL,
        bg_stroke: Stroke::new(1.0, BORDER_SUBTLE),
        corner_radius: cr_med,
        fg_stroke: Stroke::new(1.0, TEXT_MUTED),
        expansion: 0.0,
    };
    v.widgets.inactive = WidgetVisuals {
        bg_fill: BG_ELEVATED,
        weak_bg_fill: Color32::from_rgb(27, 36, 49),
        bg_stroke: Stroke::new(1.0, BORDER_SUBTLE),
        corner_radius: cr_small,
        fg_stroke: Stroke::new(1.0, TEXT_PRIMARY),
        expansion: 0.0,
    };
    v.widgets.hovered = WidgetVisuals {
        bg_fill: Color32::from_rgb(40, 54, 74),
        weak_bg_fill: Color32::from_rgb(35, 47, 65),
        bg_stroke: Stroke::new(1.4, ACCENT_EMERALD),
        corner_radius: cr_small,
        fg_stroke: Stroke::new(1.2, Color32::WHITE),
        expansion: 0.5,
    };
    v.widgets.active = WidgetVisuals {
        bg_fill: ACCENT_EMERALD,
        weak_bg_fill: ACCENT_EMERALD,
        bg_stroke: Stroke::new(1.4, ACCENT_EMERALD_GLOW),
        corner_radius: cr_small,
        fg_stroke: Stroke::new(1.4, Color32::WHITE),
        expansion: 0.0,
    };
    v.widgets.open = WidgetVisuals {
        bg_fill: Color32::from_rgb(38, 52, 70),
        weak_bg_fill: Color32::from_rgb(33, 45, 62),
        bg_stroke: Stroke::new(1.2, ACCENT_CYAN),
        corner_radius: cr_small,
        fg_stroke: Stroke::new(1.2, Color32::WHITE),
        expansion: 0.0,
    };

    style.visuals = v;
    ctx.set_style_of(egui::Theme::Dark, style);
}

pub fn card_frame() -> Frame {
    Frame::new()
        .fill(BG_CARD)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
}

pub fn subcard_frame() -> Frame {
    Frame::new()
        .fill(BG_PANEL)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
}

pub fn console_frame() -> Frame {
    Frame::new()
        .fill(BG_CONSOLE)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
}

pub fn section_heading(ui: &mut egui::Ui, title: &str) {
    ui.label(
        RichText::new(title)
            .size(15.5)
            .strong()
            .color(TEXT_PRIMARY),
    );
}

pub fn status_pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 32);
    let stroke = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 140);
    Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.8).strong().color(color));
        });
}

pub fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let btn = egui::Button::new(RichText::new(text).strong().color(Color32::from_rgb(10, 22, 16)))
        .fill(ACCENT_EMERALD)
        .stroke(Stroke::new(1.0, ACCENT_EMERALD_GLOW))
        .corner_radius(CornerRadius::same(7));
    ui.add(btn)
}

pub fn accent_button(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 44);
    let btn = egui::Button::new(RichText::new(text).strong().color(color))
        .fill(fill)
        .stroke(Stroke::new(1.0, color))
        .corner_radius(CornerRadius::same(7));
    ui.add(btn)
}

pub fn danger_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    accent_button(ui, text, ACCENT_RED)
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

pub fn load_window_icon(path: &Path) -> Option<IconData> {
    let img = image::open(path).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(IconData {
        rgba: rgba.into_raw(),
        width,
        height,
    })
}
