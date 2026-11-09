#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod apply;
mod config;
mod fonts;
mod guard;
mod icon;
mod lifecycle;
mod logging;
mod model;
mod monitor;
mod sankey;
mod sankey_window;
mod tray;
mod ui;
mod win;

use eframe::egui;
use win::single_instance::SingleInstanceGuard;

fn main() -> eframe::Result {
    let args = std::env::args().collect::<Vec<_>>();

    // 预览窗口复用同一个 exe，但必须绕开单实例互斥和主程序初始化。
    if let Some(path) = guard::sankey_preview_path(&args) {
        return sankey_window::run(path);
    }

    logging::init();

    if guard::quit_running_if_requested(&args) {
        return Ok(());
    }

    if guard::run_if_requested(&args) {
        return Ok(());
    }
    if guard::clear_persisted_if_requested(&args) {
        return Ok(());
    }

    let single_instance = match SingleInstanceGuard::acquire() {
        Ok(Some(guard)) => guard,
        Ok(None) => {
            log::info!("another instance is already running");
            return Ok(());
        }
        Err(error) => {
            log::error!("single instance setup failed: {error}");
            logging::show_error_message("Process Job Hacker", &format!("启动失败：{error}"));
            return Ok(());
        }
    };

    let safe_mode = guard::safe_mode(&args);
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Process Job Hacker")
        .with_inner_size([1280.0, 840.0])
        .with_min_inner_size([980.0, 640.0]);
    match icon::window_icon() {
        Ok(icon_data) => viewport = viewport.with_icon(icon_data),
        Err(error) => log::warn!("window icon load failed: {error}"),
    }
    let native_options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    eframe::run_native(
        "Process Job Hacker",
        native_options,
        Box::new(move |cc| {
            single_instance.start_show_listener(cc.egui_ctx.clone());
            Ok(Box::new(app::PjhApp::new(cc, single_instance, safe_mode)))
        }),
    )
}
