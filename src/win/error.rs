use std::fmt;

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA,
};

#[derive(Debug)]
pub struct WinError {
    pub code: u32,
    pub context: &'static str,
}

impl WinError {
    pub fn new(code: u32, context: &'static str) -> Self {
        Self { code, context }
    }

    pub fn last(context: &'static str) -> Self {
        Self::new(crate::win::last_error(), context)
    }

    pub fn hint(&self) -> &'static str {
        match self.code {
            ERROR_ACCESS_DENIED => {
                "拒绝访问：目标受保护、已在其他 Job 中且无法嵌套，或当前权限不足"
            }
            ERROR_INSUFFICIENT_BUFFER | ERROR_MORE_DATA => "缓冲区不足，请重试",
            8 => "内存不足",
            87 => "参数无效：目标可能已在不可嵌套的 Job 中",
            1450 => "系统资源不足，无法完成请求",
            _ => "Windows API 调用失败",
        }
    }

    pub fn is_access_denied(&self) -> bool {
        self.code == ERROR_ACCESS_DENIED
    }
}

impl fmt::Display for WinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = std::io::Error::from_raw_os_error(self.code as i32);
        write!(
            formatter,
            "{}（Win32 {}：{}；{}）",
            self.context,
            self.code,
            message,
            self.hint()
        )
    }
}

impl std::error::Error for WinError {}

pub type WinResult<T> = Result<T, WinError>;
