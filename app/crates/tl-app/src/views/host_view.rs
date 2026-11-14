use crate::state::AppState;
use crate::theme::{
    accent_button, card_frame, danger_button, primary_button, section_heading, status_pill,
    subcard_frame, ACCENT_AMBER, ACCENT_CYAN, ACCENT_EMERALD, ACCENT_RED, TEXT_MUTED, TEXT_PRIMARY,
};
use egui::RichText;
use tl_runtime::TunnelStatus;

pub fn render(state: &mut AppState, ui: &mut egui::Ui) {
    ui.label(
        RichText::new("Quản trị Host Chơi Online")
            .size(21.0)
            .strong()
            .color(TEXT_PRIMARY),
    );
    ui.add_space(8.0);

    let port = state.active_server_port();

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            section_heading(ui, "Địa chỉ kết nối cho người chơi");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!state.network_loading, egui::Button::new("Làm mới mạng"))
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
            endpoint_row(ui, "Mạng LAN", &ep, ACCENT_CYAN, state);
        }

        if let Some(pub_ip) = state.network_snapshot.public_ip {
            let ep = format!("{pub_ip}:{port}");
            endpoint_row(ui, "IP Công cộng", &ep, ACCENT_EMERALD, state);
        } else if let Some(err) = &state.network_snapshot.public_ip_error {
            subcard_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(format!("IP Công cộng: {err}")).color(TEXT_MUTED));
            });
            ui.add_space(4.0);
        }

        if let Some(tunnel_ep) = state.active_tunnel_endpoint() {
            endpoint_row(ui, "Tunnel Online", &tunnel_ep, ACCENT_AMBER, state);
        }
    });

    ui.add_space(12.0);

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        section_heading(ui, &format!("Mở cổng & Chuyển tiếp (Port {port})"));
        ui.add_space(8.0);

        subcard_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Windows Firewall:").strong());
                match state.network_snapshot.firewall_rule_active {
                    Some(true) => status_pill(ui, "Đã mở", ACCENT_EMERALD),
                    Some(false) => status_pill(ui, "Chưa mở", ACCENT_AMBER),
                    None => status_pill(ui, "Chưa kiểm tra", TEXT_MUTED),
                }
                ui.checkbox(&mut state.config.auto_firewall, "Tự mở khi bật Server");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Gỡ quy tắc").clicked() {
                        state.remove_firewall(port);
                    }
                    if accent_button(ui, "Mở Firewall TCP", ACCENT_EMERALD).clicked() {
                        state.ensure_firewall(port);
                    }
                });
            });
        });
        ui.add_space(6.0);

        subcard_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("UPnP Router:").strong());
                if let Some(lease) = &state.upnp_lease {
                    status_pill(
                        ui,
                        &format!("Đang mở cổng {}", lease.port),
                        ACCENT_EMERALD,
                    );
                    if lease.is_cgnat {
                        status_pill(ui, "Phát hiện CGNAT — hãy dùng Tunnel", ACCENT_AMBER);
                    }
                    if let Some(ext) = lease.external_ip {
                        ui.label(
                            RichText::new(format!("WAN IP: {ext}"))
                                .monospace()
                                .color(TEXT_MUTED),
                        );
                    }
                } else {
                    status_pill(ui, "Chưa mở", TEXT_MUTED);
                }
                ui.checkbox(&mut state.config.auto_upnp, "Tự mở khi bật Server");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if state.upnp_lease.is_some() {
                        if danger_button(ui, "Đóng UPnP").clicked() {
                            state.close_upnp();
                        }
                    } else if accent_button(ui, "Mở UPnP Router", ACCENT_CYAN).clicked() {
                        state.open_upnp(port);
                    }
                });
            });
        });
        ui.add_space(6.0);

        subcard_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let tunnel_state = state.active_tunnel_status();
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Public TCP Tunnel:").strong());
                match &tunnel_state {
                    TunnelStatus::Idle | TunnelStatus::Stopped => {
                        status_pill(ui, "Đang tắt", TEXT_MUTED);
                    }
                    TunnelStatus::Connecting => {
                        status_pill(ui, "Đang kết nối...", ACCENT_CYAN);
                        ui.spinner();
                    }
                    TunnelStatus::Running {
                        remote_host,
                        remote_port,
                    } => {
                        status_pill(
                            ui,
                            &format!("Hoạt động: {remote_host}:{remote_port}"),
                            ACCENT_EMERALD,
                        );
                    }
                    TunnelStatus::Error(err) => {
                        status_pill(ui, err, ACCENT_RED);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if state.bore_handle.is_some() {
                        if danger_button(ui, "Tắt Tunnel").clicked() {
                            state.toggle_bore_tunnel();
                        }
                    } else if primary_button(ui, "Bật Tunnel công khai").clicked() {
                        state.toggle_bore_tunnel();
                    }
                });
            });
        });

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Kiểm tra TCP thực tế:").color(TEXT_MUTED));
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
                .add_enabled(!state.port_probe_busy, egui::Button::new("Kiểm tra cổng"))
                .clicked()
            {
                state.probe_port();
            }
            if state.port_probe_busy {
                ui.spinner();
            }
            if let Some((ok, msg)) = &state.port_probe_status {
                status_pill(ui, msg, if *ok { ACCENT_EMERALD } else { ACCENT_RED });
            }
        });
    });

    ui.add_space(12.0);

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        section_heading(ui, "Quản trị Người chơi & Banlist");
        ui.add_space(8.0);

        let players = state
            .server_snapshot
            .as_ref()
            .map(|s| s.players.clone())
            .unwrap_or_default();

        if players.is_empty() {
            ui.label(
                RichText::new("Hiện chưa có người chơi nào kết nối vào server.")
                    .color(TEXT_MUTED),
            );
        } else {
            let mut kick_target: Option<String> = None;
            let mut ban_target: Option<String> = None;
            for p in &players {
                subcard_frame().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&p.name).strong());
                        if let Some(ip) = &p.ip {
                            ui.label(RichText::new(ip).monospace().color(TEXT_MUTED));
                        }
                        ui.label(
                            RichText::new(format!("Vào lúc {}", p.joined_at))
                                .size(11.5)
                                .color(TEXT_MUTED),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if danger_button(ui, "Ban").clicked() {
                                ban_target = Some(p.name.clone());
                            }
                            if accent_button(ui, "Kick", ACCENT_AMBER).clicked() {
                                kick_target = Some(p.name.clone());
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
            if danger_button(ui, "Thêm Ban").clicked() {
                state.add_manual_ban();
            }
            if ui.button("Tải lại").clicked() {
                state.refresh_banlist();
            }
        });

        ui.add_space(6.0);
        if state.banlist_entries.is_empty() {
            ui.label(RichText::new("Danh sách cấm trống.").color(TEXT_MUTED));
        } else {
            let entries = state.banlist_entries.clone();
            let mut unban_idx: Option<usize> = None;
            for (idx, entry) in entries.iter().enumerate() {
                subcard_frame().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&entry.label).strong());
                        ui.label(RichText::new(&entry.ip).monospace().color(TEXT_MUTED));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if accent_button(ui, "Gỡ cấm", ACCENT_EMERALD).clicked() {
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
    label: &str,
    endpoint: &str,
    color: egui::Color32,
    state: &mut AppState,
) {
    subcard_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            status_pill(ui, label, color);
            ui.label(RichText::new(endpoint).monospace().strong().size(14.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if accent_button(ui, "Sao chép", color).clicked() {
                    ui.ctx().copy_text(endpoint.to_string());
                    state.notify_ok(format!("Đã sao chép {endpoint}"));
                }
            });
        });
    });
    ui.add_space(5.0);
}
