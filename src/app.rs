use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use eframe::egui::{self, Align, Color32, Layout, RichText, Vec2};
use uuid::Uuid;

use crate::apply::apply_policy_for_roots;
use crate::config::AppConfig;
use crate::lifecycle::{self, GuardHandle};
use crate::model::cpu::CpuHistory;
use crate::model::journal::ActiveJobJournal;
use crate::model::policy::{
    ApplyReport, Policy, PolicyLimits, PolicyMatch, PolicyRuntime, PolicyState,
    find_topmost_matches, is_critical_process,
};
use crate::model::process::{PidKey, ProcessRecord, ProcessSnapshot};
use crate::model::tree::ProcessTree;
use crate::monitor::{MonitorEvent, ProcessMonitor};
use crate::sankey;
use crate::tray::TrayState;
use crate::ui;
use crate::ui::cpu_chart::{draw_cpu_panel, expanded_panel_height};
use crate::ui::details::draw_details;
use crate::ui::limit_editor::{
    LimitEditorAction, LimitEditorContext, LimitEditorState, draw_limit_editor,
};
use crate::ui::policies::{PolicyAction, draw_policies};
use crate::ui::process_tree::{RuleKind, TreeEvent, classify_rule, draw_process_tree, record_rule};
use crate::win::affinity::{CoreShortcut, CpuTopology, format_mask};
use crate::win::elevation;
use crate::win::process;
use crate::win::single_instance::SingleInstanceGuard;

/// CPU 面板收起时的高度（点），也是面板的最小高度。
const CPU_PANEL_COLLAPSED_HEIGHT: f32 = 34.0;
/// CPU 顶部面板的 id，同时用于读写 egui 持久化的面板尺寸。
const CPU_PANEL_ID: &str = "cpu_panel";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppCommand {
    ShowWindow,
    ToggleWindow,
    PauseAll,
    ResumeAll,
    ClearAllLimits,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InspectorTab {
    Details,
    Limits,
    Policies,
}

#[derive(Clone, Debug)]
struct ApplyRequest {
    persist: bool,
    limits: PolicyLimits,
    match_rule: PolicyMatch,
    selected_root: Option<PidKey>,
    editing_policy: Option<Uuid>,
    target_name: String,
    target_pid: u32,
    root_count: usize,
    process_count: usize,
}

struct PendingApply {
    request: ApplyRequest,
    warning: Option<String>,
}

/// 当前打开的独立预览窗口及其对应的临时 SVG。
struct SankeyPreview {
    child: Child,
    path: PathBuf,
}

pub struct PjhApp {
    config: AppConfig,
    config_path: PathBuf,
    journal_path: PathBuf,
    journal: ActiveJobJournal,
    single_instance: SingleInstanceGuard,
    tray: Option<TrayState>,
    tray_rx: Receiver<AppCommand>,
    guard: Option<GuardHandle>,
    monitor: ProcessMonitor,
    snapshot: Arc<ProcessSnapshot>,
    tree: ProcessTree,
    expanded: HashSet<PidKey>,
    selected: Option<PidKey>,
    selected_job_note: Option<String>,
    search: String,
    cpu_history: Arc<CpuHistory>,
    topology: CpuTopology,
    p0_core: Option<CoreShortcut>,
    all_p_core: Option<CoreShortcut>,
    runtimes: Vec<PolicyRuntime>,
    editor: LimitEditorState,
    editing_policy: Option<Uuid>,
    inspector_tab: InspectorTab,
    cpu_panel_open: bool,
    hidden: bool,
    /// 窗口首次获得焦点后补发一次深色标题栏同步。
    window_theme_synced: bool,
    allow_close: bool,
    quit_requested: bool,
    startup_hide_pending: bool,
    startup_pending: bool,
    safe_mode: bool,
    is_elevated: bool,
    monitor_paused: bool,
    status: String,
    last_snapshot_at: Option<Instant>,
    last_auto_apply: Instant,
    pending_apply: Option<PendingApply>,
    show_exit_confirm: bool,
    result_report: Option<ApplyReport>,
    error_message: Option<String>,
    exit_cleanup_failures: Option<String>,
    sankey_preview: Option<SankeyPreview>,
    confirm_terminate: Option<PidKey>,
    confirm_terminate_policy: Option<Uuid>,
    last_tray_state: Option<(usize, bool, bool)>,
}

impl PjhApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        single_instance: SingleInstanceGuard,
        safe_mode: bool,
    ) -> Self {
        crate::fonts::install(&cc.egui_ctx);
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        let _ = crate::win::privilege::enable_debug_privilege();

        let config_path = crate::config::config_path();
        let journal_path = crate::config::journal_path();
        let config = AppConfig::load(&config_path);
        let old_journal = ActiveJobJournal::load(&journal_path);
        lifecycle::wait_for_previous_guard(&old_journal);
        let mut cleanup = lifecycle::cleanup_journal(&old_journal);
        cleanup.merge(lifecycle::cleanup_policy_jobs(
            &config.policies,
            &old_journal,
        ));

        let mut journal = ActiveJobJournal::default();
        if !cleanup.is_ok() {
            journal.jobs = old_journal.jobs.clone();
        }
        let _ = journal.save(&journal_path);

        let guard = match lifecycle::spawn_guard(&journal_path) {
            Ok(guard) => {
                journal.guard_pid = Some(guard.pid);
                journal.guard_start_time = guard.start_time;
                let _ = journal.save(&journal_path);
                Some(guard)
            }
            Err(error) => {
                log::warn!("guard spawn failed: {error}");
                None
            }
        };

        let topology = CpuTopology::detect();
        let p0_core = topology.core_shortcut("P0");
        let all_p_core = topology.all_p_cores_shortcut();
        let is_elevated = elevation::is_elevated();
        let runtimes = config
            .policies
            .iter()
            .cloned()
            .map(PolicyRuntime::new)
            .collect::<Vec<_>>();
        let cpu_history = Arc::new(CpuHistory::new(
            60,
            "正在检测 CPU…".to_owned(),
            topology.total_logical.min(64) as usize,
        ));

        let monitor = ProcessMonitor::spawn(cc.egui_ctx.clone());
        let (tray_tx, tray_rx) = crossbeam_channel::unbounded();
        let tray = match TrayState::init(cc.egui_ctx.clone(), tray_tx.clone()) {
            Ok(tray) => Some(tray),
            Err(error) => {
                log::warn!("tray init failed: {error}");
                None
            }
        };

        let mut status = cleanup.summary();
        if safe_mode {
            status.push_str("；安全模式：不自动应用策略");
        }
        let startup_hide_pending = config.start_minimized;
        let cpu_panel_open = config.cpu_panel_open;
        let mut app = Self {
            config,
            config_path,
            journal_path,
            journal,
            single_instance,
            tray,
            tray_rx,
            guard,
            monitor,
            snapshot: Arc::new(ProcessSnapshot::default()),
            tree: ProcessTree::default(),
            expanded: HashSet::new(),
            selected: None,
            selected_job_note: None,
            search: String::new(),
            cpu_history,
            topology,
            p0_core,
            all_p_core,
            runtimes,
            editor: LimitEditorState {
                affinity_mask: 0,
                memory_mib: 1024,
                cpu_rate_percent: 50,
                ..Default::default()
            },
            editing_policy: None,
            inspector_tab: InspectorTab::Details,
            cpu_panel_open,
            hidden: false,
            window_theme_synced: false,
            allow_close: false,
            quit_requested: false,
            startup_hide_pending,
            startup_pending: true,
            safe_mode,
            is_elevated,
            monitor_paused: false,
            status,
            last_snapshot_at: None,
            last_auto_apply: Instant::now(),
            pending_apply: None,
            show_exit_confirm: false,
            result_report: None,
            error_message: None,
            exit_cleanup_failures: None,
            sankey_preview: None,
            confirm_terminate: None,
            confirm_terminate_policy: None,
            last_tray_state: None,
        };
        app.editor.affinity_mask = app.topology.all_mask();
        app.startup_hide_pending = app.config.start_minimized;
        if !app.is_elevated {
            app.status.push_str("；普通权限（部分系统进程需要管理员）");
        }
        app
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(command) = self.tray_rx.try_recv() {
            self.handle_command(ctx, command);
        }
        if self.single_instance.take_quit_request() {
            self.quit_from_installer(ctx);
            return;
        }
        if self.single_instance.take_show_request() {
            self.show_window(ctx);
        }

        let mut new_snapshot = None;
        while let Some(event) = self.monitor.try_recv() {
            match event {
                MonitorEvent::Snapshot(snapshot) => new_snapshot = Some(snapshot),
                MonitorEvent::Cpu(history) => self.cpu_history = history,
                MonitorEvent::Error(error) => self.status = format!("采集错误：{error}"),
            }
        }
        if let Some(snapshot) = new_snapshot {
            self.on_snapshot(ctx, snapshot);
        }
    }

    fn on_snapshot(&mut self, _ctx: &egui::Context, snapshot: Arc<ProcessSnapshot>) {
        self.snapshot = snapshot;
        self.tree = ProcessTree::build(&self.snapshot);
        if self.expanded.is_empty() {
            self.expanded = self.tree.default_expanded();
        }
        self.expanded
            .retain(|key| self.tree.index_by_key.contains_key(key));
        if let Some(selected) = self.selected
            && !self.tree.index_by_key.contains_key(&selected)
        {
            self.selected = None;
            self.selected_job_note = None;
        }
        self.refresh_runtimes();
        if self.startup_pending {
            self.startup_pending = false;
            if !self.safe_mode {
                self.apply_all_enabled();
            }
        } else {
            self.auto_apply_new_matches();
        }
        self.update_selected_job_note();
        self.last_snapshot_at = Some(Instant::now());
        self.update_tray();
    }

    fn refresh_runtimes(&mut self) {
        let snapshot = self.snapshot.clone();
        for runtime in &mut self.runtimes {
            let Some(job) = &runtime.job else {
                runtime.members.retain(|pid| snapshot.get(*pid).is_some());
                runtime
                    .affinity_only
                    .retain(|pid| snapshot.get(*pid).is_some());
                runtime.blocked.retain(|pid| snapshot.get(*pid).is_some());
                runtime.active_processes =
                    (runtime.members.len() + runtime.affinity_only.len()) as u32;
                runtime.current_commit = runtime
                    .members
                    .iter()
                    .chain(runtime.affinity_only.iter())
                    .filter_map(|pid| snapshot.get(*pid))
                    .map(|record| record.commit)
                    .sum();
                if !runtime.policy.enabled {
                    runtime.state = PolicyState::Paused;
                } else if runtime.members.is_empty()
                    && runtime.affinity_only.is_empty()
                    && runtime.blocked.is_empty()
                    && matches!(runtime.state, PolicyState::Active | PolicyState::Partial)
                {
                    runtime.state = PolicyState::Waiting;
                }
                continue;
            };
            match job.query_members() {
                Ok(members) => {
                    runtime.members = members;
                    runtime
                        .affinity_only
                        .retain(|pid| snapshot.get(*pid).is_some());
                    runtime.blocked.retain(|pid| snapshot.get(*pid).is_some());
                    let current_commit = runtime
                        .members
                        .iter()
                        .chain(runtime.affinity_only.iter())
                        .filter_map(|pid| snapshot.get(*pid))
                        .map(|record| record.commit)
                        .sum();
                    runtime.current_commit = current_commit;
                    runtime.active_processes =
                        runtime.members.len() as u32 + runtime.affinity_only.len() as u32;
                    if let Ok(limits) = job.query_limits() {
                        runtime.peak_commit = limits.peak_job_memory;
                        log::debug!(
                            "job {} flags=0x{:X} cpu_rate={:?} peak={}",
                            runtime.policy.job_name,
                            limits.flags,
                            limits.cpu_rate_percent,
                            limits.peak_job_memory
                        );
                    }
                    if runtime.active_processes == 0
                        && runtime.blocked.is_empty()
                        && matches!(runtime.state, PolicyState::Active | PolicyState::Partial)
                    {
                        runtime.state = PolicyState::Waiting;
                    }
                }
                Err(error) => {
                    log::warn!("failed to query job {}: {error}", runtime.policy.job_name);
                    runtime.set_error(error.to_string());
                }
            }
        }
    }

    fn apply_all_enabled(&mut self) {
        for index in 0..self.runtimes.len() {
            if self.runtimes[index].policy.enabled {
                let roots = self.runtimes[index].matched_roots(&self.snapshot, &self.tree);
                if !roots.is_empty() {
                    let _ = self.apply_runtime_at(index, roots);
                } else {
                    self.runtimes[index].state = PolicyState::Waiting;
                }
            }
        }
    }

    fn auto_apply_new_matches(&mut self) {
        if self.safe_mode || self.monitor_paused {
            return;
        }
        if self.last_auto_apply.elapsed() < Duration::from_millis(1500) {
            return;
        }
        self.last_auto_apply = Instant::now();
        for index in 0..self.runtimes.len() {
            let runtime = &self.runtimes[index];
            if !runtime.policy.enabled || !runtime.policy.auto_reapply || runtime.policy.temporary {
                continue;
            }
            if let Some(last) = runtime.last_attempt
                && last.elapsed() < Duration::from_secs(3)
            {
                continue;
            }
            let roots = runtime.matched_roots(&self.snapshot, &self.tree);
            if roots.is_empty() {
                continue;
            }
            let covered: HashSet<u32> = runtime
                .members
                .iter()
                .chain(runtime.affinity_only.iter())
                .chain(runtime.blocked.iter())
                .copied()
                .collect();
            if roots.iter().all(|root| covered.contains(&root.pid)) {
                continue;
            }
            let _ = self.apply_runtime_at(index, roots);
        }
    }

    fn apply_runtime_at(&mut self, index: usize, roots: Vec<PidKey>) -> Option<ApplyReport> {
        if index >= self.runtimes.len() {
            return None;
        }
        let policy = self.runtimes[index].policy.clone();
        if policy.limits.is_empty() {
            self.runtimes[index].set_error("策略没有设置任何限额");
            return None;
        }
        let roots = roots
            .into_iter()
            .filter(|key| {
                self.snapshot.find_by_key(*key).is_some_and(|record| {
                    !is_critical_process(&record.name, record.key.pid)
                        && record.key.pid != std::process::id()
                })
            })
            .collect::<Vec<_>>();
        if roots.is_empty() {
            self.runtimes[index].state = PolicyState::Waiting;
            return None;
        }
        let policy_id = policy.id.to_string();
        let outcome = apply_policy_for_roots(
            &policy.job_name,
            Some(&policy_id),
            &policy.limits,
            &roots,
            &self.snapshot,
            &self.tree,
            &mut self.journal,
            &self.journal_path,
        );
        self.runtimes[index].last_attempt = Some(Instant::now());
        match outcome {
            Ok(outcome) => {
                let report = outcome.report.clone();
                let job_used = outcome.job.is_some();
                let runtime = &mut self.runtimes[index];
                runtime.job = outcome.job;
                for (key, mask) in outcome.original_affinity {
                    runtime.original_affinity.entry(key).or_insert(mask);
                }
                runtime.members = outcome.members;
                runtime.affinity_only = if job_used {
                    report.affinity_only.clone()
                } else {
                    Vec::new()
                };
                runtime.blocked = report.blocked.clone();
                runtime.active_processes = if job_used {
                    runtime.members.len() as u32 + runtime.affinity_only.len() as u32
                } else {
                    runtime.members.len() as u32
                };
                runtime.current_commit = runtime
                    .members
                    .iter()
                    .chain(runtime.affinity_only.iter())
                    .filter_map(|pid| self.snapshot.get(*pid))
                    .map(|record| record.commit)
                    .sum();
                runtime.peak_commit = outcome.peak_commit;
                runtime.last_applied = Some(Instant::now());
                if !report.failed.is_empty() {
                    runtime.state = PolicyState::Partial;
                    runtime.last_error_is_warning = false;
                    runtime.last_error = Some(if report.job_limits_requested {
                        format!(
                            "{} 个进程应用失败；{} 个仅亲和性",
                            report.failed.len(),
                            report.affinity_only.len()
                        )
                    } else {
                        format!("{} 个进程设置 CPU 亲和性失败", report.failed.len())
                    });
                } else if report.job_limits_requested && !report.blocked.is_empty() {
                    runtime.state = PolicyState::Partial;
                    runtime.last_error_is_warning = true;
                    runtime.last_error = Some(format!(
                        "{} 个进程在目标自带 Job 中，内存/CPU 率无法叠加，已应用亲和性 {} 个",
                        report.blocked.len(),
                        report.affinity_only.len()
                    ));
                } else {
                    runtime.state = PolicyState::Active;
                    runtime.last_error = None;
                    runtime.last_error_is_warning = false;
                }
                Some(report)
            }
            Err(error) => {
                self.runtimes[index].set_error(error.clone());
                self.error_message = Some(error);
                None
            }
        }
    }

    fn execute_apply(&mut self, request: ApplyRequest) {
        let mut policy_index = None;
        if request.persist
            && let Some(editing) = request.editing_policy
            && let Some(index) = self.runtime_index_by_id(editing)
        {
            policy_index = Some(index);
            let runtime = &mut self.runtimes[index];
            runtime.policy.limits = request.limits.clone();
            runtime.policy.enabled = true;
            runtime.policy.updated_at = chrono::Utc::now().timestamp();
        }
        if policy_index.is_none() && request.persist {
            if let Some(index) = self.find_runtime_by_match(&request.match_rule, false) {
                policy_index = Some(index);
                let runtime = &mut self.runtimes[index];
                runtime.policy.limits = request.limits.clone();
                runtime.policy.enabled = true;
                runtime.policy.updated_at = chrono::Utc::now().timestamp();
            } else {
                let policy = Policy::new(request.match_rule.clone(), request.limits.clone());
                self.runtimes.push(PolicyRuntime::new(policy));
                policy_index = Some(self.runtimes.len() - 1);
            }
        }
        if policy_index.is_none() {
            if let Some(index) = self.find_runtime_by_match(&request.match_rule, true) {
                policy_index = Some(index);
                self.runtimes[index].policy.limits = request.limits.clone();
                self.runtimes[index].policy.enabled = true;
            } else {
                let policy =
                    Policy::new_temporary(request.match_rule.clone(), request.limits.clone());
                self.runtimes.push(PolicyRuntime::new(policy));
                policy_index = Some(self.runtimes.len() - 1);
            }
        }
        let Some(index) = policy_index else {
            return;
        };
        let temporary = self.runtimes[index].policy.temporary;
        if request.persist
            && !temporary
            && let Err(error) = self.persist_config()
        {
            self.error_message = Some(error);
            return;
        }
        let roots = if temporary {
            request
                .selected_root
                .map(|key| vec![key])
                .unwrap_or_else(|| self.runtimes[index].matched_roots(&self.snapshot, &self.tree))
        } else {
            self.runtimes[index].matched_roots(&self.snapshot, &self.tree)
        };
        if roots.is_empty() {
            self.runtimes[index].state = PolicyState::Waiting;
            self.status = "策略已保存，等待目标进程启动".to_owned();
            return;
        }
        if let Some(report) = self.apply_runtime_at(index, roots) {
            self.status = Self::describe_report(&report);
            if report.has_problem() {
                self.result_report = Some(report);
            }
        }
        if request.persist && !temporary {
            let _ = self.persist_config();
        }
    }

    fn request_apply(&mut self, persist: bool) {
        let Some(limits) = self.build_limits() else {
            return;
        };
        if limits.is_empty() {
            self.error_message = Some("至少启用 CPU 亲和性或内存上限之一".to_owned());
            return;
        }
        if let Some(mask) = limits.affinity_mask
            && let Err(error) = self.topology.validate_mask(mask)
        {
            self.error_message = Some(error);
            return;
        }

        let (match_rule, selected_root, editing_policy, target_name, target_pid, current_commit) =
            if let Some(policy_id) = self.editing_policy {
                let Some(index) = self.runtime_index_by_id(policy_id) else {
                    return;
                };
                let runtime = &self.runtimes[index];
                let roots = runtime.matched_roots(&self.snapshot, &self.tree);
                let current = self.sum_subtree_commit(&roots);
                (
                    runtime.policy.match_rule.clone(),
                    None,
                    Some(policy_id),
                    runtime.policy.match_rule.process_name.clone(),
                    0,
                    current,
                )
            } else {
                let Some(selected) = self.selected else {
                    self.error_message = Some("请先在进程树选择一个目标".to_owned());
                    return;
                };
                let Some(record) = self.snapshot.find_by_key(selected) else {
                    self.error_message = Some("目标进程已退出".to_owned());
                    return;
                };
                if is_critical_process(&record.name, record.key.pid) {
                    self.error_message = Some(format!(
                        "拒绝限制关键系统进程：{} (PID {})",
                        record.name, record.key.pid
                    ));
                    return;
                }
                if record.key.pid == std::process::id() {
                    self.error_message = Some("不能限制本程序自身".to_owned());
                    return;
                }
                let match_rule = PolicyMatch::from_record(record);
                let roots = if persist {
                    find_topmost_matches(&self.snapshot, &self.tree, &match_rule)
                } else {
                    vec![selected]
                };
                let current = self.sum_subtree_commit(&roots);
                (
                    match_rule,
                    Some(selected),
                    None,
                    record.name.clone(),
                    record.key.pid,
                    current,
                )
            };

        let roots = if let Some(policy_id) = editing_policy {
            self.runtime_index_by_id(policy_id)
                .map(|index| self.runtimes[index].matched_roots(&self.snapshot, &self.tree))
                .unwrap_or_default()
        } else if persist {
            find_topmost_matches(&self.snapshot, &self.tree, &match_rule)
        } else {
            selected_root.into_iter().collect()
        };
        let process_count = roots
            .iter()
            .map(|root| self.tree.subtree_keys(*root).len())
            .sum();
        let request = ApplyRequest {
            persist,
            limits: limits.clone(),
            match_rule,
            selected_root,
            editing_policy,
            target_name,
            target_pid,
            root_count: roots.len(),
            process_count,
        };
        let mut warnings = Vec::new();
        if request.root_count > 1 {
            warnings.push(format!(
                "该策略将同时匹配 {} 个根进程，内存上限为这些进程的合计。",
                request.root_count
            ));
        }
        if let Some(memory) = limits.memory_bytes
            && memory < current_commit
        {
            warnings.push(format!(
                "内存上限 {} 低于当前子树提交 {}，目标可能分配失败或崩溃。",
                ui::format::format_bytes(memory),
                ui::format::format_bytes(current_commit)
            ));
        }
        let warning = if warnings.is_empty() {
            None
        } else {
            Some(warnings.join(" "))
        };
        if warning.is_some() {
            self.pending_apply = Some(PendingApply { request, warning });
        } else {
            self.execute_apply(request);
        }
    }

    fn apply_quick_affinity(&mut self, key: PidKey, label: &str, mask: u64) {
        let Some(record) = self.snapshot.find_by_key(key).cloned() else {
            self.error_message = Some("目标进程已退出".to_owned());
            return;
        };
        if is_critical_process(&record.name, record.key.pid) {
            self.error_message = Some(format!(
                "拒绝限制关键系统进程：{} (PID {})",
                record.name, record.key.pid
            ));
            return;
        }
        if record.key.pid == std::process::id() {
            self.error_message = Some("不能限制本程序自身".to_owned());
            return;
        }
        if let Err(error) = self.topology.validate_mask(mask) {
            self.error_message = Some(error);
            return;
        }

        let match_rule = PolicyMatch::from_record(&record);
        let index = if let Some(index) = self.runtime_for_record(&record) {
            if self.runtimes[index].policy.temporary {
                if let Err(error) = self.clear_runtime_limits(index, false) {
                    self.error_message = Some(error);
                    return;
                }
                let runtime = &mut self.runtimes[index];
                runtime.policy.temporary = false;
                runtime.policy.auto_reapply = true;
                runtime.policy.job_name = format!("Local\\PJH-Policy-{}", runtime.policy.id);
            }
            let runtime = &mut self.runtimes[index];
            runtime.policy.limits.affinity_mask = Some(mask);
            runtime.policy.enabled = true;
            runtime.policy.updated_at = chrono::Utc::now().timestamp();
            index
        } else {
            let policy = Policy::new(
                match_rule,
                PolicyLimits {
                    affinity_mask: Some(mask),
                    ..Default::default()
                },
            );
            self.runtimes.push(PolicyRuntime::new(policy));
            self.runtimes.len() - 1
        };

        if let Err(error) = self.persist_config() {
            self.error_message = Some(error);
            return;
        }

        self.selected = Some(key);
        self.editing_policy = Some(self.runtimes[index].policy.id);
        self.editor = LimitEditorState::from_policy(&self.runtimes[index].policy, &self.topology);
        self.inspector_tab = InspectorTab::Limits;

        let roots = self.runtimes[index].matched_roots(&self.snapshot, &self.tree);
        if roots.is_empty() {
            self.runtimes[index].state = PolicyState::Waiting;
            self.status = format!("已保存「{label}」策略，等待目标进程启动");
            return;
        }

        if let Some(report) = self.apply_runtime_at(index, roots) {
            self.status = format!(
                "已保存并应用「{}」策略：{} (PID {}) 及后代 {} 个进程限制到 CPU {}",
                label,
                record.name,
                key.pid,
                report.applied.len(),
                format_mask(mask)
            );
            if report.has_problem() {
                self.result_report = Some(report);
            }
        }
        self.update_selected_job_note();
        self.update_tray();
    }

    fn build_limits(&self) -> Option<PolicyLimits> {
        Some(self.editor.to_limits(&self.topology))
    }

    fn sum_subtree_commit(&self, roots: &[PidKey]) -> u64 {
        let mut seen = HashSet::new();
        let mut sum = 0u64;
        for root in roots {
            for key in self.tree.subtree_keys(*root) {
                if seen.insert(key)
                    && let Some(record) = self.snapshot.find_by_key(key)
                {
                    sum = sum.saturating_add(record.commit);
                }
            }
        }
        sum
    }

    fn runtime_index_by_id(&self, id: Uuid) -> Option<usize> {
        self.runtimes
            .iter()
            .position(|runtime| runtime.policy.id == id)
    }

    fn find_runtime_by_match(&self, match_rule: &PolicyMatch, temporary: bool) -> Option<usize> {
        self.runtimes.iter().position(|runtime| {
            runtime.policy.temporary == temporary && runtime.policy.match_rule == *match_rule
        })
    }

    fn runtime_for_record(&self, record: &ProcessRecord) -> Option<usize> {
        if let Some(index) = self
            .runtimes
            .iter()
            .position(|runtime| runtime.members.contains(&record.key.pid))
        {
            return Some(index);
        }
        self.runtimes
            .iter()
            .position(|runtime| runtime.policy.match_rule.matches(record))
    }

    fn sync_editor_for_selection(&mut self) {
        let Some(selected) = self.selected else {
            return;
        };
        let Some(record) = self.snapshot.find_by_key(selected).cloned() else {
            return;
        };
        if let Some(index) = self.runtime_for_record(&record) {
            self.editing_policy = Some(self.runtimes[index].policy.id);
            self.editor =
                LimitEditorState::from_policy(&self.runtimes[index].policy, &self.topology);
        } else {
            self.editing_policy = None;
            self.editor = LimitEditorState {
                affinity_mask: self.topology.all_mask(),
                memory_mib: 1024,
                cpu_rate_percent: 50,
                ..Default::default()
            };
        }
    }

    fn update_selected_job_note(&mut self) {
        let Some(selected) = self.selected else {
            self.selected_job_note = None;
            return;
        };
        for runtime in &self.runtimes {
            if runtime.members.contains(&selected.pid)
                || runtime.affinity_only.contains(&selected.pid)
            {
                let note = if runtime.job.is_some() {
                    format!(
                        "受策略 {} 限制（Job: {}）",
                        runtime.policy.match_rule.process_name, runtime.policy.job_name
                    )
                } else {
                    format!(
                        "受策略 {} 限制（CPU 亲和性）",
                        runtime.policy.match_rule.process_name
                    )
                };
                self.selected_job_note = Some(note);
                return;
            }
        }
        self.selected_job_note = process::is_process_in_any_job(selected.pid)
            .ok()
            .filter(|in_job| *in_job)
            .map(|_| "该进程已在其他 Job 中（聚合内存/CPU 率无法叠加）".to_owned());
    }

    fn clear_runtime_limits(&mut self, index: usize, disable_policy: bool) -> Result<(), String> {
        if index >= self.runtimes.len() {
            return Ok(());
        }
        let job_name = self.runtimes[index].policy.job_name.clone();
        let saved = self.runtimes[index].original_affinity.clone();
        let mut failures = Vec::new();
        let job_taken = self.runtimes[index].job.take();
        let mut limits_cleared = true;
        if let Some(job) = job_taken.as_ref() {
            if let Err(error) = job.clear_all_limits() {
                limits_cleared = false;
                failures.push(error.to_string());
            }
            if limits_cleared {
                let restore = job.restore_affinity(&saved, process::system_affinity_mask());
                failures.extend(restore);
            }
        }
        if limits_cleared && job_taken.is_none() {
            // 纯亲和性策略没有 Job，直接按保存的 PID 恢复。
            failures.extend(lifecycle::restore_saved_affinity(&saved));
        }
        if !limits_cleared {
            self.runtimes[index].job = job_taken;
            let message = failures.join("；");
            self.runtimes[index].set_error(message.clone());
            return Err(message);
        }
        drop(job_taken);
        if failures.is_empty() {
            self.journal.remove_job(&job_name);
        }
        if let Err(error) = self.journal.save(&self.journal_path) {
            failures.push(format!("保存活动 Job 日志失败：{error}"));
        }
        let runtime = &mut self.runtimes[index];
        runtime.members.clear();
        runtime.affinity_only.clear();
        runtime.blocked.clear();
        if failures.is_empty() {
            runtime.original_affinity.clear();
            runtime.last_error = None;
            runtime.last_error_is_warning = false;
        }
        runtime.active_processes = 0;
        runtime.current_commit = 0;
        runtime.state = if disable_policy {
            runtime.policy.enabled = false;
            PolicyState::Paused
        } else {
            PolicyState::Waiting
        };
        if disable_policy {
            runtime.policy.updated_at = chrono::Utc::now().timestamp();
        }
        if failures.is_empty() {
            Ok(())
        } else {
            let message = failures.join("；");
            runtime.set_error(message.clone());
            Err(message)
        }
    }

    /// 强制把策略覆盖的所有进程设回全核并暂停策略。
    ///
    /// 用于旧版本把“原始亲和性”误记成 P0 掩码、或者用户在外部手工固定过 CPU 的场景。
    fn reset_policy_affinity(&mut self, index: usize) {
        if index >= self.runtimes.len() {
            return;
        }
        let policy_name = self.runtimes[index].policy.match_rule.process_name.clone();
        let mut pids: HashSet<u32> = self.runtimes[index]
            .members
            .iter()
            .chain(self.runtimes[index].affinity_only.iter())
            .chain(self.runtimes[index].blocked.iter())
            .copied()
            .collect();
        pids.extend(
            self.runtimes[index]
                .original_affinity
                .keys()
                .map(|key| key.pid),
        );
        let roots = self.runtimes[index].matched_roots(&self.snapshot, &self.tree);
        for root in &roots {
            for key in self.tree.subtree_keys(*root) {
                pids.insert(key.pid);
            }
        }

        if let Err(error) = self.clear_runtime_limits(index, true) {
            self.error_message = Some(format!("清除限制失败，未强制恢复全核：{error}"));
            return;
        }

        let system_mask = process::system_affinity_mask();
        let mut failures = Vec::new();
        for pid in pids {
            if self.snapshot.get(pid).is_none() {
                continue;
            }
            if let Err(error) = process::set_process_affinity(pid, system_mask)
                && error.code != 87
            {
                failures.push(format!("PID {pid}: {error}"));
            }
        }

        let runtime = &mut self.runtimes[index];
        runtime.policy.enabled = false;
        runtime.policy.updated_at = chrono::Utc::now().timestamp();
        runtime.state = PolicyState::Paused;
        let _ = self.persist_config();
        if failures.is_empty() {
            self.status = format!("已将 {policy_name} 覆盖的进程恢复为全核，并暂停策略");
        } else {
            self.error_message = Some(format!(
                "策略已暂停，但部分进程恢复全核失败：{}",
                failures.join("；")
            ));
        }
    }

    fn open_sankey_preview(&mut self) {
        self.close_sankey_preview();
        // 预览只反映当前生效的亲和性：暂停/禁用的策略，以及尚未匹配到运行进程的
        // 旧版本路径策略都不应出现在图里。
        let policies = self
            .runtimes
            .iter()
            .filter(|runtime| runtime.policy.enabled && !runtime.policy.temporary)
            .filter(|runtime| !runtime.matched_roots(&self.snapshot, &self.tree).is_empty())
            .map(|runtime| runtime.policy.clone())
            .collect::<Vec<_>>();
        let path = match sankey::write_preview_svg(&policies) {
            Ok(path) => path,
            Err(error) => {
                self.error_message = Some(format!("生成亲和性预览失败：{error}"));
                return;
            }
        };
        let executable = match std::env::current_exe() {
            Ok(executable) => executable,
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                self.error_message = Some(format!("定位程序路径失败：{error}"));
                return;
            }
        };
        match Command::new(executable)
            .arg("--sankey-preview")
            .arg(&path)
            .spawn()
        {
            Ok(child) => {
                self.sankey_preview = Some(SankeyPreview { child, path });
                self.status = "已生成亲和性预览".to_owned();
            }
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                self.error_message = Some(format!("打开预览窗口失败：{error}"));
            }
        }
    }

    fn poll_sankey_preview(&mut self) {
        let status = self
            .sankey_preview
            .as_mut()
            .map(|preview| preview.child.try_wait());
        match status {
            Some(Ok(Some(_))) | Some(Err(_)) => self.close_sankey_preview(),
            _ => {}
        }
    }

    fn close_sankey_preview(&mut self) {
        let Some(mut preview) = self.sankey_preview.take() else {
            return;
        };
        let _ = preview.child.kill();
        let _ = preview.child.wait();
        let _ = std::fs::remove_file(&preview.path);
    }

    fn perform_exit_cleanup(&mut self) -> Result<(), String> {
        self.close_sankey_preview();
        let mut failures = Vec::new();
        for index in 0..self.runtimes.len() {
            if let Err(error) = self.clear_runtime_limits(index, false) {
                failures.push(error);
            }
        }
        let cleanup = lifecycle::cleanup_journal(&self.journal);
        failures.extend(cleanup.failures);
        self.journal.jobs.retain(|job| {
            !cleanup
                .cleared
                .iter()
                .any(|cleared| cleared.eq_ignore_ascii_case(&job.job_name))
        });
        if let Err(error) = self.journal.save(&self.journal_path) {
            failures.push(format!("保存活动 Job 日志失败：{error}"));
        }
        let _ = self.persist_config();
        if failures.is_empty() {
            Ok(())
        } else {
            failures.sort_unstable();
            failures.dedup();
            Err(failures.join("；"))
        }
    }

    fn persist_config(&mut self) -> Result<(), String> {
        self.config.cpu_panel_open = self.cpu_panel_open;
        self.config.policies = self
            .runtimes
            .iter()
            .filter(|runtime| !runtime.policy.temporary)
            .map(|runtime| runtime.policy.clone())
            .collect();
        self.config
            .save(&self.config_path)
            .map_err(|error| format!("保存配置失败：{error}"))
    }

    fn has_active_jobs(&self) -> bool {
        self.runtimes.iter().any(|runtime| runtime.job.is_some()) || !self.journal.jobs.is_empty()
    }

    fn request_quit(&mut self, ctx: &egui::Context) {
        if self.has_active_jobs() {
            self.quit_requested = true;
            self.show_exit_confirm = true;
            self.show_window(ctx);
        } else {
            self.close_sankey_preview();
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// 安装器/卸载器请求退出：不弹确认框，尽量清理后关闭；失败项交给 guard。
    fn quit_from_installer(&mut self, ctx: &egui::Context) {
        if let Err(error) = self.perform_exit_cleanup() {
            log::warn!("installer quit cleanup failed: {error}");
        }
        self.allow_close = true;
        self.quit_requested = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn handle_command(&mut self, ctx: &egui::Context, command: AppCommand) {
        match command {
            AppCommand::ShowWindow => self.show_window(ctx),
            AppCommand::ToggleWindow => {
                if self.hidden {
                    self.show_window(ctx);
                } else {
                    self.hide_window(ctx);
                }
            }
            AppCommand::PauseAll | AppCommand::ClearAllLimits => {
                self.pause_all();
            }
            AppCommand::ResumeAll => self.resume_all(),
            AppCommand::Quit => self.request_quit(ctx),
        }
    }

    fn pause_all(&mut self) {
        for index in 0..self.runtimes.len() {
            let _ = self.clear_runtime_limits(index, true);
        }
        let _ = self.persist_config();
        self.status = "已暂停全部策略并清除当前限制".to_owned();
        self.update_tray();
    }

    fn resume_all(&mut self) {
        for runtime in &mut self.runtimes {
            runtime.policy.enabled = true;
        }
        let _ = self.persist_config();
        self.apply_all_enabled();
        self.status = "已恢复全部策略".to_owned();
        self.update_tray();
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        self.hidden = false;
        self.monitor.set_hidden(false);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn hide_window(&mut self, ctx: &egui::Context) {
        self.hidden = true;
        self.monitor.set_hidden(true);
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        self.status = "已最小化到托盘；限额继续生效".to_owned();
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if self.allow_close {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if self.config.close_to_tray && !self.quit_requested {
            self.hide_window(ctx);
        } else {
            self.quit_requested = true;
            self.show_exit_confirm = true;
            self.show_window(ctx);
        }
    }

    fn update_tray(&mut self) {
        let active = self
            .runtimes
            .iter()
            .filter(|runtime| matches!(runtime.state, PolicyState::Active | PolicyState::Partial))
            .count();
        let all_paused = !self.runtimes.is_empty()
            && self.runtimes.iter().all(|runtime| !runtime.policy.enabled);
        let has_rules = self.has_active_jobs();
        let state = (active, all_paused, has_rules);
        if self.last_tray_state == Some(state) {
            return;
        }
        self.last_tray_state = Some(state);
        if let Some(tray) = &self.tray {
            tray.update_tooltip(active);
            tray.set_paused(all_paused);
            tray.set_has_rules(has_rules);
        }
    }

    fn describe_report(report: &ApplyReport) -> String {
        if !report.job_limits_requested {
            format!(
                "已应用 CPU 亲和性：{} 个进程，{} 个失败",
                report.applied.len(),
                report.failed.len()
            )
        } else if report.job_used {
            format!(
                "已应用：{} 个进入 Job，{} 个仅亲和性，{} 个失败",
                report.assigned.len(),
                report.affinity_only.len(),
                report.failed.len()
            )
        } else {
            format!(
                "已应用：0 个进入 Job，{} 个仅亲和性（外部 Job 限制），{} 个失败",
                report.affinity_only.len(),
                report.failed.len()
            )
        }
    }

    fn preview_limits(&self, limits: &PolicyLimits) -> String {
        let mut parts = Vec::new();
        if let Some(mask) = limits.affinity_mask {
            parts.push(format!("CPU {}", crate::win::affinity::format_mask(mask)));
        }
        if let Some(memory) = limits.memory_bytes {
            parts.push(format!("内存 {}", ui::format::format_bytes(memory)));
        }
        if let Some(rate) = limits.cpu_rate_percent {
            parts.push(format!("CPU 率 {rate}%"));
        }
        if parts.is_empty() {
            "未设置限制".to_owned()
        } else {
            parts.join("；")
        }
    }

    fn selected_record(&self) -> Option<ProcessRecord> {
        self.selected
            .and_then(|key| self.snapshot.find_by_key(key))
            .cloned()
    }

    fn selected_subtree_count(&self) -> usize {
        self.selected
            .map(|key| self.tree.subtree_keys(key).len())
            .unwrap_or(0)
    }

    fn selected_subtree_commit(&self) -> u64 {
        self.selected
            .map(|key| self.sum_subtree_commit(&[key]))
            .unwrap_or(0)
    }

    fn managed_rule_kinds(&self) -> HashMap<u32, RuleKind> {
        let p0_mask = self.p0_core.as_ref().map(|shortcut| shortcut.mask);
        let all_p_mask = self.all_p_core.as_ref().map(|shortcut| shortcut.mask);
        let managed_count = self
            .runtimes
            .iter()
            .map(|runtime| runtime.members.len() + runtime.affinity_only.len())
            .sum();
        let mut rules = HashMap::with_capacity(managed_count);
        for runtime in &self.runtimes {
            let kind = classify_rule(runtime.policy.limits.affinity_mask, p0_mask, all_p_mask);
            for pid in runtime
                .members
                .iter()
                .chain(runtime.affinity_only.iter())
                .copied()
            {
                record_rule(&mut rules, pid, kind);
            }
        }
        rules
    }

    fn draw_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Process Job Hacker");
            ui.separator();
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("搜索名称 / PID")
                    .desired_width(220.0),
            );
            if ui.button("刷新").clicked() {
                self.monitor.refresh_now();
                self.status = "已请求刷新".to_owned();
            }
            let pause_label = if self.monitor_paused {
                "继续刷新"
            } else {
                "暂停刷新"
            };
            if ui.button(pause_label).clicked() {
                self.monitor_paused = !self.monitor_paused;
                self.monitor.set_paused(self.monitor_paused);
            }
            if ui.button("全部展开").clicked() {
                self.expanded = self.tree.nodes.iter().map(|node| node.key).collect();
            }
            if ui.button("全部折叠").clicked() {
                self.expanded.clear();
            }
            if ui
                .button("亲和性预览")
                .on_hover_text("仅显示当前已匹配进程的启用策略")
                .clicked()
            {
                self.open_sankey_preview();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.is_elevated {
                    ui.colored_label(Color32::from_rgb(120, 220, 140), "管理员");
                } else if ui.button("以管理员身份重启").clicked() {
                    let _ = self.persist_config();
                    match elevation::restart_as_admin() {
                        Ok(()) => {
                            self.allow_close = true;
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        Err(error) => self.error_message = Some(error.to_string()),
                    }
                }
                ui.separator();
                ui.label(format!(
                    "规则 {}/{}",
                    self.runtimes
                        .iter()
                        .filter(|runtime| matches!(
                            runtime.state,
                            PolicyState::Active | PolicyState::Partial
                        ))
                        .count(),
                    self.runtimes.len()
                ));
            });
        });
    }

    fn draw_status(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(format!("进程 {}", self.snapshot.records.len()));
            ui.separator();
            let age = self
                .last_snapshot_at
                .map(|instant| instant.elapsed().as_secs_f32())
                .unwrap_or(0.0);
            ui.label(format!("刷新 {age:.1}s 前"));
            ui.separator();
            ui.label(format!(
                "生效策略 {}",
                self.runtimes
                    .iter()
                    .filter(|runtime| matches!(
                        runtime.state,
                        PolicyState::Active | PolicyState::Partial
                    ))
                    .count()
            ));
            ui.separator();
            ui.label(if self.guard.is_some() {
                "崩溃守护：开"
            } else {
                "崩溃守护：关"
            });
            if self.safe_mode {
                ui.separator();
                ui.colored_label(Color32::from_rgb(255, 193, 7), "安全模式");
            }
            ui.separator();
            ui.label(&self.status);
        });
    }

    fn draw_inspector(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.inspector_tab, InspectorTab::Details, "详情");
            ui.selectable_value(&mut self.inspector_tab, InspectorTab::Limits, "限额");
            ui.selectable_value(&mut self.inspector_tab, InspectorTab::Policies, "策略");
        });
        ui.separator();
        match self.inspector_tab {
            InspectorTab::Details => {
                let record = self.selected_record();
                draw_details(ui, record.as_ref(), self.selected_job_note.as_deref());
            }
            InspectorTab::Limits => self.draw_limits_tab(ui),
            InspectorTab::Policies => {
                if let Some(action) = draw_policies(ui, &self.runtimes) {
                    self.handle_policy_action(action);
                }
            }
        }
    }

    fn draw_limits_tab(&mut self, ui: &mut egui::Ui) {
        let selected = self.selected_record();
        let editing_policy = self
            .editing_policy
            .and_then(|id| self.runtime_index_by_id(id))
            .map(|index| self.runtimes[index].policy.clone());
        let target_policy = editing_policy.as_ref();
        let (target_name, target_pid, root_count, process_count, current_commit, existing_policy) =
            if let Some(policy) = target_policy {
                let roots = find_topmost_matches(&self.snapshot, &self.tree, &policy.match_rule);
                (
                    policy.match_rule.process_name.clone(),
                    0,
                    roots.len(),
                    roots
                        .iter()
                        .map(|root| self.tree.subtree_keys(*root).len())
                        .sum(),
                    self.sum_subtree_commit(&roots),
                    Some(policy.clone()),
                )
            } else if let Some(record) = &selected {
                let existing = self
                    .runtime_for_record(record)
                    .map(|index| self.runtimes[index].policy.clone());
                (
                    record.name.clone(),
                    record.key.pid,
                    usize::from(self.selected.is_some()),
                    self.selected_subtree_count(),
                    self.selected_subtree_commit(),
                    existing,
                )
            } else {
                ui.add_space(16.0);
                ui.label("请选择进程，或在“策略”页点击“编辑”。");
                return;
            };

        let disabled_reason = if target_pid != 0 && is_critical_process(&target_name, target_pid) {
            Some("关键系统进程默认禁止限制")
        } else if target_pid == std::process::id() {
            Some("不能限制本程序自身")
        } else if !self.topology.supports_affinity() && self.editor.affinity_enabled {
            Some("系统存在多个处理器组，CPU 亲和性仅支持组 0")
        } else {
            None
        };
        let context = LimitEditorContext {
            topology: &self.topology,
            current_commit,
            selected_name: &target_name,
            selected_pid: target_pid,
            subtree_count: process_count,
            existing_policy: existing_policy.as_ref(),
            can_apply: self.selected.is_some() || self.editing_policy.is_some(),
            disabled_reason,
        };
        if let Some(action) = draw_limit_editor(ui, &mut self.editor, &context) {
            match action {
                LimitEditorAction::Apply { persist } => self.request_apply(persist),
                LimitEditorAction::Clear => self.clear_selected_limit(),
            }
        }
        let _ = (root_count, selected);
    }

    fn clear_selected_limit(&mut self) {
        let index = if let Some(id) = self.editing_policy {
            self.runtime_index_by_id(id)
        } else {
            self.selected
                .and_then(|key| self.snapshot.find_by_key(key))
                .and_then(|record| self.runtime_for_record(record))
        };
        if let Some(index) = index {
            let temporary = self.runtimes[index].policy.temporary;
            match self.clear_runtime_limits(index, !temporary) {
                Ok(()) => {
                    if temporary {
                        let id = self.runtimes[index].policy.id;
                        self.runtimes.retain(|runtime| runtime.policy.id != id);
                    }
                    let _ = self.persist_config();
                    self.status = "已清除限制并恢复亲和性".to_owned();
                }
                Err(error) => self.error_message = Some(error),
            }
        }
    }

    fn handle_policy_action(&mut self, action: PolicyAction) {
        match action {
            PolicyAction::Edit(id) => {
                if let Some(index) = self.runtime_index_by_id(id) {
                    self.editing_policy = Some(id);
                    self.selected = None;
                    self.selected_job_note = None;
                    let policy = self.runtimes[index].policy.clone();
                    self.editor = LimitEditorState::from_policy(&policy, &self.topology);
                    self.inspector_tab = InspectorTab::Limits;
                }
            }
            PolicyAction::ApplyNow(id) => {
                if let Some(index) = self.runtime_index_by_id(id) {
                    let roots = self.runtimes[index].matched_roots(&self.snapshot, &self.tree);
                    if roots.is_empty() {
                        self.status = "目标进程未运行".to_owned();
                    } else if let Some(report) = self.apply_runtime_at(index, roots) {
                        self.status = Self::describe_report(&report);
                        if report.has_problem() {
                            self.result_report = Some(report);
                        }
                    }
                }
            }
            PolicyAction::Pause(id) => {
                if let Some(index) = self.runtime_index_by_id(id) {
                    match self.clear_runtime_limits(index, true) {
                        Ok(()) => {
                            let _ = self.persist_config();
                            self.status = "策略已暂停".to_owned();
                        }
                        Err(error) => self.error_message = Some(error),
                    }
                }
            }
            PolicyAction::Resume(id) => {
                if let Some(index) = self.runtime_index_by_id(id) {
                    self.runtimes[index].policy.enabled = true;
                    let roots = self.runtimes[index].matched_roots(&self.snapshot, &self.tree);
                    if roots.is_empty() {
                        self.runtimes[index].state = PolicyState::Waiting;
                    } else if let Some(report) = self.apply_runtime_at(index, roots) {
                        self.status = Self::describe_report(&report);
                        if report.has_problem() {
                            self.result_report = Some(report);
                        }
                    }
                    let _ = self.persist_config();
                }
            }
            PolicyAction::Delete(id) => {
                if let Some(index) = self.runtime_index_by_id(id) {
                    let _ = self.clear_runtime_limits(index, true);
                    self.runtimes.retain(|runtime| runtime.policy.id != id);
                    if self.editing_policy == Some(id) {
                        self.editing_policy = None;
                    }
                    let _ = self.persist_config();
                    self.status = "策略已删除".to_owned();
                }
            }
            PolicyAction::SetAutoReapply(id, auto) => {
                if let Some(index) = self.runtime_index_by_id(id) {
                    self.runtimes[index].policy.auto_reapply = auto;
                    let _ = self.persist_config();
                }
            }
            PolicyAction::ClearLimits(id) => {
                if let Some(index) = self.runtime_index_by_id(id) {
                    let _ = self.clear_runtime_limits(index, true);
                    let _ = self.persist_config();
                    self.status = "已清除限制并暂停策略".to_owned();
                }
            }
            PolicyAction::ResetAffinity(id) => {
                if let Some(index) = self.runtime_index_by_id(id) {
                    self.reset_policy_affinity(index);
                    self.update_tray();
                }
            }
            PolicyAction::Terminate(id) => {
                self.confirm_terminate_policy = Some(id);
            }
        }
    }

    fn show_dialogs(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if let Some(pending) = self.pending_apply.take() {
            let mut apply = false;
            let mut cancel = false;
            egui::Window::new("确认应用限额")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(&ctx, |ui| {
                    ui.label(format!(
                        "目标：{} (PID {})",
                        pending.request.target_name, pending.request.target_pid
                    ));
                    ui.label(format!(
                        "匹配根进程 {} 个，涉及进程 {} 个",
                        pending.request.root_count, pending.request.process_count
                    ));
                    ui.label(self.preview_limits(&pending.request.limits));
                    if let Some(warning) = &pending.warning {
                        ui.colored_label(Color32::from_rgb(255, 120, 120), warning);
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("应用").clicked() {
                            apply = true;
                        }
                        if ui.button("取消").clicked() {
                            cancel = true;
                        }
                    });
                });
            if apply {
                self.execute_apply(pending.request);
            } else if !cancel {
                self.pending_apply = Some(pending);
            }
        }

        if self.show_exit_confirm {
            let mut confirm = false;
            let mut cancel = false;
            egui::Window::new("退出程序")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(&ctx, |ui| {
                    ui.label("退出前会清除所有 CPU / 内存限制，并恢复应用前的亲和性。");
                    ui.label("策略配置会保留，下次启动时自动重新应用。目标进程不会被终止。");
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("退出并清除限制").clicked() {
                            confirm = true;
                        }
                        if ui.button("取消").clicked() {
                            cancel = true;
                        }
                    });
                });
            if confirm {
                match self.perform_exit_cleanup() {
                    Ok(()) => {
                        self.allow_close = true;
                        self.show_exit_confirm = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    Err(error) => {
                        self.show_exit_confirm = false;
                        self.exit_cleanup_failures = Some(error);
                    }
                }
            } else if cancel {
                self.show_exit_confirm = false;
                self.quit_requested = false;
            }
        }

        if let Some(failures) = self.exit_cleanup_failures.clone() {
            let mut force_quit = false;
            let mut cancel = false;
            egui::Window::new("退出清理未完全成功")
                .collapsible(false)
                .resizable(true)
                .default_width(560.0)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(&ctx, |ui| {
                    ui.label("以下项目未能恢复，目标可能已退出、被保护或当前权限不足：");
                    ui.label(
                        RichText::new(&failures)
                            .monospace()
                            .small()
                            .color(Color32::from_gray(180)),
                    );
                    ui.label("仍然退出后，后台守护进程会再尝试清理一次。");
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("仍然退出").clicked() {
                            force_quit = true;
                        }
                        if ui.button("取消").clicked() {
                            cancel = true;
                        }
                    });
                });
            if force_quit {
                self.exit_cleanup_failures = None;
                self.show_exit_confirm = false;
                self.allow_close = true;
                self.quit_requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else if cancel {
                self.exit_cleanup_failures = None;
                self.quit_requested = false;
            }
        }

        if let Some(report) = self.result_report.take() {
            let mut close = false;
            let snapshot = &self.snapshot;
            let process_name = |pid: u32| {
                snapshot
                    .get(pid)
                    .map(|record| record.name.clone())
                    .unwrap_or_else(|| "未知进程".to_owned())
            };
            egui::Window::new("应用结果")
                .collapsible(false)
                .resizable(true)
                .default_width(560.0)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(&ctx, |ui| {
                    ui.label(
                        RichText::new(format!("Job：{}", report.job_name))
                            .monospace()
                            .small()
                            .color(Color32::from_gray(140)),
                    );
                    ui.label(format!(
                        "匹配根 {} 个；已应用 {} 个；进入 Job {} 个；仅亲和性 {} 个；失败 {} 个",
                        report.roots,
                        report.applied.len(),
                        report.assigned.len(),
                        report.affinity_only.len(),
                        report.failed.len()
                    ));
                    if !report.job_used {
                        ui.label(
                            RichText::new("未创建 Job（纯亲和性策略，或所有目标都无法加入 Job）")
                                .color(Color32::from_gray(150)),
                        );
                    }
                    if !report.blocked.is_empty() {
                        let without_limits = report
                            .blocked
                            .iter()
                            .filter(|pid| !report.affinity_only.contains(pid))
                            .count();
                        let mut text = format!(
                            "{} 个进程已在目标应用自带的 Job 中；Windows 只允许空 Job 接收这类进程，\
                             Job 级内存/CPU 率无法叠加",
                            report.blocked.len()
                        );
                        if without_limits > 0 {
                            text.push_str(&format!("；其中 {without_limits} 个未应用任何限制"));
                        }
                        ui.label(RichText::new(text).color(Color32::from_rgb(255, 193, 7)));
                        egui::CollapsingHeader::new("查看受外部 Job 限制的进程")
                            .default_open(false)
                            .show(ui, |ui| {
                                for pid in &report.blocked {
                                    ui.label(format!("PID {pid} {}", process_name(*pid)));
                                }
                            });
                    }
                    if !report.failed.is_empty() {
                        ui.separator();
                        let mut grouped: std::collections::BTreeMap<&str, Vec<u32>> =
                            std::collections::BTreeMap::new();
                        for (pid, _name, error) in &report.failed {
                            grouped.entry(error.as_str()).or_default().push(*pid);
                        }
                        egui::ScrollArea::vertical()
                            .max_height(240.0)
                            .show(ui, |ui| {
                                for (error, pids) in grouped {
                                    ui.label(
                                        RichText::new(format!(
                                            "{} 个进程失败：{error}",
                                            pids.len()
                                        ))
                                        .color(Color32::from_rgb(255, 140, 140)),
                                    );
                                    ui.label(
                                        RichText::new(format!(
                                            "PID {}",
                                            pids.iter()
                                                .map(u32::to_string)
                                                .collect::<Vec<_>>()
                                                .join(", ")
                                        ))
                                        .small()
                                        .color(Color32::from_gray(140)),
                                    );
                                }
                            });
                    }
                    ui.separator();
                    if ui.button("关闭").clicked() {
                        close = true;
                    }
                });
            if !close {
                self.result_report = Some(report);
            }
        }

        if let Some(message) = self.error_message.take() {
            let mut close = false;
            egui::Window::new("错误")
                .collapsible(false)
                .resizable(true)
                .default_width(480.0)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(&ctx, |ui| {
                    ui.label(RichText::new(&message).color(Color32::from_rgb(255, 140, 140)));
                    if ui.button("关闭").clicked() {
                        close = true;
                    }
                });
            if !close {
                self.error_message = Some(message);
            }
        }

        if let Some(key) = self.confirm_terminate.take() {
            let name = self
                .snapshot
                .find_by_key(key)
                .map(|record| record.name.clone())
                .unwrap_or_else(|| key.pid.to_string());
            let mut confirm = false;
            let mut cancel = false;
            egui::Window::new("终止进程")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(&ctx, |ui| {
                    ui.label(format!("确定终止 {name} (PID {})？", key.pid));
                    ui.horizontal(|ui| {
                        if ui.button("终止").clicked() {
                            confirm = true;
                        }
                        if ui.button("取消").clicked() {
                            cancel = true;
                        }
                    });
                });
            if confirm {
                match process::terminate_process(key.pid) {
                    Ok(()) => {
                        self.status = format!("已终止 PID {}", key.pid);
                        self.monitor.refresh_now();
                    }
                    Err(error) => self.error_message = Some(error.to_string()),
                }
            } else if !cancel {
                self.confirm_terminate = Some(key);
            }
        }

        if let Some(id) = self.confirm_terminate_policy.take() {
            let mut confirm = false;
            let mut cancel = false;
            egui::Window::new("终止进程树")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(&ctx, |ui| {
                    ui.label("确定终止该策略下所有进程？此操作会立即结束目标进程。");
                    ui.horizontal(|ui| {
                        if ui.button("终止整个 Job").clicked() {
                            confirm = true;
                        }
                        if ui.button("取消").clicked() {
                            cancel = true;
                        }
                    });
                });
            if confirm {
                if let Some(index) = self.runtime_index_by_id(id)
                    && let Some(job) = &self.runtimes[index].job
                {
                    match job.terminate(1) {
                        Ok(()) => {
                            self.runtimes[index].members.clear();
                            self.runtimes[index].active_processes = 0;
                            self.status = "已终止 Job 内进程".to_owned();
                            self.monitor.refresh_now();
                        }
                        Err(error) => self.error_message = Some(error.to_string()),
                    }
                }
            } else if !cancel {
                self.confirm_terminate_policy = Some(id);
            }
        }
    }
}

/// 写入 CPU 面板的目标外高度，使折叠/展开切换立即生效。
///
/// egui 按 `Panel::id` 把面板尺寸存进 `PanelState`，但没有公开的写入方法；
/// 这里用同一个 id 直接持久化，尺寸沿用当前面板的左上角与宽度。
fn set_cpu_panel_height(ui: &egui::Ui, height: f32) {
    let id = ui.scope_id().with_salt(egui::IdSalt::new(CPU_PANEL_ID));
    let current = egui::PanelState::load(ui.ctx(), id)
        .map(|state| state.outer_rect)
        .unwrap_or_else(|| egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::ZERO));
    let outer_rect = egui::Rect::from_min_size(current.min, Vec2::new(current.width(), height));
    ui.ctx()
        .data_mut(|data| data.insert_persisted(id, egui::PanelState { outer_rect }));
}

impl eframe::App for PjhApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.window_theme_synced && ctx.input(|input| input.viewport().focused.unwrap_or(false))
        {
            // 首帧窗口尚未获得焦点，egui 发出的 SetTheme 只能按“未激活”刷新非客户区；
            // 拿到焦点后再发一次，让 Windows 立即重绘深色标题栏。
            ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
            self.window_theme_synced = true;
        }
        if self.startup_hide_pending {
            self.startup_hide_pending = false;
            self.hide_window(ctx);
        }
        self.drain_events(ctx);
        if ctx.input(|input| input.viewport().close_requested()) {
            self.handle_close_request(ctx);
        }
        self.poll_sankey_preview();
        self.update_tray();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("toolbar").show(ui, |ui| self.draw_toolbar(ui));

        let previous_open = self.cpu_panel_open;
        let panel_margin = egui::Frame::side_top_panel(ui.style()).total_margin().sum();
        let content_width = (ui.available_width() - panel_margin.x).max(160.0);
        // 面板外框还要占用内边距和底部分隔线，叠加后才是面板最大外高度。
        let fit_panel_height =
            expanded_panel_height(ui, &self.cpu_history, &self.topology, content_width)
                + panel_margin.y
                + ui.style().visuals.widgets.noninteractive.bg_stroke.width;
        let max_panel_height = if self.cpu_panel_open {
            fit_panel_height
        } else {
            CPU_PANEL_COLLAPSED_HEIGHT
        };
        egui::Panel::top(CPU_PANEL_ID)
            .resizable(true)
            .default_size(if self.cpu_panel_open {
                220.0
            } else {
                CPU_PANEL_COLLAPSED_HEIGHT
            })
            .min_size(CPU_PANEL_COLLAPSED_HEIGHT)
            .max_size(max_panel_height.max(CPU_PANEL_COLLAPSED_HEIGHT))
            .show(ui, |ui| {
                draw_cpu_panel(
                    ui,
                    &self.cpu_history,
                    &self.topology,
                    &mut self.cpu_panel_open,
                );
            });
        if previous_open != self.cpu_panel_open {
            // 折叠/展开时同步面板尺寸，避免收起后的 34pt 尺寸把再次展开卡在标题栏。
            let target_height = if self.cpu_panel_open {
                fit_panel_height
            } else {
                CPU_PANEL_COLLAPSED_HEIGHT
            };
            set_cpu_panel_height(ui, target_height);
            let _ = self.persist_config();
        }

        egui::Panel::bottom("status_bar").show(ui, |ui| self.draw_status(ui));

        egui::Panel::right("inspector")
            .resizable(true)
            .default_size(390.0)
            .min_size(330.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("inspector_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.draw_inspector(ui));
            });

        egui::CentralPanel::default().show(ui, |ui| {
            if self.snapshot.records.is_empty() {
                ui.add_space(24.0);
                ui.spinner();
                ui.label("正在采集进程…");
                return;
            }
            let rules = self.managed_rule_kinds();
            if let Some(event) = draw_process_tree(
                ui,
                &self.snapshot,
                &self.tree,
                &self.expanded,
                self.selected,
                &self.search,
                &rules,
                self.p0_core.as_ref(),
                self.all_p_core.as_ref(),
            ) {
                match event {
                    TreeEvent::Select(key) => {
                        self.selected = Some(key);
                        self.editing_policy = None;
                        self.sync_editor_for_selection();
                        self.update_selected_job_note();
                    }
                    TreeEvent::Toggle(key) => {
                        if !self.expanded.remove(&key) {
                            self.expanded.insert(key);
                        }
                    }
                    TreeEvent::PinToCore { key, label, mask } => {
                        self.selected = Some(key);
                        self.editing_policy = None;
                        self.sync_editor_for_selection();
                        self.apply_quick_affinity(key, &label, mask);
                    }
                    TreeEvent::Terminate(key) => self.confirm_terminate = Some(key),
                    TreeEvent::OpenFileLocation(path) => {
                        let argument = format!("/select,\"{}\"", path.replace('"', ""));
                        if let Err(error) = std::process::Command::new("explorer.exe")
                            .arg(argument)
                            .spawn()
                        {
                            self.error_message = Some(format!("打开文件位置失败：{error}"));
                        }
                    }
                }
            }
        });

        self.show_dialogs(ui);
    }
}
