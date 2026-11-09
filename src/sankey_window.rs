use std::path::PathBuf;

use eframe::egui;

use crate::sankey::{SVG_HEIGHT, SVG_WIDTH};

/// 独立的亲和性桑基图预览窗口。
///
/// 由主程序通过 `--sankey-preview <svg路径>` 启动，不参与单实例、托盘和策略管理。
pub fn run(path: PathBuf) -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("亲和性桑基图")
        .with_inner_size([860.0, 600.0])
        .with_min_inner_size([480.0, 320.0]);
    if let Ok(icon) = crate::icon::window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let native_options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    let uri = format!("file:///{}", path.display());

    eframe::run_native(
        "亲和性桑基图",
        native_options,
        Box::new(move |cc| {
            crate::fonts::install(&cc.egui_ctx);
            cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(SankeyPreviewApp {
                uri,
                theme_initialized: false,
                theme_focus_synced: false,
            }))
        }),
    )
}

struct SankeyPreviewApp {
    uri: String,
    /// 首帧是否已发送过深色主题命令。
    theme_initialized: bool,
    /// 窗口获得焦点后是否已再次同步深色标题栏。
    theme_focus_synced: bool,
}

impl eframe::App for SankeyPreviewApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.theme_initialized {
            // 子窗口由主窗口拉起，首次可能拿不到焦点；先主动同步一次非客户区。
            ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
            self.theme_initialized = true;
        }
        let focused = ctx.input(|input| input.viewport().focused.unwrap_or(false));
        if focused && !self.theme_focus_synced {
            // 与主窗体一致：首帧的 SetTheme 可能只按“未激活”刷新，
            // 拿到焦点后再发一次，让 Windows 立即重绘深色标题栏。
            ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
            self.theme_focus_synced = true;
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            let available = ui.available_size();
            if available.x <= 1.0 || available.y <= 1.0 {
                return;
            }
            let scale = (available.x / SVG_WIDTH).min(available.y / SVG_HEIGHT);
            let size = egui::vec2(SVG_WIDTH * scale, SVG_HEIGHT * scale);
            ui.centered_and_justified(|ui| {
                ui.add(egui::Image::new(self.uri.clone()).fit_to_exact_size(size));
            });
        });
    }
}
