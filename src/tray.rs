use crossbeam_channel::Sender;
use eframe::egui;
use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::app::AppCommand;

pub struct TrayState {
    tray: TrayIcon,
    pause_item: MenuItem,
    resume_item: MenuItem,
    clear_item: MenuItem,
}

impl TrayState {
    pub fn init(ctx: egui::Context, command_tx: Sender<AppCommand>) -> anyhow::Result<Self> {
        let menu = Menu::new();
        let show_item = MenuItem::with_id("show", "显示主窗口", true, None);
        let pause_item = MenuItem::with_id("pause", "暂停全部策略", true, None);
        let resume_item = MenuItem::with_id("resume", "恢复全部策略", true, None);
        let clear_item = MenuItem::with_id("clear", "解除全部限制", true, None);
        let quit_item = MenuItem::with_id("quit", "退出（清除限制）", true, None);
        let separator = PredefinedMenuItem::separator();

        menu.append(&show_item)?;
        menu.append(&separator)?;
        menu.append(&pause_item)?;
        menu.append(&resume_item)?;
        menu.append(&clear_item)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&quit_item)?;

        let show_id = show_item.id().clone();
        let pause_id = pause_item.id().clone();
        let resume_id = resume_item.id().clone();
        let clear_id = clear_item.id().clone();
        let quit_id = quit_item.id().clone();
        let menu_ctx = ctx.clone();
        let menu_tx = command_tx.clone();
        let click_tx = command_tx;
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let command = if event.id() == &show_id {
                Some(AppCommand::ShowWindow)
            } else if event.id() == &pause_id {
                Some(AppCommand::PauseAll)
            } else if event.id() == &resume_id {
                Some(AppCommand::ResumeAll)
            } else if event.id() == &clear_id {
                Some(AppCommand::ClearAllLimits)
            } else if event.id() == &quit_id {
                Some(AppCommand::Quit)
            } else {
                None
            };
            if let Some(command) = command {
                let _ = menu_tx.send(command);
                menu_ctx.request_repaint();
            }
        }));

        let click_ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = click_tx.send(AppCommand::ToggleWindow);
                click_ctx.request_repaint();
            }
        }));

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Process Job Hacker")
            .with_icon(crate::icon::tray_icon()?)
            .with_menu_on_left_click(false)
            .build()?;

        Ok(Self {
            tray,
            pause_item,
            resume_item,
            clear_item,
        })
    }

    pub fn update_tooltip(&self, active_policies: usize) {
        let text = format!("Process Job Hacker — 生效策略 {active_policies} 条");
        let _ = self.tray.set_tooltip(Some(&text));
    }

    pub fn set_paused(&self, paused: bool) {
        self.pause_item.set_enabled(!paused);
        self.resume_item.set_enabled(paused);
    }

    pub fn set_has_rules(&self, has_rules: bool) {
        self.clear_item.set_enabled(has_rules);
    }
}
