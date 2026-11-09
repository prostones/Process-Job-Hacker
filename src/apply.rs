use std::collections::{HashMap, HashSet};
use std::path::Path;

use windows_sys::Win32::System::Threading::SetProcessAffinityMask;

use crate::model::journal::{ActiveJobJournal, ActiveJobRecord, JournalAffinity, JournalProcess};
use crate::model::policy::{ApplyReport, PolicyLimits};
use crate::model::process::{PidKey, ProcessSnapshot};
use crate::model::tree::ProcessTree;
use crate::win::error::WinError;
use crate::win::job::{JobHandle, JobLimits};
use crate::win::process::{
    is_in_job, open_for_affinity, open_for_job, process_affinity, process_start_time_handle,
};

pub struct ApplyOutcome {
    pub report: ApplyReport,
    pub job: Option<JobHandle>,
    pub original_affinity: HashMap<PidKey, u64>,
    pub members: Vec<u32>,
    pub peak_commit: u64,
}

/// 应用策略的统一入口。
///
/// 纯 CPU 亲和性不需要 Job Object：亲和性通过 `SetProcessAffinityMask` 直接作用于进程，
/// 子进程在创建时继承父进程掩码。只有聚合内存或 CPU 率限制才必须借助 Job。
#[allow(clippy::too_many_arguments)]
pub fn apply_policy_for_roots(
    job_name: &str,
    policy_id: Option<&str>,
    limits: &PolicyLimits,
    roots: &[PidKey],
    snapshot: &ProcessSnapshot,
    tree: &ProcessTree,
    journal: &mut ActiveJobJournal,
    journal_path: &Path,
) -> Result<ApplyOutcome, String> {
    if limits.is_empty() {
        return Err("至少需要设置 CPU 亲和性或内存上限".to_owned());
    }
    if roots.is_empty() {
        return Err("没有匹配到目标进程".to_owned());
    }

    if needs_job_object(limits) {
        apply_job_path(
            job_name,
            policy_id,
            limits,
            roots,
            snapshot,
            tree,
            journal,
            journal_path,
        )
    } else {
        apply_affinity_only(
            job_name,
            policy_id,
            limits,
            roots,
            snapshot,
            tree,
            journal,
            journal_path,
        )
    }
}

fn needs_job_object(limits: &PolicyLimits) -> bool {
    limits.memory_bytes.is_some() || limits.cpu_rate_percent.is_some()
}

fn collect_target_keys(roots: &[PidKey], tree: &ProcessTree) -> Vec<PidKey> {
    let mut keys = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        if tree.index_by_key.contains_key(root) && seen.insert(*root) {
            keys.push(*root);
        }
        for key in tree.subtree_keys(*root) {
            if seen.insert(key) {
                keys.push(key);
            }
        }
    }
    keys
}

fn failure_text(failures: &[(u32, String, String)]) -> String {
    failures
        .iter()
        .map(|(_, _, error)| error.clone())
        .collect::<Vec<_>>()
        .join("；")
}

fn save_journal(
    journal: &mut ActiveJobJournal,
    journal_path: &Path,
    record: ActiveJobRecord,
) -> Result<(), String> {
    journal.upsert_job(record);
    journal
        .save(journal_path)
        .map_err(|error| format!("写入活动 Job 日志失败：{error}"))
}

fn record_for(
    job_name: &str,
    policy_id: Option<&str>,
    roots: &[PidKey],
    original_affinity: &HashMap<PidKey, u64>,
) -> ActiveJobRecord {
    ActiveJobRecord {
        policy_id: policy_id.map(str::to_owned),
        job_name: job_name.to_owned(),
        roots: roots.iter().map(JournalProcess::from).collect(),
        original_affinity: original_affinity
            .iter()
            .map(|(key, mask)| JournalAffinity {
                pid: key.pid,
                start_time: key.start_time,
                mask: *mask,
            })
            .collect(),
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_affinity_only(
    job_name: &str,
    policy_id: Option<&str>,
    limits: &PolicyLimits,
    roots: &[PidKey],
    snapshot: &ProcessSnapshot,
    tree: &ProcessTree,
    journal: &mut ActiveJobJournal,
    journal_path: &Path,
) -> Result<ApplyOutcome, String> {
    let Some(mask) = limits.affinity_mask else {
        return Err("策略未设置 CPU 亲和性".to_owned());
    };
    let keys = collect_target_keys(roots, tree);
    if keys.is_empty() {
        return Err("目标进程已经退出".to_owned());
    }

    let mut failures: Vec<(u32, String, String)> = Vec::new();
    let mut handles = HashMap::new();
    let mut original_affinity = HashMap::new();
    for key in &keys {
        let Some(record) = snapshot.find_by_key(*key) else {
            failures.push((
                key.pid,
                format!("{}:{}", key.pid, key.start_time),
                "进程快照中已不存在".to_owned(),
            ));
            continue;
        };
        let handle = match open_for_affinity(key.pid) {
            Ok(handle) => handle,
            Err(error) => {
                failures.push((key.pid, record.name.clone(), error.to_string()));
                continue;
            }
        };
        let actual_start = process_start_time_handle(handle.raw()).unwrap_or_default();
        if actual_start != 0 && actual_start != key.start_time {
            failures.push((
                key.pid,
                record.name.clone(),
                "PID 已被复用，跳过".to_owned(),
            ));
            continue;
        }
        if let Ok((current, _)) = process_affinity(handle.raw()) {
            original_affinity.insert(*key, current);
        }
        handles.insert(*key, handle);
    }

    if handles.is_empty() {
        return Err(format!("无法打开任何目标进程：{}", failure_text(&failures)));
    }

    // 即使没有 Job，也要写 journal：崩溃守护在 Job 不存在时会按 journal 恢复亲和性。
    save_journal(
        journal,
        journal_path,
        record_for(job_name, policy_id, roots, &original_affinity),
    )?;

    let mut applied = Vec::new();
    for key in &keys {
        let Some(handle) = handles.get(key) else {
            continue;
        };
        let name = snapshot
            .find_by_key(*key)
            .map(|record| record.name.clone())
            .unwrap_or_else(|| key.pid.to_string());
        let ok = unsafe { SetProcessAffinityMask(handle.raw(), mask as usize) };
        if ok == 0 {
            failures.push((
                key.pid,
                name,
                WinError::last("SetProcessAffinityMask").to_string(),
            ));
        } else {
            applied.push(key.pid);
        }
    }

    if applied.is_empty() {
        journal.remove_job(job_name);
        let _ = journal.save(journal_path);
        return Err(format!(
            "无法对任何目标进程设置 CPU 亲和性：{}",
            failure_text(&failures)
        ));
    }

    Ok(ApplyOutcome {
        report: ApplyReport {
            job_name: job_name.to_owned(),
            roots: roots.len(),
            applied: applied.clone(),
            assigned: Vec::new(),
            affinity_only: Vec::new(),
            blocked: Vec::new(),
            failed: failures,
            job_used: false,
            job_limits_requested: false,
        },
        job: None,
        original_affinity,
        members: applied,
        peak_commit: 0,
    })
}

#[allow(clippy::too_many_arguments)]
fn apply_job_path(
    job_name: &str,
    policy_id: Option<&str>,
    limits: &PolicyLimits,
    roots: &[PidKey],
    snapshot: &ProcessSnapshot,
    tree: &ProcessTree,
    journal: &mut ActiveJobJournal,
    journal_path: &Path,
) -> Result<ApplyOutcome, String> {
    let keys = collect_target_keys(roots, tree);
    if keys.is_empty() {
        return Err("目标进程已经退出".to_owned());
    }

    let mut failures: Vec<(u32, String, String)> = Vec::new();
    let mut handles = HashMap::new();
    let mut original_affinity = HashMap::new();
    for key in &keys {
        let Some(record) = snapshot.find_by_key(*key) else {
            failures.push((
                key.pid,
                format!("{}:{}", key.pid, key.start_time),
                "进程快照中已不存在".to_owned(),
            ));
            continue;
        };
        let handle = match open_for_job(key.pid) {
            Ok(handle) => handle,
            Err(error) => {
                failures.push((key.pid, record.name.clone(), error.to_string()));
                continue;
            }
        };
        let actual_start = process_start_time_handle(handle.raw()).unwrap_or_default();
        if actual_start != 0 && actual_start != key.start_time {
            failures.push((
                key.pid,
                record.name.clone(),
                "PID 已被复用，跳过".to_owned(),
            ));
            continue;
        }
        if let Ok((mask, _)) = process_affinity(handle.raw()) {
            original_affinity.insert(*key, mask);
        }
        handles.insert(*key, handle);
    }

    if handles.is_empty() {
        return Err(format!("无法打开任何目标进程：{}", failure_text(&failures)));
    }

    save_journal(
        journal,
        journal_path,
        record_for(job_name, policy_id, roots, &original_affinity),
    )?;

    let job = match JobHandle::open_or_create(job_name) {
        Ok(job) => job,
        Err(error) => {
            journal.remove_job(job_name);
            let _ = journal.save(journal_path);
            return Err(format!("创建/打开 Job 失败：{error}"));
        }
    };
    let job_limits = JobLimits {
        memory_bytes: limits.memory_bytes,
        affinity_mask: None,
        cpu_rate_percent: limits.cpu_rate_percent,
        subset_affinity: false,
    };
    if let Err(error) = job.set_limits(&job_limits) {
        if job.clear_all_limits().is_ok() {
            journal.remove_job(job_name);
            let _ = journal.save(journal_path);
        } else {
            let _ = journal.save(journal_path);
        }
        return Err(format!("设置 Job 限额失败：{error}"));
    }
    // 重新应用时 Job 可能已经有成员：即使本轮没有任何新进程成功加入，
    // 也不能丢弃这个 Job，否则会连带清除现有成员的限额。
    let had_members = job
        .query_members()
        .map(|members| !members.is_empty())
        .unwrap_or(false);

    let root_set: HashSet<PidKey> = roots.iter().copied().collect();
    let mut ordered = roots.to_vec();
    for key in &keys {
        if !root_set.contains(key) {
            ordered.push(*key);
        }
    }

    let mut assigned = Vec::new();
    let mut applied = Vec::new();
    let mut affinity_only = Vec::new();
    let mut blocked = Vec::new();
    for key in ordered {
        let Some(handle) = handles.get(&key) else {
            continue;
        };
        let name = snapshot
            .find_by_key(key)
            .map(|record| record.name.clone())
            .unwrap_or_else(|| key.pid.to_string());
        let already_in_job = is_in_job(handle.raw(), None).unwrap_or(false);
        match job.assign(handle.raw()) {
            Ok(()) => {
                assigned.push(key.pid);
                applied.push(key.pid);
                if let Some(mask) = limits.affinity_mask {
                    let ok = unsafe { SetProcessAffinityMask(handle.raw(), mask as usize) };
                    if ok == 0 {
                        failures.push((
                            key.pid,
                            name,
                            format!(
                                "加入 Job 成功，但设置 CPU 亲和性失败：{}",
                                WinError::last("SetProcessAffinityMask")
                            ),
                        ));
                    }
                }
            }
            Err(error) => {
                let blocked_by_foreign_job = error.is_access_denied() && already_in_job;
                if blocked_by_foreign_job {
                    // 目标已在应用自带的 Job 中；本 Job 非空时 Windows 拒绝嵌套。
                    // 这不是故障，降级为仅亲和性。
                    blocked.push(key.pid);
                } else {
                    failures.push((key.pid, name, error.to_string()));
                }
                if let Some(mask) = limits.affinity_mask {
                    let ok = unsafe { SetProcessAffinityMask(handle.raw(), mask as usize) };
                    if ok != 0 {
                        affinity_only.push(key.pid);
                        applied.push(key.pid);
                    } else if blocked_by_foreign_job {
                        failures.push((
                            key.pid,
                            snapshot
                                .find_by_key(key)
                                .map(|record| record.name.clone())
                                .unwrap_or_else(|| key.pid.to_string()),
                            format!(
                                "目标在外部 Job 中无法加入本 Job，且设置 CPU 亲和性失败：{}",
                                WinError::last("SetProcessAffinityMask")
                            ),
                        ));
                    }
                }
            }
        }
    }

    let job_used = !assigned.is_empty() || had_members;
    let report = ApplyReport {
        job_name: job_name.to_owned(),
        roots: roots.len(),
        applied,
        assigned: assigned.clone(),
        affinity_only,
        blocked,
        failed: failures,
        job_used,
        job_limits_requested: true,
    };

    if !job_used {
        // 没有任何进程加入 Job，Job 级内存/CPU 率无法生效；清掉限额并丢弃 Job，
        // 但保留 journal，守护进程仍可恢复已应用的亲和性。
        let _ = job.clear_all_limits();
        let members = report.affinity_only.clone();
        if members.is_empty() && report.blocked.is_empty() && !report.failed.is_empty() {
            return Err(format!(
                "目标无法加入 Job：{}",
                failure_text(&report.failed)
            ));
        }
        return Ok(ApplyOutcome {
            report,
            job: None,
            original_affinity,
            members,
            peak_commit: 0,
        });
    }

    let members = job.query_members().unwrap_or(assigned);
    let peak_commit = job
        .query_limits()
        .map(|limits| limits.peak_job_memory)
        .unwrap_or(0);

    Ok(ApplyOutcome {
        report,
        job: Some(job),
        original_affinity,
        members,
        peak_commit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };

    use crate::model::process::{PidKey, ProcessRecord, ProcessSnapshot};

    fn spawn_ping() -> std::process::Child {
        Command::new("cmd.exe")
            .args(["/C", "ping -n 30 127.0.0.1 > nul"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd")
    }

    fn kill_tree(pid: u32) {
        let _ = Command::new("taskkill.exe")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    fn snapshot_for(pid: u32, start_time: u64) -> ProcessSnapshot {
        ProcessSnapshot::new(vec![ProcessRecord {
            key: PidKey::new(pid, start_time),
            parent_pid: 0,
            name: "cmd.exe".to_owned(),
            path: None,
            ..Default::default()
        }])
    }

    fn snapshot_for_pairs(pairs: &[(u32, u64)]) -> ProcessSnapshot {
        ProcessSnapshot::new(
            pairs
                .iter()
                .map(|(pid, start_time)| ProcessRecord {
                    key: PidKey::new(*pid, *start_time),
                    parent_pid: 0,
                    name: "cmd.exe".to_owned(),
                    path: None,
                    ..Default::default()
                })
                .collect(),
        )
    }

    fn create_doubao_like_job() -> HANDLE {
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        assert!(!job.is_null(), "CreateJobObjectW failed");
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        limits.BasicLimitInformation.ActiveProcessLimit = 1;
        limits.ProcessMemoryLimit = 16 * 1024 * 1024 * 1024;
        let ok = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        assert_ne!(ok, 0, "SetInformationJobObject(Extended) failed");
        job
    }

    #[test]
    fn assign_affinity_and_clear_restores() {
        let mut child = spawn_ping();
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(250));
        let start_time = crate::win::process::process_start_time(pid).expect("start time");
        let snapshot = snapshot_for(pid, start_time);
        let tree = ProcessTree::build(&snapshot);
        let key = PidKey::new(pid, start_time);
        let limits = PolicyLimits {
            memory_bytes: Some(64 * 1024 * 1024),
            affinity_mask: Some(1),
            cpu_rate_percent: None,
            subset_affinity: false,
        };
        let journal_path =
            std::env::temp_dir().join(format!("pjh-test-{}.json", uuid::Uuid::new_v4()));
        let mut journal = ActiveJobJournal::default();
        let job_name = format!("Local\\PJH-Test-{}", uuid::Uuid::new_v4());
        let outcome = apply_policy_for_roots(
            &job_name,
            None,
            &limits,
            &[key],
            &snapshot,
            &tree,
            &mut journal,
            &journal_path,
        )
        .expect("apply job");
        assert!(outcome.report.assigned.contains(&pid));
        let job = outcome.job.expect("job");
        let flags = job.query_limits().expect("query limits").flags;
        assert_ne!(flags & JOB_OBJECT_LIMIT_JOB_MEMORY, 0);
        assert!(job.query_members().expect("members").contains(&pid));
        let handle = open_for_job(pid).expect("open child");
        let (mask, _) = process_affinity(handle.raw()).expect("affinity");
        assert_eq!(mask & 1, 1);

        job.clear_all_limits().expect("clear");
        let failures = job.restore_affinity(
            &outcome.original_affinity,
            crate::win::process::system_affinity_mask(),
        );
        assert!(failures.is_empty(), "{failures:?}");
        drop(job);
        journal.remove_job(&job_name);
        let _ = journal.save(&journal_path);
        let _ = std::fs::remove_file(&journal_path);
        kill_tree(pid);
        let _ = child.wait();
    }

    #[test]
    fn affinity_only_policy_skips_job() {
        let mut child = spawn_ping();
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(250));
        let start_time = crate::win::process::process_start_time(pid).expect("start time");
        let snapshot = snapshot_for(pid, start_time);
        let tree = ProcessTree::build(&snapshot);
        let key = PidKey::new(pid, start_time);
        let limits = PolicyLimits {
            memory_bytes: None,
            affinity_mask: Some(1),
            cpu_rate_percent: None,
            subset_affinity: false,
        };
        let journal_path =
            std::env::temp_dir().join(format!("pjh-test-{}.json", uuid::Uuid::new_v4()));
        let mut journal = ActiveJobJournal::default();
        let job_name = format!("Local\\PJH-Test-{}", uuid::Uuid::new_v4());
        let outcome = apply_policy_for_roots(
            &job_name,
            None,
            &limits,
            &[key],
            &snapshot,
            &tree,
            &mut journal,
            &journal_path,
        )
        .expect("apply affinity");

        assert!(outcome.job.is_none());
        assert!(!outcome.report.job_used);
        assert!(!outcome.report.job_limits_requested);
        assert_eq!(outcome.report.applied, vec![pid]);
        assert!(outcome.report.failed.is_empty());
        let handle = open_for_affinity(pid).expect("open child");
        let (mask, _) = process_affinity(handle.raw()).expect("affinity");
        assert_eq!(mask & 1, 1);

        let original_mask = outcome
            .original_affinity
            .get(&key)
            .copied()
            .expect("original affinity");
        let failures = crate::lifecycle::restore_saved_affinity(&outcome.original_affinity);
        assert!(failures.is_empty(), "{failures:?}");
        let (mask, _) = process_affinity(handle.raw()).expect("affinity");
        assert_eq!(mask, original_mask, "清理后应恢复应用前的亲和性");

        journal.remove_job(&job_name);
        let _ = journal.save(&journal_path);
        let _ = std::fs::remove_file(&journal_path);
        kill_tree(pid);
        let _ = child.wait();
    }

    #[test]
    fn nested_limited_job_is_affinity_only() {
        let mut normal = spawn_ping();
        let mut foreign = spawn_ping();
        std::thread::sleep(std::time::Duration::from_millis(250));
        let normal_pid = normal.id();
        let foreign_pid = foreign.id();
        let normal_start =
            crate::win::process::process_start_time(normal_pid).expect("normal start time");
        let foreign_start =
            crate::win::process::process_start_time(foreign_pid).expect("foreign start time");
        let normal_key = PidKey::new(normal_pid, normal_start);
        let foreign_key = PidKey::new(foreign_pid, foreign_start);
        let snapshot =
            snapshot_for_pairs(&[(normal_pid, normal_start), (foreign_pid, foreign_start)]);
        let tree = ProcessTree::build(&snapshot);

        let outer = create_doubao_like_job();
        let process_handle = open_for_job(foreign_pid).expect("open foreign child");
        let ok = unsafe { AssignProcessToJobObject(outer, process_handle.raw()) };
        assert_ne!(ok, 0, "assign to outer job failed");

        let limits = PolicyLimits {
            memory_bytes: Some(64 * 1024 * 1024),
            affinity_mask: Some(1),
            cpu_rate_percent: None,
            subset_affinity: false,
        };
        let journal_path =
            std::env::temp_dir().join(format!("pjh-test-{}.json", uuid::Uuid::new_v4()));
        let mut journal = ActiveJobJournal::default();
        let job_name = format!("Local\\PJH-Test-{}", uuid::Uuid::new_v4());
        let outcome = apply_policy_for_roots(
            &job_name,
            None,
            &limits,
            &[normal_key, foreign_key],
            &snapshot,
            &tree,
            &mut journal,
            &journal_path,
        )
        .expect("apply nested job");

        // 普通进程先加入，Job 变为非空；已在外部 Job 中的进程随后无法再嵌套进来。
        assert!(outcome.report.job_used);
        assert!(outcome.report.assigned.contains(&normal_pid));
        assert!(outcome.report.blocked.contains(&foreign_pid));
        assert!(outcome.report.affinity_only.contains(&foreign_pid));
        assert!(
            outcome.report.failed.is_empty(),
            "{:?}",
            outcome.report.failed
        );
        for pid in [normal_pid, foreign_pid] {
            let handle = open_for_affinity(pid).expect("open child");
            let (mask, _) = process_affinity(handle.raw()).expect("affinity");
            assert_eq!(mask & 1, 1);
        }

        if let Some(job) = outcome.job.as_ref() {
            let _ = job.clear_all_limits();
        }
        let _ = crate::lifecycle::restore_saved_affinity(&outcome.original_affinity);
        drop(outcome.job);
        unsafe { CloseHandle(outer) };
        journal.remove_job(&job_name);
        let _ = journal.save(&journal_path);
        let _ = std::fs::remove_file(&journal_path);
        kill_tree(normal_pid);
        kill_tree(foreign_pid);
        let _ = normal.wait();
        let _ = foreign.wait();
    }

    #[test]
    fn cleanup_journal_clears_named_job() {
        let mut child = spawn_ping();
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(250));
        let start_time = crate::win::process::process_start_time(pid).expect("start time");
        let snapshot = snapshot_for(pid, start_time);
        let tree = ProcessTree::build(&snapshot);
        let key = PidKey::new(pid, start_time);
        let limits = PolicyLimits {
            memory_bytes: Some(64 * 1024 * 1024),
            affinity_mask: Some(1),
            cpu_rate_percent: None,
            subset_affinity: false,
        };
        let journal_path =
            std::env::temp_dir().join(format!("pjh-test-{}.json", uuid::Uuid::new_v4()));
        let mut journal = ActiveJobJournal::default();
        let job_name = format!("Local\\PJH-Test-{}", uuid::Uuid::new_v4());
        let outcome = apply_policy_for_roots(
            &job_name,
            None,
            &limits,
            &[key],
            &snapshot,
            &tree,
            &mut journal,
            &journal_path,
        )
        .expect("apply job");
        assert!(!outcome.members.is_empty());
        let job = outcome.job.expect("job");
        let original_mask = outcome
            .original_affinity
            .get(&key)
            .copied()
            .expect("original affinity");
        let report = crate::lifecycle::cleanup_journal(&journal);
        assert!(report.is_ok(), "{:?}", report.failures);
        assert!(
            report
                .cleared
                .iter()
                .any(|cleared| cleared.eq_ignore_ascii_case(&job_name)),
            "{:?}",
            report.cleared
        );
        let flags = job.query_limits().expect("query").flags;
        assert_eq!(flags & JOB_OBJECT_LIMIT_JOB_MEMORY, 0);
        let handle = open_for_job(pid).expect("open child");
        let (mask, _) = process_affinity(handle.raw()).expect("affinity");
        assert_eq!(mask, original_mask, "清理后应恢复应用前的亲和性");
        drop(job);
        journal.remove_job(&job_name);
        let _ = journal.save(&journal_path);
        let _ = std::fs::remove_file(&journal_path);
        kill_tree(pid);
        let _ = child.wait();
    }
}
