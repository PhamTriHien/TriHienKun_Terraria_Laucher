use egui::{
    epaint::Shadow, style::WidgetVisuals, Color32, CornerRadius, FontData, FontDefinitions,
    FontFamily, FontId, Frame, Margin, RichText, Stroke, TextStyle, Visuals,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tl_core::ThemeMode;

#[derive(Debug, Clone, Copy)]
pub struct ThemePalette {
    pub bg_void: Color32,
    pub bg_panel: Color32,
    pub bg_card: Color32,
    pub bg_elevated: Color32,
    pub bg_console: Color32,
    pub border_subtle: Color32,
    pub border_strong: Color32,
    pub text_primary: Color32,
    pub text_muted: Color32,
    pub console_text: Color32,
    pub accent_emerald: Color32,
    pub accent_emerald_glow: Color32,
    pub accent_cyan: Color32,
    pub accent_amber: Color32,
    pub accent_red: Color32,
    pub primary_btn_text: Color32,
    pub nav_selected_fill: Color32,
    pub nav_selected_text: Color32,
    pub faint_bg: Color32,
    pub hover_bg: Color32,
    pub hover_weak_bg: Color32,
    pub hover_fg: Color32,
    pub active_fg: Color32,
    pub shadow_color: Color32,
    pub hero_scrim_top: Color32,
    pub hero_scrim_bottom: Color32,
    pub hero_title: Color32,
    pub anim_shadow: Color32,
    pub thumb_fallback_bg: Color32,
}

const fn rgba_const(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    let pr = ((r as u16 * a as u16 + 127) / 255) as u8;
    let pg = ((g as u16 * a as u16 + 127) / 255) as u8;
    let pb = ((b as u16 * a as u16 + 127) / 255) as u8;
    Color32::from_rgba_premultiplied(pr, pg, pb, a)
}

pub const DARK_PALETTE: ThemePalette = ThemePalette {
    bg_void: Color32::from_rgb(13, 17, 23),
    bg_panel: Color32::from_rgb(18, 24, 33),
    bg_card: Color32::from_rgb(24, 32, 44),
    bg_elevated: Color32::from_rgb(31, 41, 56),
    bg_console: Color32::from_rgb(11, 15, 20),
    border_subtle: Color32::from_rgb(48, 63, 84),
    border_strong: Color32::from_rgb(72, 95, 126),
    text_primary: Color32::from_rgb(236, 242, 248),
    text_muted: Color32::from_rgb(148, 165, 186),
    console_text: Color32::from_rgb(214, 224, 236),
    accent_emerald: Color32::from_rgb(24, 188, 108),
    accent_emerald_glow: Color32::from_rgb(72, 224, 148),
    accent_cyan: Color32::from_rgb(38, 178, 238),
    accent_amber: Color32::from_rgb(238, 162, 32),
    accent_red: Color32::from_rgb(225, 64, 84),
    primary_btn_text: Color32::from_rgb(10, 22, 16),
    nav_selected_fill: rgba_const(24, 188, 108, 38),
    nav_selected_text: Color32::WHITE,
    faint_bg: Color32::from_rgb(28, 37, 50),
    hover_bg: Color32::from_rgb(40, 54, 74),
    hover_weak_bg: Color32::from_rgb(35, 47, 65),
    hover_fg: Color32::WHITE,
    active_fg: Color32::WHITE,
    shadow_color: Color32::from_black_alpha(110),
    hero_scrim_top: rgba_const(11, 16, 23, 105),
    hero_scrim_bottom: rgba_const(11, 16, 23, 224),
    hero_title: Color32::from_rgb(244, 248, 252),
    anim_shadow: Color32::from_black_alpha(85),
    thumb_fallback_bg: Color32::from_rgb(30, 41, 56),
};

pub const LIGHT_PALETTE: ThemePalette = ThemePalette {
    bg_void: Color32::from_rgb(242, 245, 249),
    bg_panel: Color32::from_rgb(230, 236, 244),
    bg_card: Color32::from_rgb(255, 255, 255),
    bg_elevated: Color32::from_rgb(236, 241, 248),
    bg_console: Color32::from_rgb(235, 240, 247),
    border_subtle: Color32::from_rgb(203, 213, 225),
    border_strong: Color32::from_rgb(148, 163, 184),
    text_primary: Color32::from_rgb(15, 23, 42),
    text_muted: Color32::from_rgb(71, 85, 105),
    console_text: Color32::from_rgb(22, 34, 48),
    accent_emerald: Color32::from_rgb(13, 148, 82),
    accent_emerald_glow: Color32::from_rgb(16, 178, 100),
    accent_cyan: Color32::from_rgb(2, 128, 198),
    accent_amber: Color32::from_rgb(186, 114, 4),
    accent_red: Color32::from_rgb(200, 30, 58),
    primary_btn_text: Color32::WHITE,
    nav_selected_fill: rgba_const(13, 148, 82, 34),
    nav_selected_text: Color32::from_rgb(8, 92, 50),
    faint_bg: Color32::from_rgb(226, 232, 240),
    hover_bg: Color32::from_rgb(218, 228, 240),
    hover_weak_bg: Color32::from_rgb(224, 233, 244),
    hover_fg: Color32::from_rgb(15, 23, 42),
    active_fg: Color32::WHITE,
    shadow_color: Color32::from_black_alpha(32),
    hero_scrim_top: rgba_const(15, 23, 42, 95),
    hero_scrim_bottom: rgba_const(15, 23, 42, 218),
    hero_title: Color32::from_rgb(248, 250, 252),
    anim_shadow: rgba_const(15, 23, 42, 42),
    thumb_fallback_bg: Color32::from_rgb(220, 228, 238),
};

pub fn palette(mode: ThemeMode) -> ThemePalette {
    match mode {
        ThemeMode::Dark => DARK_PALETTE,
        ThemeMode::Light => LIGHT_PALETTE,
    }
}

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

pub fn apply_theme(ctx: &egui::Context, mode: ThemeMode) {
    let egui_theme = if mode.is_dark() {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    ctx.set_theme(egui_theme);

    let p = palette(mode);
    let mut style = (*ctx.style_of(egui_theme)).clone();

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

    let mut v = if mode.is_dark() {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.override_text_color = Some(p.text_primary);
    v.panel_fill = p.bg_panel;
    v.window_fill = p.bg_card;
    v.extreme_bg_color = p.bg_console;
    v.faint_bg_color = p.faint_bg;
    v.code_bg_color = p.bg_console;
    v.window_corner_radius = cr_card;
    v.menu_corner_radius = cr_med;
    v.window_stroke = Stroke::new(1.2, p.border_strong);
    v.window_shadow = Shadow {
        offset: [0, 6],
        blur: 16,
        spread: 0,
        color: p.shadow_color,
    };
    v.selection.bg_fill = Color32::from_rgba_unmultiplied(
        p.accent_emerald.r(),
        p.accent_emerald.g(),
        p.accent_emerald.b(),
        68,
    );
    v.selection.stroke = Stroke::new(1.4, p.accent_emerald_glow);
    v.text_cursor.stroke = Stroke::new(2.0, p.accent_emerald_glow);
    v.hyperlink_color = p.accent_cyan;
    v.warn_fg_color = p.accent_amber;
    v.error_fg_color = p.accent_red;
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);

    v.widgets.noninteractive = WidgetVisuals {
        bg_fill: p.bg_card,
        weak_bg_fill: p.bg_panel,
        bg_stroke: Stroke::new(1.0, p.border_subtle),
        corner_radius: cr_med,
        fg_stroke: Stroke::new(1.0, p.text_muted),
        expansion: 0.0,
    };
    v.widgets.inactive = WidgetVisuals {
        bg_fill: p.bg_elevated,
        weak_bg_fill: p.faint_bg,
        bg_stroke: Stroke::new(1.0, p.border_subtle),
        corner_radius: cr_small,
        fg_stroke: Stroke::new(1.0, p.text_primary),
        expansion: 0.0,
    };
    v.widgets.hovered = WidgetVisuals {
        bg_fill: p.hover_bg,
        weak_bg_fill: p.hover_weak_bg,
        bg_stroke: Stroke::new(1.4, p.accent_emerald),
        corner_radius: cr_small,
        fg_stroke: Stroke::new(1.2, p.hover_fg),
        expansion: 0.5,
    };
    v.widgets.active = WidgetVisuals {
        bg_fill: p.accent_emerald,
        weak_bg_fill: p.accent_emerald,
        bg_stroke: Stroke::new(1.4, p.accent_emerald_glow),
        corner_radius: cr_small,
        fg_stroke: Stroke::new(1.4, p.active_fg),
        expansion: 0.0,
    };
    v.widgets.open = WidgetVisuals {
        bg_fill: p.hover_bg,
        weak_bg_fill: p.hover_weak_bg,
        bg_stroke: Stroke::new(1.2, p.accent_cyan),
        corner_radius: cr_small,
        fg_stroke: Stroke::new(1.2, p.hover_fg),
        expansion: 0.0,
    };

    style.visuals = v;
    ctx.set_style_of(egui_theme, style);
}

pub fn card_frame(p: ThemePalette) -> Frame {
    Frame::new()
        .fill(p.bg_card)
        .stroke(Stroke::new(1.0, p.border_subtle))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
}

pub fn subcard_frame(p: ThemePalette) -> Frame {
    Frame::new()
        .fill(p.bg_panel)
        .stroke(Stroke::new(1.0, p.border_subtle))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
}

pub fn console_frame(p: ThemePalette) -> Frame {
    Frame::new()
        .fill(p.bg_console)
        .stroke(Stroke::new(1.0, p.border_subtle))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
}

pub fn section_heading(ui: &mut egui::Ui, p: ThemePalette, title: &str) {
    ui.label(
        RichText::new(title)
            .size(15.5)
            .strong()
            .color(p.text_primary),
    );
}

pub fn status_pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 30);
    let stroke = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 145);
    Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.8).strong().color(color));
        });
}

pub fn primary_button(ui: &mut egui::Ui, p: ThemePalette, text: &str) -> egui::Response {
    let btn = egui::Button::new(RichText::new(text).strong().color(p.primary_btn_text))
        .fill(p.accent_emerald)
        .stroke(Stroke::new(1.0, p.accent_emerald_glow))
        .corner_radius(CornerRadius::same(7));
    ui.add(btn)
}

pub fn accent_button(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 38);
    let btn = egui::Button::new(RichText::new(text).strong().color(color))
        .fill(fill)
        .stroke(Stroke::new(1.0, color))
        .corner_radius(CornerRadius::same(7));
    ui.add(btn)
}

pub fn danger_button(ui: &mut egui::Ui, p: ThemePalette, text: &str) -> egui::Response {
    accent_button(ui, text, p.accent_red)
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
