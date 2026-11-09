# AGENTS.md

Windows-only Rust desktop app (eframe/egui 0.37 + Win32 Job Objects) that caps CPU affinity, Job commit memory, and CPU rate for process trees. `README.md` is the user-facing manual; `PLAN.md` is a historical checklist whose test counts are stale and whose remaining unchecked items are mostly manual-only. The public repository is `https://github.com/prostones/win-job-limiter` (branch `main`); keep source, docs, screenshots, and release artifacts free of build-machine paths, user names, and company identifiers.

## Verify

Run in this order before claiming done:

1. `cargo fmt --check`
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo test`
4. `scripts/package.ps1`, then confirm `rg -a -c "C:\Users\" dist\ProcessJobHacker-<version>-win-x64\process-job-hacker.exe` reports nothing (see the path-remapping bullet under Packaging).

There is no CI; these checks plus `scripts/package.ps1` are the only automated gate. Single test: `cargo test <substring>` (for example `cargo test affinity_only_policy_skips_job`).

Tests are inline `#[cfg(test)]` modules. There is no library target, so a `tests/` integration test cannot import the private `apply`/`win` modules. `cargo test` includes real Win32 integration tests: it spawns `cmd.exe /C ping`, creates Job objects, changes affinity, and restores it. It requires Windows; do not replace these with mocks.

## Packaging

- `scripts/package.ps1` is the only distribution entry point. It runs the three checks unless `-SkipTests`, builds with static CRT by default (`-DynamicCrt` opts out) into `target\x86_64-pc-windows-msvc\release`, stages `dist\ProcessJobHacker-<version>-win-x64`, writes `SHA256SUMS.txt`, builds the portable ZIP, and compiles the Inno Setup installer (`-SkipInstaller` for ZIP only).
- The release build gets `--remap-path-prefix=$env:USERPROFILE=C:\build` and `--remap-path-prefix=$repoRoot=C:\build\project` in `RUSTFLAGS`, so the panic location strings baked in from dependencies cannot reveal the build machine's user name or checkout path. These are plain rustc options, not codegen options: `-C remap-path-prefix=` fails with `error: unknown codegen option`. Keep them on both the static and the dynamic CRT path.
- Inno Setup is pinned to 7.1.0 by `installer/InnoSetup.csproj` and restored from NuGet; `scripts/Get-InnoSetup.ps1` locates `ISCC.exe`. Do not rely on a system-wide Inno Setup install.
- `installer/process-job-hacker.iss` installs per-user by default (`PrivilegesRequired=lowest`) to `%LOCALAPPDATA%\Programs\Process Job Hacker` and offers all-users at the first dialog. Keep its `AppId` stable forever: changing it installs a second copy instead of upgrading. It silently uninstalls the previous version with `/UPGRADE` while keeping `%APPDATA%\prostones\ProcessJobHacker` and `%LOCALAPPDATA%\prostones\ProcessJobHacker`.
- Keep `installer/process-job-hacker.iss` UTF-8 with BOM: it contains Chinese messages and Inno Setup needs the BOM to decode them.
- The release exe still uses `app.admin.manifest` (`requireAdministrator`): per-user install removes setup elevation, not launch elevation.
- Do not use `runasoriginaluser` for the postinstall launch: Inno runs it through `CreateProcess` with the non-elevated token, so the `requireAdministrator` exe fails with error 740 (`ERROR_ELEVATION_REQUIRED`). Use `shellexec` with `Verb: "runas"` instead; this goes through ShellExecute and triggers the normal UAC prompt.
- The installer closes a running copy through `installer/stop-running.ps1`. Without `-Force` it signals `Local\ProcessJobHacker.Quit` (no UAC), waits for the mutex and guard, and exits 0; exit 2 means an old version has no quit event or elevation is required, exit 3 means timeout. Interactive installs then re-run it elevated with `-Force`, which reads command lines, kills only the main and `--sankey-preview` processes, leaves `--guard` alive, and waits for the journal's guard PID. Silent installs cannot elevate, so a failed graceful quit aborts them. Never use `taskkill /IM` or `/T`: killing the guard would leave Job limits and affinity behind.
- `--quit-running` exposes the same quit event for manual use, but launching the installed exe with it triggers UAC because of `requireAdministrator`; the installer avoids that by calling the PowerShell helper directly.
- Release artifacts stay under `dist/` (ignored by git). Upload the Setup.exe, ZIP and SHA256 files to the GitHub release manually.
- Optional signing: pass `-PfxPath` and `-PfxPassword` (or set `PJH_PFX_PASSWORD`); the script signs the exe and installer and verifies them with `signtool`.

## Run and build gotchas

- `cargo run` opens a long-lived GUI. Closing the window hides to tray by default; the process keeps running with limits active. Stop it with the tray exit or `cargo run -- --quit-running`; if you must kill it, kill only the main process and leave `--guard` alive so it can clear limits.
- The app is single-instance. A second launch signals the running instance to show and exits silently, so an apparently "no output" `cargo run` usually means an instance is already running; `--quit-running` signals quit instead. `--sankey-preview <svg>` bypasses the mutex and normal app init entirely.
- Debug builds use `app.manifest` (`asInvoker`); release builds use `app.admin.manifest` (`requireAdministrator`) selected by `build.rs`. Use debug for UI work to avoid UAC, but debug cannot limit elevated/protected targets; use the in-app `以管理员身份重启` button or the release exe for those.
- Plain `cargo build --release` produces `target\release\process-job-hacker.exe`; `scripts/package.ps1` builds the `x86_64-pc-windows-msvc` target under `target\x86_64-pc-windows-msvc\release`. Release builds set `windows_subsystem = "windows"` (no console). Logs always go to `%LOCALAPPDATA%\prostones\ProcessJobHacker\data\logs\`.
- `cargo run -- --clear-persisted` cleans stale jobs/affinity and then shows a blocking `MessageBoxW`; do not run it in a non-interactive pipeline. `--safe-mode` starts the UI without auto-applying policies.

## Architecture

- `src/main.rs` — CLI dispatch (`--sankey-preview`, `--quit-running`, `--guard`, `--clear-persisted`, `--safe-mode`), single-instance mutex/events, eframe boot.
- `src/app.rs` — `PjhApp`: all UI state, tray commands, snapshot ingestion, 1.5 s auto-reapply, exit flow (`quit_from_installer` for `--quit-running`). UI strings are Chinese; keep them Chinese.
- `src/apply.rs` — sole policy entry point `apply_policy_for_roots`; chooses affinity-only vs Job path.
- `src/lifecycle.rs` — startup cleanup, crash-guard spawn/cleanup, journal-driven affinity restore.
- `src/monitor.rs` — background `sysinfo` thread; sends `Arc<ProcessSnapshot>` / `Arc<CpuHistory>` over crossbeam. UI code must not call `sysinfo` directly.
- `src/sankey.rs` — pure SVG rendering; `src/sankey_window.rs` — standalone `--sankey-preview` viewer.
- `src/win/` — raw Win32 FFI (`job`, `process`, `affinity`, `single_instance`); `src/model/` — serializable state and pure logic; `src/ui/` — egui views.
- Interactive views under `src/ui/` return action enums (`TreeEvent`, `PolicyAction`, `LimitEditorAction`) that `app.rs` handles; follow that pattern for new policy/tree actions instead of mutating app state in the view.
- eframe 0.37 uses `eframe::App::{logic, ui}`, not `update`. `logic` is still called while the window is hidden when a repaint is requested (no painting allowed) and must drain channels/mutate state; `ui` only renders. Put hidden-window work in `logic`.

## Native window theming (eframe 0.37)

- `egui::Context::set_theme(ThemePreference::Dark)` only changes egui visuals; it does **not** set the Win32 non-client title bar. The title bar requires `ViewportCommand::SetTheme(SystemTheme::Dark)`.
- The first `SetTheme(Dark)` can be applied while the viewport is still unfocused, so Windows renders a light/inactive title bar until the focus state changes. This is why a window can look gray on first open and turn dark only after losing and regaining focus.
- Required pattern for every eframe window, including auxiliary/preview windows:
  1. send `SetTheme(Dark)` once on the first `logic` frame (covers child windows that never receive focus);
  2. send it again once when `ctx.input(|input| input.viewport().focused.unwrap_or(false))` becomes true (covers the active title bar);
  3. use separate bool flags so each send happens at most once.
- `src/app.rs` (`PjhApp::logic`) and `src/sankey_window.rs` (`SankeyPreviewApp::logic`) already implement this. New independent windows must copy it.

## Invariants that regress easily

- Process identity is `PidKey { pid, start_time }`, never a bare PID. Re-check start time before affinity/Job operations; PIDs are reused.
- Affinity-only policies must not create a Job (`needs_job_object` is true only for memory or CPU rate). Chrome/Doubao already own Jobs; creating one causes noisy `AssignProcessToJobObject` failures.
- In this app Job Objects carry only memory/CPU-rate limits. CPU affinity is applied per process via `SetProcessAffinityMask` so destroying a Job cannot leave stale affinity.
- Do not add `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: exit and cleanup must not terminate target processes.
- `AssignProcessToJobObject` access denied for a process already in a foreign Job is normal: degrade that process to affinity-only (`blocked` / `affinity_only`), do not abort the whole policy.
- Capture original affinity before changing it. `ActiveJobJournal::upsert_job` must preserve the first-captured mask; overwriting it on reapply breaks restore.
- Write the journal before applying limits. Keep policy job names stable (`Local\PJH-Policy-{id}` / `Local\PJH-Temp-{id}`) and only clean names accepted by `is_valid_job_name` (`Local\PJH-*`); never broaden that prefix.
- Exit order is clear limits -> restore affinity -> save/clear journal (`perform_exit_cleanup`).
- Reject `is_critical_process` (includes PID <= 4) and the app's own PID before applying.
- Affinity is processor-group 0 only, max 64 logical CPUs. P/E split: equal `EfficiencyClass` means homogeneous; max class means P cores.
- Config and journal writes use `.tmp` + rename; preserve atomic writes.

## Known placeholders

- `PolicyMatch::cmdline_contains` is unimplemented: non-`None` values never match (`match_command_line`).
- `AppConfig::refresh_visible_ms` / `refresh_hidden_ms` are persisted but unused; `monitor.rs` hardcodes 1 s / 2 s.
- The UI `SUBSET_AFFINITY` checkbox is inert: `apply_job_path` passes `subset_affinity: false`, and affinity is applied per process.

## State locations

- Config: `%APPDATA%\prostones\ProcessJobHacker\config\config.json` (a parse failure renames it to `config.json.bad` and starts with defaults)
- Active-job journal: `%LOCALAPPDATA%\prostones\ProcessJobHacker\data\active_jobs.json`
- Logs: `%LOCALAPPDATA%\prostones\ProcessJobHacker\data\logs\` (level is hardcoded to `info`; `RUST_LOG` has no effect)

## Adding features

- New Win32 API: add it under `src/win/` and enable the matching `windows-sys` feature in `Cargo.toml`; only the modules listed there are enabled.
- New background data: add a `MonitorEvent` variant and consume it in `drain_events` / `on_snapshot`; never block the UI thread with `sysinfo`.
- New persisted app state: add fields to `AppConfig` (it already has struct-level `#[serde(default)]`); both loaders fall back to defaults on parse failures or a newer-version mismatch.
