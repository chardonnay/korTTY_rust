use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use iced::widget::{button, column, row, space, text};
use iced::{Element, Task, window};

use crate::session::{SessionId, Status};
use crate::sftp_window::{ConflictChoice, Location};

use super::super::{Browser, MAX_DROP_FILES};
use super::{Desktop, Message, Mode, Workspace, wrap};

const DESKTOP_DROP_SETTLE: Duration = Duration::from_millis(100);

pub(in crate::workspace) struct DesktopUploadBatch {
    target: TerminalUploadTarget,
    paths: Vec<PathBuf>,
    ready_at: Instant,
    rejected: bool,
}

impl DesktopUploadBatch {
    fn new(target: TerminalUploadTarget, now: Instant) -> Self {
        Self {
            target,
            paths: Vec::new(),
            ready_at: now + DESKTOP_DROP_SETTLE,
            rejected: false,
        }
    }

    fn push(&mut self, target: Option<&TerminalUploadTarget>, path: PathBuf, now: Instant) {
        self.ready_at = now + DESKTOP_DROP_SETTLE;
        if target != Some(&self.target) || !ordinary_drop_path(&path) {
            self.rejected = true;
        }
        if !self.rejected && !self.paths.contains(&path) {
            if self.paths.len() == MAX_DROP_FILES {
                self.rejected = true;
            } else {
                self.paths.push(path);
            }
        }
    }
}

fn ordinary_drop_path(path: &Path) -> bool {
    path.is_absolute()
        && path.file_name().is_some()
        && path.as_os_str().as_encoded_bytes().len() <= 4096
        && !path.components().any(|part| part == Component::ParentDir)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::workspace) struct TerminalUploadTarget {
    tab: u64,
    session: SessionId,
    stream: u64,
    shell_directory: Option<Vec<u8>>,
}

pub(super) struct TerminalUpload {
    target: TerminalUploadTarget,
    host: String,
    browser: Browser,
    paths: Option<Vec<PathBuf>>,
    invalidated: bool,
}

impl Workspace {
    pub(in crate::workspace) fn queue_desktop_terminal_upload(
        &mut self,
        owner: window::Id,
        session: SessionId,
        stream: u64,
        path: PathBuf,
    ) {
        let allowed = !self.exiting
            && !self.scheduler_draining
            && self.owner_dialog(owner).is_none()
            && !self.palette.is_open(owner)
            && !self.reopen_busy(owner);
        let target = allowed
            .then(|| {
                let tab = self.windows.get(&owner)?.tab()?;
                self.terminal_upload_target(owner, tab.id, session, stream)
            })
            .flatten();
        let Some(window) = self.windows.get_mut(&owner) else {
            return;
        };
        let now = Instant::now();
        if window.pending_terminal_upload.is_none() {
            window.pending_terminal_upload = target
                .as_ref()
                .map(|target| DesktopUploadBatch::new(target.clone(), now));
        }
        if let Some(batch) = &mut window.pending_terminal_upload {
            batch.push(target.as_ref(), path, now);
        }
    }

    pub(in crate::workspace) fn finish_desktop_terminal_uploads(
        &mut self,
        now: Instant,
    ) -> Task<Desktop> {
        let ready: Vec<_> = self
            .windows
            .iter_mut()
            .filter_map(|(&owner, window)| {
                window
                    .pending_terminal_upload
                    .take_if(|batch| now >= batch.ready_at)
                    .map(|batch| (owner, batch))
            })
            .collect();
        Task::batch(ready.into_iter().filter_map(|(owner, batch)| {
            if batch.rejected {
                self.notice(owner, "The file drop changed target or exceeded its limits. Drop the selection again.".into());
                return None;
            }
            self.open_terminal_upload(owner, batch.target, batch.paths)
        }))
    }
    pub(in crate::workspace) fn terminal_upload_target(
        &self,
        owner: window::Id,
        tab: u64,
        session: SessionId,
        stream: u64,
    ) -> Option<TerminalUploadTarget> {
        if !self.clipboard.policy.file_transfer_allowed
            || self.window_settings.terminal_drag_drop == Some(false)
        {
            return None;
        }
        let window = self.windows.get(&owner)?;
        let (index, _) = window.pane_location(session)?;
        let active = window.tab()?;
        if window.tabs.get(index)?.id() != active.id {
            return None;
        }
        if active.id != tab || (active.incognito && !self.clipboard.policy.incognito_allowed) {
            return None;
        }
        let (pane, _) = self.remote_pane(session)?;
        if pane.stream_generation != stream || pane.session.frame().status != Status::Running {
            return None;
        }
        Some(TerminalUploadTarget {
            tab,
            session,
            stream,
            shell_directory: self.remote_shell_file_context(session)?,
        })
    }

    pub(in crate::workspace) fn terminal_upload_target_current(
        &self,
        owner: window::Id,
        target: &TerminalUploadTarget,
    ) -> bool {
        self.terminal_upload_target(owner, target.tab, target.session, target.stream)
            .as_ref()
            == Some(target)
    }

    pub(in crate::workspace) fn open_terminal_upload(
        &mut self,
        owner: window::Id,
        target: TerminalUploadTarget,
        paths: Vec<PathBuf>,
    ) -> Option<Task<Desktop>> {
        if paths.is_empty()
            || paths.len() > MAX_DROP_FILES
            || paths.iter().any(|path| !ordinary_drop_path(path))
            || self.owner_dialog(owner).is_some()
            || !self.terminal_upload_target_current(owner, &target)
        {
            return None;
        }
        let (pane, opener) = self.remote_pane(target.session)?;
        let ssh = pane.ssh.as_ref()?;
        let host = ssh.label();
        let (browser, started) = match Browser::open(
            target.session,
            ssh,
            opener,
            target.shell_directory.clone(),
            self.clipboard.policy.remote_editor,
        ) {
            Ok(result) => result,
            Err(error) => {
                self.notice(
                    owner,
                    format!("Could not open SFTP on this terminal ({error})"),
                );
                return None;
            }
        };
        let upload = TerminalUpload {
            target,
            host,
            browser,
            paths: Some(paths),
            invalidated: false,
        };
        if let Some((_, pane)) = self
            .windows
            .get(&owner)?
            .pane_location(upload.target.session)
        {
            let _ = self.update_message(Desktop::FocusPane(owner, pane));
        }
        let open = self.open_remote_dialog(owner, Mode::TerminalUpload(Box::new(upload)));
        let dialog = self.remote_dialog_for_owner(owner)?;
        Some(Task::batch([open, update_on_completion(dialog, started)]))
    }

    pub(super) fn update_terminal_upload(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        self.revalidate_terminal_uploads();
        let Some(super::Dialog {
            mode: Mode::TerminalUpload(upload),
            ..
        }) = self.remote_dialogs.0.get_mut(&id)
        else {
            return Task::none();
        };
        if upload.invalidated {
            return Task::none();
        }
        let task = match message {
            Message::TerminalUploadCompleted(result) => upload.browser.finish(result),
            Message::Proceed if !upload.browser.busy() && upload.browser.path().is_some() => upload
                .paths
                .take()
                .map_or_else(Task::none, |paths| upload.browser.upload(paths)),
            Message::TerminalUploadResolve(revision, choice)
                if upload.browser.conflict_revision() == Some(revision) =>
            {
                upload.browser.resolve(choice)
            }
            _ => Task::none(),
        };
        update_on_completion(id, task)
    }

    pub(in crate::workspace) fn revalidate_terminal_uploads(&mut self) {
        let invalid: Vec<_> = self
            .remote_dialogs
            .0
            .iter()
            .filter_map(|(&id, dialog)| {
                let Mode::TerminalUpload(upload) = &dialog.mode else {
                    return None;
                };
                (!upload.invalidated
                    && !self.terminal_upload_target_current(dialog.owner, &upload.target))
                .then_some(id)
            })
            .collect();
        for id in invalid {
            if let Some(super::Dialog {
                mode: Mode::TerminalUpload(upload),
                ..
            }) = self.remote_dialogs.0.get_mut(&id)
            {
                upload.invalidated = true;
                if let Some(done) = upload.browser.release() {
                    self.track_session_shutdown(done);
                }
            }
        }
    }

    pub(super) fn stop_terminal_upload(&mut self, mode: Mode) {
        if let Mode::TerminalUpload(mut upload) = mode
            && let Some(done) = upload.browser.release()
        {
            self.track_session_shutdown(done);
        }
    }

    pub(in crate::workspace) fn stop_all_terminal_uploads(&mut self) {
        let stopped: Vec<_> = self
            .remote_dialogs
            .0
            .values_mut()
            .filter_map(|dialog| {
                let Mode::TerminalUpload(upload) = &mut dialog.mode else {
                    return None;
                };
                upload.invalidated = true;
                upload.browser.release()
            })
            .collect();
        self.pending_shutdown.extend(stopped);
    }
}

fn update_on_completion(id: window::Id, task: Task<super::super::Completion>) -> Task<Desktop> {
    task.map(move |reply| wrap(id, Message::TerminalUploadCompleted(reply)))
}

impl TerminalUpload {
    pub(super) fn view(&self, id: window::Id) -> Element<'_, Desktop> {
        let send = move |message| wrap(id, message);
        let mut body = column![text("Upload to SSH Terminal").size(21)].spacing(10);
        if self.invalidated {
            body = body.push(text(
                "The original SSH terminal or folder is no longer available.",
            ));
        } else {
            if let Some(path) = self.browser.path() {
                let folder = Location::Remote(path.to_vec()).display();
                body = body.push(text(format!("{} · {}", self.host, folder)));
            }
            if let Some(paths) = &self.paths {
                body = body.push(text(format!("{} item(s) selected", paths.len())));
            }
            if let Some(status) = self.browser.progress() {
                body = body.push(text(status));
            } else if let Some(status) = self.browser.status() {
                body = body.push(text(status));
            }
            if let Some(conflict) = self.browser.conflict()
                && let Some(revision) = self.browser.conflict_revision()
            {
                let action = |choice| send(Message::TerminalUploadResolve(revision, choice));
                body = body.push(text(conflict.description())).push(
                    row![
                        button("Replace").on_press_maybe(
                            conflict
                                .can_replace()
                                .then(|| action(ConflictChoice::Replace))
                        ),
                        button("Keep both").on_press(action(ConflictChoice::KeepBoth)),
                        button("Skip").on_press(action(ConflictChoice::Skip)),
                    ]
                    .spacing(8),
                );
            }
        }
        let review = !self.invalidated
            && self.paths.is_some()
            && self.browser.path().is_some()
            && !self.browser.busy();
        let label = if self.browser.busy() || review {
            "Cancel"
        } else {
            "Close"
        };
        body.push(space::vertical())
            .push(
                row![
                    space::horizontal(),
                    button(label).on_press(send(Message::Cancel)),
                    review.then(|| button("Upload").on_press(send(Message::Proceed))),
                ]
                .spacing(8),
            )
            .into()
    }
}
