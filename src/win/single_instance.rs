use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, SetLastError,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent,
    WaitForMultipleObjects,
};

use crate::model::journal::ActiveJobJournal;

const MUTEX_NAME: &str = "Local\\ProcessJobHacker.Instance";
const SHOW_EVENT_NAME: &str = "Local\\ProcessJobHacker.Show";
const QUIT_EVENT_NAME: &str = "Local\\ProcessJobHacker.Quit";
const SYNCHRONIZE: u32 = 0x0010_0000;
const INSTANCE_EXIT_TIMEOUT_MS: u32 = 15_000;
const GUARD_EXIT_TIMEOUT_MS: u32 = 15_000;
const WAIT_INTERVAL_MS: u64 = 100;

pub struct SingleInstanceGuard {
    mutex: windows_sys::Win32::Foundation::HANDLE,
    show_event: windows_sys::Win32::Foundation::HANDLE,
    quit_event: windows_sys::Win32::Foundation::HANDLE,
    show_requested: Arc<AtomicBool>,
    quit_requested: Arc<AtomicBool>,
}

impl SingleInstanceGuard {
    pub fn acquire() -> anyhow::Result<Option<Self>> {
        let mutex_name = crate::win::to_wide(MUTEX_NAME);
        let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr()) };
        if mutex.is_null() {
            anyhow::bail!("CreateMutexW 失败：{}", crate::win::last_error());
        }
        if crate::win::last_error() == ERROR_ALREADY_EXISTS {
            unsafe { CloseHandle(mutex) };
            Self::signal_existing_show();
            return Ok(None);
        }

        let show_event_name = crate::win::to_wide(SHOW_EVENT_NAME);
        let show_event = unsafe { CreateEventW(std::ptr::null(), 0, 0, show_event_name.as_ptr()) };
        if show_event.is_null() {
            unsafe { CloseHandle(mutex) };
            anyhow::bail!("CreateEventW(show) 失败：{}", crate::win::last_error());
        }

        let quit_event_name = crate::win::to_wide(QUIT_EVENT_NAME);
        let quit_event = unsafe { CreateEventW(std::ptr::null(), 0, 0, quit_event_name.as_ptr()) };
        if quit_event.is_null() {
            unsafe {
                CloseHandle(show_event);
                CloseHandle(mutex);
            }
            anyhow::bail!("CreateEventW(quit) 失败：{}", crate::win::last_error());
        }

        Ok(Some(Self {
            mutex,
            show_event,
            quit_event,
            show_requested: Arc::new(AtomicBool::new(false)),
            quit_requested: Arc::new(AtomicBool::new(false)),
        }))
    }

    fn signal_existing_show() {
        let event_name = crate::win::to_wide(SHOW_EVENT_NAME);
        let event = unsafe { OpenEventW(SYNCHRONIZE | EVENT_MODIFY_STATE, 0, event_name.as_ptr()) };
        if !event.is_null() {
            unsafe {
                SetEvent(event);
                CloseHandle(event);
            }
        }
    }

    /// 通知已有实例退出。返回 false 表示当前没有实例在运行，或旧版本没有退出事件。
    fn signal_existing_quit() -> bool {
        let event_name = crate::win::to_wide(QUIT_EVENT_NAME);
        let event = unsafe { OpenEventW(SYNCHRONIZE | EVENT_MODIFY_STATE, 0, event_name.as_ptr()) };
        if event.is_null() {
            return false;
        }
        let signaled = unsafe { SetEvent(event) } != 0;
        unsafe { CloseHandle(event) };
        signaled
    }

    /// 让已安装的主程序自动退出，并等待主进程和 guard 完成清理。
    ///
    /// 旧版本没有 `Local\ProcessJobHacker.Quit` 事件时会立即返回 Ok，由安装器
    /// 回退到只结束主窗口进程的 taskkill 路径。
    pub fn quit_running() -> anyhow::Result<()> {
        let journal = ActiveJobJournal::load(&crate::config::journal_path());
        if !Self::signal_existing_quit() {
            return Ok(());
        }

        if !wait_for_instance_exit(INSTANCE_EXIT_TIMEOUT_MS) {
            anyhow::bail!("主程序在 {} 秒内未退出", INSTANCE_EXIT_TIMEOUT_MS / 1000);
        }

        if let (Some(pid), Some(start_time)) = (journal.guard_pid, journal.guard_start_time) {
            let same_process =
                crate::win::process::process_start_time(pid).ok() == Some(start_time);
            if same_process && !crate::win::process::wait_for_process(pid, GUARD_EXIT_TIMEOUT_MS) {
                anyhow::bail!(
                    "guard 进程 {pid} 在 {} 秒内未退出",
                    GUARD_EXIT_TIMEOUT_MS / 1000
                );
            }
        }
        Ok(())
    }

    pub fn start_show_listener(&self, ctx: egui::Context) {
        let show_requested = self.show_requested.clone();
        let quit_requested = self.quit_requested.clone();
        let _ = thread::Builder::new()
            .name("pjh-single-instance".to_owned())
            .spawn(move || {
                let show_event_name = crate::win::to_wide(SHOW_EVENT_NAME);
                let show_event = unsafe {
                    OpenEventW(
                        SYNCHRONIZE | EVENT_MODIFY_STATE,
                        0,
                        show_event_name.as_ptr(),
                    )
                };
                let quit_event_name = crate::win::to_wide(QUIT_EVENT_NAME);
                let quit_event = unsafe {
                    OpenEventW(
                        SYNCHRONIZE | EVENT_MODIFY_STATE,
                        0,
                        quit_event_name.as_ptr(),
                    )
                };
                if show_event.is_null() || quit_event.is_null() {
                    if !show_event.is_null() {
                        unsafe { CloseHandle(show_event) };
                    }
                    if !quit_event.is_null() {
                        unsafe { CloseHandle(quit_event) };
                    }
                    return;
                }

                let handles = [show_event, quit_event];
                loop {
                    let wait = unsafe {
                        WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE)
                    };
                    if wait == 0 {
                        show_requested.store(true, Ordering::SeqCst);
                        ctx.request_repaint();
                    } else if wait == 1 {
                        quit_requested.store(true, Ordering::SeqCst);
                        ctx.request_repaint();
                        break;
                    } else {
                        break;
                    }
                }
                unsafe {
                    CloseHandle(show_event);
                    CloseHandle(quit_event);
                }
            });
    }

    pub fn take_show_request(&self) -> bool {
        self.show_requested.swap(false, Ordering::SeqCst)
    }

    pub fn take_quit_request(&self) -> bool {
        self.quit_requested.swap(false, Ordering::SeqCst)
    }
}

/// 供 `--quit-running` CLI 使用的自由函数入口。
pub fn quit_running() -> anyhow::Result<()> {
    SingleInstanceGuard::quit_running()
}

fn instance_exists() -> bool {
    let mutex_name = crate::win::to_wide(MUTEX_NAME);
    unsafe { SetLastError(0) };
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr()) };
    if mutex.is_null() {
        return false;
    }
    let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    unsafe { CloseHandle(mutex) };
    exists
}

fn wait_for_instance_exit(timeout_ms: u32) -> bool {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
    loop {
        if !instance_exists() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(WAIT_INTERVAL_MS));
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.show_event.is_null() {
                CloseHandle(self.show_event);
            }
            if !self.quit_event.is_null() {
                CloseHandle(self.quit_event);
            }
            if !self.mutex.is_null() {
                CloseHandle(self.mutex);
            }
        }
    }
}
