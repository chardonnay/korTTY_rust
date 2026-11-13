use std::collections::VecDeque;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread;
use std::time::Duration;

use async_channel::{Receiver, Sender, bounded};
use kortty_ssh::{
    HostKey,
    sftp::{Attributes, Error, FileKind, RemoteFile, RemoteText, Sftp},
};

use crate::clipboard_policy::{ClipboardPolicy, RemoteEditorPolicy};
use crate::ssh_connection::{Abort, Channel as SshChannel, Connected, ExecOpener, SftpOpener};
use crate::workspace::SshTarget;

use super::files::{self, LocalStamp, StagedFile};
use super::{
    Change, Directory, Entry, EntryKind, EntryName, Location, ResumePreferences, browser, compare,
    parent, search, tree,
};

type Client = Sftp<Box<dyn Read + Send>, Box<dyn Write + Send>>;
type Result<T> = std::result::Result<T, Failure>;

pub(super) mod archive;
mod deadline;
pub(in crate::sftp_window) mod download_access;
pub(in crate::sftp_window) mod draft_file;
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub(crate) mod drag_out;
pub(super) mod extract;
mod folder;
mod folder_budget;
pub(super) mod folder_usage;
pub(in crate::sftp_window) mod ordinary_access;
pub(super) use folder_budget::FolderBudget;
pub(super) mod ownership;
pub(in crate::sftp_window) mod resume;
pub(super) mod server_copy;
mod source_link;
pub(super) mod sync;
pub(super) use ownership::MAX_TARGETS as MAX_OWNERSHIP_TARGETS;
pub(super) use resume::Offer as ResumeOffer;

pub(crate) struct Worker {
    commands: SyncSender<Command>,
    control: Arc<Control>,
    done: Receiver<()>,
    editor_policy: RemoteEditorPolicy,
    exec: Option<ExecOpener>,
    sftp: Option<SftpOpener>,
}

#[derive(Clone)]
pub(crate) struct RemoteEditor {
    commands: SyncSender<Command>,
    control: Arc<Control>,
    policy: RemoteEditorPolicy,
}

pub(super) struct ComparisonConnection(Weak<Control>);

impl ComparisonConnection {
    pub(super) fn matches(&self, worker: &Worker) -> bool {
        std::ptr::eq(self.0.as_ptr(), Arc::as_ptr(&worker.control)) && !worker.is_closed()
    }
}

impl std::fmt::Debug for RemoteEditor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteEditor")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl RemoteEditor {
    pub(crate) fn writable(&self) -> bool {
        self.policy == RemoteEditorPolicy::Allow
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.control.is_closed()
    }

    pub(crate) fn allows_source_tool(&self, policy: &ClipboardPolicy, writes: bool) -> bool {
        !self.is_closed()
            && self.control.transfer_allowed(policy)
            && policy.remote_editor != RemoteEditorPolicy::Deny
            && (!writes || (self.writable() && policy.remote_editor == RemoteEditorPolicy::Allow))
    }

    pub(crate) async fn save(
        &self,
        document: RemoteText,
        text: Arc<str>,
    ) -> std::result::Result<RemoteText, String> {
        if !self.writable() {
            return Err("Application policy makes remote files read-only.".into());
        }
        if self.control.is_closed() {
            return Err("The original SFTP connection closed. Reopen the remote file.".into());
        }
        let reply = submit(&self.commands, Operation::SaveEditor { document, text })
            .map_err(|error| error.to_string())?
            .recv()
            .await
            .map_err(|_| "The SFTP connection closed during the save. Reopen and verify the file.")?
            .map_err(|error| error.message)?;
        match reply.as_ref() {
            Reply::EditorSaved(document) => Ok(document.clone()),
            _ => Err("Unexpected SFTP save response.".into()),
        }
    }

    pub(super) async fn save_external(
        &self,
        document: RemoteFile,
        bytes: Arc<[u8]>,
        allowed: Arc<AtomicBool>,
    ) -> std::result::Result<RemoteFile, String> {
        if !self.writable() || self.is_closed() || !allowed.load(Ordering::Acquire) {
            return Err(
                "External upload is disabled or the original SFTP connection closed.".into(),
            );
        }
        let reply = submit(
            &self.commands,
            Operation::SaveExternalEditor {
                document,
                bytes,
                allowed,
            },
        )
        .map_err(|error| error.to_string())?
        .recv()
        .await
        .map_err(|_| "The SFTP connection closed during the save. Reopen and verify the file.")?
        .map_err(|error| error.message)?;
        match reply.as_ref() {
            Reply::ExternalEditorSaved(document) => Ok(document.clone()),
            _ => Err("Unexpected SFTP save response.".into()),
        }
    }
}

pub(crate) enum Connection {
    Standalone(Connected),
    Terminal(SftpOpener),
}

enum Channel {
    Standalone(Connected),
    Terminal(SshChannel),
}

#[derive(Default)]
struct Control {
    cancelled: AtomicBool,
    transfer_denied: AtomicBool,
    route: OnceLock<SshTarget>,
    ownership_supported: AtomicBool,
    archive_supported: AtomicBool,
    abort: Mutex<Option<Abort>>,
    host_key: Option<HostKey>,
    resume_preferences: Mutex<ResumePreferences>,
    resume: Mutex<(u64, Option<resume::durable::Context>)>,
}

impl Control {
    fn resume_preferences(&self) -> ResumePreferences {
        *self
            .resume_preferences
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn route_allowed(&self, policy: &ClipboardPolicy) -> bool {
        policy.problem.is_none()
            && self
                .route
                .get()
                .is_none_or(|target| target.workflow_route_allowed(policy))
    }

    fn transfer_allowed(&self, policy: &ClipboardPolicy) -> bool {
        self.route_allowed(policy)
            && policy.file_transfer_allowed
            && !self.transfer_denied.load(Ordering::Acquire)
    }

    fn register(&self, abort: Abort) {
        let mut active = self.abort.lock().unwrap_or_else(|error| error.into_inner());
        if self.cancelled.load(Ordering::Acquire) {
            abort.abort();
        }
        *active = Some(abort);
    }

    fn abort(&self) {
        self.cancelled.store(true, Ordering::Release);
        let active = self.abort.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(abort) = active.as_ref() {
            abort.abort();
        }
    }

    fn is_closed(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
            || self
                .abort
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_ref()
                .is_some_and(Abort::is_closed)
    }
}

impl Connection {
    fn sftp_opener(&self) -> Option<SftpOpener> {
        match self {
            Self::Standalone(ssh) => ssh.sftp_opener(),
            Self::Terminal(opener) => Some(opener.clone()),
        }
    }

    fn verified_host_key(&self) -> Option<&HostKey> {
        match self {
            Self::Standalone(ssh) => ssh.verified_host_key(),
            Self::Terminal(opener) => opener.verified_host_key(),
        }
    }

    fn exec_opener(&self) -> Option<ExecOpener> {
        match self {
            Self::Standalone(ssh) => ssh.exec_opener(),
            Self::Terminal(opener) => Some(opener.exec_opener()),
        }
    }

    fn open(self, control: &Control) -> Result<Channel> {
        match self {
            Self::Standalone(ssh) => Ok(Channel::Standalone(ssh)),
            Self::Terminal(opener) => opener
                .open_cancellable(|| control.cancelled.load(Ordering::Acquire))
                .map(Channel::Terminal)
                .map_err(|error| Failure::disconnected(files::display(&error.to_string()))),
        }
    }
}

impl Channel {
    fn client(&mut self, control: &Control) -> Result<Client> {
        let (reader, writer, abort) = match self {
            Self::Standalone(ssh) => (ssh.take_reader(), ssh.take_writer(), ssh.abort_handle()),
            Self::Terminal(lease) => (
                lease.take_reader(),
                lease.take_writer().map(|writer| Box::new(writer) as _),
                lease.abort_handle(),
            ),
        };
        control.register(abort);
        let reader = reader.ok_or_else(|| Failure::disconnected("SSH SFTP reader missing"))?;
        let writer = writer.ok_or_else(|| Failure::disconnected("SSH SFTP writer missing"))?;
        Sftp::new(reader, writer)
            .map_err(|error| Failure::disconnected(files::display(&error.to_string())))
    }
}

struct Command {
    operation: Operation,
    response: Sender<Result<Arc<Reply>>>,
}

impl Worker {
    pub(crate) fn bind_policy_route(&self, target: &SshTarget) {
        let bound = self.control.route.get_or_init(|| target.clone());
        debug_assert_eq!(bound, target);
    }

    pub(crate) fn set_transfer_allowed(&self, allowed: bool) {
        self.control
            .transfer_denied
            .store(!allowed, Ordering::Release);
    }

    pub(super) fn set_resume_preferences(&self, preferences: ResumePreferences) {
        *self
            .control
            .resume_preferences
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = preferences;
    }

    pub(crate) fn configure_saved_transfers(
        &self,
        profile: Option<&Path>,
        route: &SshTarget,
        preferences: ResumePreferences,
    ) {
        self.set_resume_preferences(preferences);
        self.configure_resume(profile, route);
    }

    pub(in crate::sftp_window) fn configure_resume(
        &self,
        profile: Option<&Path>,
        route: &crate::workspace::SshTarget,
    ) {
        let context =
            profile
                .zip(self.verified_host_key())
                .map(|(profile, key)| resume::durable::Context {
                    profile: profile.to_path_buf(),
                    route: route.clone(),
                    host_key: key.clone(),
                });
        let mut current = self
            .control
            .resume
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if current.1 != context {
            current.0 = current.0.wrapping_add(1);
            current.1 = context;
        }
    }

    pub(crate) fn start(
        connection: Connection,
        editor_policy: RemoteEditorPolicy,
    ) -> io::Result<Self> {
        let exec = connection.exec_opener();
        let sftp = connection.sftp_opener();
        let control = Arc::new(Control {
            host_key: connection.verified_host_key().cloned(),
            ..Control::default()
        });
        let worker_control = Arc::clone(&control);
        let (commands, incoming) = mpsc::sync_channel::<Command>(1);
        let (finished, done) = bounded(1);
        thread::Builder::new()
            .name("kortty-sftp".into())
            .spawn(move || {
                let mut channel = connection.open(&worker_control);
                let mut client = match channel.as_mut() {
                    Ok(channel) => channel.client(&worker_control),
                    Err(error) => Err(error.clone()),
                };
                if let Ok(client) = &client {
                    worker_control
                        .ownership_supported
                        .store(client.supports_lsetstat(), Ordering::Release);
                    worker_control
                        .archive_supported
                        .store(client.supports_hardlink(), Ordering::Release);
                }
                loop {
                    let command = match incoming.recv_timeout(Duration::from_millis(250)) {
                        Ok(command) => command,
                        Err(RecvTimeoutError::Timeout) if !worker_control.is_closed() => continue,
                        Err(_) => break,
                    };
                    if worker_control.cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    let ownership = command.operation.ownership_cancel().cloned();
                    let archive = command.operation.archive_cancel().cloned();
                    let transfer = command.operation.transfer_cancel().cloned();
                    let extracting = command.operation.is_server_extract();
                    let publishing_draft = matches!(&command.operation, Operation::ReviewDraftFile(..) | Operation::PublishDraftFile(..));
                    let browsing_draft = matches!(&command.operation, Operation::BrowseDraftDestination(_));
                    let reply = match client.as_mut() {
                        Ok(client) => match deadline::Guard::for_operation(
                            &command.operation,
                            &worker_control,
                        ) {
                            Ok(deadline) => {
                                let reply = execute(
                                    client,
                                    command.operation,
                                    &worker_control,
                                    editor_policy,
                                );
                                if deadline.is_some_and(deadline::Guard::finish) {
                                    if ownership.is_some() {
                                        Err(ownership::timed_out(reply))
                                    } else if browsing_draft {
                                        Err(Failure::disconnected("Server folder browsing timed out. Reconnect to browse the server."))
                                    } else if publishing_draft {
                                        let detail = match reply {
                                            Ok(Reply::DraftFilePublished(note, _)) => note,
                                            Err(error) => error.message,
                                            _ => "Check the reviewed server destination before retrying.".into(),
                                        };
                                        Err(Failure::disconnected(format!("The server file operation timed out. Reconnect to browse the server. · {detail}")))
                                    } else if archive.is_some() {
                                        let mut message = if extracting {
                                            "Server extraction timed out. Reconnect to browse the server."
                                        } else {
                                            "The server archive operation timed out. Reconnect to browse the server."
                                        }.to_owned();
                                        match reply {
                                            Ok(Reply::Changed { note, .. }) => {
                                                message = format!("{note} · {message}");
                                            }
                                            Err(error) => message = format!("{message} · {}", error.message),
                                            _ => {}
                                        }
                                        Err(Failure::disconnected(message))
                                    } else {
                                        Err(Failure::disconnected(
                                            "Folder size calculation timed out. Reconnect to browse the server.",
                                        ))
                                    }
                                } else {
                                    reply.map(Arc::new)
                                }
                            }
                            Err(error) => Err(error.into()),
                        },
                        Err(error) => Err(error.clone()),
                    };
                    if let Some(cancel) = ownership.or(archive).or(transfer) {
                        cancel.finish();
                    }
                    let disconnected = match reply.as_deref() {
                        Ok(Reply::SyncFinished(report) | Reply::ServerCopyFinished(report)) => {
                            report.disconnected
                        }
                        Err(error) => error.disconnected,
                        _ => false,
                    };
                    let _ = command.response.try_send(reply);
                    if disconnected {
                        break;
                    }
                }
                drop(incoming);
                drop(channel);
                drop(finished);
            })?;
        Ok(Self {
            commands,
            control,
            done,
            editor_policy,
            exec,
            sftp,
        })
    }

    pub(super) fn supports_ownership(&self) -> bool {
        self.control.host_key.is_some()
            && self.control.ownership_supported.load(Ordering::Acquire)
            && !self.control.is_closed()
    }

    pub(super) fn supports_archive(&self) -> bool {
        self.control.host_key.is_some()
            && self.control.archive_supported.load(Ordering::Acquire)
            && !self.control.is_closed()
    }

    pub(super) fn transfer_opener(&self) -> Option<SftpOpener> {
        self.sftp
            .as_ref()
            .filter(|opener| {
                !self.is_closed()
                    && !opener.is_closed()
                    && opener.verified_host_key().is_some()
                    && opener.verified_host_key() == self.verified_host_key()
            })
            .cloned()
    }

    pub(crate) fn submit(&self, operation: Operation) -> io::Result<Receiver<Result<Arc<Reply>>>> {
        submit(&self.commands, operation)
    }

    pub(super) fn comparison_connection(&self) -> ComparisonConnection {
        ComparisonConnection(Arc::downgrade(&self.control))
    }

    pub(crate) fn editor(&self) -> Option<RemoteEditor> {
        let current = ClipboardPolicy::load();
        let policy = self.editor_policy.max(current.remote_editor);
        (policy != RemoteEditorPolicy::Deny
            && self.control.route_allowed(&current)
            && !self.is_closed())
        .then(|| RemoteEditor {
            commands: self.commands.clone(),
            control: Arc::clone(&self.control),
            policy,
        })
    }

    pub(super) fn verified_host_key(&self) -> Option<&HostKey> {
        self.control.host_key.as_ref()
    }

    pub(super) fn exec_opener(&self) -> Option<ExecOpener> {
        self.exec
            .as_ref()
            .filter(|opener| {
                !self.is_closed()
                    && !opener.is_closed()
                    && opener.verified_host_key().is_some()
                    && opener.verified_host_key() == self.verified_host_key()
            })
            .cloned()
    }

    pub(crate) fn owns_editor(&self, editor: &RemoteEditor) -> bool {
        Arc::ptr_eq(&self.control, &editor.control) && !self.is_closed()
    }

    pub(super) fn abort(&self) {
        self.control.abort();
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.done.is_closed() || self.control.is_closed()
    }

    pub(crate) fn stop(self) -> Receiver<()> {
        self.abort();
        self.done.clone()
    }
}

fn submit(
    commands: &SyncSender<Command>,
    operation: Operation,
) -> io::Result<Receiver<Result<Arc<Reply>>>> {
    let (response, receiver) = bounded(1);
    commands
        .try_send(Command {
            operation,
            response,
        })
        .map_err(|error| {
            let kind = match error {
                TrySendError::Disconnected(_) => io::ErrorKind::BrokenPipe,
                TrySendError::Full(_) => io::ErrorKind::WouldBlock,
            };
            io::Error::new(kind, "SFTP worker cannot accept this request")
        })?;
    Ok(receiver)
}

pub(super) struct LocalArchiveJob {
    cancel: sync::Cancel,
    done: Receiver<()>,
}

impl LocalArchiveJob {
    pub(super) fn stop(&self) -> Receiver<()> {
        self.cancel.stop_local();
        self.done.clone()
    }
}

impl Drop for LocalArchiveJob {
    fn drop(&mut self) {
        self.cancel.stop_local();
    }
}

pub(super) fn submit_local_archive(
    operation: Operation,
) -> io::Result<(Receiver<Result<Arc<Reply>>>, LocalArchiveJob)> {
    let cancel = operation
        .archive_cancel()
        .filter(|_| operation.archive_side() == Some(super::Side::Local))
        .cloned()
        .ok_or_else(|| io::Error::other("Expected a local archive operation"))?;
    let (response, receiver) = bounded(1);
    let (finished, done) = bounded(1);
    thread::Builder::new()
        .name("kortty-local-archive".into())
        .spawn(move || {
            let reply = match operation {
                Operation::ReviewLocalArchive(request, cancel) => {
                    archive::local::prepare(*request, &cancel)
                        .map(|plan| Reply::LocalArchiveReview(Arc::new(plan)))
                }
                Operation::ApplyLocalArchive(plan, password, cancel) => archive::local::apply(
                    &plan,
                    password.as_ref().map(crate::ssh_dialog::Password::as_str),
                    &cancel,
                ),
                _ => Err(io::Error::other("Expected a local archive operation").into()),
            };
            let _ = response.try_send(reply.map(Arc::new));
            drop(finished);
        })?;
    Ok((receiver, LocalArchiveJob { cancel, done }))
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.abort();
    }
}

#[derive(Debug, Clone)]
pub struct Failure {
    pub(crate) message: String,
    pub(crate) disconnected: bool,
    pub(super) partial: Option<ResumeOffer>,
    retain_download: bool,
}

impl Failure {
    pub(crate) fn disconnected(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            disconnected: true,
            partial: None,
            retain_download: false,
        }
    }

    pub(super) fn context(mut self, note: impl AsRef<str>) -> Self {
        self.message.push_str(note.as_ref());
        self
    }
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        let retain_download = match &error {
            Error::Transport(_) => true,
            Error::Status(status) => matches!(
                status.code,
                kortty_ssh::sftp::StatusCode::NoConnection
                    | kortty_ssh::sftp::StatusCode::ConnectionLost
            ),
            _ => false,
        };
        Self {
            message: files::display(&error.to_string()),
            disconnected: error.connection_lost(),
            partial: None,
            retain_download,
        }
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self {
            message: files::display(&error.to_string()),
            disconnected: false,
            partial: None,
            retain_download: false,
        }
    }
}

fn terminal_editor_path(client: &mut Client, path: &[u8]) -> Result<Vec<u8>> {
    let path = if let Some(relative) = path.strip_prefix(b"~/") {
        files::remote_join(&client.canonicalize(b".")?, relative)
    } else {
        path.to_vec()
    };
    let slash = path
        .iter()
        .rposition(|&byte| byte == b'/')
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid terminal file path"))?;
    let parent = &path[..slash];
    let name = &path[slash + 1..];
    if name.is_empty() || name == b"." || name == b".." {
        return Err(
            io::Error::new(io::ErrorKind::InvalidInput, "invalid terminal file name").into(),
        );
    }
    let parent = client.canonicalize(if parent.is_empty() { b"/" } else { parent })?;
    Ok(files::remote_join(&parent, name))
}

pub(crate) enum Operation {
    Initial {
        local: Option<PathBuf>,
        remote: Option<Vec<u8>>,
    },
    Browse(Location),
    BrowseDraftDestination(Vec<u8>),
    MeasureFolders(Box<browser::SizeRequest>),
    SearchTree(Box<search::Request>),
    RevealSearchHit(Box<search::Hit>),
    CompareFiles([Location; 2]),
    ReviewDraftFile(Box<draft_file::Request>, sync::Cancel),
    PublishDraftFile(Arc<draft_file::Plan>, sync::Cancel),
    CompareFolders(Box<compare::folders::Request>),
    ApplySync(Box<sync::Plan>, Progress, sync::Cancel),
    ReviewSyncReplacement(Box<sync::replacement::Request>, sync::Cancel),
    ApplySyncReplacement(Arc<sync::replacement::Plan>, Progress, sync::Cancel, bool),
    ReviewServerCopy(Box<server_copy::Request>, sync::Cancel),
    ApplyServerCopy(Arc<server_copy::Plan>, Progress, sync::Cancel),
    ReviewServerArchive(Box<archive::Request>, sync::Cancel),
    ApplyServerArchive(Arc<archive::Plan>, sync::Cancel),
    ReviewServerExtract(Box<extract::Request>, sync::Cancel),
    ApplyServerExtract(Arc<extract::Plan>, sync::Cancel),
    ReviewLocalArchive(Box<archive::local::Request>, sync::Cancel),
    ApplyLocalArchive(
        Arc<archive::local::Plan>,
        Option<crate::ssh_dialog::Password>,
        sync::Cancel,
    ),
    OpenEditor(Vec<u8>),
    OpenProjectImage(Vec<u8>),
    OpenExternalEditor(Vec<u8>),
    OpenTerminalEditor(Vec<u8>),
    SaveEditor {
        document: RemoteText,
        text: Arc<str>,
    },
    SaveExternalEditor {
        document: RemoteFile,
        bytes: Arc<[u8]>,
        allowed: Arc<AtomicBool>,
    },
    Copy {
        transfer: Transfer,
        destination: Destination,
        progress: Progress,
    },
    CopyLink {
        transfer: Transfer,
        destination: Destination,
        progress: Progress,
    },
    ExpandFolder {
        transfer: Transfer,
        destination: Destination,
        progress: Progress,
    },
    ResumeDownload {
        offer: ResumeOffer,
        progress: Progress,
    },
    FindInterruptedDownload,
    DiscardInterruptedUpload(ResumeOffer),
    UploadLocal {
        path: PathBuf,
        target: Vec<u8>,
        progress: Progress,
    },
    Change {
        directory: Location,
        operation: Change,
        selected: Option<Entry>,
        input: String,
    },
    ChangeOwnership {
        directory: Vec<u8>,
        selected: Vec<Entry>,
        uid: Option<u32>,
        gid: Option<u32>,
        route: SshTarget,
        permitted: sync::Cancel,
    },
    ReviewRemoteOwnership(Box<ownership::recursive::Request>, sync::Cancel),
    ApplyRemoteOwnership(Arc<ownership::recursive::Plan>, sync::Cancel),
    #[cfg(unix)]
    ReviewLocalOwnership {
        directory: PathBuf,
        selected: Vec<Entry>,
        principals: ownership::principals::Selection,
        recursive: bool,
        permitted: sync::Cancel,
    },
    #[cfg(unix)]
    ApplyLocalOwnership(Arc<ownership::local::Plan>, sync::Cancel),
}

impl Operation {
    fn transfer_cancel(&self) -> Option<&sync::Cancel> {
        match self {
            Self::Copy { progress, .. }
            | Self::CopyLink { progress, .. }
            | Self::ExpandFolder { progress, .. }
            | Self::ResumeDownload { progress, .. }
            | Self::UploadLocal { progress, .. } => Some(&progress.cancel),
            _ => None,
        }
    }

    pub(super) fn requires_transfer(&self) -> bool {
        match self {
            Self::ReviewServerArchive(request, _) => request.requires_transfer(),
            Self::ApplyServerArchive(plan, _) => plan.requires_transfer(),
            Self::CompareFiles(_)
            | Self::BrowseDraftDestination(_)
            | Self::ReviewDraftFile(..)
            | Self::PublishDraftFile(..)
            | Self::CompareFolders(_)
            | Self::ApplySync(..)
            | Self::ReviewSyncReplacement(..)
            | Self::ApplySyncReplacement(..)
            | Self::OpenExternalEditor(_)
            | Self::SaveExternalEditor { .. }
            | Self::Copy { .. }
            | Self::CopyLink { .. }
            | Self::ExpandFolder { .. }
            | Self::ResumeDownload { .. }
            | Self::FindInterruptedDownload
            | Self::DiscardInterruptedUpload(_)
            | Self::UploadLocal { .. } => true,
            Self::Initial { .. }
            | Self::Browse(_)
            | Self::MeasureFolders(_)
            | Self::SearchTree(_)
            | Self::RevealSearchHit(_)
            | Self::ReviewServerCopy(..)
            | Self::ApplyServerCopy(..)
            | Self::ReviewServerExtract(..)
            | Self::ApplyServerExtract(..)
            | Self::ReviewLocalArchive(..)
            | Self::ApplyLocalArchive(..)
            | Self::OpenEditor(_)
            | Self::OpenProjectImage(_)
            | Self::OpenTerminalEditor(_)
            | Self::SaveEditor { .. }
            | Self::Change { .. }
            | Self::ChangeOwnership { .. }
            | Self::ReviewRemoteOwnership(..)
            | Self::ApplyRemoteOwnership(..) => false,
            #[cfg(unix)]
            Self::ReviewLocalOwnership { .. } | Self::ApplyLocalOwnership(..) => false,
        }
    }

    pub(super) fn queued_copy(
        transfer: Transfer,
        destination: Destination,
        progress: Progress,
    ) -> Self {
        match transfer.kind {
            EntryKind::Directory => Self::ExpandFolder {
                transfer,
                destination,
                progress,
            },
            EntryKind::Link => Self::CopyLink {
                transfer,
                destination,
                progress,
            },
            _ => Self::Copy {
                transfer,
                destination,
                progress,
            },
        }
    }

    pub(super) fn archive_side(&self) -> Option<super::Side> {
        match self {
            Self::ReviewServerArchive(..)
            | Self::ApplyServerArchive(..)
            | Self::ReviewServerExtract(..)
            | Self::ApplyServerExtract(..) => Some(super::Side::Remote),
            Self::ReviewLocalArchive(..) | Self::ApplyLocalArchive(..) => Some(super::Side::Local),
            _ => None,
        }
    }

    pub(super) fn archive_cancel(&self) -> Option<&sync::Cancel> {
        match self {
            Self::ReviewServerArchive(_, cancel)
            | Self::ApplyServerArchive(_, cancel)
            | Self::ReviewServerExtract(_, cancel)
            | Self::ApplyServerExtract(_, cancel)
            | Self::ReviewLocalArchive(_, cancel)
            | Self::ApplyLocalArchive(_, _, cancel) => Some(cancel),
            _ => None,
        }
    }

    fn is_server_extract(&self) -> bool {
        matches!(
            self,
            Self::ReviewServerExtract(..) | Self::ApplyServerExtract(..)
        )
    }

    pub(super) fn ownership_cancel(&self) -> Option<&sync::Cancel> {
        match self {
            Self::ChangeOwnership { permitted, .. }
            | Self::ReviewRemoteOwnership(_, permitted)
            | Self::ApplyRemoteOwnership(_, permitted) => Some(permitted),
            #[cfg(unix)]
            Self::ReviewLocalOwnership { permitted, .. }
            | Self::ApplyLocalOwnership(_, permitted) => Some(permitted),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum Reply {
    Initial {
        local: Arc<Directory>,
        remote: Arc<Directory>,
        home: Vec<u8>,
    },
    Listed(Arc<Directory>),
    MeasuredFolders(browser::Sizes),
    Searched(Arc<search::Review>),
    SearchRevealed(Arc<Directory>, Option<EntryName>),
    Compared(compare::Review),
    DraftFileReviewed(Arc<draft_file::Plan>),
    DraftFilePublished(String, Option<draft_file::Publication>),
    ComparedFolders(Arc<compare::folders::Review>),
    SyncFinished(sync::Report),
    SyncReplacementReview(Arc<sync::replacement::Plan>),
    ServerCopyReview(Arc<server_copy::Plan>),
    ServerCopyFinished(sync::Report),
    ServerArchiveReview(Arc<archive::Plan>),
    ServerExtractReview(Arc<extract::Plan>),
    LocalArchiveReview(Arc<archive::local::Plan>),
    OwnershipReview(Arc<OwnershipReview>),
    EditorOpened(RemoteText),
    ProjectImageOpened(ProjectImage),
    ExternalEditorOpened(RemoteFile),
    ExternalEditorSaved(RemoteFile),
    EditorSaved(RemoteText),
    Changed {
        directory: Option<Arc<Directory>>,
        note: String,
    },
    Conflict(Conflict),
    TransferSkipped(String),
    FolderExpanded(Arc<FolderExpansion>),
    InterruptedDownload {
        offer: Option<ResumeOffer>,
        note: String,
    },
}

#[derive(Debug)]
pub(crate) struct FolderExpansion {
    pub(crate) children: VecDeque<Transfer>,
    pub(crate) skipped: usize,
    pub(crate) note: String,
}

#[derive(Debug)]
pub struct OwnershipReview(OwnershipPlan);

#[derive(Debug)]
enum OwnershipPlan {
    Remote(Arc<ownership::recursive::Plan>),
    #[cfg(unix)]
    Local(Arc<ownership::local::Plan>),
}

impl OwnershipReview {
    pub(super) fn directory_label(&self) -> &str {
        match &self.0 {
            OwnershipPlan::Remote(plan) => plan.directory_label(),
            #[cfg(unix)]
            OwnershipPlan::Local(plan) => plan.directory_label(),
        }
    }

    pub(super) fn rows(&self) -> &[ownership::ReviewRow] {
        match &self.0 {
            OwnershipPlan::Remote(plan) => plan.rows(),
            #[cfg(unix)]
            OwnershipPlan::Local(plan) => plan.rows(),
        }
    }

    pub(super) fn uids(&self) -> (Option<u32>, Option<u32>) {
        match &self.0 {
            OwnershipPlan::Remote(plan) => plan.uids(),
            #[cfg(unix)]
            OwnershipPlan::Local(plan) => plan.uids(),
        }
    }

    pub(super) fn principal_labels(&self) -> (String, String) {
        use ownership::principals::Kind;

        let (uid, gid) = self.uids();
        let selection = self.selection();
        (
            selection.label(Kind::User, uid),
            selection.label(Kind::Group, gid),
        )
    }

    pub(super) fn mode(&self) -> Option<u32> {
        self.selection().mode()
    }

    fn selection(&self) -> &ownership::principals::Selection {
        match &self.0 {
            OwnershipPlan::Remote(plan) => plan.principals(),
            #[cfg(unix)]
            OwnershipPlan::Local(plan) => plan.principals(),
        }
    }

    pub(super) fn skipped(&self) -> usize {
        match &self.0 {
            OwnershipPlan::Remote(plan) => plan.skipped(),
            #[cfg(unix)]
            OwnershipPlan::Local(plan) => plan.skipped(),
        }
    }

    pub(super) fn apply(&self, worker: &Worker, route: &SshTarget) -> Option<Operation> {
        let permit = sync::Cancel::default();
        match &self.0 {
            OwnershipPlan::Remote(plan) => plan
                .applies_to(worker, route)
                .then(|| Operation::ApplyRemoteOwnership(Arc::clone(plan), permit)),
            #[cfg(unix)]
            OwnershipPlan::Local(plan) => {
                Some(Operation::ApplyLocalOwnership(Arc::clone(plan), permit))
            }
        }
    }
}

pub struct ProjectImage(pub(crate) Vec<u8>);

impl std::fmt::Debug for ProjectImage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectImage")
            .field("bytes", &self.0.len())
            .finish()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Transfer {
    pub source: Location,
    pub target_directory: Location,
    pub name: EntryName,
    pub kind: EntryKind,
    pub title: String,
    pub size: Option<u64>,
}

pub(crate) enum Destination {
    Check,
    KeepBoth,
    Replace(Stamp),
}

#[derive(Debug, Clone)]
pub(crate) enum Stamp {
    Local(LocalStamp),
    Remote(Attributes),
}

#[derive(Debug, Clone)]
pub struct Conflict {
    pub(crate) transfer: Transfer,
    pub(crate) existing: Stamp,
    pub(crate) can_replace: bool,
}

impl Conflict {
    pub(crate) fn can_replace(&self) -> bool {
        self.can_replace && self.kind() == super::conflicts::Kind::File
    }

    pub(super) fn kind(&self) -> super::conflicts::Kind {
        use super::conflicts::Kind;

        let target = match &self.existing {
            Stamp::Local(stamp) => stamp.kind,
            Stamp::Remote(attrs) => match attrs.kind() {
                FileKind::File => EntryKind::File,
                FileKind::Directory => EntryKind::Directory,
                FileKind::Symlink => EntryKind::Link,
                FileKind::Special | FileKind::Unknown => EntryKind::Other,
            },
        };
        match (self.transfer.kind, target) {
            (EntryKind::File, EntryKind::File) => Kind::File,
            (EntryKind::Directory, EntryKind::Directory) => Kind::Folder,
            (_, EntryKind::Link) => Kind::Link,
            _ => Kind::TypeMismatch,
        }
    }

    pub(crate) fn description(&self) -> String {
        let size = match &self.existing {
            Stamp::Local(stamp) => Some(stamp.size),
            Stamp::Remote(attrs) => attrs.size,
        };
        let show = |size: Option<u64>| {
            size.map_or_else(|| "unknown size".into(), |bytes| format!("{bytes} bytes"))
        };
        format!(
            "{} · {} → {} · existing {}",
            self.transfer.title,
            show(self.transfer.size),
            self.transfer.target_directory.display(),
            show(size)
        )
    }
}

#[derive(Clone)]
pub(crate) struct Progress {
    bytes: Arc<AtomicU64>,
    total: Option<u64>,
    folder: Option<(FolderBudget, u64)>,
    cancel: sync::Cancel,
}

impl Progress {
    pub(crate) fn new(total: Option<u64>) -> Self {
        Self {
            bytes: Arc::new(AtomicU64::new(0)),
            total,
            folder: None,
            cancel: sync::Cancel::default(),
        }
    }
    pub(super) fn for_folder(total: Option<u64>, budget: FolderBudget, id: u64) -> Self {
        Self {
            folder: Some((budget, id)),
            ..Self::new(total)
        }
    }

    pub(super) fn inherit_folder(&mut self, other: Self) {
        self.folder = other.folder;
    }

    pub(super) fn stop(&self, worker: &Worker) {
        self.cancel.stop(worker);
    }

    fn stopped(&self) -> bool {
        self.cancel.stopped()
    }

    fn check_cancelled(&self) -> io::Result<()> {
        if self.stopped() {
            // Standard stream-copy adapters retry Interrupted. This request
            // must unwind the SFTP handle instead.
            return Err(io::Error::other("Transfer cancelled"));
        }
        Ok(())
    }

    fn reserve(&self, position: u64) -> io::Result<()> {
        self.check_cancelled()?;
        if let Some((budget, id)) = &self.folder {
            budget.reserve(*id, position)?;
        }
        Ok(())
    }

    fn set_position(&self, position: u64) -> io::Result<()> {
        self.reserve(position)?;
        self.bytes.store(position, Ordering::Relaxed);
        Ok(())
    }

    fn reserve_more(&self, bytes: usize) -> io::Result<()> {
        let position = self
            .bytes
            .load(Ordering::Relaxed)
            .checked_add(bytes as u64)
            .ok_or_else(|| io::Error::other("Transfer size overflow"))?;
        self.reserve(position)
    }
    pub(crate) fn label(&self) -> String {
        let bytes = self.bytes.load(Ordering::Relaxed);
        self.total.map_or_else(
            || format!("{bytes} bytes"),
            |total| format!("{bytes}/{total} bytes"),
        )
    }
}

pub(super) struct Counted<'a, T>(pub(super) &'a Progress, pub(super) T);

impl<T: Read> Read for Counted<'_, T> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.0.check_cancelled()?;
        let read = self.1.read(output)?;
        self.0.reserve_more(read)?;
        self.0.bytes.fetch_add(read as u64, Ordering::Relaxed);
        Ok(read)
    }
}

impl<T: Write> Write for Counted<'_, T> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        self.0.reserve_more(input.len())?;
        let written = self.1.write(input)?;
        self.0.bytes.fetch_add(written as u64, Ordering::Relaxed);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.1.flush()
    }
}

pub(super) fn local_home() -> PathBuf {
    let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn execute(
    client: &mut Client,
    operation: Operation,
    control: &Control,
    editor_policy: RemoteEditorPolicy,
) -> Result<Reply> {
    let policy = ClipboardPolicy::load();
    if !control.route_allowed(&policy) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Current policy blocks this SFTP connection",
        )
        .into());
    }
    if operation.requires_transfer() && !control.transfer_allowed(&policy) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Application policy disables file transfer",
        )
        .into());
    }
    let editor_policy = editor_policy.max(policy.remote_editor);
    match operation {
        Operation::Initial { local, remote } => {
            let home = client.canonicalize(b".")?;
            let local = files::list_local(&local.unwrap_or_else(local_home))
                .or_else(|_| files::list_local(&local_home()))?;
            let remote = match remote {
                Some(path) => match list_remote(client, &path) {
                    Err(error) if !error.disconnected => list_remote(client, &home)?,
                    other => other?,
                },
                None => list_remote(client, &home)?,
            };
            Ok(Reply::Initial {
                local: Arc::new(local),
                remote: Arc::new(remote),
                home,
            })
        }
        Operation::Browse(location) => list(client, &location).map(Reply::Listed),
        Operation::BrowseDraftDestination(path) => {
            draft_file::browse(client, &path, editor_policy, control).map(Reply::Listed)
        }
        Operation::MeasureFolders(request) => {
            browser::measure(client, *request, || control.is_closed()).map(Reply::MeasuredFolders)
        }
        Operation::SearchTree(request) => search::read(client, *request, || control.is_closed())
            .map(|review| Reply::Searched(Arc::new(review))),
        Operation::RevealSearchHit(hit) => hit
            .reveal(client, list)
            .map(|(directory, entry)| Reply::SearchRevealed(directory, entry)),
        Operation::CompareFiles(files) => {
            compare::read(client, files, editor_policy, || control.is_closed()).map(Reply::Compared)
        }
        Operation::ReviewDraftFile(request, cancel)
            if editor_policy == RemoteEditorPolicy::Allow =>
        {
            draft_file::prepare(client, *request, &cancel, control)
                .map(|plan| Reply::DraftFileReviewed(Arc::new(plan)))
        }
        Operation::PublishDraftFile(plan, cancel) if editor_policy == RemoteEditorPolicy::Allow => {
            draft_file::apply(client, &plan, &cancel, control)
                .map(|(note, publication)| Reply::DraftFilePublished(note, publication))
        }
        Operation::ReviewDraftFile(_, cancel) | Operation::PublishDraftFile(_, cancel) => {
            cancel.finish();
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Current policy forbids saving an AI draft to the server",
            )
            .into())
        }
        Operation::CompareFolders(request) => {
            compare::folders::read(client, *request, || control.is_closed())
                .map(|review| Reply::ComparedFolders(Arc::new(review)))
        }
        Operation::ApplySync(plan, progress, cancel) => Ok(Reply::SyncFinished(sync::apply(
            client, &plan, &progress, &cancel, control,
        ))),
        Operation::ReviewSyncReplacement(request, cancel) => {
            sync::replacement::prepare(client, *request, &cancel, control)
                .map(|plan| Reply::SyncReplacementReview(Arc::new(plan)))
        }
        Operation::ApplySyncReplacement(plan, progress, cancel, remove) => Ok(Reply::SyncFinished(
            sync::replacement::apply(client, &plan, &progress, &cancel, control, remove),
        )),
        Operation::ReviewServerCopy(request, cancel) => {
            server_copy::prepare(client, *request, &cancel, control)
                .map(|plan| Reply::ServerCopyReview(Arc::new(plan)))
        }
        Operation::ApplyServerCopy(plan, progress, cancel) => Ok(Reply::ServerCopyFinished(
            server_copy::apply(client, &plan, &progress, &cancel, control),
        )),
        Operation::ReviewServerArchive(request, cancel) => {
            archive::prepare(client, *request, &cancel, control)
                .map(|plan| Reply::ServerArchiveReview(Arc::new(plan)))
        }
        Operation::ApplyServerArchive(plan, cancel) => {
            archive::apply(client, &plan, &cancel, control)
        }
        Operation::ReviewServerExtract(request, cancel) => {
            extract::prepare(client, *request, &cancel, control)
                .map(|plan| Reply::ServerExtractReview(Arc::new(plan)))
        }
        Operation::ApplyServerExtract(plan, cancel) => {
            extract::apply(client, &plan, &cancel, control)
        }
        Operation::ReviewLocalArchive(..) | Operation::ApplyLocalArchive(..) => {
            Err(io::Error::other("Local archives use the local worker").into())
        }
        Operation::OpenEditor(path) if editor_policy != RemoteEditorPolicy::Deny => {
            Ok(Reply::EditorOpened(client.open_text(&path)?))
        }
        Operation::OpenProjectImage(path) if editor_policy != RemoteEditorPolicy::Deny => {
            let slash = path
                .iter()
                .rposition(|byte| *byte == b'/')
                .ok_or_else(|| io::Error::other("Invalid reviewed remote image path"))?;
            let parent = &path[..slash.max(1)];
            if client.canonicalize(parent)?.as_slice() != parent {
                return Err(
                    io::Error::other("The reviewed remote image folder is not canonical").into(),
                );
            }
            let attributes = client.lstat(&path)?;
            let size = attributes
                .size
                .filter(|size| *size <= super::picture::MAX_IMAGE_BYTES as u64);
            if attributes.kind() != FileKind::File
                || attributes.modified.is_none()
                || size.is_none()
            {
                return Err(io::Error::other(
                    "Choose a regular remote image no larger than 16 MiB",
                )
                .into());
            }
            let mut bytes = Vec::new();
            client.download_regular(&path, attributes, &mut bytes)?;
            if client.canonicalize(parent)?.as_slice() != parent {
                return Err(Error::SourceChanged.into());
            }
            Ok(Reply::ProjectImageOpened(ProjectImage(bytes)))
        }
        Operation::OpenExternalEditor(path) if editor_policy != RemoteEditorPolicy::Deny => Ok(
            Reply::ExternalEditorOpened(client.open_external_file(&path)?),
        ),
        Operation::OpenTerminalEditor(path) if editor_policy != RemoteEditorPolicy::Deny => {
            let path = terminal_editor_path(client, &path)?;
            Ok(Reply::EditorOpened(client.open_text(&path)?))
        }
        Operation::SaveEditor { document, text } if editor_policy == RemoteEditorPolicy::Allow => {
            let saved = client.save_text(&document, text, || {
                let current = ClipboardPolicy::load();
                control.is_closed()
                    || !control.route_allowed(&current)
                    || current.remote_editor != RemoteEditorPolicy::Allow
            })?;
            Ok(Reply::EditorSaved(saved))
        }
        Operation::SaveExternalEditor {
            document,
            bytes,
            allowed,
        } if editor_policy == RemoteEditorPolicy::Allow => {
            let saved = client.save_external_file(&document, bytes, || {
                let current = ClipboardPolicy::load();
                control.is_closed()
                    || !allowed.load(Ordering::Acquire)
                    || !control.transfer_allowed(&current)
                    || current.remote_editor != RemoteEditorPolicy::Allow
            })?;
            Ok(Reply::ExternalEditorSaved(saved))
        }
        Operation::OpenEditor(_)
        | Operation::OpenProjectImage(_)
        | Operation::OpenExternalEditor(_)
        | Operation::OpenTerminalEditor(_)
        | Operation::SaveEditor { .. }
        | Operation::SaveExternalEditor { .. } => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Application policy forbids this remote editor action",
        )
        .into()),
        Operation::Copy {
            transfer,
            destination,
            progress,
        } => copy(client, transfer, destination, &progress, control),
        Operation::CopyLink {
            transfer,
            destination,
            progress,
        } => copy_link(client, transfer, destination, &progress, control),
        Operation::ExpandFolder {
            transfer,
            destination,
            progress,
        } => {
            progress.check_cancelled()?;
            expand_folder(client, transfer, destination, control)
        }
        Operation::ResumeDownload { offer, progress } => {
            if !control.resume_preferences().enabled {
                return Err(io::Error::other("Resuming partial transfers is disabled").into());
            }
            offer.run(client, &progress, control)
        }
        Operation::FindInterruptedDownload => resume::durable::find_transfer(client, control),
        Operation::DiscardInterruptedUpload(offer) => offer.discard_upload(client, control),
        Operation::UploadLocal {
            path,
            target,
            progress,
        } => {
            let name = path.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "Missing local file name")
            })?;
            let metadata = fs::symlink_metadata(&path)?;
            let kind = files::local_kind(&metadata);
            if !kind.transferable() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Only ordinary files and directories can be uploaded",
                )
                .into());
            }
            let title = files::display(&name.to_string_lossy());
            let name = EntryName::Local(name.to_owned());
            let transfer = Transfer {
                source: Location::Local(path),
                target_directory: Location::Remote(target),
                name,
                kind,
                title,
                size: metadata.is_file().then_some(metadata.len()),
            };
            copy(client, transfer, Destination::Check, &progress, control)
        }
        Operation::Change {
            directory,
            operation,
            selected,
            input,
        } => {
            change(client, &directory, operation, selected.as_ref(), &input)?;
            changed(client, &directory, "File operation completed".into())
        }
        Operation::ChangeOwnership {
            directory,
            selected,
            uid,
            gid,
            route,
            permitted,
        } => ownership::change(client, directory, &selected, uid, gid, || {
            ensure_ownership_allowed(control, &permitted, Some(&route))
        }),
        Operation::ReviewRemoteOwnership(request, permitted) => {
            let route = request.route().clone();
            let plan = ownership::recursive::preview(client, *request, control, || {
                ensure_ownership_allowed(control, &permitted, Some(&route))
            })?;
            Ok(Reply::OwnershipReview(Arc::new(OwnershipReview(
                OwnershipPlan::Remote(Arc::new(plan)),
            ))))
        }
        Operation::ApplyRemoteOwnership(plan, permitted) => {
            ownership::recursive::apply(client, &plan, control, || {
                ensure_ownership_allowed(control, &permitted, Some(plan.route()))
            })
        }
        #[cfg(unix)]
        Operation::ReviewLocalOwnership {
            directory,
            selected,
            principals,
            recursive,
            permitted,
        } => {
            let plan =
                ownership::local::prepare(directory, &selected, principals, recursive, || {
                    ensure_ownership_allowed(control, &permitted, None)
                })?;
            Ok(Reply::OwnershipReview(Arc::new(OwnershipReview(
                OwnershipPlan::Local(Arc::new(plan)),
            ))))
        }
        #[cfg(unix)]
        Operation::ApplyLocalOwnership(plan, permitted) => ownership::local::apply(&plan, || {
            ensure_ownership_allowed(control, &permitted, None)
        }),
    }
}

fn ensure_ownership_allowed(
    control: &Control,
    permitted: &sync::Cancel,
    route: Option<&SshTarget>,
) -> Result<()> {
    if control.is_closed() {
        return Err(Failure::disconnected(
            "The original SFTP connection closed.",
        ));
    }
    if permitted.stopped() {
        return Err(
            io::Error::new(io::ErrorKind::Interrupted, "Ownership change cancelled.").into(),
        );
    }
    let policy = ClipboardPolicy::load();
    if !control.route_allowed(&policy)
        || route.is_some_and(|route| {
            control.host_key.is_none() || !route.workflow_route_allowed(&policy)
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Current policy does not allow this SFTP ownership change.",
        )
        .into());
    }
    Ok(())
}

fn list(client: &mut Client, location: &Location) -> Result<Arc<Directory>> {
    Ok(Arc::new(match location {
        Location::Local(path) => files::list_local(path)?,
        Location::Remote(path) => list_remote(client, path)?,
    }))
}

fn list_remote(client: &mut Client, path: &[u8]) -> Result<Directory> {
    let path = client.canonicalize(path)?;
    if !path.starts_with(b"/") || path.contains(&0) {
        return Err(io::Error::other("server returned a non-absolute path").into());
    }
    let entries = client
        .read_dir(&path)?
        .into_iter()
        .filter(|entry| files::validate_remote_name(&entry.name).is_ok())
        .map(|entry| Entry {
            display: files::display(&String::from_utf8_lossy(&entry.name)),
            hidden: entry.name.starts_with(b"."),
            name: EntryName::Remote(entry.name),
            kind: remote_kind(entry.attrs),
            size: entry.attrs.size,
            modified: entry.attrs.modified,
            owner: entry.attrs.uid,
            group: entry.attrs.gid,
            mode: entry.attrs.permissions,
        })
        .collect();
    Ok(Directory::sorted(Location::Remote(path), entries))
}

fn remote_kind(attributes: Attributes) -> EntryKind {
    match attributes.kind() {
        FileKind::File => EntryKind::File,
        FileKind::Directory => EntryKind::Directory,
        FileKind::Symlink => EntryKind::Link,
        _ => EntryKind::Other,
    }
}

fn changed(client: &mut Client, directory: &Location, note: String) -> Result<Reply> {
    match list(client, directory) {
        Ok(directory) => Ok(Reply::Changed {
            directory: Some(directory),
            note,
        }),
        Err(error) if error.disconnected => Err(error.context(format!(" · {note}"))),
        Err(error) => Ok(Reply::Changed {
            directory: None,
            note: format!("{note} · refresh failed ({})", error.message),
        }),
    }
}

fn expand_folder(
    client: &mut Client,
    transfer: Transfer,
    destination: Destination,
    control: &Control,
) -> Result<Reply> {
    if transfer.kind != EntryKind::Directory || matches!(destination, Destination::Replace(_)) {
        return Err(io::Error::other("A folder transfer cannot replace an existing entry").into());
    }
    let target = destination_path(client, &transfer, &destination)?;
    if matches!(destination, Destination::Check)
        && let Some(existing) = existing(client, &target)?
    {
        let directory = match &existing {
            Stamp::Local(stamp) => stamp.kind == EntryKind::Directory,
            Stamp::Remote(attributes) => attributes.kind() == FileKind::Directory,
        };
        if !directory {
            return Ok(Reply::Conflict(Conflict {
                transfer,
                existing,
                can_replace: false,
            }));
        }
    }
    folder::expand(
        client,
        &transfer,
        &target,
        control,
        matches!(destination, Destination::Check),
    )
    .map(|expansion| Reply::FolderExpanded(Arc::new(expansion)))
}

fn copy_link(
    client: &mut Client,
    mut transfer: Transfer,
    destination: Destination,
    progress: &Progress,
    control: &Control,
) -> Result<Reply> {
    progress.check_cancelled()?;
    if control.is_closed() {
        return Err(io::Error::from(io::ErrorKind::Interrupted).into());
    }
    let resolved = match &transfer.source {
        Location::Local(path) => {
            source_link::local(path)?.map(|(path, size)| (Location::Local(path), size))
        }
        Location::Remote(path) => match remote_existing(client, path)? {
            Some(original) => source_link::remote(client, path, original)?
                .map(|(path, size)| (Location::Remote(path), size)),
            None => None,
        },
    };
    if control.is_closed() {
        return Err(io::Error::from(io::ErrorKind::Interrupted).into());
    }
    let Some((source, size)) = resolved else {
        return Ok(Reply::TransferSkipped(format!(
            "Skipped {} · no accessible regular file target",
            transfer.title
        )));
    };
    // The destination and displayed name belong to the link. Conflicts and
    // recovery continue with the same resolved source path as this attempt.
    transfer.source = source;
    transfer.kind = EntryKind::File;
    transfer.size = Some(size);
    copy(client, transfer, destination, progress, control)
}

fn copy(
    client: &mut Client,
    transfer: Transfer,
    destination: Destination,
    progress: &Progress,
    control: &Control,
) -> Result<Reply> {
    progress.check_cancelled()?;
    let target = destination_path(client, &transfer, &destination)?;
    if let Destination::Check = destination
        && let Some(existing) = existing(client, &target)?
    {
        let can_replace = transfer.kind == EntryKind::File
            && match &existing {
                Stamp::Local(stamp) => cfg!(unix) && stamp.kind == EntryKind::File,
                Stamp::Remote(attrs) => {
                    attrs.kind() == FileKind::File
                        && attrs.permissions.is_some_and(|mode| mode & 0o7000 == 0)
                        && attrs.uid.is_some()
                        && attrs.gid.is_some()
                        && client.supports_atomic_replace()
                }
            };
        return Ok(Reply::Conflict(Conflict {
            transfer,
            existing,
            can_replace,
        }));
    }
    if transfer.kind == EntryKind::Directory {
        if matches!(destination, Destination::Replace(_)) {
            return Err(
                io::Error::other("A folder transfer cannot replace an existing entry").into(),
            );
        }
        let summary = tree::copy(client, &transfer.source, &target, progress, || {
            control.is_closed() || progress.stopped()
        })?;
        return changed(
            client,
            &transfer.target_directory,
            format!("Copied {} · {summary}", transfer.title),
        );
    }
    let replacement = match &destination {
        Destination::Replace(stamp) => Some(stamp),
        _ => None,
    };
    let count = match (&transfer.source, &target, replacement) {
        (Location::Local(source), Location::Remote(target), None) => {
            resume::upload::upload(client, &transfer, source, target, progress, control)?
        }
        (Location::Local(source), Location::Remote(target), Some(Stamp::Remote(stamp))) => {
            upload(client, source, target, Some(*stamp), progress)?
        }
        (Location::Remote(source), Location::Local(target), None) => {
            resume::download(client, &transfer, source, target, progress, control)?
        }
        (Location::Remote(source), Location::Local(target), Some(Stamp::Local(stamp))) => {
            download(client, source, target, Some(stamp), progress)?
        }
        _ => return Err(io::Error::other("invalid transfer direction").into()),
    };
    changed(
        client,
        &transfer.target_directory,
        format!("Copied {} · {count} bytes", transfer.title),
    )
}

fn destination_path(
    client: &mut Client,
    transfer: &Transfer,
    destination: &Destination,
) -> Result<Location> {
    let raw = match &transfer.name {
        EntryName::Local(name) => files::remote_name(name)?,
        EntryName::Remote(name) => {
            files::validate_transfer_name(name)?;
            name.clone()
        }
    };
    let path = |name: &[u8]| -> io::Result<Location> {
        match &transfer.target_directory {
            Location::Local(parent) => Ok(Location::Local(parent.join(files::local_name(name)?))),
            Location::Remote(parent) => Ok(Location::Remote(files::remote_join(parent, name))),
        }
    };
    if !matches!(destination, Destination::KeepBoth) {
        return Ok(path(&raw)?);
    }
    for index in 1..=1000 {
        let name = if transfer.kind == EntryKind::Directory {
            [raw.as_slice(), format!(" ({index})").as_bytes()].concat()
        } else {
            files::numbered(&raw, index)
        };
        let candidate = path(&name)?;
        if existing(client, &candidate)?.is_none() {
            return Ok(candidate);
        }
    }
    Err(io::Error::other("could not find an unused numbered name").into())
}

fn existing(client: &mut Client, location: &Location) -> Result<Option<Stamp>> {
    match location {
        Location::Local(path) => Ok(files::local_existing(path)?.map(Stamp::Local)),
        Location::Remote(path) => {
            remote_existing(client, path).map(|result| result.map(Stamp::Remote))
        }
    }
}

fn remote_existing(client: &mut Client, path: &[u8]) -> Result<Option<Attributes>> {
    match client.lstat(path) {
        Ok(attrs) => Ok(Some(attrs)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn unchanged(previous: Attributes, current: Attributes) -> bool {
    Attributes {
        accessed: None,
        ..previous
    } == Attributes {
        accessed: None,
        ..current
    }
}

fn download(
    client: &mut Client,
    source: &[u8],
    destination: &Path,
    replace: Option<&LocalStamp>,
    progress: &Progress,
) -> Result<u64> {
    let attributes = client.lstat(source)?;
    if attributes.kind() != FileKind::File {
        return Err(io::Error::other("download source is no longer a regular file").into());
    }
    let mut staged = StagedFile::create(parent(destination)?)?;
    let count = client.download(source, Counted(progress, staged.writer()))?;
    if attributes.size.is_some_and(|size| size != count)
        || !unchanged(attributes, client.lstat(source)?)
    {
        return Err(io::Error::other("remote source changed during download").into());
    }
    progress.check_cancelled()?;
    staged.publish(destination, replace)?;
    Ok(count)
}

fn upload(
    client: &mut Client,
    source: &Path,
    target: &[u8],
    replace: Option<Attributes>,
    progress: &Progress,
) -> Result<u64> {
    let original = fs::symlink_metadata(source)?;
    if !original.is_file() {
        return Err(io::Error::other("upload source is no longer a regular file").into());
    }
    let original = LocalStamp::capture(&original);
    let file = files::open_without_links(source)?;
    if LocalStamp::capture(&file.metadata()?) != original {
        return Err(io::Error::other("local source changed before upload").into());
    }
    let slash = target
        .iter()
        .rposition(|&byte| byte == b'/')
        .ok_or_else(|| io::Error::other("remote target has no parent directory"))?;
    let staging = files::remote_join(&target[..=slash], files::nonce()?.as_bytes());
    let staging_label = files::display(&String::from_utf8_lossy(&staging));
    let result = (|| -> Result<u64> {
        let count = client.upload_new(&staging, Counted(progress, &file))?;
        if count != original.size || LocalStamp::capture(&file.metadata()?) != original {
            return Err(io::Error::other("local source changed during upload").into());
        }
        if let Some(expected) = replace {
            let actual = client.lstat(target)?;
            if actual.kind() != FileKind::File || !unchanged(expected, actual) {
                return Err(io::Error::other("remote target changed during upload").into());
            }
            let stage = client.lstat(&staging)?;
            let mode = actual
                .permissions
                .filter(|mode| mode & 0o7000 == 0)
                .ok_or_else(|| io::Error::other("cannot retain remote permissions"))?;
            if actual.uid.is_none()
                || actual.uid != stage.uid
                || actual.gid.is_none()
                || actual.gid != stage.gid
            {
                return Err(io::Error::other("cannot retain remote owner and group").into());
            }
            client.set_permissions(&staging, mode & 0o777)?;
            progress.check_cancelled()?;
            client.replace(&staging, target)?;
        } else {
            progress.check_cancelled()?;
            client.rename(&staging, target)?;
        }
        Ok(count)
    })();
    result.map_err(|error| error.context(format!(" · staging path {staging_label}")))
}

fn change(
    client: &mut Client,
    directory: &Location,
    operation: Change,
    selected: Option<&Entry>,
    input: &str,
) -> Result<()> {
    let source = if operation == Change::NewFolder {
        None
    } else {
        Some(
            selected
                .and_then(|entry| directory.with(&entry.name))
                .ok_or_else(|| io::Error::other("select a file or folder"))?,
        )
    };
    let destination = if matches!(operation, Change::Rename | Change::NewFolder) {
        let name = match directory {
            Location::Local(_) => EntryName::Local(files::local_name(input.as_bytes())?),
            Location::Remote(_) => {
                files::validate_remote_name(input.as_bytes())?;
                EntryName::Remote(input.as_bytes().to_vec())
            }
        };
        directory.with(&name)
    } else {
        None
    };
    if let (Some(entry), Some(path)) = (selected, source.as_ref()) {
        let current = match path {
            Location::Local(path) => {
                let metadata = fs::symlink_metadata(path)?;
                (files::local_kind(&metadata), Some(metadata.len()))
            }
            Location::Remote(path) => {
                let attrs = client.lstat(path)?;
                (remote_kind(attrs), attrs.size)
            }
        };
        if current.0 != entry.kind || entry.size.is_some_and(|size| Some(size) != current.1) {
            return Err(io::Error::other("selected entry changed; refresh the directory").into());
        }
    }
    match (operation, source, destination) {
        (Change::NewFolder, None, Some(Location::Local(path))) => fs::create_dir(path)?,
        (Change::NewFolder, None, Some(Location::Remote(path))) => client.mkdir(path)?,
        (Change::Rename, Some(Location::Local(from)), Some(Location::Local(to))) => {
            files::rename_local(&from, &to)?
        }
        (Change::Rename, Some(Location::Remote(from)), Some(Location::Remote(to))) => {
            client.rename(from, to)?
        }
        (Change::Delete, Some(Location::Local(path)), None) => {
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                fs::remove_dir(path)?;
            } else {
                fs::remove_file(path)?;
            }
        }
        (Change::Delete, Some(Location::Remote(path)), None) => {
            if client.lstat(&path)?.kind() == FileKind::Directory {
                client.remove_dir(path)?;
            } else {
                client.remove_file(path)?;
            }
        }
        (Change::Permissions, Some(Location::Local(path)), None) => {
            files::set_permissions(&path, mode(input)?)?
        }
        (Change::Permissions, Some(Location::Remote(path)), None) => {
            if client.lstat(&path)?.kind() == FileKind::Symlink {
                return Err(io::Error::other("cannot change permissions through a link").into());
            }
            client.set_permissions(path, mode(input)?)?;
        }
        _ => return Err(io::Error::other("invalid file operation").into()),
    }
    Ok(())
}

fn mode(input: &str) -> io::Result<u32> {
    if input.len() != 3 || !input.bytes().all(|byte| matches!(byte, b'0'..=b'7')) {
        return Err(io::Error::other(
            "enter exactly three octal permission digits",
        ));
    }
    u32::from_str_radix(input, 8).map_err(io::Error::other)
}
