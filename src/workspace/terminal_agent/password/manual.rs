//! Handles one masked password entry without putting it in an application message.

use std::sync::{Arc, Mutex};

use kortty_ssh::SensitiveLine;
#[cfg(not(target_os = "windows"))]
use kortty_vault::Secret as NativePassword;
#[cfg(target_os = "windows")]
use kortty_windowing::NativePassword;

#[cfg(target_os = "windows")]
use super::Message;
use super::{Authorization, Desktop, RedactionSecrets, Run, Task, Workspace, window};

#[cfg(not(target_os = "windows"))]
mod masked;

pub(super) struct Prompt {
    #[cfg(target_os = "windows")]
    life: Arc<()>,
    #[cfg(not(target_os = "windows"))]
    field: iced::widget::Id,
    result: Arc<Mutex<Option<NativePassword>>>,
}

impl Prompt {
    fn new() -> Self {
        Self {
            #[cfg(target_os = "windows")]
            life: Arc::new(()),
            #[cfg(not(target_os = "windows"))]
            field: iced::widget::Id::unique(),
            result: Arc::new(Mutex::new(None)),
        }
    }

    fn take(self) -> Option<NativePassword> {
        self.result
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }
}

impl Workspace {
    fn focused_sudo_surface(&self, id: window::Id) -> Option<window::Id> {
        let hosted_owner = self.hosted_tool_owner(id);
        let surface = hosted_owner.unwrap_or(id);
        (self.native_focused_window == Some(surface)
            && hosted_owner.is_none_or(|owner| {
                self.active_hosted_tab(owner)
                    .is_some_and(|tool| tool.instance == id)
            }))
        .then_some(surface)
    }

    pub(super) fn open_manual_sudo_prompt(
        &self,
        id: window::Id,
        run: &mut Run,
    ) -> Result<Task<Desktop>, String> {
        let pending = run
            .password
            .as_ref()
            .ok_or("The sudo password request expired.")?;
        self.revalidate_password_sudo(run, pending)?;
        let (owner, _) = self.current_run_pane(run)?;
        let Some(surface) = self.focused_sudo_surface(id) else {
            return Ok(Task::none());
        };
        if pending.manual_open() || self.owner_dialog(owner).is_some() {
            return Ok(Task::none());
        }
        let pending = run
            .password
            .as_mut()
            .ok_or("The sudo password request expired.")?;
        let prompt = Prompt::new();
        let task = prompt.open(
            surface,
            id,
            run.epoch,
            &pending.scope.destination,
            &run.cancel,
        );
        pending.manual = Some(prompt);
        Ok(task)
    }

    pub(super) fn submit_manual_sudo_line(&self, run: &mut Run) -> Result<(), String> {
        let pending = run
            .password
            .as_ref()
            .ok_or("The sudo password request expired.")?;
        self.revalidate_password_sudo(run, pending)?;
        let (owner, _) = self.current_run_pane(run)?;
        if !matches!(pending.authorization, Some(Authorization::Manual))
            || self.owner_dialog(owner).is_some()
        {
            return Err("The password request no longer matches the review.".into());
        }
        let secret = run
            .password
            .as_mut()
            .and_then(|pending| pending.manual.take())
            .and_then(Prompt::take)
            .ok_or("The sudo password prompt was cancelled.")?;
        let line = SensitiveLine::new(secret.as_str())
            .map_err(|_| "The entered sudo password is invalid.")?;
        let overlay = RedactionSecrets::with_secret(run.learned_sudo.as_ref(), secret.as_str())
            .map_err(|_| "The reviewed sudo password could not be safely masked.")?;
        Self::send_sudo_line(run, line, overlay)
    }
}

impl Prompt {
    #[cfg(target_os = "windows")]
    fn open(
        &self,
        surface: window::Id,
        id: window::Id,
        epoch: u64,
        destination: &str,
        cancel: &super::engine::Cancel,
    ) -> Task<Desktop> {
        let life = Arc::downgrade(&self.life);
        let callback_life = life.clone();
        let result = Arc::downgrade(&self.result);
        let cancel = cancel.clone();
        let destination = destination.to_owned();
        let (signal, ready) = async_channel::bounded(1);
        let show = window::run(surface, move |native| {
            let callback = signal.clone();
            let opened = native.window_handle().ok().is_some_and(|owner| {
                kortty_windowing::open_password_prompt(
                    owner,
                    &destination,
                    move || life.strong_count() != 0 && !cancel.requested(),
                    move |password| {
                        let Some(result) = result.upgrade() else {
                            return;
                        };
                        let mut slot = result.lock().unwrap_or_else(|error| error.into_inner());
                        *slot = password.filter(|_| callback_life.strong_count() != 0);
                        if callback.try_send(slot.is_some()).is_err() {
                            *slot = None;
                        }
                    },
                )
            });
            if !opened {
                let _ = signal.try_send(false);
            }
        })
        .discard();
        let completed = Task::perform(
            async move { ready.recv().await.unwrap_or(false) },
            move |submitted| {
                Desktop::TerminalAgent(
                    id,
                    super::super::Message::Password(Message::ManualReady(epoch, submitted)),
                )
            },
        );
        Task::batch([show, completed])
    }

    #[cfg(not(target_os = "windows"))]
    fn open(
        &self,
        _: window::Id,
        _: window::Id,
        _: u64,
        _: &str,
        _: &super::engine::Cancel,
    ) -> Task<Desktop> {
        iced::widget::operation::focus(self.field.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::tool_tabs::Kind;

    #[test]
    fn manual_sudo_uses_only_the_visible_hosted_agent_surface() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let first = window::Id::unique();
        let second = window::Id::unique();
        workspace.native_focused_window = Some(owner);
        drop(workspace.host_tool(owner, Kind::TerminalAgent, first));
        assert_eq!(workspace.focused_sudo_surface(first), Some(owner));
        drop(workspace.host_tool(owner, Kind::TerminalAgent, second));
        assert_eq!(workspace.focused_sudo_surface(first), None);
        assert_eq!(workspace.focused_sudo_surface(second), Some(owner));
        workspace.native_focused_window = Some(window::Id::unique());
        assert_eq!(workspace.focused_sudo_surface(second), None);
    }
}
