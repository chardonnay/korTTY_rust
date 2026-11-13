//! Read-only text snapshots on the chat's authenticated SSH transport.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kortty_ssh::sftp::{Attributes, Error as SftpError, FileKind, Sftp, StatusCode};
use kortty_ssh::{ConnectError, HostKey};
use zeroize::Zeroizing;

use crate::clipboard_policy::RemoteEditorPolicy;
use crate::session::SessionId;
use crate::ssh_connection::{Abort, SftpOpener};
use crate::terminal_logging::Redactor;
use crate::workspace::{SshTarget, Workspace};

use super::super::Chat;
use super::{MAX_BYTES, Snapshot};

pub(in crate::workspace::terminal_ai) mod browser;

const IO_TIMEOUT: Duration = Duration::from_secs(3);
const READ_DEADLINE: Duration = Duration::from_secs(15);
const MAX_PATH_BYTES: usize = 4096;
const MAX_READS: usize = 4;
static READS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Eq, PartialEq)]
struct Source {
    session: SessionId,
    stream: u64,
    target: SshTarget,
    host_key: HostKey,
}

pub(in crate::workspace::terminal_ai) struct Access {
    source: Source,
    opener: SftpOpener,
}

pub(super) struct Version {
    source: Source,
    revoked: AtomicBool,
    requested: Vec<u8>,
    canonical: Vec<u8>,
    attributes: Attributes,
    digest: Vec<u8>,
}

struct ReadFile {
    canonical: Vec<u8>,
    attributes: Attributes,
    bytes: Zeroizing<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReadFailure {
    Unavailable,
    Missing,
    Denied,
    Unsupported,
    Changed,
    TooLarge,
    InvalidPath,
    Timeout,
    Cancelled,
}

impl ReadFailure {
    fn message(self) -> &'static str {
        match self {
            Self::Unavailable => "The original SFTP connection is unavailable.",
            Self::Missing => "The remote attachment file was not found.",
            Self::Denied => "The SFTP server denied access to the remote attachment.",
            Self::Unsupported => {
                "The server does not provide a readable regular file with known SFTP metadata."
            }
            Self::Changed => {
                "The remote attachment or its parent directory changed. Select a current snapshot."
            }
            Self::TooLarge => "The remote file exceeds the attachment size limit.",
            Self::InvalidPath => "The SFTP server did not return a valid absolute UTF-8 path.",
            Self::Timeout => "The remote attachment read timed out.",
            Self::Cancelled => "The remote attachment read was canceled.",
        }
    }
}

impl From<io::Error> for ReadFailure {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::PermissionDenied => Self::Denied,
            io::ErrorKind::TimedOut => Self::Timeout,
            io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted => Self::Cancelled,
            _ => Self::Unavailable,
        }
    }
}

impl From<SftpError> for ReadFailure {
    fn from(error: SftpError) -> Self {
        match error {
            SftpError::Status(status) => match status.code {
                StatusCode::NoSuchFile => Self::Missing,
                StatusCode::PermissionDenied => Self::Denied,
                StatusCode::Unsupported => Self::Unsupported,
                _ => Self::Unavailable,
            },
            SftpError::SourceChanged | SftpError::EditConflict(_) => Self::Changed,
            SftpError::Unsupported(_) | SftpError::Version(_) => Self::Unsupported,
            SftpError::Transport(error) | SftpError::LocalIo(error) => error.into(),
            _ => Self::Unavailable,
        }
    }
}

#[derive(Default)]
pub(super) struct Control {
    cancelled: AtomicBool,
    abort: Mutex<Option<Abort>>,
}

pub(in crate::workspace::terminal_ai) struct Cancel(Arc<Control>);

impl Cancel {
    pub(super) fn new() -> (Self, Arc<Control>) {
        let control = Arc::new(Control::default());
        (Self(Arc::clone(&control)), control)
    }
}

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.cancelled.store(true, Ordering::Release);
        let abort = self
            .0
            .abort
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(abort) = abort {
            abort.abort();
        }
    }
}

pub(in crate::workspace::terminal_ai) struct Pending<T = Arc<Snapshot>> {
    pub(in crate::workspace::terminal_ai) reply: async_channel::Receiver<Result<T, String>>,
    pub(in crate::workspace::terminal_ai) done: async_channel::Receiver<()>,
    pub(in crate::workspace::terminal_ai) cancel: Cancel,
}

impl Workspace {
    pub(in crate::workspace::terminal_ai) fn remote_attachment_access(
        &self,
        chat: &Chat,
    ) -> Option<Access> {
        if self.exiting
            || self.scheduler_draining
            || !self.ai_chat_enabled()
            || !self.clipboard.policy.file_transfer_allowed
            || !self.clipboard.policy.ai_memory.external_files_allowed()
            || self.clipboard.policy.remote_editor == RemoteEditorPolicy::Deny
        {
            return None;
        }
        let session = chat.source_pane?;
        let pane = self.terminal_chat_source(chat)?;
        if pane.session.frame().password_prompt {
            return None;
        }
        self.remote_shell_file_context(session)?;
        let (_, opener) = self.remote_pane(session)?;
        let source = Source {
            session,
            stream: pane.stream_generation,
            target: pane.ssh.clone()?,
            host_key: opener.verified_host_key()?.clone(),
        };
        Some(Access { source, opener })
    }
}

impl Access {
    pub(in crate::workspace::terminal_ai) fn start(
        self,
        path: &str,
        selected: Option<&browser::Selection>,
        limit: usize,
        redactor: Arc<Redactor>,
    ) -> Result<Pending, String> {
        let requested = absolute_path(path)?.to_vec();
        let expected = selected
            .map(|selection| selection.expected(&self, path))
            .transpose()?;
        if limit == 0 {
            return Err("The current selection leaves no room for an attachment.".into());
        }
        start(move |control| {
            self.load(requested, expected, limit, &redactor, control)
                .map(Arc::new)
        })
    }

    fn load(
        self,
        requested: Vec<u8>,
        expected: Option<Attributes>,
        limit: usize,
        redactor: &Redactor,
        control: Arc<Control>,
    ) -> Result<Snapshot, String> {
        let read = self
            .read(&requested, expected, limit, control)
            .map_err(ReadFailure::message)?;
        let text = super::text(&read.bytes).map_err(|error| error.to_string())?;
        let canonical = std::str::from_utf8(&read.canonical)
            .map_err(|_| "The canonical remote path is not UTF-8.")?;
        let name = canonical
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .ok_or("Choose a regular remote file.")?
            .to_owned();
        let label = format!("{} · {}", self.source.target.label(), canonical);
        let digest = super::digest(&read.bytes).map_err(|error| error.to_string())?;
        Snapshot::remote(
            Version {
                source: self.source,
                revoked: AtomicBool::new(false),
                requested,
                canonical: read.canonical,
                attributes: read.attributes,
                digest,
            },
            &name,
            &label,
            &text,
            redactor,
        )
    }

    fn read(
        &self,
        requested: &[u8],
        expected: Option<Attributes>,
        limit: usize,
        control: Arc<Control>,
    ) -> Result<ReadFile, ReadFailure> {
        self.with_client(control, |client| {
            read_file(client, requested, expected, limit)
        })
    }

    fn with_client<T>(
        &self,
        control: Arc<Control>,
        action: impl FnOnce(&mut Client) -> Result<T, ReadFailure>,
    ) -> Result<T, ReadFailure> {
        let deadline = Instant::now() + READ_DEADLINE;
        let check = || check_active(&control, deadline);
        let mut channel = self
            .opener
            .open_cancellable(|| check().is_err())
            .map_err(|error| match error {
                ConnectError::RequestRejected { .. } => ReadFailure::Unsupported,
                ConnectError::Io(error) => error.into(),
                _ => ReadFailure::Unavailable,
            })?;
        {
            let mut active = control
                .abort
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            check()?;
            *active = Some(channel.abort_handle());
        }
        let reader = channel
            .take_timed_reader(IO_TIMEOUT)
            .ok_or(ReadFailure::Unavailable)?;
        let writer = channel
            .take_timed_writer(IO_TIMEOUT)
            .ok_or(ReadFailure::Unavailable)?;
        let reader = BoundedIo {
            inner: reader,
            control: Arc::clone(&control),
            deadline,
        };
        let writer = BoundedIo {
            inner: writer,
            control: Arc::clone(&control),
            deadline,
        };
        let mut client = Sftp::new(reader, writer)?;
        let result = action(&mut client)?;
        check()?;
        if self.opener.is_closed() {
            return Err(ReadFailure::Unavailable);
        }
        Ok(result)
    }
}

type Client = Sftp<BoundedIo<Box<dyn Read + Send>>, BoundedIo<Box<dyn Write + Send>>>;

fn start<T: Send + 'static>(
    work: impl FnOnce(Arc<Control>) -> Result<T, String> + Send + 'static,
) -> Result<Pending<T>, String> {
    let permit = Permit::acquire()?;
    let (cancel, control) = Cancel::new();
    let (send, reply) = async_channel::bounded(1);
    let (finished, done) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("kortty-chat-sftp".into())
        .spawn(move || {
            let _ = send.try_send(work(control));
            drop(permit);
            drop(finished);
        })
        .map_err(|_| "Could not start the SSH attachment worker.")?;
    Ok(Pending {
        reply,
        done,
        cancel,
    })
}

impl Version {
    pub(super) fn matches(&self, current: &Access) -> bool {
        !self.revoked.load(Ordering::Acquire)
            && self.source == current.source
            && !current.opener.is_closed()
    }

    pub(super) fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
    }

    pub(super) fn require_active(&self, access: &Access, control: &Control) -> Result<(), String> {
        if self.matches(access) && !control.cancelled.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err("The original SSH attachment source is unavailable. Review a new snapshot.".into())
        }
    }

    pub(super) fn require_current(
        &self,
        access: &Access,
        control: &Arc<Control>,
    ) -> Result<(), String> {
        let _permit = Permit::acquire()?;
        self.require_active(access, control)?;
        let current = access
            .read(&self.requested, None, MAX_BYTES, Arc::clone(control))
            .map_err(ReadFailure::message)?;
        if current.canonical != self.canonical
            || current.attributes != self.attributes
            || super::digest(&current.bytes).map_err(|error| error.to_string())? != self.digest
        {
            return Err(
                "The attached remote file changed after review. Select a current snapshot.".into(),
            );
        }
        self.require_active(access, control)
    }
}

fn absolute_path(path: &str) -> Result<&[u8], String> {
    let relative = path
        .strip_prefix('/')
        .ok_or("Enter an absolute SFTP file path.")?;
    if path.len() > MAX_PATH_BYTES
        || path.chars().any(char::is_control)
        || relative
            .split('/')
            .any(|component| matches!(component, "" | "." | ".."))
    {
        return Err("Enter an absolute SFTP file path with ordinary path components.".into());
    }
    Ok(path.as_bytes())
}

fn read_file(
    client: &mut Sftp<impl Read, impl Write>,
    requested: &[u8],
    expected: Option<Attributes>,
    limit: usize,
) -> Result<ReadFile, ReadFailure> {
    let last_slash = requested
        .iter()
        .rposition(|&byte| byte == b'/')
        .ok_or(ReadFailure::InvalidPath)?;
    let parent = &requested[..last_slash.max(1)];
    let basename = &requested[last_slash + 1..];
    let canonical_parent = client.canonicalize(parent)?;
    let mut canonical = canonical_parent.clone();
    if !canonical.ends_with(b"/") {
        canonical.push(b'/');
    }
    canonical.extend_from_slice(basename);
    let path = std::str::from_utf8(&canonical).map_err(|_| ReadFailure::InvalidPath)?;
    absolute_path(path).map_err(|_| ReadFailure::InvalidPath)?;
    let mut attributes = client.lstat(&canonical)?;
    if let Some(mut expected) = expected {
        expected.accessed = attributes.accessed;
        if canonical != requested || expected != attributes {
            return Err(ReadFailure::Changed);
        }
    }
    let size = attributes.size.ok_or(ReadFailure::Unsupported)?;
    if attributes.kind() != FileKind::File || attributes.modified.is_none() {
        return Err(ReadFailure::Unsupported);
    }
    if size > limit.min(MAX_BYTES) as u64 {
        return Err(ReadFailure::TooLarge);
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(size as usize));
    client.download_regular(&canonical, attributes, &mut *bytes)?;
    if client.canonicalize(parent)? != canonical_parent {
        return Err(ReadFailure::Changed);
    }
    attributes.accessed = None;
    Ok(ReadFile {
        canonical,
        attributes,
        bytes,
    })
}

fn check_active(control: &Control, deadline: Instant) -> io::Result<()> {
    if control.cancelled.load(Ordering::Acquire) {
        return Err(io::ErrorKind::ConnectionAborted.into());
    }
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(())
}

struct BoundedIo<T> {
    inner: T,
    control: Arc<Control>,
    deadline: Instant,
}

impl<T: Read> Read for BoundedIo<T> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        check_active(&self.control, self.deadline)?;
        self.inner.read(bytes)
    }
}

impl<T: Write> Write for BoundedIo<T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        check_active(&self.control, self.deadline)?;
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        check_active(&self.control, self.deadline)?;
        self.inner.flush()
    }
}

struct Permit;

impl Permit {
    fn acquire() -> Result<Self, String> {
        READS
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_READS).then_some(count + 1)
            })
            .map(|_| Self)
            .map_err(|_| "Too many SSH attachment reads are pending.".into())
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        READS.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
pub(in crate::workspace::terminal_ai) fn fixture_snapshot(
    session: SessionId,
    stream: u64,
    redactor: &Redactor,
) -> Arc<Snapshot> {
    use crate::session_security::IsolationLevel;
    use crate::ssh_dialog::LoginTarget;

    let path = b"/fixture/source.txt".to_vec();
    let contents = b"remote fixture";
    let target = SshTarget::from_imported_route(
        LoginTarget {
            host: "127.0.0.1".into(),
            port: 22,
            username: "fixture".into(),
        },
        None,
        IsolationLevel::None,
    )
    .unwrap();
    let host_key = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKuSNsxvt1EJ0ELnEMXiOgkaExGvVly+uWvDAKyIGKcX",
    )
    .unwrap();
    Arc::new(
        Snapshot::remote(
            Version {
                source: Source {
                    session,
                    stream,
                    target,
                    host_key,
                },
                revoked: AtomicBool::new(false),
                requested: path.clone(),
                canonical: path,
                attributes: Attributes::default(),
                digest: super::digest(contents).unwrap(),
            },
            "source.txt",
            "fixture · /fixture/source.txt",
            std::str::from_utf8(contents).unwrap(),
            redactor,
        )
        .unwrap(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn attachment_paths_are_absolute_and_keep_the_user_selected_bytes() {
        assert_eq!(
            absolute_path("/work/Grüße.txt").unwrap(),
            "/work/Grüße.txt".as_bytes()
        );
        for path in [
            "notes.txt",
            "~/notes",
            "/",
            "/dir/../notes",
            "/dir//notes",
            "/dir/notes\n",
        ] {
            assert!(absolute_path(path).is_err(), "{path:?}");
        }
    }

    #[test]
    fn remote_read_checks_the_file_and_both_parent_resolutions() {
        let contents = b"file evidence\n";
        for after in [b"/resolved".as_slice(), b"/changed".as_slice()] {
            let mut wire = version();
            name(&mut wire, 1, b"/resolved");
            let attributes = attributes(contents.len() as u64, 0o100600);
            for id in [2, 3] {
                packet(&mut wire, 105, id, &attributes);
            }
            packet(&mut wire, 102, 4, &string(b"handle"));
            for id in [5, 6] {
                packet(&mut wire, 105, id, &attributes);
            }
            packet(&mut wire, 103, 7, &string(contents));
            status(&mut wire, 8, 1);
            packet(&mut wire, 105, 9, &attributes);
            status(&mut wire, 10, 0);
            packet(&mut wire, 105, 11, &attributes);
            name(&mut wire, 12, after);
            let mut sftp = Sftp::new(Cursor::new(wire), io::sink()).unwrap();
            let result = read_file(&mut sftp, b"/work/source.txt", None, MAX_BYTES);
            if after == b"/resolved" {
                let file = result.unwrap();
                assert_eq!(file.canonical, b"/resolved/source.txt");
                assert_eq!(file.bytes.as_slice(), contents);
                assert_eq!(file.attributes.accessed, None);
            } else {
                assert_eq!(result.err(), Some(ReadFailure::Changed));
            }
        }
    }

    #[test]
    fn remote_read_rejects_links_and_excess_size_before_opening() {
        for (size, mode, error) in [
            (20, 0o120777, ReadFailure::Unsupported),
            ((MAX_BYTES + 1) as u64, 0o100600, ReadFailure::TooLarge),
        ] {
            let mut wire = version();
            name(&mut wire, 1, b"/resolved");
            packet(&mut wire, 105, 2, &attributes(size, mode));
            let mut sftp = Sftp::new(Cursor::new(wire), io::sink()).unwrap();
            assert_eq!(
                read_file(&mut sftp, b"/work/source.txt", None, MAX_BYTES).err(),
                Some(error)
            );
        }
    }

    #[test]
    fn browser_choice_refuses_a_changed_path_or_listing_version_before_reading() {
        for (resolved, size) in [
            (b"/redirected".as_slice(), 12),
            (b"/reviewed".as_slice(), 13),
        ] {
            let mut wire = version();
            name(&mut wire, 1, resolved);
            packet(&mut wire, 105, 2, &attributes(size, 0o100600));
            let mut sftp = Sftp::new(Cursor::new(wire), io::sink()).unwrap();
            let expected = Attributes {
                size: Some(12),
                permissions: Some(0o100600),
                modified: Some(16),
                ..Attributes::default()
            };
            assert_eq!(
                read_file(
                    &mut sftp,
                    b"/reviewed/source.txt",
                    Some(expected),
                    MAX_BYTES
                )
                .err(),
                Some(ReadFailure::Changed)
            );
        }
    }

    #[test]
    fn remote_errors_use_fixed_status_without_server_text() {
        use kortty_ssh::sftp::RemoteStatus;

        let failure = ReadFailure::from(SftpError::Status(RemoteStatus {
            code: StatusCode::PermissionDenied,
            message: "Untrusted server status".into(),
        }));
        assert_eq!(failure, ReadFailure::Denied);
        assert_eq!(
            failure.message(),
            "The SFTP server denied access to the remote attachment."
        );
    }

    pub(super) fn version() -> Vec<u8> {
        vec![0, 0, 0, 5, 2, 0, 0, 0, 3]
    }

    pub(super) fn string(value: &[u8]) -> Vec<u8> {
        let mut payload = (value.len() as u32).to_be_bytes().to_vec();
        payload.extend_from_slice(value);
        payload
    }

    pub(super) fn name(wire: &mut Vec<u8>, id: u32, path: &[u8]) {
        let mut payload = 1_u32.to_be_bytes().to_vec();
        payload.extend(string(path));
        payload.extend(string(b""));
        payload.extend(0_u32.to_be_bytes());
        packet(wire, 104, id, &payload);
    }

    pub(super) fn status(wire: &mut Vec<u8>, id: u32, code: u32) {
        let mut payload = code.to_be_bytes().to_vec();
        payload.extend(string(b""));
        payload.extend(string(b""));
        packet(wire, 101, id, &payload);
    }

    pub(super) fn attributes(size: u64, mode: u32) -> Vec<u8> {
        let mut payload = 13_u32.to_be_bytes().to_vec();
        payload.extend(size.to_be_bytes());
        payload.extend(mode.to_be_bytes());
        payload.extend(15_u32.to_be_bytes());
        payload.extend(16_u32.to_be_bytes());
        payload
    }

    pub(super) fn packet(wire: &mut Vec<u8>, kind: u8, id: u32, payload: &[u8]) {
        wire.extend((5 + payload.len() as u32).to_be_bytes());
        wire.push(kind);
        wire.extend(id.to_be_bytes());
        wire.extend_from_slice(payload);
    }
}
