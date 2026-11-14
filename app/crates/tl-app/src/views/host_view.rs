use crate::anim3d;
use crate::assets::{ButtonSkin, LauncherAssets, UiIcon};
use crate::state::AppState;
use crate::theme::{
    page_title, rpg_button, rpg_card, rpg_compact_button, rpg_section_heading, rpg_subcard,
    status_pill, ThemePalette,
};
use egui::RichText;
use tl_runtime::TunnelStatus;

pub fn render(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    let tunnel_online = matches!(state.active_tunnel_status(), TunnelStatus::Running { .. });
    let upnp_active = state.upnp_lease.is_some();

    ui.horizontal(|ui| {
        anim3d::render_host_radar_3d(
            ui,
            p,
            assets.nav_host.as_ref(),
            tunnel_online,
            upnp_active,
        );
        ui.vertical(|ui| {
            ui.add_space(12.0);
            page_title(ui, p, "Quản trị Host Chơi Online");
        });
    });
    ui.add_space(8.0);

    let port = state.active_server_port();

    rpg_card(ui, assets.as_ref(), p, |ui| {
        ui.horizontal(|ui| {
            rpg_section_heading(
                ui,
                assets.as_ref(),
                p,
                UiIcon::Host,
                "Địa chỉ kết nối cho người chơi",
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled_ui(!state.network_loading, |ui| {
                        rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Slate,
                            Some(UiIcon::Refresh),
                            "Làm mới mạng",
                        )
                    })
                    .inner
                    .clicked()
                {
                    state.spawn_network_probe();
                }
                if state.network_loading {
                    ui.spinner();
                }
            });
        });
        ui.add_space(8.0);

        if let Some(lan) = state.network_snapshot.primary_lan_ip {
            let ep = format!("{lan}:{port}");
            endpoint_row(
                ui,
                assets.as_ref(),
                p,
                "Mạng LAN",
                &ep,
                p.accent_cyan,
                state,
            );
        }

        if let Some(pub_ip) = state.network_snapshot.public_ip {
            let ep = format!("{pub_ip}:{port}");
            endpoint_row(
                ui,
                assets.as_ref(),
                p,
                "IP Công cộng",
                &ep,
                p.accent_emerald,
                state,
            );
        } else if let Some(err) = &state.network_snapshot.public_ip_error {
            rpg_subcard(ui, assets.as_ref(), p, |ui| {
                ui.label(RichText::new(format!("IP Công cộng: {err}")).color(p.text_muted));
            });
            ui.add_space(4.0);
        }

        if let Some(tunnel_ep) = state.active_tunnel_endpoint() {
            endpoint_row(
                ui,
                assets.as_ref(),
                p,
                "Tunnel Online",
                &tunnel_ep,
                p.accent_amber,
                state,
            );
        }
    });

    ui.add_space(12.0);

    rpg_card(ui, assets.as_ref(), p, |ui| {
        rpg_section_heading(
            ui,
            assets.as_ref(),
            p,
            UiIcon::Key,
            &format!("Mở cổng & Chuyển tiếp (Port {port})"),
        );
        ui.add_space(8.0);

        rpg_subcard(ui, assets.as_ref(), p, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Windows Firewall:").strong());
                match state.network_snapshot.firewall_rule_active {
                    Some(true) => status_pill(ui, "Đã mở", p.accent_emerald),
                    Some(false) => status_pill(ui, "Chưa mở", p.accent_amber),
                    None => status_pill(ui, "Chưa kiểm tra", p.text_muted),
                }
                ui.checkbox(&mut state.config.auto_firewall, "Tự mở khi bật Server");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Red,
                        Some(UiIcon::Trash),
                        "Gỡ quy tắc",
                    )
                    .clicked()
                    {
                        state.remove_firewall(port);
                    }
                    if rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Emerald,
                        Some(UiIcon::Host),
                        "Mở Firewall TCP",
                    )
                    .clicked()
                    {
                        state.ensure_firewall(port);
                    }
                });
            });
        });
        ui.add_space(6.0);

        rpg_subcard(ui, assets.as_ref(), p, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("UPnP Router:").strong());
                if let Some(lease) = &state.upnp_lease {
                    status_pill(
                        ui,
                        &format!("Đang mở cổng {}", lease.port),
                        p.accent_emerald,
                    );
                    if lease.is_cgnat {
                        status_pill(ui, "Phát hiện CGNAT — hãy dùng Tunnel", p.accent_amber);
                    }
                    if let Some(ext) = lease.external_ip {
                        ui.label(
                            RichText::new(format!("WAN IP: {ext}"))
                                .monospace()
                                .color(p.text_muted),
                        );
                    }
                } else {
                    status_pill(ui, "Chưa mở", p.text_muted);
                }
                ui.checkbox(&mut state.config.auto_upnp, "Tự mở khi bật Server");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if state.upnp_lease.is_some() {
                        if rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Red,
                            Some(UiIcon::Stop),
                            "Đóng UPnP",
                        )
                        .clicked()
                        {
                            state.close_upnp();
                        }
                    } else if rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Cyan,
                        Some(UiIcon::Key),
                        "Mở UPnP Router",
                    )
                    .clicked()
                    {
                        state.open_upnp(port);
                    }
                });
            });
        });
        ui.add_space(6.0);

        rpg_subcard(ui, assets.as_ref(), p, |ui| {
            let tunnel_state = state.active_tunnel_status();
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Public TCP Tunnel:").strong());
                match &tunnel_state {
                    TunnelStatus::Idle | TunnelStatus::Stopped => {
                        status_pill(ui, "Đang tắt", p.text_muted);
                    }
                    TunnelStatus::Connecting => {
                        status_pill(ui, "Đang kết nối...", p.accent_cyan);
                        ui.spinner();
                    }
                    TunnelStatus::Running {
                        remote_host,
                        remote_port,
                    } => {
                        status_pill(
                            ui,
                            &format!("Hoạt động: {remote_host}:{remote_port}"),
                            p.accent_emerald,
                        );
                    }
                    TunnelStatus::Error(err) => {
                        status_pill(ui, err, p.accent_red);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if state.bore_handle.is_some() {
                        if rpg_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Red,
                            UiIcon::Stop,
                            "Tắt Tunnel",
                        )
                        .clicked()
                        {
                            state.toggle_bore_tunnel();
                        }
                    } else if rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Emerald,
                        UiIcon::Play,
                        "Bật Tunnel công khai",
                    )
                    .clicked()
                    {
                        state.toggle_bore_tunnel();
                    }
                });
            });
        });

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Kiểm tra TCP thực tế:").color(p.text_muted));
            ui.add(
                egui::TextEdit::singleline(&mut state.port_probe_host)
                    .desired_width(170.0)
                    .hint_text("IP / Domain"),
            );
            ui.add(
                egui::TextEdit::singleline(&mut state.port_probe_port)
                    .desired_width(64.0)
                    .hint_text("7777"),
            );
            if ui
                .add_enabled_ui(!state.port_probe_busy, |ui| {
                    rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Cyan,
                        Some(UiIcon::Refresh),
                        "Kiểm tra cổng",
                    )
                })
                .inner
                .clicked()
            {
                state.probe_port();
            }
            if state.port_probe_busy {
                ui.spinner();
            }
            if let Some((ok, msg)) = &state.port_probe_status {
                status_pill(ui, msg, if *ok { p.accent_emerald } else { p.accent_red });
            }
        });
    });

    ui.add_space(12.0);

    rpg_card(ui, assets.as_ref(), p, |ui| {
        rpg_section_heading(
            ui,
            assets.as_ref(),
            p,
            UiIcon::Server,
            "Quản trị Người chơi & Banlist",
        );
        ui.add_space(8.0);

        let players = state
            .server_snapshot
            .as_ref()
            .map(|s| s.players.clone())
            .unwrap_or_default();

        if players.is_empty() {
            ui.label(
                RichText::new("Hiện chưa có người chơi nào kết nối vào server.")
                    .color(p.text_muted),
            );
        } else {
            let mut kick_target: Option<String> = None;
            let mut ban_target: Option<String> = None;
            for player in &players {
                rpg_subcard(ui, assets.as_ref(), p, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&player.name).strong());
                        if let Some(ip) = &player.ip {
                            ui.label(RichText::new(ip).monospace().color(p.text_muted));
                        }
                        ui.label(
                            RichText::new(format!("Vào lúc {}", player.joined_at))
                                .size(11.5)
                                .color(p.text_muted),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if rpg_compact_button(
                                ui,
                                assets.as_ref(),
                                p,
                                ButtonSkin::Red,
                                Some(UiIcon::Trash),
                                "Ban",
                            )
                            .clicked()
                            {
                                ban_target = Some(player.name.clone());
                            }
                            if rpg_compact_button(
                                ui,
                                assets.as_ref(),
                                p,
                                ButtonSkin::Amber,
                                Some(UiIcon::Stop),
                                "Kick",
                            )
                            .clicked()
                            {
                                kick_target = Some(player.name.clone());
                            }
                        });
                    });
                });
                ui.add_space(4.0);
            }
            if let Some(name) = kick_target {
                state.kick_player(&name);
            }
            if let Some(name) = ban_target {
                state.ban_player(&name);
            }
        }

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);

        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Danh sách cấm (banlist.txt):").strong());
            ui.add(
                egui::TextEdit::singleline(&mut state.manual_ban_name)
                    .desired_width(130.0)
                    .hint_text("Tên / Ghi chú"),
            );
            ui.add(
                egui::TextEdit::singleline(&mut state.manual_ban_ip)
                    .desired_width(140.0)
                    .hint_text("Địa chỉ IP"),
            );
            if rpg_compact_button(
                ui,
                assets.as_ref(),
                p,
                ButtonSkin::Red,
                Some(UiIcon::Trash),
                "Thêm Ban",
            )
            .clicked()
            {
                state.add_manual_ban();
            }
            if rpg_compact_button(
                ui,
                assets.as_ref(),
                p,
                ButtonSkin::Slate,
                Some(UiIcon::Refresh),
                "Tải lại",
            )
            .clicked()
            {
                state.refresh_banlist();
            }
        });

        ui.add_space(6.0);
        if state.banlist_entries.is_empty() {
            ui.label(RichText::new("Danh sách cấm trống.").color(p.text_muted));
        } else {
            let entries = state.banlist_entries.clone();
            let mut unban_idx: Option<usize> = None;
            for (idx, entry) in entries.iter().enumerate() {
                rpg_subcard(ui, assets.as_ref(), p, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&entry.label).strong());
                        ui.label(RichText::new(&entry.ip).monospace().color(p.text_muted));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if rpg_compact_button(
                                ui,
                                assets.as_ref(),
                                p,
                                ButtonSkin::Emerald,
                                Some(UiIcon::Key),
                                "Gỡ cấm",
                            )
                            .clicked()
                            {
                                unban_idx = Some(idx);
                            }
                        });
                    });
                });
                ui.add_space(3.0);
            }
            if let Some(idx) = unban_idx {
                state.remove_ban_entry(idx);
            }
        }
    });
}

fn endpoint_row(
    ui: &mut egui::Ui,
    assets: &LauncherAssets,
    p: ThemePalette,
    label: &str,
    endpoint: &str,
    color: egui::Color32,
    state: &mut AppState,
) {
    rpg_subcard(ui, assets, p, |ui| {
        ui.horizontal(|ui| {
            status_pill(ui, label, color);
            ui.label(
                RichText::new(endpoint)
                    .monospace()
                    .strong()
                    .size(14.0)
                    .color(p.text_primary),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if rpg_compact_button(
                    ui,
                    assets,
                    p,
                    ButtonSkin::Cyan,
                    Some(UiIcon::Copy),
                    "Sao chép",
                )
                .clicked()
                {
                    ui.ctx().copy_text(endpoint.to_string());
                    state.notify_ok(format!("Đã sao chép {endpoint}"));
                }
            });
        });
    });
    ui.add_space(5.0);
}
