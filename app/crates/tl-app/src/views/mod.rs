mod host_view;
pub(crate) mod mod_configs_view;
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
use tl_runtime::ServerLifecycleState;

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
            render_close_bridge_confirm_banner(state, ui);
            egui::ScrollArea::vertical()
                .id_salt(("tl_main_scroll", state.active_tab as u8))
                .auto_shrink([false, false])
                .show(ui, |ui| match state.active_tab {
                    NavTab::Play => play_view::render(state, ui),
                    NavTab::Server => server_view::render(state, ui),
                    NavTab::Mods => mods_view::render(state, ui),
                    NavTab::Settings => settings_view::render(state, ui),
                });
        });
}

fn render_sidebar(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    let server_active = state.any_server_active();
    let compact = ui.available_width() < 160.0;

    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            anim3d::draw_sidebar_brand_stage(
                ui,
                p,
                assets.brand_emblem.as_ref(),
                state.is_game_running(),
                server_active,
            );
            ui.add_space(2.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.add_space(if compact { 5.0 } else { 3.0 });
                ui.add(
                    egui::Label::new(
                        RichText::new("TriHienKun")
                            .font(crate::theme::ui_font_strong(if compact { 13.5 } else { 15.0 }))
                            .color(p.text_primary),
                    )
                    .truncate(),
                );
                ui.add(
                    egui::Label::new(
                        RichText::new("Terraria Launcher")
                            .font(crate::theme::ui_font_strong(if compact { 11.0 } else { 12.5 }))
                            .color(p.text_muted),
                    )
                    .truncate(),
                );
            });
        });
        ui.add_space(12.0);

        for tab in [
            NavTab::Play,
            NavTab::Server,
            NavTab::Mods,
            NavTab::Settings,
        ] {
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
                anim3d::NavEmblemState { selected, hovered },
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
                state.switch_nav_tab(tab);
            }
            ui.add_space(4.0);
        }

        ui.with_layout(Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(4.0);

            ui.horizontal(|ui| {
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

            let running_count = state.total_running_servers_count();
            if running_count > 0 {
                ui.add_space(6.0);
                rpg_subcard(ui, assets.as_ref(), p, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new("Server")
                                .size(12.0)
                                .strong()
                                .color(p.accent_emerald),
                        );
                        if running_count >= 2 {
                            ui.label(
                                RichText::new(format!("({running_count} đang chạy)"))
                                    .size(11.5)
                                    .strong()
                                    .color(p.accent_cyan),
                            );
                        }
                    });

                    let supervised = state.running_supervised_snapshots();
                    let externals = state.external_servers.clone();
                    let mut stop_profile_id: Option<String> = None;
                    let mut stop_ext_pid: Option<u32> = None;
                    let mut stop_all_clicked = false;

                    for (profile_id, snap) in &supervised {
                        ui.push_id(("sidebar_sup", profile_id), |ui| {
                            ui.add_space(3.0);
                            let name = if snap.profile_name.trim().is_empty() {
                                snap.edition.label()
                            } else {
                                snap.profile_name.as_str()
                            };
                            ui.label(
                                RichText::new(name)
                                    .size(11.5)
                                    .strong()
                                    .color(p.text_primary),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "Cổng {} • {} người",
                                    snap.port,
                                    snap.players.len()
                                ))
                                .size(11.5)
                                .color(p.text_muted),
                            );
                            let is_stopping =
                                matches!(snap.state, ServerLifecycleState::Stopping);
                            if ui
                                .add_enabled_ui(!is_stopping, |ui| {
                                    rpg_compact_button(
                                        ui,
                                        assets.as_ref(),
                                        p,
                                        ButtonSkin::Red,
                                        Some(UiIcon::Stop),
                                        if is_stopping {
                                            "Đang lưu..."
                                        } else {
                                            "Lưu & Tắt"
                                        },
                                    )
                                })
                                .inner
                                .clicked()
                            {
                                stop_profile_id = Some(profile_id.clone());
                            }
                        });
                    }

                    for ext in &externals {
                        ui.push_id(("sidebar_ext", ext.pid), |ui| {
                            ui.add_space(3.0);
                            let matched_profile = ext
                                .profile_id
                                .as_deref()
                                .and_then(|pid| state.config.servers.iter().find(|s| s.id == pid));
                            let ext_name = matched_profile
                                .map(|s| s.name.as_str())
                                .unwrap_or_else(|| ext.edition.label());
                            let ext_port = ext
                                .port
                                .or_else(|| matched_profile.map(|s| s.config.port))
                                .unwrap_or_else(|| state.active_server_port());
                            ui.label(
                                RichText::new(ext_name)
                                    .size(11.5)
                                    .strong()
                                    .color(p.text_primary),
                            );
                            ui.label(
                                RichText::new(format!("Cổng {ext_port}"))
                                    .size(11.5)
                                    .color(p.text_muted),
                            );
                            if rpg_compact_button(
                                ui,
                                assets.as_ref(),
                                p,
                                ButtonSkin::Red,
                                Some(UiIcon::Stop),
                                "Buộc tắt",
                            )
                            .clicked()
                            {
                                stop_ext_pid = Some(ext.pid);
                            }
                        });
                    }

                    if running_count >= 2 {
                        ui.add_space(4.0);
                        if rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Red,
                            Some(UiIcon::Stop),
                            "Tắt tất cả Server",
                        )
                        .clicked()
                        {
                            stop_all_clicked = true;
                        }
                    }

                    if stop_all_clicked {
                        state.stop_all_servers();
                    } else if let Some(id) = stop_profile_id {
                        state.stop_server_profile(&id);
                    } else if let Some(pid) = stop_ext_pid {
                        state.stop_external_pid(pid);
                    }
                });
            }

            let sidebar_game = (state.active_tab != NavTab::Play)
                .then(|| state.current_running_game().cloned())
                .flatten();
            if let Some(game) = sidebar_game {
                ui.add_space(6.0);
                rpg_subcard(ui, assets.as_ref(), p, |ui| {
                    ui.label(
                        RichText::new(format!("Đang chạy: {}", game.edition.label()))
                            .size(12.0)
                            .strong()
                            .color(p.accent_emerald),
                    );
                    ui.add_space(2.0);
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

fn render_close_bridge_confirm_banner(state: &mut AppState, ui: &mut egui::Ui) {
    if !state.confirm_close_bridge_modal {
        return;
    }
    let p = state.palette();
    let assets = state.assets.clone();
    let color = p.accent_amber;
    let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 28);
    let mut stay_clicked = false;
    let mut exit_clicked = false;
    Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, color))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(state.close_confirm_message())
                    .size(13.0)
                    .color(p.text_primary),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if rpg_compact_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Slate,
                    None,
                    "Ở lại",
                )
                .clicked()
                {
                    stay_clicked = true;
                }
                if rpg_compact_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Red,
                    None,
                    "Vẫn đóng",
                )
                .clicked()
                {
                    exit_clicked = true;
                }
            });
        });
    ui.add_space(10.0);
    if exit_clicked {
        state.confirm_close_bridge_and_exit(ui.ctx());
    } else if stay_clicked {
        state.cancel_close_bridge_modal();
    }
}
