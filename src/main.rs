#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod theme;

use eframe::egui;
use picsync::icon;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 866.0])
            .with_min_inner_size([900.0, 700.0])
            .with_icon(std::sync::Arc::new(app_icon())),
        ..Default::default()
    };
    eframe::run_native(
        "Picsync · 图片时光归档",
        options,
        Box::new(|cc| Ok(Box::new(app::PicsyncApp::new(cc)))),
    )
}

/// The window icon comes from the same renderer as the executable icon, so the
/// title bar, taskbar and Explorer all show the identical mark.
fn app_icon() -> egui::IconData {
    let size = 128u32;
    egui::IconData {
        rgba: icon::render(size),
        width: size,
        height: size,
    }
}
