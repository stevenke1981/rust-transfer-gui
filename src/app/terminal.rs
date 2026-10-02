//! Terminal-style view for SSH tabs: black background, monospace, coloured output.

use eframe::egui::{self, RichText, TextEdit};

use super::session::{SessionTab, TermKind};
use super::theme::{TERM_BG, TERM_ERR, TERM_GREEN, TERM_INFO, TERM_TEXT};

pub fn show(ui: &mut egui::Ui, tab: &mut SessionTab) {
    egui::Frame::NONE.fill(TERM_BG).inner_margin(8.0).corner_radius(4.0).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        let input_height = 26.0;
        egui::ScrollArea::vertical()
            .id_salt(("term", tab.id))
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .max_height((ui.available_height() - input_height).max(40.0))
            .show(ui, |ui| {
                ui.style_mut().spacing.item_spacing.y = 1.0;
                for line in &tab.term.lines {
                    let color = match line.kind {
                        TermKind::Prompt => TERM_GREEN,
                        TermKind::Stdout => TERM_TEXT,
                        TermKind::Stderr => TERM_ERR,
                        TermKind::Info => TERM_INFO,
                    };
                    let text = if line.text.is_empty() { " " } else { line.text.as_str() };
                    ui.add(egui::Label::new(RichText::new(text).monospace().color(color)).wrap());
                }
            });
        ui.horizontal(|ui| {
            ui.label(RichText::new(tab.prompt()).monospace().color(TERM_GREEN));
            let resp = ui.add(
                TextEdit::singleline(&mut tab.term.input)
                    .id_salt(("term_input", tab.id))
                    .font(egui::TextStyle::Monospace)
                    .text_color(TERM_TEXT)
                    .background_color(TERM_BG)
                    .frame(egui::Frame::NONE)
                    .desired_width(f32::INFINITY)
                    .hint_text(if tab.connected { "" } else { "(not connected)" }),
            );
            if tab.term.want_focus {
                resp.request_focus();
                tab.term.want_focus = false;
            }
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                tab.run_terminal_line();
                tab.term.want_focus = true;
            }
            if resp.has_focus() {
                let (up, down) = ui.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown)));
                let hist = &tab.term.history;
                if up && !hist.is_empty() {
                    let pos = tab.term.history_pos.map_or(hist.len() - 1, |p| p.saturating_sub(1));
                    tab.term.history_pos = Some(pos);
                    tab.term.input = hist[pos].clone();
                } else if down && let Some(p) = tab.term.history_pos {
                    if p + 1 < hist.len() {
                        tab.term.history_pos = Some(p + 1);
                        tab.term.input = hist[p + 1].clone();
                    } else {
                        tab.term.history_pos = None;
                        tab.term.input.clear();
                    }
                }
            }
        });
    });
}
