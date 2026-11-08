use iced::widget::{column, text};
use iced::{Element, Size, Task, window};

use crate::clipboard_policy::RemoteEditorPolicy;
use crate::editor_window::{self, EditorWindow};
use crate::session::SessionId;
use crate::sftp_window::{self, SftpWindow};
use crate::ssh_connection::Connected;
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message, SshTarget, Workspace, ssh_access};

impl Workspace {
    pub(super) fn reconnect_sftp_busy(&self, source: SessionId) -> bool {
        self.terminal_file_uses_sftp(source)
            || self.remote_dock_blocks_reconnect(source)
            || self
                .sftp
                .values()
                .filter(|state| state.attached_to() == Some(source))
                .any(|state| {
                    state.transport_busy()
                        || self
                            .editors
                            .values()
                            .filter_map(EditorWindow::remote_editor)
                            .any(|editor| state.owns_editor(editor))
                })
    }

    pub(super) fn open_sftp_connection(
        &mut self,
        source: window::Id,
        target: SshTarget,
        ssh: Connected,
    ) -> Task<Message> {
        self.sftp_logins.remove(&source);
        if let Some(window) = self.windows.get_mut(&source) {
            window.pending_ssh = None;
        }
        if let Some(state) = self.sftp.get_mut(&source) {
            return match state.reconnect(target, ssh) {
                Ok(task) => task.map(move |message| Message::Sftp(source, message)),
                Err(error) => {
                    state.set_status(format!("Could not reopen SFTP ({error})"));
                    Task::none()
                }
            };
        }
        let (state, started) = match SftpWindow::open(
            target,
            ssh,
            self.clipboard.policy.file_transfer_allowed,
            self.clipboard.policy.remote_editor,
        ) {
            Ok(opened) => opened,
            Err(error) => {
                self.notice(source, format!("Could not open SFTP ({error})"));
                return Task::none();
            }
        };
        self.show_sftp_window(state, started)
    }

    pub(super) fn open_attached_sftp(&mut self, source: window::Id) -> Option<Task<Message>> {
        let session = self.windows.get(&source)?.tab()?.focused_session()?;
        let path = if self.remote_manager_owns(session) {
            None
        } else {
            self.tracked_remote_directory(source, session)
        };
        self.open_attached_sftp_at(source, session, path)
    }

    pub(super) fn open_attached_sftp_at(
        &mut self,
        source: window::Id,
        session: SessionId,
        path: Option<Vec<u8>>,
    ) -> Option<Task<Message>> {
        self.windows.get(&source)?.pane_location(session)?;
        if self.snippet_transfer_owns_sftp(session) {
            self.notice(source, "Wait for the snippet transfer to finish.".into());
            return Some(Task::none());
        }
        if self.terminal_file_request_pending(session) {
            self.notice(source, "Wait for the terminal file to open.".into());
            return Some(Task::none());
        }
        let (pane, opener) = self.remote_pane(session)?;
        let target = pane.ssh.clone()?;
        if let Some((&id, state)) = self
            .sftp
            .iter_mut()
            .find(|(_, state)| state.attached_to() == Some(session))
        {
            let browse = path
                .map_or_else(Task::none, |path| state.browse_from_sidebar(path))
                .map(move |message| Message::Sftp(id, message));
            return Some(Task::batch([window::gain_focus(id), browse]));
        }
        if let Some(worker) = self.take_terminal_file_worker(session) {
            let (state, started) = SftpWindow::with_worker(
                target,
                Some(session),
                worker,
                self.clipboard.policy.file_transfer_allowed,
                self.clipboard.policy.remote_editor,
                path,
            );
            return Some(self.show_sftp_window(state, started));
        }
        if let Some(task) = self.lend_remote_dock_to_manager(source, session, path.clone()) {
            return Some(task);
        }
        if let Some(task) = self.wait_for_remote_browser(source, session, path.clone()) {
            return Some(task);
        }
        if self.remote_manager_closing.contains_key(&session) {
            self.notice(source, "The previous SFTP channel is still closing.".into());
            return Some(Task::none());
        }
        let task = match SftpWindow::open_attached_at(
            target,
            session,
            opener,
            self.clipboard.policy.file_transfer_allowed,
            self.clipboard.policy.remote_editor,
            path,
        ) {
            Ok((state, started)) => self.show_sftp_window(state, started),
            Err(error) => {
                self.notice(
                    source,
                    format!("Could not open SFTP on this terminal ({error})"),
                );
                Task::none()
            }
        };
        Some(task)
    }

    fn show_sftp_window(
        &mut self,
        state: SftpWindow,
        started: Task<sftp_window::Message>,
    ) -> Task<Message> {
        let (id, opened) = self.open_placed_dialog(
            DialogKind::Sftp,
            window::Settings {
                size: Size::new(1140.0, 710.0),
                min_size: Some(Size::new(700.0, 380.0)),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.sftp.insert(id, state);
        Task::batch([
            opened.map(|_| Message::Noop),
            started.map(move |message| Message::Sftp(id, message)),
        ])
    }

    pub(super) fn update_sftp(
        &mut self,
        id: window::Id,
        message: sftp_window::Message,
    ) -> Task<Message> {
        match message {
            sftp_window::Message::ShowEditor(document, editor) => {
                if self.clipboard.policy.problem.is_some()
                    || !self.clipboard.policy.file_transfer_allowed
                    || self.clipboard.policy.remote_editor == RemoteEditorPolicy::Deny
                {
                    return Task::none();
                }
                let Some(state) = self
                    .sftp
                    .get(&id)
                    .filter(|state| state.owns_editor(&editor))
                else {
                    return Task::none();
                };
                let path = sftp_window::Location::Remote(document.path().to_vec()).display();
                let label = format!("{} · {path}", state.target().label());
                let (editor, loaded) = EditorWindow::with_remote(document, editor, label);
                return self.show_native_editor(editor, loaded);
            }
            sftp_window::Message::Reconnect => {
                return self.update_ssh(ssh_access::Message::ShowSftp(id));
            }
            sftp_window::Message::Close => return self.close_window(id),
            sftp_window::Message::ConfirmClose(true) => {
                if self.sftp.get(&id).is_some_and(SftpWindow::close_confirmed) {
                    let quit = self.quit_sftp_waiting.take() == Some(id);
                    let close = self.finish_sftp_close(id);
                    if quit && !self.should_exit_after_window_close() {
                        return Task::batch([close, self.quit()]);
                    }
                    return close;
                }
                return Task::none();
            }
            sftp_window::Message::ConfirmClose(false) | sftp_window::Message::Dismiss
                if self.quit_sftp_waiting == Some(id) =>
            {
                self.quit_sftp_waiting = None;
                super::window_lifecycle::reply_to_native_quit(false);
            }
            _ => {}
        }
        self.sftp.get_mut(&id).map_or_else(Task::none, |state| {
            state
                .update(message)
                .map(move |message| Message::Sftp(id, message))
        })
    }

    pub(super) fn show_native_editor(
        &mut self,
        editor: EditorWindow,
        loaded: Task<editor_window::Message>,
    ) -> Task<Message> {
        self.insert_native_editor(editor, loaded).1
    }

    pub(super) fn insert_native_editor(
        &mut self,
        editor: EditorWindow,
        loaded: Task<editor_window::Message>,
    ) -> (window::Id, Task<Message>) {
        let (id, opened) = self.open_placed_dialog(
            DialogKind::Editor,
            window::Settings {
                size: Size::new(1050.0, 660.0),
                min_size: Some(Size::new(600.0, 320.0)),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.editors.insert(
            id,
            editor.with_initial_font_size(self.window_settings.terminal_font),
        );
        let task = opened
            .map(|_| Message::Noop)
            .chain(loaded.map(move |message| Message::Editor(id, message)));
        (id, task)
    }

    pub(super) fn finish_sftp_close(&mut self, id: window::Id) -> Task<Message> {
        self.sftp_logins.remove(&id);
        let mut returned = Task::none();
        if let Some(mut state) = self.sftp.remove(&id) {
            let session = state.attached_to();
            let busy = state.transport_busy();
            let worker = state.take_worker().and_then(|worker| {
                if let Some(session) = session.filter(|_| !busy) {
                    self.retain_terminal_file_worker(session, worker)
                } else {
                    Some(worker)
                }
            });
            if let Some(worker) = worker {
                let done = worker.stop();
                if let Some(session) = session {
                    returned = self.remote_manager_releasing(id, session, done.clone());
                }
                self.track_session_shutdown(done);
            } else if session.is_some() {
                returned = self.sync_remote_docks();
            }
        }
        let closed = Task::batch([self.close_placed_dialog(id), returned]);
        if self.should_exit_after_window_close() {
            Task::batch([closed, self.quit()])
        } else {
            closed
        }
    }

    pub(super) fn view_sftp<'a>(
        &'a self,
        id: window::Id,
        state: &'a SftpWindow,
    ) -> Element<'a, Message> {
        if let Some(pending) = self.sftp_logins.get(&id) {
            let mut content = column![self.ssh_dialog_view(id, pending)];
            if let Some(status) = state.status() {
                content = content.push(text(status).size(13));
            }
            content.into()
        } else {
            state.view().map(move |message| Message::Sftp(id, message))
        }
    }
}
