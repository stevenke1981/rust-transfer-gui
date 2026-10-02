//! Left sidebar: a vertical tab strip with "Sessions" and "SFTP / Files" panels.

use eframe::egui::{self, RichText, Vec2};

use super::files::{self, PendingDelete};
use super::session::SessionTab;
use crate::config::SavedSession;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SidebarTab {
    Sessions,
    Files,
}

pub enum SidebarAction {
    New,
    Open(usize),
    Edit(usize),
    Delete(usize),
    ConfirmDelete(PendingDelete),
}

/// The narrow vertical tab strip. Clicking the active tab collapses the sidebar.
pub fn strip(ui: &mut egui::Ui, current: &mut SidebarTab, expanded: &mut bool) {
    ui.add_space(6.0);
    for (tab, icon, label, color) in [
        (SidebarTab::Sessions, "⭐", "Sessions", egui::Color32::from_rgb(255, 205, 70)),
        (SidebarTab::Files, "📁", "Files", egui::Color32::from_rgb(255, 180, 60)),
    ] {
        let selected = *expanded && *current == tab;
        let r = super::widgets::icon_button(ui, icon, label, color, Vec2::new(56.0, 56.0), selected, true);
        if r.clicked() {
            if selected {
                *expanded = false;
            } else {
                *current = tab;
                *expanded = true;
            }
        }
        ui.add_space(4.0);
    }
}

pub fn sessions_panel(
    ui: &mut egui::Ui,
    saved: &[SavedSession],
    selected: &mut Option<usize>,
) -> Option<SidebarAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        ui.strong("⭐ User sessions");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("➕").on_hover_text("New session").clicked() {
                action = Some(SidebarAction::New);
            }
        });
    });
    ui.separator();
    if saved.is_empty() {
        ui.weak("No saved sessions yet.\nClick ➕ or the Session button to create one.");
    }
    egui::ScrollArea::vertical().id_salt("saved_sessions").auto_shrink([false, false]).show(ui, |ui| {
        for (i, s) in saved.iter().enumerate() {
            let text = format!("{} {}", s.protocol.icon(), s.display_name());
            let r = ui.selectable_label(*selected == Some(i), RichText::new(text).strong()).on_hover_text(format!(
                "{} {}:{}{}\nDouble-click to connect, right-click for more.",
                s.protocol.label(),
                s.host,
                s.port,
                if s.user.is_empty() { String::new() } else { format!("  user: {}", s.user) }
            ));
            if r.clicked() {
                *selected = Some(i);
            }
            if r.double_clicked() {
                action = Some(SidebarAction::Open(i));
            }
            r.context_menu(|ui| {
                if ui.button("🔌 Connect").clicked() {
                    action = Some(SidebarAction::Open(i));
                    ui.close();
                }
                if ui.button("✏ Edit…").clicked() {
                    action = Some(SidebarAction::Edit(i));
                    ui.close();
                }
                if ui.button("🗑 Delete").clicked() {
                    action = Some(SidebarAction::Delete(i));
                    ui.close();
                }
            });
            ui.label(RichText::new(format!("   {}  {}:{}", s.protocol.label(), s.host, s.port)).small().weak());
        }
    });
    action
}

pub fn files_panel(ui: &mut egui::Ui, tab: Option<&mut SessionTab>) -> Option<SidebarAction> {
    let Some(tab) = tab.filter(|t| t.supports_files()) else {
        ui.strong("📁 Remote files");
        ui.separator();
        ui.weak("Open an SSH, SFTP or FTP session to browse remote files here.");
        return None;
    };
    ui.strong(format!("📁 {} — {}", tab.info.protocol.label(), tab.title()));
    let pending = files::toolbar(ui, tab);
    ui.separator();
    files::listing(ui, tab, true);
    pending.map(SidebarAction::ConfirmDelete)
}
