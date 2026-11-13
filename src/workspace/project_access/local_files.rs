//! Local file tools restored in the project tab strip.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced::futures::{StreamExt as _, stream::BoxStream};
use iced::widget::{button, column, row, text};
use iced::{Element, Length, Subscription, Task, window};

use crate::editor_window::{self, EditorWindow};
use crate::profile::{LocalFileTool, PaneLayout, RemoteProjectTool};
use crate::sftp_window::{
    self, SftpWindow,
    picture::{self, Picture},
    worker::{Connection, Operation, Reply, Worker},
};
use crate::ssh_connection::Connected;

use super::super::{Message as Desktop, SshTarget, TabEntry, Workspace};

#[derive(Clone)]
pub enum Message {
    Open,
    ReviewRemote(bool),
    RemoteLoaded(u64, Result<RemoteFile, String>),
    Editor(editor_window::Message),
    Image(Result<Picture, String>),
    Zoom(f32),
}

#[derive(Clone)]
pub enum RemoteFile {
    Editor(kortty_ssh::sftp::RemoteText),
    Image(Picture),
}

impl std::fmt::Debug for Message {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Project local file action")
    }
}

enum Content {
    Unopened,
    Editor(Box<EditorWindow>),
    LoadingImage,
    LoadingRemote(u64),
    Image(Picture),
    Failed(String),
}

pub(in crate::workspace) struct FileTab {
    source: Source,
    pub(in crate::workspace) title: Option<String>,
    pub(in crate::workspace) group: Option<String>,
    content: Content,
    close: Option<u64>,
    portable: bool,
    remote_review: bool,
    pub(super) status: Option<String>,
    pub(in crate::workspace) worker: Option<Worker>,
}

enum Source {
    Local {
        path: PathBuf,
        tool: LocalFileTool,
    },
    Remote {
        target: SshTarget,
        tool: RemoteProjectTool,
    },
}

impl FileTab {
    pub(in crate::workspace) fn new(
        path: PathBuf,
        tool: LocalFileTool,
        title: Option<String>,
        group: Option<String>,
    ) -> Self {
        Self::with_source(Source::Local { path, tool }, title, group)
    }

    pub(in crate::workspace) fn remote(
        target: SshTarget,
        tool: RemoteProjectTool,
        title: Option<String>,
        group: Option<String>,
    ) -> Self {
        Self::with_source(Source::Remote { target, tool }, title, group)
    }

    fn with_source(source: Source, title: Option<String>, group: Option<String>) -> Self {
        Self {
            source,
            title,
            group,
            content: Content::Unopened,
            close: None,
            portable: true,
            remote_review: false,
            status: None,
            worker: None,
        }
    }

    pub(in crate::workspace) fn layout(
        &self,
        sftp: Option<&crate::sftp_window::SftpWindow>,
    ) -> PaneLayout {
        match &self.source {
            Source::Local { path, tool } => PaneLayout::LocalFile {
                path: path.clone(),
                tool: *tool,
            },
            Source::Remote { target, tool } => PaneLayout::RemoteTool {
                target: target.clone(),
                tool: sftp
                    .and_then(|state| state.project_tool(tool))
                    .unwrap_or_else(|| tool.clone()),
            },
        }
    }

    pub(in crate::workspace) fn set_status(&mut self, status: String) {
        self.status = Some(status);
    }

    pub(in crate::workspace) fn remote_source(&self) -> Option<(&SshTarget, &RemoteProjectTool)> {
        match &self.source {
            Source::Remote { target, tool } => Some((target, tool)),
            Source::Local { .. } => None,
        }
    }

    fn file_tool(&mut self) -> Option<&mut LocalFileTool> {
        match &mut self.source {
            Source::Local { tool, .. }
            | Source::Remote {
                tool: RemoteProjectTool::File { tool, .. },
                ..
            } => Some(tool),
            Source::Remote { .. } => None,
        }
    }

    pub(in crate::workspace) fn portable(&self) -> bool {
        self.portable
    }

    fn label(&self) -> &str {
        self.title.as_deref().unwrap_or_else(|| match &self.source {
            Source::Local { path, .. } => path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Local file"),
            Source::Remote {
                tool: RemoteProjectTool::File { path, .. },
                ..
            } => path.rsplit('/').next().unwrap_or("Remote file"),
            Source::Remote { .. } => "SFTP Manager",
        })
    }

    fn start(&mut self) -> Task<Message> {
        if !matches!(self.content, Content::Unopened | Content::Failed(_)) {
            return Task::none();
        }
        let Source::Local { path, tool } = &self.source else {
            self.remote_review = true;
            return Task::none();
        };
        match tool {
            LocalFileTool::Editor => {
                let (editor, opened) = EditorWindow::with_project_file(path.clone());
                self.content = Content::Editor(Box::new(editor));
                opened.map(Message::Editor)
            }
            LocalFileTool::Image { .. } => {
                let path = path.clone();
                self.content = Content::LoadingImage;
                Task::perform(
                    async move { picture::load(&path).map_err(|error| error.to_string()) },
                    Message::Image,
                )
            }
        }
    }

    fn editor(&mut self) -> Option<&mut EditorWindow> {
        match &mut self.content {
            Content::Editor(editor) => Some(editor),
            _ => None,
        }
    }
}

fn idle_clock() -> BoxStream<'static, Desktop> {
    let (send, receive) = async_channel::bounded(1);
    let _ = std::thread::Builder::new()
        .name("kortty-sftp-idle".into())
        .spawn(move || {
            while !send.is_closed() {
                std::thread::sleep(Duration::from_secs(1));
                let _ = send.try_send(());
            }
        });
    receive.map(|()| Desktop::ProjectSftpTick).boxed()
}

fn portable_editor_path(path: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    let path = {
        let text = path.to_str()?;
        Path::new(text.strip_prefix(r"\\?\").unwrap_or(text))
    };
    LocalFileTool::valid_path(path).then(|| path.to_owned())
}

impl Workspace {
    pub(in crate::workspace) fn project_remote_target(&self, id: window::Id) -> Option<&SshTarget> {
        self.project_files
            .get(&id)?
            .remote_source()
            .map(|(target, _)| target)
    }

    pub(in crate::workspace) fn project_remote_denial(&self, id: window::Id) -> Option<String> {
        let (target, tool) = self.project_files.get(&id)?.remote_source()?;
        if self.exiting || self.scheduler_draining || self.hosted_tool_owner(id).is_none() {
            return Some("The project tab is no longer available.".into());
        }
        if self.clipboard.policy.problem.is_some() {
            return Some("Managed policy blocks this SFTP tab.".into());
        }
        if matches!(tool, RemoteProjectTool::File { .. })
            && self.clipboard.policy.remote_editor
                == crate::clipboard_policy::RemoteEditorPolicy::Deny
        {
            return Some("Managed policy blocks opening this remote file.".into());
        }
        if target.saved_id().is_some_and(|saved| {
            self.saved_connection(saved).is_none_or(|connection| {
                connection.incognito
                    || !target.matches(&connection)
                    || target.isolation
                        != connection
                            .isolation
                            .max(self.clipboard.policy.minimum_isolation)
            })
        }) {
            return Some("The saved SSH connection changed or became private. Review it before opening the project tab again.".into());
        }
        target.blocked_server(self)
    }

    pub(in crate::workspace) fn open_project_remote_tool(
        &mut self,
        id: window::Id,
        target: SshTarget,
        ssh: Connected,
    ) -> Task<Desktop> {
        self.sftp_logins.remove(&id);
        if ssh.verified_host_key().is_none() {
            self.notice(
                id,
                "The SFTP connection has no verified host identity.".into(),
            );
            return Task::none();
        }
        if let Some(error) = self.project_remote_denial(id) {
            self.notice(id, error);
            return Task::none();
        }
        let Some((_, tool)) = self.project_files.get(&id).and_then(FileTab::remote_source) else {
            return Task::none();
        };
        let tool = tool.clone();
        if let RemoteProjectTool::Sftp {
            local,
            remote,
            idle_minutes,
        } = tool
        {
            let (mut state, task) = match SftpWindow::open_project(
                target,
                ssh,
                self.clipboard.policy.remote_editor,
                local,
                remote,
            ) {
                Ok(opened) => opened,
                Err(error) => {
                    self.notice(id, format!("Could not open SFTP ({error})"));
                    return Task::none();
                }
            };
            super::super::sftp_access::configure_sftp_preferences(
                &mut state,
                &self.window_settings,
                &self.clipboard.policy,
                idle_minutes,
            );
            super::super::sftp_access::SftpConflictPolicy::load()
                .restrict(&self.clipboard.policy)
                .configure(&mut state, self.window_settings.sftp_conflict_default);
            state.configure_saved_downloads(crate::profile::root().ok());
            state.configure_external_editor(crate::profile::root().ok());
            super::super::sftp_access::configure_privileged_editor(
                &mut state,
                &self.clipboard.policy,
                self.vault.as_ref(),
            );
            state.restore_partial(&mut self.retained_sftp_download);
            if let Some(mut previous) = self.sftp.insert(id, state) {
                for done in previous.stop() {
                    self.track_session_shutdown(done);
                }
            }
            self.note_project_sftp_activity(id);
            return task.map(move |message| Desktop::Sftp(id, message));
        }
        let RemoteProjectTool::File { path, tool } = tool else {
            unreachable!()
        };
        let worker = match Worker::start(
            Connection::Standalone(ssh),
            self.clipboard.policy.remote_editor,
        ) {
            Ok(worker) => worker,
            Err(error) => {
                self.notice(id, format!("Could not open SFTP ({error})"));
                return Task::none();
            }
        };
        let operation = match tool {
            LocalFileTool::Editor => Operation::OpenEditor(path.into_bytes()),
            LocalFileTool::Image { .. } => Operation::OpenProjectImage(path.into_bytes()),
        };
        worker.bind_policy_route(&target);
        let reply = match worker.submit(operation) {
            Ok(reply) => reply,
            Err(error) => {
                self.track_session_shutdown(worker.stop());
                self.notice(id, format!("Could not open the remote file ({error})"));
                return Task::none();
            }
        };
        let ticket = self.allocate();
        let tab = self
            .project_files
            .get_mut(&id)
            .expect("reviewed project file");
        tab.content = Content::LoadingRemote(ticket);
        tab.status = None;
        tab.worker = Some(worker);
        Task::perform(
            async move {
                let reply = reply
                    .recv()
                    .await
                    .map_err(|_| "The SFTP connection closed.")?
                    .map_err(|_| "The reviewed remote file could not be opened.")?;
                match reply.as_ref() {
                    Reply::EditorOpened(document) => Ok(RemoteFile::Editor(document.clone())),
                    Reply::ProjectImageOpened(bytes) => picture::decode(&bytes.0)
                        .map(RemoteFile::Image)
                        .map_err(|error| error.to_string()),
                    _ => Err("Unexpected SFTP response.".into()),
                }
            },
            move |result| Desktop::ProjectFile(id, Message::RemoteLoaded(ticket, result)),
        )
    }

    fn finish_project_remote_file(
        &mut self,
        id: window::Id,
        ticket: u64,
        result: Result<RemoteFile, String>,
    ) -> Task<Desktop> {
        let denial = self.project_remote_denial(id);
        let Some(tab) = self.project_files.get_mut(&id).filter(
            |tab| matches!(tab.content, Content::LoadingRemote(active) if active == ticket),
        ) else {
            return Task::none();
        };
        let mut loaded = Task::none();
        tab.content = match denial.map_or(result, Err) {
            Ok(RemoteFile::Editor(document)) => {
                match tab.worker.as_ref().and_then(Worker::editor) {
                    Some(remote) => {
                        let label = tab.label().to_owned();
                        let (editor, task) = EditorWindow::with_remote(document, remote, label);
                        loaded = task
                            .map(move |message| Desktop::ProjectFile(id, Message::Editor(message)));
                        Content::Editor(Box::new(editor))
                    }
                    None => Content::Failed("The remote editor connection closed.".into()),
                }
            }
            Ok(RemoteFile::Image(picture)) => Content::Image(picture),
            Err(error) => Content::Failed(error),
        };
        if !matches!(tab.content, Content::Editor(_))
            && let Some(worker) = tab.worker.take()
        {
            self.track_session_shutdown(worker.stop());
        }
        loaded
    }

    pub(in crate::workspace) fn remove_project_file(&mut self, id: window::Id) {
        let retain = self.project_remote_denial(id).is_none();
        self.sftp_logins.remove(&id);
        if let Some(mut tab) = self.project_files.remove(&id)
            && let Some(worker) = tab.worker.take()
        {
            self.track_session_shutdown(worker.stop());
        }
        if let Some(state) = self.sftp.remove(&id) {
            self.stop_project_sftp(state, retain);
        }
    }

    fn stop_project_sftp(&mut self, mut state: SftpWindow, retain: bool) {
        if retain && let Some(partial) = state.retain_partial() {
            self.retained_sftp_download = Some(partial);
        }
        for done in state.stop() {
            self.track_session_shutdown(done);
        }
    }

    pub(in crate::workspace) fn project_file_close_token(&self, id: window::Id) -> Option<u64> {
        self.project_files.get(&id)?.close
    }

    pub(in crate::workspace) fn review_project_sftp_reconnect(
        &mut self,
        id: window::Id,
    ) -> Task<Desktop> {
        let retain = self.project_remote_denial(id).is_none();
        let Some(state) = self.sftp.remove(&id) else {
            return Task::none();
        };
        if let Some(tab) = self.project_files.get_mut(&id) {
            if let Source::Remote { tool, .. } = &mut tab.source
                && let Some(current) = state.project_tool(tool)
            {
                *tool = current;
            }
            tab.remote_review = true;
            tab.content = Content::Unopened;
        }
        self.stop_project_sftp(state, retain);
        Task::none()
    }

    pub(in crate::workspace) fn host_project_remote_editor(
        &mut self,
        owner: window::Id,
        target: SshTarget,
        path: String,
        editor: EditorWindow,
        loaded: Task<editor_window::Message>,
    ) -> Task<Desktop> {
        let instance = window::Id::unique();
        let mut tab = FileTab::remote(
            target,
            RemoteProjectTool::File {
                path,
                tool: LocalFileTool::Editor,
            },
            None,
            None,
        );
        tab.content = Content::Editor(Box::new(editor));
        self.project_files.insert(instance, tab);
        let shown = self.host_tool(owner, super::super::tool_tabs::Kind::ProjectFile, instance);
        Task::batch([
            shown,
            loaded.map(move |message| Desktop::ProjectFile(instance, Message::Editor(message))),
        ])
    }

    pub(in crate::workspace) fn host_project_sftp(
        &mut self,
        owner: window::Id,
        state: SftpWindow,
        started: Task<sftp_window::Message>,
    ) -> Task<Desktop> {
        let instance = window::Id::unique();
        let target = state.target().clone();
        let tool = RemoteProjectTool::Sftp {
            local: None,
            remote: None,
            idle_minutes: None,
        };
        let tab = FileTab::remote(target, tool, None, None);
        self.project_files.insert(instance, tab);
        self.sftp.insert(instance, state);
        let shown = self.host_tool(owner, super::super::tool_tabs::Kind::ProjectFile, instance);
        Task::batch([
            shown,
            started.map(move |message| Desktop::Sftp(instance, message)),
        ])
    }

    pub(in crate::workspace) fn active_project_file(&mut self, owner: window::Id) -> Task<Desktop> {
        let Some(TabEntry::Tool(tool)) = self
            .windows
            .get(&owner)
            .and_then(|window| window.active_entry())
        else {
            return Task::none();
        };
        let id = tool.instance;
        self.note_project_sftp_activity(id);
        self.project_files
            .get_mut(&id)
            .filter(|tab| matches!(tab.content, Content::Unopened) && tab.remote_source().is_none())
            .map_or_else(Task::none, |tab| {
                tab.start()
                    .map(move |message| Desktop::ProjectFile(id, message))
            })
    }

    pub(in crate::workspace) fn active_cut_editor(&self, owner: window::Id) -> Option<window::Id> {
        if self.exiting || self.scheduler_draining || !self.palette_can_open(owner) {
            return None;
        }
        let TabEntry::Tool(tool) = self.windows.get(&owner)?.active_entry()? else {
            return None;
        };
        let id = tool.instance;
        let tab = self.project_files.get(&id)?;
        let Content::Editor(editor) = &tab.content else {
            return None;
        };
        let remote_allowed = tab.remote_source().is_none()
            || (self.clipboard.policy.remote_editor
                == crate::clipboard_policy::RemoteEditorPolicy::Allow
                && self.project_remote_denial(id).is_none());
        (tab.close.is_none() && editor.can_cut() && remote_allowed).then_some(id)
    }

    pub(in crate::workspace) fn note_project_sftp_activity(&mut self, id: window::Id) {
        let project_idle = self
            .project_files
            .get(&id)
            .and_then(FileTab::remote_source)
            .and_then(|(_, tool)| match tool {
                RemoteProjectTool::Sftp { idle_minutes, .. } => *idle_minutes,
                _ => None,
            });
        if let Some(state) = self.sftp.get_mut(&id) {
            super::super::sftp_access::configure_sftp_preferences(
                state,
                &self.window_settings,
                &self.clipboard.policy,
                project_idle,
            );
            state.note_activity(Instant::now());
        }
    }

    pub(in crate::workspace) fn project_sftp_subscription(&self) -> Subscription<Desktop> {
        if self
            .sftp
            .values()
            .any(|state| state.idle_enabled() || state.transfer_running())
        {
            Subscription::run(idle_clock)
        } else {
            Subscription::none()
        }
    }

    pub(in crate::workspace) fn update_project_sftp_idle(&mut self, now: Instant) -> Task<Desktop> {
        self.refresh_sftp_preferences();
        let editors: Vec<_> = self
            .project_files
            .values()
            .filter_map(|tab| match &tab.content {
                Content::Editor(editor) => editor.remote_editor(),
                _ => None,
            })
            .chain(
                self.editors
                    .values()
                    .filter_map(EditorWindow::remote_editor),
            )
            .cloned()
            .collect();
        let mut close = Vec::new();
        for (&id, state) in &mut self.sftp {
            let protected = self
                .project_files
                .get(&id)
                .is_some_and(|tab| tab.close.is_some())
                || self.quit_sftp_waiting == Some(id)
                || editors.iter().any(|editor| state.owns_editor(editor));
            if state.idle_expired(now, protected) {
                close.push(id);
            }
        }
        Task::batch(close.into_iter().map(|id| {
            if self.hosted_tool_owner(id).is_some() {
                self.request_hosted_tool_close(id)
            } else {
                self.close_window(id)
            }
        }))
    }

    pub(in crate::workspace) fn update_project_file(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        if let Message::RemoteLoaded(ticket, result) = message {
            return self.finish_project_remote_file(id, ticket, result);
        }
        if let Message::ReviewRemote(approved) = message {
            let Some(tab) = self
                .project_files
                .get_mut(&id)
                .filter(|tab| tab.remote_review && tab.close.is_none())
            else {
                return Task::none();
            };
            tab.remote_review = false;
            return if approved {
                self.show_project_sftp_login(id)
            } else {
                Task::none()
            };
        }
        if matches!(
            message,
            Message::Editor(editor_window::Message::RequestClose)
        ) {
            return self.request_hosted_tool_close(id);
        }
        if matches!(
            message,
            Message::Editor(editor_window::Message::RequestQuit)
        ) {
            return self.quit();
        }
        if matches!(message, Message::Editor(editor_window::Message::Cut)) {
            if !self
                .hosted_tool_owner(id)
                .is_some_and(|owner| self.active_cut_editor(owner) == Some(id))
            {
                return Task::none();
            }
            let selection = self
                .project_files
                .get_mut(&id)
                .and_then(FileTab::editor)
                .and_then(EditorWindow::cut_selection);
            return selection.map_or_else(Task::none, |value| self.write_clipboard(value));
        }
        if matches!(
            message,
            Message::Editor(editor_window::Message::Save | editor_window::Message::SaveAndClose)
        ) && self.project_remote_target(id).is_some()
            && (self.project_remote_denial(id).is_some()
                || self.clipboard.policy.remote_editor
                    != crate::clipboard_policy::RemoteEditorPolicy::Allow)
        {
            self.notice(
                id,
                "Managed policy or a changed SSH route blocks this remote save.".into(),
            );
            return Task::none();
        }
        let Some(tab) = self.project_files.get_mut(&id) else {
            return Task::none();
        };
        let task = match message {
            Message::Open if tab.close.is_none() => tab.start(),
            Message::Image(result) if matches!(tab.content, Content::LoadingImage) => {
                tab.content = match result {
                    Ok(image) => Content::Image(image),
                    Err(error) => Content::Failed(error),
                };
                Task::none()
            }
            Message::Zoom(zoom) if zoom.is_finite() => {
                if let Some(LocalFileTool::Image { zoom: current }) = tab.file_tool() {
                    *current = zoom.clamp(LocalFileTool::MIN_ZOOM, LocalFileTool::MAX_ZOOM);
                }
                return Task::none();
            }
            Message::Editor(message) => {
                let close = tab.close;
                let Some(editor) = tab.editor() else {
                    return Task::none();
                };
                let completed = matches!(
                    message,
                    editor_window::Message::Opened(_) | editor_window::Message::Saved(_)
                );
                let cancel = matches!(message, editor_window::Message::DecideDiscard(false))
                    && editor.close_decision_available();
                let task = editor.update(message).map(Message::Editor);
                let close_answer = close.and_then(|token| {
                    if cancel {
                        Some((token, false))
                    } else {
                        editor.may_close().then_some((token, true))
                    }
                });
                if completed
                    && let Some(path) = editor.local_document_path().map(portable_editor_path)
                {
                    tab.portable = path.is_some();
                    if let (Some(path), Source::Local { path: current, .. }) =
                        (path, &mut tab.source)
                    {
                        *current = path;
                    }
                }
                let task = task.map(move |message| Desktop::ProjectFile(id, message));
                if let Some((token, approve)) = close_answer {
                    return Task::batch([task, self.hosted_tool_close_answer(id, token, approve)]);
                }
                if completed && close.is_some() {
                    return Task::batch([task, self.resume_hosted_tool_close(id)]);
                }
                return task;
            }
            _ => Task::none(),
        };
        task.map(move |message| Desktop::ProjectFile(id, message))
    }

    pub(in crate::workspace) fn project_file_title(&self, id: window::Id) -> String {
        self.project_files
            .get(&id)
            .map_or_else(|| "Local file".into(), |tab| tab.label().to_owned())
    }

    pub(in crate::workspace) fn project_file_needs_close(&self, id: window::Id) -> bool {
        self.sftp.get(&id).is_some_and(|state| state.transfer_running() || state.external_edit_active())
            || self.project_files.get(&id).is_some_and(
            |tab| matches!(&tab.content, Content::Editor(editor) if editor.needs_close_decision()),
        )
    }

    pub(in crate::workspace) fn prompt_project_file_close(
        &mut self,
        id: window::Id,
        token: Option<u64>,
    ) {
        if let Some(state) = self.sftp.get_mut(&id) {
            if token.is_some() {
                state.request_close();
            } else {
                state.dismiss_close();
            }
        }
        if let Some(tab) = self.project_files.get_mut(&id) {
            tab.close = token;
            if let Some(editor) = tab.editor() {
                if token.is_some() {
                    editor.request_close();
                } else {
                    editor.cancel_hosted_close();
                }
            }
        }
    }

    pub(in crate::workspace) fn project_file_view(
        &self,
        id: window::Id,
    ) -> Option<Element<'_, Desktop>> {
        let tab = self.project_files.get(&id)?;
        if let Some(pending) = self.sftp_logins.get(&id) {
            return Some(
                column![
                    self.ssh_dialog_view(id, pending),
                    tab.status.as_ref().map(|status| text(status).size(13))
                ]
                .spacing(8)
                .padding(12)
                .into(),
            );
        }
        if let Some(state) = self.sftp.get(&id) {
            return Some(self.view_sftp(id, state));
        }
        if let Content::Editor(editor) = &tab.content {
            return Some(
                editor
                    .view(
                        self.editor_content_theme(),
                        self.window_settings.editor_appearance(),
                        self.window_settings.editor_caret,
                    )
                    .map(move |message| Desktop::ProjectFile(id, Message::Editor(message))),
            );
        }
        let body: Element<'_, Message> = match &tab.content {
            Content::Unopened | Content::Failed(_) if tab.remote_review => column![
                text("Review the SSH route and file or folders shown above before connecting."),
                row![
                    button("Cancel").on_press(Message::ReviewRemote(false)),
                    button("Continue to SSH sign-in").on_press(Message::ReviewRemote(true))
                ]
                .spacing(8)
            ]
            .spacing(8)
            .into(),
            Content::Unopened => button(if tab.remote_source().is_some() {
                "Review remote tab…"
            } else {
                "Open local file"
            })
            .on_press(Message::Open)
            .into(),
            Content::Failed(error) => {
                column![text(error), button("Try again").on_press(Message::Open)]
                    .spacing(8)
                    .into()
            }
            Content::LoadingImage => text("Opening image…").into(),
            Content::LoadingRemote(_) => text("Opening the reviewed remote file…").into(),
            Content::Image(picture) => {
                let zoom = match &tab.source {
                    Source::Local {
                        tool: LocalFileTool::Image { zoom },
                        ..
                    }
                    | Source::Remote {
                        tool:
                            RemoteProjectTool::File {
                                tool: LocalFileTool::Image { zoom },
                                ..
                            },
                        ..
                    } => *zoom,
                    _ => return None,
                };
                picture.view(zoom, Message::Zoom)
            }
            Content::Editor(_) => unreachable!(),
        };
        let detail = match &tab.source {
            Source::Local { path, .. } => path.display().to_string(),
            Source::Remote { target, tool } => super::remote_tool_description(target, tool),
        };
        let view: Element<'_, Message> = column![
            text(detail).size(13),
            body,
            tab.status.as_ref().map(|status| text(status).size(13))
        ]
        .spacing(8)
        .padding(12)
        .height(Length::Fill)
        .into();
        Some(view.map(move |message| Desktop::ProjectFile(id, message)))
    }
}

#[cfg(test)]
mod tests {
    use iced::futures::executor::block_on;
    use iced::widget::text_editor::{Action, Edit, Motion};

    use super::*;
    #[cfg(unix)]
    use crate::profile::projects;
    use crate::profile::projects::{Name, ProjectFile, java::export};
    use crate::workspace::{chrome, tool_tabs};

    fn complete_file_task(workspace: &mut Workspace, task: Task<Desktop>) {
        let mut stream = iced_runtime::task::into_stream(task).expect("file load task");
        while let Some(action) = block_on(stream.next()) {
            if let iced_runtime::Action::Output(message @ Desktop::ProjectFile(..)) = action {
                let _ = workspace.update(message);
                return;
            }
        }
        panic!("file task returned no outcome");
    }

    fn load_cut_editor(
        workspace: &mut Workspace,
        owner: window::Id,
        source: PathBuf,
    ) -> window::Id {
        let id = window::Id::unique();
        workspace
            .project_files
            .insert(id, FileTab::new(source, LocalFileTool::Editor, None, None));
        let _ = workspace.host_tool(owner, tool_tabs::Kind::ProjectFile, id);
        let load = workspace.update(Desktop::ProjectFile(id, Message::Open));
        complete_file_task(workspace, load);
        for action in [
            Action::Move(Motion::DocumentStart),
            Action::Select(Motion::Right),
            Action::Select(Motion::Right),
        ] {
            let _ = workspace.update(Desktop::ProjectFile(
                id,
                Message::Editor(editor_window::Message::Edit(action)),
            ));
        }
        id
    }

    #[test]
    fn hosted_editor_cut_menu_routes_selection_clipboard_and_undo() {
        use iced::keyboard::{Key, Modifiers};

        let scratch = crate::snippets::tests::Profile::new();
        let directory = portable_editor_path(&scratch.0.canonicalize().unwrap()).unwrap();
        let source = directory.join("cut.txt");
        let original = "αβ keep this\r\nsecond line\r\n";
        std::fs::write(&source, original).unwrap();
        let mut workspace = Workspace::default();
        workspace.clipboard.policy.internal = true;
        workspace.clipboard.policy.write_allowed = false;
        workspace.clipboard.buffer.set("previous".into());
        let (owner, _) = workspace.open_window(false);
        let editor = load_cut_editor(&mut workspace, owner, source.clone());
        let editor_tab = workspace.windows[&owner].active_tab_id().unwrap();

        let invoke = |owner| Desktop::Chrome(chrome::Message::Invoke(owner, chrome::Action::Cut));
        let (other, _) = workspace.open_window(false);
        workspace.add_tab(other, crate::session::SessionId(42));
        assert!(!workspace.chrome_enabled(other, chrome::Action::Cut));
        let _ = workspace.update(invoke(other));
        assert_eq!(
            workspace.clipboard.buffer.get().as_deref(),
            Some("previous")
        );
        let key = Key::Character("x".into());
        assert_eq!(
            workspace
                .window_settings
                .key_bindings
                .action_for(&key, Modifiers::CTRL),
            None
        );
        assert_eq!(
            crate::input::encode_key(&key, Modifiers::CTRL, None, false),
            Some(vec![0x18])
        );

        assert!(workspace.chrome_enabled(owner, chrome::Action::Cut));
        let _ = workspace.update(invoke(owner));
        assert_eq!(workspace.clipboard.buffer.get().as_deref(), Some("αβ"));
        assert!(workspace.project_file_needs_close(editor));
        assert_eq!(std::fs::read_to_string(&source).unwrap(), original);

        let edit = |message| Desktop::ProjectFile(editor, Message::Editor(message));
        let _ = workspace.update(edit(editor_window::Message::Undo));
        assert!(!workspace.project_file_needs_close(editor));

        workspace.add_tab(owner, crate::session::SessionId(43));
        let _ = workspace.update(edit(editor_window::Message::Cut));
        assert!(!workspace.project_file_needs_close(editor));
        let _ = workspace.update(Desktop::SelectTab(owner, editor_tab));

        workspace.clipboard.policy.internal = false;
        let mut actions = iced_runtime::task::into_stream(workspace.update(invoke(owner))).unwrap();
        let Some(iced_runtime::Action::Clipboard(iced_runtime::clipboard::Action::Write {
            contents,
            ..
        })) = block_on(actions.next())
        else {
            panic!("expected a system clipboard write for the restored selection");
        };
        assert_eq!(contents, "αβ");
        let save = workspace.update(edit(editor_window::Message::Save));
        complete_file_task(&mut workspace, save);
        assert_eq!(
            std::fs::read_to_string(&source).unwrap(),
            " keep this\r\nsecond line\r\n"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_cut_menu_dispatches_only_to_the_focused_main_editor_owner() {
        let scratch = crate::snippets::tests::Profile::new();
        let source = scratch.0.canonicalize().unwrap().join("cut.txt");
        std::fs::write(&source, "αβ retained").unwrap();
        let mut workspace = Workspace::default();
        workspace.clipboard.policy.internal = true;
        let (owner, _) = workspace.open_window(false);
        let editor = load_cut_editor(&mut workspace, owner, source);
        let (other, _) = workspace.open_window(false);
        let _ = workspace.update(Desktop::Window(other, window::Event::Focused));
        let _ = workspace.native_menu(chrome::Action::Cut as u16);
        assert!(workspace.clipboard.buffer.get().is_none());
        assert!(!workspace.project_file_needs_close(editor));

        let _ = workspace.update(Desktop::Window(owner, window::Event::Focused));
        let _ = workspace.native_menu(chrome::Action::Cut as u16);
        assert_eq!(workspace.clipboard.buffer.get().as_deref(), Some("αβ"));
        assert!(workspace.project_file_needs_close(editor));
    }

    #[test]
    fn restored_remote_tools_remain_in_order_and_require_review_before_sign_in() {
        let scratch = crate::snippets::tests::Profile::new();
        let root = portable_editor_path(&scratch.0.canonicalize().unwrap()).unwrap();
        let input = root.join("remote.kortty");
        let catalogue = root.join("connections.xml");
        std::fs::write(
            &input,
            include_bytes!("../../profile/projects/java/fixtures/remote-tools.kortty"),
        )
        .unwrap();
        std::fs::write(
            &catalogue,
            include_bytes!("../../profile/projects/java/fixtures/connections.xml"),
        )
        .unwrap();
        let layout = crate::profile::projects::java::read(&input, Some(&catalogue))
            .unwrap()
            .layout;
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.restore_layout_into(&layout, Some(owner)).unwrap();
        let tools = workspace.hosted_tools_in(owner);
        assert_eq!(tools.len(), 3);
        assert_eq!(workspace.windows[&owner].active, 2);
        assert!(workspace.sftp.is_empty() && workspace.sftp_logins.is_empty());
        let restored = workspace.capture_layout().unwrap();
        assert_eq!(restored.windows[0].tabs, layout.windows[0].tabs);
        assert_eq!(restored.windows[0].active, 2);

        let image = tools[2].instance;
        let _ = workspace.update(Desktop::ProjectFile(image, Message::ReviewRemote(true)));
        assert!(workspace.sftp_logins.is_empty());
        let _ = workspace.update(Desktop::ProjectFile(image, Message::Open));
        assert!(workspace.sftp_logins.is_empty());
        workspace.clipboard.policy.file_transfer_allowed = false;
        workspace.clipboard.policy.remote_editor =
            crate::clipboard_policy::RemoteEditorPolicy::Deny;
        assert!(workspace.project_remote_denial(tools[0].instance).is_none());
        let _ = workspace.update(Desktop::ProjectFile(image, Message::ReviewRemote(true)));
        assert!(workspace.sftp_logins.is_empty());
        workspace.clipboard.policy.remote_editor =
            crate::clipboard_policy::RemoteEditorPolicy::ReadOnly;
        let _ = workspace.update(Desktop::ProjectFile(image, Message::Open));
        let _ = workspace.update(Desktop::ProjectFile(image, Message::ReviewRemote(true)));
        assert!(workspace.sftp_logins.contains_key(&image));
        assert!(
            workspace.sftp.is_empty(),
            "review opens only the SSH login form"
        );
        let _ = workspace.update(Desktop::Ssh(
            super::super::super::ssh_access::Message::Cancel(image),
        ));
        assert!(workspace.sftp_logins.is_empty());
    }

    #[test]
    fn java_local_file_tabs_restore_capture_export_and_keep_editor_discard_guard() {
        let scratch = crate::snippets::tests::Profile::new();
        let path = portable_editor_path(&scratch.0.canonicalize().unwrap()).unwrap();
        let source = path.join("notes.txt");
        let picture = path.join("sample.png");
        std::fs::write(&source, "fixture file bytes\n").unwrap();
        ::image::RgbaImage::new(3, 2).save(&picture).unwrap();
        let input = path.join("input.kortty");
        let output = std::env::var_os("KORTTY_JAVA_PROJECT_LOCAL_FILES_OUTPUT")
            .map_or_else(|| path.join("output.kortty"), PathBuf::from);
        let xml_path = |path: &Path| {
            path.to_str()
                .unwrap()
                .replace('&', "&amp;")
                .replace('<', "&lt;")
        };
        let xml = format!(
            r#"<project><name>Local tools</name><windows><window>
            <tabs>
                <tab><tabType>FILE_EDITOR</tabType><sessionId>editor</sessionId><tabTitle>Notes</tabTitle>
                    <editorIsRemote>false</editorIsRemote><editorFilePath>{}</editorFilePath></tab>
                <tab><tabType>IMAGE_VIEWER</tabType><sessionId>image</sessionId><imageIsRemote>false</imageIsRemote>
                    <imageFilePath>{}</imageFilePath><imageZoomLevel>1.5</imageZoomLevel></tab>
            </tabs><activeSessionId>image</activeSessionId></window></windows></project>"#,
            xml_path(&source),
            xml_path(&picture)
        );
        std::fs::write(&input, xml).unwrap();
        let project = ProjectFile::new(input, None).unwrap().read().unwrap();
        assert_eq!(project.layout.windows[0].active, 1);
        assert!(project.java_notes.is_empty());

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (_, restore) = workspace
            .restore_layout_into(&project.layout, Some(owner))
            .unwrap();
        complete_file_task(&mut workspace, restore);
        assert!(workspace.sessions.is_empty());
        let tools = workspace.hosted_tools_in(owner);
        assert_eq!(tools.len(), 2);
        let editor = tools[0];
        let image = tools[1];
        assert_eq!(workspace.project_file_title(editor.instance), "Notes");
        let Content::Image(loaded) = &workspace.project_files[&image.instance].content else {
            panic!("active image did not load");
        };
        assert_eq!(loaded.dimensions(), (3, 2));
        let task = workspace.update(Desktop::SelectTab(owner, editor.id));
        complete_file_task(&mut workspace, task);
        let _ = workspace.update(Desktop::ProjectFile(
            editor.instance,
            Message::Editor(editor_window::Message::Edit(Action::Edit(Edit::Insert(
                '!',
            )))),
        ));
        assert!(workspace.project_file_needs_close(editor.instance));
        let _ = workspace.request_hosted_tool_close(editor.instance);
        assert!(workspace.hosted_close_locks(editor.instance));
        let _ = workspace.update(Desktop::ProjectFile(
            editor.instance,
            Message::Editor(editor_window::Message::DecideDiscard(false)),
        ));
        assert!(!workspace.hosted_close_locks(editor.instance));
        assert!(workspace.project_file_needs_close(editor.instance));
        let _ = workspace.update(Desktop::ProjectFile(image.instance, Message::Zoom(2.0)));

        let layout = workspace.capture_layout().unwrap();
        let saved = &layout.windows[0];
        assert_eq!(saved.active, 0);
        assert_eq!(
            saved.tabs[0],
            PaneLayout::LocalFile {
                path: source.clone(),
                tool: LocalFileTool::Editor
            }
        );
        assert_eq!(
            saved.tabs[1],
            PaneLayout::LocalFile {
                path: picture.clone(),
                tool: LocalFileTool::Image { zoom: 2.0 }
            }
        );
        export::prepare(
            &Name::parse("Local tools").unwrap(),
            &layout,
            output.clone(),
            None,
        )
        .unwrap()
        .publish()
        .unwrap();
        let xml = std::fs::read_to_string(&output).unwrap();
        assert!(!xml.contains("fixture file bytes"));
        assert!(xml.contains("<imageZoomLevel>2</imageZoomLevel>"));
        let restored = ProjectFile::new(output, None).unwrap().read().unwrap();
        assert_eq!(restored.layout.windows[0].tabs, saved.tabs);
        assert_eq!(restored.layout.windows[0].tab_titles, saved.tab_titles);
        assert_eq!(restored.layout.windows[0].active, 0);
        assert_eq!(
            std::fs::read_to_string(source).unwrap(),
            "fixture file bytes\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn project_local_file_does_not_follow_directory_or_file_symlinks() {
        let scratch = crate::snippets::tests::Profile::new();
        let root = scratch.0.canonicalize().unwrap();
        let file = root.join("actual.txt");
        std::fs::write(&file, "fixture").unwrap();
        std::os::unix::fs::symlink(&file, root.join("alias.txt")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("alias-dir")).unwrap();
        assert!(projects::open_local_file(&root.join("alias.txt")).is_err());
        assert!(projects::open_local_file(&root.join("alias-dir").join("actual.txt")).is_err());
        assert!(projects::open_local_file(&file).is_ok());
    }
}
