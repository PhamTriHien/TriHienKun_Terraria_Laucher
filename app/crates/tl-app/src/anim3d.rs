use crate::theme::ThemePalette;
use egui::{
    epaint::{Mesh, Vertex},
    pos2, Color32, CornerRadius, Pos2, Rect, Sense, Shape, Stroke, TextureHandle, Vec2,
};
use std::f32::consts::TAU;
use tl_core::GameEdition;

pub struct NavEmblemState {
    pub selected: bool,
    pub hovered: bool,
}

fn with_alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

fn full_uv() -> Rect {
    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
}

fn draw_spinning_arc(
    ui: &egui::Ui,
    painter: &egui::Painter,
    center: Pos2,
    radius: f32,
    color: Color32,
) {
    let time = ui.input(|i| i.time) as f32;
    ui.ctx().request_repaint();

    const SEGMENTS: usize = 14;
    let start = time * 3.4;
    let sweep = TAU * 0.28;
    let mut pts = Vec::with_capacity(SEGMENTS + 1);
    for i in 0..=SEGMENTS {
        let a = start + (i as f32 / SEGMENTS as f32) * sweep;
        pts.push(pos2(center.x + a.cos() * radius, center.y + a.sin() * radius));
    }
    painter.add(Shape::line(pts, Stroke::new(1.8, with_alpha(color, 215))));
}

fn render_header_badge(
    ui: &mut egui::Ui,
    p: ThemePalette,
    tex: Option<&TextureHandle>,
    accent: Option<Color32>,
    spinning: bool,
) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), Sense::hover());
    let painter = ui.painter_at(rect.expand(3.0));
    let cr = CornerRadius::same(9);

    painter.rect_filled(rect.translate(Vec2::new(0.0, 1.0)), cr, p.anim_shadow);
    painter.rect_filled(rect, cr, p.bg_elevated);

    if let Some(col) = accent {
        painter.rect_filled(rect, cr, with_alpha(col, 24));
        painter.rect_stroke(
            rect,
            cr,
            Stroke::new(1.2, with_alpha(col, 165)),
            egui::StrokeKind::Inside,
        );
    } else {
        painter.rect_stroke(
            rect,
            cr,
            Stroke::new(1.0, p.border_subtle),
            egui::StrokeKind::Inside,
        );
    }

    if let Some(t) = tex {
        let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(24.0));
        painter.image(t.id(), icon_rect, full_uv(), Color32::WHITE);
    }

    if spinning {
        draw_spinning_arc(
            ui,
            &painter,
            rect.center(),
            16.5,
            accent.unwrap_or(p.accent_cyan),
        );
    }
}

pub fn draw_sidebar_brand_stage(
    ui: &mut egui::Ui,
    p: ThemePalette,
    brand_tex: Option<&TextureHandle>,
    game_running: bool,
    server_running: bool,
) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), Sense::hover());
    let painter = ui.painter_at(rect.expand(4.0));
    let cr = CornerRadius::same(10);

    painter.rect_filled(rect.translate(Vec2::new(0.0, 1.0)), cr, p.anim_shadow);
    painter.rect_filled(rect, cr, p.bg_elevated);

    if game_running || server_running {
        let active_col = if game_running {
            p.accent_emerald_glow
        } else {
            p.accent_cyan
        };
        painter.rect_stroke(
            rect.expand(1.5),
            CornerRadius::same(11),
            Stroke::new(2.0, with_alpha(active_col, 42)),
            egui::StrokeKind::Outside,
        );
        painter.rect_stroke(
            rect,
            cr,
            Stroke::new(1.3, with_alpha(active_col, 175)),
            egui::StrokeKind::Inside,
        );
    } else {
        painter.rect_stroke(
            rect,
            cr,
            Stroke::new(1.0, p.border_subtle),
            egui::StrokeKind::Inside,
        );
    }

    if let Some(tex) = brand_tex {
        let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(32.0));
        painter.image(tex.id(), icon_rect, full_uv(), Color32::WHITE);
    } else {
        painter.circle_filled(rect.center(), 10.0, p.accent_emerald);
    }
}

pub fn draw_nav_emblem_3d(
    painter: &egui::Painter,
    center: Pos2,
    size: f32,
    tex: Option<&TextureHandle>,
    p: ThemePalette,
    nav: NavEmblemState,
) {
    if nav.selected {
        painter.circle_filled(center, size * 0.46, with_alpha(p.accent_emerald_glow, 32));
        painter.circle_stroke(
            center,
            size * 0.46,
            Stroke::new(1.0, with_alpha(p.accent_emerald_glow, 95)),
        );
    } else if nav.hovered {
        painter.circle_filled(center, size * 0.44, with_alpha(p.accent_emerald, 20));
    }

    if let Some(t) = tex {
        let icon_rect = Rect::from_center_size(center, Vec2::splat(size * 0.78));
        painter.image(t.id(), icon_rect, full_uv(), Color32::WHITE);
    }
}

pub fn render_play_hero_3d(
    ui: &mut egui::Ui,
    p: ThemePalette,
    banner: Option<&TextureHandle>,
    edition_icon: Option<&TextureHandle>,
    edition: GameEdition,
    game_running: bool,
    installed_mod_count: usize,
) {
    let width = ui.available_width().max(320.0);
    let height = 124.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), Sense::hover());

    let painter = ui.painter_at(rect);
    let cr = CornerRadius::same(11);

    if let Some(tex) = banner {
        let [tw, th] = tex.size();
        let tex_aspect = (tw.max(1) as f32) / (th.max(1) as f32);
        let rect_aspect = rect.width() / rect.height().max(1.0);
        let uv = if rect_aspect > tex_aspect {
            let v_span = (tex_aspect / rect_aspect).clamp(0.1, 1.0);
            let v0 = (1.0 - v_span) * 0.45;
            Rect::from_min_max(pos2(0.0, v0), pos2(1.0, v0 + v_span))
        } else {
            let u_span = (rect_aspect / tex_aspect).clamp(0.1, 1.0);
            let u0 = (1.0 - u_span) * 0.5;
            Rect::from_min_max(pos2(u0, 0.0), pos2(u0 + u_span, 1.0))
        };
        painter.image(tex.id(), rect, uv, Color32::WHITE);
    } else {
        painter.rect_filled(rect, cr, p.bg_card);
    }

    let mut scrim = Mesh::default();
    scrim.vertices.extend_from_slice(&[
        Vertex {
            pos: rect.left_top(),
            uv: Pos2::ZERO,
            color: p.hero_scrim_top,
        },
        Vertex {
            pos: rect.right_top(),
            uv: Pos2::ZERO,
            color: p.hero_scrim_top,
        },
        Vertex {
            pos: rect.left_bottom(),
            uv: Pos2::ZERO,
            color: p.hero_scrim_bottom,
        },
        Vertex {
            pos: rect.right_bottom(),
            uv: Pos2::ZERO,
            color: p.hero_scrim_bottom,
        },
    ]);
    scrim.indices.extend_from_slice(&[0, 1, 2, 1, 3, 2]);
    painter.add(Shape::mesh(scrim));

    let border_col = if game_running {
        with_alpha(p.accent_emerald_glow, 175)
    } else {
        with_alpha(p.border_strong, 170)
    };
    painter.rect_stroke(
        rect,
        cr,
        Stroke::new(1.2, border_col),
        egui::StrokeKind::Inside,
    );

    let badge_center = pos2(rect.left() + 52.0, rect.center().y);
    let badge_rect = Rect::from_center_size(badge_center, Vec2::splat(64.0));
    painter.rect_filled(
        badge_rect,
        CornerRadius::same(12),
        Color32::from_black_alpha(92),
    );
    let badge_stroke = if game_running {
        Stroke::new(1.3, with_alpha(p.accent_emerald_glow, 175))
    } else {
        Stroke::new(1.0, Color32::from_white_alpha(48))
    };
    painter.rect_stroke(
        badge_rect,
        CornerRadius::same(12),
        badge_stroke,
        egui::StrokeKind::Inside,
    );

    if let Some(icon) = edition_icon {
        let icon_rect = Rect::from_center_size(badge_center, Vec2::splat(50.0));
        painter.image(icon.id(), icon_rect, full_uv(), Color32::WHITE);
    }

    painter.text(
        pos2(rect.left() + 100.0, rect.center().y - 12.0),
        egui::Align2::LEFT_CENTER,
        edition.label(),
        crate::theme::ui_font_strong(20.5),
        p.hero_title,
    );

    let subtitle = if game_running {
        "Đang chạy".to_string()
    } else if edition == GameEdition::TModLoader {
        format!("{installed_mod_count} mod đã cài")
    } else {
        "Sẵn sàng khởi chạy".to_string()
    };
    painter.text(
        pos2(rect.left() + 100.0, rect.center().y + 13.0),
        egui::Align2::LEFT_CENTER,
        subtitle,
        crate::theme::ui_font(13.0),
        p.hero_subtitle,
    );
}

pub fn render_server_beacon_3d(
    ui: &mut egui::Ui,
    p: ThemePalette,
    tex: Option<&TextureHandle>,
    active: bool,
    starting: bool,
) {
    let accent = if starting {
        Some(p.accent_cyan)
    } else if active {
        Some(p.accent_emerald)
    } else {
        None
    };
    render_header_badge(ui, p, tex, accent, starting);
}

pub fn render_mods_scanner_3d(
    ui: &mut egui::Ui,
    p: ThemePalette,
    tex: Option<&TextureHandle>,
    busy: bool,
) {
    render_header_badge(ui, p, tex, busy.then_some(p.accent_cyan), busy);
}

pub fn render_settings_gears_3d(
    ui: &mut egui::Ui,
    p: ThemePalette,
    tex: Option<&TextureHandle>,
    busy: bool,
) {
    render_header_badge(ui, p, tex, busy.then_some(p.accent_amber), busy);
}
