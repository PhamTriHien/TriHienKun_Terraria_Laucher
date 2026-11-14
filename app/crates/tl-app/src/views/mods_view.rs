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
    WorkshopItemDetails, WorkshopSort, CURATED_GITHUB_MODS,
};

pub fn render(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    let busy = state.workshop_browse_busy
        || state.workshop_busy
        || state.workshop_inspect_busy
        || state.workshop_candidates_busy
        || state.steamcmd_busy
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
            ui.label(
                RichText::new(
                    "Chưa có mod nào. Kéo thả tệp .tmod/.zip vào cửa sổ hoặc tải từ Workshop.",
                )
                .color(p.text_muted),
            );
            return;
        }

        let query = state.mod_search.trim().to_lowercase();
        let matched_indices: Vec<usize> = state
            .installed_mods
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                query.is_empty()
                    || entry.meta.display_name.to_lowercase().contains(&query)
                    || entry.meta.internal_name.to_lowercase().contains(&query)
                    || entry.meta.author.to_lowercase().contains(&query)
            })
            .map(|(idx, _)| idx)
            .collect();

        if matched_indices.is_empty() {
            ui.label(
                RichText::new("Không tìm thấy mod phù hợp với từ khóa.").color(p.text_muted),
            );
            return;
        }

        let mut toggle_action: Option<(String, bool)> = None;
        let mut delete_action: Option<(std::path::PathBuf, String)> = None;

        for idx in matched_indices {
            let tex = if let Some(cached) =
                state.mod_icons.get(&state.installed_mods[idx].meta.sha1_hex)
            {
                Some(cached.clone())
            } else if state.installed_mods[idx].meta.icon_png.is_some() {
                let meta = state.installed_mods[idx].meta.clone();
                state.mod_icon_texture(ui.ctx(), &meta)
            } else {
                None
            };

            let entry = &state.installed_mods[idx];
            rpg_subcard(ui, assets.as_ref(), p, |ui| {
                ui.horizontal(|ui| {
                    render_thumbnail_box(ui, p, tex, 44.0, None);

                    ui.vertical(|ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&entry.meta.display_name).strong().size(14.5));
                            status_pill(ui, &format!("v{}", entry.meta.version), p.accent_cyan);
                            let side_pill = match entry.meta.side.label() {
                                "Both" => None,
                                "Client" => Some("Chỉ Client"),
                                "Server" => Some("Chỉ Server"),
                                "NoSync" => Some("Không đồng bộ"),
                                other => Some(other),
                            };
                            if let Some(side_text) = side_pill {
                                status_pill(ui, side_text, p.accent_amber);
                            }
                            if !entry.missing_deps.is_empty() {
                                status_pill(
                                    ui,
                                    &format!("Thiếu: {}", entry.missing_deps.join(", ")),
                                    p.accent_red,
                                );
                            }
                        });

                        let author = if entry.meta.author.trim().is_empty() {
                            "Không rõ"
                        } else {
                            entry.meta.author.trim()
                        };
                        let size_str = format_bytes(entry.meta.file_size);
                        let same_name = entry
                            .meta
                            .internal_name
                            .replace(' ', "")
                            .eq_ignore_ascii_case(&entry.meta.display_name.replace(' ', ""));
                        let subtitle = if same_name || entry.meta.internal_name.trim().is_empty() {
                            format!("Tác giả: {author}  •  {size_str}")
                        } else {
                            format!(
                                "{}  •  Tác giả: {author}  •  {size_str}",
                                entry.meta.internal_name
                            )
                        };
                        ui.label(RichText::new(subtitle).size(12.0).color(p.text_muted));
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
                            toggle_action =
                                Some((entry.meta.internal_name.clone(), !entry.enabled));
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
            "Tải Mod từ GitHub Releases",
        );
        ui.add_space(6.0);

        let mut clicked_repo: Option<&'static str> = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Gợi ý nhanh:").color(p.text_muted));
            ui.add_enabled_ui(!state.github_busy, |ui| {
                for src in CURATED_GITHUB_MODS {
                    if rpg_compact_button(ui, assets.as_ref(), p, ButtonSkin::Slate, None, src.name)
                        .clicked()
                    {
                        clicked_repo = Some(src.repo);
                    }
                }
            });
        });
        if let Some(repo) = clicked_repo {
            state.fetch_github_repo(repo);
        }

        ui.add_space(8.0);
        let can_fetch = !state.github_repo_input.trim().is_empty() && !state.github_busy;
        let mut trigger_fetch = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Repo GitHub:").color(p.text_muted));
            let resp = ui.add(
                egui::TextEdit::singleline(&mut state.github_repo_input)
                    .desired_width(280.0)
                    .hint_text("owner/repo (vd: JavidPack/RecipeBrowser)"),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && can_fetch {
                trigger_fetch = true;
            }
            if ui
                .add_enabled_ui(can_fetch, |ui| {
                    rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Cyan,
                        UiIcon::Download,
                        "Lấy Release",
                    )
                })
                .inner
                .clicked()
            {
                trigger_fetch = true;
            }
            if state.github_busy {
                ui.spinner();
            }
        });
        if trigger_fetch {
            let repo = state.github_repo_input.clone();
            state.fetch_github_repo(&repo);
        }

        if !state.github_assets.is_empty() {
            ui.add_space(8.0);
            let mut download_target: Option<(String, String, String)> = None;
            let gh_busy = state.github_busy;
            for asset in &state.github_assets {
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
                                .add_enabled_ui(!gh_busy, |ui| {
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
                                download_target = Some((
                                    asset.download_url.clone(),
                                    asset.asset_name.clone(),
                                    asset.repo.clone(),
                                ));
                            }
                        });
                    });
                });
                ui.add_space(4.0);
            }
            if let Some((url, asset_name, repo)) = download_target {
                state.download_mod_url(&url, Some(asset_name), Some(repo));
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
        let can_download = !state.direct_mod_url.trim().is_empty() && !state.github_busy;
        let mut trigger_download = false;
        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut state.direct_mod_url)
                    .desired_width((ui.available_width() - 150.0).max(160.0))
                    .hint_text("https://.../TenMod.tmod"),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && can_download {
                trigger_download = true;
            }
            if ui
                .add_enabled_ui(can_download, |ui| {
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
                trigger_download = true;
            }
            if state.github_busy {
                ui.spinner();
            }
        });
        if trigger_download {
            let url = state.direct_mod_url.clone();
            state.download_mod_url(&url, None, None);
        }
    });
}

fn render_modpacks_tab(state: &mut AppState, ui: &mut egui::Ui) {
    render_modpack_list_card(state, ui);
    ui.add_space(12.0);
    render_modpack_create_card(state, ui);

    if let Some(preview) = state.share_export_preview.clone() {
        ui.add_space(12.0);
        render_modpack_share_export_card(state, ui, &preview);
    }

    ui.add_space(12.0);
    render_share_key_import_card(state, ui);
}

fn render_modpack_list_card(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    rpg_card(ui, assets.as_ref(), p, |ui| {
        rpg_section_heading(ui, assets.as_ref(), p, UiIcon::Pack, "Danh sách ModPack");
        ui.add_space(6.0);

        if state.modpacks.is_empty() {
            ui.label(RichText::new("Chưa tạo gói ModPack nào.").color(p.text_muted));
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
                if ui
                    .add_enabled_ui(!is_active, |ui| {
                        rpg_compact_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Emerald,
                            if is_active { None } else { Some(UiIcon::Play) },
                            if is_active { "Đang dùng" } else { "Kích hoạt" },
                        )
                    })
                    .inner
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

fn render_modpack_create_card(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    rpg_card(ui, assets.as_ref(), p, |ui| {
        rpg_section_heading(ui, assets.as_ref(), p, UiIcon::Pack, "Tạo & Xuất ModPack");
        ui.add_space(6.0);

        let can_create = !state.new_modpack_name.trim().is_empty();
        let mut trigger_create = false;
        let mut trigger_export_enabled = false;

        ui.horizontal_wrapped(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut state.new_modpack_name)
                    .desired_width(220.0)
                    .hint_text("Tên ModPack mới..."),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && can_create {
                trigger_create = true;
            }
            if ui
                .add_enabled_ui(can_create, |ui| {
                    rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Emerald,
                        UiIcon::Pack,
                        "Tạo ModPack",
                    )
                })
                .inner
                .clicked()
            {
                trigger_create = true;
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
                trigger_export_enabled = true;
            }
        });

        if trigger_create {
            state.create_new_modpack();
        }
        if trigger_export_enabled {
            state.prepare_share_key_from_enabled();
        }
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
        let mut close_export = false;
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
                    close_export = true;
                }
            });
        });
        if close_export {
            state.close_share_export();
            return;
        }

        ui.add_space(6.0);
        let key_preview = if preview.share_key.chars().count() > 48 {
            let prefix: String = preview.share_key.chars().take(48).collect();
            format!("{prefix}...")
        } else {
            preview.share_key.clone()
        };
        let mut copied = false;
        console_frame(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(key_preview)
                        .monospace()
                        .size(12.2)
                        .color(p.console_text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                        copied = true;
                    }
                });
            });
        });
        if copied {
            ui.ctx().copy_text(preview.share_key.clone());
            state.notify_ok("Đã sao chép Share Key vào clipboard.");
        }

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

        let linked_entries: Vec<&ModPackExportEntry> = preview
            .entries
            .iter()
            .filter(|e| !matches!(e.source, SharedModSource::ManualOnly))
            .collect();
        if !linked_entries.is_empty() {
            ui.add_space(6.0);
            egui::CollapsingHeader::new("Danh sách nguồn mod trong gói").show(ui, |ui| {
                for entry in linked_entries {
                    ui.push_id(("linked_src", &entry.internal_name), |ui| {
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
                for cand in state.workshop_candidates.iter().take(8) {
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

fn render_share_key_import_card(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    rpg_card(ui, assets.as_ref(), p, |ui| {
        rpg_section_heading(
            ui,
            assets.as_ref(),
            p,
            UiIcon::Key,
            "Nhập ModPack bằng Share Key",
        );
        ui.add_space(6.0);

        let mut trigger_check = false;
        let mut trigger_clear = false;
        let has_key_input = !state.share_key_input.trim().is_empty();

        ui.horizontal_wrapped(|ui| {
            let reserve = if has_key_input { 95.0 } else { 8.0 };
            let key_resp = ui.add(
                egui::TextEdit::singleline(&mut state.share_key_input)
                    .desired_width((ui.available_width() - reserve).max(200.0))
                    .hint_text("Dán Share Key (TLPACK1:...) từ bạn bè..."),
            );
            if key_resp.changed() {
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

        if state.modpack_sync_busy
            || (state.modpack_sync_report.is_some() && !state.workshop_logs.is_empty())
        {
            ui.add_space(8.0);
            render_collapsible_log_console(
                state,
                ui,
                "modpack_sync_logs",
                "Nhật ký đồng bộ ModPack",
                state.modpack_sync_busy,
            );
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
        render_workshop_details_card(state, ui, &details);
    }

    if !state.workshop_results.is_empty()
        || state.workshop_browse_busy
        || state.workshop_browse_error.is_none()
    {
        ui.add_space(12.0);
        render_workshop_catalog_card(state, ui);
    }

    if !state.workshop_logs.is_empty() || state.workshop_busy || state.steamcmd_busy {
        ui.add_space(12.0);
        let p = state.palette();
        let assets = state.assets.clone();
        rpg_card(ui, assets.as_ref(), p, |ui| {
            render_collapsible_log_console(
                state,
                ui,
                "workshop_dl_logs",
                "Nhật ký tải Workshop",
                state.workshop_busy || state.steamcmd_busy,
            );
        });
    }
}

fn render_workshop_search_card(state: &mut AppState, ui: &mut egui::Ui) {
    let p = state.palette();
    let assets = state.assets.clone();
    rpg_card(ui, assets.as_ref(), p, |ui| {
        ui.horizontal_wrapped(|ui| {
            rpg_section_heading(ui, assets.as_ref(), p, UiIcon::Workshop, "Steam Workshop");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if state.env.steamcmd_path.is_some() {
                    status_pill(ui, "SteamCMD sẵn sàng", p.accent_emerald);
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
        let query_busy = state.workshop_browse_busy || state.workshop_inspect_busy;
        let parsed_id_valid = parse_workshop_id(&state.workshop_input).is_some();
        let can_download_id = !state.workshop_busy && state.env.steamcmd_path.is_some();
        let mut trigger_smart_query = false;
        let mut trigger_download_id = false;
        let mut sort_changed = false;

        ui.horizontal_wrapped(|ui| {
            let search_resp = ui.add(
                egui::TextEdit::singleline(&mut state.workshop_input)
                    .desired_width(300.0)
                    .hint_text("Nhập tên mod, Workshop ID hoặc dán link Steam..."),
            );
            if search_resp.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                && !query_busy
            {
                trigger_smart_query = true;
            }

            if ui
                .add_enabled_ui(!query_busy, |ui| {
                    rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Emerald,
                        UiIcon::Workshop,
                        "Tìm / Tra cứu",
                    )
                })
                .inner
                .clicked()
            {
                trigger_smart_query = true;
            }

            if parsed_id_valid
                && ui
                    .add_enabled_ui(can_download_id, |ui| {
                        rpg_button(
                            ui,
                            assets.as_ref(),
                            p,
                            ButtonSkin::Cyan,
                            UiIcon::Download,
                            "Tải ngay ID",
                        )
                    })
                    .inner
                    .clicked()
            {
                trigger_download_id = true;
            }

            ui.add_enabled_ui(!state.workshop_browse_busy, |ui| {
                egui::ComboBox::from_id_salt("workshop_sort_combo")
                    .selected_text(state.workshop_sort.label())
                    .width(155.0)
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

            if query_busy {
                ui.spinner();
            }
        });

        if trigger_smart_query {
            state.smart_workshop_query();
        }
        if trigger_download_id {
            state.download_workshop();
        }
        if sort_changed {
            state.browse_workshop(1);
        }

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
                        "Cấu hình Proxy",
                    )
                    .clicked()
                    {
                        state.active_tab = NavTab::Settings;
                    }
                });
            });
        }
    });
}

fn render_workshop_details_card(
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
                });

                let clean_tags: Vec<&str> = details
                    .tags
                    .iter()
                    .map(String::as_str)
                    .filter(|t| !t.trim().is_empty())
                    .collect();
                if !clean_tags.is_empty() {
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(format!("Nhãn: {}", clean_tags.join(", ")))
                            .size(11.8)
                            .color(p.text_muted),
                    );
                }
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

        let header_resp = ui
            .horizontal(|ui| {
                rpg_section_heading(
                    ui,
                    assets.as_ref(),
                    p,
                    UiIcon::Workshop,
                    "Danh sách Mod Workshop",
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    status_pill(ui, &format!("Trang {page}"), p.accent_cyan);
                });
            })
            .response;

        ui.add_space(8.0);
        if state.workshop_browse_busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Đang tải danh sách Workshop...").color(p.text_muted));
            });
            return;
        }

        if state.workshop_results.is_empty() {
            if state.workshop_browse_error.is_none() {
                ui.label(
                    RichText::new("Không tìm thấy mod phù hợp trên Steam Workshop.")
                        .color(p.text_muted),
                );
            }
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
                RichText::new(format!("Hiển thị {} mod", items.len()))
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
                render_thumbnail_box(ui, p, tex, 68.0, Some("Chưa có ảnh"));
                ui.vertical(|ui| {
                    ui.label(RichText::new(&item.title).strong().size(14.0));
                    if !item.author.trim().is_empty() {
                        ui.label(
                            RichText::new(format!("Tác giả: {}", item.author))
                                .size(11.8)
                                .color(p.text_muted),
                        );
                    }
                    if install_status.is_some() {
                        ui.horizontal_wrapped(|ui| {
                            render_install_status_pills(ui, p, install_status.as_ref());
                        });
                    }
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

            if !item.description.trim().is_empty() {
                let snippet = truncate_snippet(&strip_steam_bbcode(&item.description), 110);
                if !snippet.is_empty() {
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(RichText::new(snippet).size(12.0).color(p.text_muted))
                            .truncate(),
                    );
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

fn render_collapsible_log_console(
    state: &mut AppState,
    ui: &mut egui::Ui,
    id_salt: &str,
    title: &str,
    active_busy: bool,
) {
    let p = state.palette();
    let assets = state.assets.clone();
    let was_busy_id = ui.make_persistent_id((id_salt, "was_busy"));
    let was_busy = ui.ctx().data_mut(|d| {
        let prev = d.get_temp::<bool>(was_busy_id).unwrap_or(false);
        d.insert_temp(was_busy_id, active_busy);
        prev
    });
    let open_override = if active_busy {
        Some(true)
    } else if was_busy {
        Some(false)
    } else {
        None
    };

    egui::CollapsingHeader::new(title)
        .id_salt(id_salt)
        .default_open(false)
        .open(open_override)
        .show(ui, |ui| {
            if !state.workshop_logs.is_empty() {
                ui.horizontal(|ui| {
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
                            state.workshop_logs.clear();
                        }
                    });
                });
                ui.add_space(4.0);
            }

            console_frame(p).show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical()
                    .id_salt((id_salt, "scroll"))
                    .max_height(200.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if state.workshop_logs.is_empty() {
                            let wait_msg = if state.steamcmd_busy {
                                "Đang tải và cài đặt SteamCMD..."
                            } else if state.modpack_sync_busy {
                                "Đang đồng bộ ModPack từ Share Key..."
                            } else {
                                "Đang khởi chạy SteamCMD để tải mod..."
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
