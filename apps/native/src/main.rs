#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod app;
mod backend;
mod config;
mod dialogs;
mod files;
mod icons;
mod moderation;
mod settings;
fn main() -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../icons/icon.png")).ok();
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([440., 720.])
        .with_min_inner_size([340., 480.]);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    eframe::run_native(
        "CongSpeak",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Glow,
            viewport,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
