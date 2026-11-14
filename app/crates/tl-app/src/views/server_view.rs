use crate::anim3d;
use crate::assets::{ButtonSkin, UiIcon};
use crate::state::{AppState, ServerSubTab};
use crate::theme::{
    console_frame, page_title, rpg_button, rpg_card, rpg_compact_button, rpg_section_heading,
    rpg_selectable_button, rpg_subcard, status_pill, ThemePalette,
};
use egui::RichText;
use tl_core::{GameEdition, WorldDifficultyOption};
use tl_runtime::{ConsoleLogLine, DirectHostStatus, ServerLifecycleState};

pub fn render(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    let active_snap_state = state.active_server_snapshot().map(|s| &s.state);
    let ext_starting = state.is_active_external_starting();
    let any_supervised_starting = state.server_snapshots.iter().any(|(id, snap)| {
        state
            .server_supervisors
            .get(id)
            .is_some_and(|s| s.is_active())
            && matches!(snap.state, ServerLifecycleState::Starting)
    });
    let any_supervised_running = state.server_snapshots.iter().any(|(id, snap)| {
        state
            .server_supervisors
            .get(id)
            .is_some_and(|s| s.is_active())
            && matches!(snap.state, ServerLifecycleState::Running)
    });
    let active_is_starting =
        matches!(active_snap_state, Some(ServerLifecycleState::Starting)) || ext_starting;
    let server_running = matches!(active_snap_state, Some(ServerLifecycleState::Running))
        || (state.showing_external_server() && !ext_starting)
        || (!active_is_starting
            && (any_supervised_running || !state.external_servers.is_empty()));
    let server_starting = active_is_starting || (!server_running && any_supervised_starting);

    ui.horizontal(|ui| {
        anim3d::render_server_beacon_3d(
            ui,
            p,
            assets.nav_server.as_ref(),
            server_running,
            server_starting,
        );
        ui.vertical(|ui| {
            ui.add_space(12.0);
            page_title(ui, p, "Server & Chơi Online");
        });
    });
    ui.add_space(8.0);

    render_profile_and_controls_card(state, ui, p);
    ui.add_space(10.0);

    let online_players_cnt = if state.showing_external_server() {
        state
            .active_external_log_summary()
            .map(|s| s.players.len())
            .unwrap_or(0)
    } else {
        state
            .active_server_snapshot()
            .map(|s| s.players.len())
            .unwrap_or(0)
    };
    let direct_open = matches!(
        state.active_direct_host_status(),
        DirectHostStatus::Running(_) | DirectHostStatus::Starting { .. }
    ) || state.pending_auto_direct_host_profile_id.is_some();

    ui.horizontal_wrapped(|ui| {
        if rpg_selectable_button(
            ui,
            assets.as_ref(),
            p,
            state.server_tab == ServerSubTab::Config,
            ButtonSkin::Cyan,
            UiIcon::Settings,
            ServerSubTab::Config.label(),
        )
        .clicked()
        {
            state.server_tab = ServerSubTab::Config;
        }

        let net_label = if direct_open {
            "Mở Port & IP • Đang mở"
        } else {
            ServerSubTab::Network.label()
        };
        if rpg_selectable_button(
            ui,
            assets.as_ref(),
            p,
            state.server_tab == ServerSubTab::Network,
            ButtonSkin::Emerald,
            UiIcon::Host,
            net_label,
        )
        .clicked()
        {
            state.server_tab = ServerSubTab::Network;
        }

        let console_label = if online_players_cnt > 0 {
            format!("Console & Người chơi ({online_players_cnt})")
        } else {
            ServerSubTab::Console.label().to_string()
        };
        if rpg_selectable_button(
            ui,
            assets.as_ref(),
            p,
            state.server_tab == ServerSubTab::Console,
            ButtonSkin::Cyan,
            UiIcon::Server,
            &console_label,
        )
        .clicked()
        {
            state.server_tab = ServerSubTab::Console;
        }
    });
    ui.add_space(10.0);

    match state.server_tab {
        ServerSubTab::Config => render_config_editor(state, ui, p),
        ServerSubTab::Network => super::host_view::render_network_panel(state, ui, p),
        ServerSubTab::Console => {
            if ui.available_width() >= 960.0 {
                ui.columns(2, |cols| {
                    render_console_panel(state, &mut cols[0], p);
                    super::host_view::render_players_and_banlist_card(state, &mut cols[1], p);
                });
            } else {
                render_console_panel(state, ui, p);
                ui.add_space(12.0);
                super::host_view::render_players_and_banlist_card(state, ui, p);
            }
        }
    }
}

fn render_profile_and_controls_card(state: &mut AppState, ui: &mut egui::Ui, p: ThemePalette) {
    let assets = state.assets.clone();
    let is_active = state
        .active_server_supervisor()
        .is_some_and(|s| s.is_active());
    let showing_ext = state.showing_external_server();
    let active_id = state.config.active_server_id.clone();
    let profile_busy = is_active || showing_ext || state.is_profile_running(&active_id);

    if profile_busy && state.confirm_delete_profile_id.as_deref() == Some(active_id.as_str()) {
        state.confirm_delete_profile_id = None;
    }

    rpg_card(ui, assets.as_ref(), p, |ui| {
        rpg_section_heading(
            ui,
            assets.as_ref(),
            p,
            UiIcon::Pack,
            "Hồ sơ & Điều khiển Server",
        );
        ui.add_space(6.0);

        ui.horizontal_wrapped(|ui| {
            let mut selected_profile_id = None;
            for profile in &state.config.servers {
                let active = state.config.active_server_id == profile.id;
                let running =
                    state.is_profile_running(&profile.id) || (active && showing_ext);
                let label = if running {
                    format!(
                        "{} ({}) • Đang chạy",
                        profile.name,
                        profile.edition.label()
                    )
                } else {
                    format!("{} ({})", profile.name, profile.edition.label())
                };
                if rpg_selectable_button(
                    ui,
                    assets.as_ref(),
                    p,
                    active,
                    ButtonSkin::Emerald,
                    UiIcon::Server,
                    &label,
                )
                .clicked()
                    && !active
                {
                    selected_profile_id = Some(profile.id.clone());
                }
            }
            if let Some(id) = selected_profile_id {
                state.select_server_profile(id);
            }

            if rpg_compact_button(
                ui,
                assets.as_ref(),
                p,
                ButtonSkin::Cyan,
                Some(UiIcon::Pack),
                "+ Thêm hồ sơ",
            )
            .clicked()
            {
                state.show_new_profile_form = !state.show_new_profile_form;
            }

            if state.config.servers.len() > 1 {
                let confirming_this = !profile_busy
                    && state.confirm_delete_profile_id.as_deref() == Some(active_id.as_str());
                if confirming_this {
                    if rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Red,
                        Some(UiIcon::Trash),
                        "Xác nhận xóa",
                    )
                    .clicked()
                    {
                        state.delete_active_server_profile();
                        state.confirm_delete_profile_id = None;
                    }
                    if rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Slate,
                        None,
                        "Hủy",
                    )
                    .clicked()
                    {
                        state.confirm_delete_profile_id = None;
                    }
                } else if ui
                    .add_enabled_ui(!profile_busy, |ui| {
                        rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Slate,
                            Some(UiIcon::Trash),
                            "Xóa hồ sơ",
                        )
                    })
                    .inner
                    .on_disabled_hover_text("Tắt server của hồ sơ này trước khi xóa")
                    .clicked()
                {
                    state.confirm_delete_profile_id = Some(active_id.clone());
                }
            }
        });

        if state.show_new_profile_form {
            ui.add_space(6.0);
            rpg_subcard(ui, assets.as_ref(), p, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let name_resp = ui.add(
                        egui::TextEdit::singleline(&mut state.new_profile_name)
                            .desired_width(160.0)
                            .hint_text("Tên hồ sơ mới"),
                    );
                    egui::ComboBox::from_id_salt("srv_new_edition")
                        .selected_text(state.new_profile_edition.label())
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut state.new_profile_edition,
                                GameEdition::TModLoader,
                                GameEdition::TModLoader.label(),
                            );
                            ui.selectable_value(
                                &mut state.new_profile_edition,
                                GameEdition::Vanilla,
                                GameEdition::Vanilla.label(),
                            );
                        });
                    let enter_submit = name_resp.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let create_clicked = rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Cyan,
                        Some(UiIcon::Pack),
                        "+ Tạo",
                    )
                    .clicked();
                    if create_clicked || enter_submit {
                        state.add_server_profile();
                        state.show_new_profile_form = false;
                    }
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
                        state.show_new_profile_form = false;
                    }
                });
            });
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);

        let is_active = state
            .active_server_supervisor()
            .is_some_and(|s| s.is_active());
        let is_stopping_sup = state
            .active_server_supervisor()
            .is_some_and(|s| s.is_stopping());
        let active_snap = state.active_server_snapshot().cloned();
        let active_snap_state = active_snap.as_ref().map(|s| &s.state);
        let showing_ext = state.showing_external_server();
        let ext_starting = state.is_active_external_starting();
        let ext_server = if showing_ext {
            state.active_external_server().cloned()
        } else {
            None
        };
        let ext_summary = state.active_external_log_summary().cloned();
        let max_players = state
            .config
            .active_server()
            .map(|s| s.config.maxplayers)
            .unwrap_or(8);

        let hover_metrics = if let Some(ext) = &ext_server {
            Some(format!(
                "PID {} • {} MB RAM • {:.1}% CPU",
                ext.pid, ext.memory_mb, ext.cpu_percent
            ))
        } else if is_active {
            active_snap.as_ref().and_then(|snap| {
                snap.pid.map(|pid| {
                    format!(
                        "PID {pid} • {} MB RAM • {:.1}% CPU",
                        snap.memory_mb, snap.cpu_percent
                    )
                })
            })
        } else {
            None
        };

        let direct_status = state.active_direct_host_status();
        let direct_running_or_starting = matches!(
            direct_status,
            DirectHostStatus::Running(_) | DirectHostStatus::Starting { .. }
        );
        let direct_open_or_pending =
            direct_running_or_starting || state.pending_auto_direct_host_profile_id.is_some();

        ui.horizontal_wrapped(|ui| {
            let status_color = state.badge_color_for_server();
            let status_text = state.server_status_short();
            if let Some(tip) = &hover_metrics {
                ui.scope(|ui| {
                    status_pill(ui, status_text, status_color);
                })
                .response
                .on_hover_text(tip);
            } else {
                status_pill(ui, status_text, status_color);
            }

            if let Some(ext) = &ext_server {
                let port = ext.port.unwrap_or_else(|| state.active_server_port());
                let player_cnt = ext_summary.as_ref().map(|s| s.players.len()).unwrap_or(0);
                status_pill(
                    ui,
                    &format!("Cổng {port} • {player_cnt}/{max_players} người"),
                    p.accent_cyan,
                );
            } else if is_active {
                let port = active_snap
                    .as_ref()
                    .map(|s| s.port)
                    .filter(|&pt| pt > 0)
                    .unwrap_or_else(|| state.active_server_port());
                let player_cnt = active_snap.as_ref().map(|s| s.players.len()).unwrap_or(0);
                status_pill(
                    ui,
                    &format!("Cổng {port} • {player_cnt}/{max_players} người"),
                    p.accent_cyan,
                );
                if let Some(snap) = &active_snap {
                    if !matches!(snap.state, ServerLifecycleState::Running)
                        && !snap.status_detail.trim().is_empty()
                    {
                        ui.label(
                            RichText::new(&snap.status_detail)
                                .size(12.5)
                                .color(p.text_muted),
                        );
                    }
                }
            } else {
                if let Some(snap) = &active_snap {
                    if matches!(snap.state, ServerLifecycleState::Crashed(_))
                        && !snap.status_detail.trim().is_empty()
                    {
                        ui.label(
                            RichText::new(&snap.status_detail)
                                .size(12.5)
                                .color(p.accent_red),
                        );
                    }
                }
                if state.network_snapshot.local_port_listening {
                    status_pill(
                        ui,
                        &format!("Cổng {} đang bận", state.active_server_port()),
                        p.accent_amber,
                    );
                }
            }

            if let Some(ep) = state.active_online_endpoint() {
                status_pill(ui, &format!("Online: {ep}"), p.accent_emerald);
                if rpg_compact_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Cyan,
                    Some(UiIcon::Copy),
                    "Sao chép",
                )
                .clicked()
                {
                    ui.ctx().copy_text(ep.clone());
                    state.notify_ok(format!("Đã sao chép {ep}"));
                }
            } else if matches!(direct_status, DirectHostStatus::Starting { .. })
                || state.pending_auto_direct_host_profile_id.is_some()
            {
                status_pill(ui, "Đang mở cổng Online...", p.accent_cyan);
                ui.spinner();
            } else if let DirectHostStatus::Running(info) = &direct_status {
                if let Some(v6) = &info.ipv6_endpoint {
                    status_pill(ui, &format!("IPv6: {v6}"), p.accent_emerald);
                    if rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Cyan,
                        Some(UiIcon::Copy),
                        "Sao chép",
                    )
                    .clicked()
                    {
                        ui.ctx().copy_text(v6.clone());
                        state.notify_ok(format!("Đã sao chép {v6}"));
                    }
                }
            } else if let DirectHostStatus::Error(err) = &direct_status {
                if state.server_tab != ServerSubTab::Network {
                    ui.scope(|ui| {
                        status_pill(ui, "Lỗi mở cổng Online", p.accent_red);
                    })
                    .response
                    .on_hover_text(err);
                }
            }
        });

        ui.add_space(8.0);

        let is_running = matches!(active_snap_state, Some(ServerLifecycleState::Running));
        let is_stopping =
            matches!(active_snap_state, Some(ServerLifecycleState::Stopping)) || is_stopping_sup;

        ui.horizontal_wrapped(|ui| {
            if is_active {
                if ui
                    .add_enabled_ui(!is_stopping, |ui| {
                        rpg_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Amber,
                            UiIcon::Stop,
                            "Lưu & Tắt",
                        )
                    })
                    .inner
                    .clicked()
                {
                    state.stop_selected_server();
                }

                if rpg_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Red,
                    UiIcon::Stop,
                    "Dừng gấp",
                )
                .clicked()
                {
                    state.force_kill_server();
                }

                if is_running
                    && ui
                        .add_enabled_ui(!state.is_game_running(), |ui| {
                            rpg_button(
                                ui,
                                assets.as_ref(),
                                p,
                                ButtonSkin::Cyan,
                                UiIcon::Play,
                                "Vào Server",
                            )
                        })
                        .inner
                        .on_disabled_hover_text("Đóng cửa sổ game đang mở trước")
                        .clicked()
                {
                    state.join_own_hosted_server();
                }

                if direct_open_or_pending {
                    if rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Slate,
                        UiIcon::Stop,
                        "Đóng Online",
                    )
                    .clicked()
                    {
                        if direct_running_or_starting {
                            state.toggle_direct_host();
                        } else {
                            state.pending_auto_direct_host_profile_id = None;
                            state.notify_ok("Đã hủy tự mở cổng Online.");
                        }
                    }
                } else if !is_stopping
                    && rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Emerald,
                        UiIcon::Host,
                        "Mở Online",
                    )
                    .clicked()
                {
                    state.start_self_host_online();
                    state.server_tab = ServerSubTab::Network;
                }
            } else if ext_server.is_some() {
                if rpg_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Red,
                    UiIcon::Stop,
                    "Tắt Server",
                )
                .clicked()
                {
                    state.stop_external_server();
                }

                if !ext_starting
                    && ui
                        .add_enabled_ui(!state.is_game_running(), |ui| {
                            rpg_button(
                                ui,
                                assets.as_ref(),
                                p,
                                ButtonSkin::Cyan,
                                UiIcon::Play,
                                "Vào Server",
                            )
                        })
                        .inner
                        .on_disabled_hover_text("Đóng cửa sổ game đang mở trước")
                        .clicked()
                {
                    state.join_own_hosted_server();
                }

                if direct_open_or_pending {
                    if rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Slate,
                        UiIcon::Stop,
                        "Đóng Online",
                    )
                    .clicked()
                    {
                        if direct_running_or_starting {
                            state.toggle_direct_host();
                        } else {
                            state.pending_auto_direct_host_profile_id = None;
                            state.notify_ok("Đã hủy tự mở cổng Online.");
                        }
                    }
                } else if rpg_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Emerald,
                    UiIcon::Host,
                    "Mở Online",
                )
                .clicked()
                {
                    state.start_self_host_online();
                    state.server_tab = ServerSubTab::Network;
                }
            } else {
                if rpg_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Cyan,
                    UiIcon::Play,
                    "Khởi chạy Server",
                )
                .clicked()
                {
                    state.start_selected_server();
                    state.server_tab = ServerSubTab::Console;
                }
                if rpg_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Emerald,
                    UiIcon::Host,
                    "Khởi chạy & Mở Online",
                )
                .clicked()
                {
                    state.start_self_host_online();
                    state.server_tab = ServerSubTab::Network;
                }
            }
        });

        render_other_running_servers(state, ui, p);
    });
}

fn lifecycle_color(state: &ServerLifecycleState, p: ThemePalette) -> egui::Color32 {
    match state {
        ServerLifecycleState::Running => p.accent_emerald,
        ServerLifecycleState::Starting => p.accent_cyan,
        ServerLifecycleState::Stopping => p.accent_amber,
        ServerLifecycleState::Crashed(_) => p.accent_red,
        ServerLifecycleState::Stopped => p.text_muted,
    }
}

fn render_other_running_servers(state: &mut AppState, ui: &mut egui::Ui, p: ThemePalette) {
    let active_id = state.config.active_server_id.clone();
    let showing_ext = state.showing_external_server();
    let active_ext_pid = if showing_ext {
        state.active_external_server().map(|e| e.pid)
    } else {
        None
    };

    let other_supervised: Vec<_> = state
        .running_supervised_snapshots()
        .into_iter()
        .filter(|(profile_id, _)| profile_id != &active_id)
        .collect();
    let other_externals: Vec<_> = state
        .external_servers
        .iter()
        .filter(|ext| {
            ext.profile_id.as_deref() != Some(active_id.as_str())
                && Some(ext.pid) != active_ext_pid
        })
        .cloned()
        .collect();

    let other_count = other_supervised.len() + other_externals.len();
    if other_count == 0 {
        return;
    }

    let assets = state.assets.clone();
    let total_running = state.total_running_servers_count();

    ui.add_space(10.0);
    ui.separator();
    ui.add_space(6.0);

    let mut select_profile: Option<String> = None;
    let mut stop_profile: Option<String> = None;
    let mut stop_ext_pid: Option<u32> = None;
    let mut stop_all_clicked = false;

    ui.horizontal(|ui| {
        rpg_section_heading(
            ui,
            assets.as_ref(),
            p,
            UiIcon::Server,
            &format!("Server khác đang chạy ({other_count})"),
        );
        if total_running >= 2 {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if rpg_compact_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Red,
                    Some(UiIcon::Stop),
                    "Tắt tất cả",
                )
                .clicked()
                {
                    stop_all_clicked = true;
                }
            });
        }
    });
    ui.add_space(4.0);

    for (profile_id, snap) in &other_supervised {
        ui.push_id(("srv_overview_sup", profile_id), |ui| {
            rpg_subcard(ui, assets.as_ref(), p, |ui| {
                let name = if snap.profile_name.trim().is_empty() {
                    snap.edition.label()
                } else {
                    snap.profile_name.as_str()
                };
                let max_p = state
                    .config
                    .servers
                    .iter()
                    .find(|s| s.id == *profile_id)
                    .map(|s| s.config.maxplayers)
                    .unwrap_or(8);

                ui.horizontal(|ui| {
                    ui.label(RichText::new(name).strong().color(p.text_primary));
                    ui.label(RichText::new(snap.edition.label()).size(12.0).color(p.text_muted));
                    status_pill(ui, snap.state.label(), lifecycle_color(&snap.state, p));

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Amber,
                            Some(UiIcon::Stop),
                            "Lưu & Tắt",
                        )
                        .clicked()
                        {
                            stop_profile = Some(profile_id.clone());
                        }
                        if rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Cyan,
                            Some(UiIcon::Server),
                            "Xem",
                        )
                        .clicked()
                        {
                            select_profile = Some(profile_id.clone());
                        }
                    });
                });

                ui.label(
                    RichText::new(format!(
                        "Cổng {} • {}/{} người",
                        snap.port,
                        snap.players.len(),
                        max_p
                    ))
                    .size(12.0)
                    .color(p.text_muted),
                );
            });
        });
    }

    for ext in &other_externals {
        ui.push_id(("srv_overview_ext", ext.pid), |ui| {
            rpg_subcard(ui, assets.as_ref(), p, |ui| {
                let matched_profile = ext
                    .profile_id
                    .as_deref()
                    .and_then(|pid| state.config.servers.iter().find(|s| s.id == pid));
                let name = matched_profile
                    .map(|s| s.name.as_str())
                    .unwrap_or("Server độc lập");
                let port_label = ext
                    .port
                    .or_else(|| matched_profile.map(|s| s.config.port))
                    .map(|pt| pt.to_string())
                    .unwrap_or_else(|| "?".to_string());
                let summary = ext
                    .profile_id
                    .as_deref()
                    .and_then(|pid| state.external_log_summaries.get(pid));
                let ext_starting = summary.is_some_and(|s| {
                    !s.is_listening && (!s.lines.is_empty() || !s.status_detail.is_empty())
                });
                let (status_label, status_color) = if ext_starting {
                    ("Đang khởi động (phiên trước)...", p.accent_cyan)
                } else {
                    ("Đang chạy (phiên trước)", p.accent_emerald)
                };
                let player_cnt = summary.map(|s| s.players.len()).unwrap_or(0);
                let players_text = if let Some(prof) = matched_profile {
                    format!("{player_cnt}/{} người", prof.config.maxplayers)
                } else {
                    format!("{player_cnt} người")
                };

                ui.horizontal(|ui| {
                    ui.label(RichText::new(name).strong().color(p.text_primary));
                    ui.label(RichText::new(ext.edition.label()).size(12.0).color(p.text_muted));
                    status_pill(ui, status_label, status_color);

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Red,
                            Some(UiIcon::Stop),
                            "Tắt",
                        )
                        .clicked()
                        {
                            stop_ext_pid = Some(ext.pid);
                        }
                        if let Some(profile) = matched_profile {
                            if rpg_compact_button(
                                ui,
                                assets.as_ref(),
                                p,
                                ButtonSkin::Cyan,
                                Some(UiIcon::Server),
                                "Xem",
                            )
                            .clicked()
                            {
                                select_profile = Some(profile.id.clone());
                            }
                        }
                    });
                });

                ui.label(
                    RichText::new(format!("Cổng {port_label} • {players_text}"))
                        .size(12.0)
                        .color(p.text_muted),
                );
            });
        });
    }

    if stop_all_clicked {
        state.stop_all_servers();
    } else if let Some(id) = stop_profile {
        state.stop_server_profile(&id);
    } else if let Some(pid) = stop_ext_pid {
        state.stop_external_pid(pid);
    } else if let Some(id) = select_profile {
        state.select_server_profile(id);
    }
}

fn difficulty_vn_label(diff: WorldDifficultyOption) -> &'static str {
    match diff {
        WorldDifficultyOption::Classic => "Thường",
        WorldDifficultyOption::Expert => "Expert",
        WorldDifficultyOption::Master => "Master",
        WorldDifficultyOption::Journey => "Hành trình",
    }
}

fn world_meta_size_vn(w: &tl_core::WorldMetadata) -> &'static str {
    match w.size_label() {
        "Small" => "Nhỏ",
        "Medium" => "Vừa",
        "Large" => "Lớn",
        other => other,
    }
}

fn world_meta_diff_vn(w: &tl_core::WorldMetadata) -> &'static str {
    match w.difficulty_label() {
        "Classic" => "Thường",
        "Journey" => "Hành trình",
        other => other,
    }
}

fn open_in_explorer(path: &std::path::Path) {
    let _ = std::fs::create_dir_all(path);
    let _ = open::that(path);
}

fn render_config_editor(state: &mut AppState, ui: &mut egui::Ui, p: ThemePalette) {
    let Some(profile_edition) = state.config.active_server().map(|prof| prof.edition) else {
        return;
    };
    let assets = state.assets.clone();
    let existing_worlds = state.worlds_for_edition(profile_edition).to_vec();
    let edition_worlds_dir = match profile_edition {
        GameEdition::Vanilla => state.paths.vanilla_worlds_dir(),
        GameEdition::TModLoader => state.paths.tml_worlds_dir(),
    };
    let is_active = state
        .active_server_supervisor()
        .is_some_and(|s| s.is_active())
        || state.showing_external_server();
    let direct_host_active = matches!(
        state.active_direct_host_status(),
        DirectHostStatus::Running(_) | DirectHostStatus::Starting { .. }
    );

    let mut save_clicked = false;
    let mut port_edited = false;

    {
        let modpacks = &state.modpacks;
        let server_port_input = &mut state.server_port_input;
        let Some(profile) = state.config.active_server_mut() else {
            return;
        };

        rpg_card(ui, assets.as_ref(), p, |ui| {
            ui.horizontal(|ui| {
                rpg_section_heading(ui, assets.as_ref(), p, UiIcon::Settings, "Cấu hình Server");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Cyan,
                        Some(UiIcon::Server),
                        "Lưu cấu hình",
                    )
                    .clicked()
                    {
                        save_clicked = true;
                    }
                });
            });
            if is_active {
                ui.label(
                    RichText::new("Áp dụng cho lần khởi động kế tiếp")
                        .size(11.5)
                        .color(p.text_muted),
                );
            }
            ui.add_space(8.0);

            egui::Grid::new("server_cfg_grid")
                .num_columns(2)
                .spacing([14.0, 8.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Tên hồ sơ:").color(p.text_muted));
                    ui.add(
                        egui::TextEdit::singleline(&mut profile.name)
                            .desired_width(200.0)
                            .hint_text("Tên hồ sơ"),
                    );
                    ui.end_row();

                    ui.label(RichText::new("Thế giới:").color(p.text_muted));
                    ui.horizontal_wrapped(|ui| {
                        let worldname_resp = ui.add(
                            egui::TextEdit::singleline(&mut profile.config.worldname)
                                .desired_width(180.0)
                                .hint_text("Tên thế giới"),
                        );
                        if worldname_resp.changed() {
                            let matches_selected_existing = existing_worlds.iter().any(|w| {
                                w.world_name == profile.config.worldname
                                    && w.file_path
                                        .to_string_lossy()
                                        .eq_ignore_ascii_case(&profile.config.world)
                            });
                            if !matches_selected_existing {
                                let sanitized: String = profile
                                    .config
                                    .worldname
                                    .chars()
                                    .filter(|c| {
                                        c.is_alphanumeric()
                                            || *c == ' '
                                            || *c == '_'
                                            || *c == '-'
                                    })
                                    .collect::<String>()
                                    .trim()
                                    .to_string();
                                let file_stem = if sanitized.is_empty() {
                                    "World"
                                } else {
                                    sanitized.as_str()
                                };
                                profile.config.world = edition_worlds_dir
                                    .join(format!("{file_stem}.wld"))
                                    .to_string_lossy()
                                    .into_owned();
                                profile.config.worldpath =
                                    edition_worlds_dir.to_string_lossy().into_owned();
                            }
                        }

                        egui::ComboBox::from_id_salt("srv_pick_wld")
                            .selected_text("Chọn thế giới có sẵn")
                            .width(156.0)
                            .show_ui(ui, |ui| {
                                if existing_worlds.is_empty() {
                                    ui.label(
                                        RichText::new("Chưa có thế giới lưu sẵn")
                                            .color(p.text_muted),
                                    );
                                } else {
                                    for w in &existing_worlds {
                                        let w_path_str =
                                            w.file_path.to_string_lossy().into_owned();
                                        let selected = profile
                                            .config
                                            .world
                                            .eq_ignore_ascii_case(&w_path_str);
                                        let item_label = format!(
                                            "{} ({}, {})",
                                            w.world_name,
                                            world_meta_size_vn(w),
                                            world_meta_diff_vn(w)
                                        );
                                        if ui.selectable_label(selected, item_label).clicked() {
                                            profile.config.world = w_path_str;
                                            profile.config.worldname = w.world_name.clone();
                                            profile.config.worldpath = edition_worlds_dir
                                                .to_string_lossy()
                                                .into_owned();
                                        }
                                    }
                                }
                            });

                        if rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Slate,
                            Some(UiIcon::Folder),
                            "Thư mục",
                        )
                        .clicked()
                        {
                            open_in_explorer(&edition_worlds_dir);
                        }
                    });
                    ui.end_row();

                    ui.label(RichText::new("Khi chưa có file:").color(p.text_muted));
                    let autocreate_label = match profile.config.autocreate {
                        Some(1) => "Tự tạo Nhỏ",
                        Some(2) => "Tự tạo Vừa",
                        Some(3) => "Tự tạo Lớn",
                        _ => "Không tự tạo",
                    };
                    egui::ComboBox::from_id_salt("srv_autocreate")
                        .selected_text(autocreate_label)
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut profile.config.autocreate,
                                None,
                                "Không tự tạo",
                            );
                            ui.selectable_value(
                                &mut profile.config.autocreate,
                                Some(1),
                                "Tự tạo Nhỏ",
                            );
                            ui.selectable_value(
                                &mut profile.config.autocreate,
                                Some(2),
                                "Tự tạo Vừa",
                            );
                            ui.selectable_value(
                                &mut profile.config.autocreate,
                                Some(3),
                                "Tự tạo Lớn",
                            );
                        });
                    ui.end_row();

                    if profile.config.autocreate.is_some() {
                        ui.label(RichText::new("Độ khó thế giới mới:").color(p.text_muted));
                        let diff = WorldDifficultyOption::from_u8(profile.config.difficulty);
                        egui::ComboBox::from_id_salt("srv_difficulty")
                            .selected_text(difficulty_vn_label(diff))
                            .width(130.0)
                            .show_ui(ui, |ui| {
                                for opt in [
                                    WorldDifficultyOption::Classic,
                                    WorldDifficultyOption::Expert,
                                    WorldDifficultyOption::Master,
                                    WorldDifficultyOption::Journey,
                                ] {
                                    ui.selectable_value(
                                        &mut profile.config.difficulty,
                                        opt as u8,
                                        difficulty_vn_label(opt),
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label(RichText::new("Seed thế giới mới:").color(p.text_muted));
                        ui.vertical(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut profile.config.seed)
                                    .desired_width(200.0)
                                    .hint_text("Để trống = ngẫu nhiên"),
                            );
                            if profile.edition == GameEdition::Vanilla {
                                egui::CollapsingHeader::new("Seed đặc biệt (1.4.5)")
                                    .id_salt("srv_special_seeds_145")
                                    .default_open(false)
                                    .show(ui, |ui| {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.checkbox(
                                                &mut profile.config.seed_celebration,
                                                "celebrationmk10",
                                            );
                                            ui.checkbox(
                                                &mut profile.config.seed_theconstant,
                                                "constant",
                                            );
                                            ui.checkbox(
                                                &mut profile.config.seed_notthebees,
                                                "not the bees",
                                            );
                                            ui.checkbox(
                                                &mut profile.config.seed_notraps,
                                                "no traps",
                                            );
                                            ui.checkbox(
                                                &mut profile.config.seed_fortheworthy,
                                                "for the worthy",
                                            );
                                            ui.checkbox(
                                                &mut profile.config.seed_remix,
                                                "don't dig up",
                                            );
                                            ui.checkbox(
                                                &mut profile.config.seed_drunk,
                                                "drunk world",
                                            );
                                            ui.checkbox(
                                                &mut profile.config.seed_zenith,
                                                "get fixed boi",
                                            );
                                        });
                                    });
                            }
                        });
                        ui.end_row();
                    }

                    ui.label(RichText::new("Cổng TCP:").color(p.text_muted));
                    ui.horizontal_wrapped(|ui| {
                        let port_resp = ui.add(
                            egui::TextEdit::singleline(server_port_input)
                                .desired_width(76.0)
                                .hint_text("7777"),
                        );
                        if port_resp.changed() {
                            if let Some(p_val) = server_port_input
                                .trim()
                                .parse::<u16>()
                                .ok()
                                .filter(|&pt| pt > 0)
                            {
                                profile.config.port = p_val;
                                port_edited = true;
                            }
                        }
                        if port_resp.lost_focus()
                            && server_port_input
                                .trim()
                                .parse::<u16>()
                                .ok()
                                .filter(|&pt| pt > 0)
                                .is_none()
                        {
                            *server_port_input = profile.config.port.max(1).to_string();
                        }

                        ui.add_space(4.0);
                        ui.label(RichText::new("Số người tối đa:").color(p.text_muted));
                        let mut mp = profile.config.maxplayers as i32;
                        if ui
                            .add(egui::DragValue::new(&mut mp).range(1..=255))
                            .changed()
                        {
                            profile.config.maxplayers = mp.clamp(1, 255) as u16;
                        }
                    });
                    ui.end_row();

                    ui.label(RichText::new("Mật khẩu phòng:").color(p.text_muted));
                    ui.add(
                        egui::TextEdit::singleline(&mut profile.config.password)
                            .password(true)
                            .desired_width(220.0)
                            .hint_text("Tùy chọn — để trống nếu không đặt"),
                    );
                    ui.end_row();

                    ui.label(RichText::new("Lời chào (MOTD):").color(p.text_muted));
                    ui.add(
                        egui::TextEdit::singleline(&mut profile.config.motd)
                            .desired_width(240.0)
                            .hint_text("Thông điệp hiện khi người chơi vào server"),
                    );
                    ui.end_row();

                    if profile.edition == GameEdition::TModLoader {
                        ui.label(RichText::new("Gói ModPack:").color(p.text_muted));
                        let mp_label = if profile.config.modpack.is_empty() {
                            "Mặc định (Mod đang bật)".to_string()
                        } else {
                            profile.config.modpack.clone()
                        };
                        egui::ComboBox::from_id_salt("srv_modpack")
                            .selected_text(mp_label)
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut profile.config.modpack,
                                    String::new(),
                                    "Mặc định (Mod đang bật)",
                                );
                                for pack in modpacks {
                                    ui.selectable_value(
                                        &mut profile.config.modpack,
                                        pack.name.clone(),
                                        &pack.name,
                                    );
                                }
                            });
                        ui.end_row();
                    }

                    ui.label(RichText::new("Tùy chọn:").color(p.text_muted));
                    ui.horizontal_wrapped(|ui| {
                        ui.checkbox(&mut profile.auto_restart, "Tự khởi động lại nếu sập");
                        ui.checkbox(&mut profile.config.secure, "Chống gian lận");
                        ui.checkbox(&mut profile.config.upnp, "Tự mở cổng UPnP");
                        if profile.edition == GameEdition::Vanilla {
                            ui.checkbox(&mut profile.config.slowliquids, "Giảm tải nước chảy");
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Số bản lưu lùi:").color(p.text_muted));
                                ui.add(
                                    egui::DragValue::new(&mut profile.config.worldrollbackstokeep)
                                        .range(0..=20),
                                );
                            });
                        }
                    });
                    ui.end_row();

                    ui.label(RichText::new("Tham số nâng cao:").color(p.text_muted));
                    ui.add(
                        egui::TextEdit::singleline(&mut profile.extra_args)
                            .desired_width(240.0)
                            .hint_text("Tùy chọn"),
                    );
                    ui.end_row();
                });
        });
    }

    if port_edited && !direct_host_active {
        let port_str = state.active_server_port().to_string();
        state.port_probe_port = port_str.clone();
        state.direct_host_port_input = port_str;
    }
    if save_clicked {
        state.save_config();
        state.notify_ok("Đã lưu cấu hình server.");
    }
}

fn render_console_lines(
    ui: &mut egui::Ui,
    p: ThemePalette,
    logs: &[ConsoleLogLine],
    empty_hint: &str,
) {
    if logs.is_empty() {
        ui.label(RichText::new(empty_hint).monospace().color(p.text_muted));
        return;
    }
    for line in logs {
        let color = if line.is_error {
            p.accent_red
        } else if line.text.starts_with('>') {
            p.accent_cyan
        } else if line.text.contains("Listening on port") || line.text.contains("Server started") {
            p.accent_emerald
        } else {
            p.console_text
        };
        ui.label(
            RichText::new(format!("[{}] {}", line.timestamp, line.text))
                .monospace()
                .size(12.0)
                .color(color),
        );
    }
}

fn render_console_panel(state: &mut AppState, ui: &mut egui::Ui, p: ThemePalette) {
    let assets = state.assets.clone();
    let sup_active = state
        .active_server_supervisor()
        .is_some_and(|s| s.is_active());
    let snap_state = state.active_server_snapshot().map(|s| &s.state);
    let has_stdin = sup_active && matches!(snap_state, Some(ServerLifecycleState::Running));

    rpg_card(ui, assets.as_ref(), p, |ui| {
        ui.horizontal(|ui| {
            rpg_section_heading(ui, assets.as_ref(), p, UiIcon::Server, "Console trực tiếp");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if rpg_compact_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Slate,
                    Some(UiIcon::Trash),
                    "Xóa log",
                )
                .clicked()
                {
                    state.clear_active_server_logs();
                }
                ui.checkbox(&mut state.console_autoscroll, "Cuộn tự động");
            });
        });

        ui.add_space(6.0);
        ui.add_enabled_ui(has_stdin, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (label, cmd, skin, icon) in [
                    ("Lưu thế giới", "save", ButtonSkin::Amber, UiIcon::Download),
                    ("Bình minh", "dawn", ButtonSkin::Slate, UiIcon::Sun),
                    ("Giữa trưa", "noon", ButtonSkin::Slate, UiIcon::Sun),
                    ("Hoàng hôn", "dusk", ButtonSkin::Slate, UiIcon::Moon),
                    ("Nửa đêm", "midnight", ButtonSkin::Slate, UiIcon::Moon),
                    ("Ổn định nước", "settle", ButtonSkin::Slate, UiIcon::Refresh),
                ] {
                    if rpg_compact_button(ui, assets.as_ref(), p, skin, Some(icon), label).clicked()
                    {
                        state.send_server_command(cmd);
                    }
                }
            });
        });

        ui.add_space(6.0);
        console_frame(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .id_salt("srv_console_scroll")
                .max_height(340.0)
                .min_scrolled_height(300.0)
                .stick_to_bottom(state.console_autoscroll)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if !state.showing_external_server() {
                        if let Some(sup) = state.active_server_supervisor() {
                            let logs = sup.logs();
                            render_console_lines(ui, p, &logs, "Chưa có dữ liệu log.");
                        } else {
                            ui.label(
                                RichText::new("Nhấn 'Khởi chạy Server' để xem log thực tế tại đây.")
                                    .monospace()
                                    .color(p.text_muted),
                            );
                        }
                    } else if let Some(summary) = state.active_external_log_summary() {
                        render_console_lines(
                            ui,
                            p,
                            &summary.lines,
                            "Chưa có log mới hoặc log đã được xóa.",
                        );
                    } else {
                        ui.label(
                            RichText::new(
                                "Server chạy từ phiên Launcher trước chưa ghi nhận file log.",
                            )
                            .monospace()
                            .color(p.text_muted),
                        );
                    }
                });
        });

        ui.add_space(8.0);
        let input_hint = if state.showing_external_server() {
            "Server phiên trước chỉ xem log — khởi chạy lại để gửi lệnh"
        } else {
            "Nhập lệnh server (vd: say Xin chao!, save, kick Ten)..."
        };
        ui.add_enabled_ui(has_stdin, |ui| {
            ui.horizontal(|ui| {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut state.console_input)
                        .desired_width((ui.available_width() - 116.0).max(120.0))
                        .hint_text(input_hint),
                );
                let submit = rpg_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Emerald,
                    UiIcon::Play,
                    "Gửi lệnh",
                )
                .clicked()
                    || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                if submit && !state.console_input.trim().is_empty() {
                    let cmd = std::mem::take(&mut state.console_input);
                    state.send_server_command(&cmd);
                }
            });
        });
    });
}
