use eframe::egui::{self, RichText};

use crate::model::policy::{Policy, PolicyLimits};
use crate::ui::format::format_bytes;
use crate::win::affinity::{CoreKind, CpuTopology};

#[derive(Clone, Debug, Default)]
pub struct LimitEditorState {
    pub affinity_enabled: bool,
    pub affinity_mask: u64,
    pub memory_enabled: bool,
    pub memory_mib: u64,
    pub cpu_rate_enabled: bool,
    pub cpu_rate_percent: u8,
    pub subset_affinity: bool,
}

impl LimitEditorState {
    pub fn from_policy(policy: &Policy, topology: &CpuTopology) -> Self {
        let limits = &policy.limits;
        Self {
            affinity_enabled: limits.affinity_mask.is_some(),
            affinity_mask: limits.affinity_mask.unwrap_or_else(|| topology.all_mask()),
            memory_enabled: limits.memory_bytes.is_some(),
            memory_mib: limits
                .memory_bytes
                .map(|bytes| bytes / (1024 * 1024))
                .unwrap_or(1024),
            cpu_rate_enabled: limits.cpu_rate_percent.is_some(),
            cpu_rate_percent: limits.cpu_rate_percent.unwrap_or(50),
            subset_affinity: limits.subset_affinity,
        }
    }

    pub fn to_limits(&self, topology: &CpuTopology) -> PolicyLimits {
        PolicyLimits {
            memory_bytes: self
                .memory_enabled
                .then(|| self.memory_mib.max(16) * 1024 * 1024),
            affinity_mask: self
                .affinity_enabled
                .then(|| self.affinity_mask & topology.all_mask())
                .filter(|mask| *mask != 0),
            cpu_rate_percent: self
                .cpu_rate_enabled
                .then(|| self.cpu_rate_percent.clamp(1, 100)),
            subset_affinity: self.subset_affinity,
        }
    }
}

#[derive(Clone, Debug)]
pub enum LimitEditorAction {
    Apply { persist: bool },
    Clear,
}

pub struct LimitEditorContext<'a> {
    pub topology: &'a CpuTopology,
    pub current_commit: u64,
    pub selected_name: &'a str,
    pub selected_pid: u32,
    pub subtree_count: usize,
    pub existing_policy: Option<&'a Policy>,
    pub can_apply: bool,
    pub disabled_reason: Option<&'a str>,
}

pub fn draw_limit_editor(
    ui: &mut egui::Ui,
    state: &mut LimitEditorState,
    context: &LimitEditorContext<'_>,
) -> Option<LimitEditorAction> {
    let mut action = None;
    ui.heading("限额设置");
    ui.label(
        RichText::new(format!(
            "目标：{} (PID {})，含后代 {} 个",
            context.selected_name, context.selected_pid, context.subtree_count
        ))
        .strong(),
    );
    ui.add_space(4.0);

    if let Some(reason) = context.disabled_reason {
        ui.colored_label(egui::Color32::from_rgb(255, 170, 80), reason);
    }
    if let Some(policy) = context.existing_policy {
        ui.colored_label(
            egui::Color32::from_rgb(120, 220, 255),
            format!("该目标已有策略：{}", policy.match_rule.process_name),
        );
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.checkbox(&mut state.affinity_enabled, "限制 CPU 亲和性");
        ui.label("（作用于当前树全部进程，后续子进程继承）");
    });
    if state.affinity_enabled {
        if !context.topology.supports_affinity() {
            ui.colored_label(
                egui::Color32::from_rgb(255, 170, 80),
                format!(
                    "系统有 {} 个处理器组（>64 逻辑核），当前只支持组 0",
                    context.topology.processor_groups
                ),
            );
        }
        ui.horizontal(|ui| {
            if ui.small_button("全选").clicked() {
                state.affinity_mask = context.topology.all_mask();
            }
            if ui.small_button("清空").clicked() {
                state.affinity_mask = 0;
            }
            if ui.small_button("反选").clicked() {
                state.affinity_mask = !state.affinity_mask & context.topology.all_mask();
            }
            ui.label(format!(
                "已选 {} 个逻辑 CPU",
                (state.affinity_mask & context.topology.all_mask()).count_ones()
            ));
        });
        let mut last_kind = None;
        for core in &context.topology.cores {
            if core.logical.iter().all(|logical| *logical >= 64) {
                continue;
            }
            if last_kind != Some(core.kind) {
                last_kind = Some(core.kind);
                let title = core.kind.title();
                let color = match core.kind {
                    CoreKind::Performance => egui::Color32::from_rgb(120, 220, 255),
                    CoreKind::Efficiency => egui::Color32::from_rgb(150, 220, 160),
                    CoreKind::Unknown => egui::Color32::from_gray(170),
                };
                ui.label(RichText::new(title).small().color(color));
            }
            ui.horizontal(|ui| {
                ui.add_sized(
                    [38.0, 20.0],
                    egui::Label::new(RichText::new(&core.label).monospace().color(
                        match core.kind {
                            CoreKind::Performance => egui::Color32::from_rgb(120, 220, 255),
                            CoreKind::Efficiency => egui::Color32::from_rgb(150, 220, 160),
                            CoreKind::Unknown => egui::Color32::from_gray(180),
                        },
                    )),
                );
                for logical in &core.logical {
                    if *logical >= 64 {
                        continue;
                    }
                    let selected = state.affinity_mask & (1u64 << logical) != 0;
                    if ui
                        .selectable_label(selected, format!("{logical}"))
                        .on_hover_text(format!("{} · CPU {logical}", core.label))
                        .clicked()
                    {
                        state.affinity_mask ^= 1u64 << logical;
                    }
                }
            });
        }
        ui.checkbox(
            &mut state.subset_affinity,
            "允许进程在所选范围内进一步收窄（SUBSET_AFFINITY）",
        );
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.checkbox(&mut state.memory_enabled, "限制 Job 总提交内存");
        if state.memory_enabled {
            ui.add(
                egui::DragValue::new(&mut state.memory_mib)
                    .range(16..=1_048_576)
                    .suffix(" MiB")
                    .speed(16),
            );
        }
    });
    if state.memory_enabled {
        ui.horizontal_wrapped(|ui| {
            for mib in [256u64, 512, 1024, 2048, 4096, 8192] {
                if ui.small_button(format!("{mib} MiB")).clicked() {
                    state.memory_mib = mib;
                }
            }
        });
        let requested = state.memory_mib.saturating_mul(1024 * 1024);
        ui.label(format!(
            "当前子树提交约 {}，上限 {}",
            format_bytes(context.current_commit),
            format_bytes(requested)
        ));
        if requested < context.current_commit {
            ui.colored_label(
                egui::Color32::from_rgb(255, 120, 120),
                "上限低于当前提交内存，目标程序的后续内存分配可能失败，甚至崩溃。",
            );
        }
    }

    ui.add_space(4.0);
    ui.collapsing("高级：CPU 率硬上限", |ui| {
        ui.checkbox(&mut state.cpu_rate_enabled, "启用 CPU 率上限");
        if state.cpu_rate_enabled {
            ui.add(
                egui::Slider::new(&mut state.cpu_rate_percent, 1..=100)
                    .suffix("%")
                    .text("占系统总 CPU"),
            );
            ui.label(
                "说明：百分比按系统总 CPU 计算，100% 约等于全部核心；RDS/DFSS 环境可能不生效。",
            );
        }
    });

    ui.add_space(8.0);
    let enabled = context.can_apply
        && (state.affinity_enabled || state.memory_enabled || state.cpu_rate_enabled);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                enabled,
                egui::Button::new(RichText::new("保存策略并应用").strong()),
            )
            .clicked()
        {
            action = Some(LimitEditorAction::Apply { persist: true });
        }
        if ui
            .add_enabled(enabled, egui::Button::new("仅本次应用"))
            .clicked()
        {
            action = Some(LimitEditorAction::Apply { persist: false });
        }
    });
    if context.existing_policy.is_some()
        && ui
            .button("清除该策略的限制")
            .on_hover_text("清除 Job 限额并恢复应用前的 CPU 亲和性")
            .clicked()
    {
        action = Some(LimitEditorAction::Clear);
    }

    action
}
