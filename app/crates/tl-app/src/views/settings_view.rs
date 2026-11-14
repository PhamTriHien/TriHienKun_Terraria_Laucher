use crate::assets::{ButtonSkin, LauncherAssets, UiIcon};
use crate::state::AppState;
use crate::theme::ThemePalette;
use crate::views::play_view::render_install_progress_bar;
use crate::{anim3d, theme};
use egui::RichText;
use tl_core::{GraphicsBackend, ProcessPriority, ThemeMode};
use tl_runtime::TmodLoaderReleaseInfo;

pub fn render(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    let busy = state.install_busy || state.tml_releases_busy || state.terraria_check_busy;

    ui.horizontal(|ui| {
        anim3d::render_settings_gears_3d(ui, p, assets.nav_settings.as_ref(), busy);
        ui.vertical(|ui| {
            theme::page_title(ui, p, "Cài đặt & Môi trường");
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

    theme::rpg_card(ui, assets.as_ref(), p, |ui| {
        let current_mode = state.config.theme_mode;
        let theme_icon = if current_mode.is_dark() {
            UiIcon::Moon
        } else {
            UiIcon::Sun
        };
        ui.horizontal_wrapped(|ui| {
            theme::rpg_section_heading(
                ui,
                assets.as_ref(),
                p,
                theme_icon,
                "Giao diện Launcher (Dark / Light)",
            );
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
                let (active_skin, icon) = match mode {
                    ThemeMode::Dark => (ButtonSkin::Cyan, UiIcon::Moon),
                    ThemeMode::Light => (ButtonSkin::Amber, UiIcon::Sun),
                };
                if theme::rpg_selectable_button(
                    ui,
                    assets.as_ref(),
                    p,
                    active,
                    active_skin,
                    icon,
                    mode.label(),
                )
                .clicked()
                    && !active
                {
                    next_mode = Some(mode);
                }
            }
        });
        if let Some(mode) = next_mode {
            state.set_theme_mode(ui.ctx(), mode);
        }
    });

    ui.add_space(12.0);

    theme::rpg_card(ui, assets.as_ref(), p, |ui| {
        theme::rpg_section_heading(
            ui,
            assets.as_ref(),
            p,
            UiIcon::Settings,
            "Trạng thái Thành phần",
        );
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
                        theme::rpg_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Amber,
                            UiIcon::Download,
                            "Cài XNA Framework 4.0",
                        )
                    })
                    .inner
                    .clicked()
            {
                trigger_xna = true;
            }
            if theme::rpg_button(
                ui,
                assets.as_ref(),
                p,
                ButtonSkin::Slate,
                UiIcon::Folder,
                "Mở thư mục gốc Launcher",
            )
            .clicked()
            {
                let _ = open::that(&state.paths.base_dir);
            }
            if theme::rpg_button(
                ui,
                assets.as_ref(),
                p,
                ButtonSkin::Slate,
                UiIcon::Refresh,
                "Làm mới kiểm tra",
            )
            .clicked()
            {
                state.refresh_environment();
                state.notify_ok("Đã kiểm tra lại môi trường.");
            }
        });
        if trigger_xna {
            state.install_xna();
        }
    });

    ui.add_space(12.0);

    render_performance_card(state, assets.as_ref(), ui, p);

    ui.add_space(12.0);

    render_game_installer_card(state, assets.as_ref(), ui, p);

    ui.add_space(12.0);

    theme::rpg_card(ui, assets.as_ref(), p, |ui| {
        theme::rpg_section_heading(
            ui,
            assets.as_ref(),
            p,
            UiIcon::Folder,
            "Thư mục Game & Công cụ (để trống = dùng mặc định)",
        );
        ui.add_space(8.0);

        theme::rpg_subcard(ui, assets.as_ref(), p, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Thư mục Terraria:").color(p.text_muted));
                ui.add(
                    egui::TextEdit::singleline(&mut state.config.custom_terraria_dir)
                        .desired_width(300.0)
                        .hint_text(state.paths.terraria_dir.to_string_lossy().as_ref()),
                );
                if theme::rpg_compact_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Slate,
                    Some(UiIcon::Folder),
                    "Chọn...",
                )
                .clicked()
                {
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
                if theme::rpg_compact_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Slate,
                    Some(UiIcon::Folder),
                    "Chọn...",
                )
                .clicked()
                {
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
                if theme::rpg_compact_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Slate,
                    Some(UiIcon::Folder),
                    "Chọn...",
                )
                .clicked()
                {
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

    theme::rpg_card(ui, assets.as_ref(), p, |ui| {
        theme::rpg_section_heading(
            ui,
            assets.as_ref(),
            p,
            UiIcon::Host,
            "Cấu hình Tunnel & Proxy",
        );
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
        if theme::rpg_button(
            ui,
            assets.as_ref(),
            p,
            ButtonSkin::Emerald,
            UiIcon::Pack,
            "Lưu & Áp dụng",
        )
        .clicked()
        {
            state.save_config();
            state.refresh_environment();
            state.notify_ok("Đã lưu cài đặt và làm mới cấu hình.");
        }
    });
}

fn render_performance_card(
    state: &mut AppState,
    assets: &LauncherAssets,
    ui: &mut egui::Ui,
    p: ThemePalette,
) {
    theme::rpg_card(ui, assets, p, |ui| {
        ui.horizontal_wrapped(|ui| {
            theme::rpg_section_heading(
                ui,
                assets,
                p,
                UiIcon::Settings,
                "Hiệu năng & Đồ họa",
            );
            theme::status_pill(
                ui,
                &format!(
                    "Đồ họa: {} • CPU: {}",
                    state.config.tml_graphics_backend.label(),
                    state.config.game_process_priority.label()
                ),
                p.accent_cyan,
            );
        });
        ui.add_space(8.0);

        theme::rpg_subcard(ui, assets, p, |ui| {
            ui.label(
                RichText::new("Graphics Backend (tModLoader)")
                    .strong()
                    .color(p.text_primary),
            );
            ui.add_space(6.0);

            let current_backend = state.config.tml_graphics_backend;
            let mut next_backend = None;
            ui.horizontal_wrapped(|ui| {
                for backend in GraphicsBackend::ALL {
                    let active = current_backend == backend;
                    if theme::rpg_selectable_button(
                        ui,
                        assets,
                        p,
                        active,
                        ButtonSkin::Cyan,
                        UiIcon::Play,
                        backend.label(),
                    )
                    .clicked()
                        && !active
                    {
                        next_backend = Some(backend);
                    }
                }
            });
            if let Some(backend) = next_backend {
                state.config.tml_graphics_backend = backend;
                state.save_config();
            }

            ui.add_space(4.0);
            ui.label(
                RichText::new("Windows dùng card rời (RTX/GTX/RX) mặc định Tự động chọn Direct3D 11. Chỉ đổi sang Vulkan hoặc OpenGL nếu bị lỗi hiển thị hoặc tụt FPS bất thường.")
                    .size(12.0)
                    .color(p.text_muted),
            );
        });

        ui.add_space(8.0);

        theme::rpg_subcard(ui, assets, p, |ui| {
            ui.label(
                RichText::new("Tối ưu .NET 8 (tModLoader) & CPU")
                    .strong()
                    .color(p.text_primary),
            );
            ui.add_space(6.0);

            let pgo_changed = ui
                .checkbox(
                    &mut state.config.dotnet_tiered_pgo,
                    "Bật tối ưu JIT tăng tốc theo thời gian (Tiered PGO)",
                )
                .changed();
            let gc_changed = ui
                .checkbox(
                    &mut state.config.dotnet_server_gc,
                    "Dùng Server GC cho modpack nặng (tiêu tốn RAM hơn)",
                )
                .changed();
            if pgo_changed || gc_changed {
                state.save_config();
            }

            ui.add_space(6.0);
            let current_priority = state.config.game_process_priority;
            let mut next_priority = None;
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Ưu tiên CPU:").color(p.text_muted));
                for prio in ProcessPriority::ALL {
                    let active = current_priority == prio;
                    if theme::rpg_selectable_button(
                        ui,
                        assets,
                        p,
                        active,
                        ButtonSkin::Amber,
                        UiIcon::Settings,
                        prio.label(),
                    )
                    .clicked()
                        && !active
                    {
                        next_priority = Some(prio);
                    }
                }
            });
            if let Some(prio) = next_priority {
                state.config.game_process_priority = prio;
                state.save_config();
            }
        });
    });
}

fn render_game_installer_card(
    state: &mut AppState,
    assets: &LauncherAssets,
    ui: &mut egui::Ui,
    p: ThemePalette,
) {
    theme::rpg_card(ui, assets, p, |ui| {
        theme::rpg_section_heading(
            ui,
            assets,
            p,
            UiIcon::Download,
            "Cài đặt & Cập nhật Game",
        );
        ui.add_space(8.0);

        theme::rpg_subcard(ui, assets, p, |ui| {
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
                    .add_enabled_ui(!state.tml_releases_busy, |ui| {
                        theme::rpg_compact_button(
                            ui,
                            assets,
                            p,
                            ButtonSkin::Cyan,
                            Some(UiIcon::Refresh),
                            "Kiểm tra bản mới",
                        )
                    })
                    .inner
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
                    if theme::rpg_button(
                        ui,
                        assets,
                        p,
                        ButtonSkin::Cyan,
                        UiIcon::Download,
                        "Cài đặt / Cập nhật tModLoader",
                    )
                    .clicked()
                    {
                        do_install_tml = true;
                    }
                    let (dotnet_label, dotnet_icon) = if dotnet_missing {
                        ("Cài .NET 8 Portable", UiIcon::Download)
                    } else {
                        ("Cài lại .NET 8 Portable", UiIcon::Refresh)
                    };
                    if theme::rpg_button(
                        ui,
                        assets,
                        p,
                        ButtonSkin::Amber,
                        dotnet_icon,
                        dotnet_label,
                    )
                    .clicked()
                    {
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

        theme::rpg_subcard(ui, assets, p, |ui| {
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
                    .add_enabled_ui(!state.terraria_check_busy, |ui| {
                        theme::rpg_compact_button(
                            ui,
                            assets,
                            p,
                            ButtonSkin::Cyan,
                            Some(UiIcon::Refresh),
                            "Kiểm tra cập nhật",
                        )
                    })
                    .inner
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
            let (install_skin, install_icon) = if state.env.terraria_installed && is_synced {
                (ButtonSkin::Amber, UiIcon::Refresh)
            } else {
                (ButtonSkin::Cyan, UiIcon::Download)
            };
            let mut do_url = false;
            let mut do_zip = false;
            let mut do_save = false;
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled_ui(!state.install_busy, |ui| {
                    if theme::rpg_button(
                        ui,
                        assets,
                        p,
                        install_skin,
                        install_icon,
                        &install_btn_label,
                    )
                    .clicked()
                    {
                        do_url = true;
                    }
                    if theme::rpg_button(
                        ui,
                        assets,
                        p,
                        ButtonSkin::Slate,
                        UiIcon::Folder,
                        "Chọn file .zip trên máy...",
                    )
                    .clicked()
                    {
                        do_zip = true;
                    }
                });
                if theme::rpg_button(
                    ui,
                    assets,
                    p,
                    ButtonSkin::Emerald,
                    UiIcon::Pack,
                    "Lưu nguồn cài",
                )
                .clicked()
                {
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
            theme::rpg_subcard(ui, assets, p, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("Lỗi cài đặt: {err}")).color(p.accent_red));
                    if theme::rpg_compact_button(
                        ui,
                        assets,
                        p,
                        ButtonSkin::Red,
                        Some(UiIcon::Stop),
                        "Đóng",
                    )
                    .clicked()
                    {
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
