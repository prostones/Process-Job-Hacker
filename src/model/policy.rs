use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::process::{PidKey, ProcessSnapshot};
use crate::model::tree::ProcessTree;
use crate::win::job::JobHandle;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyMatch {
    pub exe_path: Option<String>,
    pub process_name: String,
    pub cmdline_contains: Option<String>,
}

impl PolicyMatch {
    pub fn from_record(record: &crate::model::process::ProcessRecord) -> Self {
        Self {
            exe_path: record.path.as_deref().map(normalize_path),
            process_name: record.name.clone(),
            cmdline_contains: None,
        }
    }

    pub fn matches(&self, record: &crate::model::process::ProcessRecord) -> bool {
        if let Some(expected) = &self.exe_path {
            if let Some(actual) = &record.path
                && normalize_path(actual) == *expected
            {
                return self.match_command_line(record);
            }
            if record.path.is_some() {
                return false;
            }
        }
        if !self.process_name.is_empty() && !record.name.eq_ignore_ascii_case(&self.process_name) {
            return false;
        }
        self.match_command_line(record)
    }

    fn match_command_line(&self, _record: &crate::model::process::ProcessRecord) -> bool {
        // Command-line filtering is a later enhancement; null means match all.
        self.cmdline_contains.is_none()
    }
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\").to_lowercase()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyLimits {
    pub memory_bytes: Option<u64>,
    pub affinity_mask: Option<u64>,
    pub cpu_rate_percent: Option<u8>,
    pub subset_affinity: bool,
}

impl PolicyLimits {
    pub fn is_empty(&self) -> bool {
        self.memory_bytes.is_none()
            && self.affinity_mask.is_none()
            && self.cpu_rate_percent.is_none()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Policy {
    pub id: Uuid,
    pub enabled: bool,
    pub auto_reapply: bool,
    #[serde(default)]
    pub temporary: bool,
    pub match_rule: PolicyMatch,
    pub limits: PolicyLimits,
    pub job_name: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default)]
pub struct ApplyReport {
    pub job_name: String,
    pub roots: usize,
    /// 至少应用了一种限制的进程（Job 成员或已设置亲和性）。
    pub applied: Vec<u32>,
    /// 成功加入 Job 的进程。
    pub assigned: Vec<u32>,
    /// 已设置亲和性、但无法加入 Job 的进程。
    pub affinity_only: Vec<u32>,
    /// 因目标已在其他 Job 中、且本 Job 非空而无法加入的进程（Windows 嵌套限制）。
    pub blocked: Vec<u32>,
    pub failed: Vec<(u32, String, String)>,
    pub job_used: bool,
    pub job_limits_requested: bool,
}

impl ApplyReport {
    /// 是否存在需要用户注意的问题：真实失败，或 Job 级限额因外部 Job 未能生效。
    pub fn has_problem(&self) -> bool {
        !self.failed.is_empty() || (self.job_limits_requested && !self.blocked.is_empty())
    }
}

impl Policy {
    pub fn new(match_rule: PolicyMatch, limits: PolicyLimits) -> Self {
        let id = Uuid::new_v4();
        let now = chrono::Utc::now().timestamp();
        Self {
            id,
            enabled: true,
            auto_reapply: true,
            temporary: false,
            match_rule,
            limits,
            job_name: format!("Local\\PJH-Policy-{id}"),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn new_temporary(match_rule: PolicyMatch, limits: PolicyLimits) -> Self {
        let mut policy = Self::new(match_rule, limits);
        policy.temporary = true;
        policy.auto_reapply = false;
        policy.job_name = format!("Local\\PJH-Temp-{}", policy.id);
        policy
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyState {
    Waiting,
    Active,
    Partial,
    Paused,
    Error(String),
}

impl PolicyState {
    pub fn label(&self) -> &str {
        match self {
            Self::Waiting => "等待目标",
            Self::Active => "生效中",
            Self::Partial => "部分生效",
            Self::Paused => "已暂停",
            Self::Error(_) => "错误",
        }
    }
}

pub struct PolicyRuntime {
    pub policy: Policy,
    pub state: PolicyState,
    pub job: Option<JobHandle>,
    pub members: Vec<u32>,
    /// 已应用亲和性但无法加入 Job 的进程（目标自带 Job 时）。
    pub affinity_only: Vec<u32>,
    /// 因目标已在其他 Job 中、本 Job 非空而无法加入的进程。
    pub blocked: Vec<u32>,
    pub active_processes: u32,
    pub current_commit: u64,
    pub peak_commit: u64,
    pub last_error: Option<String>,
    /// true 表示 last_error 只是“仅亲和性/目标自带 Job”的提示，不是故障。
    pub last_error_is_warning: bool,
    pub last_applied: Option<Instant>,
    pub last_attempt: Option<Instant>,
    pub original_affinity: HashMap<PidKey, u64>,
}

impl PolicyRuntime {
    pub fn new(policy: Policy) -> Self {
        let state = if policy.enabled {
            PolicyState::Waiting
        } else {
            PolicyState::Paused
        };
        Self {
            policy,
            state,
            job: None,
            members: Vec::new(),
            affinity_only: Vec::new(),
            blocked: Vec::new(),
            active_processes: 0,
            current_commit: 0,
            peak_commit: 0,
            last_error: None,
            last_error_is_warning: false,
            last_applied: None,
            last_attempt: None,
            original_affinity: HashMap::new(),
        }
    }

    pub fn set_error(&mut self, message: impl Into<String>) {
        let message = message.into();
        log::warn!("policy {} error: {message}", self.policy.id);
        self.last_error = Some(message.clone());
        self.last_error_is_warning = false;
        self.state = PolicyState::Error(message);
    }

    pub fn matched_roots(&self, snapshot: &ProcessSnapshot, tree: &ProcessTree) -> Vec<PidKey> {
        find_topmost_matches(snapshot, tree, &self.policy.match_rule)
    }
}

pub fn find_topmost_matches(
    snapshot: &ProcessSnapshot,
    tree: &ProcessTree,
    match_rule: &PolicyMatch,
) -> Vec<PidKey> {
    let mut matching = HashSet::new();
    for node in &tree.nodes {
        let record = &snapshot.records[node.record_index];
        if match_rule.matches(record) {
            matching.insert(node_index_by_key(tree, node.key));
        }
    }
    let mut roots = Vec::new();
    for index in matching.iter().copied() {
        let mut current = tree.nodes[index].parent;
        let mut covered = false;
        while let Some(parent) = current {
            if matching.contains(&parent) {
                covered = true;
                break;
            }
            current = tree.nodes[parent].parent;
        }
        if !covered {
            roots.push(tree.nodes[index].key);
        }
    }
    roots.sort_by_key(|key| key.pid);
    roots
}

fn node_index_by_key(tree: &ProcessTree, key: PidKey) -> usize {
    tree.index_by_key[&key]
}

pub fn is_critical_process(name: &str, pid: u32) -> bool {
    if pid <= 4 {
        return true;
    }
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "system"
            | "smss.exe"
            | "csrss.exe"
            | "wininit.exe"
            | "winlogon.exe"
            | "services.exe"
            | "lsass.exe"
            | "fontdrvhost.exe"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_path_is_case_insensitive() {
        let rule = PolicyMatch {
            exe_path: Some("c:\\windows\\notepad.exe".to_owned()),
            process_name: "notepad.exe".to_owned(),
            cmdline_contains: None,
        };
        let record = crate::model::process::ProcessRecord {
            key: PidKey::new(1, 1),
            parent_pid: 0,
            name: "notepad.exe".to_owned(),
            path: Some(std::path::PathBuf::from(r"C:\Windows\Notepad.exe")),
            ..Default::default()
        };
        assert!(rule.matches(&record));
    }

    #[test]
    fn versioned_policy_only_matches_its_own_path() {
        let old_policy = Policy::new(
            PolicyMatch {
                exe_path: Some(
                    r"c:\program files\windowsapps\microsoft.bingwallpaper_1.1.471.0_x86__8wekyb3d8bbwe\bingwallpaper.exe"
                        .to_owned(),
                ),
                process_name: "BingWallpaper.exe".to_owned(),
                cmdline_contains: None,
            },
            PolicyLimits {
                affinity_mask: Some(0x3),
                ..Default::default()
            },
        );
        let current_policy = Policy::new(
            PolicyMatch {
                exe_path: Some(
                    r"c:\program files\windowsapps\microsoft.bingwallpaper_1.1.472.0_x86__8wekyb3d8bbwe\bingwallpaper.exe"
                        .to_owned(),
                ),
                process_name: "BingWallpaper.exe".to_owned(),
                cmdline_contains: None,
            },
            PolicyLimits {
                affinity_mask: Some(0x3),
                ..Default::default()
            },
        );
        let snapshot = ProcessSnapshot::new(vec![crate::model::process::ProcessRecord {
            key: PidKey::new(30344, 1),
            parent_pid: 0,
            name: "BingWallpaper.exe".to_owned(),
            path: Some(std::path::PathBuf::from(
                r"C:\Program Files\WindowsApps\Microsoft.BingWallpaper_1.1.472.0_x86__8wekyb3d8bbwe\BingWallpaper.exe",
            )),
            ..Default::default()
        }]);
        let tree = ProcessTree::build(&snapshot);
        let old_runtime = PolicyRuntime::new(old_policy);
        let current_runtime = PolicyRuntime::new(current_policy);
        assert!(old_runtime.matched_roots(&snapshot, &tree).is_empty());
        assert_eq!(current_runtime.matched_roots(&snapshot, &tree).len(), 1);
    }

    #[test]
    fn critical_processes_are_detected() {
        assert!(is_critical_process("csrss.exe", 500));
        assert!(is_critical_process("anything.exe", 4));
        assert!(!is_critical_process("notepad.exe", 500));
    }
}
