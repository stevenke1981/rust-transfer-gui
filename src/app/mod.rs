//! The egui front-end (MobaXterm-inspired layout). It owns only UI state; all
//! networking happens on background threads in [`crate::worker`].
//!
//! Layout:
//! * menu bar (Session, Tools, View, Help) and a tool bar with large buttons,
//! * collapsible left sidebar with vertical tabs: saved *Sessions* and *SFTP/Files*,
//! * main area with closable session tabs (SSH terminal, SFTP/FTP browser, TFTP form),
//! * status bar with connection state and transfer progress.

mod dialog;
mod drop_files;
mod files;
mod fonts;
mod session;
mod sidebar;
mod terminal;
mod theme;
mod widgets;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::i18n::Language;

use eframe::egui::{self, Color32, RichText, Vec2};

use crate::common::human_bytes;
use crate::config::{AuthMethod, Config, Protocol, SavedSession, Settings};
use crate::worker::{Level, SessionCommand, TftpOp, Waker};
use dialog::{DialogResult, SessionDialog};
use files::PendingDelete;
use session::{ConnState, Secrets, SessionTab};
use sidebar::{SidebarAction, SidebarTab};

/// Main application state.
pub struct TransferApp {
    config: Config,
    tabs: Vec<SessionTab>,
    /// `None` = the Home tab.
    active: Option<u64>,
    next_id: u64,
    sidebar_tab: SidebarTab,
    selected_saved: Option<usize>,
    dialog: SessionDialog,
    pending_delete: Option<PendingDelete>,
    pending_upload: Option<drop_files::PendingUpload>,
    show_settings: bool,
    settings_draft: Settings,
    cjk_fonts_available: bool,
    show_about: bool,
    notice: Option<(Level, String)>,
    waker: Waker,
    theme_applied: Option<bool>,
}

impl TransferApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::with_config(cc.egui_ctx.clone(), Config::load())
    }

    fn with_config(ctx: egui::Context, config: Config) -> Self {
        config.settings.language.install(&ctx);
        let cjk_fonts_available = fonts::install(&ctx);
        theme::apply(&ctx, config.settings.dark_mode);
        Self {
            theme_applied: Some(config.settings.dark_mode),
            settings_draft: config.settings.clone(),
            cjk_fonts_available,
            config,
            tabs: Vec::new(),
            active: None,
            next_id: 1,
            sidebar_tab: SidebarTab::Sessions,
            selected_saved: None,
            dialog: SessionDialog::default(),
            pending_delete: None,
            pending_upload: None,
            show_settings: false,
            show_about: false,
            notice: None,
            waker: Arc::new(move || ctx.request_repaint()),
        }
    }

    fn active_tab(&mut self) -> Option<&mut SessionTab> {
        let id = self.active?;
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    fn save_config(&mut self) {
        match self.config.save() {
            Ok(p) => {
                self.notice = Some((
                    Level::Info,
                    self.config
                        .settings
                        .language
                        .format("Saved settings to {path}", &[("path", &p.display().to_string())]),
                ))
            }
            Err(e) => {
                self.notice = Some((
                    Level::Error,
                    self.config
                        .settings
                        .language
                        .format("Could not save settings: {error}", &[("error", &e.to_string())]),
                ))
            }
        }
    }

    fn open_settings(&mut self) {
        self.settings_draft = self.config.settings.clone();
        self.show_settings = true;
    }

    fn apply_settings(&mut self, ctx: &egui::Context, save: impl FnOnce(&Config) -> std::io::Result<PathBuf>) {
        let mut candidate = self.config.clone();
        candidate.settings = self.settings_draft.clone();
        match save(&candidate) {
            Ok(path) => {
                self.config = candidate;
                self.config.settings.language.install(ctx);
                self.notice = Some((
                    Level::Info,
                    self.config
                        .settings
                        .language
                        .format("Saved settings to {path}", &[("path", &path.display().to_string())]),
                ));
                self.show_settings = false;
                ctx.request_repaint();
            }
            Err(error) => {
                self.notice = Some((
                    Level::Error,
                    self.config
                        .settings
                        .language
                        .format("Could not save settings: {error}", &[("error", &error.to_string())]),
                ));
            }
        }
    }

    fn open_session(&mut self, info: SavedSession, secrets: Secrets) {
        let id = self.next_id;
        self.next_id += 1;
        let tab = SessionTab::open(id, info, secrets, self.waker.clone());
        if tab.supports_files() && tab.info.protocol != Protocol::Ssh {
            self.sidebar_tab = SidebarTab::Files;
        }
        self.tabs.push(tab);
        self.active = Some(id);
    }

    /// Open a saved session; asks for the password first if one is needed.
    fn open_saved(&mut self, index: usize) {
        let Some(s) = self.config.sessions.get(index).cloned() else { return };
        let needs_secret = match s.protocol {
            Protocol::Ssh | Protocol::Sftp => s.auth == AuthMethod::Password,
            Protocol::Ftp => !s.user.is_empty() && s.user != "anonymous",
            Protocol::Tftp => false,
        };
        if needs_secret {
            self.dialog.open_saved(&s, index, false);
        } else {
            self.open_session(s, Secrets::default());
        }
    }

    fn close_tab(&mut self, id: u64) {
        if let Some(pos) = self.tabs.iter().position(|t| t.id == id) {
            let mut t = self.tabs.remove(pos);
            t.disconnect();
            // Dropping the worker handle closes its channel; the thread exits on its own.
            if self.active == Some(id) {
                self.active = self.tabs.get(pos.min(self.tabs.len().saturating_sub(1))).map(|t| t.id);
            }
        }
    }

    fn upsert_saved(&mut self, session: SavedSession, replace: Option<usize>) {
        match replace.filter(|&i| i < self.config.sessions.len()) {
            Some(i) => self.config.sessions[i] = session,
            None => self.config.sessions.push(session),
        }
        self.save_config();
    }

    // ------------------------------------------------------------------ bars

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let language = Language::from_context(ui.ctx());
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button(language.text("Session"), |ui| {
                if ui.button(language.text("➕ New session…")).clicked() {
                    self.dialog.open_new(Protocol::Ssh);
                    ui.close();
                }
                let has_active = self.active.is_some();
                if ui.add_enabled(has_active, egui::Button::new(language.text("⟳ Reconnect"))).clicked() {
                    if let Some(t) = self.active_tab()
                        && t.state() == ConnState::Disconnected
                    {
                        t.connect();
                    }
                    ui.close();
                }
                if ui.add_enabled(has_active, egui::Button::new(language.text("⏏ Disconnect"))).clicked() {
                    if let Some(t) = self.active_tab() {
                        t.disconnect();
                    }
                    ui.close();
                }
                if ui.add_enabled(has_active, egui::Button::new(language.text("✖ Close tab"))).clicked() {
                    if let Some(id) = self.active {
                        self.close_tab(id);
                    }
                    ui.close();
                }
                ui.separator();
                if ui.button(language.text("🚪 Quit")).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button(language.text("Tools"), |ui| {
                if ui.button(language.text("🧹 Clear terminal / log of current tab")).clicked() {
                    if let Some(t) = self.active_tab() {
                        t.term.lines.clear();
                        t.log.clear();
                    }
                    ui.close();
                }
                if ui.button(language.text("🔄 Reload saved sessions")).clicked() {
                    self.config.sessions = Config::load().sessions;
                    ui.close();
                }
                if ui.button(language.text("⚙ Settings…")).clicked() {
                    self.open_settings();
                    ui.close();
                }
            });
            ui.menu_button(language.text("View"), |ui| {
                if ui.checkbox(&mut self.config.settings.show_sidebar, language.text("Show sidebar")).changed() {
                    self.save_config();
                }
                if ui.checkbox(&mut self.config.settings.dark_mode, language.text("Dark theme")).changed() {
                    self.save_config();
                }
                ui.separator();
                if ui.button(language.text("⭐ Sessions panel")).clicked() {
                    self.sidebar_tab = SidebarTab::Sessions;
                    self.config.settings.show_sidebar = true;
                    ui.close();
                }
                if ui.button(language.text("📁 Files panel")).clicked() {
                    self.sidebar_tab = SidebarTab::Files;
                    self.config.settings.show_sidebar = true;
                    ui.close();
                }
                if ui.button(language.text("🏠 Home tab")).clicked() {
                    self.active = None;
                    ui.close();
                }
            });
            ui.menu_button(language.text("Help"), |ui| {
                if ui.button(language.text("ℹ About…")).clicked() {
                    self.show_about = true;
                    ui.close();
                }
            });
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let language = Language::from_context(ui.ctx());
        let size = Vec2::new(88.0, 56.0);
        let can_disc = self.active_tab().is_some_and(|t| t.worker.is_some());
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let b = |ui: &mut egui::Ui, icon: &str, label: &str, color: Color32, enabled: bool, tip: &str| {
                widgets::icon_button(ui, icon, label, color, size, false, enabled).on_hover_text(tip).clicked()
            };
            if b(
                ui,
                "🖥",
                language.text("Session"),
                Color32::from_rgb(90, 170, 255),
                true,
                language.text("New session (Ctrl+N)"),
            ) {
                self.dialog.open_new(Protocol::Ssh);
            }
            if b(ui, "📂", "SFTP", Color32::from_rgb(255, 180, 60), true, language.text("New SFTP session")) {
                self.dialog.open_new(Protocol::Sftp);
            }
            if b(ui, "🌐", "FTP", Color32::from_rgb(110, 210, 120), true, language.text("New FTP session")) {
                self.dialog.open_new(Protocol::Ftp);
            }
            if b(ui, "📡", "TFTP", Color32::from_rgb(200, 140, 255), true, language.text("New TFTP session")) {
                self.dialog.open_new(Protocol::Tftp);
            }
            ui.separator();
            if b(
                ui,
                "⏏",
                language.text("Disconnect"),
                Color32::from_rgb(240, 100, 100),
                can_disc,
                language.text("Disconnect the current session"),
            ) && let Some(t) = self.active_tab()
            {
                t.disconnect();
            }
            if b(ui, "⚙", language.text("Settings"), Color32::from_rgb(190, 190, 200), true, language.text("Settings"))
            {
                self.open_settings();
            }
            if b(
                ui,
                "◧",
                language.text("Sidebar"),
                Color32::from_rgb(120, 200, 230),
                true,
                language.text("Show / hide the left sidebar"),
            ) {
                self.config.settings.show_sidebar = !self.config.settings.show_sidebar;
            }
            if b(ui, "❓", language.text("Help"), Color32::from_rgb(120, 200, 230), true, language.text("About")) {
                self.show_about = true;
            }
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let language = Language::from_context(ui.ctx());
        let n = self.tabs.len();
        let notice = self.notice.clone();
        ui.horizontal(|ui| {
            match self.active_tab() {
                None => {
                    ui.label(RichText::new(language.text("🏠 Home")).strong());
                }
                Some(t) => {
                    let (color, text) = match t.state() {
                        ConnState::Connected => (theme::OK_GREEN, language.text("Connected")),
                        ConnState::Connecting => (theme::WARN_YELLOW, language.text("Connecting…")),
                        ConnState::Disconnected => (theme::ERR_RED, language.text("Disconnected")),
                        ConnState::Ready => (theme::ACCENT_LIGHT, language.text("Ready (UDP)")),
                    };
                    widgets::status_dot(ui, color);
                    ui.colored_label(color, text);
                    ui.separator();
                    ui.label(format!("{} {}:{}", t.info.protocol.label(), t.info.host, t.info.port));
                    if !t.info.user.is_empty() && t.info.protocol != Protocol::Tftp {
                        ui.label(language.format("user {user}", &[("user", &t.info.user)]));
                    }
                    if let Some(b) = &t.busy {
                        ui.separator();
                        ui.spinner();
                        ui.label(language.text(b));
                        if let Some((done, total)) = t.progress {
                            match total {
                                Some(tot) if tot > 0 => {
                                    let f = (done as f32 / tot as f32).clamp(0.0, 1.0);
                                    ui.add(egui::ProgressBar::new(f).desired_width(220.0).fill(theme::ACCENT).text(
                                        format!("{} / {} ({:.0}%)", human_bytes(done), human_bytes(tot), f * 100.0),
                                    ));
                                }
                                _ => {
                                    ui.label(human_bytes(done));
                                }
                            }
                        }
                    } else if let Some(last) = t.log.last() {
                        ui.separator();
                        let c = level_color(ui, last.level);
                        ui.add(egui::Label::new(RichText::new(&last.text).color(c)).truncate());
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(concat!("v", env!("CARGO_PKG_VERSION")));
                ui.separator();
                ui.label(if n == 1 {
                    language.text("1 session").to_string()
                } else {
                    language.format("{count} sessions", &[("count", &n.to_string())])
                });
                if let Some((level, msg)) = notice {
                    ui.separator();
                    let c = level_color(ui, level);
                    ui.add(egui::Label::new(RichText::new(msg).small().color(c)).truncate());
                }
            });
        });
    }

    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let language = Language::from_context(ui.ctx());
        let mut close = None;
        egui::ScrollArea::horizontal().id_salt("tab_strip").show(ui, |ui| {
            ui.horizontal(|ui| {
                let home = egui::Button::new(language.text("🏠 Home")).selected(self.active.is_none());
                if ui.add(home).clicked() {
                    self.active = None;
                }
                for t in &self.tabs {
                    let dot = match t.state() {
                        ConnState::Connected => theme::OK_GREEN,
                        ConnState::Connecting => theme::WARN_YELLOW,
                        ConnState::Disconnected => theme::ERR_RED,
                        ConnState::Ready => theme::ACCENT_LIGHT,
                    };
                    let selected = self.active == Some(t.id);
                    egui::Frame::NONE
                        .fill(if selected { theme::ACCENT } else { ui.visuals().faint_bg_color })
                        .corner_radius(4.0)
                        .inner_margin(egui::Margin::symmetric(6, 2))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                widgets::status_dot(ui, dot);
                                let label = format!("{} {}", t.info.protocol.icon(), t.title());
                                let text = if selected {
                                    RichText::new(label).color(Color32::WHITE)
                                } else {
                                    RichText::new(label)
                                };
                                if ui.add(egui::Button::new(text).frame(false)).clicked() {
                                    self.active = Some(t.id);
                                }
                                if ui
                                    .add(egui::Button::new("✖").frame(false).small())
                                    .on_hover_text(language.text("Close"))
                                    .clicked()
                                {
                                    close = Some(t.id);
                                }
                            });
                        });
                }
            });
        });
        if let Some(id) = close {
            self.close_tab(id);
        }
    }

    // ------------------------------------------------------------------ main area

    fn home(&mut self, ui: &mut egui::Ui) {
        let language = Language::from_context(ui.ctx());
        ui.vertical_centered(|ui| {
            ui.add_space(30.0);
            ui.label(RichText::new("Rust Transfer GUI").size(30.0).strong().color(theme::ACCENT_LIGHT));
            ui.label(language.text("FTP · SFTP · SSH · TFTP file transfer client"));
            ui.add_space(20.0);
            if ui
                .add(
                    egui::Button::new(RichText::new(language.text("➕  Start new session")).size(18.0))
                        .min_size(Vec2::new(260.0, 44.0))
                        .fill(theme::ACCENT),
                )
                .clicked()
            {
                self.dialog.open_new(Protocol::Ssh);
            }
            ui.add_space(20.0);
            if !self.config.sessions.is_empty() {
                ui.label(RichText::new(language.text("Saved sessions")).strong());
                let mut open = None;
                for (i, s) in self.config.sessions.iter().enumerate().take(10) {
                    if ui
                        .link(format!(
                            "{} {}  ({} {}:{})",
                            s.protocol.icon(),
                            s.display_name(),
                            s.protocol.label(),
                            s.host,
                            s.port
                        ))
                        .clicked()
                    {
                        open = Some(i);
                    }
                }
                if let Some(i) = open {
                    self.open_saved(i);
                }
            }
            ui.add_space(20.0);
            ui.weak(
                language.text(
                    "Tip: double-click a saved session in the left sidebar to connect. Passwords are never stored.",
                ),
            );
        });
    }

    fn session_view(ui: &mut egui::Ui, tab: &mut SessionTab) -> Option<PendingDelete> {
        let language = Language::from_context(ui.ctx());
        // Header with connection info and reconnect.
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} {}", tab.info.protocol.icon(), tab.title())).strong().size(15.0));
            ui.weak(format!("{}://{}:{}", tab.info.protocol.label().to_lowercase(), tab.info.host, tab.info.port));
            if tab.state() == ConnState::Disconnected && ui.button(language.text("⟳ Reconnect")).clicked() {
                tab.connect();
            }
        });
        ui.separator();
        let mut pending = None;
        match tab.info.protocol {
            Protocol::Ssh => terminal::show(ui, tab),
            Protocol::Sftp | Protocol::Ftp => {
                egui::Panel::bottom(egui::Id::new(("log_panel", tab.id))).resizable(true).default_size(150.0).show(
                    ui,
                    |ui| {
                        log_view(ui, tab);
                    },
                );
                egui::Panel::bottom(egui::Id::new(("xfer_panel", tab.id))).show(ui, |ui| {
                    ui.add_space(4.0);
                    files::transfer_form(ui, tab);
                    ui.add_space(4.0);
                });
                egui::CentralPanel::default().show(ui, |ui| {
                    pending = files::toolbar(ui, tab);
                    files::listing(ui, tab, false);
                });
            }
            Protocol::Tftp => {
                tftp_form(ui, tab);
                ui.separator();
                log_view(ui, tab);
            }
        }
        pending
    }

    // ------------------------------------------------------------------ windows

    fn windows(&mut self, ctx: &egui::Context) {
        let language = self.config.settings.language;
        if let Some(res) = self.dialog.show(ctx) {
            match res {
                DialogResult::Connect { session, secrets, save, replace } => {
                    if save {
                        self.upsert_saved(session.clone(), replace);
                    }
                    self.open_session(session, secrets);
                }
                DialogResult::Save { session, replace } => self.upsert_saved(session, replace),
            }
        }

        if let Some(p) = &self.pending_delete {
            let mut decision = None;
            let what = if p.is_dir { language.text("folder") } else { language.text("file") };
            let name = p.name.clone();
            let m = egui::Modal::new(egui::Id::new("confirm_delete")).show(ctx, |ui| {
                ui.heading(language.text("Confirm delete"));
                ui.label(language.format(
                    "Delete remote {kind} \"{name}\"? This cannot be undone.",
                    &[("kind", what), ("name", &name)],
                ));
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(language.text("🗑 Delete")).fill(theme::ERR_RED)).clicked() {
                        decision = Some(true);
                    }
                    if ui.button(language.text("Cancel")).clicked() {
                        decision = Some(false);
                    }
                });
            });
            if m.should_close() && decision.is_none() {
                decision = Some(false);
            }
            if let Some(yes) = decision {
                if let Some(p) = self.pending_delete.take()
                    && yes
                    && let Some(t) = self.tabs.iter_mut().find(|t| t.id == p.tab_id)
                {
                    t.send(SessionCommand::Delete { name: p.name, is_dir: p.is_dir });
                }
                self.pending_delete = None;
            }
        }

        let mut open = self.show_settings;
        let mut apply = false;
        let mut cancel = false;
        egui::Window::new(language.text("⚙ Settings"))
            .id(egui::Id::new("settings_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Language / 語言 / 言語");
                    egui::ComboBox::from_id_salt("language_selector")
                        .selected_text(self.settings_draft.language.name())
                        .show_ui(ui, |ui| {
                            for choice in Language::ALL {
                                ui.selectable_value(&mut self.settings_draft.language, choice, choice.name());
                            }
                        });
                });
                ui.checkbox(&mut self.settings_draft.dark_mode, language.text("Dark theme (blue accents)"));
                ui.checkbox(&mut self.settings_draft.show_sidebar, language.text("Show sidebar"));
                ui.weak(language.text("Changes take effect after Apply; no restart is needed."));
                if !self.cjk_fonts_available {
                    ui.colored_label(
                        theme::WARN_YELLOW,
                        "CJK font unavailable. Install Noto Sans CJK or set RUST_TRANSFER_GUI_FONT.",
                    );
                }
                ui.separator();
                ui.label(language.text("Saved sessions are stored in:"));
                ui.monospace(
                    Config::path()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| language.text("(unavailable)").into()),
                );
                ui.weak(language.text("Passwords and key passphrases are never written to disk."));
                ui.horizontal(|ui| {
                    apply = ui.button(language.text("Apply")).clicked();
                    cancel = ui.button(language.text("Cancel")).clicked();
                });
            });
        self.show_settings = open && !cancel;
        if apply {
            self.apply_settings(ctx, Config::save);
        }

        let mut open = self.show_about;
        egui::Window::new(language.text("ℹ About"))
            .id(egui::Id::new("about_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(RichText::new(concat!("Rust Transfer GUI v", env!("CARGO_PKG_VERSION"))).strong());
                ui.label(language.text("A small FTP / SFTP / SSH / TFTP client written in Rust with egui."));
                ui.label(language.text(
                    "Layout inspired by classic multi-protocol terminal tools; not affiliated with any of them.",
                ));
                ui.label("© 2026 Ke Sheng Da — MIT License");
            });
        self.show_about = open;
    }
}

impl eframe::App for TransferApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.config.settings.language.install(&ctx);
        if self.theme_applied != Some(self.config.settings.dark_mode) {
            theme::apply(&ctx, self.config.settings.dark_mode);
            self.theme_applied = Some(self.config.settings.dark_mode);
        }
        for t in &mut self.tabs {
            t.poll();
        }
        if self.pending_upload.is_none() && ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::N)) {
            self.dialog.open_new(Protocol::Ssh);
        }
        if self.pending_upload.is_none()
            && ctx.input(|input| input.modifiers.command && input.key_pressed(egui::Key::Comma))
        {
            self.open_settings();
        }

        let dark = self.config.settings.dark_mode;
        egui::Panel::top("menu_bar").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::top("tool_bar")
            .frame(egui::Frame::NONE.fill(theme::toolbar_fill(dark)).inner_margin(egui::Margin::symmetric(6, 4)))
            .show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui));

        let mut expanded = self.config.settings.show_sidebar;
        egui::Panel::left("sidebar_strip")
            .resizable(false)
            .exact_size(66.0)
            .frame(egui::Frame::NONE.fill(theme::strip_fill(dark)).inner_margin(egui::Margin::symmetric(5, 0)))
            .show(ui, |ui| sidebar::strip(ui, &mut self.sidebar_tab, &mut expanded));
        if expanded != self.config.settings.show_sidebar {
            self.config.settings.show_sidebar = expanded;
        }

        let mut action = None;
        if expanded {
            egui::Panel::left("sidebar")
                .resizable(true)
                .default_size(300.0)
                .min_size(200.0)
                .max_size(ui.available_width() * 0.45)
                .show(ui, |ui| {
                    ui.add_space(4.0);
                    action = match self.sidebar_tab {
                        SidebarTab::Sessions => {
                            sidebar::sessions_panel(ui, &self.config.sessions, &mut self.selected_saved)
                        }
                        SidebarTab::Files => {
                            let id = self.active;
                            let tab = id.and_then(|id| self.tabs.iter_mut().find(|t| t.id == id));
                            sidebar::files_panel(ui, tab)
                        }
                    };
                });
        }
        match action {
            Some(SidebarAction::New) => self.dialog.open_new(Protocol::Ssh),
            Some(SidebarAction::Open(i)) => self.open_saved(i),
            Some(SidebarAction::Edit(i)) => {
                if let Some(s) = self.config.sessions.get(i).cloned() {
                    self.dialog.open_saved(&s, i, true);
                }
            }
            Some(SidebarAction::Delete(i)) => {
                if i < self.config.sessions.len() {
                    self.config.sessions.remove(i);
                    self.selected_saved = None;
                    self.save_config();
                }
            }
            Some(SidebarAction::ConfirmDelete(p)) => self.pending_delete = Some(p),
            None => {}
        }

        egui::CentralPanel::default().show(ui, |ui| {
            self.tab_strip(ui);
            ui.separator();
            let id = self.active;
            match id.and_then(|id| self.tabs.iter_mut().find(|t| t.id == id)) {
                Some(tab) => {
                    if let Some(p) = Self::session_view(ui, tab) {
                        self.pending_delete = Some(p);
                    }
                }
                None => {
                    self.active = None;
                    self.home(ui);
                }
            }
        });

        self.windows(&ctx);
        self.handle_file_drop(&ctx);
        self.upload_confirmation(&ctx);

        if self.tabs.iter().any(SessionTab::is_busy) {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}

fn level_color(ui: &egui::Ui, level: Level) -> Color32 {
    match level {
        Level::Info => ui.visuals().text_color(),
        Level::Success => theme::OK_GREEN,
        Level::Error => theme::ERR_RED,
    }
}

fn log_view(ui: &mut egui::Ui, tab: &mut SessionTab) {
    let language = Language::from_context(ui.ctx());
    ui.horizontal(|ui| {
        ui.strong(language.text("📝 Session log"));
        if ui.small_button(language.text("Clear")).clicked() {
            tab.log.clear();
        }
    });
    egui::ScrollArea::vertical().id_salt(("log", tab.id)).stick_to_bottom(true).auto_shrink([false, false]).show(
        ui,
        |ui| {
            for line in &tab.log {
                let s = line.at.as_secs();
                let c = level_color(ui, line.level);
                ui.label(
                    RichText::new(format!(
                        "[{:02}:{:02}:{:02}] {}",
                        s / 3600,
                        s / 60 % 60,
                        s % 60,
                        language.text(&line.text)
                    ))
                    .monospace()
                    .color(c),
                );
            }
        },
    );
}

fn tftp_form(ui: &mut egui::Ui, tab: &mut SessionTab) {
    let language = Language::from_context(ui.ctx());
    ui.weak(language.text("Drop one file to prepare a TFTP upload."));
    let busy = tab.tftp.events.is_some();
    let mut op = None;
    ui.push_id(("tftp", tab.id), |ui| {
        let f = &mut tab.tftp;
        ui.label(language.text("Remote file"));
        ui.add(
            egui::TextEdit::singleline(&mut f.remote)
                .desired_width(ui.available_width())
                .hint_text(language.text("e.g. firmware.bin")),
        );
        ui.label(
            RichText::new(language.format("mode: {mode}", &[("mode", crate::tftp::MODE_OCTET)])).monospace().weak(),
        );
        ui.label(language.text("Local file"));
        ui.add(
            egui::TextEdit::singleline(&mut f.local)
                .desired_width(ui.available_width())
                .hint_text(language.text("local path")),
        );
        ui.horizontal_wrapped(|ui| {
            if ui.button(language.text("📂 Open…")).on_hover_text(language.text("Pick a file to upload")).clicked()
                && let Some(p) = rfd::FileDialog::new().set_directory(default_local_dir()).pick_file()
            {
                if f.remote.trim().is_empty() {
                    f.remote = files::file_name_of(&p);
                }
                f.local = p.to_string_lossy().into_owned();
            }
            if ui
                .button(language.text("💾 Save as…"))
                .on_hover_text(language.text("Choose where to store a download"))
                .clicked()
                && let Some(p) = rfd::FileDialog::new()
                    .set_directory(default_local_dir())
                    .set_file_name(f.remote.rsplit(['/', '\\']).next().unwrap_or_default())
                    .save_file()
            {
                f.local = p.to_string_lossy().into_owned();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(language.text("Timeout (s)"));
            ui.add(egui::DragValue::new(&mut f.timeout_secs).range(1..=60));
            ui.label(language.text("Retries"));
            ui.add(egui::DragValue::new(&mut f.retries).range(0..=50));
        });
    });
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        let ready = !busy && !tab.tftp.remote.trim().is_empty();
        if ui
            .add_enabled(
                ready,
                egui::Button::new(RichText::new(language.text("⤵ Get (download)")).size(15.0))
                    .min_size(Vec2::new(150.0, 32.0)),
            )
            .clicked()
        {
            op = Some(TftpOp::Get);
        }
        if ui
            .add_enabled(
                ready,
                egui::Button::new(RichText::new(language.text("⤴ Put (upload)")).size(15.0))
                    .min_size(Vec2::new(150.0, 32.0)),
            )
            .clicked()
        {
            op = Some(TftpOp::Put);
        }
    });
    if let Some(op) = op {
        tab.start_tftp(op);
    }
    ui.weak(language.text("TFTP (RFC 1350) has no authentication and no directory listing."));
}

/// The user's home directory (or the current directory as a fallback).
pub(crate) fn default_local_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}
