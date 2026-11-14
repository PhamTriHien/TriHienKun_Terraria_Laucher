use crate::anim3d;
use crate::assets::{ButtonSkin, LauncherAssets, UiIcon};
use crate::state::{AppState, ModsSubTab, NavTab};
use crate::theme::{
    console_frame, format_bytes, page_title, rpg_button, rpg_card, rpg_compact_button,
    rpg_section_heading, rpg_selectable_button, rpg_subcard, status_pill, ui_font, ThemePalette,
};
use egui::RichText;
use tl_core::{ModPackInfo, SharedModSource};
use tl_runtime::{
    format_unix_timestamp, parse_workshop_id, strip_steam_bbcode, ModPackExportEntry,
    ModPackImportPreview, ModPackSharePreview, ModPackSyncProgress, ModPackSyncReport,
    StagedWorkshopMod, WorkshopItemDetails, WorkshopSort, CURATED_GITHUB_MODS,
};

pub fn render(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    let busy = state.workshop_browse_busy
        || state.workshop_busy
        || state.github_busy
        || state.modpack_sync_busy;

    ui.horizontal(|ui| {
        anim3d::render_mods_scanner_3d(ui, p, assets.nav_mods.as_ref(), busy);
        ui.vertical(|ui| {
            page_title(ui, p, "Quản lý Mod & ModPack");
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                for (sub, icon) in [
                    (ModsSubTab::Installed, UiIcon::Pack),
                    (ModsSubTab::GitHub, UiIcon::Download),
                    (ModsSubTab::ModPacks, UiIcon::Key),
                    (ModsSubTab::Workshop, UiIcon::Workshop),
                ] {
                    let active = state.mods_tab == sub;
                    if rpg_selectable_button(
                        ui,
                        assets.as_ref(),
                        p,
                        active,
                        ButtonSkin::Emerald,
                        icon,
                        sub.label(),
                    )
                    .clicked()
                    {
                        state.mods_tab = sub;
                    }
                }
            });
        });
    });
    ui.add_space(10.0);

    match state.mods_tab {
        ModsSubTab::Installed => render_installed_tab(state, ui),
        ModsSubTab::GitHub => render_github_tab(state, ui),
        ModsSubTab::ModPacks => render_modpacks_tab(state, ui),
        ModsSubTab::Workshop => render_workshop_tab(state, ui),
    }
}

fn render_installed_tab(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    rpg_card(ui, assets.as_ref(), p, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.mod_search)
                    .desired_width(210.0)
                    .hint_text("Tìm mod theo tên hoặc tác giả..."),
            );
            if rpg_button(
                ui,
                assets.as_ref(),
                p,
                ButtonSkin::Emerald,
                UiIcon::Download,
                "Nhập .tmod / .zip",
            )
            .clicked()
            {
                state.import_mods_dialog();
            }
            if rpg_button(
                ui,
                assets.as_ref(),
                p,
                ButtonSkin::Slate,
                UiIcon::Folder,
                "Mở thư mục Mods",
            )
            .clicked()
            {
                let _ = open::that(state.paths.tml_mods_dir());
            }
            if rpg_button(
                ui,
                assets.as_ref(),
                p,
                ButtonSkin::Slate,
                UiIcon::Refresh,
                "Quét lại",
            )
            .clicked()
            {
                state.refresh_mods_and_packs();
            }
            let enabled_count = state.installed_mods.iter().filter(|m| m.enabled).count();
            status_pill(
                ui,
                &format!("{enabled_count}/{} đang bật", state.installed_mods.len()),
                p.accent_emerald,
            );
        });

        ui.add_space(8.0);
        if state.installed_mods.is_empty() {
            rpg_subcard(ui, assets.as_ref(), p, |ui| {
                ui.label(
                    RichText::new("Chưa có file .tmod nào. Bạn có thể kéo thả file .tmod/.zip vào cửa sổ hoặc tải từ GitHub / Workshop.")
                        .color(p.text_muted),
                );
            });
            return;
        }

        let query = state.mod_search.trim().to_lowercase();
        let mods = state.installed_mods.clone();
        let mut toggle_action: Option<(String, bool)> = None;
        let mut delete_action: Option<(std::path::PathBuf, String)> = None;

        for entry in &mods {
            if !query.is_empty()
                && !entry.meta.display_name.to_lowercase().contains(&query)
                && !entry.meta.internal_name.to_lowercase().contains(&query)
                && !entry.meta.author.to_lowercase().contains(&query)
            {
                continue;
            }

            let tex = state.mod_icon_texture(ui.ctx(), &entry.meta);
            rpg_subcard(ui, assets.as_ref(), p, |ui| {
                ui.horizontal(|ui| {
                    render_thumbnail_box(ui, p, tex, 44.0, None);

                    ui.vertical(|ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&entry.meta.display_name).strong().size(14.5));
                            status_pill(ui, &format!("v{}", entry.meta.version), p.accent_cyan);
                            status_pill(ui, entry.meta.side.label(), p.accent_amber);
                            if entry.enabled {
                                status_pill(ui, "Đang bật", p.accent_emerald);
                            } else {
                                status_pill(ui, "Đã tắt", p.text_muted);
                            }
                            if !entry.missing_deps.is_empty() {
                                status_pill(
                                    ui,
                                    &format!("Thiếu: {}", entry.missing_deps.join(", ")),
                                    p.accent_red,
                                );
                            }
                        });
                        ui.label(
                            RichText::new(format!(
                                "{}  •  Tác giả: {}  •  {}",
                                entry.meta.internal_name,
                                if entry.meta.author.is_empty() {
                                    "Không rõ"
                                } else {
                                    &entry.meta.author
                                },
                                format_bytes(entry.meta.file_size)
                            ))
                            .size(12.0)
                            .color(p.text_muted),
                        );
                    });

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
                            delete_action = Some((
                                entry.meta.file_path.clone(),
                                entry.meta.internal_name.clone(),
                            ));
                        }
                        let (btn_skin, btn_icon, btn_text) = if entry.enabled {
                            (ButtonSkin::Slate, UiIcon::Stop, "Tắt")
                        } else {
                            (ButtonSkin::Emerald, UiIcon::Play, "Bật")
                        };
                        if rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            btn_skin,
                            Some(btn_icon),
                            btn_text,
                        )
                        .clicked()
                        {
                            toggle_action = Some((entry.meta.internal_name.clone(), !entry.enabled));
                        }
                    });
                });
            });
            ui.add_space(5.0);
        }

        if let Some((name, enable)) = toggle_action {
            state.toggle_mod(&name, enable);
        }
        if let Some((path, name)) = delete_action {
            state.delete_mod(&path, &name);
        }
    });
}

fn render_github_tab(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    rpg_card(ui, assets.as_ref(), p, |ui| {
        rpg_section_heading(
            ui,
            assets.as_ref(),
            p,
            UiIcon::Download,
            "Nguồn GitHub Release phổ biến",
        );
        ui.add_space(6.0);

        let mut clicked_repo: Option<String> = None;
        for src in CURATED_GITHUB_MODS {
            rpg_subcard(ui, assets.as_ref(), p, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(src.name).strong().size(14.0));
                        ui.label(RichText::new(src.summary).size(12.0).color(p.text_muted));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled_ui(!state.github_busy, |ui| {
                                rpg_compact_button(
                                    ui,
                                    assets.as_ref(),
                                    p,
                                    ButtonSkin::Cyan,
                                    Some(UiIcon::Refresh),
                                    "Tìm bản mới",
                                )
                            })
                            .inner
                            .clicked()
                        {
                            clicked_repo = Some(src.repo.to_string());
                        }
                    });
                });
            });
            ui.add_space(4.0);
        }
        if let Some(repo) = clicked_repo {
            state.fetch_github_repo(&repo);
        }

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Repo GitHub:").color(p.text_muted));
            ui.add(
                egui::TextEdit::singleline(&mut state.github_repo_input)
                    .desired_width(240.0)
                    .hint_text("owner/repo"),
            );
            if ui
                .add_enabled_ui(!state.github_busy, |ui| {
                    rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Cyan,
                        UiIcon::Download,
                        "Lấy Release .tmod",
                    )
                })
                .inner
                .clicked()
            {
                let r = state.github_repo_input.clone();
                state.fetch_github_repo(&r);
            }
            if state.github_busy {
                ui.spinner();
            }
        });

        if !state.github_assets.is_empty() {
            ui.add_space(8.0);
            let github_assets = state.github_assets.clone();
            for asset in github_assets {
                rpg_subcard(ui, assets.as_ref(), p, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&asset.asset_name).strong());
                        status_pill(ui, &asset.tag_name, p.accent_cyan);
                        ui.label(
                            RichText::new(format_bytes(asset.size_bytes))
                                .size(12.0)
                                .color(p.text_muted),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled_ui(!state.github_busy, |ui| {
                                    rpg_compact_button(
                                        ui,
                                        assets.as_ref(),
                                        p,
                                        ButtonSkin::Emerald,
                                        Some(UiIcon::Download),
                                        "Tải & Cài đặt",
                                    )
                                })
                                .inner
                                .clicked()
                            {
                                state.download_mod_url(
                                    &asset.download_url,
                                    Some(asset.asset_name.clone()),
                                    Some(asset.repo.clone()),
                                );
                            }
                        });
                    });
                });
                ui.add_space(4.0);
            }
        }
    });

    ui.add_space(12.0);

    rpg_card(ui, assets.as_ref(), p, |ui| {
        rpg_section_heading(
            ui,
            assets.as_ref(),
            p,
            UiIcon::Download,
            "Tải trực tiếp từ URL (.tmod)",
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.direct_mod_url)
                    .desired_width((ui.available_width() - 130.0).max(160.0))
                    .hint_text("https://.../TenMod.tmod"),
            );
            if ui
                .add_enabled_ui(!state.github_busy, |ui| {
                    rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Emerald,
                        UiIcon::Download,
                        "Tải về",
                    )
                })
                .inner
                .clicked()
            {
                let url = state.direct_mod_url.clone();
                state.download_mod_url(&url, None, None);
            }
        });
    });
}

fn render_modpacks_tab(state: &mut AppState, ui: &mut egui::Ui) {
    render_modpack_share_import_card(state, ui);
    ui.add_space(12.0);
    render_modpack_create_card(state, ui);

    if let Some(preview) = state.share_export_preview.clone() {
        ui.add_space(12.0);
        render_modpack_share_export_card(state, ui, &preview);
    }

    ui.add_space(12.0);
    render_modpack_list_card(state, ui);

    if state.modpack_sync_busy || state.modpack_sync_report.is_some() || state.steamcmd_busy {
        ui.add_space(12.0);
        render_workshop_logs_card(state, ui);
    }
}

fn render_modpack_share_import_card(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    rpg_card(ui, assets.as_ref(), p, |ui| {
        rpg_section_heading(
            ui,
            assets.as_ref(),
            p,
            UiIcon::Key,
            "Đồng bộ ModPack từ Share Key",
        );
        ui.add_space(6.0);

        let mut trigger_check = false;
        let mut trigger_clear = false;
        let has_key_input = !state.share_key_input.trim().is_empty();

        ui.horizontal_wrapped(|ui| {
            let key_resp = ui.add(
                egui::TextEdit::singleline(&mut state.share_key_input)
                    .desired_width((ui.available_width() - 240.0).max(200.0))
                    .hint_text("Dán Share Key (TLPACK1:...) từ bạn bè..."),
            );
            if key_resp.changed() {
                trigger_check = true;
            }
            if rpg_button(
                ui,
                assets.as_ref(),
                p,
                ButtonSkin::Cyan,
                UiIcon::Key,
                "Kiểm tra Key",
            )
            .clicked()
            {
                trigger_check = true;
            }
            if has_key_input
                && rpg_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Red,
                    UiIcon::Trash,
                    "Xóa",
                )
                .clicked()
            {
                trigger_clear = true;
            }
        });

        if trigger_clear {
            state.clear_share_key_import();
        } else if trigger_check {
            state.update_share_key_input();
        }

        if let Some(preview_res) = state.share_import_preview.clone() {
            ui.add_space(8.0);
            match preview_res {
                Err(err) => {
                    ui.label(RichText::new(err).color(p.accent_red));
                }
                Ok(preview) => {
                    render_share_import_preview(state, ui, &preview);
                }
            }
        }

        if let Some(prog) = state.modpack_sync_progress.clone() {
            ui.add_space(8.0);
            render_share_sync_progress(ui, p, &prog);
        }

        if let Some(report) = state.modpack_sync_report.clone() {
            ui.add_space(8.0);
            render_share_sync_report(ui, assets.as_ref(), p, &report);
        }
    });
}

fn render_share_import_preview(
    state: &mut AppState,
    ui: &mut egui::Ui,
    preview: &ModPackImportPreview,
) {
    let p = state.palette();
    let assets = state.assets.clone();
    rpg_subcard(ui, assets.as_ref(), p, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Tên ModPack mới:").color(p.text_muted));
            ui.add(
                egui::TextEdit::singleline(&mut state.share_import_pack_name)
                    .desired_width(220.0)
                    .hint_text("Tên ModPack riêng..."),
            );
        });

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            status_pill(
                ui,
                &format!("{} mod", preview.payload.mods.len()),
                p.accent_cyan,
            );
            if preview.already_present_count > 0 {
                status_pill(
                    ui,
                    &format!("Có sẵn {}", preview.already_present_count),
                    p.accent_emerald,
                );
            }
            if preview.workshop_download_count > 0 {
                status_pill(
                    ui,
                    &format!("Tải Workshop {}", preview.workshop_download_count),
                    p.accent_cyan,
                );
            }
            if preview.url_download_count > 0 {
                status_pill(
                    ui,
                    &format!("Tải URL {}", preview.url_download_count),
                    p.accent_cyan,
                );
            }
            if preview.manual_only_count > 0 {
                status_pill(
                    ui,
                    &format!("Cài tay {}", preview.manual_only_count),
                    p.accent_amber,
                );
            }
        });

        if preview.workshop_download_count > 0 && state.env.steamcmd_path.is_none() {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Cần SteamCMD để tải mod Workshop.").color(p.accent_amber));
                if ui
                    .add_enabled_ui(!state.steamcmd_busy, |ui| {
                        rpg_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Cyan,
                            UiIcon::Download,
                            "Cài SteamCMD tự động",
                        )
                    })
                    .inner
                    .clicked()
                {
                    state.install_steamcmd();
                }
                if state.steamcmd_busy {
                    ui.spinner();
                }
            });
            if let Some(prog) = &state.steamcmd_progress {
                ui.add_space(4.0);
                crate::views::play_view::render_install_progress_bar(ui, prog);
            }
        }

        ui.add_space(8.0);
        let can_sync = !state.modpack_sync_busy
            && !state.share_import_pack_name.trim().is_empty()
            && (preview.workshop_download_count == 0 || state.env.steamcmd_path.is_some());
        if ui
            .add_enabled_ui(can_sync, |ui| {
                rpg_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Emerald,
                    UiIcon::Download,
                    "Tải đồng bộ & Kích hoạt",
                )
            })
            .inner
            .clicked()
        {
            state.sync_modpack_from_key();
        }

        if !preview.payload.mods.is_empty() {
            ui.add_space(6.0);
            egui::CollapsingHeader::new("Chi tiết mod trong Share Key").show(ui, |ui| {
                for entry in &preview.payload.mods {
                    let installed = preview.installed_matches.contains(&entry.internal_name);
                    let display = if entry.display_name.is_empty() {
                        &entry.internal_name
                    } else {
                        &entry.display_name
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(display).strong().size(13.0));
                        if !entry.version.is_empty() {
                            ui.label(
                                RichText::new(format!("v{}", entry.version))
                                    .size(12.0)
                                    .color(p.text_muted),
                            );
                        }
                        ui.label(
                            RichText::new(format_shared_source_label(&entry.source))
                                .size(12.0)
                                .color(p.text_muted),
                        );
                        if installed {
                            status_pill(ui, "Đã có", p.accent_emerald);
                        } else {
                            let color = if matches!(entry.source, SharedModSource::ManualOnly) {
                                p.accent_amber
                            } else {
                                p.accent_cyan
                            };
                            status_pill(ui, "Cần tải", color);
                        }
                    });
                }
            });
        }
    });
}

fn render_share_sync_progress(ui: &mut egui::Ui, p: ThemePalette, prog: &ModPackSyncProgress) {
    let frac = if prog.total == 0 {
        0.0
    } else {
        (prog.completed as f32 / prog.total as f32).clamp(0.0, 1.0)
    };
    ui.add(egui::ProgressBar::new(frac).text(format!(
        "{}/{}  —  {}",
        prog.completed, prog.total, prog.stage
    )));
    if !prog.current_mod.is_empty() {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(
                RichText::new(format!("Đang xử lý: {}", prog.current_mod))
                    .size(12.0)
                    .color(p.text_muted),
            );
        });
    }
}

fn render_share_sync_report(
    ui: &mut egui::Ui,
    assets: &LauncherAssets,
    p: ThemePalette,
    report: &ModPackSyncReport,
) {
    rpg_subcard(ui, assets, p, |ui| {
        ui.label(
            RichText::new(format!(
                "Đã tạo và kích hoạt ModPack '{}' ({} mod).",
                report.pack_name, report.enabled_count
            ))
            .strong()
            .color(p.accent_emerald),
        );
        if let Some(backup) = &report.backup_pack_name {
            ui.label(
                RichText::new(format!(
                    "Đã sao lưu danh sách mod cũ thành ModPack '{backup}'."
                ))
                .color(p.accent_cyan),
            );
        }
        if !report.missing_manual.is_empty() {
            ui.label(
                RichText::new(format!(
                    "Mod thiếu file .tmod cần chép thủ công: {}",
                    report.missing_manual.join(", ")
                ))
                .color(p.accent_amber),
            );
        }
        if !report.failed_mods.is_empty() {
            for (name, reason) in &report.failed_mods {
                ui.label(RichText::new(format!("Lỗi tải {name}: {reason}")).color(p.accent_red));
            }
        }
    });
}

fn render_modpack_create_card(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    rpg_card(ui, assets.as_ref(), p, |ui| {
        rpg_section_heading(
            ui,
            assets.as_ref(),
            p,
            UiIcon::Pack,
            "Tạo & Chia sẻ ModPack từ danh sách mod đang bật",
        );
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.new_modpack_name)
                    .desired_width(220.0)
                    .hint_text("Tên ModPack mới..."),
            );
            if rpg_button(
                ui,
                assets.as_ref(),
                p,
                ButtonSkin::Emerald,
                UiIcon::Pack,
                "Tạo ModPack",
            )
            .clicked()
            {
                state.create_new_modpack();
            }
            if rpg_button(
                ui,
                assets.as_ref(),
                p,
                ButtonSkin::Cyan,
                UiIcon::Key,
                "Tạo & Lấy Share Key",
            )
            .clicked()
            {
                state.create_modpack_and_share();
            }
            if rpg_button(
                ui,
                assets.as_ref(),
                p,
                ButtonSkin::Slate,
                UiIcon::Key,
                "Lấy Share Key (mod đang bật)",
            )
            .clicked()
            {
                state.prepare_share_key_from_enabled();
            }
        });
    });
}

fn render_modpack_share_export_card(
    state: &mut AppState,
    ui: &mut egui::Ui,
    preview: &ModPackSharePreview,
) {
    let p = state.palette();
    let assets = state.assets.clone();
    rpg_card(ui, assets.as_ref(), p, |ui| {
        ui.horizontal(|ui| {
            rpg_section_heading(
                ui,
                assets.as_ref(),
                p,
                UiIcon::Key,
                &format!("Share Key — {}", preview.pack_name),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if rpg_compact_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Slate,
                    Some(UiIcon::Stop),
                    "Đóng",
                )
                .clicked()
                {
                    state.close_share_export();
                }
                if rpg_compact_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Cyan,
                    Some(UiIcon::Copy),
                    "Copy Share Key",
                )
                .clicked()
                {
                    ui.ctx().copy_text(preview.share_key.clone());
                    state.notify_ok("Đã sao chép Share Key vào clipboard.");
                }
            });
        });

        ui.add_space(6.0);
        let mut key_buf = preview.share_key.clone();
        ui.add(
            egui::TextEdit::multiline(&mut key_buf)
                .desired_rows(3)
                .desired_width(ui.available_width())
                .font(egui::TextStyle::Monospace),
        );

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            status_pill(
                ui,
                &format!("{} Workshop", preview.workshop_count),
                p.accent_emerald,
            );
            status_pill(
                ui,
                &format!("{} GitHub/URL", preview.url_count),
                p.accent_cyan,
            );
            if preview.manual_count > 0 {
                status_pill(
                    ui,
                    &format!("{} chưa có nguồn", preview.manual_count),
                    p.accent_amber,
                );
            }
        });

        let manual_entries: Vec<&ModPackExportEntry> = preview
            .entries
            .iter()
            .filter(|e| matches!(e.source, SharedModSource::ManualOnly))
            .collect();
        if !manual_entries.is_empty() {
            ui.add_space(8.0);
            ui.label(
                RichText::new("Mod chưa có ID / nguồn tải (gắn thêm để tích hợp vào Share Key)")
                    .strong()
                    .color(p.accent_amber),
            );
            ui.add_space(4.0);
            for entry in manual_entries {
                ui.push_id(("manual_src", &entry.internal_name), |ui| {
                    render_modpack_source_editor_row(state, ui, entry);
                });
                ui.add_space(4.0);
            }
        }

        if !preview.entries.is_empty() {
            ui.add_space(6.0);
            egui::CollapsingHeader::new("Danh sách nguồn mod trong gói").show(ui, |ui| {
                for entry in &preview.entries {
                    ui.push_id(("all_src", &entry.internal_name), |ui| {
                        render_modpack_source_editor_row(state, ui, entry);
                    });
                    ui.add_space(4.0);
                }
            });
        }
    });
}

fn render_modpack_source_editor_row(
    state: &mut AppState,
    ui: &mut egui::Ui,
    entry: &ModPackExportEntry,
) {
    let p = state.palette();
    let assets = state.assets.clone();
    let display = if entry.display_name.is_empty() {
        entry.internal_name.as_str()
    } else {
        entry.display_name.as_str()
    };
    let mut save_raw: Option<String> = None;
    let mut search_clicked = false;
    let mut chosen_candidate_id: Option<String> = None;

    rpg_subcard(ui, assets.as_ref(), p, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(display).strong());
            if !entry.display_name.is_empty() && entry.display_name != entry.internal_name {
                ui.label(
                    RichText::new(format!("({})", entry.internal_name))
                        .size(12.0)
                        .color(p.text_muted),
                );
            }
            if !entry.version.is_empty() {
                ui.label(
                    RichText::new(format!("v{}", entry.version))
                        .size(12.0)
                        .color(p.text_muted),
                );
            }
            let (pill_text, pill_color) = match &entry.source {
                SharedModSource::ManualOnly => ("Chưa có nguồn".to_string(), p.accent_amber),
                other => (format_shared_source_label(other), p.accent_cyan),
            };
            status_pill(ui, &pill_text, pill_color);
        });

        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let input_width = (ui.available_width() - 290.0).max(180.0);
            let buf = state
                .unlinked_source_inputs
                .entry(entry.internal_name.clone())
                .or_default();
            let edit_resp = ui.add(
                egui::TextEdit::singleline(buf)
                    .desired_width(input_width)
                    .hint_text("Workshop ID, link Steam, owner/repo GitHub hoặc link .tmod..."),
            );
            let current_input = buf.clone();
            let submit_enter =
                edit_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if rpg_button(
                ui,
                assets.as_ref(),
                p,
                ButtonSkin::Cyan,
                UiIcon::Pack,
                "Lưu nguồn",
            )
            .clicked()
                || submit_enter
            {
                save_raw = Some(current_input);
            }
            if ui
                .add_enabled_ui(!state.workshop_candidates_busy, |ui| {
                    rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Slate,
                        UiIcon::Workshop,
                        "Tìm Workshop",
                    )
                })
                .inner
                .clicked()
            {
                search_clicked = true;
            }
        });

        if state.workshop_candidate_target.as_deref() == Some(entry.internal_name.as_str()) {
            ui.add_space(6.0);
            if state.workshop_candidates_busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Đang tìm trên Steam Workshop...").color(p.text_muted));
                });
            } else if state.workshop_candidates.is_empty() {
                ui.label(
                    RichText::new("Không tìm thấy kết quả phù hợp trên Steam Workshop.")
                        .size(12.0)
                        .color(p.text_muted),
                );
            } else {
                let candidates: Vec<WorkshopItemDetails> =
                    state.workshop_candidates.iter().take(8).cloned().collect();
                for cand in &candidates {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(&cand.title).strong().size(12.5));
                        if !cand.author.is_empty() {
                            ui.label(
                                RichText::new(format!("— {}", cand.author))
                                    .size(12.0)
                                    .color(p.text_muted),
                            );
                        }
                        status_pill(ui, &format!("ID {}", cand.published_file_id), p.accent_cyan);
                        if rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Emerald,
                            Some(UiIcon::Play),
                            "Chọn",
                        )
                        .clicked()
                        {
                            chosen_candidate_id = Some(cand.published_file_id.clone());
                        }
                    });
                }
            }
        }
    });

    if let Some(id) = chosen_candidate_id {
        state.save_manual_mod_source(&entry.internal_name, display, &entry.version, &id);
    } else if let Some(raw) = save_raw {
        state.save_manual_mod_source(&entry.internal_name, display, &entry.version, &raw);
    } else if search_clicked {
        state.search_mod_workshop_candidates(&entry.internal_name, display);
    }
}

fn render_modpack_list_card(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    rpg_card(ui, assets.as_ref(), p, |ui| {
        rpg_section_heading(ui, assets.as_ref(), p, UiIcon::Pack, "Danh sách ModPack");
        ui.add_space(6.0);

        if state.modpacks.is_empty() {
            ui.label(
                RichText::new("Chưa có ModPack nào trong Saves\\tModLoader\\ModPacks.")
                    .color(p.text_muted),
            );
            return;
        }

        let packs = state.modpacks.clone();
        for pack in &packs {
            render_modpack_row(state, ui, pack);
            ui.add_space(4.0);
        }
    });
}

fn render_modpack_row(state: &mut AppState, ui: &mut egui::Ui, pack: &ModPackInfo) {
    let p = state.palette();
    let assets = state.assets.clone();
    let is_active = state.config.active_modpack == pack.name;
    rpg_subcard(ui, assets.as_ref(), p, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(&pack.name).strong().size(14.5));
            status_pill(ui, &format!("{} mod", pack.enabled_mods.len()), p.accent_cyan);
            if pack.local_tmod_count > 0 {
                status_pill(
                    ui,
                    &format!("{} file .tmod", pack.local_tmod_count),
                    p.accent_emerald,
                );
            }
            if is_active {
                status_pill(ui, "Đang chọn", p.accent_emerald);
            }
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
                    state.remove_modpack(pack);
                }
                if rpg_compact_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Emerald,
                    Some(UiIcon::Play),
                    "Kích hoạt",
                )
                .clicked()
                {
                    state.activate_selected_modpack(pack);
                }
                if rpg_compact_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Cyan,
                    Some(UiIcon::Key),
                    "Share Key",
                )
                .clicked()
                {
                    state.prepare_share_key_for_pack(pack);
                }
            });
        });
    });
}

fn format_shared_source_label(source: &SharedModSource) -> String {
    match source {
        SharedModSource::Workshop { id } => format!("Workshop ID {id}"),
        SharedModSource::Github { repo, url } => {
            if !repo.is_empty() {
                format!("GitHub: {repo}")
            } else {
                format!("URL: {url}")
            }
        }
        SharedModSource::Url { url } => format!("URL: {url}"),
        SharedModSource::ManualOnly => "Chưa có nguồn tải".to_string(),
    }
}

enum CatalogAction {
    Download(WorkshopItemDetails),
    Inspect(String),
    OpenSteam(String),
}

enum StagedAction {
    Install(Box<StagedWorkshopMod>),
    Toggle(String, bool),
    Remove(String),
}

fn render_workshop_tab(state: &mut AppState, ui: &mut egui::Ui) {
    if !state.workshop_initial_loaded
        && !state.workshop_browse_busy
        && state.workshop_browse_error.is_none()
    {
        state.browse_workshop(1);
    }

    render_workshop_search_card(state, ui);

    if let Some(details) = state.workshop_details.clone() {
        ui.add_space(12.0);
        render_workshop_detail_card(state, ui, &details);
    }

    ui.add_space(12.0);
    render_workshop_catalog_card(state, ui);

    ui.add_space(12.0);
    render_staged_workshop_section(state, ui);

    if !state.workshop_logs.is_empty() || state.workshop_busy || state.steamcmd_busy {
        ui.add_space(12.0);
        render_workshop_logs_card(state, ui);
    }
}

fn render_workshop_search_card(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    rpg_card(ui, assets.as_ref(), p, |ui| {
        ui.horizontal_wrapped(|ui| {
            rpg_section_heading(ui, assets.as_ref(), p, UiIcon::Workshop, "Steam Workshop");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(steamcmd_path) = &state.env.steamcmd_path {
                    status_pill(
                        ui,
                        &format!("SteamCMD: {}", steamcmd_path.display()),
                        p.accent_emerald,
                    );
                } else {
                    if ui
                        .add_enabled_ui(!state.steamcmd_busy, |ui| {
                            rpg_button(
                                ui,
                                assets.as_ref(),
                                p,
                                ButtonSkin::Cyan,
                                UiIcon::Download,
                                "Cài SteamCMD tự động",
                            )
                        })
                        .inner
                        .clicked()
                    {
                        state.install_steamcmd();
                    }
                    if state.steamcmd_busy {
                        ui.spinner();
                    }
                    status_pill(ui, "Chưa có SteamCMD", p.accent_amber);
                }
            });
        });

        if let Some(prog) = &state.steamcmd_progress {
            ui.add_space(6.0);
            crate::views::play_view::render_install_progress_bar(ui, prog);
        }

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let search_resp = ui.add(
                egui::TextEdit::singleline(&mut state.workshop_input)
                    .desired_width(280.0)
                    .hint_text("Tìm tên mod hoặc dán Workshop ID / link Steam..."),
            );
            if search_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                state.browse_workshop(1);
            }

            if ui
                .add_enabled_ui(!state.workshop_browse_busy, |ui| {
                    rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Emerald,
                        UiIcon::Workshop,
                        "Tìm / Duyệt",
                    )
                })
                .inner
                .clicked()
            {
                state.browse_workshop(1);
            }

            let parsed_id_valid = parse_workshop_id(&state.workshop_input).is_some();
            if ui
                .add_enabled_ui(parsed_id_valid && !state.workshop_busy, |ui| {
                    rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Slate,
                        UiIcon::Key,
                        "Tra ID",
                    )
                })
                .inner
                .clicked()
            {
                state.lookup_workshop();
            }

            if ui
                .add_enabled_ui(parsed_id_valid && !state.workshop_busy, |ui| {
                    rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Cyan,
                        UiIcon::Download,
                        "Tải về & Cài",
                    )
                })
                .inner
                .clicked()
            {
                state.download_workshop();
            }

            let mut sort_changed = false;
            ui.add_enabled_ui(!state.workshop_browse_busy, |ui| {
                egui::ComboBox::from_id_salt("workshop_sort_combo")
                    .selected_text(state.workshop_sort.label())
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        for sort in [
                            WorkshopSort::Trending,
                            WorkshopSort::MostSubscribed,
                            WorkshopSort::TopRated,
                            WorkshopSort::MostRecent,
                        ] {
                            if ui
                                .selectable_value(&mut state.workshop_sort, sort, sort.label())
                                .changed()
                            {
                                sort_changed = true;
                            }
                        }
                    });
            });
            if sort_changed {
                state.browse_workshop(1);
            }

            if ui
                .add_enabled_ui(!state.workshop_browse_busy, |ui| {
                    rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Slate,
                        UiIcon::Refresh,
                        "Làm mới",
                    )
                })
                .inner
                .clicked()
            {
                let page = state.workshop_page.max(1);
                state.browse_workshop(page);
            }

            if state.workshop_browse_busy || state.workshop_busy {
                ui.spinner();
            }
        });

        if let Some(err) = state.workshop_browse_error.clone() {
            ui.add_space(8.0);
            rpg_subcard(ui, assets.as_ref(), p, |ui| {
                ui.label(RichText::new(err).color(p.accent_red).size(12.5));
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled_ui(!state.workshop_browse_busy, |ui| {
                            rpg_button(
                                ui,
                                assets.as_ref(),
                                p,
                                ButtonSkin::Amber,
                                UiIcon::Refresh,
                                "Thử lại",
                            )
                        })
                        .inner
                        .clicked()
                    {
                        let page = state.workshop_page.max(1);
                        state.browse_workshop(page);
                    }
                    if rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Cyan,
                        UiIcon::Settings,
                        "Mở Cài đặt",
                    )
                    .clicked()
                    {
                        state.active_tab = NavTab::Settings;
                    }
                });
            });
        } else if state.config.proxy_url.trim().is_empty() {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("Nếu nhà mạng chặn Steam, bạn có thể nhập Proxy ở tab Cài đặt.")
                        .size(12.0)
                        .color(p.text_muted),
                );
                if rpg_compact_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Cyan,
                    Some(UiIcon::Settings),
                    "Mở Cài đặt",
                )
                .clicked()
                {
                    state.active_tab = NavTab::Settings;
                }
            });
        }
    });
}

fn render_workshop_detail_card(
    state: &mut AppState,
    ui: &mut egui::Ui,
    details: &WorkshopItemDetails,
) {
    let p = state.palette();
    let assets = state.assets.clone();
    let thumb = state.request_workshop_thumbnail(ui.ctx(), &details.preview_url);
    let install_status =
        state.workshop_install_status(&details.published_file_id, details.time_updated);

    rpg_card(ui, assets.as_ref(), p, |ui| {
        let mut close_details = false;
        ui.horizontal(|ui| {
            rpg_section_heading(
                ui,
                assets.as_ref(),
                p,
                UiIcon::Workshop,
                "Chi tiết Mod Workshop",
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if rpg_compact_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Slate,
                    Some(UiIcon::Stop),
                    "Đóng",
                )
                .clicked()
                {
                    close_details = true;
                }
            });
        });
        if close_details {
            state.workshop_details = None;
            return;
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            render_thumbnail_box(ui, p, thumb, 96.0, Some("Chưa có ảnh"));
            ui.vertical(|ui| {
                ui.label(RichText::new(&details.title).strong().size(15.5));
                if !details.author.trim().is_empty() {
                    ui.label(
                        RichText::new(format!("Tác giả: {}", details.author))
                            .size(12.5)
                            .color(p.text_muted),
                    );
                }

                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    status_pill(
                        ui,
                        &format!("ID {}", details.published_file_id),
                        p.accent_cyan,
                    );
                    render_install_status_pills(ui, p, install_status.as_ref());
                    if details.file_size > 0 {
                        status_pill(ui, &format_bytes(details.file_size), p.accent_emerald);
                    }
                    let updated = format_unix_timestamp(details.time_updated);
                    if !updated.is_empty() {
                        status_pill(ui, &updated, p.text_muted);
                    }
                    if details.subscriptions > 0 {
                        status_pill(
                            ui,
                            &format!("{} đăng ký", format_count(details.subscriptions)),
                            p.accent_cyan,
                        );
                    }
                    if details.favorited > 0 {
                        status_pill(
                            ui,
                            &format!("{} yêu thích", format_count(details.favorited)),
                            p.accent_amber,
                        );
                    }
                    for tag in details
                        .tags
                        .iter()
                        .filter(|t| !t.trim().is_empty())
                        .take(5)
                    {
                        status_pill(ui, tag, p.text_muted);
                    }
                });
            });
        });

        if !details.description.trim().is_empty() {
            let clean_desc = strip_steam_bbcode(&details.description);
            if !clean_desc.is_empty() {
                ui.add_space(8.0);
                rpg_subcard(ui, assets.as_ref(), p, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("workshop_detail_desc")
                        .max_height(130.0)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(clean_desc).size(12.5).color(p.text_primary));
                        });
                });
            }
        }

        let mut inspect_dep: Option<String> = None;
        if !details.child_ids.is_empty() {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Mod phụ thuộc:").size(12.5).color(p.text_muted));
                for cid in &details.child_ids {
                    if rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Slate,
                        Some(UiIcon::Key),
                        &format!("ID {cid}"),
                    )
                    .clicked()
                    {
                        inspect_dep = Some(cid.clone());
                    }
                }
            });
        }

        ui.add_space(8.0);
        let is_this_downloading =
            state.workshop_downloading_id.as_deref() == Some(details.published_file_id.as_str());
        let download_label = match install_status.as_ref() {
            Some((_, _, true)) => "Cập nhật",
            Some((_, _, false)) => "Cài lại",
            None => "Tải về & Cài",
        };

        let mut trigger_download = false;
        let mut trigger_toggle: Option<(String, bool)> = None;

        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled_ui(!state.workshop_busy, |ui| {
                    rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Emerald,
                        UiIcon::Download,
                        download_label,
                    )
                })
                .inner
                .clicked()
            {
                trigger_download = true;
            }
            if is_this_downloading {
                ui.spinner();
            }
            if let Some((internal_name, enabled, _)) = install_status.as_ref() {
                let (btn_text, btn_skin, btn_icon) = if *enabled {
                    ("Tắt", ButtonSkin::Slate, UiIcon::Stop)
                } else {
                    ("Bật", ButtonSkin::Emerald, UiIcon::Play)
                };
                if rpg_button(ui, assets.as_ref(), p, btn_skin, btn_icon, btn_text).clicked() {
                    trigger_toggle = Some((internal_name.clone(), !*enabled));
                }
            }
            if rpg_button(
                ui,
                assets.as_ref(),
                p,
                ButtonSkin::Slate,
                UiIcon::Workshop,
                "Mở trang Steam",
            )
            .clicked()
            {
                let _ = open::that(format!(
                    "https://steamcommunity.com/sharedfiles/filedetails/?id={}",
                    details.published_file_id
                ));
            }
        });

        if let Some(cid) = inspect_dep {
            state.inspect_workshop_id(&cid);
        }
        if trigger_download {
            state.download_workshop_item(&details.published_file_id, Some(details.clone()));
        }
        if let Some((name, enable)) = trigger_toggle {
            state.toggle_mod(&name, enable);
        }
    });
}

fn render_workshop_pagination_controls(
    ui: &mut egui::Ui,
    assets: &LauncherAssets,
    p: ThemePalette,
    page: u32,
    has_next: bool,
    busy: bool,
) -> Option<u32> {
    let mut target_page = None;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui
            .add_enabled_ui(has_next && !busy, |ui| {
                rpg_compact_button(ui, assets, p, ButtonSkin::Slate, None, "Sau →")
            })
            .inner
            .clicked()
        {
            target_page = Some(page + 1);
        }
        status_pill(ui, &format!("Trang {page}"), p.accent_cyan);
        if ui
            .add_enabled_ui(page > 1 && !busy, |ui| {
                rpg_compact_button(ui, assets, p, ButtonSkin::Slate, None, "← Trang trước")
            })
            .inner
            .clicked()
        {
            target_page = Some(page - 1);
        }
    });
    target_page
}

fn render_workshop_catalog_card(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    rpg_card(ui, assets.as_ref(), p, |ui| {
        let page = state.workshop_page.max(1);
        let has_next = state.workshop_has_next;
        let browse_busy = state.workshop_browse_busy;
        let mut nav_page: Option<u32> = None;

        let header_resp = ui
            .horizontal_wrapped(|ui| {
                rpg_section_heading(
                    ui,
                    assets.as_ref(),
                    p,
                    UiIcon::Workshop,
                    "Danh sách Mod Workshop",
                );
                if let Some(target) = render_workshop_pagination_controls(
                    ui,
                    assets.as_ref(),
                    p,
                    page,
                    has_next,
                    browse_busy,
                ) {
                    nav_page = Some(target);
                }
            })
            .response;

        if let Some(target_page) = nav_page {
            state.browse_workshop(target_page);
        }

        ui.add_space(8.0);
        if state.workshop_browse_busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Đang tải danh sách Workshop...").color(p.text_muted));
            });
            return;
        }

        if state.workshop_results.is_empty() {
            let empty_msg = if state.workshop_browse_error.is_some() {
                "Chưa tải được danh sách Workshop. Hãy thử lại sau khi bật WARP/VPN hoặc lưu Proxy."
            } else {
                "Không tìm thấy mod phù hợp trên Steam Workshop."
            };
            rpg_subcard(ui, assets.as_ref(), p, |ui| {
                ui.label(RichText::new(empty_msg).color(p.text_muted));
            });
            return;
        }

        let items = state.workshop_results.clone();
        let two_cols = ui.available_width() >= 760.0;
        let mut action: Option<CatalogAction> = None;

        if two_cols {
            for pair in items.chunks(2) {
                ui.columns(2, |cols| {
                    render_workshop_catalog_item(state, &mut cols[0], &pair[0], &mut action);
                    if let Some(second) = pair.get(1) {
                        render_workshop_catalog_item(state, &mut cols[1], second, &mut action);
                    }
                });
                ui.add_space(6.0);
            }
        } else {
            for item in &items {
                render_workshop_catalog_item(state, ui, item, &mut action);
                ui.add_space(6.0);
            }
        }

        if let Some(act) = action {
            match act {
                CatalogAction::Download(item) => {
                    let id = item.published_file_id.clone();
                    state.download_workshop_item(&id, Some(item));
                }
                CatalogAction::Inspect(id) => state.inspect_workshop_id(&id),
                CatalogAction::OpenSteam(id) => {
                    let _ = open::that(format!(
                        "https://steamcommunity.com/sharedfiles/filedetails/?id={id}"
                    ));
                }
            }
        }

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        let mut bottom_nav_page: Option<u32> = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("Hiển thị {} mod trên trang {page}", items.len()))
                    .size(12.0)
                    .color(p.text_muted),
            );
            bottom_nav_page = render_workshop_pagination_controls(
                ui,
                assets.as_ref(),
                p,
                page,
                has_next,
                state.workshop_browse_busy,
            );
        });
        if let Some(target_page) = bottom_nav_page {
            header_resp.scroll_to_me(Some(egui::Align::TOP));
            state.browse_workshop(target_page);
        }
    });
}

fn render_workshop_catalog_item(
    state: &mut AppState,
    ui: &mut egui::Ui,
    item: &WorkshopItemDetails,
    action: &mut Option<CatalogAction>,
) {
    let p = state.palette();
    let assets = state.assets.clone();
    let tex = state.request_workshop_thumbnail(ui.ctx(), &item.preview_url);
    let install_status =
        state.workshop_install_status(&item.published_file_id, item.time_updated);
    let is_downloading =
        state.workshop_downloading_id.as_deref() == Some(item.published_file_id.as_str());
    let workshop_busy = state.workshop_busy;

    ui.push_id(&item.published_file_id, |ui| {
        rpg_subcard(ui, assets.as_ref(), p, |ui| {
            ui.horizontal(|ui| {
                render_thumbnail_box(ui, p, tex, 72.0, Some("Chưa có ảnh"));
                ui.vertical(|ui| {
                    ui.label(RichText::new(&item.title).strong().size(14.0));
                    if !item.author.trim().is_empty() {
                        ui.label(
                            RichText::new(format!("Tác giả: {}", item.author))
                                .size(11.8)
                                .color(p.text_muted),
                        );
                    }
                    ui.horizontal_wrapped(|ui| {
                        status_pill(ui, &format!("ID {}", item.published_file_id), p.accent_cyan);
                        render_install_status_pills(ui, p, install_status.as_ref());
                    });
                });
            });

            let mut meta_parts = Vec::new();
            if item.file_size > 0 {
                meta_parts.push(format_bytes(item.file_size));
            }
            if item.subscriptions > 0 {
                meta_parts.push(format!("{} đăng ký", format_count(item.subscriptions)));
            }
            let ts = format_unix_timestamp(item.time_updated);
            if !ts.is_empty() {
                meta_parts.push(ts);
            }
            if !meta_parts.is_empty() {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(meta_parts.join("  •  "))
                        .size(11.5)
                        .color(p.text_muted),
                );
            }

            let tags: Vec<&str> = item
                .tags
                .iter()
                .map(String::as_str)
                .filter(|t| !t.trim().is_empty())
                .take(3)
                .collect();
            if !tags.is_empty() {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    for tag in tags {
                        status_pill(ui, tag, p.text_muted);
                    }
                });
            }

            if !item.description.trim().is_empty() {
                let snippet = truncate_snippet(&strip_steam_bbcode(&item.description), 130);
                if !snippet.is_empty() {
                    ui.add_space(4.0);
                    ui.label(RichText::new(snippet).size(12.0).color(p.text_muted));
                }
            }

            ui.add_space(6.0);
            let download_label = match install_status.as_ref() {
                Some((_, _, true)) => "Cập nhật",
                _ => "Tải & Cài",
            };
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled_ui(!workshop_busy, |ui| {
                        rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Emerald,
                            Some(UiIcon::Download),
                            download_label,
                        )
                    })
                    .inner
                    .clicked()
                {
                    *action = Some(CatalogAction::Download(item.clone()));
                }
                if is_downloading {
                    ui.spinner();
                }
                if rpg_compact_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Cyan,
                    Some(UiIcon::Key),
                    "Chi tiết",
                )
                .clicked()
                {
                    *action = Some(CatalogAction::Inspect(item.published_file_id.clone()));
                }
                if rpg_compact_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Slate,
                    Some(UiIcon::Workshop),
                    "Steam",
                )
                .clicked()
                {
                    *action = Some(CatalogAction::OpenSteam(item.published_file_id.clone()));
                }
            });
        });
    });
}

fn render_staged_workshop_section(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    rpg_card(ui, assets.as_ref(), p, |ui| {
        let mut rescan = false;
        ui.horizontal_wrapped(|ui| {
            rpg_section_heading(
                ui,
                assets.as_ref(),
                p,
                UiIcon::Folder,
                "Mod Workshop đã tải trên máy",
            );
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
                    rescan = true;
                }
                if rpg_compact_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Slate,
                    Some(UiIcon::Folder),
                    "Mở thư mục Staging",
                )
                .clicked()
                {
                    let _ = open::that(state.paths.steamcmd_dir.join("staging"));
                }
            });
        });
        if rescan {
            state.refresh_mods_and_packs();
        }

        ui.add_space(8.0);
        if state.staged_workshop_mods.is_empty() {
            ui.label(
                RichText::new("Chưa có mod Workshop nào được tải vào bộ nhớ tạm.")
                    .color(p.text_muted),
            );
            return;
        }

        let staged_list = state.staged_workshop_mods.clone();
        let mut action: Option<StagedAction> = None;

        for staged in &staged_list {
            render_staged_workshop_card(state, ui, staged, &mut action);
            ui.add_space(5.0);
        }

        if let Some(act) = action {
            match act {
                StagedAction::Install(s) => state.install_staged_workshop_mod(&s),
                StagedAction::Toggle(name, enable) => state.toggle_mod(&name, enable),
                StagedAction::Remove(id) => state.remove_staged_workshop_mod(&id),
            }
        }
    });
}

fn render_staged_workshop_card(
    state: &mut AppState,
    ui: &mut egui::Ui,
    staged: &StagedWorkshopMod,
    action: &mut Option<StagedAction>,
) {
    let p = state.palette();
    let assets = state.assets.clone();
    let mut tex = state.mod_icon_texture(ui.ctx(), &staged.meta);
    if tex.is_none() {
        if let Some(url) = staged.preview_url.as_deref() {
            tex = state.request_workshop_thumbnail(ui.ctx(), url);
        }
    }

    let (is_installed, is_enabled) = match state
        .installed_mods
        .iter()
        .find(|m| m.meta.internal_name == staged.meta.internal_name)
    {
        Some(entry) => (true, entry.enabled),
        None => (staged.installed, staged.enabled),
    };

    let display_name = if !staged.meta.display_name.trim().is_empty() {
        staged.meta.display_name.as_str()
    } else if let Some(title) = staged
        .workshop_title
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        title
    } else {
        staged.meta.internal_name.as_str()
    };

    let author = if !staged.meta.author.trim().is_empty() {
        staged.meta.author.as_str()
    } else {
        staged
            .author
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Không rõ")
    };

    ui.push_id((&staged.workshop_id, &staged.meta.file_name), |ui| {
        rpg_subcard(ui, assets.as_ref(), p, |ui| {
            ui.horizontal(|ui| {
                render_thumbnail_box(ui, p, tex, 44.0, None);

                ui.vertical(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(display_name).strong().size(14.0));
                        status_pill(ui, &format!("v{}", staged.meta.version), p.accent_cyan);
                        status_pill(ui, &format!("ID {}", staged.workshop_id), p.text_muted);
                        if is_installed && is_enabled {
                            status_pill(ui, "Đang bật", p.accent_emerald);
                        } else if is_installed {
                            status_pill(ui, "Đã cài", p.accent_cyan);
                        } else {
                            status_pill(ui, "Chưa cài vào Mods", p.accent_amber);
                        }
                    });
                    ui.label(
                        RichText::new(format!(
                            "Tác giả: {author}  •  {}",
                            format_bytes(staged.meta.file_size)
                        ))
                        .size(12.0)
                        .color(p.text_muted),
                    );
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Red,
                        Some(UiIcon::Trash),
                        "Xóa staging",
                    )
                    .clicked()
                    {
                        *action = Some(StagedAction::Remove(staged.workshop_id.clone()));
                    }
                    if is_installed {
                        let (btn_text, btn_skin, btn_icon) = if is_enabled {
                            ("Tắt", ButtonSkin::Slate, UiIcon::Stop)
                        } else {
                            ("Bật", ButtonSkin::Emerald, UiIcon::Play)
                        };
                        if rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            btn_skin,
                            Some(btn_icon),
                            btn_text,
                        )
                        .clicked()
                        {
                            *action = Some(StagedAction::Toggle(
                                staged.meta.internal_name.clone(),
                                !is_enabled,
                            ));
                        }
                    } else if rpg_compact_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Emerald,
                        Some(UiIcon::Download),
                        "Cài vào Mods",
                    )
                    .clicked()
                    {
                        *action = Some(StagedAction::Install(Box::new(staged.clone())));
                    }
                });
            });
        });
    });
}

fn render_workshop_logs_card(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    rpg_card(ui, assets.as_ref(), p, |ui| {
        ui.horizontal(|ui| {
            let (heading_icon, heading) = if state.steamcmd_busy {
                (UiIcon::Download, "Nhật ký cài đặt SteamCMD")
            } else if state.modpack_sync_busy || state.mods_tab == ModsSubTab::ModPacks {
                (UiIcon::Pack, "Nhật ký đồng bộ ModPack")
            } else {
                (UiIcon::Workshop, "Nhật ký SteamCMD")
            };
            rpg_section_heading(ui, assets.as_ref(), p, heading_icon, heading);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled_ui(!state.workshop_logs.is_empty(), |ui| {
                        rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Slate,
                            Some(UiIcon::Trash),
                            "Xóa log",
                        )
                    })
                    .inner
                    .clicked()
                {
                    state.workshop_logs.clear();
                }
            });
        });
        ui.add_space(6.0);
        console_frame(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .id_salt("workshop_log_scroll")
                .max_height(220.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if state.workshop_logs.is_empty() {
                        let wait_msg = if state.steamcmd_busy {
                            "Đang tải và cài đặt SteamCMD..."
                        } else if state.modpack_sync_busy {
                            "Đang đồng bộ ModPack từ Share Key..."
                        } else if state.workshop_downloading_id.is_some() {
                            "Đang khởi chạy SteamCMD để tải mod..."
                        } else if state.mods_tab == ModsSubTab::ModPacks {
                            "Chưa có nhật ký tải thêm."
                        } else {
                            "Đang truy vấn thông tin Workshop..."
                        };
                        ui.label(
                            RichText::new(wait_msg)
                                .monospace()
                                .size(11.8)
                                .color(p.text_muted),
                        );
                    } else {
                        for line in &state.workshop_logs {
                            ui.label(
                                RichText::new(line)
                                    .monospace()
                                    .size(11.8)
                                    .color(p.console_text),
                            );
                        }
                    }
                });
        });
    });
}

fn render_install_status_pills(
    ui: &mut egui::Ui,
    p: ThemePalette,
    status: Option<&(String, bool, bool)>,
) {
    let Some((_, enabled, update_available)) = status else {
        return;
    };
    if *enabled {
        status_pill(ui, "Đang bật", p.accent_emerald);
    } else {
        status_pill(ui, "Đã cài", p.accent_cyan);
    }
    if *update_available {
        status_pill(ui, "Có bản cập nhật", p.accent_amber);
    }
}

fn render_thumbnail_box(
    ui: &mut egui::Ui,
    p: ThemePalette,
    tex: Option<egui::TextureHandle>,
    size: f32,
    fallback_label: Option<&str>,
) {
    let desired = egui::vec2(size, size);
    if let Some(handle) = tex {
        ui.add(
            egui::Image::new((handle.id(), desired))
                .corner_radius(7.0)
                .fit_to_exact_size(desired),
        );
    } else {
        let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());
        ui.painter().rect_filled(rect, 7.0, p.thumb_fallback_bg);
        if let Some(label) = fallback_label {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                ui_font(11.5),
                p.text_muted,
            );
        }
    }
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn truncate_snippet(text: &str, max_chars: usize) -> String {
    let mut collapsed = String::new();
    for word in text.split_whitespace() {
        if !collapsed.is_empty() {
            collapsed.push(' ');
        }
        collapsed.push_str(word);
        if collapsed.chars().count() > max_chars + 16 {
            break;
        }
    }
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let truncated: String = collapsed.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", truncated.trim_end())
}
