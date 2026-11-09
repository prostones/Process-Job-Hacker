use std::collections::{HashMap, HashSet};
use std::mem::size_of;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_CPU_RATE_CONTROL_ENABLE,
    JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP, JOB_OBJECT_LIMIT_AFFINITY, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_SUBSET_AFFINITY, JOBOBJECT_BASIC_PROCESS_ID_LIST,
    JOBOBJECT_CPU_RATE_CONTROL_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicProcessIdList, JobObjectCpuRateControlInformation,
    JobObjectExtendedLimitInformation, OpenJobObjectW, QueryInformationJobObject,
    SetInformationJobObject, TerminateJobObject,
};

use crate::model::process::PidKey;
use crate::win::error::{WinError, WinResult};

const JOB_OBJECT_ALL_ACCESS: u32 = 0x001F_001F;
const ERROR_FILE_NOT_FOUND: u32 = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobLimits {
    pub memory_bytes: Option<u64>,
    pub affinity_mask: Option<u64>,
    pub cpu_rate_percent: Option<u8>,
    pub subset_affinity: bool,
}

#[derive(Clone, Debug, Default)]
pub struct JobLimitSnapshot {
    pub flags: u32,
    pub cpu_rate_percent: Option<u32>,
    pub peak_job_memory: u64,
}

#[derive(Debug)]
pub struct JobHandle {
    handle: HANDLE,
}

impl JobHandle {
    pub fn create_named(name: &str) -> WinResult<Self> {
        let wide = crate::win::to_wide(name);
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), wide.as_ptr()) };
        if handle.is_null() {
            return Err(WinError::last("CreateJobObjectW"));
        }
        Ok(Self { handle })
    }

    pub fn open_named(name: &str) -> WinResult<Option<Self>> {
        let wide = crate::win::to_wide(name);
        let handle = unsafe { OpenJobObjectW(JOB_OBJECT_ALL_ACCESS, 0, wide.as_ptr()) };
        if handle.is_null() {
            let error = crate::win::last_error();
            if error == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            return Err(WinError::new(error, "OpenJobObjectW"));
        }
        Ok(Some(Self { handle }))
    }

    pub fn open_or_create(name: &str) -> WinResult<Self> {
        if let Some(job) = Self::open_named(name)? {
            return Ok(job);
        }
        Self::create_named(name)
    }

    pub fn assign(&self, process: HANDLE) -> WinResult<()> {
        let ok = unsafe { AssignProcessToJobObject(self.handle, process) };
        if ok == 0 {
            Err(WinError::last("AssignProcessToJobObject"))
        } else {
            Ok(())
        }
    }

    pub fn set_limits(&self, limits: &JobLimits) -> WinResult<()> {
        let mut extended: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        let mut flags = 0u32;

        if let Some(bytes) = limits.memory_bytes {
            flags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
            extended.JobMemoryLimit = bytes.min(usize::MAX as u64) as usize;
        }
        if let Some(mask) = limits.affinity_mask {
            if mask == 0 {
                return Err(WinError::new(87, "JobLimits::set_limits"));
            }
            flags |= JOB_OBJECT_LIMIT_AFFINITY;
            if limits.subset_affinity {
                flags |= JOB_OBJECT_LIMIT_SUBSET_AFFINITY;
            }
            extended.BasicLimitInformation.Affinity = mask as usize;
        }
        extended.BasicLimitInformation.LimitFlags = flags;

        let ok = unsafe {
            SetInformationJobObject(
                self.handle,
                JobObjectExtendedLimitInformation,
                (&extended as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(WinError::last("SetInformationJobObject(Extended)"));
        }

        self.apply_cpu_rate(limits.cpu_rate_percent)
    }

    fn apply_cpu_rate(&self, percent: Option<u8>) -> WinResult<()> {
        let mut rate: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = unsafe { std::mem::zeroed() };
        if let Some(percent) = percent {
            rate.ControlFlags =
                JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
            rate.Anonymous.CpuRate = (percent.clamp(1, 100) as u32) * 100;
            let ok = unsafe {
                SetInformationJobObject(
                    self.handle,
                    JobObjectCpuRateControlInformation,
                    (&rate as *const JOBOBJECT_CPU_RATE_CONTROL_INFORMATION).cast(),
                    size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                return Err(WinError::last("SetInformationJobObject(CpuRate)"));
            }
            return Ok(());
        }

        let currently_enabled = self
            .query_limits()
            .map(|limits| limits.cpu_rate_percent.is_some())
            .unwrap_or(false);
        if !currently_enabled {
            return Ok(());
        }

        // Windows rejects ControlFlags = 0 on some builds. Try it first, then fall
        // back to an effectively unlimited 100% hard cap.
        let ok = unsafe {
            SetInformationJobObject(
                self.handle,
                JobObjectCpuRateControlInformation,
                (&rate as *const JOBOBJECT_CPU_RATE_CONTROL_INFORMATION).cast(),
                size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
            )
        };
        if ok != 0 {
            return Ok(());
        }
        rate.ControlFlags =
            JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
        rate.Anonymous.CpuRate = 10_000;
        let ok = unsafe {
            SetInformationJobObject(
                self.handle,
                JobObjectCpuRateControlInformation,
                (&rate as *const JOBOBJECT_CPU_RATE_CONTROL_INFORMATION).cast(),
                size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(WinError::last("SetInformationJobObject(CpuRateDisable)"));
        }
        Ok(())
    }

    pub fn clear_all_limits(&self) -> WinResult<()> {
        self.set_limits(&JobLimits::default())
    }

    pub fn query_limits(&self) -> WinResult<JobLimitSnapshot> {
        let mut extended: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        let mut returned = 0u32;
        let ok = unsafe {
            QueryInformationJobObject(
                self.handle,
                JobObjectExtendedLimitInformation,
                (&mut extended as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                &mut returned,
            )
        };
        if ok == 0 {
            return Err(WinError::last("QueryInformationJobObject(Extended)"));
        }

        let mut rate: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = unsafe { std::mem::zeroed() };
        let mut rate_returned = 0u32;
        let rate_ok = unsafe {
            QueryInformationJobObject(
                self.handle,
                JobObjectCpuRateControlInformation,
                (&mut rate as *mut JOBOBJECT_CPU_RATE_CONTROL_INFORMATION).cast(),
                size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
                &mut rate_returned,
            )
        };
        let cpu_rate_percent =
            if rate_ok != 0 && rate.ControlFlags & JOB_OBJECT_CPU_RATE_CONTROL_ENABLE != 0 {
                let raw = unsafe { rate.Anonymous.CpuRate };
                let percent = raw / 100;
                (percent < 100).then_some(percent)
            } else {
                None
            };

        Ok(JobLimitSnapshot {
            flags: extended.BasicLimitInformation.LimitFlags,
            cpu_rate_percent,
            peak_job_memory: extended.PeakJobMemoryUsed as u64,
        })
    }

    pub fn query_members(&self) -> WinResult<Vec<u32>> {
        let header_size = size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>();
        let mut capacity = 256usize;
        loop {
            let bytes = header_size.saturating_add(
                capacity
                    .saturating_sub(1)
                    .saturating_mul(size_of::<usize>()),
            );
            let mut buffer = vec![0u8; bytes];
            let list = buffer.as_mut_ptr() as *mut JOBOBJECT_BASIC_PROCESS_ID_LIST;
            let mut returned = 0u32;
            let ok = unsafe {
                QueryInformationJobObject(
                    self.handle,
                    JobObjectBasicProcessIdList,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                    &mut returned,
                )
            };
            if ok == 0 {
                let error = crate::win::last_error();
                if error == 234 || error == 122 {
                    let assigned = unsafe { (*list).NumberOfAssignedProcesses as usize };
                    capacity = assigned.max(capacity * 2);
                    continue;
                }
                return Err(WinError::new(
                    error,
                    "QueryInformationJobObject(ProcessIdList)",
                ));
            }
            let assigned = unsafe { (*list).NumberOfAssignedProcesses as usize };
            let count = unsafe { (*list).NumberOfProcessIdsInList as usize };
            if assigned > count {
                capacity = assigned;
                continue;
            }
            let ids = unsafe {
                std::slice::from_raw_parts((*list).ProcessIdList.as_ptr(), count)
                    .iter()
                    .map(|value| *value as u32)
                    .collect()
            };
            return Ok(ids);
        }
    }

    pub fn terminate(&self, exit_code: u32) -> WinResult<()> {
        let ok = unsafe { TerminateJobObject(self.handle, exit_code) };
        if ok == 0 {
            Err(WinError::last("TerminateJobObject"))
        } else {
            Ok(())
        }
    }

    /// 一次性恢复 Job 成员以及 `saved` 中其余进程的亲和性。
    ///
    /// 成员按实时启动时间匹配；`saved` 中不属于当前成员的条目（仅亲和性进程、
    /// 已退出的历史进程）也会处理。已退出或 PID 已复用的进程静默跳过。
    pub fn restore_affinity(&self, saved: &HashMap<PidKey, u64>, system_mask: u64) -> Vec<String> {
        let mut failures = Vec::new();
        let mut handled = HashSet::new();
        match self.query_members() {
            Ok(members) => {
                for pid in members {
                    handled.insert(pid);
                    let start_time = match crate::win::process::process_start_time(pid) {
                        Ok(start_time) => start_time,
                        Err(error) if error.code == 87 || error.code == 1168 => continue,
                        Err(error) => {
                            failures.push(format!("PID {pid} 读取启动时间失败：{error}"));
                            continue;
                        }
                    };
                    let key = PidKey::new(pid, start_time);
                    let mask = saved.get(&key).copied().unwrap_or(system_mask);
                    if let Err(error) = crate::win::process::restore_affinity_for_key(key, mask) {
                        failures.push(format!("PID {pid} 恢复亲和性失败：{error}"));
                    }
                }
            }
            Err(error) => failures.push(format!("无法枚举 Job 成员：{error}")),
        }
        for (key, mask) in saved {
            if handled.contains(&key.pid) {
                continue;
            }
            if let Err(error) = crate::win::process::restore_affinity_for_key(*key, *mask) {
                failures.push(format!("PID {} 恢复亲和性失败：{error}", key.pid));
            }
        }
        failures
    }
}

impl Drop for JobHandle {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}
