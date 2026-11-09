use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};

use crate::model::journal::{ActiveJobJournal, JournalAffinity, is_valid_job_name};
use crate::model::policy::Policy;
use crate::model::process::PidKey;
use crate::win::job::JobHandle;
use crate::win::process::{RestoreAffinity, restore_affinity_for_key, system_affinity_mask};

const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Default)]
pub struct CleanupReport {
    pub cleared: Vec<String>,
    pub affinity_restored: usize,
    pub failures: Vec<String>,
}

impl CleanupReport {
    pub fn is_ok(&self) -> bool {
        self.failures.is_empty()
    }

    pub fn merge(&mut self, other: CleanupReport) {
        self.cleared.extend(other.cleared);
        self.affinity_restored += other.affinity_restored;
        self.failures.extend(other.failures);
    }

    pub fn summary(&self) -> String {
        if self.failures.is_empty() {
            format!(
                "已清理 {} 个残留 Job，恢复 {} 个亲和性",
                self.cleared.len(),
                self.affinity_restored
            )
        } else {
            format!(
                "已清理 {} 个 Job、恢复 {} 个亲和性，失败 {} 个：{}",
                self.cleared.len(),
                self.affinity_restored,
                self.failures.len(),
                self.failures.join("；")
            )
        }
    }
}

pub struct GuardHandle {
    pub child: Child,
    pub pid: u32,
    pub start_time: Option<u64>,
}

impl Drop for GuardHandle {
    fn drop(&mut self) {
        // The guard must outlive this process and clean up after it.
        let _ = self.child.try_wait();
    }
}

fn restore_affinity_entries(entries: &[JournalAffinity]) -> Vec<String> {
    let saved: HashMap<PidKey, u64> = entries
        .iter()
        .map(|entry| (entry.key(), entry.mask))
        .collect();
    restore_saved_affinity(&saved)
}

/// 按 PID + 启动时间恢复保存的亲和性。
///
/// 恢复集合不依赖 Job 成员表，因此同样适用于“只应用了亲和性、未加入 Job”的进程。
/// 已退出、正在退出或 PID 已被复用的进程直接跳过，不视为失败。
pub fn restore_saved_affinity(saved: &HashMap<PidKey, u64>) -> Vec<String> {
    let system_mask = system_affinity_mask();
    let mut failures = Vec::new();
    for (key, saved_mask) in saved {
        let mask = if *saved_mask == 0 {
            system_mask
        } else {
            *saved_mask
        };
        match restore_affinity_for_key(*key, mask) {
            Ok(RestoreAffinity::Restored | RestoreAffinity::Gone | RestoreAffinity::Reused) => {}
            Err(error) => failures.push(format!("PID {} 恢复亲和性失败：{error}", key.pid)),
        }
    }
    failures
}

pub fn cleanup_journal(journal: &ActiveJobJournal) -> CleanupReport {
    let system_mask = system_affinity_mask();
    let mut report = CleanupReport::default();
    for record in &journal.jobs {
        if !is_valid_job_name(&record.job_name) {
            report
                .failures
                .push(format!("忽略非 PJH Job：{}", record.job_name));
            continue;
        }
        match JobHandle::open_named(&record.job_name) {
            Ok(Some(job)) => {
                let mut saved = HashMap::new();
                for affinity in &record.original_affinity {
                    saved.insert(affinity.key(), affinity.mask);
                }
                let cleared = match job.clear_all_limits() {
                    Ok(()) => true,
                    Err(error) => {
                        report
                            .failures
                            .push(format!("{} 清除限额失败：{error}", record.job_name));
                        false
                    }
                };
                let restore_failures = job.restore_affinity(&saved, system_mask);
                report.failures.extend(
                    restore_failures
                        .into_iter()
                        .map(|failure| format!("{}：{failure}", record.job_name)),
                );
                if cleared {
                    report.cleared.push(record.job_name.clone());
                }
            }
            Ok(None) => {
                // The job object may already have been destroyed (for example when
                // it was a nested job and the last handle closed). The affinity set
                // by the job is not automatically rolled back, so restore it from
                // the journal using PID + start time.
                let failures = restore_affinity_entries(&record.original_affinity);
                if failures.is_empty() {
                    report.affinity_restored += record.original_affinity.len();
                    report.cleared.push(record.job_name.clone());
                } else {
                    report.failures.extend(failures);
                }
            }
            Err(error) => report
                .failures
                .push(format!("{} 打开失败：{error}", record.job_name)),
        }
    }
    report
}

pub fn cleanup_policy_jobs(policies: &[Policy], journal: &ActiveJobJournal) -> CleanupReport {
    let known: std::collections::HashSet<String> = journal
        .jobs
        .iter()
        .map(|job| job.job_name.to_lowercase())
        .collect();
    let system_mask = system_affinity_mask();
    let mut report = CleanupReport::default();
    for policy in policies {
        if known.contains(&policy.job_name.to_lowercase()) {
            continue;
        }
        if !is_valid_job_name(&policy.job_name) {
            continue;
        }
        match JobHandle::open_named(&policy.job_name) {
            Ok(Some(job)) => {
                let cleared = match job.clear_all_limits() {
                    Ok(()) => true,
                    Err(error) => {
                        report
                            .failures
                            .push(format!("{} 清除限额失败：{error}", policy.job_name));
                        false
                    }
                };
                let failures = job.restore_affinity(&HashMap::new(), system_mask);
                report.failures.extend(
                    failures
                        .into_iter()
                        .map(|failure| format!("{}：{failure}", policy.job_name)),
                );
                if cleared {
                    report.cleared.push(policy.job_name.clone());
                }
            }
            Ok(None) => {}
            Err(error) => report
                .failures
                .push(format!("{} 打开失败：{error}", policy.job_name)),
        }
    }
    report
}

pub fn wait_for_previous_guard(journal: &ActiveJobJournal) {
    let (Some(pid), Some(start_time)) = (journal.guard_pid, journal.guard_start_time) else {
        return;
    };
    let Ok(actual_start) = crate::win::process::process_start_time(pid) else {
        return;
    };
    if actual_start != start_time {
        return;
    }
    log::info!("waiting for previous guard pid {pid}");
    if !crate::win::process::wait_for_process(pid, 3_000) {
        log::warn!("previous guard pid {pid} did not exit; terminating it");
        let _ = crate::win::process::terminate_process(pid);
        let _ = crate::win::process::wait_for_process(pid, 1_000);
    }
}

pub fn spawn_guard(journal_path: &Path) -> anyhow::Result<GuardHandle> {
    let exe = std::env::current_exe()?;
    let parent_pid = std::process::id();
    let journal = journal_path.to_string_lossy().into_owned();
    let args = vec![
        "--guard".to_owned(),
        "--parent-pid".to_owned(),
        parent_pid.to_string(),
        "--journal".to_owned(),
        journal,
    ];
    let mut command = Command::new(&exe);
    command.args(&args);
    command.creation_flags(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            log::warn!("guard spawn with breakaway failed: {error}; retrying without breakaway");
            let mut fallback = Command::new(&exe);
            fallback.args(&args);
            fallback.creation_flags(CREATE_NO_WINDOW);
            fallback.spawn()?
        }
    };
    let pid = child.id();
    let start_time = crate::win::process::process_start_time(pid).ok();
    log::info!("guard started: pid {pid}");
    Ok(GuardHandle {
        child,
        pid,
        start_time,
    })
}

pub fn run_guard(parent_pid: u32, journal_path: &Path) {
    log::info!("guard started for parent {parent_pid}");
    let parent = crate::win::process::open_process(parent_pid, crate::win::process::SYNCHRONIZE);
    if let Ok(parent) = parent {
        unsafe {
            windows_sys::Win32::System::Threading::WaitForSingleObject(
                parent.raw(),
                windows_sys::Win32::System::Threading::INFINITE,
            );
        }
    }
    let journal = ActiveJobJournal::load(journal_path);
    let report = cleanup_journal(&journal);
    if report.is_ok() {
        log::info!("guard cleanup: {}", report.summary());
    } else {
        log::error!("guard cleanup: {}", report.summary());
    }
    let mut remaining = journal;
    remaining.jobs.retain(|job| {
        !report
            .cleared
            .iter()
            .any(|cleared| cleared.eq_ignore_ascii_case(&job.job_name))
    });
    if let Err(error) = remaining.save(journal_path) {
        log::error!("guard failed to save journal: {error}");
    }
    log::info!("guard exiting");
}

pub fn run_clear_persisted(config_path: &Path, journal_path: &Path) -> anyhow::Result<()> {
    let config = crate::config::AppConfig::load(config_path);
    let journal = ActiveJobJournal::load(journal_path);
    wait_for_previous_guard(&journal);
    let mut report = cleanup_journal(&journal);
    report.merge(cleanup_policy_jobs(&config.policies, &journal));
    let mut remaining = journal;
    remaining.jobs.retain(|job| {
        !report
            .cleared
            .iter()
            .any(|cleared| cleared.eq_ignore_ascii_case(&job.job_name))
    });
    remaining.save(journal_path)?;
    crate::logging::show_error_message("Process Job Hacker", &report.summary());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn restore_saved_affinity_includes_non_members() {
        let mut child = Command::new("cmd.exe")
            .args(["/C", "ping -n 30 127.0.0.1 > nul"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd");
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(250));
        let start_time = crate::win::process::process_start_time(pid).expect("start time");
        crate::win::process::set_process_affinity(pid, 1).expect("set affinity");

        let mut saved = HashMap::new();
        saved.insert(PidKey::new(pid, start_time), system_affinity_mask());
        let failures = restore_saved_affinity(&saved);
        assert!(failures.is_empty(), "{failures:?}");

        let handle = crate::win::process::open_for_affinity(pid).expect("open");
        let (mask, _) = crate::win::process::process_affinity(handle.raw()).expect("affinity");
        let system_mask = system_affinity_mask();
        assert_eq!(mask & system_mask, system_mask);

        let _ = Command::new("taskkill.exe")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = child.wait();
    }

    fn spawn_ping() -> Child {
        Command::new("cmd.exe")
            .args(["/C", "ping -n 30 127.0.0.1 > nul"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd")
    }

    fn kill_tree(pid: u32, child: &mut Child) {
        let _ = Command::new("taskkill.exe")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = child.wait();
    }

    #[test]
    fn restore_skips_reused_pid() {
        let mut child = spawn_ping();
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(250));
        let start_time = crate::win::process::process_start_time(pid).expect("start time");
        let mut saved = HashMap::new();
        saved.insert(PidKey::new(pid, start_time.saturating_add(1)), 1);
        let failures = restore_saved_affinity(&saved);
        assert!(failures.is_empty(), "PID 复用不应算失败：{failures:?}");
        kill_tree(pid, &mut child);
    }

    #[test]
    fn restore_skips_exited_pid() {
        let mut child = spawn_ping();
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(250));
        let start_time = crate::win::process::process_start_time(pid).expect("start time");
        kill_tree(pid, &mut child);
        let mut saved = HashMap::new();
        saved.insert(PidKey::new(pid, start_time), 1);
        let failures = restore_saved_affinity(&saved);
        assert!(failures.is_empty(), "已退出进程不应算失败：{failures:?}");
    }

    #[test]
    fn process_has_exited_tracks_lifecycle() {
        let mut child = spawn_ping();
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(250));
        let handle = crate::win::process::open_for_affinity(pid).expect("open");
        assert!(!crate::win::process::process_has_exited(handle.raw()));
        kill_tree(pid, &mut child);
        assert!(crate::win::process::process_has_exited(handle.raw()));
    }
}
