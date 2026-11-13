//! Reviewed same-server copy over one authenticated SFTP channel.

use std::sync::Weak;

use crate::workspace::SshTarget;

use super::sync::{Cancel, RemoteStage, Report, review_root};
use super::*;

mod folder;

const MAX_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_COMPONENTS: usize = 64;

struct Route {
    target: SshTarget,
    peer: HostKey,
    owner: Weak<Control>,
}

impl Route {
    fn bound(&self, control: &Control) -> bool {
        std::ptr::eq(self.owner.as_ptr(), control)
            && control.host_key.as_ref() == Some(&self.peer)
            && !control.is_closed()
    }
}

pub(crate) struct Request {
    source: Vec<u8>,
    destination: Vec<u8>,
    route: Route,
}

impl Request {
    pub(in crate::sftp_window) fn new(
        source: Vec<u8>,
        destination: Vec<u8>,
        target: SshTarget,
        worker: &Worker,
    ) -> io::Result<Self> {
        let peer = worker
            .verified_host_key()
            .filter(|_| !worker.is_closed())
            .cloned()
            .ok_or_else(|| io::Error::other("A verified live SSH connection is required"))?;
        Ok(Self {
            source,
            destination,
            route: Route {
                target,
                peer,
                owner: Arc::downgrade(&worker.control),
            },
        })
    }
}

pub struct Plan {
    route: Route,
    source: Vec<u8>,
    destination: Vec<u8>,
    parents: [Folder; 2],
    contents: Contents,
    labels: [String; 2],
    bytes: u64,
}

enum Contents {
    File(Attributes),
    Folder(folder::Plan),
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerCopyPlan")
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

impl Plan {
    pub(in crate::sftp_window) fn labels(&self) -> &[String; 2] {
        &self.labels
    }

    pub(in crate::sftp_window) fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(in crate::sftp_window) fn folder_review(&self) -> Option<(usize, &[(String, bool)])> {
        match &self.contents {
            Contents::File(_) => None,
            Contents::Folder(plan) => Some((plan.files, &plan.entries)),
        }
    }

    pub(in crate::sftp_window) fn applies_to(&self, worker: &Worker, target: &SshTarget) -> bool {
        self.route.target == *target && self.route.bound(&worker.control)
    }
}

pub(super) fn prepare(
    client: &mut Client,
    request: Request,
    cancel: &Cancel,
    control: &Control,
) -> Result<Plan> {
    let stopped = || cancel.stopped() || control.is_closed();
    let result = prepare_checked(client, request, control, &stopped);
    cancel.finish();
    result
}

fn prepare_checked(
    client: &mut Client,
    request: Request,
    control: &Control,
    stopped: &impl Fn() -> bool,
) -> Result<Plan> {
    if !request.route.bound(control) {
        return Err(changed_route());
    }
    supported(client)?;
    let Request {
        source,
        destination: folder,
        route,
    } = request;
    let slash = source
        .iter()
        .rposition(|&byte| byte == b'/')
        .ok_or_else(|| io::Error::other("Choose an absolute source path"))?;
    let name = &source[slash + 1..];
    files::validate_transfer_name(name)?;
    let source_parent = source[..slash.max(1)].to_vec();
    let destination = files::remote_join(&folder, name);
    if source == destination
        || destination
            .strip_prefix(source.as_slice())
            .is_some_and(|rest| rest.starts_with(b"/"))
    {
        return Err(io::Error::other("Choose a destination outside the source path").into());
    }
    let labels = [
        review_root(&Location::Remote(source.clone()))?,
        review_root(&Location::Remote(destination.clone()))?,
    ];
    let target_folder = Folder::read(client, folder, stopped)?;
    let source_folder = Folder::read(client, source_parent, stopped)?;
    let owner = target_folder.owner();
    if !source_folder.trusted(owner) || !target_folder.trusted(owner) {
        return Err(unsafe_parent().into());
    }
    if client.canonicalize(&source)? != source {
        return Err(io::Error::other("The remote source is not canonical").into());
    }
    absent(client, &destination)?;
    let source_stamp = client.lstat(&source)?;
    let (contents, bytes) = if source_stamp.kind() == FileKind::Directory {
        let (tree, bytes) = folder::prepare(client, &source, &destination, owner, stopped)?;
        (Contents::Folder(tree), bytes)
    } else {
        let bytes = protected_file(source_stamp, owner)?;
        (Contents::File(source_stamp), bytes)
    };
    source_folder.verify(client, true)?;
    target_folder.verify(client, true)?;
    absent(client, &destination)?;
    running(stopped)?;
    Ok(Plan {
        route,
        source,
        destination,
        parents: [source_folder, target_folder],
        contents,
        labels,
        bytes,
    })
}

pub(crate) struct Folder {
    path: Vec<u8>,
    stamps: Vec<(usize, Attributes)>,
}

impl Folder {
    pub(crate) fn read(
        client: &mut Client,
        path: Vec<u8>,
        stopped: &impl Fn() -> bool,
    ) -> Result<Self> {
        if !path.starts_with(b"/") {
            return Err(io::Error::other("Choose an absolute canonical server directory").into());
        }
        let parts = path[1..].split(|&byte| byte == b'/');
        let mut lengths = vec![1];
        let mut end = 0;
        if path != b"/" {
            for part in parts {
                if lengths.len() > MAX_COMPONENTS {
                    return Err(unsafe_parent().into());
                }
                files::validate_transfer_name(part)?;
                end += 1 + part.len();
                lengths.push(end);
            }
        }
        running(stopped)?;
        if client.canonicalize(&path)? != path {
            return Err(io::Error::other(
                "The server directory must be canonical and contain no links",
            )
            .into());
        }
        let mut stamps = Vec::with_capacity(lengths.len());
        for length in lengths {
            running(stopped)?;
            let attrs = client.lstat(&path[..length])?;
            if attrs.kind() != FileKind::Directory
                || attrs.uid.is_none()
                || attrs.gid.is_none()
                || attrs.permissions.is_none()
            {
                return Err(unsafe_parent().into());
            }
            stamps.push((length, attrs));
        }
        Ok(Self { path, stamps })
    }

    pub(crate) fn owner(&self) -> u32 {
        self.stamps
            .last()
            .and_then(|(_, stamp)| stamp.uid)
            .expect("checked owner")
    }

    pub(crate) fn trusted(&self, owner: u32) -> bool {
        self.stamps.iter().all(|(length, attrs)| {
            let uid = attrs.uid.expect("checked owner");
            let mode = attrs.permissions.expect("checked permissions");
            (uid == 0 || uid == owner)
                && (mode & 0o022 == 0
                    || *length != self.path.len() && uid == 0 && mode & 0o1000 != 0)
        })
    }

    pub(crate) fn verify(&self, client: &mut Client, strict: bool) -> Result<()> {
        if client.canonicalize(&self.path)? != self.path {
            return Err(unsafe_parent().into());
        }
        for &(length, previous) in &self.stamps {
            let current = client.lstat(&self.path[..length])?;
            if current.kind() != FileKind::Directory
                || current.uid != previous.uid
                || current.gid != previous.gid
                || current.permissions != previous.permissions
                || strict && length == self.path.len() && !unchanged(previous, current)
            {
                return Err(unsafe_parent().into());
            }
        }
        Ok(())
    }
}

pub(super) fn apply(
    client: &mut Client,
    plan: &Plan,
    progress: &Progress,
    cancel: &Cancel,
    control: &Control,
) -> Report {
    let stopped = || cancel.stopped() || control.is_closed();
    let result = if plan.route.bound(control) {
        let disconnected = || control.is_closed();
        match &plan.contents {
            Contents::File(stamp) => {
                apply_checked(client, plan, *stamp, progress, &stopped, &disconnected)
            }
            Contents::Folder(tree) => {
                folder::apply(client, plan, tree, progress, &stopped, &disconnected)
            }
        }
    } else {
        Err(changed_route())
    };
    cancel.finish();
    match result {
        Ok((note, disconnected)) => Report {
            note,
            disconnected: disconnected || control.is_closed(),
            directory: None,
        },
        Err(error) => Report {
            note: format!(
                "{} · {}",
                if cancel.stopped() {
                    "Server copy cancelled"
                } else {
                    "Server copy stopped"
                },
                error.message,
            ),
            disconnected: error.disconnected || control.is_closed(),
            directory: None,
        },
    }
}

fn apply_checked(
    client: &mut Client,
    plan: &Plan,
    source_stamp: Attributes,
    progress: &Progress,
    stopped: &impl Fn() -> bool,
    disconnected: &impl Fn() -> bool,
) -> Result<(String, bool)> {
    supported(client)?;
    let [source, target] = &plan.parents;
    running(stopped)?;
    source.verify(client, true)?;
    target.verify(client, true)?;
    source_current(client, &plan.source, source_stamp)?;
    absent(client, &plan.destination)?;
    running(stopped)?;
    let stage = RemoteStage::claim(client, &target.path, target.owner())?;
    let mut publishing = false;
    let result = (|| -> Result<()> {
        running(stopped)?;
        client.copy_regular_new_with_progress(
            &plan.source,
            source_stamp,
            &stage.payload,
            stopped,
            |bytes| progress.bytes.store(bytes, Ordering::Relaxed),
        )?;
        running(stopped)?;
        source.verify(client, false)?;
        target.verify(client, false)?;
        source_current(client, &plan.source, source_stamp)?;
        stage.verify(client)?;
        let payload = client.lstat(&stage.payload)?;
        if !private_payload(payload, &stage) || payload.size != Some(plan.bytes) {
            return Err(io::Error::other("The private copy payload changed").into());
        }
        running(stopped)?;
        publishing = true;
        client.hardlink(&stage.payload, &plan.destination)?;
        Ok(())
    })();
    let cleanup = (!disconnected()
        && result
            .as_ref()
            .err()
            .is_none_or(|error| !error.disconnected))
    .then(|| cleanup(client, target, &stage, plan.bytes));
    let cleaned = matches!(&cleanup, Some(Ok(())));
    let cleanup_disconnected = matches!(&cleanup, Some(Err(error)) if error.disconnected);
    match result {
        Ok(()) if cleaned => Ok((format!("Copied {} bytes on the server", plan.bytes), false)),
        Ok(()) => Ok((
            format!(
                "Copied {} bytes on the server · Check private staging path {}",
                plan.bytes,
                stage.label(),
            ),
            cleanup_disconnected,
        )),
        Err(error) if cleaned => Err(error),
        Err(mut error) => {
            let note = if publishing && (error.disconnected || disconnected()) {
                " · Server publication could not be confirmed"
            } else {
                ""
            };
            error.disconnected |= cleanup_disconnected;
            Err(error.context(format!(
                "{note} · Check private staging path {}",
                stage.label()
            )))
        }
    }
}

fn cleanup(client: &mut Client, target: &Folder, stage: &RemoteStage, limit: u64) -> Result<()> {
    target.verify(client, false)?;
    stage.verify(client)?;
    if let Some(payload) = remote_existing(client, &stage.payload)?
        && (!private_payload(payload, stage) || payload.size.is_none_or(|size| size > limit))
    {
        return Err(
            io::Error::other("The private copy payload could not be verified for cleanup").into(),
        );
    }
    stage.remove(client)
}

fn private_payload(attrs: Attributes, stage: &RemoteStage) -> bool {
    attrs.kind() == FileKind::File
        && attrs.uid == Some(stage.uid)
        && attrs.gid == Some(stage.gid)
        && attrs.permissions.is_some_and(|mode| mode & 0o7777 == 0o600)
}

fn source_current(client: &mut Client, source: &[u8], stamp: Attributes) -> Result<()> {
    if !unchanged(stamp, client.lstat(source)?) || client.canonicalize(source)? != source {
        return Err(Error::SourceChanged.into());
    }
    Ok(())
}

fn protected(attributes: Attributes, owner: u32) -> bool {
    attributes.modified.is_some()
        && attributes.uid.is_some_and(|uid| uid == 0 || uid == owner)
        && attributes.gid.is_some()
        && attributes.permissions.is_some_and(|mode| mode & 0o022 == 0)
}

fn protected_file(attributes: Attributes, owner: u32) -> io::Result<u64> {
    attributes
        .size
        .filter(|&size| size <= MAX_BYTES)
        .filter(|_| attributes.kind() == FileKind::File && protected(attributes, owner))
        .ok_or_else(|| io::Error::other("Choose a protected regular server file of at most 1 GiB"))
}

fn absent(client: &mut Client, target: &[u8]) -> Result<()> {
    if remote_existing(client, target)?.is_some() {
        return Err(io::Error::other("The server copy destination already exists").into());
    }
    Ok(())
}

fn running(stopped: &impl Fn() -> bool) -> io::Result<()> {
    if stopped() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Server copy stopped before publication",
        ));
    }
    Ok(())
}

fn supported(client: &Client) -> Result<()> {
    if !client.supports_copy_data() || !client.supports_hardlink() {
        return Err(io::Error::other(
            "Server copy requires SFTP copy-data and no-replace hard-link support",
        )
        .into());
    }
    Ok(())
}

fn unsafe_parent() -> io::Error {
    io::Error::other("Server copy requires protected canonical source and destination directories")
}

fn changed_route() -> Failure {
    io::Error::other("The reviewed SSH connection changed").into()
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, sink};

    use super::*;

    #[test]
    fn server_copy_requires_both_advertised_extensions() {
        let handshake: [u8; 9] = [0, 0, 0, 5, 2, 0, 0, 0, 3];
        let input: Box<dyn Read + Send> = Box::new(Cursor::new(handshake));
        let output: Box<dyn Write + Send> = Box::new(sink());
        let client = Sftp::new(input, output).unwrap();
        let error = supported(&client).unwrap_err();
        assert!(error.message.contains("copy-data and no-replace hard-link"));
    }
}
