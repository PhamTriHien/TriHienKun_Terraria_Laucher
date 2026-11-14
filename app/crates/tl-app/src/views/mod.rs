pub mod host_view;
pub mod mods_view;
pub mod play_view;
pub mod server_view;
pub mod settings_view;

use crate::anim3d;
use crate::assets::{ButtonSkin, UiIcon};
use crate::state::{AppState, NavTab};
use crate::theme::{rpg_compact_button, rpg_selectable_button, rpg_subcard};
use egui::{pos2, Color32, CornerRadius, Frame, Layout, Margin, RichText, Sense, Stroke};
use tl_core::ThemeMode;

pub fn render_root(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let available = ui.available_width();
    let compact = available < 760.0;
    let nav_width = if compact { 174.0 } else { 226.0 };

    egui::Panel::left("tl_sidebar")
        .resizable(false)
        .default_size(nav_width)
        .size_range(nav_width..=nav_width)
        .frame(
            Frame::side_top_panel(ui.style())
                .fill(p.bg_panel)
                .stroke(Stroke::new(1.0, p.border_subtle))
                .inner_margin(Margin::same(12)),
        )
        .show(ui, |ui| {
            render_sidebar(state, ui);
        });

    egui::CentralPanel::default()
        .frame(
            Frame::central_panel(ui.style())
                .fill(p.bg_void)
                .inner_margin(Margin::symmetric(18, 14)),
        )
        .show(ui, |ui| {
            render_notification_banner(state, ui);
            egui::ScrollArea::vertical()
                .id_salt(("tl_main_scroll", state.active_tab as u8))
                .auto_shrink([false, false])
                .show(ui, |ui| match state.active_tab {
                    NavTab::Play => play_view::render(state, ui),
                    NavTab::Server => server_view::render(state, ui),
                    NavTab::Mods => mods_view::render(state, ui),
                    NavTab::Host => host_view::render(state, ui),
                    NavTab::Settings => settings_view::render(state, ui),
                });
        });
}

fn render_sidebar(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    let time = ui.input(|i| i.time) as f32;
    let server_active = state
        .server_supervisor
        .as_ref()
        .map(|s| s.is_active())
        .unwrap_or(false);

    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            anim3d::draw_sidebar_brand_stage(
                ui,
                p,
                assets.brand_emblem.as_ref(),
                state.active_game.is_some(),
                server_active,
            );
            ui.vertical(|ui| {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Terraria\nLauncher")
                        .font(crate::theme::ui_font_strong(16.0))
                        .color(p.text_primary),
                );
                ui.label(
                    RichText::new(state.config.active_edition.label())
                        .size(11.5)
                        .color(p.accent_emerald),
                );
            });
        });
        ui.add_space(12.0);

        for (idx, tab) in [
            NavTab::Play,
            NavTab::Server,
            NavTab::Mods,
            NavTab::Host,
            NavTab::Settings,
        ]
        .into_iter()
        .enumerate()
        {
            let selected = state.active_tab == tab;
            let row_w = ui.available_width();
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(row_w, 40.0), Sense::click());
            let hovered = resp.hovered();

            let fill = if selected {
                p.nav_selected_fill
            } else if hovered {
                Color32::from_rgba_unmultiplied(
                    p.accent_emerald.r(),
                    p.accent_emerald.g(),
                    p.accent_emerald.b(),
                    18,
                )
            } else {
                Color32::TRANSPARENT
            };
            let stroke = if selected {
                Stroke::new(1.1, p.accent_emerald)
            } else if hovered {
                Stroke::new(1.0, p.border_subtle)
            } else {
                Stroke::NONE
            };

            let painter = ui.painter();
            painter.rect_filled(rect, CornerRadius::same(8), fill);
            if stroke != Stroke::NONE {
                painter.rect_stroke(rect, CornerRadius::same(8), stroke, egui::StrokeKind::Inside);
            }

            let icon_center = pos2(rect.left() + 21.0, rect.center().y);
            anim3d::draw_nav_emblem_3d(
                painter,
                icon_center,
                28.0,
                assets.nav_icon(tab),
                p,
                anim3d::NavEmblemState {
                    selected,
                    hovered,
                    time,
                    seed: idx as f32 * 1.15,
                },
            );

            let text_color = if selected {
                p.nav_selected_text
            } else {
                p.text_primary
            };
            let tab_font = if selected {
                crate::theme::ui_font_strong(14.0)
            } else {
                crate::theme::ui_font(14.0)
            };
            painter.text(
                pos2(rect.left() + 42.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                tab.label(),
                tab_font,
                text_color,
            );

            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                state.active_tab = tab;
            }
            ui.add_space(4.0);
        }

        ui.with_layout(Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(4.0);

            rpg_subcard(ui, assets.as_ref(), p, |ui| {
                ui.label(RichText::new("Giao diện").size(12.0).color(p.text_muted));
                ui.horizontal_wrapped(|ui| {
                    for (mode, skin, icon) in [
                        (ThemeMode::Light, ButtonSkin::Amber, UiIcon::Sun),
                        (ThemeMode::Dark, ButtonSkin::Cyan, UiIcon::Moon),
                    ] {
                        let active = state.config.theme_mode == mode;
                        if rpg_selectable_button(
                            ui,
                            assets.as_ref(),
                            p,
                            active,
                            skin,
                            icon,
                            mode.short_label(),
                        )
                        .clicked()
                            && !active
                        {
                            state.set_theme_mode(ui.ctx(), mode);
                        }
                    }
                });
            });
            ui.add_space(6.0);

            if let Some(endpoint) = state.active_tunnel_endpoint() {
                rpg_subcard(ui, assets.as_ref(), p, |ui| {
                    ui.label(
                        RichText::new("Tunnel Online")
                            .size(11.5)
                            .strong()
                            .color(p.accent_cyan),
                    );
                    ui.label(
                        RichText::new(&endpoint)
                            .monospace()
                            .size(12.0)
                            .color(p.text_primary),
                    );
                    if rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Cyan,
                        Some(UiIcon::Copy),
                        "Sao chép địa chỉ",
                    )
                    .clicked()
                    {
                        ui.ctx().copy_text(endpoint);
                        state.notify_ok("Đã sao chép địa chỉ Tunnel.");
                    }
                });
                ui.add_space(6.0);
            }

            rpg_subcard(ui, assets.as_ref(), p, |ui| {
                let srv_color = state.badge_color_for_server();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Server:").size(12.0).color(p.text_muted));
                    ui.label(
                        RichText::new(state.server_status_short())
                            .size(12.0)
                            .strong()
                            .color(srv_color),
                    );
                });
                if let Some(snap) = &state.server_snapshot {
                    if state
                        .server_supervisor
                        .as_ref()
                        .map(|s| s.is_active())
                        .unwrap_or(false)
                    {
                        ui.label(
                            RichText::new(format!(
                                "Cổng {}  •  {} người chơi",
                                snap.port,
                                snap.players.len()
                            ))
                            .size(11.5)
                            .color(p.text_muted),
                        );
                        if rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Red,
                            Some(UiIcon::Stop),
                            "Lưu & Tắt Server",
                        )
                        .clicked()
                        {
                            state.stop_selected_server();
                        }
                    }
                }
            });

            if let Some(game) = state.running_game.clone() {
                ui.add_space(6.0);
                rpg_subcard(ui, assets.as_ref(), p, |ui| {
                    ui.label(
                        RichText::new(format!("Đang chạy: {}", game.edition.label()))
                            .size(12.0)
                            .strong()
                            .color(p.accent_emerald),
                    );
                    ui.label(
                        RichText::new(format!(
                            "PID {}  •  {} MB",
                            game.pid, game.memory_mb
                        ))
                        .size(11.5)
                        .color(p.text_muted),
                    );
                    if rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Red,
                        Some(UiIcon::Stop),
                        "Đóng Game",
                    )
                    .clicked()
                    {
                        state.kill_running_game();
                    }
                });
            }
        });
    });
}

fn render_notification_banner(state: &mut AppState, ui: &mut egui::Ui) {
    let Some(notif) = state.notification.clone() else {
        return;
    };
    let p = state.palette();
    let assets = state.assets.clone();
    let color = if notif.is_error {
        p.accent_red
    } else {
        p.accent_emerald
    };
    let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 28);
    let mut dismiss = false;
    Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, color))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(&notif.message).size(13.0).color(p.text_primary));
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    if rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Slate,
                        None,
                        "Đóng",
                    )
                    .clicked()
                    {
                        dismiss = true;
                    }
                });
            });
        });
    if dismiss {
        state.notification = None;
    }
    ui.add_space(10.0);
}
