use crate::assets::{ButtonSkin, LauncherAssets, UiIcon};
use crate::state::AppState;
use crate::theme::ThemePalette;
use crate::views::play_view::render_install_progress_bar;
use crate::{anim3d, theme};
use egui::RichText;
use tl_core::{GraphicsBackend, ProcessPriority};
use tl_runtime::TmodLoaderReleaseInfo;

pub fn render(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    let busy = state.install_busy
        || state.steamcmd_busy
        || state.tml_releases_busy
        || state.terraria_check_busy;

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
    ui.add_space(10.0);

    render_components_and_installer_card(state, assets.as_ref(), ui, p);
    ui.add_space(12.0);
    render_performance_card(state, assets.as_ref(), ui, p);
    ui.add_space(12.0);
    render_paths_and_proxy_card(state, assets.as_ref(), ui, p);
}

fn render_components_and_installer_card(
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
            "Thành phần & Cài đặt Game",
        );
        ui.add_space(8.0);

        let dotnet_ok = state.env.dotnet8_version.is_some();
        let steamcmd_ok = state.env.steamcmd_path.is_some();
        let mut do_recheck = false;

        ui.horizontal_wrapped(|ui| {
            theme::status_pill(
                ui,
                if state.env.terraria_installed {
                    "Terraria: Đã cài"
                } else {
                    "Terraria: Chưa cài"
                },
                if state.env.terraria_installed {
                    p.accent_emerald
                } else {
                    p.accent_red
                },
            );
            theme::status_pill(
                ui,
                if state.env.tmodloader_installed {
                    "tModLoader: Đã cài"
                } else {
                    "tModLoader: Chưa cài"
                },
                if state.env.tmodloader_installed {
                    p.accent_emerald
                } else {
                    p.accent_red
                },
            );
            theme::status_pill(
                ui,
                if dotnet_ok {
                    ".NET 8: Sẵn sàng"
                } else {
                    ".NET 8: Chưa có"
                },
                if dotnet_ok {
                    p.accent_emerald
                } else {
                    p.accent_red
                },
            );
            theme::status_pill(
                ui,
                if state.env.xna_installed {
                    "XNA 4.0: Sẵn sàng"
                } else {
                    "XNA 4.0: Chưa cài"
                },
                if state.env.xna_installed {
                    p.accent_emerald
                } else {
                    p.accent_amber
                },
            );
            theme::status_pill(
                ui,
                if steamcmd_ok {
                    "SteamCMD: Sẵn sàng"
                } else {
                    "SteamCMD: Chưa cài"
                },
                if steamcmd_ok {
                    p.accent_emerald
                } else {
                    p.accent_amber
                },
            );

            if theme::rpg_compact_button(
                ui,
                assets,
                p,
                ButtonSkin::Slate,
                Some(UiIcon::Refresh),
                "Kiểm tra lại",
            )
            .clicked()
            {
                do_recheck = true;
            }
        });

        if do_recheck {
            state.refresh_environment();
            state.notify_ok("Đã kiểm tra lại trạng thái thành phần.");
        }

        if let Some(prog) = &state.install_progress {
            ui.add_space(8.0);
            render_install_progress_bar(ui, prog);
        }
        if let Some(prog) = &state.steamcmd_progress {
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

        ui.add_space(8.0);
        render_tml_installer_subcard(state, assets, ui, p);
        ui.add_space(8.0);
        render_terraria_installer_subcard(state, assets, ui, p);
    });
}

fn render_tml_installer_subcard(
    state: &mut AppState,
    assets: &LauncherAssets,
    ui: &mut egui::Ui,
    p: ThemePalette,
) {
    theme::rpg_subcard(ui, assets, p, |ui| {
        ui.label(RichText::new("tModLoader").strong().color(p.text_primary));
        ui.add_space(4.0);

        let selected_text = if state.tml_releases_busy {
            "Đang tải danh sách...".to_string()
        } else if state.tml_releases.is_empty() {
            "Bản ổn định mới nhất".to_string()
        } else {
            state
                .tml_releases
                .iter()
                .find(|r| r.tag_name == state.selected_tml_tag)
                .or_else(|| state.tml_releases.iter().find(|r| !r.prerelease))
                .or_else(|| state.tml_releases.first())
                .map(format_release_option)
                .unwrap_or_else(|| "Bản ổn định mới nhất".to_string())
        };

        let mut do_refresh_releases = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Phiên bản:").color(p.text_muted));
            egui::ComboBox::from_id_salt("settings_tml_release_combo")
                .selected_text(selected_text)
                .width(270.0)
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
            if state.tml_releases_busy {
                ui.spinner();
            }
        });
        if do_refresh_releases {
            state.refresh_tmodloader_releases();
        }

        ui.add_space(4.0);
        let dotnet_ok = state.env.dotnet8_version.is_some();
        let steamcmd_missing = state.env.steamcmd_path.is_none();
        let mut do_install_tml = false;
        let mut do_install_dotnet = false;
        let mut do_install_steamcmd = false;

        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!state.install_busy, |ui| {
                if theme::rpg_button(
                    ui,
                    assets,
                    p,
                    ButtonSkin::Emerald,
                    UiIcon::Download,
                    "Cài đặt / Cập nhật tModLoader",
                )
                .clicked()
                {
                    do_install_tml = true;
                }

                let (dotnet_skin, dotnet_icon, dotnet_label) = if !dotnet_ok {
                    (ButtonSkin::Amber, UiIcon::Download, "Cài .NET 8")
                } else {
                    (ButtonSkin::Slate, UiIcon::Refresh, "Cài lại .NET 8")
                };
                if theme::rpg_button(ui, assets, p, dotnet_skin, dotnet_icon, dotnet_label)
                    .clicked()
                {
                    do_install_dotnet = true;
                }
            });

            if steamcmd_missing {
                if ui
                    .add_enabled_ui(!state.steamcmd_busy, |ui| {
                        theme::rpg_button(
                            ui,
                            assets,
                            p,
                            ButtonSkin::Cyan,
                            UiIcon::Download,
                            "Cài SteamCMD",
                        )
                    })
                    .inner
                    .clicked()
                {
                    do_install_steamcmd = true;
                }
                if state.steamcmd_busy {
                    ui.spinner();
                }
            }
        });

        if do_install_tml {
            state.install_tmodloader_selected();
        }
        if do_install_dotnet {
            state.install_dotnet8();
        }
        if do_install_steamcmd {
            state.install_steamcmd();
        }
    });
}

fn render_terraria_installer_subcard(
    state: &mut AppState,
    assets: &LauncherAssets,
    ui: &mut egui::Ui,
    p: ThemePalette,
) {
    theme::rpg_subcard(ui, assets, p, |ui| {
        let is_synced = state.env.terraria_installed
            && state
                .terraria_remote_manifest
                .as_ref()
                .zip(state.terraria_installed_receipt.as_ref())
                .is_some_and(|(m, r)| {
                    !r.sha256.is_empty() && r.sha256.eq_ignore_ascii_case(&m.sha256)
                });

        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Terraria").strong().color(p.text_primary));
            if state.env.terraria_installed {
                if let Some(rec) = state
                    .terraria_installed_receipt
                    .as_ref()
                    .filter(|r| !r.version.trim().is_empty())
                {
                    theme::status_pill(
                        ui,
                        &format!("Đã cài: {}", rec.version),
                        p.accent_emerald,
                    );
                }
            }
            if let Some(m) = state
                .terraria_remote_manifest
                .as_ref()
                .filter(|m| !m.version.trim().is_empty())
            {
                let size_part = if m.total_size > 0 {
                    format!(" ({})", theme::format_bytes(m.total_size))
                } else {
                    String::new()
                };
                if is_synced {
                    theme::status_pill(ui, &format!("Đã đồng bộ{size_part}"), p.accent_cyan);
                } else {
                    theme::status_pill(
                        ui,
                        &format!("Nguồn tải: {}{}", m.version, size_part),
                        p.accent_amber,
                    );
                }
            }
        });

        ui.add_space(6.0);
        let mut do_url = false;
        let mut do_check_update = false;
        let mut do_zip = false;
        let mut do_xna = false;

        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!state.install_busy, |ui| {
                let primary_skin = if state.env.terraria_installed && is_synced {
                    ButtonSkin::Cyan
                } else {
                    ButtonSkin::Emerald
                };
                if theme::rpg_button(
                    ui,
                    assets,
                    p,
                    primary_skin,
                    UiIcon::Download,
                    "Tải & Cài Terraria",
                )
                .clicked()
                {
                    do_url = true;
                }
            });

            if ui
                .add_enabled_ui(!state.terraria_check_busy, |ui| {
                    theme::rpg_button(
                        ui,
                        assets,
                        p,
                        ButtonSkin::Cyan,
                        UiIcon::Refresh,
                        "Kiểm tra cập nhật",
                    )
                })
                .inner
                .clicked()
            {
                do_check_update = true;
            }
            if state.terraria_check_busy {
                ui.spinner();
            }

            ui.add_enabled_ui(!state.install_busy, |ui| {
                if theme::rpg_button(
                    ui,
                    assets,
                    p,
                    ButtonSkin::Slate,
                    UiIcon::Folder,
                    "Cài từ file .zip",
                )
                .clicked()
                {
                    do_zip = true;
                }

                if !state.env.xna_installed
                    && theme::rpg_button(
                        ui,
                        assets,
                        p,
                        ButtonSkin::Amber,
                        UiIcon::Download,
                        "Cài XNA Framework 4.0",
                    )
                    .clicked()
                {
                    do_xna = true;
                }
            });
        });

        ui.add_space(4.0);
        let mut config_dirty = false;
        let mut clear_remote_manifest = false;
        egui::CollapsingHeader::new("Nguồn tải nâng cao")
            .default_open(false)
            .show(ui, |ui| {
                egui::Grid::new("terraria_adv_source_grid")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Nguồn tải:").color(p.text_muted));
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut state.config.terraria_install_source)
                                .desired_width(340.0)
                                .hint_text("owner/repo#branch, owner/repo[@tag], hoặc URL .zip"),
                        );
                        if resp.changed() {
                            clear_remote_manifest = true;
                        }
                        if resp.lost_focus() {
                            config_dirty = true;
                        }
                        ui.end_row();

                        ui.label(RichText::new("GitHub Token:").color(p.text_muted));
                        let tok_resp = ui.add(
                            egui::TextEdit::singleline(&mut state.config.github_release_token)
                                .password(true)
                                .desired_width(260.0)
                                .hint_text("Tùy chọn (dùng cho kho lưu trữ riêng tư)"),
                        );
                        if tok_resp.lost_focus() {
                            config_dirty = true;
                        }
                        ui.end_row();
                    });
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
        if do_url {
            state.install_terraria_url();
        }
        if do_zip {
            state.pick_and_install_terraria_zip();
        }
        if do_xna {
            state.install_xna();
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
        theme::rpg_section_heading(ui, assets, p, UiIcon::Settings, "Hiệu năng & Đồ họa");
        ui.add_space(8.0);

        let current_backend = state.config.tml_graphics_backend;
        let mut next_backend = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Trình dựng hình (tModLoader):").color(p.text_muted));
            for backend in GraphicsBackend::ALL {
                let active = current_backend == backend;
                let skin = if active {
                    ButtonSkin::Emerald
                } else {
                    ButtonSkin::Slate
                };
                if theme::rpg_compact_button(ui, assets, p, skin, None, backend.label()).clicked()
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

        ui.add_space(6.0);
        let pgo_changed = ui
            .checkbox(
                &mut state.config.dotnet_tiered_pgo,
                "Tăng tốc biên dịch khi chơi lâu",
            )
            .changed();
        let gc_changed = ui
            .checkbox(
                &mut state.config.dotnet_server_gc,
                "Tối ưu bộ nhớ cho gói nhiều Mod (tốn thêm RAM)",
            )
            .changed();
        if pgo_changed || gc_changed {
            state.save_config();
        }

        ui.add_space(6.0);
        let current_priority = state.config.game_process_priority;
        let mut next_priority = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Mức ưu tiên tiến trình:").color(p.text_muted));
            for prio in ProcessPriority::ALL {
                let active = current_priority == prio;
                let skin = if active {
                    ButtonSkin::Emerald
                } else {
                    ButtonSkin::Slate
                };
                if theme::rpg_compact_button(ui, assets, p, skin, None, priority_clean_label(prio))
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
}

fn render_paths_and_proxy_card(
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
            UiIcon::Folder,
            "Thư mục & Kết nối Proxy",
        );
        ui.add_space(8.0);

        let default_terraria_hint = state.paths.terraria_dir.to_string_lossy().into_owned();
        let default_tml_hint = state.paths.tmodloader_dir.to_string_lossy().into_owned();
        let input_w = (ui.available_width() - 330.0).max(210.0);
        let pressed_enter = ui.input(|i| i.key_pressed(egui::Key::Enter));

        let mut auto_apply = false;
        let mut explicit_apply = false;

        egui::Grid::new("settings_paths_proxy_grid")
            .num_columns(2)
            .spacing([14.0, 8.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Thư mục Terraria:").color(p.text_muted));
                ui.horizontal(|ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut state.config.custom_terraria_dir)
                            .desired_width(input_w)
                            .hint_text(&default_terraria_hint),
                    );
                    if resp.lost_focus() {
                        if pressed_enter {
                            explicit_apply = true;
                        } else {
                            auto_apply = true;
                        }
                    }
                    if theme::rpg_compact_button(
                        ui,
                        assets,
                        p,
                        ButtonSkin::Slate,
                        Some(UiIcon::Folder),
                        "Chọn...",
                    )
                    .clicked()
                    {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            state.config.custom_terraria_dir = dir.to_string_lossy().into_owned();
                            auto_apply = true;
                        }
                    }
                    let has_custom = !state.config.custom_terraria_dir.trim().is_empty();
                    if ui
                        .add_enabled_ui(has_custom, |ui| {
                            theme::rpg_compact_button(
                                ui,
                                assets,
                                p,
                                ButtonSkin::Slate,
                                None,
                                "Mặc định",
                            )
                        })
                        .inner
                        .clicked()
                    {
                        state.config.custom_terraria_dir.clear();
                        auto_apply = true;
                    }
                });
                ui.end_row();

                ui.label(RichText::new("Thư mục tModLoader:").color(p.text_muted));
                ui.horizontal(|ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut state.config.custom_tmodloader_dir)
                            .desired_width(input_w)
                            .hint_text(&default_tml_hint),
                    );
                    if resp.lost_focus() {
                        if pressed_enter {
                            explicit_apply = true;
                        } else {
                            auto_apply = true;
                        }
                    }
                    if theme::rpg_compact_button(
                        ui,
                        assets,
                        p,
                        ButtonSkin::Slate,
                        Some(UiIcon::Folder),
                        "Chọn...",
                    )
                    .clicked()
                    {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            state.config.custom_tmodloader_dir = dir.to_string_lossy().into_owned();
                            auto_apply = true;
                        }
                    }
                    let has_custom = !state.config.custom_tmodloader_dir.trim().is_empty();
                    if ui
                        .add_enabled_ui(has_custom, |ui| {
                            theme::rpg_compact_button(
                                ui,
                                assets,
                                p,
                                ButtonSkin::Slate,
                                None,
                                "Mặc định",
                            )
                        })
                        .inner
                        .clicked()
                    {
                        state.config.custom_tmodloader_dir.clear();
                        auto_apply = true;
                    }
                });
                ui.end_row();

                ui.label(RichText::new("Công cụ SteamCMD:").color(p.text_muted));
                ui.horizontal(|ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut state.config.custom_steamcmd_path)
                            .desired_width(input_w)
                            .hint_text("Mặc định (tự nhận diện)"),
                    );
                    if resp.lost_focus() {
                        if pressed_enter {
                            explicit_apply = true;
                        } else {
                            auto_apply = true;
                        }
                    }
                    if theme::rpg_compact_button(
                        ui,
                        assets,
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
                            auto_apply = true;
                        }
                    }
                    let has_custom = !state.config.custom_steamcmd_path.trim().is_empty();
                    if ui
                        .add_enabled_ui(has_custom, |ui| {
                            theme::rpg_compact_button(
                                ui,
                                assets,
                                p,
                                ButtonSkin::Slate,
                                None,
                                "Mặc định",
                            )
                        })
                        .inner
                        .clicked()
                    {
                        state.config.custom_steamcmd_path.clear();
                        auto_apply = true;
                    }
                });
                ui.end_row();

                ui.label(RichText::new("Proxy mạng:").color(p.text_muted));
                ui.horizontal(|ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut state.config.proxy_url)
                            .desired_width(input_w)
                            .hint_text("http://127.0.0.1:8080 hoặc socks5://127.0.0.1:1080"),
                    );
                    if resp.lost_focus() {
                        if pressed_enter {
                            explicit_apply = true;
                        } else {
                            auto_apply = true;
                        }
                    }
                    let has_proxy = !state.config.proxy_url.trim().is_empty();
                    if ui
                        .add_enabled_ui(has_proxy, |ui| {
                            theme::rpg_compact_button(
                                ui,
                                assets,
                                p,
                                ButtonSkin::Slate,
                                None,
                                "Mặc định",
                            )
                        })
                        .inner
                        .clicked()
                    {
                        state.config.proxy_url.clear();
                        auto_apply = true;
                    }
                });
                ui.end_row();
            });

        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            if theme::rpg_button(
                ui,
                assets,
                p,
                ButtonSkin::Emerald,
                UiIcon::Pack,
                "Lưu & Áp dụng",
            )
            .clicked()
            {
                explicit_apply = true;
            }
            if theme::rpg_button(
                ui,
                assets,
                p,
                ButtonSkin::Slate,
                UiIcon::Folder,
                "Mở thư mục Launcher",
            )
            .clicked()
            {
                let _ = open::that(&state.paths.base_dir);
            }
        });

        if explicit_apply {
            state.save_config();
            state.refresh_environment();
            state.notify_ok("Đã lưu và áp dụng cài đặt.");
        } else if auto_apply {
            state.save_config();
            state.refresh_environment();
        }
    });
}

fn priority_clean_label(prio: ProcessPriority) -> &'static str {
    let raw = prio.label();
    if raw.contains("Cao hơn") || raw.contains("Trên") || raw.contains("Above") {
        "Cao hơn"
    } else if raw.contains("Cao") || raw.contains("High") {
        "Cao"
    } else {
        "Bình thường"
    }
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
