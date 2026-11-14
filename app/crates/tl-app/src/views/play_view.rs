use crate::anim3d;
use crate::state::{AppState, NavTab};
use crate::theme::{
    accent_button, card_frame, danger_button, format_bytes, primary_button, section_heading,
    status_pill, subcard_frame, ThemePalette, DARK_PALETTE, LIGHT_PALETTE,
};
use egui::RichText;
use tl_core::GameEdition;
use tl_runtime::{InstallProgress, ServerLifecycleState};

fn is_edition_ready(state: &AppState, edition: GameEdition) -> bool {
    match edition {
        GameEdition::Vanilla => state.env.terraria_installed && state.env.xna_installed,
        GameEdition::TModLoader => {
            state.env.terraria_installed
                && state.env.tmodloader_installed
                && state.env.dotnet8_version.is_some()
        }
    }
}

pub fn render(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();

    ui.label(
        RichText::new("Khởi chạy Game & Thế giới")
            .size(21.0)
            .strong()
            .color(p.text_primary),
    );
    ui.add_space(8.0);

    anim3d::render_play_hero_showcase(
        ui,
        p,
        state.assets.hero_banner(state.config.active_edition),
        state.assets.edition_icon(state.config.active_edition),
        state.config.active_edition,
        state.running_game.is_some(),
        state.installed_mods.len(),
    );
    ui.add_space(10.0);

    let active_edition_ready = is_edition_ready(state, state.config.active_edition);

    card_frame(p).show(ui, |ui| {
        ui.set_width(ui.available_width());
        section_heading(ui, p, "Phiên bản & Khởi động");
        ui.add_space(6.0);

        ui.horizontal_wrapped(|ui| {
            for edition in [GameEdition::TModLoader, GameEdition::Vanilla] {
                let active = state.config.active_edition == edition;
                let label = if active {
                    RichText::new(edition.label()).strong()
                } else {
                    RichText::new(edition.label())
                };
                let resp = if let Some(icon) = state.assets.edition_icon(edition) {
                    let icon_img = egui::Image::new((icon.id(), egui::vec2(18.0, 18.0)))
                        .fit_to_exact_size(egui::vec2(18.0, 18.0))
                        .corner_radius(4.0);
                    ui.add(egui::Button::selectable(active, (icon_img, label)).gap(6.0))
                } else {
                    ui.selectable_label(active, label)
                };
                if resp.clicked() && !active {
                    state.config.active_edition = edition;
                    state.save_config();
                    state.refresh_worlds_and_backups();
                }
            }
        });

        ui.add_space(8.0);

        if state.config.active_edition == GameEdition::TModLoader {
            ui.horizontal(|ui| {
                ui.label(RichText::new("ModPack:").color(p.text_muted));
                let mut selected_modpack = state.config.active_modpack.clone();
                let selected_label = if selected_modpack.is_empty() {
                    "Mặc định (enabled.json)".to_string()
                } else {
                    selected_modpack.clone()
                };
                let mut modpack_changed = false;
                egui::ComboBox::from_id_salt("play_modpack_combo")
                    .selected_text(selected_label)
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(
                                &mut selected_modpack,
                                String::new(),
                                "Mặc định (enabled.json)",
                            )
                            .changed()
                        {
                            modpack_changed = true;
                        }
                        for pack in &state.modpacks {
                            if ui
                                .selectable_value(
                                    &mut selected_modpack,
                                    pack.name.clone(),
                                    format!("{} ({} mod)", pack.name, pack.enabled_mods.len()),
                                )
                                .changed()
                            {
                                modpack_changed = true;
                            }
                        }
                    });
                if modpack_changed {
                    let trimmed = selected_modpack.trim();
                    let pack_to_activate = (!trimmed.is_empty()
                        && state
                            .paths
                            .tml_modpacks_dir()
                            .join(trimmed)
                            .join("Mods")
                            .is_dir())
                    .then(|| state.modpacks.iter().find(|pack| pack.name == trimmed).cloned())
                    .flatten();
                    if let Some(pack) = pack_to_activate {
                        state.activate_selected_modpack(&pack);
                    } else {
                        state.config.active_modpack = selected_modpack;
                        state.save_config();
                    }
                }
                let enabled_cnt = state.installed_mods.iter().filter(|m| m.enabled).count();
                status_pill(
                    ui,
                    &format!("{enabled_cnt}/{} mod đang bật", state.installed_mods.len()),
                    p.accent_cyan,
                );
            });
            ui.add_space(8.0);
        }

        render_missing_prereq_bar(state, ui, p);

        ui.horizontal_wrapped(|ui| {
            if let Some(running) = state.running_game.clone() {
                status_pill(
                    ui,
                    &format!(
                        "Đang chạy {}  •  PID {}  •  {} MB  •  {:.1}% CPU",
                        running.edition.label(),
                        running.pid,
                        running.memory_mb,
                        running.cpu_percent
                    ),
                    p.accent_emerald,
                );
                if danger_button(ui, p, "Đóng Game").clicked() {
                    state.kill_running_game();
                }
            } else if ui
                .add_enabled_ui(!state.install_busy && active_edition_ready, |ui| {
                    primary_button(
                        ui,
                        p,
                        &format!("Chơi {}", state.config.active_edition.label()),
                    )
                })
                .inner
                .clicked()
            {
                state.launch_selected_game(Some((String::new(), 0, String::new())));
            }
        });
    });

    ui.add_space(12.0);

    card_frame(p).show(ui, |ui| {
        ui.set_width(ui.available_width());
        section_heading(ui, p, "Tham gia Server nhanh");
        ui.add_space(6.0);

        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Host:").color(p.text_muted));
            ui.add(
                egui::TextEdit::singleline(&mut state.config.quick_join_host)
                    .desired_width(180.0)
                    .hint_text("127.0.0.1 hoặc bore.pub"),
            );
            ui.label(RichText::new("Cổng:").color(p.text_muted));
            let mut port_str = state.config.quick_join_port.to_string();
            if ui
                .add(egui::TextEdit::singleline(&mut port_str).desired_width(64.0))
                .changed()
            {
                if let Ok(port) = port_str.trim().parse::<u16>() {
                    state.config.quick_join_port = port;
                }
            }
            ui.label(RichText::new("Mật khẩu:").color(p.text_muted));
            ui.add(
                egui::TextEdit::singleline(&mut state.config.quick_join_password)
                    .password(true)
                    .desired_width(110.0),
            );
            if ui
                .add_enabled_ui(!state.install_busy && active_edition_ready, |ui| {
                    primary_button(ui, p, "Tham gia ngay")
                })
                .inner
                .clicked()
            {
                if state.config.quick_join_host.trim().is_empty() {
                    state.notify_err("Nhập địa chỉ Host / IP trước khi tham gia.");
                } else {
                    state.launch_selected_game(None);
                }
            }
        });

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            let srv_running = matches!(
                state.server_snapshot.as_ref().map(|s| &s.state),
                Some(ServerLifecycleState::Running)
            );
            if srv_running {
                let port = state.active_server_port();
                if accent_button(
                    ui,
                    &format!("Điền server máy này (127.0.0.1:{port})"),
                    p.accent_cyan,
                )
                .clicked()
                {
                    state.config.quick_join_host = "127.0.0.1".to_string();
                    state.config.quick_join_port = port;
                    if let Some(profile) = state.config.active_server() {
                        state.config.quick_join_password = profile.config.password.clone();
                    }
                    state.save_config();
                }
            }
            if let Some(ep) = state.active_tunnel_endpoint() {
                if accent_button(ui, &format!("Điền Tunnel ({ep})"), p.accent_amber).clicked() {
                    if let Some((h, p_str)) = ep.rsplit_once(':') {
                        state.config.quick_join_host = h.to_string();
                        if let Ok(port) = p_str.parse::<u16>() {
                            state.config.quick_join_port = port;
                        }
                        state.save_config();
                    }
                }
            }
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);

        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.new_join_name)
                    .desired_width(120.0)
                    .hint_text("Tên gợi nhớ"),
            );
            ui.add(
                egui::TextEdit::singleline(&mut state.new_join_host)
                    .desired_width(150.0)
                    .hint_text("IP / Domain"),
            );
            ui.add(
                egui::TextEdit::singleline(&mut state.new_join_port)
                    .desired_width(60.0)
                    .hint_text("7777"),
            );
            ui.add(
                egui::TextEdit::singleline(&mut state.new_join_password)
                    .password(true)
                    .desired_width(90.0)
                    .hint_text("Mật khẩu"),
            );
            if accent_button(ui, "+ Lưu Server", p.accent_emerald).clicked() {
                state.add_saved_join_server();
            }
        });

        if !state.config.saved_join_servers.is_empty() {
            ui.add_space(8.0);
            let mut join_target = None;
            let mut remove_idx = None;
            for (idx, srv) in state.config.saved_join_servers.iter().enumerate() {
                subcard_frame(p).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&srv.name).strong());
                        ui.label(
                            RichText::new(format!("{}:{}", srv.host, srv.port))
                                .monospace()
                                .color(p.text_muted),
                        );
                        status_pill(ui, srv.edition.label(), p.accent_cyan);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if danger_button(ui, p, "Xóa").clicked() {
                                remove_idx = Some(idx);
                            }
                            if primary_button(ui, p, "Vào").clicked() {
                                join_target = Some((
                                    srv.edition,
                                    srv.host.clone(),
                                    srv.port,
                                    srv.password.clone(),
                                ));
                            }
                        });
                    });
                });
                ui.add_space(4.0);
            }
            if let Some(idx) = remove_idx {
                state.config.saved_join_servers.remove(idx);
                state.save_config();
            }
            if let Some((edition, host, port, pass)) = join_target {
                if state.active_game.is_none() {
                    state.config.active_edition = edition;
                    state.refresh_worlds_and_backups();
                }
                state.launch_selected_game(Some((host, port, pass)));
            }
        }
    });

    ui.add_space(12.0);

    card_frame(p).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 46.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                anim3d::render_worlds_island_3d(ui, p, state.assets.nav_worlds.as_ref());
                section_heading(
                    ui,
                    p,
                    &format!("Thế giới ({})", state.config.active_edition.label()),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Quét lại").clicked() {
                        state.refresh_worlds_and_backups();
                    }
                });
            },
        );
        ui.add_space(6.0);

        if state.worlds.is_empty() {
            ui.label(
                RichText::new("Chưa có file thế giới (.wld) trong thư mục lưu.")
                    .color(p.text_muted),
            );
        } else {
            let worlds = state.worlds.clone();
            for world in &worlds {
                subcard_frame(p).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(&world.world_name).strong().size(14.5));
                        status_pill(ui, world.size_label(), p.accent_cyan);
                        status_pill(ui, world.difficulty_label(), p.accent_amber);
                        if world.has_twld {
                            status_pill(ui, ".twld", p.accent_emerald);
                        }
                        if !world.special_seeds.is_empty() {
                            status_pill(ui, &world.special_seeds.join(", "), p.accent_cyan);
                        }
                        ui.label(
                            RichText::new(format_bytes(world.file_size))
                                .size(12.0)
                                .color(p.text_muted),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if accent_button(ui, "Sao lưu ZIP", p.accent_emerald).clicked() {
                                state.backup_world_file(world);
                            }
                        });
                    });
                });
                ui.add_space(4.0);
            }
        }

        if !state.world_backups.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Bản sao lưu gần đây").strong());
            ui.add_space(4.0);
            let backups = state.world_backups.clone();
            for bak in backups.iter().take(6) {
                subcard_frame(p).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&bak.file_name).monospace().size(12.5));
                        ui.label(
                            RichText::new(format!(
                                "{}  •  {}",
                                format_bytes(bak.file_size),
                                bak.modified_display
                            ))
                            .size(11.5)
                            .color(p.text_muted),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if accent_button(ui, "Khôi phục", p.accent_amber).clicked() {
                                state.restore_backup(bak);
                            }
                        });
                    });
                });
                ui.add_space(3.0);
            }
        }
    });
}

fn render_missing_prereq_bar(state: &mut AppState, ui: &mut egui::Ui, p: ThemePalette) {
    let missing_terraria = !state.env.terraria_installed;
    let missing_xna = !state.env.xna_installed;
    let missing_tml = !state.env.tmodloader_installed;
    let missing_dotnet = state.env.dotnet8_version.is_none();

    let mut warnings = Vec::new();
    if missing_terraria {
        warnings.push("Thiếu Terraria.exe");
    }
    if missing_xna {
        warnings.push("Thiếu XNA");
    }
    if missing_tml {
        warnings.push("Thiếu tModLoader.dll");
    }
    if missing_dotnet {
        warnings.push("Thiếu .NET 8");
    }

    if warnings.is_empty() && state.install_progress.is_none() {
        return;
    }

    let active_missing = !is_edition_ready(state, state.config.active_edition);

    subcard_frame(p).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if !warnings.is_empty() {
            let pill_color = if active_missing {
                p.accent_red
            } else {
                p.accent_amber
            };
            let mut do_tml = false;
            let mut do_dotnet = false;
            let mut do_xna = false;
            let mut do_terraria_url = false;
            let mut do_zip = false;
            let mut open_settings = false;
            let mut do_refresh = false;

            ui.horizontal_wrapped(|ui| {
                status_pill(ui, &warnings.join(" • "), pill_color);
                ui.add_enabled_ui(!state.install_busy, |ui| {
                    if missing_tml && primary_button(ui, p, "Cài tModLoader + .NET 8").clicked() {
                        do_tml = true;
                    }
                    if missing_dotnet
                        && !missing_tml
                        && accent_button(ui, "Cài .NET 8 Portable", p.accent_cyan).clicked()
                    {
                        do_dotnet = true;
                    }
                    if missing_xna && accent_button(ui, "Cài XNA 4.0", p.accent_amber).clicked() {
                        do_xna = true;
                    }
                    if missing_terraria
                        && primary_button(ui, p, "Tải & Cài Terraria")
                            .on_hover_text(format!(
                                "Tải trực tiếp từ nguồn: {}",
                                state.effective_terraria_source()
                            ))
                            .clicked()
                    {
                        do_terraria_url = true;
                    }
                    if missing_terraria
                        && accent_button(ui, "Cài từ file .zip", p.accent_cyan).clicked()
                    {
                        do_zip = true;
                    }
                });
                if missing_terraria && ui.button("Nguồn tải Terraria").clicked() {
                    open_settings = true;
                }
                if ui.button("Làm mới kiểm tra").clicked() {
                    do_refresh = true;
                }
            });

            if do_tml {
                state.install_tmodloader_selected();
            }
            if do_dotnet {
                state.install_dotnet8();
            }
            if do_xna {
                state.install_xna();
            }
            if do_terraria_url {
                state.install_terraria_url();
            }
            if do_zip {
                state.pick_and_install_terraria_zip();
            }
            if open_settings {
                state.active_tab = NavTab::Settings;
            }
            if do_refresh {
                state.refresh_environment();
            }
        }

        if let Some(prog) = &state.install_progress {
            if !warnings.is_empty() {
                ui.add_space(6.0);
            }
            render_install_progress_bar(ui, prog);
        }
    });
    ui.add_space(8.0);
}

pub(crate) fn render_install_progress_bar(ui: &mut egui::Ui, p: &InstallProgress) {
    let accent = if ui.visuals().dark_mode {
        DARK_PALETTE.accent_emerald
    } else {
        LIGHT_PALETTE.accent_emerald
    };
    let stage_lower = p.stage.to_lowercase();
    let is_extraction = stage_lower.contains("giải nén") || stage_lower.contains("extract");
    let detail = match (is_extraction, p.total) {
        (true, Some(total)) if total > 0 => format!(" ({}/{})", p.current, total),
        (true, _) if p.current > 0 => format!(" ({})", p.current),
        (false, Some(total)) if total > 0 => {
            format!(" ({} / {})", format_bytes(p.current), format_bytes(total))
        }
        (false, _) if p.current > 0 => format!(" ({})", format_bytes(p.current)),
        _ => String::new(),
    };
    let label = format!("{}: {}{}", p.target.label(), p.stage, detail);
    let frac = p.fraction();
    ui.add(
        egui::ProgressBar::new(frac.unwrap_or(0.0))
            .text(label)
            .fill(accent)
            .animate(frac.is_none())
            .desired_width(ui.available_width()),
    );
}
