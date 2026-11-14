use crate::anim3d;
use crate::assets::{ButtonSkin, UiIcon};
use crate::state::{AppState, NavTab};
use crate::theme::{
    format_bytes, page_title, rpg_button, rpg_card, rpg_compact_button, rpg_section_heading,
    rpg_subcard, rpg_tex_button, status_pill, ThemePalette, DARK_PALETTE, LIGHT_PALETTE,
};
use egui::RichText;
use tl_core::GameEdition;
use tl_runtime::{InstallProgress, ServerLifecycleState};

fn is_edition_ready(state: &AppState, edition: GameEdition) -> bool {
    match edition {
        GameEdition::Vanilla => state.env.terraria_installed && state.env.xna_installed,
        GameEdition::TModLoader => {
            state.env.tmodloader_installed && state.env.dotnet8_version.is_some()
        }
    }
}

fn clean_world_size_label(raw: &str) -> &str {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("small") || raw.contains("Nhỏ") {
        "Nhỏ"
    } else if lower.contains("medium") || raw.contains("Vừa") || raw.contains("Trung") {
        "Vừa"
    } else if lower.contains("large") || raw.contains("Lớn") {
        "Lớn"
    } else {
        raw
    }
}

fn clean_world_difficulty_label(raw: &str) -> &str {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("journey") || raw.contains("Hành trình") {
        "Hành trình"
    } else if lower.contains("master") {
        "Master"
    } else if lower.contains("expert") {
        "Expert"
    } else if lower.contains("classic")
        || lower.contains("normal")
        || raw.contains("Thường")
        || raw.contains("Cổ điển")
    {
        "Thường"
    } else {
        raw
    }
}

pub fn render(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();

    page_title(ui, p, "Khởi chạy Game & Thế giới");
    ui.add_space(8.0);

    anim3d::render_play_hero_3d(
        ui,
        p,
        assets.hero_banner(state.config.active_edition),
        assets.edition_icon(state.config.active_edition),
        state.config.active_edition,
        state.is_game_running(),
        state.installed_mods.len(),
    );
    ui.add_space(10.0);

    let active_edition_ready = is_edition_ready(state, state.config.active_edition);

    rpg_card(ui, assets.as_ref(), p, |ui| {
        rpg_section_heading(
            ui,
            assets.as_ref(),
            p,
            UiIcon::Play,
            "Phiên bản & Khởi động",
        );
        ui.add_space(6.0);

        ui.horizontal_wrapped(|ui| {
            for edition in [GameEdition::TModLoader, GameEdition::Vanilla] {
                let selected = state.config.active_edition == edition;
                let skin = if selected {
                    match edition {
                        GameEdition::Vanilla => ButtonSkin::Emerald,
                        GameEdition::TModLoader => ButtonSkin::Cyan,
                    }
                } else {
                    ButtonSkin::Slate
                };
                let resp = rpg_tex_button(
                    ui,
                    assets.as_ref(),
                    p,
                    skin,
                    assets.edition_icon(edition),
                    edition.label(),
                    selected,
                );
                if resp.clicked() && !selected {
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
                    "Mặc định (Mod đang bật)".to_string()
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
                                "Mặc định (Mod đang bật)",
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
            if let Some(running) = state.current_running_game().cloned() {
                status_pill(
                    ui,
                    &format!("Đang chạy {}", running.edition.label()),
                    p.accent_emerald,
                );
                if rpg_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Red,
                    UiIcon::Stop,
                    "Đóng Game",
                )
                .clicked()
                {
                    state.kill_running_game();
                }
            } else {
                let launch_disabled_hint = if state.install_busy {
                    "Đang cài đặt thành phần..."
                } else {
                    "Cài đặt thành phần còn thiếu ở thanh phía trên trước khi chơi"
                };
                if ui
                    .add_enabled_ui(!state.install_busy && active_edition_ready, |ui| {
                        rpg_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Emerald,
                            UiIcon::Play,
                            &format!("Chơi {}", state.config.active_edition.label()),
                        )
                    })
                    .inner
                    .on_disabled_hover_text(launch_disabled_hint)
                    .clicked()
                {
                    state.launch_selected_game(Some((String::new(), 0, String::new())));
                }
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
            "Tham gia Server nhanh",
        );
        ui.add_space(6.0);

        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Host:").color(p.text_muted));
            let pasted = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))));
            let host_resp = ui.add(
                egui::TextEdit::singleline(&mut state.config.quick_join_host)
                    .desired_width(185.0)
                    .hint_text("IP, Domain hoặc [IPv6]:port"),
            );
            if (host_resp.changed() && pasted) || host_resp.lost_focus() {
                if let Some((host, port)) = parse_pasted_host_port(&state.config.quick_join_host) {
                    state.config.quick_join_host = host;
                    state.config.quick_join_port = port;
                    state.quick_join_port_input = port.to_string();
                    state.save_config();
                } else if host_resp.lost_focus() {
                    state.save_config();
                }
            }

            ui.label(RichText::new("Cổng:").color(p.text_muted));
            let port_resp = ui.add(
                egui::TextEdit::singleline(&mut state.quick_join_port_input)
                    .desired_width(62.0)
                    .hint_text("7777"),
            );
            let parsed_port = port_resp
                .changed()
                .then(|| state.quick_join_port_input.trim().parse::<u16>().ok())
                .flatten()
                .filter(|&port| port > 0);
            if let Some(port) = parsed_port {
                state.config.quick_join_port = port;
            }
            if port_resp.lost_focus() {
                match state.quick_join_port_input.trim().parse::<u16>() {
                    Ok(port) if port > 0 => {
                        state.config.quick_join_port = port;
                        state.quick_join_port_input = port.to_string();
                    }
                    _ => {
                        let fallback = state.config.quick_join_port.max(1);
                        state.config.quick_join_port = fallback;
                        state.quick_join_port_input = fallback.to_string();
                    }
                }
                state.save_config();
            }

            ui.label(RichText::new("Mật khẩu:").color(p.text_muted));
            let pass_resp = ui.add(
                egui::TextEdit::singleline(&mut state.config.quick_join_password)
                    .password(true)
                    .desired_width(105.0)
                    .hint_text("Tùy chọn"),
            );
            if pass_resp.lost_focus() {
                state.save_config();
            }

            let host_non_empty = !state.config.quick_join_host.trim().is_empty();
            let can_join_now = !state.install_busy
                && active_edition_ready
                && !state.is_game_running()
                && host_non_empty;
            let join_disabled_hint = if state.install_busy {
                "Đang cài đặt thành phần..."
            } else if state.is_game_running() {
                "Đóng cửa sổ game đang mở trước khi tham gia server"
            } else if !active_edition_ready {
                "Cần cài đặt đầy đủ phiên bản game trước"
            } else {
                "Nhập địa chỉ Host / IP trước"
            };
            if ui
                .add_enabled_ui(can_join_now, |ui| {
                    rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Emerald,
                        UiIcon::Play,
                        "Tham gia ngay",
                    )
                })
                .inner
                .on_disabled_hover_text(join_disabled_hint)
                .clicked()
            {
                state.launch_selected_game(None);
            }
        });

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            let ext_ready =
                state.showing_external_server() && !state.is_active_external_starting();
            let srv_running = matches!(
                state.active_server_snapshot().map(|s| &s.state),
                Some(ServerLifecycleState::Running)
            ) || ext_ready;
            if srv_running {
                let port = state.active_server_port();
                let local_disabled_hint = if state.install_busy {
                    "Đang cài đặt thành phần..."
                } else {
                    "Đóng cửa sổ game đang mở trước"
                };
                if ui
                    .add_enabled_ui(!state.install_busy && !state.is_game_running(), |ui| {
                        rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Cyan,
                            Some(UiIcon::Server),
                            &format!("Vào Server máy này (127.0.0.1:{port})"),
                        )
                    })
                    .inner
                    .on_disabled_hover_text(local_disabled_hint)
                    .clicked()
                {
                    state.join_own_hosted_server();
                }
            }

            ui.add(
                egui::TextEdit::singleline(&mut state.new_join_name)
                    .desired_width(135.0)
                    .hint_text("Tên lưu (tùy chọn)"),
            );
            let can_save_fav = !state.config.quick_join_host.trim().is_empty();
            if ui
                .add_enabled_ui(can_save_fav, |ui| {
                    rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Cyan,
                        Some(UiIcon::Pack),
                        "+ Lưu danh sách",
                    )
                })
                .inner
                .on_disabled_hover_text("Nhập địa chỉ Host / IP trước")
                .clicked()
            {
                state.save_quick_join_server();
            }
        });

        if !state.config.saved_join_servers.is_empty() {
            ui.add_space(8.0);
            let mut join_target = None;
            let mut remove_idx = None;
            for (idx, srv) in state.config.saved_join_servers.iter().enumerate() {
                let srv_ready = is_edition_ready(state, srv.edition);
                let can_join_saved =
                    !state.install_busy && !state.is_game_running() && srv_ready;
                let saved_disabled_hint = if state.install_busy {
                    "Đang cài đặt thành phần..."
                } else if state.is_game_running() {
                    "Đóng cửa sổ game đang mở trước"
                } else {
                    "Phiên bản của server này chưa cài đủ thành phần"
                };
                rpg_subcard(ui, assets.as_ref(), p, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&srv.name).strong());
                        let ep_text = if srv.host.contains(':') {
                            format!("[{}]:{}", srv.host, srv.port)
                        } else {
                            format!("{}:{}", srv.host, srv.port)
                        };
                        ui.label(RichText::new(ep_text).monospace().color(p.text_muted));
                        status_pill(ui, srv.edition.label(), p.accent_cyan);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if rpg_compact_button(
                                ui,
                                assets.as_ref(),
                                p,
                                ButtonSkin::Red,
                                Some(UiIcon::Trash),
                                "Xóa",
                            )
                            .clicked()
                            {
                                remove_idx = Some(idx);
                            }
                            if ui
                                .add_enabled_ui(can_join_saved, |ui| {
                                    rpg_compact_button(
                                        ui,
                                        assets.as_ref(),
                                        p,
                                        ButtonSkin::Emerald,
                                        Some(UiIcon::Play),
                                        "Vào",
                                    )
                                })
                                .inner
                                .on_disabled_hover_text(saved_disabled_hint)
                                .clicked()
                            {
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
                if !state.is_game_running() {
                    state.config.active_edition = edition;
                    state.refresh_worlds_and_backups();
                }
                state.launch_selected_game(Some((host, port, pass)));
            }
        }
    });

    ui.add_space(12.0);

    rpg_card(ui, assets.as_ref(), p, |ui| {
        ui.horizontal(|ui| {
            rpg_section_heading(ui, assets.as_ref(), p, UiIcon::Worlds, "Thế giới đã lưu");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if rpg_compact_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Slate,
                    Some(UiIcon::Refresh),
                    "Quét lại",
                )
                .clicked()
                {
                    state.refresh_worlds_and_backups();
                }
            });
        });
        ui.add_space(6.0);

        if state.worlds.is_empty() {
            ui.label(
                RichText::new("Chưa có thế giới nào cho phiên bản đang chọn.")
                    .color(p.text_muted),
            );
        } else {
            let worlds = state.worlds.clone();
            for world in &worlds {
                rpg_subcard(ui, assets.as_ref(), p, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(&world.world_name).strong().size(14.5));
                        status_pill(ui, clean_world_size_label(world.size_label()), p.accent_cyan);
                        status_pill(
                            ui,
                            clean_world_difficulty_label(world.difficulty_label()),
                            p.accent_amber,
                        );
                        if !world.special_seeds.is_empty() {
                            status_pill(ui, &world.special_seeds.join(", "), p.accent_cyan);
                        }
                        ui.label(
                            RichText::new(format_bytes(world.file_size))
                                .size(12.0)
                                .color(p.text_muted),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if rpg_compact_button(
                                ui,
                                assets.as_ref(),
                                p,
                                ButtonSkin::Cyan,
                                Some(UiIcon::Pack),
                                "Sao lưu ZIP",
                            )
                            .clicked()
                            {
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
            let backups = state.world_backups.clone();
            let mut ask_confirm = None;
            let mut cancel_confirm = false;
            let mut restore_target = None;

            egui::CollapsingHeader::new(
                RichText::new(format!("Bản sao lưu ({})", backups.len())).strong(),
            )
            .default_open(false)
            .show(ui, |ui| {
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .id_salt("play_world_backups_scroll")
                    .max_height(200.0)
                    .show(ui, |ui| {
                        for bak in &backups {
                            let is_confirming =
                                state.confirm_restore_backup.as_ref() == Some(&bak.file_path);
                            rpg_subcard(ui, assets.as_ref(), p, |ui| {
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
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if is_confirming {
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
                                                    cancel_confirm = true;
                                                }
                                                if rpg_compact_button(
                                                    ui,
                                                    assets.as_ref(),
                                                    p,
                                                    ButtonSkin::Amber,
                                                    Some(UiIcon::Refresh),
                                                    "Ghi đè",
                                                )
                                                .clicked()
                                                {
                                                    restore_target = Some(bak.clone());
                                                }
                                            } else if rpg_compact_button(
                                                ui,
                                                assets.as_ref(),
                                                p,
                                                ButtonSkin::Amber,
                                                Some(UiIcon::Refresh),
                                                "Khôi phục",
                                            )
                                            .clicked()
                                            {
                                                ask_confirm = Some(bak.file_path.clone());
                                            }
                                        },
                                    );
                                });
                            });
                            ui.add_space(3.0);
                        }
                    });
            });

            if cancel_confirm {
                state.confirm_restore_backup = None;
            }
            if let Some(path) = ask_confirm {
                state.confirm_restore_backup = Some(path);
            }
            if let Some(bak) = restore_target {
                state.confirm_restore_backup = None;
                state.restore_backup(&bak);
            }
        }
    });
}

fn render_missing_prereq_bar(state: &mut AppState, ui: &mut egui::Ui, p: ThemePalette) {
    let assets = state.assets.clone();
    let (missing_terraria, missing_xna, missing_tml, missing_dotnet) =
        match state.config.active_edition {
            GameEdition::Vanilla => (!state.env.terraria_installed, !state.env.xna_installed, false, false),
            GameEdition::TModLoader => (
                false,
                false,
                !state.env.tmodloader_installed,
                state.env.dotnet8_version.is_none(),
            ),
        };

    let mut warnings = Vec::new();
    if missing_terraria {
        warnings.push("Chưa cài Terraria");
    }
    if missing_xna {
        warnings.push("Thiếu XNA 4.0");
    }
    if missing_tml {
        warnings.push("Chưa cài tModLoader");
    }
    if missing_dotnet {
        warnings.push("Thiếu .NET 8");
    }

    if warnings.is_empty() && state.install_progress.is_none() {
        return;
    }

    rpg_subcard(ui, assets.as_ref(), p, |ui| {
        if !warnings.is_empty() {
            let mut do_tml = false;
            let mut do_dotnet = false;
            let mut do_xna = false;
            let mut do_terraria_url = false;
            let mut open_settings = false;

            ui.horizontal_wrapped(|ui| {
                status_pill(ui, &warnings.join(" • "), p.accent_red);
                ui.add_enabled_ui(!state.install_busy, |ui| {
                    if missing_terraria
                        && rpg_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Cyan,
                            UiIcon::Download,
                            "Tải & Cài Terraria",
                        )
                        .on_hover_text(format!(
                            "Tải trực tiếp từ nguồn: {}",
                            state.effective_terraria_source()
                        ))
                        .clicked()
                    {
                        do_terraria_url = true;
                    }
                    if missing_xna
                        && rpg_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Amber,
                            UiIcon::Download,
                            "Cài XNA 4.0",
                        )
                        .clicked()
                    {
                        do_xna = true;
                    }
                    if missing_tml {
                        let tml_btn_label = if missing_dotnet {
                            "Cài tModLoader + .NET 8"
                        } else {
                            "Cài tModLoader"
                        };
                        if rpg_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Cyan,
                            UiIcon::Download,
                            tml_btn_label,
                        )
                        .clicked()
                        {
                            do_tml = true;
                        }
                    } else if missing_dotnet
                        && rpg_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Cyan,
                            UiIcon::Download,
                            "Cài .NET 8",
                        )
                        .clicked()
                    {
                        do_dotnet = true;
                    }
                });
                if rpg_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Slate,
                    UiIcon::Settings,
                    "Mở Cài đặt",
                )
                .clicked()
                {
                    open_settings = true;
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
            if open_settings {
                state.active_tab = NavTab::Settings;
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

pub(crate) fn parse_pasted_host_port(raw: &str) -> Option<(String, u16)> {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix('[') {
        let (ipv6, port_part) = rest.split_once("]:")?;
        let ipv6 = ipv6.split('%').next().unwrap_or(ipv6).trim();
        if ipv6.is_empty() {
            return None;
        }
        let port = port_part.trim().parse::<u16>().ok().filter(|&p| p > 0)?;
        return Some((ipv6.to_string(), port));
    }
    if trimmed.matches(':').count() != 1 {
        return None;
    }
    let (host, port_str) = trimmed.split_once(':')?;
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    let port = port_str.trim().parse::<u16>().ok().filter(|&p| p > 0)?;
    Some((host.to_string(), port))
}
