use std::path::PathBuf;

pub fn run_if_requested(args: &[String]) -> bool {
    if !args.iter().any(|arg| arg == "--guard") {
        return false;
    }
    let parent_pid = value_after(args, "--parent-pid")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    let journal = value_after(args, "--journal")
        .map(PathBuf::from)
        .unwrap_or_else(crate::config::journal_path);
    crate::lifecycle::run_guard(parent_pid, &journal);
    true
}

/// 通知已有主程序退出，供安装器/卸载器使用。
pub fn quit_running_if_requested(args: &[String]) -> bool {
    if !args.iter().any(|arg| arg == "--quit-running") {
        return false;
    }
    if let Err(error) = crate::win::single_instance::quit_running() {
        log::error!("quit running failed: {error}");
        std::process::exit(1);
    }
    true
}

pub fn clear_persisted_if_requested(args: &[String]) -> bool {
    if !args.iter().any(|arg| arg == "--clear-persisted") {
        return false;
    }
    if let Err(error) = crate::lifecycle::run_clear_persisted(
        &crate::config::config_path(),
        &crate::config::journal_path(),
    ) {
        log::error!("clear persisted failed: {error}");
        crate::logging::show_error_message("Process Job Hacker", &format!("清理失败：{error}"));
    }
    true
}

pub fn safe_mode(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--safe-mode")
}

/// 解析 `--sankey-preview <svg路径>`，用于启动独立预览窗口。
pub fn sankey_preview_path(args: &[String]) -> Option<PathBuf> {
    value_after(args, "--sankey-preview").map(PathBuf::from)
}

fn value_after<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}
