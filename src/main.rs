// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use rust_transfer_gui::app::TransferApp;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_drag_and_drop(true)
            .with_title("Rust Transfer GUI — FTP / SFTP / TFTP")
            .with_inner_size([1000.0, 760.0])
            .with_min_inner_size([720.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native("rust-transfer-gui", options, Box::new(|cc| Ok(Box::new(TransferApp::new(cc)))))
}
