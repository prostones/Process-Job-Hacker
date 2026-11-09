use flexi_logger::{FileSpec, Logger};

pub fn init() {
    let log_dir = crate::config::log_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    let result = Logger::try_with_str("info").and_then(|logger| {
        logger
            .log_to_file(
                FileSpec::default()
                    .directory(&log_dir)
                    .basename("process-job-hacker")
                    .suffix("log"),
            )
            .start()
    });
    if let Err(error) = result {
        eprintln!("failed to initialize file logger: {error}");
    }

    std::panic::set_hook(Box::new(|info| {
        log::error!("panic: {info}");
        let message = format!("Process Job Hacker 发生未处理异常：\n{info}");
        show_error_message("Process Job Hacker", &message);
    }));
}

pub fn show_error_message(title: &str, message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let title = crate::win::to_wide(title);
    let message = crate::win::to_wide(message);
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}
