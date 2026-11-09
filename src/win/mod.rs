pub mod affinity;
pub mod elevation;
pub mod error;
pub mod job;
pub mod privilege;
pub mod process;
pub mod single_instance;

pub fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn last_error() -> u32 {
    unsafe { windows_sys::Win32::Foundation::GetLastError() }
}
