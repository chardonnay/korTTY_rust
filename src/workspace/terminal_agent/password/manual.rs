//! Routes a native prompt result without putting a password in an Iced message.

use std::sync::{Arc, Mutex};

use kortty_ssh::SensitiveLine;
use kortty_windowing::NativePassword;

use super::{Authorization, Desktop, Message, RedactionSecrets, Run, Task, Workspace, window};

pub(super) struct Prompt {
    life: Arc<()>,
    result: Arc<Mutex<Option<NativePassword>>>,
}

impl Prompt {
    fn new() -> Self {
        Self {
            life: Arc::new(()),
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
        if pending.manual_open()
            || self.native_focused_window != Some(id)
            || self.owner_dialog(owner).is_some()
        {
            return Ok(Task::none());
        }
        let pending = run
            .password
            .as_mut()
            .ok_or("The sudo password request expired.")?;
        let prompt = Prompt::new();
        let life = Arc::downgrade(&prompt.life);
        let callback_life = life.clone();
        let result = Arc::downgrade(&prompt.result);
        let cancel = run.cancel.clone();
        let destination = pending.scope.destination.clone();
        pending.manual = Some(prompt);
        let epoch = run.epoch;
        let (signal, ready) = async_channel::bounded(1);
        let show = window::run(id, move |native| {
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
        Ok(Task::batch([show, completed]))
    }

    pub(super) fn submit_manual_sudo_line(&self, run: &mut Run) -> Result<(), String> {
        let pending = run
            .password
            .as_ref()
            .ok_or("The sudo password request expired.")?;
        self.revalidate_password_sudo(run, pending)?;
        let (owner, _) = self.current_run_pane(run)?;
        if !matches!(pending.authorization, Authorization::Manual)
            || self.owner_dialog(owner).is_some()
        {
            return Err("The native password request no longer matches the review.".into());
        }
        let secret = run
            .password
            .as_mut()
            .and_then(|pending| pending.manual.take())
            .and_then(Prompt::take)
            .ok_or("The native sudo password prompt was cancelled.")?;
        let line = SensitiveLine::new(secret.as_str())
            .map_err(|_| "The native sudo password is invalid.")?;
        let overlay = RedactionSecrets::with_secret(run.learned_sudo.as_ref(), secret.as_str())
            .map_err(|_| "The reviewed sudo password could not be safely masked.")?;
        Self::send_sudo_line(run, line, overlay)
    }
}
