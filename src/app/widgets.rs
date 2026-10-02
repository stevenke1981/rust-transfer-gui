//! Small custom widgets: large "icon over label" buttons for the tool bar,
//! the sidebar tab strip and the protocol picker.

use eframe::egui::{self, Align2, Color32, FontId, Response, Sense, Stroke, Vec2};

use super::theme::ACCENT;

/// A big tool-bar style button with a large glyph above a small label.
pub fn icon_button(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    icon_color: Color32,
    size: Vec2,
    selected: bool,
    enabled: bool,
) -> Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let hovered = enabled && resp.hovered();
        if selected {
            painter.rect_filled(rect, 5.0, ACCENT);
        } else if hovered {
            painter.rect_filled(rect, 5.0, ui.visuals().widgets.hovered.weak_bg_fill);
            painter.rect_stroke(rect, 5.0, Stroke::new(1.0, ACCENT), egui::StrokeKind::Inside);
        }
        let text_color = if !enabled {
            ui.visuals().weak_text_color()
        } else if selected {
            Color32::WHITE
        } else {
            ui.visuals().text_color()
        };
        let glyph_color = if !enabled {
            ui.visuals().weak_text_color()
        } else if selected {
            Color32::WHITE
        } else {
            icon_color
        };
        let icon_size = (size.y * 0.42).clamp(14.0, 30.0);
        painter.text(
            egui::pos2(rect.center().x, rect.top() + size.y * 0.38),
            Align2::CENTER_CENTER,
            icon,
            FontId::proportional(icon_size),
            glyph_color,
        );
        painter.text(
            egui::pos2(rect.center().x, rect.bottom() - size.y * 0.18),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(12.5),
            text_color,
        );
    }
    if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
}

/// A small filled circle used as a connection-state indicator.
pub fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, color);
}
