//! "New session" dialog: protocol buttons across the top, settings below.

use eframe::egui::{self, RichText, Vec2};

use super::session::Secrets;
use super::theme::ACCENT;
use crate::config::{AuthMethod, Protocol, SavedSession};

pub enum DialogResult {
    /// Open a tab with these settings (and optionally save them).
    Connect { session: SavedSession, secrets: Secrets, save: bool, replace: Option<usize> },
    /// Only save/update the bookmark.
    Save { session: SavedSession, replace: Option<usize> },
}

pub struct SessionDialog {
    pub open: bool,
    pub session: SavedSession,
    pub secrets: Secrets,
    pub save: bool,
    /// Index in the saved-session list when editing an existing entry.
    pub editing: Option<usize>,
    focus_password: bool,
}

impl Default for SessionDialog {
    fn default() -> Self {
        Self {
            open: false,
            session: SavedSession::new(Protocol::Ssh),
            secrets: Secrets::default(),
            save: true,
            editing: None,
            focus_password: false,
        }
    }
}

impl SessionDialog {
    /// Open an empty dialog for `protocol`.
    pub fn open_new(&mut self, protocol: Protocol) {
        let mut s = SavedSession::new(protocol);
        s.host = self.session.host.clone();
        s.user = if self.session.user.is_empty() { default_user() } else { self.session.user.clone() };
        if protocol == Protocol::Ftp && s.user.is_empty() {
            s.user = "anonymous".into();
        }
        s.key_path = default_key_path();
        *self = Self { open: true, session: s, save: true, ..Default::default() };
    }

    /// Open pre-filled from a saved session (to enter the password, or to edit it).
    pub fn open_saved(&mut self, s: &SavedSession, index: usize, editing: bool) {
        *self = Self {
            open: true,
            session: s.clone(),
            save: editing,
            editing: Some(index),
            focus_password: !editing,
            ..Default::default()
        };
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<DialogResult> {
        if !self.open {
            return None;
        }
        let mut result = None;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("session_dialog")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.horizontal(|ui| {
                ui.heading(if self.editing.is_some() && self.save { "Edit session" } else { "Session settings" });
            });
            ui.add_space(6.0);
            // Protocol buttons across the top.
            ui.horizontal(|ui| {
                for p in Protocol::ALL {
                    let selected = self.session.protocol == p;
                    let color = match p {
                        Protocol::Ssh => egui::Color32::from_rgb(90, 170, 255),
                        Protocol::Sftp => egui::Color32::from_rgb(255, 180, 60),
                        Protocol::Ftp => egui::Color32::from_rgb(110, 210, 120),
                        Protocol::Tftp => egui::Color32::from_rgb(200, 140, 255),
                    };
                    let r = super::widgets::icon_button(
                        ui,
                        p.icon(),
                        p.label(),
                        color,
                        Vec2::new(122.0, 64.0),
                        selected,
                        true,
                    );
                    if r.clicked() && !selected {
                        let old_default = self.session.protocol.default_port();
                        self.session.protocol = p;
                        if self.session.port == old_default {
                            self.session.port = p.default_port();
                        }
                    }
                }
            });
            ui.add_space(8.0);
            let p = self.session.protocol;
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(format!("Basic {} settings", p.label())).strong());
                egui::Grid::new("dlg_grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                    let s = &mut self.session;
                    ui.label("Remote host *");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut s.host).desired_width(230.0).hint_text("hostname or IP"),
                        );
                        ui.label("Port");
                        ui.add(egui::DragValue::new(&mut s.port).range(1..=65535));
                    });
                    ui.end_row();
                    if p != Protocol::Tftp {
                        ui.label("Username");
                        ui.add(egui::TextEdit::singleline(&mut s.user).desired_width(230.0));
                        ui.end_row();
                    }
                    match p {
                        Protocol::Ssh | Protocol::Sftp => {
                            ui.label("Authentication");
                            ui.horizontal(|ui| {
                                ui.radio_value(&mut s.auth, AuthMethod::Password, "Password");
                                ui.radio_value(&mut s.auth, AuthMethod::KeyFile, "Private key");
                            });
                            ui.end_row();
                            if s.auth == AuthMethod::Password {
                                ui.label("Password");
                                let r = ui.add(
                                    egui::TextEdit::singleline(&mut self.secrets.password)
                                        .password(true)
                                        .desired_width(230.0),
                                );
                                if self.focus_password {
                                    r.request_focus();
                                    self.focus_password = false;
                                }
                                ui.end_row();
                            } else {
                                ui.label("Private key file");
                                ui.horizontal(|ui| {
                                    ui.add(egui::TextEdit::singleline(&mut s.key_path).desired_width(230.0));
                                    if ui.button("📂").on_hover_text("Browse…").clicked()
                                        && let Some(f) = rfd::FileDialog::new()
                                            .set_directory(super::default_local_dir().join(".ssh"))
                                            .pick_file()
                                    {
                                        s.key_path = f.to_string_lossy().into_owned();
                                    }
                                });
                                ui.end_row();
                                ui.label("Passphrase");
                                let r = ui.add(
                                    egui::TextEdit::singleline(&mut self.secrets.passphrase)
                                        .password(true)
                                        .hint_text("optional")
                                        .desired_width(230.0),
                                );
                                if self.focus_password {
                                    r.request_focus();
                                    self.focus_password = false;
                                }
                                ui.end_row();
                            }
                        }
                        Protocol::Ftp => {
                            ui.label("Password");
                            let r = ui.add(
                                egui::TextEdit::singleline(&mut self.secrets.password)
                                    .password(true)
                                    .desired_width(230.0),
                            );
                            if self.focus_password {
                                r.request_focus();
                                self.focus_password = false;
                            }
                            ui.end_row();
                            ui.label("");
                            ui.checkbox(&mut s.passive, "Passive mode (PASV)");
                            ui.end_row();
                        }
                        Protocol::Tftp => {
                            ui.label("");
                            ui.weak("TFTP uses UDP, octet mode, no authentication.");
                            ui.end_row();
                        }
                    }
                    ui.label("Session name");
                    ui.add(egui::TextEdit::singleline(&mut s.name).desired_width(230.0).hint_text("optional"));
                    ui.end_row();
                });
            });
            ui.add_space(4.0);
            ui.checkbox(&mut self.save, "Save in the Sessions list (passwords are never saved)");
            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                let valid = !self.session.host.trim().is_empty()
                    && (self.session.protocol == Protocol::Tftp
                        || matches!(self.session.protocol, Protocol::Ftp)
                        || !self.session.user.trim().is_empty());
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.add_enabled(valid, egui::Button::new(RichText::new("✔ OK").strong()).fill(ACCENT)).clicked()
                    || (valid && enter)
                {
                    let mut session = self.session.clone();
                    session.host = session.host.trim().to_string();
                    session.user = session.user.trim().to_string();
                    result = Some(DialogResult::Connect {
                        session,
                        secrets: std::mem::take(&mut self.secrets),
                        save: self.save,
                        replace: self.editing,
                    });
                    close = true;
                }
                if self.save && ui.add_enabled(valid, egui::Button::new("💾 Save only")).clicked() {
                    result = Some(DialogResult::Save { session: self.session.clone(), replace: self.editing });
                    close = true;
                }
                if ui.button("✖ Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close || modal.should_close() {
            self.open = false;
            self.secrets = Secrets::default();
        }
        result
    }
}

pub fn default_user() -> String {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default()
}

fn default_key_path() -> String {
    super::default_local_dir().join(".ssh").join("id_ed25519").to_string_lossy().into_owned()
}
