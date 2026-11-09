use std::collections::VecDeque;

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};

use crate::model::cpu::{CpuHistory, CpuLevel, level_for_usage};
use crate::win::affinity::{CoreKind, CpuTopology};

const NORMAL: Color32 = Color32::from_rgb(79, 195, 247);
const WARN: Color32 = Color32::from_rgb(255, 193, 7);
const CRITICAL: Color32 = Color32::from_rgb(244, 67, 54);
const BACKGROUND: Color32 = Color32::from_rgb(17, 22, 30);
const GRID: Color32 = Color32::from_rgb(48, 58, 72);
const P_COLOR: Color32 = Color32::from_rgb(120, 220, 255);
const E_COLOR: Color32 = Color32::from_rgb(150, 220, 160);

/// 子核图卡片的最小宽度（点）。
const CARD_MIN_WIDTH: f32 = 118.0;
/// 子核图卡片在窄窗口下允许压缩到的最小渲染宽度（点）。
const CARD_FALLBACK_WIDTH: f32 = 86.0;
/// 子核图卡片高度（点）。
const CARD_HEIGHT: f32 = 54.0;
/// 子核图卡片之间的间距（点）。
const CARD_GAP: f32 = 5.0;
/// 总 CPU 折线图高度（点）。
const TOTAL_CHART_HEIGHT: f32 = 48.0;
/// P 核与 E 核两个子图区之间的间隔（点）。
const SECTION_GAP: f32 = 5.0;
/// 标题行与总 CPU 图之间的固定间距（点）。
const HEADER_GAP: f32 = 4.0;
/// 高度估算的安全余量（点），吸收字体度量与对齐舍入误差，避免最后一行被裁剪。
const HEIGHT_SAFETY_PADDING: f32 = 8.0;

pub fn usage_color(usage: f32) -> Color32 {
    usage_color_for(CoreKind::Unknown, usage)
}

fn normal_color_for(kind: CoreKind) -> Color32 {
    match kind {
        CoreKind::Efficiency => E_COLOR,
        CoreKind::Performance | CoreKind::Unknown => NORMAL,
    }
}

fn usage_color_for(kind: CoreKind, usage: f32) -> Color32 {
    match level_for_usage(usage) {
        CpuLevel::Normal => normal_color_for(kind),
        CpuLevel::Warn => WARN,
        CpuLevel::Critical => CRITICAL,
    }
}

/// 按可用宽度计算子核图每行可放置的卡片数。
fn card_columns(available_width: f32) -> usize {
    (((available_width + CARD_GAP) / (CARD_MIN_WIDTH + CARD_GAP)).floor() as usize).max(1)
}

/// 按可用宽度计算给定数量子核图占用的行数。
fn core_grid_rows(count: usize, available_width: f32) -> usize {
    if count == 0 {
        return 0;
    }
    count.div_ceil(card_columns(available_width))
}

/// 计算单个子图区（标题行 + 卡片网格）占用的高度。
fn section_height(count: usize, content_width: f32, title_height: f32, spacing: f32) -> f32 {
    let rows = core_grid_rows(count, content_width);
    let grid_height = rows as f32 * CARD_HEIGHT + rows.saturating_sub(1) as f32 * CARD_GAP;
    title_height + spacing + grid_height
}

/// 子图区标题行的真实高度。
///
/// `ui.horizontal` 的初始高度固定为 `interact_size.y`，即使只放一个小字号标签，
/// 其 `min_rect` 也会按整行高度计入，因此不能只按字号行高估算。
fn section_title_height(ui: &egui::Ui) -> f32 {
    ui.spacing()
        .interact_size
        .y
        .max(ui.text_style_height(&egui::TextStyle::Small))
}

/// 计算 CPU 面板展开后完整显示所有子核图所需的内容高度（点）。
///
/// `content_width` 为面板边框以内的可用宽度，与 [`draw_core_section`] 中
/// `ui.available_width()` 的取值一致，保证高度估算与真实布局使用相同的列数。
/// 返回值不含面板边框，由调用方叠加边框后作为面板最大高度。
pub fn expanded_panel_height(
    ui: &egui::Ui,
    history: &CpuHistory,
    topology: &CpuTopology,
    content_width: f32,
) -> f32 {
    let spacing = ui.spacing().item_spacing.y;
    let title_height = section_title_height(ui);
    let header_height = ui.spacing().interact_size.y;
    // 标题行和总 CPU 图之后都会先追加 item_spacing，再追加 add_space。
    let mut height =
        header_height + spacing + HEADER_GAP + TOTAL_CHART_HEIGHT + spacing + HEADER_GAP;
    if topology.hybrid {
        if !topology.p_logical.is_empty() {
            height += section_height(
                topology.p_logical.len(),
                content_width,
                title_height,
                spacing,
            );
        }
        if !topology.e_logical.is_empty() {
            if !topology.p_logical.is_empty() {
                height += spacing + SECTION_GAP;
            }
            height += section_height(
                topology.e_logical.len(),
                content_width,
                title_height,
                spacing,
            );
        }
    } else {
        height += section_height(history.core_count(), content_width, title_height, spacing);
    }
    height + HEIGHT_SAFETY_PADDING
}

pub fn draw_cpu_panel(
    ui: &mut egui::Ui,
    history: &CpuHistory,
    topology: &CpuTopology,
    open: &mut bool,
) {
    ui.horizontal(|ui| {
        let arrow = if *open { "▾" } else { "▸" };
        if ui
            .button(format!("{arrow} CPU"))
            .on_hover_text("展开/收起 CPU 核心图")
            .clicked()
        {
            *open = !*open;
        }
        ui.label(egui::RichText::new(&history.brand).strong());
        ui.separator();
        ui.label("60 秒内的利用率 %");
        if topology.hybrid {
            ui.separator();
            ui.colored_label(P_COLOR, format!("P 核 {} 线程", topology.p_thread_count()));
            ui.colored_label(E_COLOR, format!("E 核 {} 线程", topology.e_thread_count()));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!("{:.1}%", history.latest_total()))
                    .monospace()
                    .color(usage_color(history.latest_total())),
            );
            ui.label("总计");
        });
    });

    if !*open {
        return;
    }

    ui.add_space(HEADER_GAP);
    draw_total_chart(ui, history);

    ui.add_space(HEADER_GAP);
    egui::ScrollArea::vertical()
        .id_salt("cpu_core_charts")
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(ui, |ui| {
            if topology.hybrid {
                if !topology.p_logical.is_empty() {
                    draw_core_section(
                        ui,
                        history,
                        topology,
                        "P 核 · 性能核",
                        CoreKind::Performance,
                        &topology.p_logical,
                    );
                }
                if !topology.e_logical.is_empty() {
                    ui.add_space(SECTION_GAP);
                    draw_core_section(
                        ui,
                        history,
                        topology,
                        "E 核 · 能效核",
                        CoreKind::Efficiency,
                        &topology.e_logical,
                    );
                }
            } else {
                let indices = (0..history.core_count() as u32).collect::<Vec<_>>();
                draw_core_section(
                    ui,
                    history,
                    topology,
                    "CPU 核心",
                    CoreKind::Unknown,
                    &indices,
                );
            }
        });
}

fn draw_core_section(
    ui: &mut egui::Ui,
    history: &CpuHistory,
    topology: &CpuTopology,
    title: &str,
    kind: CoreKind,
    indices: &[u32],
) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("{title} · {} 个线程", indices.len()))
                .small()
                .color(match kind {
                    CoreKind::Performance => P_COLOR,
                    CoreKind::Efficiency => E_COLOR,
                    CoreKind::Unknown => Color32::from_gray(170),
                }),
        );
    });
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(CARD_GAP, CARD_GAP);
        let available = ui.available_width();
        let columns = card_columns(available);
        let card_width = ((available - CARD_GAP * (columns.saturating_sub(1)) as f32)
            / columns as f32)
            .max(CARD_FALLBACK_WIDTH);
        for index in indices {
            let Some(core_history) = history.cores.get(*index as usize) else {
                continue;
            };
            draw_core_card(
                ui,
                *index,
                core_history,
                card_width,
                CARD_HEIGHT,
                history.capacity,
                kind,
                topology,
            );
        }
    });
}

fn draw_total_chart(ui: &mut egui::Ui, history: &CpuHistory) {
    let width = ui.available_width().clamp(220.0, 420.0);
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, TOTAL_CHART_HEIGHT), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        3.0,
        BACKGROUND,
        Stroke::new(
            1.0,
            usage_color(history.latest_total()).gamma_multiply(0.55),
        ),
        egui::StrokeKind::Inside,
    );
    draw_history(
        &painter,
        rect.shrink2(Vec2::new(4.0, 3.0)),
        &history.total,
        history.capacity,
        CoreKind::Unknown,
    );
    response.on_hover_ui(|ui| {
        ui.label(format!("总 CPU：{:.1}%", history.latest_total()));
        ui.label("窗口：60 秒");
    });
}

#[allow(clippy::too_many_arguments)]
fn draw_core_card(
    ui: &mut egui::Ui,
    index: u32,
    history: &VecDeque<f32>,
    width: f32,
    height: f32,
    capacity: usize,
    kind: CoreKind,
    topology: &CpuTopology,
) {
    let usage = history.back().copied().unwrap_or(0.0);
    let color = usage_color_for(kind, usage);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        3.0,
        BACKGROUND,
        Stroke::new(1.0, color.gamma_multiply(0.55)),
        egui::StrokeKind::Inside,
    );
    let chart = Rect::from_min_max(
        Pos2::new(rect.left() + 3.0, rect.top() + 14.0),
        Pos2::new(rect.right() - 3.0, rect.bottom() - 3.0),
    );
    draw_history(&painter, chart, history, capacity, kind);
    let prefix = match kind {
        CoreKind::Performance => "P",
        CoreKind::Efficiency => "E",
        CoreKind::Unknown => "CPU",
    };
    let label = if topology.hybrid {
        format!("{prefix} · CPU {index}")
    } else {
        format!("CPU {index}")
    };
    painter.text(
        Pos2::new(rect.left() + 5.0, rect.top() + 3.0),
        Align2::LEFT_TOP,
        label,
        FontId::proportional(10.0),
        match kind {
            CoreKind::Performance => P_COLOR,
            CoreKind::Efficiency => E_COLOR,
            CoreKind::Unknown => Color32::from_gray(185),
        },
    );
    painter.text(
        Pos2::new(rect.right() - 5.0, rect.top() + 2.0),
        Align2::RIGHT_TOP,
        format!("{usage:.0}%"),
        FontId::monospace(10.0),
        color,
    );
    response.on_hover_ui(|ui| {
        let max = history.iter().copied().fold(0.0f32, f32::max);
        let avg = if history.is_empty() {
            0.0
        } else {
            history.iter().sum::<f32>() / history.len() as f32
        };
        let core = topology.core_for_logical(index);
        if let Some(core) = core {
            ui.label(format!("{} · CPU {}", core.label, index));
            ui.label(format!(
                "类型：{} · EfficiencyClass {}",
                kind.title(),
                core.efficiency_class
            ));
        } else {
            ui.label(format!("CPU {index}"));
            ui.label(format!("类型：{}", kind.title()));
        }
        ui.label(format!("当前：{usage:.1}%"));
        ui.label(format!("60 秒最大：{max:.1}%"));
        ui.label(format!("60 秒平均：{avg:.1}%"));
    });
}

fn draw_history(
    painter: &egui::Painter,
    rect: Rect,
    history: &VecDeque<f32>,
    capacity: usize,
    kind: CoreKind,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    for fraction in [0.25f32, 0.5, 0.75, 1.0] {
        let y = rect.bottom() - rect.height() * fraction;
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(1.0, GRID.gamma_multiply(0.55)),
        );
    }
    for fraction in [0.25f32, 0.5, 0.75] {
        let x = rect.left() + rect.width() * fraction;
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.0, GRID.gamma_multiply(0.3)),
        );
    }

    let count = history.len();
    if count < 2 {
        return;
    }
    let step = rect.width() / (capacity.saturating_sub(1).max(1) as f32);
    let points: Vec<(Pos2, f32)> = history
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let x = rect.right() - (count - 1 - index) as f32 * step;
            let y = rect.bottom() - (value.clamp(0.0, 100.0) / 100.0) * rect.height();
            (Pos2::new(x, y), *value)
        })
        .collect();

    let mut fill = egui::Mesh::default();
    for pair in points.windows(2) {
        let (left, left_value) = pair[0];
        let (right, _) = pair[1];
        let base = fill.vertices.len() as u32;
        let color = usage_color_for(kind, left_value).gamma_multiply(0.22);
        fill.colored_vertex(Pos2::new(left.x, rect.bottom()), color);
        fill.colored_vertex(left, color);
        fill.colored_vertex(right, color);
        fill.colored_vertex(Pos2::new(right.x, rect.bottom()), color);
        fill.add_triangle(base, base + 1, base + 2);
        fill.add_triangle(base, base + 2, base + 3);
    }
    if !fill.is_empty() {
        painter.add(egui::Shape::mesh(fill));
    }

    for pair in points.windows(2) {
        let (left, _) = pair[0];
        let (right, right_value) = pair[1];
        painter.line_segment(
            [left, right],
            Stroke::new(1.35, usage_color_for(kind, right_value)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e_core_normal_is_green() {
        assert_eq!(usage_color_for(CoreKind::Efficiency, 10.0), E_COLOR);
    }

    #[test]
    fn p_core_normal_is_blue() {
        assert_eq!(usage_color_for(CoreKind::Performance, 10.0), NORMAL);
    }

    #[test]
    fn e_core_thresholds_override_green() {
        assert_eq!(usage_color_for(CoreKind::Efficiency, 85.0), WARN);
        assert_eq!(usage_color_for(CoreKind::Efficiency, 96.0), CRITICAL);
    }

    #[test]
    fn unknown_core_stays_blue() {
        assert_eq!(usage_color_for(CoreKind::Unknown, 10.0), NORMAL);
    }

    #[test]
    fn card_columns_follow_min_width() {
        assert_eq!(card_columns(118.0), 1);
        assert_eq!(card_columns(240.0), 1);
        assert_eq!(card_columns(241.0), 2);
        assert_eq!(card_columns(1000.0), 8);
    }

    #[test]
    fn grid_rows_wrap_by_columns() {
        assert_eq!(core_grid_rows(16, 820.0), 3);
        assert_eq!(core_grid_rows(4, 820.0), 1);
        assert_eq!(core_grid_rows(16, 620.0), 4);
        assert_eq!(core_grid_rows(0, 820.0), 0);
    }

    #[test]
    fn narrower_panel_needs_more_height() {
        let topology = flat_topology(20);
        let history = CpuHistory::new(60, "cpu".to_owned(), 20);
        egui::__run_test_ui(|ui| {
            let wide = expanded_panel_height(ui, &history, &topology, 1000.0);
            let narrow = expanded_panel_height(ui, &history, &topology, 620.0);
            assert!(
                narrow > wide,
                "窄窗口需要更高：narrow={narrow}, wide={wide}"
            );
        });
    }

    #[test]
    fn section_title_uses_interact_height() {
        egui::__run_test_ui(|ui| {
            assert_eq!(
                section_title_height(ui),
                ui.spacing().interact_size.y,
                "ui.horizontal 标题行应按 interact_size.y 计入高度"
            );
        });
    }

    #[test]
    fn single_section_height_includes_full_title_row() {
        let topology = flat_topology(1);
        let history = CpuHistory::new(60, "cpu".to_owned(), 1);
        egui::__run_test_ui(|ui| {
            let spacing = ui.spacing().item_spacing.y;
            let row = ui.spacing().interact_size.y;
            let expected = row
                + spacing
                + HEADER_GAP
                + TOTAL_CHART_HEIGHT
                + spacing
                + HEADER_GAP
                + row
                + spacing
                + CARD_HEIGHT
                + HEIGHT_SAFETY_PADDING;
            let actual = expanded_panel_height(ui, &history, &topology, 1000.0);
            assert!(
                (actual - expected).abs() < f32::EPSILON,
                "标题行未按整行高度计入：actual={actual}, expected={expected}"
            );
        });
    }

    fn flat_topology(logical_count: u32) -> CpuTopology {
        CpuTopology {
            total_logical: logical_count,
            processor_groups: 1,
            group0_count: logical_count,
            system_mask: u64::MAX,
            cores: Vec::new(),
            p_logical: Vec::new(),
            e_logical: Vec::new(),
            hybrid: false,
        }
    }
}
