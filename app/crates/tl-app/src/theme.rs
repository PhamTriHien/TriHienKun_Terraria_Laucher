use crate::assets::{ButtonSkin, LauncherAssets, UiIcon};
use egui::{
    epaint::{Mesh, Shadow, Vertex},
    pos2,
    style::WidgetVisuals,
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Frame, Margin, Rect,
    RichText, Sense, Shape, Stroke, TextStyle, TextureHandle, Vec2, Visuals,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tl_core::ThemeMode;

#[derive(Debug, Clone, Copy)]
pub struct ThemePalette {
    pub mode: ThemeMode,
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
    pub hero_subtitle: Color32,
    pub anim_shadow: Color32,
    pub thumb_fallback_bg: Color32,
}

pub fn ui_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn ui_font_strong(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("Strong".into()))
}

pub fn page_title(ui: &mut egui::Ui, p: ThemePalette, title: &str) {
    ui.label(
        RichText::new(title)
            .font(ui_font_strong(21.0))
            .color(p.text_primary),
    );
}

const fn rgba_const(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    let pr = ((r as u16 * a as u16 + 127) / 255) as u8;
    let pg = ((g as u16 * a as u16 + 127) / 255) as u8;
    let pb = ((b as u16 * a as u16 + 127) / 255) as u8;
    Color32::from_rgba_premultiplied(pr, pg, pb, a)
}

pub const DARK_PALETTE: ThemePalette = ThemePalette {
    mode: ThemeMode::Dark,
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
    accent_red: Color32::from_rgb(242, 86, 102),
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
    hero_subtitle: Color32::from_rgb(206, 217, 232),
    anim_shadow: Color32::from_black_alpha(85),
    thumb_fallback_bg: Color32::from_rgb(30, 41, 56),
};

pub const LIGHT_PALETTE: ThemePalette = ThemePalette {
    mode: ThemeMode::Light,
    bg_void: Color32::from_rgb(227, 233, 242),
    bg_panel: Color32::from_rgb(218, 226, 238),
    bg_card: Color32::from_rgb(239, 242, 247),
    bg_elevated: Color32::from_rgb(255, 255, 255),
    bg_console: Color32::from_rgb(242, 246, 252),
    border_subtle: Color32::from_rgb(184, 197, 214),
    border_strong: Color32::from_rgb(126, 146, 170),
    text_primary: Color32::from_rgb(15, 23, 42),
    text_muted: Color32::from_rgb(66, 80, 100),
    console_text: Color32::from_rgb(20, 32, 46),
    accent_emerald: Color32::from_rgb(8, 122, 64),
    accent_emerald_glow: Color32::from_rgb(14, 158, 86),
    accent_cyan: Color32::from_rgb(2, 110, 176),
    accent_amber: Color32::from_rgb(156, 84, 0),
    accent_red: Color32::from_rgb(196, 26, 54),
    nav_selected_fill: rgba_const(8, 122, 64, 38),
    nav_selected_text: Color32::from_rgb(6, 78, 40),
    faint_bg: Color32::from_rgb(225, 232, 242),
    hover_bg: Color32::from_rgb(235, 241, 249),
    hover_weak_bg: Color32::from_rgb(228, 236, 246),
    hover_fg: Color32::from_rgb(15, 23, 42),
    active_fg: Color32::WHITE,
    shadow_color: Color32::from_black_alpha(32),
    hero_scrim_top: rgba_const(15, 23, 42, 95),
    hero_scrim_bottom: rgba_const(15, 23, 42, 218),
    hero_title: Color32::from_rgb(248, 250, 252),
    hero_subtitle: Color32::from_rgb(218, 228, 240),
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
    let mut ui_loaded = false;

    for path_str in [
        r"C:\Windows\Fonts\segoeui.ttf",
        r"C:\Windows\Fonts\seguisb.ttf",
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
            ui_loaded = true;
            installed = true;
            break;
        }
    }

    let mut strong_list = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    for path_str in [
        r"C:\Windows\Fonts\seguisb.ttf",
        r"C:\Windows\Fonts\segoeuib.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(Path::new(path_str)) {
            fonts.font_data.insert(
                "tl_ui_strong".to_owned(),
                Arc::new(FontData::from_owned(bytes)),
            );
            strong_list.insert(0, "tl_ui_strong".to_owned());
            installed = true;
            break;
        }
    }
    if ui_loaded || !strong_list.is_empty() {
        fonts
            .families
            .insert(FontFamily::Name("Strong".into()), strong_list);
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

fn build_theme_style(ctx: &egui::Context, mode: ThemeMode) -> egui::Style {
    let egui_theme = if mode.is_dark() {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    let p = palette(mode);
    let mut style = (*ctx.style_of(egui_theme)).clone();

    let mut text_styles = BTreeMap::new();
    text_styles.insert(TextStyle::Heading, ui_font_strong(18.0));
    text_styles.insert(TextStyle::Name("Title".into()), ui_font_strong(21.0));
    text_styles.insert(TextStyle::Name("Subheading".into()), ui_font_strong(15.5));
    text_styles.insert(TextStyle::Body, ui_font(13.5));
    text_styles.insert(TextStyle::Button, ui_font_strong(13.0));
    text_styles.insert(
        TextStyle::Monospace,
        FontId::new(12.5, FontFamily::Monospace),
    );
    text_styles.insert(TextStyle::Small, ui_font(11.8));
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
    v.weak_text_color = Some(p.text_muted);
    v.panel_fill = p.bg_panel;
    v.window_fill = p.bg_card;
    v.extreme_bg_color = if mode.is_dark() {
        p.bg_console
    } else {
        p.bg_elevated
    };
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
    style
}

pub fn apply_theme(ctx: &egui::Context, mode: ThemeMode) {
    let dark_style = build_theme_style(ctx, ThemeMode::Dark);
    let light_style = build_theme_style(ctx, ThemeMode::Light);
    ctx.set_style_of(egui::Theme::Dark, dark_style);
    ctx.set_style_of(egui::Theme::Light, light_style);

    let active_theme = if mode.is_dark() {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    ctx.set_theme(active_theme);
}

#[derive(Clone, Copy)]
struct NineSliceSpec {
    uv_left: f32,
    uv_top: f32,
    uv_right: f32,
    uv_bottom: f32,
    px: [f32; 4],
}

const CARD_SLICE: NineSliceSpec = NineSliceSpec {
    uv_left: 58.0 / 320.0,
    uv_top: 54.0 / 240.0,
    uv_right: 58.0 / 320.0,
    uv_bottom: 54.0 / 240.0,
    px: [24.0, 22.0, 24.0, 22.0],
};

const SUBCARD_SLICE: NineSliceSpec = NineSliceSpec {
    uv_left: 18.0 / 320.0,
    uv_top: 16.0 / 192.0,
    uv_right: 18.0 / 320.0,
    uv_bottom: 16.0 / 192.0,
    px: [10.0, 9.0, 10.0, 9.0],
};

const BUTTON_SLICE: NineSliceSpec = NineSliceSpec {
    uv_left: 44.0 / 261.0,
    uv_top: 14.0 / 56.0,
    uv_right: 44.0 / 261.0,
    uv_bottom: 14.0 / 56.0,
    px: [18.0, 8.0, 18.0, 8.0],
};

fn build_nine_slice_mesh(
    tex: &TextureHandle,
    rect: Rect,
    spec: NineSliceSpec,
    tint: Color32,
) -> Mesh {
    let [mut bl, mut bt, mut br, mut bb] = spec.px;
    let w = rect.width().max(1.0);
    let h = rect.height().max(1.0);
    if bl + br > w {
        let s = w / (bl + br);
        bl *= s;
        br *= s;
    }
    if bt + bb > h {
        let s = h / (bt + bb);
        bt *= s;
        bb *= s;
    }

    let xs = [rect.left(), rect.left() + bl, rect.right() - br, rect.right()];
    let ys = [rect.top(), rect.top() + bt, rect.bottom() - bb, rect.bottom()];
    let us = [0.0, spec.uv_left, 1.0 - spec.uv_right, 1.0];
    let vs = [0.0, spec.uv_top, 1.0 - spec.uv_bottom, 1.0];

    let mut mesh = Mesh::with_texture(tex.id());
    for row in 0..4 {
        for col in 0..4 {
            mesh.vertices.push(Vertex {
                pos: pos2(xs[col], ys[row]),
                uv: pos2(us[col], vs[row]),
                color: tint,
            });
        }
    }
    for row in 0..3 {
        for col in 0..3 {
            let i0 = (row * 4 + col) as u32;
            let i1 = i0 + 1;
            let i2 = i0 + 4;
            let i3 = i2 + 1;
            mesh.indices.extend_from_slice(&[i0, i1, i2, i1, i3, i2]);
        }
    }
    mesh
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

pub fn rpg_card<R>(
    ui: &mut egui::Ui,
    assets: &LauncherAssets,
    p: ThemePalette,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    if let Some(tex) = assets.card_panel(p.mode) {
        let bg_idx = ui.painter().add(Shape::Noop);
        let resp = Frame::new()
            .inner_margin(Margin::symmetric(16, 15))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                add_contents(ui)
            });
        let mesh = build_nine_slice_mesh(tex, resp.response.rect, CARD_SLICE, Color32::WHITE);
        ui.painter().set(bg_idx, Shape::mesh(mesh));
        resp
    } else {
        card_frame(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
    }
}

pub fn rpg_subcard<R>(
    ui: &mut egui::Ui,
    assets: &LauncherAssets,
    p: ThemePalette,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    if let Some(tex) = assets.subcard_panel(p.mode) {
        let bg_idx = ui.painter().add(Shape::Noop);
        let resp = Frame::new()
            .inner_margin(Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                add_contents(ui)
            });
        let mesh = build_nine_slice_mesh(tex, resp.response.rect, SUBCARD_SLICE, Color32::WHITE);
        ui.painter().set(bg_idx, Shape::mesh(mesh));
        resp
    } else {
        subcard_frame(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
    }
}

pub fn console_frame(p: ThemePalette) -> Frame {
    Frame::new()
        .fill(p.bg_console)
        .stroke(Stroke::new(1.0, p.border_subtle))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
}

pub fn rpg_section_heading(
    ui: &mut egui::Ui,
    assets: &LauncherAssets,
    p: ThemePalette,
    icon: UiIcon,
    title: &str,
) {
    ui.horizontal(|ui| {
        if let Some(tex) = assets.ui_icon(icon) {
            ui.add(
                egui::Image::new((tex.id(), egui::vec2(20.0, 20.0)))
                    .fit_to_exact_size(egui::vec2(20.0, 20.0)),
            );
        }
        ui.label(
            RichText::new(title)
                .font(ui_font_strong(15.5))
                .color(p.text_primary),
        );
    });
}

pub fn status_pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 26);
    let stroke = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 145);
    Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).font(ui_font_strong(11.8)).color(color));
        });
}

#[derive(Clone, Copy)]
struct RpgBtnLayout {
    skin: ButtonSkin,
    height: f32,
    icon_size: f32,
    h_pad: f32,
    font_size: f32,
    highlighted: bool,
}

fn draw_rpg_tex_button(
    ui: &mut egui::Ui,
    assets: &LauncherAssets,
    p: ThemePalette,
    icon_tex: Option<&TextureHandle>,
    text: &str,
    cfg: RpgBtnLayout,
) -> egui::Response {
    let label_color = if cfg.skin == ButtonSkin::Amber {
        Color32::from_rgb(255, 252, 240)
    } else {
        Color32::from_rgb(248, 250, 252)
    };
    let galley = egui::WidgetText::from(RichText::new(text).font(ui_font_strong(cfg.font_size)))
        .into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Button);

    let gap = if icon_tex.is_some() && !text.is_empty() {
        6.0
    } else {
        0.0
    };
    let icon_w = if icon_tex.is_some() { cfg.icon_size } else { 0.0 };
    let content_w = icon_w + gap + galley.size().x;
    let total_w = (content_w + cfg.h_pad * 2.0).max(if text.is_empty() { 34.0 } else { 62.0 });
    let total_h = cfg.height.max(galley.size().y + 12.0);

    let (rect, resp) = ui.allocate_exact_size(Vec2::new(total_w, total_h), Sense::click());
    let enabled = ui.is_enabled();
    let down = enabled && resp.is_pointer_button_down_on();
    let hovered = enabled && resp.hovered();

    let draw_rect = if down {
        rect.translate(Vec2::new(0.0, 1.0))
    } else if hovered {
        rect.expand(0.5)
    } else {
        rect
    };

    let base_tint = if !enabled {
        Color32::from_rgba_unmultiplied(112, 118, 128, 232)
    } else if down {
        Color32::from_rgb(206, 208, 214)
    } else if hovered || cfg.highlighted {
        Color32::WHITE
    } else {
        Color32::from_rgb(238, 240, 244)
    };
    let tint = if enabled && cfg.skin == ButtonSkin::Amber {
        Color32::from_rgba_premultiplied(
            ((base_tint.r() as u16 * 218) / 255) as u8,
            ((base_tint.g() as u16 * 202) / 255) as u8,
            ((base_tint.b() as u16 * 178) / 255) as u8,
            base_tint.a(),
        )
    } else {
        base_tint
    };

    let painter = ui.painter();
    if let Some(bg_tex) = assets.button_skin(cfg.skin) {
        let mesh = build_nine_slice_mesh(bg_tex, draw_rect, BUTTON_SLICE, tint);
        painter.add(Shape::mesh(mesh));
    } else {
        let fallback_col = match cfg.skin {
            ButtonSkin::Emerald => p.accent_emerald,
            ButtonSkin::Cyan => p.accent_cyan,
            ButtonSkin::Amber => p.accent_amber,
            ButtonSkin::Red => p.accent_red,
            ButtonSkin::Slate => p.bg_elevated,
        };
        painter.rect_filled(draw_rect, CornerRadius::same(7), fallback_col);
    }

    if enabled && (cfg.highlighted || hovered) {
        let glow = match cfg.skin {
            ButtonSkin::Emerald => p.accent_emerald_glow,
            ButtonSkin::Cyan => p.accent_cyan,
            ButtonSkin::Amber => p.accent_amber,
            ButtonSkin::Red => p.accent_red,
            ButtonSkin::Slate => p.border_strong,
        };
        let alpha = if cfg.highlighted { 200 } else { 145 };
        painter.rect_stroke(
            draw_rect,
            CornerRadius::same(7),
            Stroke::new(1.3, Color32::from_rgba_unmultiplied(glow.r(), glow.g(), glow.b(), alpha)),
            egui::StrokeKind::Inside,
        );
    }

    let start_x = draw_rect.center().x - content_w * 0.5;
    if let Some(ico) = icon_tex {
        let ico_rect = Rect::from_center_size(
            pos2(start_x + cfg.icon_size * 0.5, draw_rect.center().y),
            Vec2::splat(cfg.icon_size),
        );
        let ico_tint = if enabled {
            Color32::WHITE
        } else {
            Color32::from_white_alpha(190)
        };
        painter.image(
            ico.id(),
            ico_rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            ico_tint,
        );
    }

    let text_pos = pos2(
        start_x + icon_w + gap,
        draw_rect.center().y - galley.size().y * 0.5,
    );
    let final_text_col = if enabled {
        label_color
    } else {
        Color32::from_rgba_unmultiplied(228, 234, 242, 220)
    };
    let shadow_alpha = if enabled { 205 } else { 155 };
    painter.galley(
        text_pos + Vec2::new(0.0, 1.1),
        galley.clone(),
        Color32::from_black_alpha(shadow_alpha),
    );
    painter.galley(text_pos, galley, final_text_col);

    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

pub fn rpg_button(
    ui: &mut egui::Ui,
    assets: &LauncherAssets,
    p: ThemePalette,
    skin: ButtonSkin,
    icon: UiIcon,
    text: &str,
) -> egui::Response {
    draw_rpg_tex_button(
        ui,
        assets,
        p,
        assets.ui_icon(icon),
        text,
        RpgBtnLayout {
            skin,
            height: 31.0,
            icon_size: 16.0,
            h_pad: 13.0,
            font_size: 13.0,
            highlighted: false,
        },
    )
}

pub fn rpg_tex_button(
    ui: &mut egui::Ui,
    assets: &LauncherAssets,
    p: ThemePalette,
    skin: ButtonSkin,
    icon_tex: Option<&TextureHandle>,
    text: &str,
    highlighted: bool,
) -> egui::Response {
    draw_rpg_tex_button(
        ui,
        assets,
        p,
        icon_tex,
        text,
        RpgBtnLayout {
            skin,
            height: 31.0,
            icon_size: 18.0,
            h_pad: 13.0,
            font_size: 13.0,
            highlighted,
        },
    )
}

pub fn rpg_selectable_button(
    ui: &mut egui::Ui,
    assets: &LauncherAssets,
    p: ThemePalette,
    active: bool,
    active_skin: ButtonSkin,
    icon: UiIcon,
    text: &str,
) -> egui::Response {
    draw_rpg_tex_button(
        ui,
        assets,
        p,
        assets.ui_icon(icon),
        text,
        RpgBtnLayout {
            skin: if active { active_skin } else { ButtonSkin::Slate },
            height: 29.0,
            icon_size: 15.0,
            h_pad: 11.0,
            font_size: 12.5,
            highlighted: active,
        },
    )
}

pub fn rpg_compact_button(
    ui: &mut egui::Ui,
    assets: &LauncherAssets,
    p: ThemePalette,
    skin: ButtonSkin,
    icon: Option<UiIcon>,
    text: &str,
) -> egui::Response {
    draw_rpg_tex_button(
        ui,
        assets,
        p,
        icon.and_then(|i| assets.ui_icon(i)),
        text,
        RpgBtnLayout {
            skin,
            height: 25.0,
            icon_size: 13.5,
            h_pad: 9.0,
            font_size: 11.8,
            highlighted: false,
        },
    )
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
