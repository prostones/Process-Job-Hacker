use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use eframe::egui;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::model::cpu::CpuHistory;
use crate::model::process::{PidKey, ProcessRecord, ProcessSnapshot};

#[derive(Clone, Debug)]
pub enum MonitorEvent {
    Snapshot(Arc<ProcessSnapshot>),
    Cpu(Arc<CpuHistory>),
    Error(String),
}

enum MonitorCommand {
    SetHidden(bool),
    SetPaused(bool),
    RefreshNow,
    Stop,
}

pub struct ProcessMonitor {
    command_tx: Sender<MonitorCommand>,
    event_rx: Receiver<MonitorEvent>,
    join: Option<thread::JoinHandle<()>>,
}

impl ProcessMonitor {
    pub fn spawn(ctx: egui::Context) -> Self {
        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let join = thread::Builder::new()
            .name("pjh-monitor".to_owned())
            .spawn(move || monitor_loop(ctx, command_rx, event_tx))
            .ok();
        Self {
            command_tx,
            event_rx,
            join,
        }
    }

    pub fn set_hidden(&self, hidden: bool) {
        let _ = self.command_tx.send(MonitorCommand::SetHidden(hidden));
    }

    pub fn set_paused(&self, paused: bool) {
        let _ = self.command_tx.send(MonitorCommand::SetPaused(paused));
    }

    pub fn refresh_now(&self) {
        let _ = self.command_tx.send(MonitorCommand::RefreshNow);
    }

    pub fn try_recv(&self) -> Option<MonitorEvent> {
        match self.event_rx.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }
}

impl Drop for ProcessMonitor {
    fn drop(&mut self) {
        let _ = self.command_tx.send(MonitorCommand::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn monitor_loop(
    ctx: egui::Context,
    command_rx: Receiver<MonitorCommand>,
    event_tx: Sender<MonitorEvent>,
) {
    let mut system = match System::new() {
        Ok(system) => system,
        Err(error) => {
            let _ = event_tx.send(MonitorEvent::Error(error.to_string()));
            ctx.request_repaint();
            return;
        }
    };
    system.refresh_cpu_usage();
    thread::sleep(Duration::from_millis(350));
    system.refresh_cpu_usage();

    let brand = system
        .cpus()
        .first()
        .map(|cpu| cpu.brand().to_string())
        .unwrap_or_else(|| "Unknown CPU".to_owned());
    let mut cpu_history = CpuHistory::new(60, brand, system.cpus().len());
    let mut hidden = false;
    let mut paused = false;
    let mut tick: u64 = 0;
    let process_kind = ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .with_exe(UpdateKind::OnlyIfNotSet);

    loop {
        let mut should_refresh_processes = true;
        match command_rx.recv_timeout(Duration::from_millis(1000)) {
            Ok(MonitorCommand::Stop) => break,
            Ok(MonitorCommand::SetHidden(value)) => {
                hidden = value;
                should_refresh_processes = false;
            }
            Ok(MonitorCommand::SetPaused(value)) => {
                paused = value;
                should_refresh_processes = false;
            }
            Ok(MonitorCommand::RefreshNow) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }

        system.refresh_cpu_usage();
        let cores: Vec<f32> = system.cpus().iter().map(|cpu| cpu.usage()).collect();
        cpu_history.push(system.global_cpu_usage(), &cores);
        if event_tx
            .send(MonitorEvent::Cpu(Arc::new(cpu_history.clone())))
            .is_err()
        {
            break;
        }
        ctx.request_repaint();

        let process_due =
            !paused && should_refresh_processes && (!hidden || tick.is_multiple_of(2));
        if process_due {
            let updated =
                system.refresh_processes_specifics(ProcessesToUpdate::All, true, process_kind);
            if updated == 0 {
                log::debug!("process refresh returned no processes");
            }
            let snapshot = build_snapshot(&system);
            if event_tx
                .send(MonitorEvent::Snapshot(Arc::new(snapshot)))
                .is_err()
            {
                break;
            }
            ctx.request_repaint();
        }

        tick = tick.wrapping_add(1);
    }
}

fn build_snapshot(system: &System) -> ProcessSnapshot {
    let mut records = Vec::with_capacity(system.processes().len());
    for (pid, process) in system.processes() {
        let start_time = process.start_time();
        let path = process
            .exe()
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| path.to_path_buf());
        records.push(ProcessRecord {
            key: PidKey::new(pid.as_u32(), start_time),
            parent_pid: process.parent().map(|parent| parent.as_u32()).unwrap_or(0),
            name: process.name().to_string_lossy().into_owned(),
            cpu_percent: process.cpu_usage(),
            memory: process.memory(),
            commit: process.virtual_memory(),
            path,
        });
    }
    ProcessSnapshot::new(records)
}
