use std::collections::{HashMap, HashSet};

use eframe::egui::{self, Align, Color32, Layout, RichText, Sense, Vec2};

use crate::model::process::{PidKey, ProcessRecord, ProcessSnapshot};
use crate::model::tree::ProcessTree;
use crate::ui::format::{format_bytes, format_cpu_percent};
use crate::win::affinity::{CoreShortcut, format_mask};

const ROW_HEIGHT: f32 = 21.0;
const PID_WIDTH: f32 = 66.0;
const PPID_WIDTH: f32 = 60.0;
const CPU_WIDTH: f32 = 62.0;
const MEM_WIDTH: f32 = 86.0;
const COMMIT_WIDTH: f32 = 86.0;
const RULE_WIDTH: f32 = 54.0;

/// 已生效规则的展示类型，用于进程树名称与“规则”列着色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleKind {
    /// 仅绑定到 P0 物理核（例如 CPU 0-1）。
    P0,
    /// 绑定到全部 P 核（例如 CPU 0-15）。
    AllP,
    /// 其他亲和性、内存或 CPU 率策略。
    Other,
}

/// P0 规则的颜色（橙色）。
const RULE_P0_COLOR: Color32 = Color32::from_rgb(255, 152, 0);
/// 全部 P 核规则的颜色（蓝色，保持原有配色）。
const RULE_ALL_P_COLOR: Color32 = Color32::from_rgb(120, 220, 255);
/// 其他规则的颜色（绿色）。
const RULE_OTHER_COLOR: Color32 = Color32::from_rgb(120, 220, 160);
/// 未设置限制的进程名称颜色（灰白色）。
const DEFAULT_NAME_COLOR: Color32 = Color32::from_gray(220);

impl RuleKind {
    /// 规则对应的展示颜色。
    pub fn color(self) -> Color32 {
        match self {
            Self::P0 => RULE_P0_COLOR,
            Self::AllP => RULE_ALL_P_COLOR,
            Self::Other => RULE_OTHER_COLOR,
        }
    }

    /// 同一进程被多个规则覆盖时的具体程度，数值越大越优先。
    pub fn priority(self) -> u8 {
        match self {
            Self::P0 => 2,
            Self::AllP => 1,
            Self::Other => 0,
        }
    }
}

/// 按策略的亲和性掩码判定规则类型。
pub fn classify_rule(mask: Option<u64>, p0_mask: Option<u64>, all_p_mask: Option<u64>) -> RuleKind {
    match mask {
        Some(mask) if p0_mask == Some(mask) => RuleKind::P0,
        Some(mask) if all_p_mask == Some(mask) => RuleKind::AllP,
        _ => RuleKind::Other,
    }
}

/// 记录某个 PID 的规则类型；同一 PID 被多个规则覆盖时保留更具体的一种。
pub fn record_rule(rules: &mut HashMap<u32, RuleKind>, pid: u32, kind: RuleKind) {
    rules
        .entry(pid)
        .and_modify(|current| {
            if kind.priority() > current.priority() {
                *current = kind;
            }
        })
        .or_insert(kind);
}

#[derive(Clone, Debug)]
pub enum TreeEvent {
    Select(PidKey),
    Toggle(PidKey),
    Terminate(PidKey),
    OpenFileLocation(String),
    PinToCore {
        key: PidKey,
        label: String,
        mask: u64,
    },
}

#[allow(clippy::too_many_arguments)]
pub fn draw_process_tree(
    ui: &mut egui::Ui,
    snapshot: &ProcessSnapshot,
    tree: &ProcessTree,
    expanded: &HashSet<PidKey>,
    selected: Option<PidKey>,
    filter: &str,
    rules: &HashMap<u32, RuleKind>,
    p0_shortcut: Option<&CoreShortcut>,
    all_p_shortcut: Option<&CoreShortcut>,
) -> Option<TreeEvent> {
    let available = ui.available_width();
    let name_width = (available
        - PID_WIDTH
        - PPID_WIDTH
        - CPU_WIDTH
        - MEM_WIDTH
        - COMMIT_WIDTH
        - RULE_WIDTH
        - 32.0)
        .max(150.0);

    let mut event = None;
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(name_width, 20.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.label(RichText::new("名称").strong());
            },
        );
        numeric_header(ui, PID_WIDTH, "PID");
        numeric_header(ui, PPID_WIDTH, "PPID");
        numeric_header(ui, CPU_WIDTH, "CPU%");
        numeric_header(ui, MEM_WIDTH, "工作集");
        numeric_header(ui, COMMIT_WIDTH, "提交");
        numeric_header(ui, RULE_WIDTH, "规则");
    });
    ui.separator();

    let rows = tree.flatten(snapshot, expanded, filter, false);
    if rows.is_empty() {
        ui.add_space(24.0);
        ui.label("没有匹配的进程");
        return None;
    }

    egui::ScrollArea::vertical()
        .id_salt("process_tree_scroll")
        .auto_shrink([false, false])
        .show_rows(ui, ROW_HEIGHT, rows.len(), |ui, range| {
            for row_index in range {
                let row = rows[row_index];
                let node = &tree.nodes[row.node_index];
                let record = &snapshot.records[node.record_index];
                let is_selected = selected == Some(node.key);
                let row_width = ui.available_width();
                let (rect, row_response) =
                    ui.allocate_exact_size(Vec2::new(row_width, ROW_HEIGHT), Sense::click());
                if is_selected {
                    ui.painter()
                        .rect_filled(rect, 2.0, Color32::from_rgb(36, 62, 82));
                } else if row_response.hovered() {
                    ui.painter()
                        .rect_filled(rect, 2.0, Color32::from_rgb(28, 36, 46));
                }

                let mut child_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(rect.shrink2(Vec2::new(2.0, 1.0)))
                        .layout(Layout::left_to_right(Align::Center)),
                );
                child_ui.spacing_mut().item_spacing.x = 2.0;
                child_ui.add_space(row.depth as f32 * 12.0);
                if row.has_children {
                    let arrow = if row.expanded { "▾" } else { "▸" };
                    if child_ui
                        .add(egui::Button::new(arrow).frame(false).small())
                        .clicked()
                    {
                        event = Some(TreeEvent::Toggle(node.key));
                    }
                } else {
                    child_ui.add_space(14.0);
                }

                let name = record.display_name();
                let name_width_for_row = (name_width - 18.0 - row.depth as f32 * 12.0).max(80.0);
                let name_response = child_ui
                    .allocate_ui_with_layout(
                        Vec2::new(name_width_for_row, ROW_HEIGHT),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(name).color(
                                        rules
                                            .get(&record.key.pid)
                                            .map_or(DEFAULT_NAME_COLOR, |kind| kind.color()),
                                    ),
                                )
                                .truncate(),
                            )
                        },
                    )
                    .inner;
                if name_response.clicked() {
                    event = Some(TreeEvent::Select(node.key));
                }
                name_response.context_menu(|ui| {
                    if let Some(menu_event) =
                        draw_row_context_menu(ui, node.key, record, p0_shortcut, all_p_shortcut)
                    {
                        event = Some(menu_event);
                    }
                });

                numeric_cell(
                    &mut child_ui,
                    PID_WIDTH,
                    record.key.pid.to_string(),
                    Color32::from_gray(170),
                );
                numeric_cell(
                    &mut child_ui,
                    PPID_WIDTH,
                    record.parent_pid.to_string(),
                    Color32::from_gray(150),
                );
                numeric_cell(
                    &mut child_ui,
                    CPU_WIDTH,
                    format_cpu_percent(record.cpu_percent),
                    if record.cpu_percent > 80.0 {
                        Color32::from_rgb(255, 193, 7)
                    } else {
                        Color32::from_gray(200)
                    },
                );
                numeric_cell(
                    &mut child_ui,
                    MEM_WIDTH,
                    format_bytes(record.memory),
                    Color32::from_gray(190),
                );
                numeric_cell(
                    &mut child_ui,
                    COMMIT_WIDTH,
                    format_bytes(record.commit),
                    Color32::from_gray(170),
                );
                let rule_kind = rules.get(&record.key.pid).copied();
                let rule_text = if rule_kind.is_some() {
                    "● 生效"
                } else {
                    ""
                };
                numeric_cell(
                    &mut child_ui,
                    RULE_WIDTH,
                    rule_text.to_owned(),
                    rule_kind.map_or(Color32::from_gray(170), RuleKind::color),
                );

                // 右键与左键一样先同步选中态：后续右键菜单里的“一键限制”依赖当前选中行，
                // 资源管理器也是右键即选中，避免菜单项承担“设为限额目标”这种隐式选中语义。
                if row_response.clicked() || row_response.secondary_clicked() {
                    event = Some(TreeEvent::Select(node.key));
                }
                row_response.context_menu(|ui| {
                    if let Some(menu_event) =
                        draw_row_context_menu(ui, node.key, record, p0_shortcut, all_p_shortcut)
                    {
                        event = Some(menu_event);
                    }
                });
            }
        });
    event
}

fn draw_row_context_menu(
    ui: &mut egui::Ui,
    key: PidKey,
    record: &ProcessRecord,
    p0_shortcut: Option<&CoreShortcut>,
    all_p_shortcut: Option<&CoreShortcut>,
) -> Option<TreeEvent> {
    let mut event = None;
    if let Some(shortcut) = p0_shortcut
        && let Some(menu_event) = quick_affinity_item(ui, key, shortcut, "")
    {
        event = Some(menu_event);
    }
    if let Some(shortcut) = all_p_shortcut
        && let Some(menu_event) =
            quick_affinity_item(ui, key, shortcut, "仅使用 P 核，不会调度到 E 核。")
    {
        event = Some(menu_event);
    }
    if let Some(path) = &record.path
        && ui.button("打开文件位置").clicked()
    {
        event = Some(TreeEvent::OpenFileLocation(
            path.to_string_lossy().into_owned(),
        ));
        ui.close();
    }
    ui.separator();
    if ui.button("终止进程").clicked() {
        event = Some(TreeEvent::Terminate(key));
        ui.close();
    }
    event
}

fn quick_affinity_item(
    ui: &mut egui::Ui,
    key: PidKey,
    shortcut: &CoreShortcut,
    extra_hint: &str,
) -> Option<TreeEvent> {
    let label = format!(
        "一键限制到 {}（CPU {}，持久化）",
        shortcut.label,
        format_mask(shortcut.mask)
    );
    let hover = format!(
        "将当前进程及全部后代绑定到 {}（逻辑 CPU {}），并写入 config.json；子进程继承。{}",
        shortcut.label,
        shortcut
            .logical
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        extra_hint
    );
    if ui.button(label).on_hover_text(hover).clicked() {
        ui.close();
        return Some(TreeEvent::PinToCore {
            key,
            label: shortcut.label.clone(),
            mask: shortcut.mask,
        });
    }
    None
}

fn numeric_header(ui: &mut egui::Ui, width: f32, text: &str) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, 20.0),
        Layout::right_to_left(Align::Center),
        |ui| {
            ui.label(RichText::new(text).strong());
        },
    );
}

fn numeric_cell(ui: &mut egui::Ui, width: f32, text: String, color: Color32) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, ROW_HEIGHT),
        Layout::right_to_left(Align::Center),
        |ui| {
            ui.label(RichText::new(text).monospace().color(color));
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const P0_MASK: u64 = 0b11;
    const ALL_P_MASK: u64 = 0xFFFF;

    #[test]
    fn classify_rule_detects_p0_first() {
        assert_eq!(
            classify_rule(Some(P0_MASK), Some(P0_MASK), Some(ALL_P_MASK)),
            RuleKind::P0
        );
    }

    #[test]
    fn classify_rule_detects_all_p() {
        assert_eq!(
            classify_rule(Some(ALL_P_MASK), Some(P0_MASK), Some(ALL_P_MASK)),
            RuleKind::AllP
        );
    }

    #[test]
    fn classify_rule_falls_back_to_other() {
        assert_eq!(
            classify_rule(Some(0b1111), Some(P0_MASK), Some(ALL_P_MASK)),
            RuleKind::Other
        );
        assert_eq!(
            classify_rule(None, Some(P0_MASK), Some(ALL_P_MASK)),
            RuleKind::Other
        );
    }

    #[test]
    fn rule_kind_colors_are_expected() {
        assert_eq!(RuleKind::P0.color(), RULE_P0_COLOR);
        assert_eq!(RuleKind::AllP.color(), RULE_ALL_P_COLOR);
        assert_eq!(RuleKind::Other.color(), RULE_OTHER_COLOR);
    }

    #[test]
    fn rule_kind_priority_orders_specificity() {
        assert!(RuleKind::P0.priority() > RuleKind::AllP.priority());
        assert!(RuleKind::AllP.priority() > RuleKind::Other.priority());
    }

    #[test]
    fn record_rule_keeps_most_specific_kind() {
        let mut rules = HashMap::new();
        record_rule(&mut rules, 42, RuleKind::Other);
        record_rule(&mut rules, 42, RuleKind::AllP);
        record_rule(&mut rules, 42, RuleKind::P0);
        record_rule(&mut rules, 42, RuleKind::Other);
        assert_eq!(rules.get(&42), Some(&RuleKind::P0));
    }
}
