use crate::anim3d;
use crate::assets::{ButtonSkin, UiIcon};
use crate::state::AppState;
use crate::theme::{
    console_frame, page_title, rpg_button, rpg_card, rpg_compact_button, rpg_section_heading,
    rpg_selectable_button, status_pill, ThemePalette,
};
use egui::RichText;
use tl_core::{GameEdition, WorldDifficultyOption, WorldSizeOption};
use tl_runtime::ServerLifecycleState;

pub fn render(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    let server_running = matches!(
        state.server_snapshot.as_ref().map(|s| &s.state),
        Some(ServerLifecycleState::Running)
    );
    let server_starting = matches!(
        state.server_snapshot.as_ref().map(|s| &s.state),
        Some(ServerLifecycleState::Starting)
    );

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
            page_title(ui, p, "Quản lý Server Game");
        });
    });
    ui.add_space(8.0);

    rpg_card(ui, assets.as_ref(), p, |ui| {
        rpg_section_heading(ui, assets.as_ref(), p, UiIcon::Pack, "Hồ sơ Server");
        ui.add_space(6.0);

        ui.horizontal_wrapped(|ui| {
            let mut selected_profile_id = None;
            for profile in &state.config.servers {
                let active = state.config.active_server_id == profile.id;
                let label = format!("{} ({})", profile.name, profile.edition.label());
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
                state.config.active_server_id = id;
                state.refresh_banlist();
            }
        });

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.new_profile_name)
                    .desired_width(150.0)
                    .hint_text("Tên hồ sơ mới"),
            );
            egui::ComboBox::from_id_salt("srv_new_edition")
                .selected_text(state.new_profile_edition.label())
                .width(145.0)
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
            if rpg_button(
                ui,
                assets.as_ref(),
                p,
                ButtonSkin::Cyan,
                UiIcon::Pack,
                "+ Thêm",
            )
            .clicked()
            {
                state.add_server_profile();
            }
            if state.config.servers.len() > 1
                && rpg_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Red,
                    UiIcon::Trash,
                    "Xóa hồ sơ",
                )
                .clicked()
            {
                state.delete_active_server_profile();
            }
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);

        let is_active = state
            .server_supervisor
            .as_ref()
            .map(|s| s.is_active())
            .unwrap_or(false);

        ui.horizontal_wrapped(|ui| {
            let status_color = state.badge_color_for_server();
            status_pill(ui, state.server_status_short(), status_color);

            if let Some(snap) = &state.server_snapshot {
                ui.label(
                    RichText::new(&snap.status_detail)
                        .size(12.5)
                        .color(p.text_muted),
                );
                if let Some(pid) = snap.pid {
                    status_pill(
                        ui,
                        &format!(
                            "PID {} • {} MB • {:.1}% CPU • {}/{} người",
                            pid,
                            snap.memory_mb,
                            snap.cpu_percent,
                            snap.players.len(),
                            state
                                .config
                                .active_server()
                                .map(|s| s.config.maxplayers)
                                .unwrap_or(8)
                        ),
                        p.accent_cyan,
                    );
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if is_active {
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
                    if rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Amber,
                        UiIcon::Stop,
                        "Lưu & Tắt",
                    )
                    .clicked()
                    {
                        state.stop_selected_server();
                    }
                } else {
                    if rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Amber,
                        UiIcon::Download,
                        "Lưu cấu hình",
                    )
                    .clicked()
                    {
                        state.save_config();
                        state.notify_ok("Đã lưu cấu hình server.");
                    }
                    if rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Emerald,
                        UiIcon::Play,
                        "Khởi chạy Server",
                    )
                    .clicked()
                    {
                        state.start_selected_server();
                    }
                }
            });
        });
    });

    ui.add_space(12.0);

    let wide = ui.available_width() >= 900.0;
    if wide {
        ui.columns(2, |cols| {
            render_config_editor(state, &mut cols[0], p);
            render_console_panel(state, &mut cols[1], p);
        });
    } else {
        render_config_editor(state, ui, p);
        ui.add_space(12.0);
        render_console_panel(state, ui, p);
    }
}

fn render_config_editor(state: &mut AppState, ui: &mut egui::Ui, p: ThemePalette) {
    let assets = state.assets.clone();
    let modpacks = state.modpacks.clone();
    let tml_worlds_dir = state.paths.tml_worlds_dir();
    let vanilla_worlds_dir = state.paths.vanilla_worlds_dir();

    let Some(profile) = state.config.active_server_mut() else {
        return;
    };

    rpg_card(ui, assets.as_ref(), p, |ui| {
        rpg_section_heading(
            ui,
            assets.as_ref(),
            p,
            UiIcon::Settings,
            "Cấu hình Server (serverconfig.txt)",
        );
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label(RichText::new("Tên hồ sơ:").color(p.text_muted));
            ui.add(egui::TextEdit::singleline(&mut profile.name).desired_width(200.0));
        });

        ui.horizontal(|ui| {
            ui.label(RichText::new("Tên thế giới:").color(p.text_muted));
            ui.add(egui::TextEdit::singleline(&mut profile.config.worldname).desired_width(180.0));
        });

        let edition_worlds_dir = match profile.edition {
            GameEdition::Vanilla => vanilla_worlds_dir,
            GameEdition::TModLoader => tml_worlds_dir,
        };
        let available_wlds: Vec<String> = std::fs::read_dir(&edition_worlds_dir)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|path| {
                path.extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("wld"))
                    .unwrap_or(false)
            })
            .map(|path| path.to_string_lossy().into_owned())
            .collect();

        ui.horizontal(|ui| {
            ui.label(RichText::new("File .wld:").color(p.text_muted));
            ui.add(egui::TextEdit::singleline(&mut profile.config.world).desired_width(240.0));
            if !available_wlds.is_empty() {
                egui::ComboBox::from_id_salt("srv_pick_wld")
                    .selected_text("Chọn sẵn")
                    .width(96.0)
                    .show_ui(ui, |ui| {
                        for path_str in &available_wlds {
                            let file_label = std::path::Path::new(path_str)
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path_str.clone());
                            if ui.selectable_label(false, file_label).clicked() {
                                profile.config.world = path_str.clone();
                                if let Some(stem) = std::path::Path::new(path_str).file_stem() {
                                    profile.config.worldname = stem.to_string_lossy().into_owned();
                                }
                            }
                        }
                    });
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Tạo mới nếu thiếu:").color(p.text_muted));
            let autocreate_label = match profile.config.autocreate {
                Some(1) => WorldSizeOption::Small.label(),
                Some(2) => WorldSizeOption::Medium.label(),
                Some(3) => WorldSizeOption::Large.label(),
                _ => "Không tự tạo",
            };
            egui::ComboBox::from_id_salt("srv_autocreate")
                .selected_text(autocreate_label)
                .width(150.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut profile.config.autocreate, None, "Không tự tạo");
                    ui.selectable_value(
                        &mut profile.config.autocreate,
                        Some(1),
                        WorldSizeOption::Small.label(),
                    );
                    ui.selectable_value(
                        &mut profile.config.autocreate,
                        Some(2),
                        WorldSizeOption::Medium.label(),
                    );
                    ui.selectable_value(
                        &mut profile.config.autocreate,
                        Some(3),
                        WorldSizeOption::Large.label(),
                    );
                });

            ui.label(RichText::new("Độ khó:").color(p.text_muted));
            let diff = WorldDifficultyOption::from_u8(profile.config.difficulty);
            egui::ComboBox::from_id_salt("srv_difficulty")
                .selected_text(diff.label())
                .width(100.0)
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
                            opt.label(),
                        );
                    }
                });
        });

        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Cổng TCP:").color(p.text_muted));
            let mut port_str = profile.config.port.to_string();
            if ui
                .add(egui::TextEdit::singleline(&mut port_str).desired_width(68.0))
                .changed()
            {
                if let Ok(port) = port_str.trim().parse::<u16>() {
                    profile.config.port = port;
                }
            }

            ui.label(RichText::new("Số người tối đa:").color(p.text_muted));
            let mut mp = profile.config.maxplayers as i32;
            if ui
                .add(egui::DragValue::new(&mut mp).range(1..=255))
                .changed()
            {
                profile.config.maxplayers = mp.clamp(1, 255) as u16;
            }
        });

        ui.horizontal(|ui| {
            ui.label(RichText::new("Mật khẩu:").color(p.text_muted));
            ui.add(
                egui::TextEdit::singleline(&mut profile.config.password)
                    .desired_width(170.0)
                    .hint_text("Để trống nếu công khai"),
            );
        });

        ui.horizontal(|ui| {
            ui.label(RichText::new("MOTD:").color(p.text_muted));
            ui.add(egui::TextEdit::singleline(&mut profile.config.motd).desired_width(250.0));
        });

        ui.horizontal(|ui| {
            ui.label(RichText::new("Seed:").color(p.text_muted));
            ui.add(
                egui::TextEdit::singleline(&mut profile.config.seed)
                    .desired_width(190.0)
                    .hint_text("Ngẫu nhiên nếu để trống"),
            );
        });

        if profile.edition == GameEdition::TModLoader {
            ui.horizontal(|ui| {
                ui.label(RichText::new("ModPack:").color(p.text_muted));
                let mp_label = if profile.config.modpack.is_empty() {
                    "Theo enabled.json chung".to_string()
                } else {
                    profile.config.modpack.clone()
                };
                egui::ComboBox::from_id_salt("srv_modpack")
                    .selected_text(mp_label)
                    .width(210.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut profile.config.modpack,
                            String::new(),
                            "Theo enabled.json chung",
                        );
                        for pack in &modpacks {
                            ui.selectable_value(
                                &mut profile.config.modpack,
                                pack.name.clone(),
                                &pack.name,
                            );
                        }
                    });
            });
        }

        if profile.edition == GameEdition::Vanilla {
            egui::CollapsingHeader::new("Tùy chọn đặc biệt Terraria 1.4.5")
                .default_open(false)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.checkbox(&mut profile.config.seed_celebration, "celebrationmk10");
                        ui.checkbox(&mut profile.config.seed_theconstant, "constant");
                        ui.checkbox(&mut profile.config.seed_notthebees, "not the bees");
                        ui.checkbox(&mut profile.config.seed_notraps, "no traps");
                        ui.checkbox(&mut profile.config.seed_fortheworthy, "for the worthy");
                        ui.checkbox(&mut profile.config.seed_remix, "don't dig up");
                        ui.checkbox(&mut profile.config.seed_drunk, "drunk world");
                        ui.checkbox(&mut profile.config.seed_zenith, "get fixed boi");
                    });
                    ui.checkbox(&mut profile.config.slowliquids, "Giảm tải xử lý chất lỏng");
                });
        }

        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut profile.config.secure, "Chống cheat (-secure)");
            ui.checkbox(&mut profile.config.upnp, "Bật UPnP trong server");
            ui.checkbox(&mut profile.auto_restart, "Tự khởi động lại khi crash");
        });

        ui.horizontal(|ui| {
            ui.label(RichText::new("Tham số thêm:").color(p.text_muted));
            ui.add(egui::TextEdit::singleline(&mut profile.extra_args).desired_width(220.0));
        });
    });
}

fn render_console_panel(state: &mut AppState, ui: &mut egui::Ui, p: ThemePalette) {
    let assets = state.assets.clone();
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
                    if let Some(sup) = &state.server_supervisor {
                        sup.clear_logs();
                    }
                }
                ui.checkbox(&mut state.console_autoscroll, "Cuộn tự động");
            });
        });

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            for (label, cmd, skin, icon) in [
                ("Người chơi", "playing", ButtonSkin::Cyan, UiIcon::Host),
                ("Lưu thế giới", "save", ButtonSkin::Amber, UiIcon::Download),
                ("Bình minh", "dawn", ButtonSkin::Slate, UiIcon::Sun),
                ("Trưa", "noon", ButtonSkin::Slate, UiIcon::Sun),
                ("Hoàng hôn", "dusk", ButtonSkin::Slate, UiIcon::Moon),
                ("Nửa đêm", "midnight", ButtonSkin::Slate, UiIcon::Moon),
                ("Settle", "settle", ButtonSkin::Slate, UiIcon::Refresh),
            ] {
                if rpg_compact_button(ui, assets.as_ref(), p, skin, Some(icon), label).clicked() {
                    state.send_server_command(cmd);
                }
            }
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
                    if let Some(sup) = &state.server_supervisor {
                        let logs = sup.logs();
                        if logs.is_empty() {
                            ui.label(
                                RichText::new("Chưa có dữ liệu log.")
                                    .monospace()
                                    .color(p.text_muted),
                            );
                        } else {
                            for line in logs {
                                let color = if line.is_error {
                                    p.accent_red
                                } else if line.text.starts_with('>') {
                                    p.accent_cyan
                                } else if line.text.contains("Listening on port")
                                    || line.text.contains("Server started")
                                {
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
                    } else {
                        ui.label(
                            RichText::new("Nhấn 'Khởi chạy Server' để xem log thực tế tại đây.")
                                .monospace()
                                .color(p.text_muted),
                        );
                    }
                });
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut state.console_input)
                    .desired_width((ui.available_width() - 116.0).max(120.0))
                    .hint_text("Nhập lệnh server (vd: say Xin chao!, save, kick Ten)..."),
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
}
