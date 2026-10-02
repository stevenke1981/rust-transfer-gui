//! Remote file browser widgets shared by the sidebar and the SFTP/FTP tabs.

use std::path::{Path, PathBuf};

use crate::i18n::Language;

use eframe::egui::{self, RichText};

use super::session::SessionTab;
use crate::common::{format_unix_time, human_bytes};
use crate::worker::SessionCommand;

/// A delete that awaits user confirmation.
pub struct PendingDelete {
    pub tab_id: u64,
    pub name: String,
    pub is_dir: bool,
}

/// Up / refresh / upload / download / new folder / delete buttons.
pub fn toolbar(ui: &mut egui::Ui, tab: &mut SessionTab) -> Option<PendingDelete> {
    let language = Language::from_context(ui.ctx());
    ui.weak(language.text("Drop files here to upload to the active session."));
    let idle = tab.is_idle();
    let mut pending = None;
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(idle, egui::Button::new("⬆")).on_hover_text(language.text("Parent directory")).clicked() {
            tab.send(SessionCommand::Up);
        }
        if ui.add_enabled(idle, egui::Button::new("⟳")).on_hover_text(language.text("Refresh")).clicked() {
            tab.send(SessionCommand::Refresh);
        }
        if ui
            .add_enabled(idle, egui::Button::new(language.text("⤴ Upload")))
            .on_hover_text(language.text("Upload a local file here"))
            .clicked()
            && let Some(p) = rfd::FileDialog::new().set_directory(super::default_local_dir()).pick_file()
        {
            let remote_name = file_name_of(&p);
            tab.send(SessionCommand::Upload { local: p, remote_name });
        }
        let sel_file = tab.browser.selected_entry().filter(|e| !e.is_dir).map(|e| e.name.clone());
        if ui
            .add_enabled(idle && sel_file.is_some(), egui::Button::new(language.text("⤵ Download")))
            .on_hover_text(language.text("Download the selected file"))
            .on_disabled_hover_text(language.text("Select a remote file first"))
            .clicked()
            && let Some(name) = sel_file
            && let Some(local) =
                rfd::FileDialog::new().set_directory(super::default_local_dir()).set_file_name(name.clone()).save_file()
        {
            tab.send(SessionCommand::Download { remote_name: name, local });
        }
        if ui.add_enabled(idle, egui::Button::new("📁+")).on_hover_text(language.text("New folder")).clicked() {
            tab.browser.new_folder = Some(String::new());
        }
        let sel = tab.browser.selected_entry().map(|e| (e.name.clone(), e.is_dir));
        if ui
            .add_enabled(idle && sel.is_some(), egui::Button::new("🗑"))
            .on_hover_text(language.text("Delete the selected file or (empty) folder"))
            .clicked()
            && let Some((name, is_dir)) = sel
        {
            pending = Some(PendingDelete { tab_id: tab.id, name, is_dir });
        }
    });
    if let Some(name) = tab.browser.new_folder.as_mut() {
        let mut create = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            let r = ui
                .add(egui::TextEdit::singleline(name).hint_text(language.text("new folder name")).desired_width(150.0));
            if !r.has_focus() && name.is_empty() {
                r.request_focus();
            }
            create = ui.button(language.text("Create")).clicked()
                || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            cancel = ui.button(language.text("Cancel")).clicked();
        });
        if create && !name.trim().is_empty() {
            let n = name.trim().to_string();
            tab.send(SessionCommand::Mkdir(n));
            tab.browser.new_folder = None;
        } else if cancel {
            tab.browser.new_folder = None;
        }
    }
    pending
}

/// Directory listing with name / size / modified columns. Double-click enters folders.
pub fn listing(ui: &mut egui::Ui, tab: &mut SessionTab, compact: bool) {
    let language = Language::from_context(ui.ctx());
    let idle = tab.is_idle();
    ui.horizontal(|ui| {
        ui.label("📂");
        let r = ui.add_enabled(
            idle,
            egui::TextEdit::singleline(&mut tab.browser.path_input)
                .font(egui::TextStyle::Monospace)
                .desired_width(ui.available_width()),
        );
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !tab.browser.path_input.trim().is_empty()
        {
            tab.send(SessionCommand::Cd(tab.browser.path_input.trim().to_string()));
        }
    });
    let mut enter: Option<String> = None;
    egui::ScrollArea::both().id_salt(("listing", tab.id, compact)).auto_shrink([false, false]).show(ui, |ui| {
        if !tab.connected {
            ui.weak(if tab.worker.is_some() {
                language.text("Connecting…")
            } else {
                language.text("Not connected.")
            });
            return;
        }
        egui::Grid::new(("grid", tab.id, compact))
            .striped(true)
            .num_columns(3)
            .min_col_width(if compact { 40.0 } else { 70.0 })
            .show(ui, |ui| {
                ui.label(RichText::new(language.text("Name")).strong());
                ui.label(RichText::new(language.text("Size")).strong());
                ui.label(RichText::new(language.text("Modified (UTC)")).strong());
                ui.end_row();
                if ui.selectable_label(false, "📁 ..").double_clicked() {
                    enter = Some("..".into());
                }
                ui.label("");
                ui.label("");
                ui.end_row();
                for (i, e) in tab.browser.entries.iter().enumerate() {
                    let icon = if e.is_dir { "📁" } else { "📄" };
                    let r = ui.selectable_label(tab.browser.selected == Some(i), format!("{icon} {}", e.name));
                    if r.clicked() {
                        tab.browser.selected = Some(i);
                    }
                    if r.double_clicked() && e.is_dir {
                        enter = Some(e.name.clone());
                    }
                    ui.label(RichText::new(e.size.map(human_bytes).unwrap_or_default()).monospace());
                    ui.label(RichText::new(e.modified.map(format_unix_time).unwrap_or_default()).monospace().weak());
                    ui.end_row();
                }
            });
    });
    if let Some(name) = enter
        && idle
    {
        if name == ".." {
            tab.send(SessionCommand::Up);
        } else {
            tab.send(SessionCommand::Cd(name));
        }
    }
}

/// Text-field based upload/download form (works without a native file dialog).
pub fn transfer_form(ui: &mut egui::Ui, tab: &mut SessionTab) {
    let language = Language::from_context(ui.ctx());
    let idle = tab.is_idle();
    // Keep the suggested download path in sync with the selection.
    if let Some(e) = tab.browser.selected_entry().filter(|e| !e.is_dir) {
        let name = e.name.clone();
        let current = Path::new(&tab.browser.download_local);
        if current.file_name().is_none_or(|f| f.to_string_lossy() != name) {
            let dir = current
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or_else(super::default_local_dir);
            tab.browser.download_local = dir.join(&name).to_string_lossy().into_owned();
        }
    }
    let b = &mut tab.browser;
    let mut cmd = None;
    let label_w = 140.0;
    let field = egui::vec2((ui.available_width() - label_w - 330.0).clamp(160.0, 520.0), 20.0);
    ui.horizontal_wrapped(|ui| {
        ui.add_sized([label_w, 20.0], egui::Label::new(language.text("Upload local file")));
        ui.add_sized(
            field,
            egui::TextEdit::singleline(&mut b.upload_local).hint_text(language.text("/path/to/local/file")),
        );
        ui.label(language.text("as"));
        ui.add_sized(
            [140.0, 20.0],
            egui::TextEdit::singleline(&mut b.upload_remote_name).hint_text(language.text("remote name")),
        );
        if ui
            .add_enabled(idle && !b.upload_local.trim().is_empty(), egui::Button::new(language.text("⤴ Upload")))
            .clicked()
        {
            let local = PathBuf::from(b.upload_local.trim());
            let mut name = b.upload_remote_name.trim().to_string();
            if name.is_empty() {
                name = file_name_of(&local);
            }
            cmd = Some(SessionCommand::Upload { local, remote_name: name });
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.add_sized([label_w, 20.0], egui::Label::new(language.text("Download selected to")));
        ui.add_sized(
            field,
            egui::TextEdit::singleline(&mut b.download_local).hint_text(language.text("select a remote file")),
        );
        let sel = b.selected_entry().filter(|e| !e.is_dir).map(|e| e.name.clone());
        if ui
            .add_enabled(
                idle && sel.is_some() && !b.download_local.trim().is_empty(),
                egui::Button::new(language.text("⤵ Download")),
            )
            .on_disabled_hover_text(language.text("Select a remote file first"))
            .clicked()
            && let Some(remote_name) = sel
        {
            cmd = Some(SessionCommand::Download { remote_name, local: PathBuf::from(b.download_local.trim()) });
        }
    });
    if let Some(c) = cmd {
        tab.send(c);
    }
}

pub fn file_name_of(p: &Path) -> String {
    p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
}
