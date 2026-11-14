pub mod host_view;
pub mod mods_view;
pub mod play_view;
pub mod server_view;
pub mod settings_view;

use crate::state::{AppState, NavTab};
use crate::theme::{
    accent_button, subcard_frame, ACCENT_AMBER, ACCENT_CYAN, ACCENT_EMERALD, ACCENT_RED, BG_VOID,
    BG_PANEL, BORDER_SUBTLE, TEXT_MUTED, TEXT_PRIMARY,
};
use egui::{Color32, CornerRadius, Frame, Layout, Margin, RichText, Stroke};

pub fn render_root(state: &mut AppState, ui: &mut egui::Ui) {
    let available = ui.available_width();
    let compact = available < 760.0;
    let nav_width = if compact { 166.0 } else { 216.0 };

    egui::Panel::left("tl_sidebar")
        .resizable(false)
        .default_size(nav_width)
        .size_range(nav_width..=nav_width)
        .frame(
            Frame::side_top_panel(ui.style())
                .fill(BG_PANEL)
                .stroke(Stroke::new(1.0, BORDER_SUBTLE))
                .inner_margin(Margin::same(12)),
        )
        .show(ui, |ui| {
            render_sidebar(state, ui);
        });

    egui::CentralPanel::default()
        .frame(
            Frame::central_panel(ui.style())
                .fill(BG_VOID)
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
    ui.vertical(|ui| {
        ui.label(
            RichText::new("Terraria Launcher")
                .size(17.5)
                .strong()
                .color(TEXT_PRIMARY),
        );
        ui.label(
            RichText::new(state.config.active_edition.label())
                .size(12.0)
                .color(ACCENT_EMERALD),
        );
        ui.add_space(14.0);

        for tab in [
            NavTab::Play,
            NavTab::Server,
            NavTab::Mods,
            NavTab::Host,
            NavTab::Settings,
        ] {
            let selected = state.active_tab == tab;
            let fill = if selected {
                Color32::from_rgba_unmultiplied(
                    ACCENT_EMERALD.r(),
                    ACCENT_EMERALD.g(),
                    ACCENT_EMERALD.b(),
                    34,
                )
            } else {
                Color32::TRANSPARENT
            };
            let stroke = if selected {
                Stroke::new(1.0, ACCENT_EMERALD)
            } else {
                Stroke::NONE
            };
            let text_color = if selected { Color32::WHITE } else { TEXT_PRIMARY };

            let btn = egui::Button::new(RichText::new(tab.label()).size(14.0).strong().color(text_color))
                .fill(fill)
                .stroke(stroke)
                .corner_radius(CornerRadius::same(7))
                .min_size(egui::vec2(ui.available_width(), 36.0));
            if ui.add(btn).clicked() {
                state.active_tab = tab;
            }
            ui.add_space(3.0);
        }

        ui.with_layout(Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(4.0);

            if let Some(endpoint) = state.active_tunnel_endpoint() {
                subcard_frame().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new("Tunnel Online")
                            .size(11.5)
                            .strong()
                            .color(ACCENT_CYAN),
                    );
                    ui.label(RichText::new(&endpoint).monospace().size(12.0));
                    if accent_button(ui, "Sao chép địa chỉ", ACCENT_CYAN).clicked() {
                        ui.ctx().copy_text(endpoint);
                        state.notify_ok("Đã sao chép địa chỉ Tunnel.");
                    }
                });
                ui.add_space(6.0);
            }

            subcard_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                let srv_color = state.badge_color_for_server();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Server:").size(12.0).color(TEXT_MUTED));
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
                            .color(TEXT_MUTED),
                        );
                        if accent_button(ui, "Lưu & Tắt Server", ACCENT_AMBER).clicked() {
                            state.stop_selected_server();
                        }
                    }
                }
            });

            if let Some(game) = state.running_game.clone() {
                ui.add_space(6.0);
                subcard_frame().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new(format!("Đang chạy: {}", game.edition.label()))
                            .size(12.0)
                            .strong()
                            .color(ACCENT_EMERALD),
                    );
                    ui.label(
                        RichText::new(format!(
                            "PID {}  •  {} MB",
                            game.pid, game.memory_mb
                        ))
                        .size(11.5)
                        .color(TEXT_MUTED),
                    );
                    if accent_button(ui, "Đóng Game", ACCENT_RED).clicked() {
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
    let color = if notif.is_error {
        ACCENT_RED
    } else {
        ACCENT_EMERALD
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
            ui.horizontal(|ui| {
                ui.label(RichText::new(&notif.message).size(13.0).color(TEXT_PRIMARY));
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Đóng").clicked() {
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
