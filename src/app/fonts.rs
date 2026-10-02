use std::path::PathBuf;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use skrifa::MetadataProvider;

fn candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = std::env::var_os("RUST_TRANSFER_GUI_FONT") {
        paths.push(PathBuf::from(path));
    }
    if cfg!(windows) {
        let directory = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:/Windows"));
        paths.push(directory.join("Fonts/msjh.ttc"));
        paths.push(directory.join("Fonts/YuGothR.ttc"));
    } else if cfg!(target_os = "macos") {
        paths.push(PathBuf::from("/System/Library/Fonts/PingFang.ttc"));
        paths.push(PathBuf::from("/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc"));
    } else {
        paths.push(PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"));
        paths.push(PathBuf::from("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc"));
    }
    paths
}

pub fn install(ctx: &egui::Context) -> bool {
    let mut fonts = FontDefinitions::default();
    let mut has_chinese = false;
    let mut has_japanese = false;
    for (index, path) in candidates().into_iter().enumerate() {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let Ok(face) = skrifa::FontRef::from_index(&bytes, 0) else { continue };
        has_chinese |= "繁體中文".chars().all(|character| face.charmap().map(character).is_some());
        has_japanese |= "日本語あア".chars().all(|character| face.charmap().map(character).is_some());
        let name = format!("cjk_fallback_{index}");
        fonts.font_data.insert(name.clone(), FontData::from_owned(bytes).into());
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(name.clone());
        }
        if has_chinese && has_japanese {
            break;
        }
    }
    ctx.set_fonts(fonts);
    has_chinese && has_japanese
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires installed Chinese and Japanese system fonts"]
    fn system_fonts_render_chinese_and_japanese_glyphs() {
        let ctx = egui::Context::default();
        assert!(install(&ctx), "Install a CJK font or set RUST_TRANSFER_GUI_FONT");
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.ctx().fonts_mut(|fonts| {
                for family in [FontFamily::Proportional, FontFamily::Monospace] {
                    let font = egui::FontId::new(16.0, family);
                    for character in "繁體中文連線設定檔案日本語アップロード".chars() {
                        assert!(fonts.has_glyph(&font, character), "Missing glyph: {character}");
                    }
                }
            });
        });
        output.textures_delta.clear();
    }
}
