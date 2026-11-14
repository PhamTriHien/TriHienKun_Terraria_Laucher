use crate::state::AppState;
use crate::theme::ThemePalette;
use crate::views::play_view::render_install_progress_bar;
use crate::{anim3d, theme};
use egui::RichText;
use tl_core::ThemeMode;
use tl_runtime::TmodLoaderReleaseInfo;

pub fn render(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let busy = state.install_busy || state.tml_releases_busy || state.terraria_check_busy;

    ui.horizontal(|ui| {
        anim3d::render_settings_gears_3d(ui, p, state.assets.nav_settings.as_ref(), busy);
        ui.vertical(|ui| {
            ui.label(
                RichText::new("Cài đặt & Môi trường")
                    .size(21.0)
                    .strong()
                    .color(p.text_primary),
            );
            if busy {
                ui.label(
                    RichText::new("Đang kiểm tra / cài đặt...")
                        .size(12.0)
                        .color(p.accent_cyan),
                );
            }
        });
    });
    ui.add_space(8.0);

    theme::card_frame(p).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let current_mode = state.config.theme_mode;
        ui.horizontal_wrapped(|ui| {
            theme::section_heading(ui, p, "Giao diện Launcher (Dark / Light)");
            theme::status_pill(
                ui,
                &format!("Đang dùng: {}", current_mode.label()),
                if current_mode.is_dark() {
                    p.accent_cyan
                } else {
                    p.accent_amber
                },
            );
        });
        ui.add_space(8.0);

        let mut next_mode = None;
        ui.horizontal_wrapped(|ui| {
            for mode in [ThemeMode::Dark, ThemeMode::Light] {
                let active = current_mode == mode;
                let clicked = if active {
                    theme::primary_button(ui, p, mode.label()).clicked()
                } else {
                    ui.button(mode.label()).clicked()
                };
                if clicked && !active {
                    next_mode = Some(mode);
                }
            }
        });
        if let Some(mode) = next_mode {
            state.set_theme_mode(ui.ctx(), mode);
        }
    });

    ui.add_space(12.0);

    theme::card_frame(p).show(ui, |ui| {
        ui.set_width(ui.available_width());
        theme::section_heading(ui, p, "Trạng thái Thành phần");
        ui.add_space(8.0);

        ui.horizontal_wrapped(|ui| {
            theme::status_pill(
                ui,
                if state.env.terraria_installed {
                    "Terraria.exe: Sẵn sàng"
                } else {
                    "Terraria.exe: Chưa thấy"
                },
                if state.env.terraria_installed {
                    p.accent_emerald
                } else {
                    p.accent_red
                },
            );
            theme::status_pill(
                ui,
                if state.env.terraria_server_installed {
                    "TerrariaServer.exe: Sẵn sàng"
                } else {
                    "TerrariaServer.exe: Chưa thấy"
                },
                if state.env.terraria_server_installed {
                    p.accent_emerald
                } else {
                    p.accent_red
                },
            );
            theme::status_pill(
                ui,
                if state.env.tmodloader_installed {
                    "tModLoader.dll: Sẵn sàng"
                } else {
                    "tModLoader.dll: Chưa thấy"
                },
                if state.env.tmodloader_installed {
                    p.accent_emerald
                } else {
                    p.accent_red
                },
            );
            theme::status_pill(
                ui,
                &match &state.env.dotnet8_version {
                    Some(v) => format!(".NET {v}: Sẵn sàng"),
                    None => ".NET 8: Chưa cài".to_string(),
                },
                if state.env.dotnet8_version.is_some() {
                    p.accent_emerald
                } else {
                    p.accent_red
                },
            );
            theme::status_pill(
                ui,
                if state.env.xna_installed {
                    "XNA Framework 4.0: Đã cài"
                } else {
                    "XNA Framework 4.0: Chưa cài"
                },
                if state.env.xna_installed {
                    p.accent_emerald
                } else {
                    p.accent_amber
                },
            );
        });

        ui.add_space(8.0);
        let mut trigger_xna = false;
        ui.horizontal_wrapped(|ui| {
            if !state.env.xna_installed
                && ui
                    .add_enabled_ui(!state.install_busy, |ui| {
                        theme::accent_button(ui, "Cài XNA Framework 4.0", p.accent_amber)
                    })
                    .inner
                    .clicked()
            {
                trigger_xna = true;
            }
            if ui.button("Mở thư mục gốc Launcher").clicked() {
                let _ = open::that(&state.paths.base_dir);
            }
            if ui.button("Làm mới kiểm tra").clicked() {
                state.refresh_environment();
                state.notify_ok("Đã kiểm tra lại môi trường.");
            }
        });
        if trigger_xna {
            state.install_xna();
        }
    });

    ui.add_space(12.0);

    render_game_installer_card(state, ui, p);

    ui.add_space(12.0);

    theme::card_frame(p).show(ui, |ui| {
        ui.set_width(ui.available_width());
        theme::section_heading(ui, p, "Thư mục Game & Công cụ (để trống = dùng mặc định)");
        ui.add_space(8.0);

        theme::subcard_frame(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Thư mục Terraria:").color(p.text_muted));
                ui.add(
                    egui::TextEdit::singleline(&mut state.config.custom_terraria_dir)
                        .desired_width(300.0)
                        .hint_text(state.paths.terraria_dir.to_string_lossy().as_ref()),
                );
                if ui.button("Chọn...").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        state.config.custom_terraria_dir = dir.to_string_lossy().into_owned();
                    }
                }
            });

            ui.horizontal(|ui| {
                ui.label(RichText::new("Thư mục tModLoader:").color(p.text_muted));
                ui.add(
                    egui::TextEdit::singleline(&mut state.config.custom_tmodloader_dir)
                        .desired_width(284.0)
                        .hint_text(state.paths.tmodloader_dir.to_string_lossy().as_ref()),
                );
                if ui.button("Chọn...").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        state.config.custom_tmodloader_dir = dir.to_string_lossy().into_owned();
                    }
                }
            });

            ui.horizontal(|ui| {
                ui.label(RichText::new("File steamcmd.exe:").color(p.text_muted));
                ui.add(
                    egui::TextEdit::singleline(&mut state.config.custom_steamcmd_path)
                        .desired_width(296.0)
                        .hint_text("Tự tìm trong WinGet / PATH / data\\SteamCMD"),
                );
                if ui.button("Chọn...").clicked() {
                    if let Some(file) = rfd::FileDialog::new()
                        .add_filter("Executable", &["exe"])
                        .pick_file()
                    {
                        state.config.custom_steamcmd_path = file.to_string_lossy().into_owned();
                    }
                }
            });
        });
    });

    ui.add_space(12.0);

    theme::card_frame(p).show(ui, |ui| {
        ui.set_width(ui.available_width());
        theme::section_heading(ui, p, "Cấu hình Tunnel & Proxy");
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label(RichText::new("Bore Server:").color(p.text_muted));
            ui.add(
                egui::TextEdit::singleline(&mut state.config.bore_server)
                    .desired_width(200.0)
                    .hint_text("bore.pub"),
            );
            ui.label(RichText::new("Secret (tùy chọn):").color(p.text_muted));
            ui.add(
                egui::TextEdit::singleline(&mut state.config.bore_secret)
                    .password(true)
                    .desired_width(140.0),
            );
        });

        ui.horizontal(|ui| {
            ui.label(RichText::new("Proxy URL (tùy chọn):").color(p.text_muted));
            ui.add(
                egui::TextEdit::singleline(&mut state.config.proxy_url)
                    .desired_width(280.0)
                    .hint_text("vd: http://127.0.0.1:7890 hoặc socks5://127.0.0.1:10808"),
            );
        });

        ui.add_space(8.0);
        if theme::primary_button(ui, p, "Lưu & Áp dụng").clicked() {
            state.save_config();
            state.refresh_environment();
            state.notify_ok("Đã lưu cài đặt và làm mới cấu hình.");
        }
    });
}

fn render_game_installer_card(state: &mut AppState, ui: &mut egui::Ui, p: ThemePalette) {
    theme::card_frame(p).show(ui, |ui| {
        ui.set_width(ui.available_width());
        theme::section_heading(ui, p, "Cài đặt & Cập nhật Game");
        ui.add_space(8.0);

        theme::subcard_frame(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("tModLoader (GitHub Releases)")
                    .strong()
                    .color(p.text_primary),
            );
            ui.add_space(4.0);

            let selected_text = if state.tml_releases_busy {
                "Đang tải danh sách phiên bản...".to_string()
            } else if state.tml_releases.is_empty() {
                "Bản ổn định mới nhất (tự lấy khi cài)".to_string()
            } else {
                state
                    .tml_releases
                    .iter()
                    .find(|r| r.tag_name == state.selected_tml_tag)
                    .or_else(|| state.tml_releases.iter().find(|r| !r.prerelease))
                    .or_else(|| state.tml_releases.first())
                    .map(format_release_option)
                    .unwrap_or_else(|| "Chọn phiên bản".to_string())
            };

            let mut do_refresh_releases = false;
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Phiên bản:").color(p.text_muted));
                egui::ComboBox::from_id_salt("settings_tml_release_combo")
                    .selected_text(selected_text)
                    .width(290.0)
                    .show_ui(ui, |ui| {
                        if state.tml_releases.is_empty() {
                            ui.label(
                                RichText::new("Nhấn 'Kiểm tra bản mới' để tải danh sách.")
                                    .color(p.text_muted),
                            );
                        } else {
                            for rel in &state.tml_releases {
                                ui.selectable_value(
                                    &mut state.selected_tml_tag,
                                    rel.tag_name.clone(),
                                    format_release_option(rel),
                                );
                            }
                        }
                    });

                if ui
                    .add_enabled(
                        !state.tml_releases_busy,
                        egui::Button::new("Kiểm tra bản mới"),
                    )
                    .clicked()
                {
                    do_refresh_releases = true;
                }
            });
            if do_refresh_releases {
                state.refresh_tmodloader_releases();
            }

            ui.add_space(4.0);
            let dotnet_missing = state.env.dotnet8_version.is_none();
            let mut do_install_tml = false;
            let mut do_install_dotnet = false;
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled_ui(!state.install_busy, |ui| {
                    if theme::primary_button(ui, p, "Cài đặt / Cập nhật tModLoader").clicked() {
                        do_install_tml = true;
                    }
                    let dotnet_label = if dotnet_missing {
                        "Cài .NET 8 Portable"
                    } else {
                        "Cài lại .NET 8 Portable"
                    };
                    if theme::accent_button(ui, dotnet_label, p.accent_cyan).clicked() {
                        do_install_dotnet = true;
                    }
                });
            });
            if do_install_tml {
                state.install_tmodloader_selected();
            }
            if do_install_dotnet {
                state.install_dotnet8();
            }
        });

        ui.add_space(8.0);

        theme::subcard_frame(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let is_synced = state.env.terraria_installed
                && state
                    .terraria_remote_manifest
                    .as_ref()
                    .zip(state.terraria_installed_receipt.as_ref())
                    .is_some_and(|(m, r)| {
                        !r.sha256.is_empty() && r.sha256.eq_ignore_ascii_case(&m.sha256)
                    });

            let terraria_status_pill = if let Some(m) = &state.terraria_remote_manifest {
                let size_part = if m.total_size > 0 {
                    format!(" ({})", theme::format_bytes(m.total_size))
                } else {
                    String::new()
                };
                if !state.env.terraria_installed {
                    Some((
                        format!("Có sẵn bản {}{}", m.version, size_part),
                        p.accent_amber,
                    ))
                } else if is_synced {
                    Some((
                        format!("Đã đồng bộ bản {}{}", m.version, size_part),
                        p.accent_emerald,
                    ))
                } else {
                    Some((
                        format!("Bản trên nguồn: {}{}", m.version, size_part),
                        p.accent_cyan,
                    ))
                }
            } else if state.env.terraria_installed {
                state
                    .terraria_installed_receipt
                    .as_ref()
                    .map(|rec| (format!("Đã cài bản {}", rec.version), p.text_muted))
            } else {
                None
            };

            let install_btn_label = match &state.terraria_remote_manifest {
                Some(m) if !m.version.trim().is_empty() && !state.env.terraria_installed => {
                    format!("Tải & Cài Terraria ({})", m.version)
                }
                Some(m) if !m.version.trim().is_empty() && !is_synced => {
                    format!("Cập nhật Terraria ({})", m.version)
                }
                Some(m) if !m.version.trim().is_empty() => {
                    format!("Cài lại Terraria ({})", m.version)
                }
                _ => "Tải & Cài từ nguồn".to_string(),
            };

            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("Terraria (Nhánh GitHub, Release hoặc URL .zip)")
                        .strong()
                        .color(p.text_primary),
                );
                if let Some((text, color)) = terraria_status_pill {
                    theme::status_pill(ui, &text, color);
                }
            });
            ui.add_space(4.0);

            let mut config_dirty = false;
            let mut clear_remote_manifest = false;
            let mut do_check_update = false;
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Nguồn tải:").color(p.text_muted));
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut state.config.terraria_install_source)
                        .desired_width(360.0)
                        .hint_text("owner/repo#branch, owner/repo[@tag], hoặc URL .zip"),
                );
                if resp.changed() {
                    clear_remote_manifest = true;
                }
                if resp.lost_focus() {
                    config_dirty = true;
                }
                if ui
                    .add_enabled(
                        !state.terraria_check_busy,
                        egui::Button::new("Kiểm tra cập nhật"),
                    )
                    .clicked()
                {
                    do_check_update = true;
                }
            });

            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("GitHub Token (tùy chọn):").color(p.text_muted));
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut state.config.github_release_token)
                        .password(true)
                        .desired_width(250.0)
                        .hint_text("Dùng cho Private repo"),
                );
                if resp.lost_focus() {
                    config_dirty = true;
                }
            });

            if clear_remote_manifest {
                state.terraria_remote_manifest = None;
            }
            if config_dirty || do_check_update {
                state.save_config();
            }
            if do_check_update {
                state.refresh_terraria_update_info();
            }

            ui.add_space(4.0);
            let mut do_url = false;
            let mut do_zip = false;
            let mut do_save = false;
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled_ui(!state.install_busy, |ui| {
                    if theme::primary_button(ui, p, &install_btn_label).clicked() {
                        do_url = true;
                    }
                    if theme::accent_button(ui, "Chọn file .zip trên máy...", p.accent_cyan)
                        .clicked()
                    {
                        do_zip = true;
                    }
                });
                if ui.button("Lưu nguồn cài").clicked() {
                    do_save = true;
                }
            });
            if do_save {
                state.save_config();
                state.notify_ok("Đã lưu nguồn cài đặt Terraria.");
            }
            if do_url {
                state.install_terraria_url();
            }
            if do_zip {
                state.pick_and_install_terraria_zip();
            }
        });

        if let Some(prog) = &state.install_progress {
            ui.add_space(8.0);
            render_install_progress_bar(ui, prog);
        }

        if let Some(err) = state.install_last_error.clone() {
            ui.add_space(8.0);
            theme::subcard_frame(p).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("Lỗi cài đặt: {err}")).color(p.accent_red));
                    if ui.button("Đóng").clicked() {
                        state.install_last_error = None;
                    }
                });
            });
        }
    });
}

fn format_release_option(rel: &TmodLoaderReleaseInfo) -> String {
    let kind = if rel.prerelease {
        "Preview"
    } else {
        "Ổn định"
    };
    if rel.size > 0 {
        format!(
            "{} [{}] • {}",
            rel.tag_name,
            kind,
            theme::format_bytes(rel.size)
        )
    } else {
        format!("{} [{}]", rel.tag_name, kind)
    }
}
