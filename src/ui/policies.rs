use eframe::egui::{self, Color32, RichText};
use uuid::Uuid;

use crate::model::policy::{PolicyRuntime, PolicyState};
use crate::ui::format::format_bytes;
use crate::win::affinity::format_mask;

#[derive(Clone, Debug)]
pub enum PolicyAction {
    Edit(Uuid),
    ApplyNow(Uuid),
    Pause(Uuid),
    Resume(Uuid),
    Delete(Uuid),
    SetAutoReapply(Uuid, bool),
    ClearLimits(Uuid),
    ResetAffinity(Uuid),
    Terminate(Uuid),
}

pub fn draw_policies(ui: &mut egui::Ui, runtimes: &[PolicyRuntime]) -> Option<PolicyAction> {
    let mut action = None;
    ui.heading("策略");
    ui.label("策略会持久化；程序退出时清除限制，下次启动时自动恢复。");
    ui.separator();

    if runtimes.is_empty() {
        ui.add_space(16.0);
        ui.label("还没有策略。在左侧进程树选择一个进程后，到“限额”页保存。");
        return None;
    }

    for runtime in runtimes {
        let policy = &runtime.policy;
        let state_color = match &runtime.state {
            PolicyState::Active => Color32::from_rgb(120, 220, 140),
            PolicyState::Partial => Color32::from_rgb(255, 193, 7),
            PolicyState::Waiting => Color32::from_gray(160),
            PolicyState::Paused => Color32::from_gray(130),
            PolicyState::Error(_) => Color32::from_rgb(244, 100, 100),
        };
        let header = format!(
            "{}  [{}]",
            policy.match_rule.process_name,
            runtime.state.label()
        );
        egui::CollapsingHeader::new(RichText::new(header).color(state_color))
            .id_salt(policy.id)
            .default_open(true)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(format!(
                        "匹配路径：{}",
                        policy
                            .match_rule
                            .exe_path
                            .clone()
                            .unwrap_or_else(|| "<未知>".to_owned())
                    ))
                    .monospace(),
                );
                ui.horizontal(|ui| {
                    ui.label(format!("成员：{}", runtime.active_processes));
                    ui.separator();
                    ui.label(format!(
                        "当前提交：{}",
                        format_bytes(runtime.current_commit)
                    ));
                    ui.separator();
                    ui.label(format!("峰值：{}", format_bytes(runtime.peak_commit)));
                });
                ui.horizontal(|ui| {
                    if let Some(mask) = policy.limits.affinity_mask {
                        ui.label(format!("CPU：{}", format_mask(mask)));
                    } else {
                        ui.label("CPU：不限");
                    }
                    ui.separator();
                    if let Some(memory) = policy.limits.memory_bytes {
                        ui.label(format!("内存：{}", format_bytes(memory)));
                    } else {
                        ui.label("内存：不限");
                    }
                    if let Some(rate) = policy.limits.cpu_rate_percent {
                        ui.separator();
                        ui.label(format!("CPU 率：{rate}%"));
                    }
                });
                if let Some(error) = &runtime.last_error {
                    let color = if runtime.last_error_is_warning {
                        Color32::from_rgb(255, 193, 7)
                    } else {
                        Color32::from_rgb(255, 120, 120)
                    };
                    ui.colored_label(color, error);
                }
                if !runtime.affinity_only.is_empty() {
                    ui.label(
                        RichText::new(format!(
                            "仅亲和性：{} 个（目标自带 Job，内存/CPU 率不叠加）",
                            runtime.affinity_only.len()
                        ))
                        .color(Color32::from_rgb(255, 193, 7)),
                    );
                }
                let mut auto = policy.auto_reapply;
                if ui.checkbox(&mut auto, "目标重启后自动重新应用").changed() {
                    action = Some(PolicyAction::SetAutoReapply(policy.id, auto));
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.button("编辑").clicked() {
                        action = Some(PolicyAction::Edit(policy.id));
                    }
                    if ui.button("立即应用").clicked() {
                        action = Some(PolicyAction::ApplyNow(policy.id));
                    }
                    if policy.enabled {
                        if ui.button("暂停").clicked() {
                            action = Some(PolicyAction::Pause(policy.id));
                        }
                    } else if ui.button("恢复").clicked() {
                        action = Some(PolicyAction::Resume(policy.id));
                    }
                    if ui.button("清除限制").clicked() {
                        action = Some(PolicyAction::ClearLimits(policy.id));
                    }
                    if ui
                        .button("恢复全核并暂停")
                        .on_hover_text(
                            "把该策略覆盖的所有进程强制设回全核，并暂停策略；\
                             用于旧版本残留的 P0 亲和性或外部手工固定",
                        )
                        .clicked()
                    {
                        action = Some(PolicyAction::ResetAffinity(policy.id));
                    }
                    if ui
                        .button(RichText::new("终止进程树").color(Color32::from_rgb(255, 120, 120)))
                        .clicked()
                    {
                        action = Some(PolicyAction::Terminate(policy.id));
                    }
                    if ui.button("删除策略").clicked() {
                        action = Some(PolicyAction::Delete(policy.id));
                    }
                });
                let job_text = if runtime.job.is_some() {
                    format!("Job：{}", policy.job_name)
                } else if policy.limits.memory_bytes.is_some()
                    || policy.limits.cpu_rate_percent.is_some()
                {
                    "Job：未创建（目标自带 Job，内存/CPU 率未生效）".to_owned()
                } else {
                    "Job：未创建（纯亲和性策略）".to_owned()
                };
                ui.label(
                    RichText::new(job_text)
                        .monospace()
                        .small()
                        .color(Color32::from_gray(120)),
                );
            });
        ui.add_space(2.0);
    }
    action
}
