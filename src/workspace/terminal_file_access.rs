use std::collections::HashMap;
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::path::{Path, PathBuf};

use async_channel::bounded;
use iced::{Task, window};

use crate::clipboard_policy::RemoteEditorPolicy;
use crate::editor_window::{self, Document, EditorWindow};
use crate::session::{SessionId, Status};
use crate::sftp_window::{
    Location,
    worker::{Connection, Operation, Reply, Worker},
};
use crate::terminal_view::quick_select::FileTarget;

use super::{Message, PaneSession, Workspace};

const MAX_PENDING: usize = 4;
const MAX_REMOTE_WORKERS: usize = 4;

#[derive(Default)]
pub(super) struct TerminalFiles {
    pending: HashMap<u64, Pending>,
    workers: HashMap<SessionId, Worker>,
    remote_editors: HashMap<window::Id, SessionId>,
}

struct Pending {
    owner: window::Id,
    session: SessionId,
    policy: RemoteEditorPolicy,
    line: Option<usize>,
    column: Option<usize>,
    remote: bool,
}

impl Workspace {
    pub(super) fn open_terminal_file(
        &mut self,
        owner: window::Id,
        session: SessionId,
        file: FileTarget,
    ) -> Option<Task<Message>> {
        let pane = self.terminal_file_pane(owner, session)?;
        let remote = pane.ssh.is_some();
        if remote && self.snippet_transfer_owns_sftp(session) {
            return Some(
                self.terminal_file_error(owner, "Wait for the snippet transfer to finish."),
            );
        }
        if !remote && pane.session.local_shell_pid().is_none() {
            return None;
        }
        let policy = self.clipboard.policy.remote_editor;
        if self.clipboard.policy.problem.is_some() || policy == RemoteEditorPolicy::Deny {
            return Some(self.terminal_file_error(owner, "File opening is disabled by policy."));
        }
        if self.terminal_files.pending.len() >= MAX_PENDING {
            return Some(self.terminal_file_error(owner, "Too many file requests are pending."));
        }
        let line = file.line;
        let column = file.column;
        let ticket = self.allocate();
        let request = if remote {
            self.read_remote_terminal_file(owner, session, ticket, policy, &file.path)
        } else {
            self.read_local_terminal_file(session, ticket, policy, file.path)
        };
        match request {
            Ok(task) => {
                self.terminal_files.pending.insert(
                    ticket,
                    Pending {
                        owner,
                        session,
                        policy,
                        line,
                        column,
                        remote,
                    },
                );
                self.notice(owner, "Opening terminal file…".into());
                Some(task)
            }
            Err(error) => {
                self.prune_terminal_file_worker(session);
                Some(self.terminal_file_error(owner, &error))
            }
        }
    }

    fn terminal_file_pane(&self, owner: window::Id, session: SessionId) -> Option<&PaneSession> {
        if self.exiting || self.scheduler_draining || !self.palette_can_open(owner) {
            return None;
        }
        let window = self.windows.get(&owner)?;
        if !window.focused || window.tab()?.focused_session() != Some(session) {
            return None;
        }
        let pane = self.sessions.get(&session)?;
        let frame = pane.session.frame();
        (frame.status == Status::Running && !frame.password_prompt).then_some(pane)
    }

    fn read_local_terminal_file(
        &mut self,
        session: SessionId,
        ticket: u64,
        policy: RemoteEditorPolicy,
        file: String,
    ) -> Result<Task<Message>, String> {
        let pane = &self.sessions[&session];
        if pane.session.sandbox_backend().is_some()
            || self.clipboard.policy.minimum_isolation
                == crate::session_security::IsolationLevel::Sandbox
        {
            return Err("Opening files from a sandboxed shell is unavailable.".into());
        }
        #[cfg(unix)]
        let path = pane
            .session
            .local_shell_directory()
            .map_err(|_| "The shell directory could not be requested.")?;
        #[cfg(windows)]
        let path = pane.session.windows_terminal_file(&file)?;
        let (reply, received) = bounded(1);
        let (finished, done) = bounded(1);
        std::thread::Builder::new()
            .name("kortty-terminal-file".into())
            .spawn(move || {
                let path = path
                    .recv_blocking()
                    .map_err(|_| "The original shell closed.".to_owned())
                    .and_then(|result| result.map_err(|error| error.to_string()));
                #[cfg(unix)]
                let path = path.and_then(|directory| {
                    local_path(&directory, &file).map_err(|error| error.to_string())
                });
                let result = path.and_then(|path| {
                    editor_window::open_terminal_document(
                        &path,
                        policy == RemoteEditorPolicy::Allow,
                    )
                    .map_err(|error| error.to_string())
                });
                let _ = reply.try_send(result);
                drop(finished);
            })
            .map_err(|error| error.to_string())?;
        self.track_session_shutdown(done);
        Ok(Task::perform(
            async move {
                received
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The file worker stopped.".into()))
            },
            move |result| Message::TerminalFileLoaded(ticket, result),
        ))
    }

    fn read_remote_terminal_file(
        &mut self,
        owner: window::Id,
        session: SessionId,
        ticket: u64,
        policy: RemoteEditorPolicy,
        file: &str,
    ) -> Result<Task<Message>, String> {
        let (pane, opener) = self
            .remote_pane(session)
            .ok_or("The original SSH connection is unavailable.")?;
        let target = pane
            .ssh
            .as_ref()
            .ok_or("The original SSH target is unavailable.")?
            .clone();
        let label = target.label();
        let directory = self
            .remote_shell_file_context(session)
            .ok_or("The original remote shell could not be verified.")?;
        let path = remote_path(directory.as_deref(), file)?;

        let manager = self
            .sftp
            .values()
            .find(|state| state.attached_to() == Some(session));
        let shared = manager
            .map(|state| state.terminal_editor_worker())
            .or_else(|| self.remote_dock_editor_worker(owner, session));
        let worker = if let Some(shared) = shared {
            shared.ok_or("Wait for the current SFTP operation to finish.")?
        } else {
            if self
                .remote_browser_closing
                .get(&session)
                .is_some_and(|done| !done.is_closed())
                || self
                    .remote_manager_closing
                    .get(&session)
                    .is_some_and(|done| !done.is_closed())
            {
                return Err("The previous SFTP channel is still closing.".into());
            }
            if !self.terminal_files.workers.contains_key(&session)
                && self.terminal_files.workers.len() >= MAX_REMOTE_WORKERS
            {
                return Err("Close an existing remote file editor first.".into());
            }
            match self.terminal_files.workers.entry(session) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => entry.insert(
                    Worker::start(Connection::Terminal(opener), policy)
                        .map_err(|error| error.to_string())?,
                ),
            }
        };
        worker.bind_policy_route(&target);
        let editor = worker
            .editor()
            .ok_or("The original SFTP channel is unavailable.")?;
        let reply = worker
            .submit(Operation::OpenTerminalEditor(path))
            .map_err(|error| error.to_string())?;
        let task = Task::perform(
            async move {
                let response = reply
                    .recv()
                    .await
                    .map_err(|_| "The original SFTP channel closed.")?
                    .map_err(|failure| failure.message)?;
                let Reply::EditorOpened(document) = response.as_ref() else {
                    return Err("Unexpected SFTP response.".into());
                };
                let path = Location::Remote(document.path().to_vec()).display();
                Ok(Document::remote(
                    document.clone(),
                    editor,
                    format!("{label} · {path}"),
                ))
            },
            move |result| Message::TerminalFileLoaded(ticket, result),
        );
        Ok(task)
    }

    pub(super) fn finish_terminal_file(
        &mut self,
        ticket: u64,
        result: Result<Document, String>,
    ) -> Task<Message> {
        let Some(pending) = self.terminal_files.pending.remove(&ticket) else {
            return Task::none();
        };
        let Pending {
            owner,
            session,
            policy,
            line,
            column,
            remote,
        } = pending;
        let task = if self.clipboard.policy.problem.is_none()
            && self.clipboard.policy.remote_editor == policy
            && self.terminal_file_pane(owner, session).is_some_and(|pane| {
                remote
                    || (pane.session.sandbox_backend().is_none()
                        && self.clipboard.policy.minimum_isolation
                            != crate::session_security::IsolationLevel::Sandbox)
            }) {
            match result {
                Ok(document)
                    if document
                        .remote_editor()
                        .is_some_and(|editor| editor.is_closed()) =>
                {
                    self.terminal_file_error(owner, "The original SFTP channel closed.")
                }
                Ok(document) => {
                    let (editor, loaded) = EditorWindow::with_document(document, line, column);
                    let (id, task) = self.insert_native_editor(editor, loaded);
                    if remote {
                        self.terminal_files.remote_editors.insert(id, session);
                    }
                    self.notice(owner, "Opened terminal file.".into());
                    task
                }
                Err(error) => self.terminal_file_error(owner, &error),
            }
        } else {
            Task::none()
        };
        self.prune_terminal_file_worker(session);
        if remote {
            Task::batch([task, self.sync_remote_dock(owner)])
        } else {
            task
        }
    }

    fn terminal_file_error(&mut self, owner: window::Id, error: &str) -> Task<Message> {
        self.notice(owner, format!("Could not open terminal file. {error}"));
        Task::none()
    }

    fn prune_terminal_file_worker(&mut self, session: SessionId) {
        if self.terminal_files.workers.contains_key(&session)
            && !self.terminal_file_editor_open(session)
            && !self.terminal_file_request_pending(session)
        {
            self.stop_terminal_file_session(session);
        }
    }

    pub(super) fn close_terminal_file_editor(&mut self, id: window::Id) {
        if let Some(session) = self.terminal_files.remote_editors.remove(&id) {
            self.prune_terminal_file_worker(session);
        }
    }

    pub(super) fn terminal_file_uses_sftp(&self, session: SessionId) -> bool {
        self.terminal_file_request_pending(session) || self.terminal_file_editor_open(session)
    }

    pub(super) fn terminal_file_request_pending(&self, session: SessionId) -> bool {
        self.terminal_files
            .pending
            .values()
            .any(|pending| pending.remote && pending.session == session)
    }

    pub(super) fn terminal_file_editor_open(&self, session: SessionId) -> bool {
        self.terminal_files
            .remote_editors
            .iter()
            .any(|(&id, &source)| {
                source == session
                    && self
                        .editors
                        .get(&id)
                        .and_then(EditorWindow::remote_editor)
                        .is_some_and(|editor| !editor.is_closed())
            })
    }

    pub(super) fn take_terminal_file_worker(&mut self, session: SessionId) -> Option<Worker> {
        self.terminal_files.workers.remove(&session)
    }

    pub(super) fn retain_terminal_file_worker(
        &mut self,
        session: SessionId,
        worker: Worker,
    ) -> Option<Worker> {
        let keep = !self.exiting
            && !worker.is_closed()
            && !self.terminal_files.workers.contains_key(&session)
            && (self.terminal_file_request_pending(session)
                || self
                    .terminal_files
                    .remote_editors
                    .iter()
                    .any(|(&id, &source)| {
                        source == session
                            && self
                                .editors
                                .get(&id)
                                .and_then(EditorWindow::remote_editor)
                                .is_some_and(|editor| worker.owns_editor(editor))
                    }));
        if keep {
            self.terminal_files.workers.insert(session, worker);
            None
        } else {
            Some(worker)
        }
    }

    pub(super) fn stop_terminal_file_session(&mut self, session: SessionId) {
        self.terminal_files
            .pending
            .retain(|_, pending| pending.session != session);
        self.terminal_files
            .remote_editors
            .retain(|_, source| *source != session);
        if let Some(worker) = self.terminal_files.workers.remove(&session) {
            let done = worker.stop();
            self.remote_browser_closing
                .retain(|_, pending| !pending.is_closed());
            if !done.is_closed() {
                self.remote_browser_closing.insert(session, done.clone());
            }
            self.track_session_shutdown(done);
        }
    }

    pub(super) fn stop_all_terminal_files(&mut self) {
        self.terminal_files.pending.clear();
        self.terminal_files.remote_editors.clear();
        for (_, worker) in std::mem::take(&mut self.terminal_files.workers) {
            self.track_session_shutdown(worker.stop());
        }
    }
}

#[cfg(unix)]
fn local_path(directory: &Path, path: &str) -> io::Result<PathBuf> {
    if path.contains('\\') || path.starts_with("//") || path.contains(':') || path.ends_with('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid local file path",
        ));
    }
    if let Some(relative) = path.strip_prefix("~/") {
        let home = std::env::home_dir()
            .filter(|path| path.is_absolute())
            .ok_or_else(|| io::Error::other("the local home directory is unavailable"))?;
        return Ok(home.join(relative.trim_start_matches('/')));
    }
    Ok(directory.join(path))
}

fn remote_path(directory: Option<&[u8]>, path: &str) -> Result<Vec<u8>, String> {
    if path.contains('\\') || path.starts_with("//") || path.contains(':') || path.ends_with('/') {
        return Err("Invalid remote file path.".into());
    }
    if path.starts_with('/') || path.starts_with("~/") {
        return Ok(path.as_bytes().to_vec());
    }
    let directory = directory.ok_or("The remote shell has not reported its working directory.")?;
    let mut absolute = directory.to_vec();
    if !absolute.ends_with(b"/") {
        absolute.push(b'/');
    }
    absolute.extend_from_slice(path.as_bytes());
    Ok(absolute)
}
