use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use async_channel::{Receiver, Sender, bounded};
use kortty_ssh::sftp::{Attributes, Error, FileKind, RemoteText, Sftp};

use crate::clipboard_policy::RemoteEditorPolicy;
use crate::ssh_connection::{Abort, Channel as SshChannel, Connected, SftpOpener};

use super::files::{self, LocalStamp, StagedFile};
use super::{Change, Directory, Entry, EntryKind, EntryName, Location, parent, tree};

type Client = Sftp<Box<dyn Read + Send>, Box<dyn Write + Send>>;
type Result<T> = std::result::Result<T, Failure>;

pub(crate) struct Worker {
    commands: SyncSender<Command>,
    control: Arc<Control>,
    done: Receiver<()>,
    editor_policy: RemoteEditorPolicy,
}

#[derive(Clone)]
pub(crate) struct RemoteEditor {
    commands: SyncSender<Command>,
    control: Arc<Control>,
    policy: RemoteEditorPolicy,
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
    abort: Mutex<Option<Abort>>,
}

impl Control {
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
    pub(crate) fn start(
        connection: Connection,
        editor_policy: RemoteEditorPolicy,
    ) -> io::Result<Self> {
        let control = Arc::new(Control::default());
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
                loop {
                    let command = match incoming.recv_timeout(Duration::from_millis(250)) {
                        Ok(command) => command,
                        Err(RecvTimeoutError::Timeout) if !worker_control.is_closed() => continue,
                        Err(_) => break,
                    };
                    if worker_control.cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    let reply = match client.as_mut() {
                        Ok(client) => {
                            execute(client, command.operation, &worker_control, editor_policy)
                                .map(Arc::new)
                        }
                        Err(error) => Err(error.clone()),
                    };
                    let disconnected = reply.as_ref().is_err_and(|error| error.disconnected);
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
        })
    }

    pub(crate) fn submit(&self, operation: Operation) -> io::Result<Receiver<Result<Arc<Reply>>>> {
        submit(&self.commands, operation)
    }

    pub(crate) fn editor(&self) -> Option<RemoteEditor> {
        (self.editor_policy != RemoteEditorPolicy::Deny && !self.is_closed()).then(|| {
            RemoteEditor {
                commands: self.commands.clone(),
                control: Arc::clone(&self.control),
                policy: self.editor_policy,
            }
        })
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

impl Drop for Worker {
    fn drop(&mut self) {
        self.abort();
    }
}

#[derive(Debug, Clone)]
pub struct Failure {
    pub(crate) message: String,
    pub(crate) disconnected: bool,
}

impl Failure {
    pub(crate) fn disconnected(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            disconnected: true,
        }
    }

    pub(super) fn context(mut self, note: impl AsRef<str>) -> Self {
        self.message.push_str(note.as_ref());
        self
    }
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self {
            message: files::display(&error.to_string()),
            disconnected: error.connection_lost(),
        }
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self {
            message: files::display(&error.to_string()),
            disconnected: false,
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
    OpenEditor(Vec<u8>),
    OpenTerminalEditor(Vec<u8>),
    SaveEditor {
        document: RemoteText,
        text: Arc<str>,
    },
    Copy {
        transfer: Transfer,
        destination: Destination,
        progress: Progress,
    },
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
}

#[derive(Debug)]
pub enum Reply {
    Initial {
        local: Arc<Directory>,
        remote: Arc<Directory>,
        home: Vec<u8>,
    },
    Listed(Arc<Directory>),
    EditorOpened(RemoteText),
    EditorSaved(RemoteText),
    Changed {
        directory: Option<Arc<Directory>>,
        note: String,
    },
    Conflict(Conflict),
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
        self.can_replace
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
}

impl Progress {
    pub(crate) fn new(total: Option<u64>) -> Self {
        Self {
            bytes: Arc::new(AtomicU64::new(0)),
            total,
        }
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
        let read = self.1.read(output)?;
        self.0.bytes.fetch_add(read as u64, Ordering::Relaxed);
        Ok(read)
    }
}

impl<T: Write> Write for Counted<'_, T> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
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
        Operation::OpenEditor(path) if editor_policy != RemoteEditorPolicy::Deny => {
            Ok(Reply::EditorOpened(client.open_text(&path)?))
        }
        Operation::OpenTerminalEditor(path) if editor_policy != RemoteEditorPolicy::Deny => {
            let path = terminal_editor_path(client, &path)?;
            Ok(Reply::EditorOpened(client.open_text(&path)?))
        }
        Operation::SaveEditor { document, text } if editor_policy == RemoteEditorPolicy::Allow => {
            let saved = client.save_text(&document, text, || control.is_closed())?;
            Ok(Reply::EditorSaved(saved))
        }
        Operation::OpenEditor(_)
        | Operation::OpenTerminalEditor(_)
        | Operation::SaveEditor { .. } => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Application policy forbids this remote editor action",
        )
        .into()),
        Operation::Copy {
            transfer,
            destination,
            progress,
        } => copy(client, transfer, destination, &progress, control),
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
    }
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

fn copy(
    client: &mut Client,
    transfer: Transfer,
    destination: Destination,
    progress: &Progress,
    control: &Control,
) -> Result<Reply> {
    let target = destination_path(client, &transfer, &destination)?;
    if let Destination::Check = destination
        && let Some(existing) = existing(client, &target)?
    {
        let can_replace = transfer.kind == EntryKind::File
            && match &existing {
                Stamp::Local(stamp) => cfg!(unix) && stamp.kind == EntryKind::File,
                Stamp::Remote(attrs) => {
                    attrs.kind() == FileKind::File && client.supports_atomic_replace()
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
            control.is_closed()
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
            upload(client, source, target, None, progress)?
        }
        (Location::Local(source), Location::Remote(target), Some(Stamp::Remote(stamp))) => {
            upload(client, source, target, Some(*stamp), progress)?
        }
        (Location::Remote(source), Location::Local(target), None) => {
            download(client, source, target, None, progress)?
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

fn unchanged(previous: Attributes, current: Attributes) -> bool {
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
            client.replace(&staging, target)?;
        } else {
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
