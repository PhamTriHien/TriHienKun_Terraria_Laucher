use crate::assets::{ButtonSkin, UiIcon};
use crate::state::AppState;
use crate::theme::{rpg_button, rpg_card, rpg_section_heading};
use egui::{RichText, TextStyle};
use std::path::Path;
use tl_core::mod_configs::{
    list_mod_configs, read_mod_config, read_mod_config_import, save_mod_config, validate_mod_config,
};
use tl_runtime::clean_launcher_error_msg;

#[derive(Default)]
pub(crate) struct ModConfigEditor {
    files: Option<Vec<String>>,
    draft: Option<Draft>,
    list_error: Option<String>,
    edit_session: u64,
}

struct Draft {
    name: String,
    baseline: Option<String>,
    text: String,
    error: Option<String>,
}

impl Draft {
    fn new(name: String, baseline: Option<String>, text: String) -> Self {
        let mut draft = Self {
            name,
            baseline,
            text,
            error: None,
        };
        draft.validate();
        draft
    }

    fn is_dirty(&self) -> bool {
        self.baseline.as_deref() != Some(self.text.as_str())
    }

    fn validate(&mut self) {
        self.error = validate_mod_config(&self.text)
            .err()
            .map(clean_launcher_error_msg);
    }
}

impl ModConfigEditor {
    fn refresh(&mut self, dir: &Path) {
        match list_mod_configs(dir) {
            Ok(files) => {
                self.files = Some(files);
                self.list_error = None;
            }
            Err(err) => {
                self.files = Some(Vec::new());
                self.list_error = Some(clean_launcher_error_msg(err));
            }
        }
    }

    fn open(&mut self, dir: &Path, name: String) -> tl_core::LauncherResult<()> {
        let text = read_mod_config(dir, &name)?.ok_or_else(|| {
            tl_core::LauncherError::NotFound(format!(
                "Không còn tìm thấy {name}. Hãy quét lại danh sách."
            ))
        })?;
        self.edit_session += 1;
        self.draft = Some(Draft::new(name, Some(text.clone()), text));
        Ok(())
    }

    fn import(&mut self, dir: &Path, path: &Path) -> tl_core::LauncherResult<()> {
        let (name, text) = read_mod_config_import(path)?;
        let baseline = read_mod_config(dir, &name)?;
        self.edit_session += 1;
        self.draft = Some(Draft::new(name, baseline, text));
        Ok(())
    }
}

pub(super) fn render(state: &mut AppState, ui: &mut egui::Ui) {
    let dir = state.paths.tml_saves_dir.join("ModConfigs");
    let p = state.palette();
    let assets = state.assets.clone();
    let locked = state.is_mod_config_write_locked();
    if state.mod_config_editor.files.is_none() {
        state.mod_config_editor.refresh(&dir);
    }
    rpg_card(ui, assets.as_ref(), p, |ui| {
        rpg_section_heading(
            ui,
            assets.as_ref(),
            p,
            UiIcon::Settings,
            "Cấu hình mod tModLoader",
        );
        ui.label(
            RichText::new("Dùng chung cho game và server tModLoader. Áp dụng ở lần chạy sau.")
                .color(p.text_muted),
        );
        let can_open = !state
            .mod_config_editor
            .draft
            .as_ref()
            .is_some_and(Draft::is_dirty);
        let mut selected = None;
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(can_open, |ui| {
                let current = state
                    .mod_config_editor
                    .draft
                    .as_ref()
                    .map_or("Chọn file cấu hình", |draft| draft.name.as_str());
                egui::ComboBox::from_id_salt("tml_mod_config_file")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for name in state.mod_config_editor.files.iter().flatten() {
                            if ui.selectable_label(name == current, name).clicked() {
                                selected = Some(name.clone());
                            }
                        }
                    });
                if rpg_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Cyan,
                    UiIcon::Download,
                    "Nhập JSON",
                )
                .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("JSON", &["json"])
                        .pick_file()
                    {
                        if let Err(err) = state.mod_config_editor.import(&dir, &path) {
                            state.notify_err(clean_launcher_error_msg(err));
                        }
                    }
                }
            })
            .response
            .on_disabled_hover_text("Lưu hoặc bỏ thay đổi trước khi mở file khác.");
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
                state.mod_config_editor.refresh(&dir);
            }
        });
        if let Some(name) = selected {
            if let Err(err) = state.mod_config_editor.open(&dir, name) {
                state.notify_err(clean_launcher_error_msg(err));
            }
        }
        if let Some(error) = &state.mod_config_editor.list_error {
            ui.colored_label(p.accent_red, error);
        }
        let session = state.mod_config_editor.edit_session;
        let Some(draft) = state.mod_config_editor.draft.as_mut() else {
            if state.mod_config_editor.list_error.is_none()
                && state
                    .mod_config_editor
                    .files
                    .as_ref()
                    .is_some_and(Vec::is_empty)
            {
                ui.label("Chưa có file cấu hình. Nhập JSON hoặc chạy mod để tModLoader tạo file khi cần.");
            }
            return;
        };
        ui.add_space(8.0);
        let destination = dir.join(&draft.name);
        let editor_label = ui.label(
            RichText::new(format!("File đích: {}", destination.display())).color(p.text_muted),
        );
        egui::ScrollArea::vertical()
            .id_salt(("mod_config_text_scroll", session))
            .max_height(300.0)
            .show(ui, |ui| {
                if ui
                    .add(
                        egui::TextEdit::multiline(&mut draft.text)
                            .id_salt(("mod_config_text", session))
                            .font(TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .desired_rows(10),
                    )
                    .labelled_by(editor_label.id)
                    .changed()
                {
                    draft.validate();
                }
            });
        if let Some(error) = &draft.error {
            ui.colored_label(p.accent_red, error);
        }
        if locked {
            ui.colored_label(
                p.accent_amber,
                "Dừng tModLoader và server tModLoader trước khi lưu.",
            );
        }
        let dirty = draft.is_dirty();
        let can_save = dirty && draft.error.is_none() && !locked;
        if dirty && draft.baseline.is_some() {
            ui.label("Lưu sẽ thay file hiện tại và sao lưu bản cũ.");
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled_ui(can_save, |ui| {
                    rpg_button(
                        ui,
                        assets.as_ref(),
                        p,
                        ButtonSkin::Emerald,
                        UiIcon::Settings,
                        "Lưu cấu hình",
                    )
                })
                .inner
                .clicked()
            {
                save_draft(state, &dir);
            }
            if dirty
                && rpg_button(
                    ui,
                    assets.as_ref(),
                    p,
                    ButtonSkin::Slate,
                    UiIcon::Refresh,
                    "Bỏ thay đổi",
                )
                .clicked()
            {
                state.mod_config_editor.draft = None;
            }
        });
    });
}

fn save_draft(state: &mut AppState, dir: &Path) {
    state.scan_external_servers();
    if state.is_mod_config_write_locked() {
        state.notify_err("Dừng tModLoader và server tModLoader trước khi lưu.");
        return;
    }
    let Some(draft) = &mut state.mod_config_editor.draft else {
        return;
    };
    match save_mod_config(dir, &draft.name, draft.baseline.as_deref(), &draft.text) {
        Ok(backup) => {
            draft.baseline = Some(draft.text.clone());
            state.mod_config_editor.refresh(dir);
            if let Some(path) = backup {
                state.notify_ok(format!("Đã lưu cấu hình. Bản sao lưu: {}", path.display()));
            } else {
                state.notify_ok("Đã lưu cấu hình mod.");
            }
        }
        Err(err) => state.notify_err(clean_launcher_error_msg(err)),
    }
}
