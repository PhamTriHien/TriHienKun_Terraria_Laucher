use crate::theme::ThemePalette;
use egui::{
    epaint::{Mesh, PathShape, Vertex},
    pos2, Color32, CornerRadius, Pos2, Rect, Sense, Shape, Stroke, TextureHandle, Vec2,
};
use std::f32::consts::TAU;
use tl_core::GameEdition;

#[derive(Clone, Copy)]
struct Vec3 {
    x: f32,
    y: f32,
    z: f32,
}

#[derive(Clone, Copy)]
struct Rot3 {
    rx: f32,
    ry: f32,
    rz: f32,
}

impl Rot3 {
    const fn new(rx: f32, ry: f32, rz: f32) -> Self {
        Self { rx, ry, rz }
    }
}

struct OrbitStyle {
    front: Color32,
    back: Color32,
    dot: Color32,
    phase: f32,
}

struct CubeColors {
    front: Color32,
    back: Color32,
    fill: Color32,
}

struct GearSpec {
    outer_r: f32,
    inner_r: f32,
    teeth: usize,
    angle: f32,
}

pub struct NavEmblemState {
    pub selected: bool,
    pub hovered: bool,
    pub time: f32,
    pub seed: f32,
}

impl Vec3 {
    const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    fn rotate_xyz(self, rot: Rot3) -> Self {
        let (sx, cx) = rot.rx.sin_cos();
        let (sy, cy) = rot.ry.sin_cos();
        let (sz, cz) = rot.rz.sin_cos();

        let y1 = self.y * cx - self.z * sx;
        let z1 = self.y * sx + self.z * cx;
        let x1 = self.x;

        let x2 = x1 * cy + z1 * sy;
        let z2 = -x1 * sy + z1 * cy;
        let y2 = y1;

        let x3 = x2 * cz - y2 * sz;
        let y3 = x2 * sz + y2 * cz;

        Self { x: x3, y: y3, z: z2 }
    }

    fn project(self, center: Pos2, scale: f32, cam_dist: f32) -> (Pos2, f32) {
        let denom = (cam_dist + self.z).max(0.6);
        let persp = cam_dist / denom;
        (
            pos2(center.x + self.x * scale * persp, center.y + self.y * scale * persp),
            self.z,
        )
    }
}

fn with_alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

fn pointer_tilt_angles(ui: &egui::Ui, rect: Rect, max_tilt_rad: f32) -> (f32, f32) {
    let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) else {
        return (0.0, 0.0);
    };
    let expanded = rect.expand(48.0);
    if !expanded.contains(pointer) {
        return (0.0, 0.0);
    }
    let nx = ((pointer.x - rect.center().x) / (rect.width().max(1.0) * 0.65)).clamp(-1.0, 1.0);
    let ny = ((pointer.y - rect.center().y) / (rect.height().max(1.0) * 0.65)).clamp(-1.0, 1.0);
    (-ny * max_tilt_rad, nx * max_tilt_rad)
}

/// Paints an embedded RGBA emblem with 4x4 perspective-projected 3D mesh deformation
/// so rotating / hovering gives genuine foreshortening instead of flat 2D scaling.
fn paint_3d_emblem_quad(
    painter: &egui::Painter,
    tex: &TextureHandle,
    center: Pos2,
    half_size: f32,
    rot: Rot3,
    tint: Color32,
) {
    const SUBDIV: usize = 4;
    let mut mesh = Mesh::with_texture(tex.id());
    let stride = SUBDIV + 1;

    for iy in 0..=SUBDIV {
        let v = iy as f32 / SUBDIV as f32;
        let py = (v - 0.5) * 2.0;
        for ix in 0..=SUBDIV {
            let u = ix as f32 / SUBDIV as f32;
            let px = (u - 0.5) * 2.0;
            let p3 = Vec3::new(px, py, 0.0).rotate_xyz(rot);
            let (screen, _z) = p3.project(center, half_size, 3.6);
            mesh.vertices.push(Vertex {
                pos: screen,
                uv: pos2(u, v),
                color: tint,
            });
        }
    }

    for iy in 0..SUBDIV {
        for ix in 0..SUBDIV {
            let i0 = (iy * stride + ix) as u32;
            let i1 = i0 + 1;
            let i2 = i0 + stride as u32;
            let i3 = i2 + 1;
            mesh.indices.extend_from_slice(&[i0, i1, i2, i1, i3, i2]);
        }
    }

    painter.add(Shape::mesh(mesh));
}

fn draw_floating_shadow(painter: &egui::Painter, center: Pos2, base_rx: f32, bob_norm: f32, color: Color32) {
    let rx = base_rx * (0.94 - bob_norm * 0.14);
    let ry = (rx * 0.24).max(2.0);
    painter.add(Shape::ellipse_filled(center, Vec2::new(rx, ry), color));
}

fn draw_3d_orbit_ring(
    painter: &egui::Painter,
    center: Pos2,
    radius: f32,
    rot: Rot3,
    style: OrbitStyle,
) {
    const SEGMENTS: usize = 28;
    let mut pts: Vec<(Pos2, f32)> = Vec::with_capacity(SEGMENTS + 1);
    for s in 0..=SEGMENTS {
        let a = (s as f32 / SEGMENTS as f32) * TAU;
        let p3 = Vec3::new(a.cos(), 0.0, a.sin()).rotate_xyz(rot);
        pts.push(p3.project(center, radius, 4.0));
    }

    for i in 0..SEGMENTS {
        let (p0, z0) = pts[i];
        let (p1, z1) = pts[i + 1];
        let avg_z = (z0 + z1) * 0.5;
        let col = if avg_z < 0.0 { style.front } else { style.back };
        let w = if avg_z < 0.0 { 1.8 } else { 1.1 };
        painter.line_segment([p0, p1], Stroke::new(w, col));
    }

    let orb = Vec3::new(style.phase.cos(), 0.0, style.phase.sin()).rotate_xyz(rot);
    let (orb_pos, _) = orb.project(center, radius, 4.0);
    painter.circle_filled(orb_pos, 2.8, style.dot);
}

fn draw_3d_octahedron(
    painter: &egui::Painter,
    center: Pos2,
    scale: f32,
    rot: Rot3,
    base_color: Color32,
    highlight_color: Color32,
) {
    let verts_raw = [
        Vec3::new(0.0, -1.25, 0.0),
        Vec3::new(0.85, 0.0, 0.0),
        Vec3::new(0.0, 0.0, 0.85),
        Vec3::new(-0.85, 0.0, 0.0),
        Vec3::new(0.0, 0.0, -0.85),
        Vec3::new(0.0, 1.25, 0.0),
    ];

    let transformed: Vec<Vec3> = verts_raw
        .iter()
        .map(|v| v.rotate_xyz(rot))
        .collect();
    let projected: Vec<Pos2> = transformed
        .iter()
        .map(|v| v.project(center, scale, 3.8).0)
        .collect();

    const FACES: [[usize; 3]; 8] = [
        [0, 1, 2],
        [0, 2, 3],
        [0, 3, 4],
        [0, 4, 1],
        [5, 2, 1],
        [5, 3, 2],
        [5, 4, 3],
        [5, 1, 4],
    ];

    let mut ordered: Vec<(f32, [usize; 3], f32)> = FACES
        .iter()
        .map(|&f| {
            let a = transformed[f[0]];
            let b = transformed[f[1]];
            let c = transformed[f[2]];
            let avg_z = (a.z + b.z + c.z) / 3.0;
            let e1 = Vec3::new(b.x - a.x, b.y - a.y, b.z - a.z);
            let e2 = Vec3::new(c.x - a.x, c.y - a.y, c.z - a.z);
            let ny = e1.z * e2.x - e1.x * e2.z;
            let nz = e1.x * e2.y - e1.y * e2.x;
            let shade = (0.55 - ny * 0.25 + (-nz).max(0.0) * 0.35).clamp(0.32, 1.0);
            (avg_z, f, shade)
        })
        .collect();

    ordered.sort_by(|a, b| b.0.total_cmp(&a.0));

    for (_z, f, shade) in ordered {
        let col = if shade > 0.72 {
            highlight_color
        } else {
            base_color
        };
        let r = ((col.r() as f32) * shade).clamp(0.0, 255.0) as u8;
        let g = ((col.g() as f32) * shade).clamp(0.0, 255.0) as u8;
        let b = ((col.b() as f32) * shade).clamp(0.0, 255.0) as u8;
        painter.add(Shape::Path(PathShape::convex_polygon(
            vec![projected[f[0]], projected[f[1]], projected[f[2]]],
            Color32::from_rgba_unmultiplied(r, g, b, 210),
            Stroke::new(1.0, with_alpha(highlight_color, 215)),
        )));
    }
}

fn draw_3d_cube_wire(
    painter: &egui::Painter,
    center: Pos2,
    size: f32,
    rx: f32,
    ry: f32,
    colors: CubeColors,
) {
    let corners = [
        Vec3::new(-0.7, -0.7, -0.7),
        Vec3::new(0.7, -0.7, -0.7),
        Vec3::new(0.7, 0.7, -0.7),
        Vec3::new(-0.7, 0.7, -0.7),
        Vec3::new(-0.7, -0.7, 0.7),
        Vec3::new(0.7, -0.7, 0.7),
        Vec3::new(0.7, 0.7, 0.7),
        Vec3::new(-0.7, 0.7, 0.7),
    ];
    let rot = Rot3::new(rx, ry, 0.0);
    let rotated: Vec<(Pos2, f32)> = corners
        .iter()
        .map(|v| v.rotate_xyz(rot).project(center, size, 3.6))
        .collect();

    const EDGES: [(usize, usize); 12] = [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ];

    painter.circle_filled(center, size * 0.56, colors.fill);

    for (a, b) in EDGES {
        let (pa, za) = rotated[a];
        let (pb, zb) = rotated[b];
        let front = (za + zb) * 0.5 < 0.0;
        let stroke = if front {
            Stroke::new(1.7, colors.front)
        } else {
            Stroke::new(1.1, colors.back)
        };
        painter.line_segment([pa, pb], stroke);
    }
}

fn draw_gear_ring(
    painter: &egui::Painter,
    center: Pos2,
    spec: GearSpec,
    fill: Color32,
    stroke: Color32,
) {
    let mut pts = Vec::with_capacity(spec.teeth * 4);
    let step = TAU / spec.teeth as f32;
    for i in 0..spec.teeth {
        let base = spec.angle + i as f32 * step;
        let angles = [
            (base, spec.inner_r),
            (base + step * 0.18, spec.outer_r),
            (base + step * 0.52, spec.outer_r),
            (base + step * 0.70, spec.inner_r),
        ];
        for (a, r) in angles {
            pts.push(pos2(center.x + a.cos() * r, center.y + a.sin() * r));
        }
    }
    painter.add(Shape::Path(PathShape::closed_line(
        pts,
        Stroke::new(1.8, stroke),
    )));
    painter.circle_stroke(center, spec.inner_r * 0.62, Stroke::new(1.6, fill));
}

pub fn draw_sidebar_brand_stage(
    ui: &mut egui::Ui,
    p: ThemePalette,
    brand_tex: Option<&TextureHandle>,
    game_running: bool,
    server_running: bool,
) {
    let time = ui.input(|i| i.time) as f32;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(56.0, 56.0), Sense::hover());
    let (tilt_x, tilt_y) = pointer_tilt_angles(ui, rect, 0.34);

    let float_y = (time * 2.3).sin() * 2.5;
    let bob_norm = ((time * 2.3).sin() + 1.0) * 0.5;
    let center = rect.center() + egui::vec2(0.0, float_y - 2.0);
    let painter = ui.painter_at(rect.expand(8.0));

    draw_floating_shadow(
        &painter,
        pos2(rect.center().x, rect.bottom() - 4.0),
        19.0,
        bob_norm,
        p.anim_shadow,
    );

    if game_running || server_running {
        let pulse_t = (time * 0.95) % 1.0;
        let pulse_r = 16.0 + pulse_t * 16.0;
        let pulse_a = ((1.0 - pulse_t) * 145.0) as u8;
        let ring_col = if game_running {
            p.accent_emerald_glow
        } else {
            p.accent_cyan
        };
        painter.circle_stroke(center, pulse_r, Stroke::new(1.6, with_alpha(ring_col, pulse_a)));
    }

    draw_3d_orbit_ring(
        &painter,
        center,
        25.0,
        Rot3::new(0.95, time * 0.9, 0.35),
        OrbitStyle {
            front: with_alpha(p.accent_cyan, 165),
            back: with_alpha(p.accent_cyan, 48),
            dot: p.accent_emerald_glow,
            phase: time * 2.2,
        },
    );

    if let Some(tex) = brand_tex {
        let rx = (time * 1.4).cos() * 0.08 + tilt_x;
        let ry = (time * 1.1).sin() * 0.16 + tilt_y;
        paint_3d_emblem_quad(
            &painter,
            tex,
            center,
            21.0,
            Rot3::new(rx, ry, 0.0),
            Color32::WHITE,
        );
    } else {
        draw_3d_octahedron(
            &painter,
            center,
            15.0,
            Rot3::new(0.35 + tilt_x, time * 1.3 + tilt_y, 0.0),
            p.accent_emerald,
            p.accent_emerald_glow,
        );
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
    let amp = if nav.selected {
        0.22
    } else if nav.hovered {
        0.28
    } else {
        0.09
    };
    let speed = if nav.hovered { 3.4 } else { 1.7 };
    let bob = ((nav.time * speed + nav.seed).sin())
        * if nav.selected || nav.hovered { 1.6 } else { 0.6 };
    let rx = (nav.time * 1.8 + nav.seed).cos() * amp * 0.55;
    let ry = (nav.time * speed + nav.seed).sin() * amp;
    let draw_center = pos2(center.x, center.y + bob);

    if nav.selected {
        painter.circle_stroke(
            draw_center,
            size * 0.56,
            Stroke::new(1.3, with_alpha(p.accent_emerald_glow, 165)),
        );
        let orb_a = nav.time * 2.6 + nav.seed;
        let orb = pos2(
            draw_center.x + orb_a.cos() * (size * 0.56),
            draw_center.y + orb_a.sin() * (size * 0.56),
        );
        painter.circle_filled(orb, 2.2, p.accent_emerald_glow);
    }

    if let Some(t) = tex {
        paint_3d_emblem_quad(
            painter,
            t,
            draw_center,
            size * 0.46,
            Rot3::new(rx, ry, 0.0),
            Color32::WHITE,
        );
    }
}

pub fn render_play_hero_showcase(
    ui: &mut egui::Ui,
    p: ThemePalette,
    banner: Option<&TextureHandle>,
    edition_icon: Option<&TextureHandle>,
    edition: GameEdition,
    is_running: bool,
    installed_mod_count: usize,
) {
    let time = ui.input(|i| i.time) as f32;
    let width = ui.available_width().max(320.0);
    let height = 148.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), Sense::hover());
    let (tilt_x, tilt_y) = pointer_tilt_angles(ui, rect, 0.28);

    let painter = ui.painter_at(rect);
    let cr = CornerRadius::same(11);

    if let Some(tex) = banner {
        let pan_x = (time * 0.16).sin() * 0.025 + 0.025;
        let pan_y = (time * 0.12).cos() * 0.02 + 0.02;
        let uv = Rect::from_min_max(pos2(pan_x, pan_y), pos2(0.95 - pan_x, 0.96 - pan_y));
        painter.image(tex.id(), rect, uv, Color32::WHITE);
    } else {
        painter.rect_filled(rect, cr, p.bg_card);
    }

    // Smooth atmospheric dark gradient overlay so foreground emblems and title keep crisp contrast in Dark & Light modes
    let mut scrim = Mesh::default();
    let v_tl = Vertex {
        pos: rect.left_top(),
        uv: Pos2::ZERO,
        color: p.hero_scrim_top,
    };
    let v_tr = Vertex {
        pos: rect.right_top(),
        uv: Pos2::ZERO,
        color: p.hero_scrim_top,
    };
    let v_bl = Vertex {
        pos: rect.left_bottom(),
        uv: Pos2::ZERO,
        color: p.hero_scrim_bottom,
    };
    let v_br = Vertex {
        pos: rect.right_bottom(),
        uv: Pos2::ZERO,
        color: p.hero_scrim_bottom,
    };
    scrim.vertices.extend_from_slice(&[v_tl, v_tr, v_bl, v_br]);
    scrim.indices.extend_from_slice(&[0, 1, 2, 1, 3, 2]);
    painter.add(Shape::mesh(scrim));
    painter.rect_stroke(
        rect,
        cr,
        Stroke::new(1.2, with_alpha(p.accent_emerald, 145)),
        egui::StrokeKind::Inside,
    );

    // 3D floating edition crest on the left
    let crest_base = pos2(rect.left() + 62.0, rect.center().y - 2.0);
    let float_y = (time * 2.1).sin() * 3.5;
    let bob_norm = ((time * 2.1).sin() + 1.0) * 0.5;
    let crest_center = pos2(crest_base.x, crest_base.y + float_y);

    draw_floating_shadow(
        &painter,
        pos2(crest_base.x, rect.bottom() - 18.0),
        28.0,
        bob_norm,
        Color32::from_black_alpha(120),
    );

    if is_running {
        for phase_offset in [0.0_f32, 0.5] {
            let t = (time * 0.8 + phase_offset) % 1.0;
            let r = 28.0 + t * 24.0;
            let a = ((1.0 - t) * 180.0) as u8;
            painter.circle_stroke(
                crest_center,
                r,
                Stroke::new(1.8, with_alpha(p.accent_emerald_glow, a)),
            );
        }
    }

    draw_3d_orbit_ring(
        &painter,
        crest_center,
        39.0,
        Rot3::new(1.05, time * 0.75, 0.25),
        OrbitStyle {
            front: with_alpha(p.accent_emerald_glow, 180),
            back: with_alpha(p.accent_emerald, 55),
            dot: p.accent_amber,
            phase: time * 1.9,
        },
    );

    if let Some(icon) = edition_icon {
        let rx = (time * 1.25).cos() * 0.08 + tilt_x;
        let ry = (time * 0.95).sin() * 0.16 + tilt_y;
        paint_3d_emblem_quad(
            &painter,
            icon,
            crest_center,
            31.0,
            Rot3::new(rx, ry, 0.0),
            Color32::WHITE,
        );
    }

    // Companion floating 3D crystal shard to the right side of the hero banner
    let crystal_center = pos2(rect.right() - 48.0, rect.center().y + (time * 1.7 + 1.2).sin() * 4.0);
    let crystal_col = if edition == GameEdition::TModLoader {
        p.accent_cyan
    } else {
        p.accent_emerald
    };
    draw_3d_octahedron(
        &painter,
        crystal_center,
        19.0,
        Rot3::new(0.38 + tilt_x, time * 1.35 + tilt_y, 0.12),
        crystal_col,
        p.accent_emerald_glow,
    );

    let title_pos = pos2(rect.left() + 116.0, rect.center().y - 16.0);
    painter.text(
        title_pos,
        egui::Align2::LEFT_CENTER,
        edition.label(),
        crate::theme::ui_font_strong(21.0),
        p.hero_title,
    );

    let state_text = if is_running {
        "Đang hoạt động".to_string()
    } else if edition == GameEdition::TModLoader {
        format!("{installed_mod_count} mod đang cài sẵn")
    } else {
        "Phiên bản gốc 1.4.5.8".to_string()
    };
    painter.text(
        pos2(rect.left() + 116.0, rect.center().y + 12.0),
        egui::Align2::LEFT_CENTER,
        state_text,
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
    let time = ui.input(|i| i.time) as f32;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(52.0, 52.0), Sense::hover());
    let (tilt_x, tilt_y) = pointer_tilt_angles(ui, rect, 0.32);
    let center = rect.center() + egui::vec2(0.0, (time * 2.2).sin() * 2.0);
    let painter = ui.painter_at(rect.expand(6.0));

    let beacon_col = if starting {
        p.accent_cyan
    } else if active {
        p.accent_emerald
    } else {
        p.text_muted
    };

    if active || starting {
        let wave = (time * 1.1) % 1.0;
        let a = ((1.0 - wave) * 170.0) as u8;
        painter.circle_stroke(center, 15.0 + wave * 16.0, Stroke::new(1.7, with_alpha(beacon_col, a)));
    }

    draw_3d_orbit_ring(
        &painter,
        center,
        24.0,
        Rot3::new(1.0, time * if active { 1.4 } else { 0.5 }, -0.3),
        OrbitStyle {
            front: with_alpha(beacon_col, 190),
            back: with_alpha(beacon_col, 55),
            dot: p.accent_amber,
            phase: time * 2.4,
        },
    );

    if let Some(t) = tex {
        paint_3d_emblem_quad(
            &painter,
            t,
            center,
            18.5,
            Rot3::new(tilt_x, (time * 1.1).sin() * 0.14 + tilt_y, 0.0),
            Color32::WHITE,
        );
    } else {
        draw_3d_octahedron(
            &painter,
            center,
            13.0,
            Rot3::new(0.3, time * 1.2, 0.0),
            beacon_col,
            p.accent_emerald_glow,
        );
    }
}

pub fn render_mods_scanner_3d(
    ui: &mut egui::Ui,
    p: ThemePalette,
    tex: Option<&TextureHandle>,
    busy: bool,
) {
    let time = ui.input(|i| i.time) as f32;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(52.0, 52.0), Sense::hover());
    let (tilt_x, tilt_y) = pointer_tilt_angles(ui, rect, 0.34);
    let speed = if busy { 2.8 } else { 0.9 };
    let center = rect.center() + egui::vec2(0.0, (time * 2.0).sin() * 2.0);
    let painter = ui.painter_at(rect.expand(6.0));

    draw_3d_cube_wire(
        &painter,
        center,
        25.0,
        0.48 + tilt_x,
        time * speed + tilt_y,
        CubeColors {
            front: with_alpha(p.accent_cyan, 200),
            back: with_alpha(p.accent_emerald, 85),
            fill: with_alpha(p.accent_cyan, 18),
        },
    );

    if let Some(t) = tex {
        paint_3d_emblem_quad(
            &painter,
            t,
            center,
            17.5,
            Rot3::new(tilt_x * 0.8, (time * 1.3).sin() * 0.14 + tilt_y * 0.8, 0.0),
            Color32::WHITE,
        );
    }

    // Animated vertical laser scan sweep across the mod emblem
    let scan_y = rect.top() + 8.0 + ((time * if busy { 2.6 } else { 1.1 }).sin() * 0.5 + 0.5) * (rect.height() - 16.0);
    painter.line_segment(
        [pos2(rect.left() + 6.0, scan_y), pos2(rect.right() - 6.0, scan_y)],
        Stroke::new(1.6, with_alpha(p.accent_emerald_glow, if busy { 225 } else { 125 })),
    );
}

pub fn render_host_radar_3d(
    ui: &mut egui::Ui,
    p: ThemePalette,
    tex: Option<&TextureHandle>,
    tunnel_online: bool,
    upnp_active: bool,
) {
    let time = ui.input(|i| i.time) as f32;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(52.0, 52.0), Sense::hover());
    let (tilt_x, tilt_y) = pointer_tilt_angles(ui, rect, 0.3);
    let center = rect.center();
    let painter = ui.painter_at(rect.expand(6.0));

    let pulse_col = if tunnel_online {
        p.accent_cyan
    } else if upnp_active {
        p.accent_emerald
    } else {
        p.accent_amber
    };

    for offset in [0.0_f32, 0.5] {
        let wave = (time * if tunnel_online { 1.15 } else { 0.55 } + offset) % 1.0;
        let a = ((1.0 - wave) * if tunnel_online { 185.0 } else { 95.0 }) as u8;
        painter.circle_stroke(center, 11.0 + wave * 18.0, Stroke::new(1.5, with_alpha(pulse_col, a)));
    }

    draw_3d_orbit_ring(
        &painter,
        center,
        24.0,
        Rot3::new(0.75, time * 1.25, 0.45),
        OrbitStyle {
            front: with_alpha(p.accent_cyan, 195),
            back: with_alpha(p.accent_cyan, 60),
            dot: p.accent_amber,
            phase: time * 2.1,
        },
    );
    draw_3d_orbit_ring(
        &painter,
        center,
        22.0,
        Rot3::new(-0.8, -time * 0.95, -0.55),
        OrbitStyle {
            front: with_alpha(p.accent_emerald, 175),
            back: with_alpha(p.accent_emerald, 50),
            dot: p.accent_cyan,
            phase: -time * 1.7,
        },
    );

    if let Some(t) = tex {
        paint_3d_emblem_quad(
            &painter,
            t,
            center,
            17.0,
            Rot3::new(tilt_x, (time * 1.2).sin() * 0.14 + tilt_y, 0.0),
            Color32::WHITE,
        );
    }
}

pub fn render_worlds_island_3d(
    ui: &mut egui::Ui,
    p: ThemePalette,
    tex: Option<&TextureHandle>,
) {
    let time = ui.input(|i| i.time) as f32;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(46.0, 46.0), Sense::hover());
    let (tilt_x, tilt_y) = pointer_tilt_angles(ui, rect, 0.28);
    let bob = (time * 1.9).sin() * 2.2;
    let bob_norm = ((time * 1.9).sin() + 1.0) * 0.5;
    let center = pos2(rect.center().x, rect.center().y + bob - 1.5);
    let painter = ui.painter_at(rect.expand(4.0));

    draw_floating_shadow(
        &painter,
        pos2(rect.center().x, rect.bottom() - 3.0),
        16.0,
        bob_norm,
        p.anim_shadow,
    );

    if let Some(t) = tex {
        paint_3d_emblem_quad(
            &painter,
            t,
            center,
            18.0,
            Rot3::new(tilt_x, (time * 0.95).sin() * 0.12 + tilt_y, 0.0),
            Color32::WHITE,
        );
    }
}

pub fn render_settings_gears_3d(
    ui: &mut egui::Ui,
    p: ThemePalette,
    tex: Option<&TextureHandle>,
    busy: bool,
) {
    let time = ui.input(|i| i.time) as f32;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(52.0, 52.0), Sense::hover());
    let (tilt_x, tilt_y) = pointer_tilt_angles(ui, rect, 0.3);
    let speed = if busy { 2.4 } else { 0.8 };
    let center = rect.center();
    let painter = ui.painter_at(rect.expand(4.0));

    draw_gear_ring(
        &painter,
        pos2(center.x - 4.0, center.y - 3.0),
        GearSpec {
            outer_r: 20.0,
            inner_r: 15.5,
            teeth: 10,
            angle: time * speed,
        },
        with_alpha(p.accent_amber, 160),
        with_alpha(p.accent_amber, 195),
    );
    draw_gear_ring(
        &painter,
        pos2(center.x + 12.0, center.y + 11.0),
        GearSpec {
            outer_r: 11.0,
            inner_r: 8.2,
            teeth: 8,
            angle: -time * speed * 1.25,
        },
        with_alpha(p.accent_cyan, 160),
        with_alpha(p.accent_cyan, 195),
    );

    if let Some(t) = tex {
        paint_3d_emblem_quad(
            &painter,
            t,
            center,
            17.5,
            Rot3::new(tilt_x, (time * 1.0).sin() * 0.12 + tilt_y, 0.0),
            Color32::WHITE,
        );
    }
}
