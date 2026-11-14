use crate::state::{AppState, ModsSubTab, NavTab};
use crate::theme::{
    accent_button, card_frame, console_frame, danger_button, format_bytes, primary_button,
    section_heading, status_pill, subcard_frame, ACCENT_AMBER, ACCENT_CYAN, ACCENT_EMERALD,
    ACCENT_RED, BG_ELEVATED, TEXT_MUTED, TEXT_PRIMARY,
};
use egui::RichText;
use tl_runtime::{
    format_unix_timestamp, parse_workshop_id, strip_steam_bbcode, StagedWorkshopMod,
    WorkshopItemDetails, WorkshopSort, CURATED_GITHUB_MODS,
};

pub fn render(state: &mut AppState, ui: &mut egui::Ui) {
    ui.label(
        RichText::new("Quản lý Mod & ModPack")
            .size(21.0)
            .strong()
            .color(TEXT_PRIMARY),
    );
    ui.add_space(8.0);

    ui.horizontal_wrapped(|ui| {
        for sub in [
            ModsSubTab::Installed,
            ModsSubTab::GitHub,
            ModsSubTab::ModPacks,
            ModsSubTab::Workshop,
        ] {
            let active = state.mods_tab == sub;
            if ui.selectable_label(active, sub.label()).clicked() {
                state.mods_tab = sub;
            }
        }
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
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.mod_search)
                    .desired_width(210.0)
                    .hint_text("Tìm mod theo tên hoặc tác giả..."),
            );
            if primary_button(ui, "Nhập .tmod / .zip").clicked() {
                state.import_mods_dialog();
            }
            if ui.button("Mở thư mục Mods").clicked() {
                let _ = open::that(state.paths.tml_mods_dir());
            }
            if ui.button("Quét lại").clicked() {
                state.refresh_mods_and_packs();
            }
            let enabled_count = state.installed_mods.iter().filter(|m| m.enabled).count();
            status_pill(
                ui,
                &format!("{enabled_count}/{} đang bật", state.installed_mods.len()),
                ACCENT_EMERALD,
            );
        });

        ui.add_space(8.0);
        if state.installed_mods.is_empty() {
            subcard_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new("Chưa có file .tmod nào. Bạn có thể kéo thả file .tmod/.zip vào cửa sổ hoặc tải từ GitHub / Workshop.")
                        .color(TEXT_MUTED),
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
            subcard_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    if let Some(handle) = tex {
                        ui.image((handle.id(), egui::vec2(44.0, 44.0)));
                    } else {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
                        ui.painter().rect_filled(rect, 7.0, BG_ELEVATED);
                    }

                    ui.vertical(|ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&entry.meta.display_name).strong().size(14.5));
                            status_pill(ui, &format!("v{}", entry.meta.version), ACCENT_CYAN);
                            status_pill(ui, entry.meta.side.label(), ACCENT_AMBER);
                            if !entry.missing_deps.is_empty() {
                                status_pill(
                                    ui,
                                    &format!("Thiếu: {}", entry.missing_deps.join(", ")),
                                    ACCENT_RED,
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
                            .color(TEXT_MUTED),
                        );
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if danger_button(ui, "Xóa").clicked() {
                            delete_action = Some((
                                entry.meta.file_path.clone(),
                                entry.meta.internal_name.clone(),
                            ));
                        }
                        let mut enabled = entry.enabled;
                        if ui.checkbox(&mut enabled, "Bật").changed() {
                            toggle_action = Some((entry.meta.internal_name.clone(), enabled));
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
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        section_heading(ui, "Nguồn GitHub Release phổ biến");
        ui.add_space(6.0);

        let mut clicked_repo: Option<String> = None;
        for src in CURATED_GITHUB_MODS {
            subcard_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(src.name).strong().size(14.0));
                        ui.label(RichText::new(src.summary).size(12.0).color(TEXT_MUTED));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(!state.github_busy, egui::Button::new("Tìm bản mới")).clicked() {
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
            ui.label(RichText::new("Repo GitHub:").color(TEXT_MUTED));
            ui.add(
                egui::TextEdit::singleline(&mut state.github_repo_input)
                    .desired_width(240.0)
                    .hint_text("owner/repo"),
            );
            if ui
                .add_enabled(!state.github_busy, egui::Button::new("Lấy Release .tmod"))
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
            let assets = state.github_assets.clone();
            for asset in assets {
                subcard_frame().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&asset.asset_name).strong());
                        status_pill(ui, &asset.tag_name, ACCENT_CYAN);
                        ui.label(
                            RichText::new(format_bytes(asset.size_bytes))
                                .size(12.0)
                                .color(TEXT_MUTED),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(
                                    !state.github_busy,
                                    egui::Button::new("Tải & Cài đặt"),
                                )
                                .clicked()
                            {
                                state.download_mod_url(
                                    &asset.download_url,
                                    Some(asset.asset_name.clone()),
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

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        section_heading(ui, "Tải trực tiếp từ URL (.tmod)");
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.direct_mod_url)
                    .desired_width((ui.available_width() - 130.0).max(160.0))
                    .hint_text("https://.../TenMod.tmod"),
            );
            if ui
                .add_enabled(!state.github_busy, egui::Button::new("Tải về"))
                .clicked()
            {
                let url = state.direct_mod_url.clone();
                state.download_mod_url(&url, None);
            }
        });
    });
}

fn render_modpacks_tab(state: &mut AppState, ui: &mut egui::Ui) {
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        section_heading(ui, "Tạo ModPack từ danh sách mod đang bật");
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.new_modpack_name)
                    .desired_width(220.0)
                    .hint_text("Tên ModPack mới..."),
            );
            if primary_button(ui, "Tạo ModPack").clicked() {
                state.create_new_modpack();
            }
        });
    });

    ui.add_space(12.0);

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        section_heading(ui, "Danh sách ModPack");
        ui.add_space(6.0);

        if state.modpacks.is_empty() {
            ui.label(
                RichText::new("Chưa có ModPack nào trong Saves\\tModLoader\\ModPacks.")
                    .color(TEXT_MUTED),
            );
            return;
        }

        let packs = state.modpacks.clone();
        for pack in &packs {
            let is_active = state.config.active_modpack == pack.name;
            subcard_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&pack.name).strong().size(14.5));
                    status_pill(
                        ui,
                        &format!("{} mod", pack.enabled_mods.len()),
                        ACCENT_CYAN,
                    );
                    if pack.local_tmod_count > 0 {
                        status_pill(
                            ui,
                            &format!("{} file .tmod", pack.local_tmod_count),
                            ACCENT_EMERALD,
                        );
                    }
                    if is_active {
                        status_pill(ui, "Đang chọn", ACCENT_EMERALD);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if danger_button(ui, "Xóa").clicked() {
                            state.remove_modpack(pack);
                        }
                        if primary_button(ui, "Kích hoạt").clicked() {
                            state.activate_selected_modpack(pack);
                        }
                    });
                });
            });
            ui.add_space(4.0);
        }
    });
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

    if !state.workshop_logs.is_empty() || state.workshop_busy {
        ui.add_space(12.0);
        render_workshop_logs_card(state, ui);
    }
}

fn render_workshop_search_card(state: &mut AppState, ui: &mut egui::Ui) {
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            section_heading(ui, "Steam Workshop");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(p) = &state.env.steamcmd_path {
                    status_pill(ui, &format!("SteamCMD: {}", p.display()), ACCENT_EMERALD);
                } else {
                    if ui
                        .add_enabled(
                            !state.steamcmd_busy,
                            egui::Button::new("Cài SteamCMD tự động"),
                        )
                        .clicked()
                    {
                        state.install_steamcmd();
                    }
                    if state.steamcmd_busy {
                        ui.spinner();
                    }
                    status_pill(ui, "Chưa có SteamCMD", ACCENT_AMBER);
                }
            });
        });

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
                    primary_button(ui, "Tìm / Duyệt")
                })
                .inner
                .clicked()
            {
                state.browse_workshop(1);
            }

            let parsed_id_valid = parse_workshop_id(&state.workshop_input).is_some();
            if ui
                .add_enabled(
                    parsed_id_valid && !state.workshop_busy,
                    egui::Button::new("Tra ID"),
                )
                .clicked()
            {
                state.lookup_workshop();
            }

            if ui
                .add_enabled(
                    parsed_id_valid && !state.workshop_busy,
                    egui::Button::new("Tải về & Cài"),
                )
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
                .add_enabled(!state.workshop_browse_busy, egui::Button::new("Làm mới"))
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
            subcard_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(err).color(ACCENT_RED).size(12.5));
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(!state.workshop_browse_busy, egui::Button::new("Thử lại"))
                        .clicked()
                    {
                        let page = state.workshop_page.max(1);
                        state.browse_workshop(page);
                    }
                    if accent_button(ui, "Mở Cài đặt", ACCENT_CYAN).clicked() {
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
                        .color(TEXT_MUTED),
                );
                if accent_button(ui, "Mở Cài đặt", ACCENT_CYAN).clicked() {
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
    let thumb = state.request_workshop_thumbnail(ui.ctx(), &details.preview_url);
    let install_status =
        state.workshop_install_status(&details.published_file_id, details.time_updated);

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        let mut close_details = false;
        ui.horizontal(|ui| {
            section_heading(ui, "Chi tiết Mod Workshop");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Đóng").clicked() {
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
            render_thumbnail_box(ui, thumb, 96.0, Some("Chưa có ảnh"));
            ui.vertical(|ui| {
                ui.label(RichText::new(&details.title).strong().size(15.5));
                if !details.author.trim().is_empty() {
                    ui.label(
                        RichText::new(format!("Tác giả: {}", details.author))
                            .size(12.5)
                            .color(TEXT_MUTED),
                    );
                }

                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    status_pill(
                        ui,
                        &format!("ID {}", details.published_file_id),
                        ACCENT_CYAN,
                    );
                    render_install_status_pills(ui, install_status.as_ref());
                    if details.file_size > 0 {
                        status_pill(ui, &format_bytes(details.file_size), ACCENT_EMERALD);
                    }
                    let updated = format_unix_timestamp(details.time_updated);
                    if !updated.is_empty() {
                        status_pill(ui, &updated, TEXT_MUTED);
                    }
                    if details.subscriptions > 0 {
                        status_pill(
                            ui,
                            &format!("{} đăng ký", format_count(details.subscriptions)),
                            ACCENT_CYAN,
                        );
                    }
                    if details.favorited > 0 {
                        status_pill(
                            ui,
                            &format!("{} yêu thích", format_count(details.favorited)),
                            ACCENT_AMBER,
                        );
                    }
                    for tag in details
                        .tags
                        .iter()
                        .filter(|t| !t.trim().is_empty())
                        .take(5)
                    {
                        status_pill(ui, tag, TEXT_MUTED);
                    }
                });
            });
        });

        if !details.description.trim().is_empty() {
            let clean_desc = strip_steam_bbcode(&details.description);
            if !clean_desc.is_empty() {
                ui.add_space(8.0);
                subcard_frame().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical()
                        .id_salt("workshop_detail_desc")
                        .max_height(130.0)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(clean_desc).size(12.5).color(TEXT_PRIMARY));
                        });
                });
            }
        }

        let mut inspect_dep: Option<String> = None;
        if !details.child_ids.is_empty() {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Mod phụ thuộc:").size(12.5).color(TEXT_MUTED));
                for cid in &details.child_ids {
                    if ui.small_button(format!("ID {cid}")).clicked() {
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
                .add_enabled_ui(!state.workshop_busy, |ui| primary_button(ui, download_label))
                .inner
                .clicked()
            {
                trigger_download = true;
            }
            if is_this_downloading {
                ui.spinner();
            }
            if let Some((internal_name, enabled, _)) = install_status.as_ref() {
                let (btn_text, btn_color) = if *enabled {
                    ("Tắt", ACCENT_AMBER)
                } else {
                    ("Bật", ACCENT_EMERALD)
                };
                if accent_button(ui, btn_text, btn_color).clicked() {
                    trigger_toggle = Some((internal_name.clone(), !*enabled));
                }
            }
            if ui.button("Mở trang Steam").clicked() {
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

fn render_workshop_catalog_card(state: &mut AppState, ui: &mut egui::Ui) {
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        let page = state.workshop_page.max(1);
        let mut nav_page: Option<u32> = None;

        ui.horizontal_wrapped(|ui| {
            section_heading(ui, "Danh sách Mod Workshop");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        state.workshop_has_next && !state.workshop_browse_busy,
                        egui::Button::new("Sau →"),
                    )
                    .clicked()
                {
                    nav_page = Some(page + 1);
                }
                status_pill(ui, &format!("Trang {page}"), ACCENT_CYAN);
                if ui
                    .add_enabled(
                        page > 1 && !state.workshop_browse_busy,
                        egui::Button::new("← Trang trước"),
                    )
                    .clicked()
                {
                    nav_page = Some(page - 1);
                }
            });
        });

        if let Some(target_page) = nav_page {
            state.browse_workshop(target_page);
        }

        ui.add_space(8.0);
        if state.workshop_browse_busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Đang tải danh sách Workshop...").color(TEXT_MUTED));
            });
            return;
        }

        if state.workshop_results.is_empty() {
            let empty_msg = if state.workshop_browse_error.is_some() {
                "Chưa tải được danh sách Workshop. Hãy thử lại sau khi bật WARP/VPN hoặc lưu Proxy."
            } else {
                "Không tìm thấy mod phù hợp trên Steam Workshop."
            };
            subcard_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(empty_msg).color(TEXT_MUTED));
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
    });
}

fn render_workshop_catalog_item(
    state: &mut AppState,
    ui: &mut egui::Ui,
    item: &WorkshopItemDetails,
    action: &mut Option<CatalogAction>,
) {
    let tex = state.request_workshop_thumbnail(ui.ctx(), &item.preview_url);
    let install_status =
        state.workshop_install_status(&item.published_file_id, item.time_updated);
    let is_downloading =
        state.workshop_downloading_id.as_deref() == Some(item.published_file_id.as_str());
    let workshop_busy = state.workshop_busy;

    ui.push_id(&item.published_file_id, |ui| {
        subcard_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                render_thumbnail_box(ui, tex, 72.0, Some("Chưa có ảnh"));
                ui.vertical(|ui| {
                    ui.label(RichText::new(&item.title).strong().size(14.0));
                    if !item.author.trim().is_empty() {
                        ui.label(
                            RichText::new(format!("Tác giả: {}", item.author))
                                .size(11.8)
                                .color(TEXT_MUTED),
                        );
                    }
                    ui.horizontal_wrapped(|ui| {
                        status_pill(ui, &format!("ID {}", item.published_file_id), ACCENT_CYAN);
                        render_install_status_pills(ui, install_status.as_ref());
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
                        .color(TEXT_MUTED),
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
                        status_pill(ui, tag, TEXT_MUTED);
                    }
                });
            }

            if !item.description.trim().is_empty() {
                let snippet = truncate_snippet(&strip_steam_bbcode(&item.description), 130);
                if !snippet.is_empty() {
                    ui.add_space(4.0);
                    ui.label(RichText::new(snippet).size(12.0).color(TEXT_MUTED));
                }
            }

            ui.add_space(6.0);
            let download_label = match install_status.as_ref() {
                Some((_, _, true)) => "Cập nhật",
                _ => "Tải & Cài",
            };
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled_ui(!workshop_busy, |ui| primary_button(ui, download_label))
                    .inner
                    .clicked()
                {
                    *action = Some(CatalogAction::Download(item.clone()));
                }
                if is_downloading {
                    ui.spinner();
                }
                if ui.button("Chi tiết").clicked() {
                    *action = Some(CatalogAction::Inspect(item.published_file_id.clone()));
                }
                if ui.button("Steam").clicked() {
                    *action = Some(CatalogAction::OpenSteam(item.published_file_id.clone()));
                }
            });
        });
    });
}

fn render_staged_workshop_section(state: &mut AppState, ui: &mut egui::Ui) {
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        let mut rescan = false;
        ui.horizontal_wrapped(|ui| {
            section_heading(ui, "Mod Workshop đã tải trên máy");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Quét lại").clicked() {
                    rescan = true;
                }
                if ui.button("Mở thư mục Staging").clicked() {
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
                    .color(TEXT_MUTED),
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
        subcard_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                render_thumbnail_box(ui, tex, 44.0, None);

                ui.vertical(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(display_name).strong().size(14.0));
                        status_pill(ui, &format!("v{}", staged.meta.version), ACCENT_CYAN);
                        status_pill(ui, &format!("ID {}", staged.workshop_id), TEXT_MUTED);
                        if is_installed && is_enabled {
                            status_pill(ui, "Đang bật", ACCENT_EMERALD);
                        } else if is_installed {
                            status_pill(ui, "Đã cài", ACCENT_CYAN);
                        } else {
                            status_pill(ui, "Chưa cài vào Mods", ACCENT_AMBER);
                        }
                    });
                    ui.label(
                        RichText::new(format!(
                            "Tác giả: {author}  •  {}",
                            format_bytes(staged.meta.file_size)
                        ))
                        .size(12.0)
                        .color(TEXT_MUTED),
                    );
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if danger_button(ui, "Xóa staging").clicked() {
                        *action = Some(StagedAction::Remove(staged.workshop_id.clone()));
                    }
                    if is_installed {
                        let (btn_text, btn_color) = if is_enabled {
                            ("Tắt", ACCENT_AMBER)
                        } else {
                            ("Bật", ACCENT_EMERALD)
                        };
                        if accent_button(ui, btn_text, btn_color).clicked() {
                            *action = Some(StagedAction::Toggle(
                                staged.meta.internal_name.clone(),
                                !is_enabled,
                            ));
                        }
                    } else if primary_button(ui, "Cài vào Mods").clicked() {
                        *action = Some(StagedAction::Install(Box::new(staged.clone())));
                    }
                });
            });
        });
    });
}

fn render_workshop_logs_card(state: &mut AppState, ui: &mut egui::Ui) {
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            section_heading(ui, "Nhật ký SteamCMD");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!state.workshop_logs.is_empty(), egui::Button::new("Xóa log"))
                    .clicked()
                {
                    state.workshop_logs.clear();
                }
            });
        });
        ui.add_space(6.0);
        console_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .id_salt("workshop_log_scroll")
                .max_height(220.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if state.workshop_logs.is_empty() {
                        let wait_msg = if state.workshop_downloading_id.is_some() {
                            "Đang khởi chạy SteamCMD để tải mod..."
                        } else {
                            "Đang truy vấn thông tin Workshop..."
                        };
                        ui.label(
                            RichText::new(wait_msg)
                                .monospace()
                                .size(11.8)
                                .color(TEXT_MUTED),
                        );
                    } else {
                        for line in &state.workshop_logs {
                            ui.label(RichText::new(line).monospace().size(11.8));
                        }
                    }
                });
        });
    });
}

fn render_install_status_pills(ui: &mut egui::Ui, status: Option<&(String, bool, bool)>) {
    let Some((_, enabled, update_available)) = status else {
        return;
    };
    if *enabled {
        status_pill(ui, "Đang bật", ACCENT_EMERALD);
    } else {
        status_pill(ui, "Đã cài", ACCENT_CYAN);
    }
    if *update_available {
        status_pill(ui, "Có bản cập nhật", ACCENT_AMBER);
    }
}

fn render_thumbnail_box(
    ui: &mut egui::Ui,
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
        ui.painter().rect_filled(rect, 7.0, BG_ELEVATED);
        if let Some(label) = fallback_label {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(11.0),
                TEXT_MUTED,
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
