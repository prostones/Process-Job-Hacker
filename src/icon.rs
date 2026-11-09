use std::sync::Arc;

use eframe::egui;
use image::imageops::FilterType;

pub fn window_icon() -> anyhow::Result<Arc<egui::IconData>> {
    let image = image::load_from_memory(include_bytes!("../assets/icon.png"))?.to_rgba8();
    let (width, height) = image.dimensions();
    Ok(Arc::new(egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    }))
}

pub fn tray_icon() -> anyhow::Result<tray_icon::Icon> {
    let image = image::load_from_memory(include_bytes!("../assets/tray-icon.png"))?.to_rgba8();
    let resized = image::imageops::resize(&image, 32, 32, FilterType::Lanczos3);
    tray_icon::Icon::from_rgba(resized.into_raw(), 32, 32)
        .map_err(|error| anyhow::anyhow!("tray icon: {error}"))
}
