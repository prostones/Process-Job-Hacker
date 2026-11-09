use eframe::egui::{self, Color32, RichText};

use crate::model::process::ProcessRecord;
use crate::ui::format::{format_bytes, format_cpu_percent};

pub fn draw_details(ui: &mut egui::Ui, record: Option<&ProcessRecord>, job_note: Option<&str>) {
    ui.heading("进程详情");
    let Some(record) = record else {
        ui.add_space(12.0);
        ui.label("在左侧进程树中选择一个进程。");
        return;
    };

    let mut rows: Vec<(&str, String)> = vec![
        ("名称", record.name.clone()),
        ("PID", record.key.pid.to_string()),
        ("父 PID", record.parent_pid.to_string()),
        ("CPU（单核口径）", format_cpu_percent(record.cpu_percent)),
        ("工作集", format_bytes(record.memory)),
        ("提交内存", format_bytes(record.commit)),
    ];
    if record.key.start_time > 0 {
        rows.push(("启动时间", format_start_time(record.key.start_time)));
    }
    if let Some(path) = &record.path {
        rows.push(("路径", path.to_string_lossy().into_owned()));
    }

    egui::Grid::new("process_details_grid")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            for (key, value) in &rows {
                ui.label(RichText::new(*key).color(Color32::from_gray(150)));
                ui.add(
                    egui::Label::new(RichText::new(value).monospace())
                        .wrap_mode(egui::TextWrapMode::Truncate),
                )
                .on_hover_text(value);
                ui.end_row();
            }
        });

    if let Some(note) = job_note {
        ui.add_space(6.0);
        ui.colored_label(Color32::from_rgb(120, 220, 255), note);
    }

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("复制 PID").clicked() {
            ui.ctx().copy_text(record.key.pid.to_string());
        }
        if let Some(path) = &record.path
            && ui.button("复制路径").clicked()
        {
            ui.ctx().copy_text(path.to_string_lossy().into_owned());
        }
    });
}

fn format_start_time(unix_seconds: u64) -> String {
    chrono::DateTime::from_timestamp(unix_seconds as i64, 0)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| unix_seconds.to_string())
}
