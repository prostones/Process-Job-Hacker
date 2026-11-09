use windows_sys::Win32::Foundation::{CloseHandle, LUID};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES,
    TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

pub fn enable_debug_privilege() -> bool {
    let mut token = std::ptr::null_mut();
    let ok = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    };
    if ok == 0 {
        return false;
    }

    let privilege_name = crate::win::to_wide("SeDebugPrivilege");
    let mut luid = LUID::default();
    let ok = unsafe { LookupPrivilegeValueW(std::ptr::null(), privilege_name.as_ptr(), &mut luid) };
    if ok == 0 {
        unsafe { CloseHandle(token) };
        return false;
    }

    let mut privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        ..Default::default()
    };
    privileges.Privileges[0].Luid = luid;
    privileges.Privileges[0].Attributes = SE_PRIVILEGE_ENABLED;

    let ok = unsafe {
        AdjustTokenPrivileges(
            token,
            0,
            &privileges,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    let error = crate::win::last_error();
    unsafe { CloseHandle(token) };
    ok != 0 && error == 0
}
