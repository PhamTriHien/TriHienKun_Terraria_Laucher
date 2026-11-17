use super::play_view::parse_pasted_host_port;
use crate::assets::{ButtonSkin, UiIcon};
use crate::state::AppState;
use crate::theme::{
    rpg_button, rpg_card, rpg_compact_button, rpg_section_heading, rpg_subcard, status_pill,
    ThemePalette,
};
use egui::RichText;
use tl_runtime::{
    format_p2p_ticket_short, is_p2p_ticket, DirectHostStatus, ExternalReachability,
    Ipv6PinholeStatus, P2pHostStatus, ServerLifecycleState, UpnpPortStatus,
};

pub(super) fn render_network_panel(state: &mut AppState, ui: &mut egui::Ui, p: ThemePalette) {
    let assets = state.assets.clone();
    let port = state.active_server_port();
    let srv_state = state.active_server_snapshot().map(|s| s.state.clone());
    let srv_active = state
        .active_server_supervisor()
        .is_some_and(|s| s.is_active());
    let srv_stopping_sup = state
        .active_server_supervisor()
        .is_some_and(|s| s.is_stopping());
    let has_ext = state.showing_external_server() && state.active_external_server().is_some();
    let ext_starting = state.is_active_external_starting();
    let ext_ready = has_ext && !ext_starting;
    let srv_running = matches!(srv_state, Some(ServerLifecycleState::Running)) || ext_ready;
    let srv_stopping =
        matches!(srv_state, Some(ServerLifecycleState::Stopping)) || srv_stopping_sup;
    let server_ready = srv_running || state.network_snapshot.local_port_listening;
    let server_unstarted = !srv_active && !has_ext && !server_ready;
    let pending_auto = state.is_active_profile_pending_direct_host();

    let direct_status = state.active_direct_host_status();
    let direct_running = matches!(direct_status, DirectHostStatus::Running(_));
    let direct_busy = matches!(
        direct_status,
        DirectHostStatus::Running(_) | DirectHostStatus::Starting { .. }
    ) || pending_auto;
    let missing_reason = match state.config.active_server() {
        Some(active_profile) => {
            super::server_view::server_edition_missing_reason(state, active_profile.edition)
        }
        None => Some("Chưa có hồ sơ server nào."),
    };

    let effective_fw_port = state
        .direct_host_port_input
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|&pt| pt > 0)
        .unwrap_or(port);

    let mut copy_target: Option<String> = None;
    let mut ask_refresh_net = false;
    let mut ask_toggle_direct = false;
    let mut ask_start_self_host = false;

    rpg_card(ui, assets.as_ref(), p, |ui| {
        ui.horizontal(|ui| {
            rpg_section_heading(
                ui,
                assets.as_ref(),
                p,
                UiIcon::Host,
                "Mở Port & Địa chỉ Kết nối",
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
                    ask_refresh_net = true;
                }
                if state.network_loading {
                    ui.spinner();
                }
            });
        });

        ui.add_space(8.0);

        if let DirectHostStatus::Running(info) = &direct_status {
            if info.active_clients > 0 {
                ui.horizontal_wrapped(|ui| {
                    status_pill(
                        ui,
                        &format!("Đang kết nối: {}", info.active_clients),
                        p.accent_cyan,
                    );
                });
                ui.add_space(4.0);
            }

            match &info.p2p_status {
                P2pHostStatus::Running { ticket, .. } => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Cổng Ảo P2P:").strong());
                        status_pill(ui, &format_p2p_ticket_short(ticket), p.accent_emerald);
                        status_pill(ui, "Xuyên mọi mạng", p.accent_emerald);
                        if rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Emerald,
                            Some(UiIcon::Copy),
                            "Sao chép",
                        )
                        .clicked()
                        {
                            copy_target = Some(ticket.clone());
                        }
                    });
                    ui.label(
                        RichText::new(
                            "Bạn bè dán mã này vào 'Tham gia Server nhanh' để vào thẳng không phụ thuộc Modem hay nhà mạng.",
                        )
                        .size(11.5)
                        .color(p.text_muted),
                    );
                }
                P2pHostStatus::Starting => {
                    ui.horizontal_wrapped(|ui| {
                        ui.spinner();
                        ui.label("Đang khởi tạo Cổng Ảo P2P...");
                    });
                }
                P2pHostStatus::Error(err) => {
                    ui.label(
                        RichText::new(format!("Cổng Ảo P2P chưa kết nối được: {err}"))
                            .size(11.8)
                            .color(p.accent_amber),
                    );
                }
                P2pHostStatus::Disabled => {}
            }

            for (idx, (nic_name, endpoint)) in info.lan_endpoints.iter().enumerate() {
                ui.push_id(("direct_lan_ep", idx), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(format!("Cùng mạng LAN ({nic_name}):")).strong());
                        status_pill(ui, endpoint, p.accent_cyan);
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
                            copy_target = Some(endpoint.clone());
                        }
                    });
                });
            }

            for (idx, (nic_name, endpoint)) in info.vpn_endpoints.iter().enumerate() {
                ui.push_id(("direct_vpn_ep", idx), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(format!("VPN ({nic_name}):")).strong());
                        status_pill(ui, endpoint, p.accent_emerald);
                        if rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Emerald,
                            Some(UiIcon::Copy),
                            "Sao chép",
                        )
                        .clicked()
                        {
                            copy_target = Some(endpoint.clone());
                        }
                    });
                });
            }

            if let Some(v6_ep) = &info.ipv6_endpoint {
                match (&info.ipv6_reachability, &info.ipv6_pinhole_status) {
                    (ExternalReachability::Reachable, _) => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Online IPv6:").strong());
                            status_pill(ui, v6_ep, p.accent_emerald);
                            status_pill(ui, "Đã thông Internet", p.accent_emerald);
                            if rpg_compact_button(
                                ui,
                                assets.as_ref(),
                                p,
                                ButtonSkin::Emerald,
                                Some(UiIcon::Copy),
                                "Sao chép",
                            )
                            .clicked()
                            {
                                copy_target = Some(v6_ep.clone());
                            }
                        });
                        ui.label(
                            RichText::new(
                                "Bạn bè có mạng IPv6 dán địa chỉ này vào 'Tham gia Server nhanh' trên Launcher.",
                            )
                            .size(11.5)
                            .color(p.text_muted),
                        );
                    }
                    (ExternalReachability::Checking, _) | (_, Ipv6PinholeStatus::Opening) => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Online IPv6:").strong());
                            status_pill(ui, v6_ep, p.accent_cyan);
                            ui.spinner();
                            ui.label(
                                RichText::new("Đang mở tường lửa Modem & kiểm tra kết nối...")
                                    .size(11.8)
                                    .color(p.text_muted),
                            );
                        });
                    }
                    (ExternalReachability::Blocked, _) => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("IPv6 (Bị Modem chặn):").strong());
                            status_pill(ui, v6_ep, p.accent_amber);
                            if rpg_compact_button(
                                ui,
                                assets.as_ref(),
                                p,
                                ButtonSkin::Slate,
                                Some(UiIcon::Copy),
                                "Sao chép",
                            )
                            .clicked()
                            {
                                copy_target = Some(v6_ep.clone());
                            }
                        });
                        ui.label(
                            RichText::new(format!(
                                "Đã mở trên máy nhưng tường lửa IPv6 của Modem đang chặn chiều vào — hãy cho phép TCP {} trong IPv6 Firewall của Modem hoặc dùng Cổng Ảo P2P / VPN.",
                                info.host_port
                            ))
                            .size(11.5)
                            .color(p.text_muted),
                        );
                    }
                    (
                        ExternalReachability::CheckUnavailable(_)
                        | ExternalReachability::NotChecked,
                        _,
                    ) => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("IPv6:").strong());
                            status_pill(ui, v6_ep, p.accent_amber);
                            status_pill(ui, "Chưa xác nhận qua Modem", p.accent_amber);
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
                                copy_target = Some(v6_ep.clone());
                            }
                        });
                    }
                }
            }

            match &info.upnp_status {
                UpnpPortStatus::Mapped {
                    external_ip,
                    external_port,
                    ..
                } => {
                    let v4_ep = format!("{external_ip}:{external_port}");
                    ui.horizontal_wrapped(|ui| match &info.ipv4_reachability {
                        ExternalReachability::Reachable => {
                            ui.label(RichText::new("Online IPv4:").strong());
                            status_pill(ui, &v4_ep, p.accent_emerald);
                            status_pill(ui, "Đã thông Internet", p.accent_emerald);
                            if rpg_compact_button(
                                ui,
                                assets.as_ref(),
                                p,
                                ButtonSkin::Emerald,
                                Some(UiIcon::Copy),
                                "Sao chép",
                            )
                            .clicked()
                            {
                                copy_target = Some(v4_ep);
                            }
                        }
                        ExternalReachability::Checking => {
                            ui.label(RichText::new("Online IPv4:").strong());
                            status_pill(ui, &v4_ep, p.accent_cyan);
                            ui.spinner();
                            ui.label(
                                RichText::new("Đang kiểm tra kết nối...")
                                    .size(11.8)
                                    .color(p.text_muted),
                            );
                        }
                        ExternalReachability::Blocked => {
                            ui.label(RichText::new("IPv4 (Bị Modem chặn):").strong());
                            status_pill(ui, &v4_ep, p.accent_amber);
                            status_pill(ui, "Chưa thông Internet", p.accent_amber);
                            if rpg_compact_button(
                                ui,
                                assets.as_ref(),
                                p,
                                ButtonSkin::Slate,
                                Some(UiIcon::Copy),
                                "Sao chép",
                            )
                            .clicked()
                            {
                                copy_target = Some(v4_ep);
                            }
                        }
                        ExternalReachability::CheckUnavailable(_)
                        | ExternalReachability::NotChecked => {
                            ui.label(RichText::new("Online IPv4:").strong());
                            status_pill(ui, &v4_ep, p.accent_amber);
                            status_pill(ui, "Chưa xác nhận qua Modem", p.accent_amber);
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
                                copy_target = Some(v4_ep);
                            }
                        }
                    });
                }
                UpnpPortStatus::Mapping => {
                    ui.horizontal_wrapped(|ui| {
                        ui.spinner();
                        ui.label("Đang mở cổng IPv4 trên Modem...");
                    });
                }
                UpnpPortStatus::CarrierGradeNat {
                    wan_ip, public_ip, ..
                } => {
                    let msg = match public_ip {
                        Some(pub_ip) if pub_ip != wan_ip => format!(
                            "Nhà mạng đang dùng CGNAT (WAN Modem {wan_ip} ≠ IP thực {pub_ip}) — dùng Cổng Ảo P2P ở trên, IPv6 hoặc VPN."
                        ),
                        _ => format!(
                            "Nhà mạng đang dùng CGNAT ({wan_ip}) — dùng Cổng Ảo P2P ở trên, IPv6 hoặc VPN."
                        ),
                    };
                    ui.label(RichText::new(msg).size(11.8).color(p.text_muted));
                }
                UpnpPortStatus::Unavailable(_) => {
                    let msg = if matches!(info.p2p_status, P2pHostStatus::Running { .. })
                        || info.ipv6_reachability == ExternalReachability::Reachable
                    {
                        "UPnP IPv4 không khả dụng — dùng Cổng Ảo P2P / kênh Online ở trên."
                            .to_string()
                    } else {
                        format!(
                            "Modem chặn mở cổng IPv4 tự động — hãy bật Cổng Ảo P2P, dùng IPv6 / VPN chung mạng hoặc mở Port Forwarding TCP {} trên Modem.",
                            info.host_port
                        )
                    };
                    ui.label(RichText::new(msg).size(11.8).color(p.text_muted));
                }
                UpnpPortStatus::Disabled => {}
            }
        } else {
            let vpn_ifaces: Vec<(String, std::net::Ipv4Addr)> = state
                .network_snapshot
                .interfaces
                .iter()
                .filter(|iface| {
                    iface.is_vpn && Some(iface.ipv4) != state.network_snapshot.primary_lan_ip
                })
                .map(|iface| (iface.name.clone(), iface.ipv4))
                .collect();
            let has_preview = state.network_snapshot.primary_lan_ip.is_some()
                || !vpn_ifaces.is_empty()
                || state.network_snapshot.global_ipv6.is_some();

            if has_preview {
                ui.horizontal_wrapped(|ui| {
                    if let Some(lan) = state.network_snapshot.primary_lan_ip {
                        let lan_ep = format!("{lan}:{effective_fw_port}");
                        status_pill(ui, &format!("LAN: {lan_ep}"), p.accent_cyan);
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
                            copy_target = Some(lan_ep);
                        }
                    }

                    for (idx, (iface_name, ipv4)) in vpn_ifaces.iter().enumerate() {
                        ui.push_id(("vpn_iface_pill", idx), |ui| {
                            let vpn_ep = format!("{ipv4}:{effective_fw_port}");
                            status_pill(ui, &format!("VPN ({iface_name}): {vpn_ep}"), p.accent_cyan);
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
                                copy_target = Some(vpn_ep);
                            }
                        });
                    }

                    if let Some(v6) = state.network_snapshot.global_ipv6 {
                        let v6_ep = format!("[{v6}]:{effective_fw_port}");
                        status_pill(ui, &format!("IPv6: {v6_ep}"), p.accent_cyan);
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
                            copy_target = Some(v6_ep);
                        }
                    }
                });
                ui.add_space(2.0);
            }

            ui.label(
                RichText::new(
                    "Nhấn 'Mở Online' ở trên hoặc 'Mở Port Online' bên dưới để đón người chơi.",
                )
                .size(12.0)
                .color(p.text_muted),
            );
        }

        if !direct_running {
            ui.add_space(8.0);
            ui.add_enabled_ui(!direct_busy, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Mạng:").color(p.text_muted));
                    let prev_bind_ip = state.active_direct_bind_ip();
                    let mut bind_ip = prev_bind_ip;
                    let selected_nic_text = match bind_ip {
                        None => "Tất cả mạng đang kết nối".to_string(),
                        Some(ip) => state
                            .network_snapshot
                            .interfaces
                            .iter()
                            .find(|iface| iface.ipv4 == ip)
                            .map(|iface| format!("{} ({})", iface.name, iface.ipv4))
                            .unwrap_or_else(|| ip.to_string()),
                    };
                    let selectable_ifaces: Vec<(std::net::Ipv4Addr, String)> = state
                        .network_snapshot
                        .interfaces
                        .iter()
                        .filter(|iface| !iface.is_hypervisor)
                        .map(|iface| (iface.ipv4, format!("{} ({})", iface.name, iface.ipv4)))
                        .collect();

                    egui::ComboBox::from_id_salt("host_direct_nic")
                        .selected_text(selected_nic_text)
                        .width(210.0)
                        .truncate()
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut bind_ip,
                                None,
                                "Tất cả mạng đang kết nối",
                            );
                            for (ipv4, label) in &selectable_ifaces {
                                ui.selectable_value(&mut bind_ip, Some(*ipv4), label);
                            }
                        });
                    if bind_ip != prev_bind_ip {
                        state.set_active_direct_bind_ip(bind_ip);
                    }

                    ui.label(RichText::new("Cổng mở:").color(p.text_muted));
                    let host_port_resp = ui.add(
                        egui::TextEdit::singleline(&mut state.direct_host_port_input)
                            .desired_width(64.0)
                            .hint_text(port.to_string()),
                    );
                    if host_port_resp.changed() {
                        if !direct_busy {
                            if let Some(valid_port) = state
                                .direct_host_port_input
                                .trim()
                                .parse::<u16>()
                                .ok()
                                .filter(|&pt| pt > 0)
                            {
                                state.port_probe_port = valid_port.to_string();
                            }
                        }
                        ask_refresh_net = true;
                    } else if host_port_resp.lost_focus() {
                        ask_refresh_net = true;
                    }

                    if ui
                        .checkbox(
                            &mut state.direct_try_upnp,
                            "Tự động mở cổng UPnP trên Modem",
                        )
                        .changed()
                    {
                        if let Some(prof) = state.config.active_server_mut() {
                            prof.config.upnp = state.direct_try_upnp;
                        }
                        state.save_config();
                    }
                    if ui
                        .checkbox(&mut state.direct_enable_p2p, "Cổng Ảo P2P xuyên mạng")
                        .changed()
                    {
                        if let Some(prof) = state.config.active_server_mut() {
                            prof.p2p_tunnel = state.direct_enable_p2p;
                        }
                        state.save_config();
                    }
                });
            });
        }

        ui.add_space(8.0);
        match &direct_status {
            DirectHostStatus::Idle | DirectHostStatus::Stopped | DirectHostStatus::Error(_) => {
                if let DirectHostStatus::Error(err) = &direct_status {
                    ui.label(RichText::new(err).color(p.accent_red));
                    ui.add_space(4.0);
                }

                ui.horizontal_wrapped(|ui| {
                    if pending_auto {
                        ui.spinner();
                        ui.label(
                            RichText::new("Đang đợi Server khởi động xong để mở Port...")
                                .color(p.accent_cyan),
                        );
                        if rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Red,
                            Some(UiIcon::Stop),
                            "Hủy",
                        )
                        .clicked()
                        {
                            let active_profile_id = state.config.active_server_id.clone();
                            state.cancel_pending_auto_direct_host(&active_profile_id);
                        }
                    } else if server_unstarted {
                        let can_launch = !state.install_busy && port > 0 && missing_reason.is_none();
                        let launch_hint = if state.install_busy {
                            "Đang cài đặt thành phần..."
                        } else {
                            missing_reason.unwrap_or("Cổng server phải lớn hơn 0")
                        };
                        if ui
                            .add_enabled_ui(can_launch, |ui| {
                                rpg_button(
                                    ui,
                                    assets.as_ref(),
                                    p,
                                    ButtonSkin::Emerald,
                                    UiIcon::Host,
                                    "Khởi chạy & Mở Online",
                                )
                            })
                            .inner
                            .on_disabled_hover_text(launch_hint)
                            .clicked()
                        {
                            ask_start_self_host = true;
                        }
                    } else if ui
                        .add_enabled_ui(server_ready && !srv_stopping, |ui| {
                            rpg_button(
                                ui,
                                assets.as_ref(),
                                p,
                                ButtonSkin::Emerald,
                                UiIcon::Host,
                                "Mở Port Online",
                            )
                        })
                        .inner
                        .on_disabled_hover_text("Chờ server khởi động xong trước khi mở cổng")
                        .clicked()
                    {
                        ask_toggle_direct = true;
                    }
                });
            }
            DirectHostStatus::Starting { host_port, .. } => {
                ui.horizontal_wrapped(|ui| {
                    ui.spinner();
                    ui.label(format!("Đang mở cổng {host_port}..."));
                    if rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Red,
                        Some(UiIcon::Stop),
                        "Hủy",
                    )
                    .clicked()
                    {
                        ask_toggle_direct = true;
                    }
                });
            }
            DirectHostStatus::Running(_) => {
                ui.horizontal_wrapped(|ui| {
                    if rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Amber,
                        UiIcon::Stop,
                        "Đóng Port Online",
                    )
                    .clicked()
                    {
                        ask_toggle_direct = true;
                    }
                });
            }
        }
    });

    if ask_refresh_net {
        state.refresh_network_and_recheck_online();
    }
    if ask_start_self_host {
        state.start_self_host_online();
    }
    if ask_toggle_direct {
        state.toggle_direct_host();
    }
    if let Some(txt) = copy_target {
        ui.ctx().copy_text(txt.clone());
        let msg = if is_p2p_ticket(&txt) {
            "Đã sao chép mã Cổng Ảo P2P".to_string()
        } else {
            format!("Đã sao chép {txt}")
        };
        state.notify_ok(msg);
    }

    ui.add_space(12.0);

    rpg_card(ui, assets.as_ref(), p, |ui| {
        rpg_section_heading(
            ui,
            assets.as_ref(),
            p,
            UiIcon::Settings,
            "Tường lửa & Kiểm tra cổng",
        );
        ui.add_space(8.0);

        rpg_subcard(ui, assets.as_ref(), p, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("Windows Firewall (TCP {effective_fw_port}):")).strong(),
                );
                match state.network_snapshot.firewall_rule_active {
                    Some(true) => status_pill(ui, "Đã mở", p.accent_emerald),
                    Some(false) => status_pill(ui, "Chưa mở", p.accent_amber),
                    None => status_pill(ui, "Chưa kiểm tra", p.text_muted),
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(!state.firewall_busy && effective_fw_port > 0, |ui| {
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
                            state.remove_firewall(effective_fw_port);
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
                            state.ensure_firewall(effective_fw_port);
                        }
                    });
                    if state.firewall_busy {
                        ui.spinner();
                    }
                });
            });
        });

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Kiểm tra cổng:").color(p.text_muted));
            let pasted =
                ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))));
            let probe_host_resp = ui.add(
                egui::TextEdit::singleline(&mut state.port_probe_host)
                    .desired_width(170.0)
                    .hint_text("IP, Domain hoặc TLPORT:..."),
            );
            let probe_is_p2p = is_p2p_ticket(&state.port_probe_host);
            let parsed_probe = (!probe_is_p2p
                && ((probe_host_resp.changed() && pasted) || probe_host_resp.lost_focus()))
                .then(|| parse_pasted_host_port(&state.port_probe_host))
                .flatten();
            if let Some((host, parsed_port)) = parsed_probe {
                state.port_probe_host = host;
                state.port_probe_port = parsed_port.to_string();
            }
            ui.add_enabled(
                !probe_is_p2p,
                egui::TextEdit::singleline(&mut state.port_probe_port)
                    .desired_width(64.0)
                    .hint_text("7777"),
            );
            let probe_port_valid = state
                .port_probe_port
                .trim()
                .parse::<u16>()
                .ok()
                .is_some_and(|pt| pt > 0);
            let probe_ready = !state.port_probe_busy
                && !state.port_probe_host.trim().is_empty()
                && (probe_is_p2p || probe_port_valid);
            if ui
                .add_enabled_ui(probe_ready, |ui| {
                    rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Cyan,
                        Some(UiIcon::Refresh),
                        "Thử",
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
}

pub(super) fn render_players_and_banlist_card(
    state: &mut AppState,
    ui: &mut egui::Ui,
    p: ThemePalette,
) {
    let assets = state.assets.clone();
    let is_ext_readonly = state.showing_external_server();
    let can_refresh_players = !is_ext_readonly
        && state
            .active_server_supervisor()
            .is_some_and(|s| s.is_active() && s.has_stdin())
        && matches!(
            state.active_server_snapshot().map(|s| &s.state),
            Some(ServerLifecycleState::Running)
        );

    rpg_card(ui, assets.as_ref(), p, |ui| {
        ui.horizontal(|ui| {
            rpg_section_heading(
                ui,
                assets.as_ref(),
                p,
                UiIcon::Server,
                "Người chơi đang kết nối & Danh sách cấm",
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled_ui(can_refresh_players, |ui| {
                        rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Slate,
                            Some(UiIcon::Refresh),
                            "Làm mới",
                        )
                    })
                    .inner
                    .clicked()
                {
                    state.refresh_online_players();
                }
            });
        });
        ui.add_space(8.0);

        let players = if is_ext_readonly {
            state
                .active_external_log_summary()
                .map(|s| s.players.clone())
                .unwrap_or_default()
        } else {
            state
                .active_server_snapshot()
                .map(|s| s.players.clone())
                .unwrap_or_default()
        };

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
                            let display_ip = state.format_player_ip_for_active(ip);
                            ui.label(RichText::new(&display_ip).monospace().color(p.text_muted));
                        }
                        ui.label(
                            RichText::new(format!("Vào lúc {}", player.joined_at))
                                .size(11.5)
                                .color(p.text_muted),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            for (skin, icon, label, target) in [
                                (ButtonSkin::Red, UiIcon::Trash, "Ban", &mut ban_target),
                                (ButtonSkin::Amber, UiIcon::Stop, "Kick", &mut kick_target),
                            ] {
                                if ui
                                    .add_enabled_ui(!is_ext_readonly, |ui| {
                                        rpg_compact_button(
                                            ui,
                                            assets.as_ref(),
                                            p,
                                            skin,
                                            Some(icon),
                                            label,
                                        )
                                    })
                                    .inner
                                    .on_disabled_hover_text("Server phiên trước chỉ hỗ trợ xem log")
                                    .clicked()
                                {
                                    *target = Some(player.name.clone());
                                }
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

        let banlist_writable = !is_ext_readonly
            || state
                .active_external_server()
                .and_then(|e| e.profile_id.as_ref())
                .is_some();
        let standalone_ban_hint =
            "Server độc lập bên ngoài không gắn với hồ sơ banlist của Launcher";

        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Cấm địa chỉ IP:").strong());
            ui.add(
                egui::TextEdit::singleline(&mut state.manual_ban_name)
                    .desired_width(130.0)
                    .hint_text("Tên / Ghi chú"),
            );
            let ip_resp = ui.add(
                egui::TextEdit::singleline(&mut state.manual_ban_ip)
                    .desired_width(140.0)
                    .hint_text("IP hoặc p2p:..."),
            );
            let can_ban = !state.manual_ban_ip.trim().is_empty();
            let ban_enabled = banlist_writable && can_ban;
            let ban_hint = if !banlist_writable {
                standalone_ban_hint
            } else {
                "Nhập địa chỉ IP cần cấm trước"
            };
            let enter_ban =
                ban_enabled && ip_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let clicked_ban = ui
                .add_enabled_ui(ban_enabled, |ui| {
                    rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Red,
                        Some(UiIcon::Trash),
                        "Thêm Ban",
                    )
                })
                .inner
                .on_disabled_hover_text(ban_hint)
                .clicked();
            if clicked_ban || enter_ban {
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
                            if ui
                                .add_enabled_ui(banlist_writable, |ui| {
                                    rpg_compact_button(
                                        ui,
                                        assets.as_ref(),
                                        p,
                                        ButtonSkin::Emerald,
                                        Some(UiIcon::Key),
                                        "Gỡ cấm",
                                    )
                                })
                                .inner
                                .on_disabled_hover_text(standalone_ban_hint)
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
