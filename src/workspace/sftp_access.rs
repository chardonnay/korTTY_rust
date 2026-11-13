use iced::widget::{column, text};
use iced::{Element, Size, Task, window};
use std::time::Instant;

use crate::clipboard_policy::{ClipboardPolicy, RemoteEditorPolicy};
use crate::editor_window::{self, EditorWindow};
use crate::session::SessionId;
use crate::sftp_window::{self, ConflictDefault, SftpWindow, comparison_ai};
use crate::ssh_connection::Connected;
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message, SshTarget, TabEntry, Workspace, ssh_access};

#[derive(Clone, Copy)]
pub(super) struct SftpConflictPolicy {
    pub(super) transfer_allowed: bool,
    pub(super) parallel_cap: u8,
    pub(super) managed: Option<ConflictDefault>,
}

impl SftpConflictPolicy {
    pub(super) fn load() -> Self {
        Self::from_policy(&ClipboardPolicy::load())
    }

    pub(super) fn from_policy(policy: &ClipboardPolicy) -> Self {
        Self {
            transfer_allowed: policy.problem.is_none() && policy.file_transfer_allowed,
            parallel_cap: sftp_window::parallel::managed_cap(policy.sftp_max_parallel),
            managed: policy.sftp_conflict_default,
        }
    }

    pub(super) fn restrict(self, policy: &ClipboardPolicy) -> Self {
        let other = Self::from_policy(policy);
        Self {
            transfer_allowed: self.transfer_allowed && other.transfer_allowed,
            parallel_cap: self.parallel_cap.min(other.parallel_cap),
            managed: match (self.managed, other.managed) {
                (Some(installed), Some(cached)) => Some(installed.most_restrictive(cached)),
                (installed, cached) => installed.or(cached),
            },
        }
    }

    pub(super) fn editable(self) -> bool {
        self.transfer_allowed && self.managed.is_none()
    }

    pub(super) fn configure(self, state: &mut SftpWindow, preference: ConflictDefault) {
        state.configure_conflict_default(preference, self.managed);
    }
}

pub(super) fn configure_sftp_preferences(
    state: &mut SftpWindow,
    settings: &super::tab_titles::WindowSettings,
    policy: &ClipboardPolicy,
    project_idle: Option<u16>,
) {
    state.revalidate_file_transfers(policy.problem.is_none() && policy.file_transfer_allowed);
    state.revalidate_remote_images(
        policy.problem.is_none()
            && policy.remote_editor != RemoteEditorPolicy::Deny
            && state.target().workflow_route_allowed(policy),
    );
    state.configure_parallel(settings.sftp_parallel_transfers, policy.sftp_max_parallel);
    state.configure_resume_preferences(sftp_window::ResumePreferences {
        enabled: settings.sftp_resume_partial_transfers,
        keep_on_cancel: settings.sftp_keep_partial_on_cancel,
    });
    state.configure_idle_timeout(settings.sftp_idle_minutes, project_idle);
    state.configure_archive_preferences(&settings.sftp_archive);
}

fn sftp_project_shareable(
    incognito: bool,
    target: &SshTarget,
    saved: Option<&crate::connection_store::Connection>,
) -> bool {
    !incognito
        && target.saved_id().is_none_or(|id| {
            saved.is_some_and(|saved| saved.id == id && !saved.incognito && target.matches(saved))
        })
}

pub(super) fn configure_privileged_editor(
    state: &mut SftpWindow,
    policy: &ClipboardPolicy,
    vault: Option<&crate::vault_window::VaultWindow>,
) {
    let allowed = !state.incognito() && state.target().sftp_sudo_allowed(policy);
    let credential = vault.filter(|_| allowed).and_then(|vault| {
        vault
            .agent_sudo_credential(state.target().sftp_sudo_login())
            .ok()
    });
    state.set_privileged_editor_policy(allowed, credential);
}

impl Workspace {
    pub(super) fn sftp_resume_preferences(&self) -> sftp_window::ResumePreferences {
        sftp_window::ResumePreferences {
            enabled: self.window_settings.sftp_resume_partial_transfers,
            keep_on_cancel: self.window_settings.sftp_keep_partial_on_cancel,
        }
    }

    pub(super) fn refresh_sftp_preferences(&mut self) {
        if !self.window_settings.sftp_resume_partial_transfers {
            self.retained_sftp_download = None;
        }
        for (&id, state) in &mut self.sftp {
            let project_idle = self
                .project_files
                .get(&id)
                .and_then(|tab| tab.remote_source())
                .and_then(|(_, tool)| match tool {
                    crate::profile::RemoteProjectTool::Sftp { idle_minutes, .. } => *idle_minutes,
                    _ => None,
                });
            configure_sftp_preferences(
                state,
                &self.window_settings,
                &self.clipboard.policy,
                project_idle,
            );
        }
        self.refresh_sidebar_resume_preferences();
    }

    pub(super) fn refresh_sftp_conflict_defaults(&mut self, policy: SftpConflictPolicy) {
        for state in self.sftp.values_mut() {
            policy.configure(state, self.window_settings.sftp_conflict_default);
        }
    }

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
        incognito: bool,
    ) -> Task<Message> {
        self.sftp_logins.remove(&source);
        if let Some(window) = self.windows.get_mut(&source) {
            window.pending_ssh = None;
        }
        if let Some(state) = self.sftp.get_mut(&source) {
            if state.incognito() != incognito && state.external_edit_active() {
                state.set_status(
                    "Stop and clear the external edit before changing privacy mode.".into(),
                );
                return Task::none();
            }
            state.set_incognito(incognito);
            configure_sftp_preferences(state, &self.window_settings, &self.clipboard.policy, None);
            SftpConflictPolicy::load()
                .restrict(&self.clipboard.policy)
                .configure(state, self.window_settings.sftp_conflict_default);
            state.note_activity(Instant::now());
            return match state.reconnect(target, ssh) {
                Ok(task) => {
                    state.configure_saved_downloads(crate::profile::root().ok());
                    state.configure_external_editor(crate::profile::root().ok());
                    state.restore_partial(&mut self.retained_sftp_download);
                    task.map(move |message| Message::Sftp(source, message))
                }
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
        self.show_sftp_window(Some(source), state, started, incognito)
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
        let owner = self.windows.get(&source)?;
        let (tab, _) = owner.pane_location(session)?;
        let incognito = owner.tabs.get(tab)?.terminal()?.incognito;
        if self.snippet_transfer_owns_sftp(session) {
            self.notice(source, "Wait for the snippet transfer to finish.".into());
            return Some(Task::none());
        }
        if self.terminal_file_request_pending(session) {
            self.notice(source, "Wait for the terminal file to open.".into());
            return Some(Task::none());
        }
        let blocked = self
            .sessions
            .get(&session)
            .and_then(|pane| pane.ssh.as_ref())
            .and_then(|target| target.blocked_server(self));
        if let Some(notice) = blocked {
            self.notice(source, notice);
            return Some(Task::none());
        }
        let (pane, opener) = self.remote_pane(session)?;
        let target = pane.ssh.clone()?;
        if let Some((&id, state)) = self
            .sftp
            .iter_mut()
            .find(|(_, state)| state.attached_to() == Some(session))
        {
            SftpConflictPolicy::load()
                .restrict(&self.clipboard.policy)
                .configure(state, self.window_settings.sftp_conflict_default);
            state.note_activity(Instant::now());
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
            return Some(self.show_sftp_window(None, state, started, incognito));
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
            Ok((state, started)) => self.show_sftp_window(None, state, started, incognito),
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
        owner: Option<window::Id>,
        mut state: SftpWindow,
        started: Task<sftp_window::Message>,
        incognito: bool,
    ) -> Task<Message> {
        state.set_incognito(incognito);
        configure_sftp_preferences(
            &mut state,
            &self.window_settings,
            &self.clipboard.policy,
            None,
        );
        SftpConflictPolicy::load()
            .restrict(&self.clipboard.policy)
            .configure(&mut state, self.window_settings.sftp_conflict_default);
        state.configure_saved_downloads(crate::profile::root().ok());
        state.configure_external_editor(crate::profile::root().ok());
        state.set_external_editor_policy(
            (self.clipboard.policy.problem.is_none()
                && self.clipboard.policy.file_transfer_allowed)
                .then_some(self.clipboard.policy.remote_editor),
        );
        configure_privileged_editor(&mut state, &self.clipboard.policy, self.vault.as_ref());
        state.restore_partial(&mut self.retained_sftp_download);
        if let Some(owner) = owner.filter(|owner| self.windows.contains_key(owner))
            && self.window_settings.open_tool_windows_as_tabs
            && self.clipboard.policy.problem.is_none()
            && sftp_project_shareable(
                incognito,
                state.target(),
                state
                    .target()
                    .saved_id()
                    .and_then(|id| self.saved_connection(id))
                    .as_ref(),
            )
        {
            return self.host_project_sftp(owner, state, started);
        }
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
        #[cfg(target_os = "linux")]
        if let sftp_window::Message::ExternalFileDrag(request) = message {
            return self.start_linux_file_drag(id, request);
        }
        #[cfg(target_os = "windows")]
        if let sftp_window::Message::ExternalFileDrag(request) = message {
            return self.start_windows_sftp_drag(id, request);
        }
        self.note_project_sftp_activity(id);
        let pointer_transfer = match &message {
            sftp_window::Message::DropTransfer(_) => Some(true),
            sftp_window::Message::DesktopDrop(drop) => Some(drop.is_remote()),
            #[cfg(target_os = "linux")]
            sftp_window::Message::WaylandDesktopDrop(drop) => Some(drop.is_remote()),
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            sftp_window::Message::PrepareExternalFileDrag
            | sftp_window::Message::ExternalFileDragPrepared(..) => Some(true),
            sftp_window::Message::DesktopDropReady(reply) => Some(reply.is_remote()),
            _ => None,
        };
        if pointer_transfer.is_some() && !self.sftp_drop_surface_active(id) {
            if let Some(state) = self.sftp.get_mut(&id) {
                state.cancel_desktop_drop();
            }
            return Task::none();
        }
        if pointer_transfer == Some(false)
            || matches!(
                message,
                sftp_window::Message::DesktopLocalCopyDone(_)
                    | sftp_window::Message::CancelDesktopLocalCopy
                    | sftp_window::Message::DesktopLocalListed(..)
            )
        {
            return self.sftp.get_mut(&id).map_or_else(Task::none, |state| {
                state
                    .update(message)
                    .map(move |message| Message::Sftp(id, message))
            });
        }
        let installed = ClipboardPolicy::load();
        let cached = &self.clipboard.policy;
        let transfer_allowed = installed.problem.is_none()
            && cached.problem.is_none()
            && installed.file_transfer_allowed
            && cached.file_transfer_allowed;
        let editor_policy = installed.remote_editor.max(cached.remote_editor);
        let remote_images_allowed = installed.problem.is_none()
            && cached.problem.is_none()
            && editor_policy != RemoteEditorPolicy::Deny;
        let hosted_owner = self
            .hosted_tool_owner(id)
            .filter(|_| self.project_remote_target(id).is_some());
        if hosted_owner.is_some() {
            match message {
                sftp_window::Message::Reconnect => return self.review_project_sftp_reconnect(id),
                sftp_window::Message::Close => return self.request_hosted_tool_close(id),
                sftp_window::Message::ConfirmClose(approve) => {
                    let Some(token) = self.project_file_close_token(id) else {
                        return Task::none();
                    };
                    if !approve || self.sftp.get(&id).is_some_and(SftpWindow::close_confirmed) {
                        return if approve {
                            self.hosted_tool_close_answer(id, token, true)
                        } else {
                            self.cancel_hosted_sftp_close(id, token)
                        };
                    }
                    return Task::none();
                }
                sftp_window::Message::Dismiss => {
                    if let Some(token) = self.project_file_close_token(id) {
                        return self.cancel_hosted_sftp_close(id, token);
                    }
                }
                _ => {}
            }
            if let Some(error) = self.project_remote_denial(id) {
                let reset = self.review_project_sftp_reconnect(id);
                self.notice(id, error);
                return reset;
            }
        }
        if let Some(state) = self.sftp.get_mut(&id) {
            state.revalidate_file_transfers(transfer_allowed);
            state.revalidate_remote_images(
                remote_images_allowed
                    && state.target().workflow_route_allowed(&installed)
                    && state
                        .target()
                        .workflow_route_allowed(&self.clipboard.policy),
            );
            state.configure_parallel(
                self.window_settings.sftp_parallel_transfers,
                Some(
                    SftpConflictPolicy::from_policy(&installed)
                        .restrict(&self.clipboard.policy)
                        .parallel_cap,
                ),
            );
            state.set_external_editor_policy(transfer_allowed.then_some(editor_policy));
            configure_privileged_editor(state, &self.clipboard.policy, self.vault.as_ref());
        }
        if pointer_transfer == Some(true)
            && let Some(state) = self.sftp.get_mut(&id)
            && (!state.target().workflow_route_allowed(&installed)
                || !state.target().workflow_route_allowed(cached)
                || (state.incognito()
                    && (!installed.incognito_allowed || !cached.incognito_allowed)))
        {
            state.cancel_desktop_drop();
            state.set_status("Managed policy blocks transfers on this SFTP connection.".into());
            return Task::none();
        }
        match message {
            sftp_window::Message::ComparisonView(sftp_window::ComparisonMessage::Ai(
                ticket,
                event,
            )) => {
                let config = self.ai_chat_configuration();
                let scope = comparison_ai::Scope {
                    config,
                    policy: &self.clipboard.policy,
                    vault: self.vault.as_ref(),
                    terminal_secrets: self
                        .sftp
                        .get(&id)
                        .and_then(SftpWindow::attached_to)
                        .and_then(|session| self.sessions.get(&session))
                        .and_then(|pane| pane.session.journal_redaction_secrets()),
                };
                let result = self
                    .sftp
                    .get_mut(&id)
                    .map_or(comparison_ai::Update::None, |state| {
                        state.update_comparison_ai(ticket, event, scope)
                    });
                return match result {
                    comparison_ai::Update::None => Task::none(),
                    comparison_ai::Update::Task(task) => task.map(move |event| {
                        Message::Sftp(
                            id,
                            sftp_window::Message::ComparisonView(
                                sftp_window::ComparisonMessage::Ai(ticket, event),
                            ),
                        )
                    }),
                    comparison_ai::Update::Draft(draft) => {
                        let (editor, opened) = EditorWindow::with_unsaved_draft(&draft);
                        self.show_native_editor(editor, opened)
                    }
                    comparison_ai::Update::Snippet(draft) => {
                        let owner = self.hosted_tool_owner(id).unwrap_or(id);
                        self.open_file_comparison_snippet(owner, draft)
                    }
                };
            }
            sftp_window::Message::ExternalEdit(
                sftp_window::external_edit::Message::ApproveVaultPassword,
            ) => {
                return self.sftp.get_mut(&id).map_or_else(Task::none, |state| {
                    state
                        .approve_vault_sftp_edit(self.vault.as_ref())
                        .map(move |message| Message::Sftp(id, message))
                });
            }
            sftp_window::Message::SetConflictDefault(preference) => {
                self.save_sftp_conflict_default(id, preference);
                return Task::none();
            }
            sftp_window::Message::ShowEditor(document, editor) => {
                if self.clipboard.policy.problem.is_some()
                    || installed.problem.is_some()
                    || editor_policy == RemoteEditorPolicy::Deny
                {
                    return Task::none();
                }
                let Some(state) = self.sftp.get(&id).filter(|state| {
                    state.owns_editor(&editor) && state.target().workflow_route_allowed(&installed)
                }) else {
                    return Task::none();
                };
                let path = sftp_window::Location::Remote(document.path().to_vec()).display();
                let label = format!("{} · {path}", state.target().label());
                let project_file = hosted_owner
                    .zip(std::str::from_utf8(document.path()).ok().filter(|path| {
                        crate::profile::RemoteProjectTool::valid_remote_path(path, false)
                    }))
                    .map(|(owner, path)| (owner, state.target().clone(), path.to_owned()));
                let (editor, loaded) = EditorWindow::with_remote(document, editor, label);
                if let Some((owner, target, path)) = project_file {
                    return self.host_project_remote_editor(owner, target, path, editor, loaded);
                }
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

    fn save_sftp_conflict_default(&mut self, id: window::Id, preference: ConflictDefault) {
        let policy = SftpConflictPolicy::load().restrict(&self.clipboard.policy);
        self.refresh_sftp_conflict_defaults(policy);
        if !policy.editable() {
            return;
        }
        let Some(state) = self
            .sftp
            .get_mut(&id)
            .filter(|state| state.can_set_conflict_default())
        else {
            return;
        };
        if state.incognito() {
            state.set_private_conflict_default(preference);
            return;
        }
        if self.window_settings.sftp_conflict_default == preference {
            return;
        }
        let mut next = self.window_settings.clone();
        next.sftp_conflict_default = preference;
        if let Err(error) = crate::profile::window_settings_path().and_then(|path| next.save(&path))
        {
            self.notice(
                id,
                format!("Could not save the SFTP conflict default ({error})"),
            );
            return;
        }
        self.window_settings.sftp_conflict_default = preference;
        self.refresh_sftp_conflict_defaults(policy);
    }

    fn cancel_hosted_sftp_close(&mut self, id: window::Id, token: u64) -> Task<Message> {
        let close = self.hosted_tool_close_answer(id, token, false);
        let resume = self.sftp.get_mut(&id).map_or_else(Task::none, |state| {
            state
                .resume_after_close()
                .map(move |message| Message::Sftp(id, message))
        });
        Task::batch([close, resume])
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
        self.editors.insert(id, editor);
        let task = opened
            .map(|_| Message::Noop)
            .chain(loaded.map(move |message| Message::Editor(id, message)));
        (id, task)
    }

    pub(super) fn finish_sftp_close(&mut self, id: window::Id) -> Task<Message> {
        #[cfg(target_os = "linux")]
        self.wayland_file_drop.forget(id);
        #[cfg(target_os = "windows")]
        self.windows_file_drag.forget(id);
        self.sftp_logins.remove(&id);
        let mut returned = Task::none();
        if let Some(mut state) = self.sftp.remove(&id) {
            if let Some(partial) = state.retain_partial() {
                self.retained_sftp_download = Some(partial);
            }
            let session = state.attached_to();
            let busy = state.transport_busy();
            if let Some(done) = state.stop_local_desktop_copy() {
                self.track_session_shutdown(done);
            }
            for done in state.stop_parallel() {
                self.track_session_shutdown(done);
            }
            state.close_external_edit();
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
            #[cfg(target_os = "linux")]
            let desktop_drag = self
                .wayland_file_drop
                .can_drag(self.hosted_tool_owner(id).unwrap_or(id));
            #[cfg(target_os = "windows")]
            let desktop_drag =
                self.windows_drag_owner_available(self.hosted_tool_owner(id).unwrap_or(id));
            state
                .view(
                    #[cfg(any(target_os = "linux", target_os = "windows"))]
                    desktop_drag,
                )
                .map(move |message| Message::Sftp(id, message))
        }
    }

    fn sftp_drop_surface_active(&self, id: window::Id) -> bool {
        if self.sftp_logins.contains_key(&id) {
            return false;
        }
        if let Some(owner) = self.hosted_tool_owner(id) {
            return self.owner_dialog(owner).is_none()
                && self.windows.get(&owner).is_some_and(|window| {
                    matches!(window.active_entry(), Some(TabEntry::Tool(tool)) if tool.instance == id)
                });
        }
        self.owner_dialog(id).is_none()
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    pub(super) fn native_sftp_drag_target(&self, owner: window::Id) -> Option<window::Id> {
        if self.exiting
            || self.scheduler_draining
            || self.owner_dialog(owner).is_some()
            || self.palette.is_open(owner)
        {
            return None;
        }
        let id = if let Some(window) = self.windows.get(&owner) {
            let Some(TabEntry::Tool(tool)) = window.active_entry() else {
                return None;
            };
            tool.instance
        } else if self.hosted_tool_owner(owner).is_none() {
            owner
        } else {
            return None;
        };
        (self.sftp.contains_key(&id)
            && !self.hosted_close_locks(id)
            && self.sftp_drop_surface_active(id))
        .then_some(id)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn sftp_conflict_policy_combines_installed_and_startup_locks() {
        let mut cached = ClipboardPolicy::default();
        let mut installed = ClipboardPolicy {
            sftp_conflict_default: Some(ConflictDefault::Overwrite),
            ..ClipboardPolicy::default()
        };
        let newly_managed = SftpConflictPolicy::from_policy(&installed).restrict(&cached);
        assert_eq!(newly_managed.managed, Some(ConflictDefault::Overwrite));
        assert!(!newly_managed.editable());

        cached.sftp_conflict_default = Some(ConflictDefault::Skip);
        let stricter = SftpConflictPolicy::from_policy(&installed).restrict(&cached);
        assert_eq!(stricter.managed, Some(ConflictDefault::Skip));
        assert!(!stricter.editable());

        installed.sftp_conflict_default = None;
        let still_managed = SftpConflictPolicy::from_policy(&installed).restrict(&cached);
        assert_eq!(still_managed.managed, Some(ConflictDefault::Skip));
        assert!(!still_managed.editable());

        cached.sftp_conflict_default = None;
        installed.file_transfer_allowed = false;
        let transfers_denied = SftpConflictPolicy::from_policy(&installed).restrict(&cached);
        assert_eq!(transfers_denied.managed, None);
        assert!(!transfers_denied.editable());
    }

    #[test]
    fn standalone_sftp_from_a_saved_private_or_changed_route_stays_detached() {
        let target: SshTarget = serde_json::from_value(json!({
            "host": "project.example", "port": 22, "username": "fixture", "saved": 4
        }))
        .unwrap();
        let mut saved: crate::connection_store::Connection = serde_json::from_value(json!({
            "id": 4, "name": "Fixture", "tag": "", "jump": null,
            "login": {"endpoint": {"host": "project.example", "port": 22, "username": "fixture"}, "auth": "prompt"},
            "emulation": "xterm-256color", "tunnels": []
        })).unwrap();
        assert!(sftp_project_shareable(false, &target, Some(&saved)));
        assert!(!sftp_project_shareable(true, &target, Some(&saved)));
        assert!(!sftp_project_shareable(false, &target, None));
        saved.incognito = true;
        assert!(!sftp_project_shareable(false, &target, Some(&saved)));
        saved.incognito = false;
        saved.login.endpoint.port = 2222;
        assert!(!sftp_project_shareable(false, &target, Some(&saved)));
    }
}
