use std::sync::Arc;

use eframe::egui;

const FONT_CANDIDATES: &[(&str, u32)] = &[
    (r"C:\Windows\Fonts\msyh.ttc", 0),
    (r"C:\Windows\Fonts\msyhbd.ttc", 0),
    (r"C:\Windows\Fonts\simhei.ttf", 0),
    (r"C:\Windows\Fonts\simsun.ttc", 0),
];

pub fn install(ctx: &egui::Context) {
    for (path, index) in FONT_CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let mut fonts = egui::FontDefinitions::default();
        let data = egui::FontData::from_owned(bytes);
        let data = egui::FontData {
            index: *index,
            ..data
        };
        fonts.font_data.insert("cjk".to_owned(), Arc::new(data));
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .insert(0, "cjk".to_owned());
        }
        ctx.set_fonts(fonts);
        log::info!("loaded CJK font: {path}");
        return;
    }
    log::warn!("no CJK system font found; falling back to egui system fonts");
}
