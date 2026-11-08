//! Binds a reviewed transfer to the terminal pane that supplied its directory.

use iced::futures::{StreamExt as _, stream};
use iced::{Task, window};

use crate::session::{SessionId, Status};
use crate::session_security::IsolationLevel;
use crate::snippet_transfer::{Choice, Event, Request, Source, View, Worker};
use crate::snippet_window::transfer::Message as Action;

use super::{Message, Placement, Workspace};

const MAX_TRANSFERS: usize = 4;

pub(in crate::workspace) struct State {
    pub(super) view: View,
    active: Option<Active>,
}

impl State {
    pub(super) fn active(&self) -> bool {
        self.active.is_some()
    }

    pub(in crate::workspace) fn cancel(&self) {
        if let Some(active) = &self.active {
            active.worker.cancel();
        }
    }
}

struct Active {
    token: u64,
    target: Target,
    label: String,
    remote: Option<Remote>,
    worker: Worker,
}

struct Remote {
    label: String,
    cwd: Option<Vec<u8>>,
}

#[derive(Clone, Copy)]
struct Target {
    owner: window::Id,
    tab: u64,
    session: SessionId,
}

impl Workspace {
    fn snippet_transfer_owner(&self, id: window::Id) -> Option<window::Id> {
        match self.snippets.get(&id)?.placement {
            Placement::Native { source } => Some(source),
            Placement::Hosted => self.hosted_tool_owner(id),
        }
    }

    fn snippet_transfer_target(&self, id: window::Id) -> Option<Target> {
        let owner = self.snippet_transfer_owner(id)?;
        let window = self.windows.get(&owner)?;
        let mut terminals = window.terminal_tabs();
        let tab = window
            .tab()
            .or_else(|| {
                window
                    .tab_mru
                    .iter()
                    .find_map(|id| window.terminal_tabs().find(|tab| tab.id == *id))
            })
            .or_else(|| {
                let only = terminals.next()?;
                terminals.next().is_none().then_some(only)
            })?;
        Some(Target {
            owner,
            tab: tab.id,
            session: tab.focused_session()?,
        })
    }

    fn check_snippet_transfer(
        &self,
        id: window::Id,
        target: Target,
    ) -> Result<&crate::workspace::PaneSession, &'static str> {
        if self.exiting || self.scheduler_draining || self.clipboard.policy.problem.is_some() {
            return Err("File copying is unavailable under the current application policy.");
        }
        if self.snippet_transfer_owner(id) != Some(target.owner)
            || !self.windows.get(&target.owner).is_some_and(|window| {
                window.terminal_tabs().any(|tab| {
                    tab.id == target.tab && tab.focused_session() == Some(target.session)
                })
            })
        {
            return Err("The selected terminal pane moved, closed, or changed focus.");
        }
        let pane = self
            .sessions
            .get(&target.session)
            .ok_or("The selected terminal pane closed.")?;
        let frame = pane.session.frame();
        if frame.status != Status::Running || frame.password_prompt || frame.screen.alternate_screen
        {
            return Err("The selected terminal pane is disconnected or busy with input.");
        }
        if pane.ssh.is_some() {
            if !self.clipboard.policy.file_transfer_allowed {
                return Err("Copying files to SSH sessions is disabled by policy.");
            }
            if self.remote_pane(target.session).is_none() {
                return Err("The selected SSH connection cannot open SFTP.");
            }
        } else if !cfg!(unix)
            || pane.session.local_shell_pid().is_none()
            || pane.session.sandbox_backend().is_some()
            || self.clipboard.policy.minimum_isolation == IsolationLevel::Sandbox
        {
            return Err("A foreground local shell directory is unavailable for this pane.");
        }
        Ok(pane)
    }

    pub(super) fn snippet_transfer_enabled(&self, id: window::Id) -> bool {
        self.snippet_transfer_target(id)
            .is_some_and(|target| self.check_snippet_transfer(id, target).is_ok())
    }

    pub(in crate::workspace) fn snippet_transfer_owns_sftp(&self, session: SessionId) -> bool {
        self.snippets.values().any(|surface| {
            surface
                .transfer
                .as_ref()
                .and_then(|state| state.active.as_ref())
                .is_some_and(|active| active.target.session == session && active.remote.is_some())
        })
    }

    pub(in crate::workspace) fn cancel_snippet_transfers_for_session(&self, session: SessionId) {
        for active in self
            .snippets
            .values()
            .filter_map(|surface| surface.transfer.as_ref()?.active.as_ref())
            .filter(|active| active.target.session == session)
        {
            active.worker.cancel();
        }
    }

    pub(super) fn update_snippet_transfer(
        &mut self,
        id: window::Id,
        action: Action,
    ) -> Task<Message> {
        if self.hosted_close_locks(id) {
            return Task::none();
        }
        match action {
            Action::Folder(_) | Action::Snippets(_) => self.begin_snippet_transfer(id, action),
            Action::Confirm(choice) => self.confirm_snippet_transfer(id, choice),
            Action::Cancel | Action::Close => {
                let Some(surface) = self.snippets.get_mut(&id) else {
                    return Task::none();
                };
                if let Some(state) = &mut surface.transfer {
                    if state.active() {
                        state.cancel();
                        if let View::Copying { cancelling, .. } = &mut state.view {
                            *cancelling = true;
                        }
                    } else {
                        surface.transfer = None;
                    }
                }
                Task::none()
            }
        }
    }

    fn begin_snippet_transfer(&mut self, id: window::Id, action: Action) -> Task<Message> {
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        if surface.transfer.is_some() || !surface.window.prepare_transfer() {
            return Task::none();
        }
        match self.start_snippet_transfer(id, &action) {
            Ok(task) => task,
            Err(error) => {
                self.snippets
                    .get_mut(&id)
                    .expect("existing snippet surface")
                    .transfer = Some(State {
                    view: View::Failed { message: error },
                    active: None,
                });
                Task::none()
            }
        }
    }

    fn start_snippet_transfer(
        &mut self,
        id: window::Id,
        action: &Action,
    ) -> Result<Task<Message>, String> {
        let target = self
            .snippet_transfer_target(id)
            .ok_or("Open a terminal pane before copying snippets.")?;
        let pane = self.check_snippet_transfer(id, target)?;
        if self
            .snippets
            .values()
            .filter(|surface| surface.transfer.as_ref().is_some_and(State::active))
            .count()
            >= MAX_TRANSFERS
        {
            return Err("Wait for a current snippet transfer to finish.".into());
        }
        let (source, remote) = if let Some(ssh) = &pane.ssh {
            let (_, opener) = self
                .remote_pane(target.session)
                .ok_or("The selected SSH connection cannot open SFTP.")?;
            let cwd = self
                .remote_shell_file_context(target.session)
                .ok_or("The original remote shell could not be verified.")?;
            let remote = Remote {
                label: ssh.label(),
                cwd: cwd.clone(),
            };
            (Source::Remote(opener, cwd), Some(remote))
        } else {
            let reply = pane
                .session
                .local_shell_directory()
                .map_err(|_| "The shell directory could not be requested.")?;
            (Source::Local(reply), None)
        };
        let surface = &self.snippets[&id];
        let layout = surface
            .window
            .transfer_layout(action)
            .map_err(|error| error.to_string())?;
        let label = match action {
            Action::Folder(folder) => surface.window.transfer_folder_label(*folder),
            _ => match layout.entries.as_slice() {
                [entry] => entry.path.clone(),
                _ => "Selected snippets".into(),
            },
        };
        let released = if remote.is_some() {
            self.release_remote_dock_for_snippet_transfer(target.session)?
        } else {
            None
        };
        let token = self.allocate();
        let (worker, events, done) =
            Worker::start(source, layout, released).map_err(|error| error.to_string())?;
        self.snippets
            .get_mut(&id)
            .expect("existing snippet surface")
            .transfer = Some(State {
            view: View::Preparing,
            active: Some(Active {
                token,
                target,
                label,
                remote,
                worker,
            }),
        });
        self.track_session_shutdown(done);
        let events = events.chain(stream::once(async {
            Event::Finished(Err("The snippet transfer worker stopped.".into()))
        }));
        Ok(Task::run(events, move |event| {
            Message::SnippetTransfer(id, token, event)
        }))
    }

    fn confirm_snippet_transfer(&mut self, id: window::Id, choice: Choice) -> Task<Message> {
        let Some(state) = self
            .snippets
            .get(&id)
            .and_then(|surface| surface.transfer.as_ref())
        else {
            return Task::none();
        };
        let (
            Some(active),
            View::Confirm {
                target,
                files,
                conflicts,
                overwrite,
                ..
            },
        ) = (&state.active, &state.view)
        else {
            return Task::none();
        };
        if active.worker.is_cancelled()
            || !match choice {
                Choice::Copy => conflicts.is_empty(),
                Choice::Skip => !conflicts.is_empty(),
                Choice::Overwrite => !conflicts.is_empty() && *overwrite,
            }
        {
            return Task::none();
        }
        let target_path = target.clone();
        let total = *files;
        let directory = self.check_snippet_transfer(id, active.target).and_then(|pane| {
            if let Some(remote) = &active.remote {
                if self.remote_shell_file_context(active.target.session).as_ref() != Some(&remote.cwd) {
                    return Err("The remote shell or working directory changed. Review the destination again.");
                }
                Ok(None)
            } else {
                pane.session.local_shell_directory().map(Some)
                    .map_err(|_| "The shell directory could not be rechecked.")
            }
        });
        let state = self
            .snippets
            .get_mut(&id)
            .and_then(|surface| surface.transfer.as_mut())
            .expect("existing transfer state");
        let active = state.active.as_ref().expect("active transfer");
        let confirmed = directory.and_then(|directory| {
            active
                .worker
                .confirm(Request { choice, directory })
                .then_some(())
                .ok_or("The transfer is no longer available.")
        });
        match confirmed {
            Ok(()) => {
                state.view = View::Copying {
                    target: target_path,
                    done: 0,
                    total,
                    current: String::new(),
                    cancelling: false,
                }
            }
            Err(message) => {
                active.worker.cancel();
                state.view = View::Failed {
                    message: message.into(),
                };
            }
        }
        Task::none()
    }

    pub(in crate::workspace) fn snippet_transfer_event(
        &mut self,
        id: window::Id,
        token: u64,
        event: Event,
    ) -> Task<Message> {
        let closing = self.hosted_close_locks(id);
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(state) = surface.transfer.as_mut() else {
            return Task::none();
        };
        let Some(active) = state.active.as_ref().filter(|active| active.token == token) else {
            return Task::none();
        };
        match event {
            Event::Prepared {
                target,
                files,
                conflicts,
                overwrite,
            } => {
                if matches!(state.view, View::Preparing) && !active.worker.is_cancelled() {
                    let target = match &active.remote {
                        Some(remote) if remote.cwd.is_none() => {
                            format!("{} · SFTP start directory · {target}", remote.label)
                        }
                        Some(remote) => format!("{} · {target}", remote.label),
                        None => target,
                    };
                    state.view = View::Confirm {
                        label: active.label.clone(),
                        target,
                        files,
                        conflicts,
                        overwrite,
                    };
                }
            }
            Event::Progress(progress) => {
                if let View::Copying {
                    done,
                    total,
                    current,
                    ..
                } = &mut state.view
                {
                    *done = progress.done;
                    *total = progress.total;
                    *current = progress.path;
                }
            }
            Event::Finished(result) => {
                let target = active.target;
                let remote = active.remote.is_some();
                let reveal = result.as_ref().is_ok_and(|outcome| !outcome.cancelled)
                    && matches!(state.view, View::Copying { .. });
                state.active = None;
                let copy_path = match &state.view {
                    View::Copying { target, .. } => Some(target.clone()),
                    _ => None,
                };
                match (result, copy_path) {
                    (Ok(outcome), Some(target)) => {
                        state.view = View::Finished {
                            target,
                            written: outcome.written,
                            skipped: outcome.skipped,
                            cancelled: outcome.cancelled,
                        };
                    }
                    (Ok(_), None) if matches!(state.view, View::Failed { .. }) => {}
                    (Ok(_), None) => surface.transfer = None,
                    (Err(message), _) => state.view = View::Failed { message },
                }
                let after = if closing {
                    surface.transfer = None;
                    self.resume_hosted_tool_close(id)
                } else if surface.close_after_work {
                    self.request_snippet_close(id)
                } else if reveal && self.check_snippet_transfer(id, target).is_ok() {
                    self.select_tab(target.owner, target.tab, false);
                    window::gain_focus(target.owner)
                } else {
                    Task::none()
                };
                let dock = if remote {
                    self.sync_remote_dock(target.owner)
                } else {
                    Task::none()
                };
                return Task::batch([dock, after]);
            }
        }
        Task::none()
    }
}
