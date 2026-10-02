//! Dark theme with blue accents (MobaXterm-inspired) plus a matching light theme.

use eframe::egui::{self, Color32, Stroke, Theme, Visuals};

pub const ACCENT: Color32 = Color32::from_rgb(0, 122, 204);
pub const ACCENT_LIGHT: Color32 = Color32::from_rgb(64, 160, 230);
pub const OK_GREEN: Color32 = Color32::from_rgb(80, 200, 120);
pub const ERR_RED: Color32 = Color32::from_rgb(240, 90, 90);
pub const WARN_YELLOW: Color32 = Color32::from_rgb(230, 200, 80);

pub const TERM_BG: Color32 = Color32::from_rgb(0, 0, 0);
pub const TERM_GREEN: Color32 = Color32::from_rgb(90, 230, 110);
pub const TERM_TEXT: Color32 = Color32::from_rgb(225, 225, 225);
pub const TERM_ERR: Color32 = Color32::from_rgb(255, 110, 110);
pub const TERM_INFO: Color32 = Color32::from_rgb(110, 190, 255);

fn dark() -> Visuals {
    let mut v = Visuals::dark();
    v.panel_fill = Color32::from_rgb(32, 34, 37);
    v.window_fill = Color32::from_rgb(40, 43, 47);
    v.extreme_bg_color = Color32::from_rgb(20, 21, 23);
    v.faint_bg_color = Color32::from_rgb(38, 40, 44);
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = ACCENT_LIGHT;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_LIGHT);
    v.widgets.active.bg_fill = ACCENT;
    v.widgets.active.weak_bg_fill = ACCENT;
    v
}

fn light() -> Visuals {
    let mut v = Visuals::light();
    v.selection.bg_fill = ACCENT_LIGHT;
    v.selection.stroke = Stroke::new(1.0, Color32::BLACK);
    v.hyperlink_color = ACCENT;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    v
}

/// Install both visuals and select the preferred theme.
pub fn apply(ctx: &egui::Context, dark_mode: bool) {
    ctx.set_visuals_of(Theme::Dark, dark());
    ctx.set_visuals_of(Theme::Light, light());
    ctx.set_theme(if dark_mode { Theme::Dark } else { Theme::Light });
}

/// Fill colour of the tool bar strip.
pub fn toolbar_fill(dark_mode: bool) -> Color32 {
    if dark_mode { Color32::from_rgb(24, 40, 62) } else { Color32::from_rgb(214, 230, 248) }
}

/// Fill colour of the narrow vertical tab strip of the sidebar.
pub fn strip_fill(dark_mode: bool) -> Color32 {
    if dark_mode { Color32::from_rgb(22, 30, 42) } else { Color32::from_rgb(200, 214, 232) }
}
