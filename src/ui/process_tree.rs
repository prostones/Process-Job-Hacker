use std::collections::{HashMap, HashSet};

use eframe::egui::{self, Align, Color32, Layout, RichText, Sense, Vec2};
use egui_extras::{Column, TableBuilder};

use crate::model::process::{PidKey, ProcessRecord, ProcessSnapshot};
use crate::model::tree::{ProcessSort, ProcessSortKey, ProcessTree, SortDirection};
use crate::ui::format::{format_bytes, format_cpu_percent};
use crate::ui::icon_cache::{ICON_SIZE, ProcessIconCache};
use crate::win::affinity::{CoreShortcut, format_mask};

const HEADER_HEIGHT: f32 = 20.0;
const ROW_HEIGHT: f32 = 18.0;
const INDENT_WIDTH: f32 = 12.0;
const TOGGLE_WIDTH: f32 = 14.0;
const PID_WIDTH: f32 = 66.0;
const PPID_WIDTH: f32 = 60.0;
const CPU_WIDTH: f32 = 62.0;
const MEM_WIDTH: f32 = 86.0;
const COMMIT_WIDTH: f32 = 86.0;
const RULE_WIDTH: f32 = 54.0;
const NAME_MIN_WIDTH: f32 = 150.0;
const TABLE_ID_SALT: &str = "process_tree_table";
const SORT_HOVER_HINT: &str = "点击按该列排序";

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
    /// 点击表头请求按该列排序，由 `app.rs` 重建进程树。
    Sort(ProcessSortKey),
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
    sort: ProcessSort,
    p0_shortcut: Option<&CoreShortcut>,
    all_p_shortcut: Option<&CoreShortcut>,
    icons: &mut ProcessIconCache,
) -> Option<TreeEvent> {
    icons.begin_frame();
    // 表格内不需要选中文本；Label 默认会抢走点击，导致文字/数字上右键失效。
    ui.style_mut().interaction.selectable_labels = false;
    let rows = tree.flatten(snapshot, expanded, filter, false);
    if rows.is_empty() {
        ui.add_space(24.0);
        ui.label("没有匹配的进程");
        return None;
    }

    let mut sort_clicked = None;
    let mut event = None;

    TableBuilder::new(ui)
        .id_salt(TABLE_ID_SALT)
        .striped(true)
        .resizable(true)
        .auto_shrink([false, false])
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(
            Column::remainder()
                .at_least(NAME_MIN_WIDTH)
                .clip(true)
                .resizable(false),
        )
        .column(Column::initial(PID_WIDTH).at_least(50.0).clip(true))
        .column(Column::initial(PPID_WIDTH).at_least(46.0).clip(true))
        .column(Column::initial(CPU_WIDTH).at_least(50.0).clip(true))
        .column(Column::initial(MEM_WIDTH).at_least(70.0).clip(true))
        .column(Column::initial(COMMIT_WIDTH).at_least(70.0).clip(true))
        .column(Column::initial(RULE_WIDTH).at_least(46.0).clip(true))
        .header(HEADER_HEIGHT, |mut header| {
            header.col(|ui| {
                if sortable_header(ui, "名称", ProcessSortKey::Name, sort, false).clicked() {
                    sort_clicked = Some(ProcessSortKey::Name);
                }
            });
            header.col(|ui| {
                if sortable_header(ui, "PID", ProcessSortKey::Pid, sort, true).clicked() {
                    sort_clicked = Some(ProcessSortKey::Pid);
                }
            });
            header.col(|ui| {
                if sortable_header(ui, "PPID", ProcessSortKey::ParentPid, sort, true).clicked() {
                    sort_clicked = Some(ProcessSortKey::ParentPid);
                }
            });
            header.col(|ui| {
                if sortable_header(ui, "CPU%", ProcessSortKey::Cpu, sort, true).clicked() {
                    sort_clicked = Some(ProcessSortKey::Cpu);
                }
            });
            header.col(|ui| {
                if sortable_header(ui, "工作集", ProcessSortKey::Memory, sort, true).clicked() {
                    sort_clicked = Some(ProcessSortKey::Memory);
                }
            });
            header.col(|ui| {
                if sortable_header(ui, "提交", ProcessSortKey::Commit, sort, true).clicked() {
                    sort_clicked = Some(ProcessSortKey::Commit);
                }
            });
            // 规则列只是状态标识，不支持排序。
            header.col(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new("规则").strong());
                });
            });
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, rows.len(), |mut row| {
                let visible = rows[row.index()];
                let node = &tree.nodes[visible.node_index];
                let record = &snapshot.records[node.record_index];
                row.set_selected(selected == Some(node.key));

                row.col(|ui| {
                    cell_row_response(
                        ui,
                        node.key,
                        record,
                        &mut event,
                        p0_shortcut,
                        all_p_shortcut,
                    );
                    ui.push_id(node.key, |ui| {
                        ui.horizontal(|ui| {
                            // 缩进、折叠箭头、名称之间保持紧凑，避免层级深时挤占名称宽度。
                            ui.spacing_mut().item_spacing.x = 2.0;
                            ui.add_space(visible.depth as f32 * INDENT_WIDTH);
                            if visible.has_children {
                                let arrow = if visible.expanded { "▾" } else { "▸" };
                                if ui
                                    .add(egui::Button::new(arrow).frame(false).small())
                                    .clicked()
                                {
                                    event = Some(TreeEvent::Toggle(node.key));
                                }
                            } else {
                                ui.add_space(TOGGLE_WIDTH);
                            }
                            if let Some(texture) =
                                icons.texture_for(ui.ctx(), record.path.as_deref())
                            {
                                ui.add(
                                    egui::Image::new(texture)
                                        .fit_to_exact_size(Vec2::splat(ICON_SIZE)),
                                );
                            } else {
                                // 无图标时保留占位，保证同层名称左对齐。
                                ui.add_space(ICON_SIZE);
                            }
                            let color = rules
                                .get(&record.key.pid)
                                .map_or(DEFAULT_NAME_COLOR, |kind| kind.color());
                            ui.add(
                                egui::Label::new(RichText::new(record.display_name()).color(color))
                                    .truncate()
                                    .selectable(false),
                            );
                        });
                    });
                });
                row.col(|ui| {
                    cell_row_response(
                        ui,
                        node.key,
                        record,
                        &mut event,
                        p0_shortcut,
                        all_p_shortcut,
                    );
                    numeric_cell(ui, record.key.pid.to_string(), Color32::from_gray(170));
                });
                row.col(|ui| {
                    cell_row_response(
                        ui,
                        node.key,
                        record,
                        &mut event,
                        p0_shortcut,
                        all_p_shortcut,
                    );
                    numeric_cell(ui, record.parent_pid.to_string(), Color32::from_gray(150));
                });
                row.col(|ui| {
                    cell_row_response(
                        ui,
                        node.key,
                        record,
                        &mut event,
                        p0_shortcut,
                        all_p_shortcut,
                    );
                    let color = if record.cpu_percent > 80.0 {
                        Color32::from_rgb(255, 193, 7)
                    } else {
                        Color32::from_gray(200)
                    };
                    numeric_cell(ui, format_cpu_percent(record.cpu_percent), color);
                });
                row.col(|ui| {
                    cell_row_response(
                        ui,
                        node.key,
                        record,
                        &mut event,
                        p0_shortcut,
                        all_p_shortcut,
                    );
                    numeric_cell(ui, format_bytes(record.memory), Color32::from_gray(190));
                });
                row.col(|ui| {
                    cell_row_response(
                        ui,
                        node.key,
                        record,
                        &mut event,
                        p0_shortcut,
                        all_p_shortcut,
                    );
                    numeric_cell(ui, format_bytes(record.commit), Color32::from_gray(170));
                });
                row.col(|ui| {
                    cell_row_response(
                        ui,
                        node.key,
                        record,
                        &mut event,
                        p0_shortcut,
                        all_p_shortcut,
                    );
                    let rule_kind = rules.get(&record.key.pid).copied();
                    let rule_text = if rule_kind.is_some() {
                        "● 生效"
                    } else {
                        ""
                    };
                    numeric_cell(
                        ui,
                        rule_text.to_owned(),
                        rule_kind.map_or(Color32::from_gray(170), RuleKind::color),
                    );
                });
            });
        });

    if let Some(key) = sort_clicked {
        return Some(TreeEvent::Sort(key));
    }
    event
}

/// 为单元格注册整行点击与右键响应。
///
/// 必须在单元格内容之前调用：`ui.interact` 先注册，折叠按钮等内部控件后注册，
/// egui 的命中测试会优先选中内部控件，避免整行响应吞掉折叠点击。
fn cell_row_response(
    ui: &mut egui::Ui,
    key: PidKey,
    record: &ProcessRecord,
    event: &mut Option<TreeEvent>,
    p0_shortcut: Option<&CoreShortcut>,
    all_p_shortcut: Option<&CoreShortcut>,
) -> egui::Response {
    let response = ui.interact(
        ui.max_rect(),
        ui.scope_id().with("row_click"),
        Sense::click(),
    );
    if response.clicked() {
        *event = Some(TreeEvent::Select(key));
    }
    if response.secondary_clicked() {
        *event = Some(TreeEvent::Select(key));
    }
    response.context_menu(|ui| {
        if let Some(menu_event) =
            draw_row_context_menu(ui, key, record, p0_shortcut, all_p_shortcut)
        {
            *event = Some(menu_event);
        }
    });
    response
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

fn sortable_header(
    ui: &mut egui::Ui,
    text: &str,
    key: ProcessSortKey,
    sort: ProcessSort,
    right_aligned: bool,
) -> egui::Response {
    let arrow = if sort.key == key {
        match sort.direction {
            SortDirection::Ascending => " ▲",
            SortDirection::Descending => " ▼",
        }
    } else {
        ""
    };
    let label = RichText::new(format!("{text}{arrow}")).strong();
    let button = egui::Button::new(label).frame(false).small();
    let response = if right_aligned {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.add(button))
            .inner
    } else {
        ui.add(button)
    };
    response.on_hover_text(SORT_HOVER_HINT)
}

fn numeric_cell(ui: &mut egui::Ui, text: String, color: Color32) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.add(
            egui::Label::new(RichText::new(text).monospace().color(color))
                .truncate()
                .selectable(false),
        );
    });
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
