use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, STILL_ACTIVE};
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
use windows_sys::Win32::System::Threading::{
    GetActiveProcessorCount, GetActiveProcessorGroupCount, GetExitCodeProcess,
    GetProcessAffinityMask, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SET_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE, SetProcessAffinityMask,
    TerminateProcess, WaitForSingleObject,
};

use crate::model::process::PidKey;
use crate::win::error::{WinError, WinResult};

pub const SYNCHRONIZE: u32 = 0x0010_0000;
pub const WAIT_OBJECT_0: u32 = 0;

pub struct OwnedHandle(pub HANDLE);

impl OwnedHandle {
    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

pub fn open_process(pid: u32, access: u32) -> WinResult<OwnedHandle> {
    let handle = unsafe { OpenProcess(access, 0, pid) };
    if handle.is_null() {
        Err(WinError::last("OpenProcess"))
    } else {
        Ok(OwnedHandle(handle))
    }
}

pub fn open_for_job(pid: u32) -> WinResult<OwnedHandle> {
    open_process(
        pid,
        PROCESS_SET_QUOTA
            | PROCESS_TERMINATE
            | PROCESS_SET_INFORMATION
            | PROCESS_QUERY_LIMITED_INFORMATION,
    )
}

pub fn open_for_affinity(pid: u32) -> WinResult<OwnedHandle> {
    open_process(
        pid,
        PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION,
    )
}

pub fn process_start_time(pid: u32) -> WinResult<u64> {
    let handle = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    process_start_time_handle(handle.raw())
}

pub fn process_start_time_handle(handle: HANDLE) -> WinResult<u64> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let ok = unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    if ok == 0 {
        return Err(WinError::last("GetProcessTimes"));
    }
    let ticks = ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64;
    let seconds_since_1601 = ticks / 10_000_000;
    Ok(seconds_since_1601.saturating_sub(11_644_473_600))
}

pub fn process_affinity(handle: HANDLE) -> WinResult<(u64, u64)> {
    let mut process_mask = 0usize;
    let mut system_mask = 0usize;
    let ok = unsafe { GetProcessAffinityMask(handle, &mut process_mask, &mut system_mask) };
    if ok == 0 {
        return Err(WinError::last("GetProcessAffinityMask"));
    }
    Ok((process_mask as u64, system_mask as u64))
}

pub fn set_process_affinity(pid: u32, mask: u64) -> WinResult<()> {
    let handle = open_for_affinity(pid)?;
    let ok = unsafe { SetProcessAffinityMask(handle.raw(), mask as usize) };
    if ok == 0 {
        Err(WinError::last("SetProcessAffinityMask"))
    } else {
        Ok(())
    }
}

/// 进程是否已经退出（包括正在退出的进程）。
pub fn process_has_exited(handle: HANDLE) -> bool {
    let mut exit_code = 0u32;
    let ok = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
    ok != 0 && exit_code != STILL_ACTIVE as u32
}

/// 单个进程的亲和性恢复结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreAffinity {
    /// 亲和性已恢复。
    Restored,
    /// 进程已退出或正在退出，无需恢复。
    Gone,
    /// PID 已被复用（启动时间不匹配），不能改动当前进程。
    Reused,
}

/// 按 PID + 启动时间恢复单个进程的亲和性。
///
/// 已退出、正在退出或 PID 已被复用的进程返回 [`RestoreAffinity::Gone`] /
/// [`RestoreAffinity::Reused`]，由调用方静默跳过；只有进程仍存活且设置失败时才返回错误。
pub fn restore_affinity_for_key(key: PidKey, mask: u64) -> WinResult<RestoreAffinity> {
    let handle = match open_for_affinity(key.pid) {
        Ok(handle) => handle,
        Err(error) if error.code == 87 || error.code == 1168 => {
            return Ok(RestoreAffinity::Gone);
        }
        Err(error) => return Err(error),
    };
    if process_has_exited(handle.raw()) {
        return Ok(RestoreAffinity::Gone);
    }
    let actual_start = match process_start_time_handle(handle.raw()) {
        Ok(start_time) => start_time,
        Err(_) => return Ok(RestoreAffinity::Gone),
    };
    if actual_start != 0 && key.start_time != 0 && actual_start != key.start_time {
        return Ok(RestoreAffinity::Reused);
    }
    let ok = unsafe { SetProcessAffinityMask(handle.raw(), mask as usize) };
    if ok == 0 {
        let error = WinError::last("SetProcessAffinityMask");
        if process_has_exited(handle.raw()) {
            return Ok(RestoreAffinity::Gone);
        }
        return Err(error);
    }
    Ok(RestoreAffinity::Restored)
}

pub fn terminate_process(pid: u32) -> WinResult<()> {
    let handle = open_process(pid, PROCESS_TERMINATE)?;
    let ok = unsafe { TerminateProcess(handle.raw(), 1) };
    if ok == 0 {
        Err(WinError::last("TerminateProcess"))
    } else {
        Ok(())
    }
}

pub fn is_in_job(handle: HANDLE, job: Option<HANDLE>) -> WinResult<bool> {
    let mut result = 0i32;
    let job = job.unwrap_or(std::ptr::null_mut());
    let ok = unsafe { IsProcessInJob(handle, job, &mut result) };
    if ok == 0 {
        Err(WinError::last("IsProcessInJob"))
    } else {
        Ok(result != 0)
    }
}

pub fn is_process_in_any_job(pid: u32) -> WinResult<bool> {
    let handle = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    is_in_job(handle.raw(), None)
}

pub fn wait_for_process(pid: u32, timeout_ms: u32) -> bool {
    let Ok(handle) = open_process(pid, SYNCHRONIZE) else {
        return true;
    };
    let result = unsafe { WaitForSingleObject(handle.raw(), timeout_ms) };
    result == WAIT_OBJECT_0
}

pub fn system_affinity_mask() -> u64 {
    let mut info = SYSTEM_INFO::default();
    unsafe { GetSystemInfo(&mut info) };
    let mask = info.dwActiveProcessorMask as u64;
    if mask != 0 {
        return mask;
    }
    let count = info.dwNumberOfProcessors.min(64);
    if count == 64 {
        u64::MAX
    } else {
        (1u64 << count) - 1
    }
}

pub fn logical_processor_count() -> u32 {
    unsafe { GetActiveProcessorCount(u16::MAX) }
}

pub fn processor_group_count() -> u16 {
    unsafe { GetActiveProcessorGroupCount() }
}
