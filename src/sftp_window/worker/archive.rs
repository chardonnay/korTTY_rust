//! Review and publish an archive over the same authenticated SFTP connection.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::io::Cursor;
use std::sync::Weak;

use kortty_server_archive::protocol::{self, CreateReview, CreateSpec};
use kortty_ssh::sftp::{UploadMetadata, UploadOwner, UploadStage};

use crate::job_executor::{BrowserArchiveManifest, scan_browser_archive};
use crate::scheduler_window::{Archive, ArchiveFormat, MAX_ARCHIVE_SOURCES, RemotePath};

use super::ownership::{principals::Selection, resolve_archive_selection};
use super::server_copy::Folder;
use super::sync::{Cancel, RemoteStage, review_root};
use super::*;

pub(in crate::sftp_window) mod local;

pub(in crate::sftp_window) struct Options {
    pub name: String,
    pub format: ArchiveFormat,
    pub level: u8,
    pub excludes: Vec<String>,
}

pub(crate) struct Request {
    directory: Vec<u8>,
    selected: Vec<Entry>,
    archive: Archive,
    target: SshTarget,
    peer: HostKey,
    owner: Weak<Control>,
    metadata: Option<MetadataRequest>,
    creation: Option<ServerCreation>,
}

struct ServerCreation {
    opener: ExecOpener,
    spec: CreateSpec,
}

struct MetadataRequest {
    selection: Selection,
    opener: Option<ExecOpener>,
    wait_for_session: bool,
}

impl Request {
    pub(in crate::sftp_window) fn new(
        directory: Vec<u8>,
        selected: Vec<Entry>,
        options: Options,
        target: SshTarget,
        worker: &Worker,
    ) -> io::Result<Self> {
        let peer = worker
            .verified_host_key()
            .filter(|_| !worker.is_closed())
            .cloned()
            .ok_or_else(|| io::Error::other("A verified live SSH connection is required"))?;
        let archive = request_archive(&directory, &selected, options)?;
        Ok(Self {
            directory,
            selected,
            archive,
            target,
            peer,
            owner: Arc::downgrade(&worker.control),
            metadata: None,
            creation: None,
        })
    }

    pub(in crate::sftp_window) fn on_server(mut self, worker: &Worker) -> io::Result<Self> {
        if !self.bound(&worker.control) {
            return Err(stale());
        }
        let opener = worker.exec_opener().ok_or_else(|| {
            io::Error::other("A verified SSH command channel is required for server-only archives")
        })?;
        self.creation = Some(ServerCreation {
            opener,
            spec: self.archive.server_spec().map_err(io::Error::other)?,
        });
        Ok(self)
    }

    pub(super) fn requires_transfer(&self) -> bool {
        self.creation.is_none()
    }

    pub(in crate::sftp_window) fn with_metadata(
        mut self,
        selection: Selection,
        worker: &Worker,
        wait_for_session: bool,
    ) -> io::Result<Self> {
        if !self.bound(&worker.control) {
            return Err(io::Error::other("The reviewed SSH connection changed"));
        }
        self.metadata = if selection.is_empty() {
            None
        } else {
            let opener = selection
                .uses_names()
                .then(|| {
                    worker.exec_opener().ok_or_else(|| {
                        io::Error::other(
                            "A verified SSH command channel is required for server account lookup",
                        )
                    })
                })
                .transpose()?;
            Some(MetadataRequest {
                selection,
                opener,
                wait_for_session,
            })
        };
        Ok(self)
    }

    fn resolve_metadata(
        &self,
        preparing: bool,
        guard: &impl Fn() -> Result<()>,
    ) -> Result<Option<OwnerIds>> {
        self.metadata
            .as_ref()
            .map(|metadata| {
                guard()?;
                resolve_archive_selection(
                    &metadata.selection,
                    metadata.opener.as_ref(),
                    &self.peer,
                    preparing && metadata.wait_for_session,
                    guard,
                )
            })
            .transpose()
    }

    fn bound(&self, control: &Control) -> bool {
        std::ptr::eq(self.owner.as_ptr(), control)
            && control.host_key.as_ref() == Some(&self.peer)
            && !control.is_closed()
    }
}

type OwnerIds = (Option<u32>, Option<u32>);

fn request_archive(directory: &[u8], selected: &[Entry], options: Options) -> io::Result<Archive> {
    if !directory.starts_with(b"/") || !(1..=MAX_ARCHIVE_SOURCES).contains(&selected.len()) {
        return Err(io::Error::other(
            "Select between one and 16 files or folders in an absolute server directory",
        ));
    }
    let mut names = BTreeSet::new();
    let mut sources = Vec::with_capacity(selected.len());
    for entry in selected {
        let EntryName::Remote(name) = &entry.name else {
            return Err(stale());
        };
        if !entry.ownership_reviewable() || !names.insert(name) {
            return Err(stale());
        }
        files::validate_transfer_name(name)?;
        sources.push(remote_path(files::remote_join(directory, name))?);
    }
    let Options {
        name,
        format,
        level,
        excludes,
    } = options;
    let target = if name.starts_with('/') {
        RemotePath::try_from(name).map_err(io::Error::other)?
    } else {
        files::validate_transfer_name(name.as_bytes())?;
        remote_path(files::remote_join(directory, name.as_bytes()))?
    };
    if sources.iter().any(|source| source.contains(&target)) {
        return Err(io::Error::other(
            "Choose an archive destination outside every selected source",
        ));
    }
    let archive = Archive {
        sources,
        target,
        format,
        level,
        excludes,
        password_credential: None,
        download: None,
    };
    archive.validate().map_err(io::Error::other)?;
    Ok(archive)
}

fn remote_path(path: Vec<u8>) -> io::Result<RemotePath> {
    let path = String::from_utf8(path)
        .map_err(|_| io::Error::other("Archive paths must use valid UTF-8"))?;
    RemotePath::try_from(path).map_err(io::Error::other)
}

pub struct Plan {
    request: Request,
    source_folder: Folder,
    destination_folder: Folder,
    contents: Contents,
    destination: String,
    metadata_ids: Option<OwnerIds>,
    used: AtomicBool,
}

enum Contents {
    Client(BrowserArchiveManifest),
    Server {
        review: CreateReview,
        entries: Vec<(String, bool)>,
    },
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerArchivePlan")
            .field("entries", &self.entries().len())
            .finish_non_exhaustive()
    }
}

impl Plan {
    pub(super) fn requires_transfer(&self) -> bool {
        self.request.requires_transfer()
    }

    pub(in crate::sftp_window) fn server_only(&self) -> bool {
        !self.requires_transfer()
    }

    pub(in crate::sftp_window) fn applies_to(&self, worker: &Worker, target: &SshTarget) -> bool {
        self.request.target == *target && self.request.bound(&worker.control)
    }

    pub(in crate::sftp_window) fn destination(&self) -> &str {
        &self.destination
    }

    pub(in crate::sftp_window) fn format(&self) -> ArchiveFormat {
        self.request.archive.format
    }

    pub(in crate::sftp_window) fn entries(&self) -> &[(String, bool)] {
        match &self.contents {
            Contents::Client(manifest) => manifest.entries(),
            Contents::Server { entries, .. } => entries,
        }
    }

    pub(in crate::sftp_window) fn input_bytes(&self) -> u64 {
        match &self.contents {
            Contents::Client(manifest) => manifest.input_bytes(),
            Contents::Server { review, .. } => review.manifest.expanded_bytes,
        }
    }

    pub(in crate::sftp_window) fn excluded_count(&self) -> usize {
        match &self.contents {
            Contents::Client(manifest) => manifest.excluded_count(),
            Contents::Server { review, .. } => review.excluded,
        }
    }

    pub(in crate::sftp_window) fn metadata(&self) -> Option<(&Selection, OwnerIds)> {
        self.request
            .metadata
            .as_ref()
            .zip(self.metadata_ids)
            .map(|(metadata, ids)| (&metadata.selection, ids))
    }
}

pub(super) fn prepare(
    client: &mut Client,
    request: Request,
    cancel: &Cancel,
    control: &Control,
) -> Result<Plan> {
    let stopped = || cancel.stopped() || control.is_closed();
    let result = prepare_checked(client, request, control, cancel, &stopped);
    cancel.finish();
    mark_disconnect(result, control)
}

fn prepare_checked(
    client: &mut Client,
    request: Request,
    control: &Control,
    cancel: &Cancel,
    stopped: &impl Fn() -> bool,
) -> Result<Plan> {
    check_route(&request, control)?;
    if request.requires_transfer() {
        supported(client)?;
    }
    let destination_path = request.archive.target.as_str().as_bytes();
    let destination = review_root(&Location::Remote(destination_path.to_vec()))?;
    let source_folder = Folder::read(client, request.directory.clone(), stopped)?;
    let destination_folder = Folder::read(
        client,
        request.archive.target.parent().as_bytes().to_vec(),
        stopped,
    )?;
    for folder in [&source_folder, &destination_folder] {
        let owner = folder.owner();
        if owner == u32::MAX || !folder.trusted(owner) {
            return Err(io::Error::other(
                "Choose protected canonical server source and destination folders",
            )
            .into());
        }
    }
    verify_roots(client, &request, stopped)?;
    absent(client, destination_path, stopped)?;
    let metadata_ids = request.resolve_metadata(true, &|| {
        ensure_ownership_allowed(control, cancel, Some(&request.target))
    })?;
    let contents = if let Some(creation) = &request.creation {
        let reply = exchange_creation(
            &request,
            creation,
            &protocol::Request::ReviewCreation {
                spec: creation.spec.clone(),
            },
            cancel,
            control,
        )?;
        let protocol::Reply::CreationReview { review } = reply else {
            return Err(creation_reply_failure(reply));
        };
        review.validate_for(&creation.spec)?;
        let entries = review
            .manifest
            .members
            .iter()
            .map(|member| {
                (
                    member.path.clone(),
                    member.kind == kortty_server_archive::manifest::MemberKind::Directory,
                )
            })
            .collect();
        Contents::Server { review, entries }
    } else {
        let manifest =
            scan_browser_archive(client, &request.archive, stopped).map_err(archive_error)?;
        manifest.require_protected(source_folder.owner())?;
        Contents::Client(manifest)
    };
    verify_folders(client, &source_folder, &destination_folder, true, stopped)?;
    verify_roots(client, &request, stopped)?;
    absent(client, destination_path, stopped)?;
    running(stopped)?;
    Ok(Plan {
        request,
        source_folder,
        destination_folder,
        contents,
        destination,
        metadata_ids,
        used: AtomicBool::new(false),
    })
}

pub(super) fn apply(
    client: &mut Client,
    plan: &Plan,
    cancel: &Cancel,
    control: &Control,
) -> Result<Reply> {
    let stopped = || cancel.stopped() || control.is_closed();
    let metadata_allowed = || ensure_ownership_allowed(control, cancel, Some(&plan.request.target));
    let result = (|| {
        check_route(&plan.request, control)?;
        if plan.requires_transfer() {
            supported(client)?;
        }
        running(&stopped)?;
        if plan.metadata_ids.is_some() {
            metadata_allowed()?;
        }
        if plan.used.swap(true, Ordering::AcqRel) {
            return Err(io::Error::other("This server archive review was already used").into());
        }
        verify_boundaries(client, plan, true, &stopped)?;
        if let Contents::Server { review, .. } = &plan.contents {
            let creation = plan.request.creation.as_ref().ok_or_else(stale)?;
            let reply = exchange_creation(
                &plan.request,
                creation,
                &protocol::Request::ApplyCreation {
                    spec: creation.spec.clone(),
                    token: review.token.clone(),
                },
                cancel,
                control,
            )?;
            if !matches!(&reply, protocol::Reply::Applied { destination } if destination == &creation.spec.destination)
            {
                return Err(creation_reply_failure(reply));
            }
            let mut note = format!("Created server archive · {}", plan.destination);
            let directory = refresh(client, &plan.request.directory, control, &mut note);
            return Ok(Reply::Changed { directory, note });
        }
        verify_manifest(client, plan, &stopped)?;
        verify_boundaries(client, plan, true, &stopped)?;
        let Contents::Client(manifest) = &plan.contents else {
            unreachable!()
        };
        let bytes = manifest
            .build(client, &plan.request.archive, &stopped)
            .map_err(archive_error)?;
        verify_boundaries(client, plan, true, &stopped)?;
        publish(client, plan, &bytes, &stopped, control, &metadata_allowed)
    })();
    cancel.finish();
    mark_disconnect(result, control)
}

fn verify_boundaries(
    client: &mut Client,
    plan: &Plan,
    strict: bool,
    stopped: &impl Fn() -> bool,
) -> Result<()> {
    let request = &plan.request;
    let destination = request.archive.target.as_str().as_bytes();
    verify_folders(
        client,
        &plan.source_folder,
        &plan.destination_folder,
        strict,
        stopped,
    )?;
    verify_roots(client, request, stopped)?;
    absent(client, destination, stopped)
}

fn verify_manifest(client: &mut Client, plan: &Plan, stopped: &impl Fn() -> bool) -> Result<()> {
    let current =
        scan_browser_archive(client, &plan.request.archive, stopped).map_err(archive_error)?;
    if !matches!(&plan.contents, Contents::Client(manifest) if &current == manifest) {
        return Err(stale().into());
    }
    running(stopped)
}

fn exchange_creation(
    request: &Request,
    creation: &ServerCreation,
    message: &protocol::Request,
    cancel: &Cancel,
    control: &Control,
) -> Result<protocol::Reply> {
    let authorize = || {
        if cancel.stopped()
            || !request.bound(control)
            || creation.opener.verified_host_key() != Some(&request.peer)
            || creation.opener.is_closed()
        {
            return Err(stale());
        }
        let policy = ClipboardPolicy::load();
        if policy.problem.is_some() || !request.target.workflow_route_allowed(&policy) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Current policy forbids this server archive",
            ));
        }
        Ok(())
    };
    authorize()?;
    let cancel = cancel.clone();
    let owner = request.owner.clone();
    super::extract::transport::exchange(
        &creation.opener,
        message,
        move || cancel.stopped() || owner.upgrade().is_none_or(|control| control.is_closed()),
        authorize,
    )
    .map_err(Failure::from)
}

fn creation_reply_failure(reply: protocol::Reply) -> Failure {
    match reply {
        protocol::Reply::Failed {
            message,
            outcome_unknown,
        } => {
            let message: String = message
                .chars()
                .filter(|c| !c.is_control())
                .take(320)
                .collect();
            let suffix = if outcome_unknown {
                " · Check the reviewed destination before retrying."
            } else {
                ""
            };
            io::Error::other(format!(
                "Server archive helper refused creation · {message}{suffix}"
            ))
            .into()
        }
        _ => io::Error::other("The server archive helper returned an unexpected receipt").into(),
    }
}

fn verify_folders(
    client: &mut Client,
    source: &Folder,
    destination: &Folder,
    strict: bool,
    stopped: &impl Fn() -> bool,
) -> Result<()> {
    running(stopped)?;
    source.verify(client, strict)?;
    running(stopped)?;
    destination.verify(client, strict)
}

fn verify_roots(client: &mut Client, request: &Request, stopped: &impl Fn() -> bool) -> Result<()> {
    for (path, selected) in request.archive.sources.iter().zip(&request.selected) {
        running(stopped)?;
        let path = path.as_str().as_bytes();
        let attrs = client.lstat(path)?;
        if remote_kind(attrs) != selected.kind
            || attrs.uid != selected.owner
            || attrs.gid != selected.group
            || attrs.permissions != selected.mode
            || attrs.size != selected.size
            || attrs.modified != selected.modified
            || client.canonicalize(path)? != path
        {
            return Err(stale().into());
        }
    }
    Ok(())
}

fn publish(
    client: &mut Client,
    plan: &Plan,
    bytes: &[u8],
    stopped: &impl Fn() -> bool,
    control: &Control,
    metadata_allowed: &impl Fn() -> Result<()>,
) -> Result<Reply> {
    let target = &plan.request.archive.target;
    let destination = target.as_str().as_bytes();
    running(stopped)?;
    let stage = RemoteStage::claim(
        client,
        target.parent().as_bytes(),
        plan.destination_folder.owner(),
    )?;
    let owner = UploadOwner {
        uid: stage.uid,
        gid: stage.gid,
    };
    let length = bytes.len() as u64;
    let mut publishing = false;
    let mut metadata_payload = None;
    let result = (|| -> Result<()> {
        client.upload_private_verified(
            &stage.payload,
            UploadStage::Create,
            length,
            Cursor::new(bytes),
            |observed, _| {
                if observed != owner {
                    return Err(io::Error::other(
                        "The private archive payload owner changed",
                    ));
                }
                Ok(())
            },
            stopped,
        )?;
        // Creating the stage may change the listing folder's modification time.
        // It cannot add a member to a selected source because overlap is forbidden.
        verify_boundaries(client, plan, false, stopped)?;
        stage.verify(client)?;
        let payload = client.lstat(&stage.payload)?;
        let payload = if plan.metadata_ids.is_some() {
            if !private_payload(&stage, payload, length)
                || payload.size != Some(length)
                || payload.modified.is_none()
            {
                return Err(io::Error::other("The private archive payload changed").into());
            }
            let verified = metadata_payload.insert(payload);
            configure_payload(
                client,
                plan,
                &stage.payload,
                verified,
                bytes,
                stopped,
                metadata_allowed,
            )?;
            *verified
        } else {
            if client.verify_private_upload(
                &stage.payload,
                owner,
                length,
                Cursor::new(bytes),
                stopped,
            )? != length
            {
                return Err(io::Error::other("The private archive payload changed").into());
            }
            payload
        };
        verify_manifest(client, plan, stopped)?;
        verify_boundaries(client, plan, false, stopped)?;
        stage.verify(client)?;
        if !unchanged(payload, client.lstat(&stage.payload)?) {
            return Err(io::Error::other("The private archive payload changed").into());
        }
        running(stopped)?;
        if plan.metadata_ids.is_some() {
            metadata_allowed()?;
        }
        publishing = true;
        client
            .hardlink(&stage.payload, destination)
            .map_err(|error| {
                let error = Failure::from(error);
                if plan.metadata_ids.is_some() && !error.disconnected {
                    error.context(
                        " · The server did not publish the archive with its reviewed metadata",
                    )
                } else {
                    error
                }
            })?;
        Ok(())
    })();
    let cleanup = (!control.is_closed()
        && result
            .as_ref()
            .err()
            .is_none_or(|error| !error.disconnected))
    .then(|| {
        cleanup(
            client,
            &plan.destination_folder,
            &stage,
            length,
            metadata_payload,
        )
    });
    let cleaned = matches!(&cleanup, Some(Ok(())));
    let cleanup_disconnected = matches!(&cleanup, Some(Err(error)) if error.disconnected);
    let stage_note = || format!(" · Check private staging path {}", stage.label());
    match result {
        Ok(()) => {
            let mut note = format!(
                "Created {} archive with {} entries and {length} bytes · {}",
                plan.format(),
                plan.entries().len(),
                plan.destination(),
            );
            if !cleaned {
                note.push_str(&stage_note());
            }
            if cleanup_disconnected {
                control.abort();
            }
            let directory = if stopped() {
                None
            } else {
                refresh(client, &plan.request.directory, control, &mut note)
            };
            Ok(Reply::Changed { directory, note })
        }
        Err(mut error) => {
            if publishing && (error.disconnected || control.is_closed()) {
                error = error.context(" · Server archive publication could not be confirmed");
            }
            if !cleaned {
                error = error.context(stage_note());
            }
            error.disconnected |= cleanup_disconnected;
            Err(error)
        }
    }
}

fn configure_payload(
    client: &mut Client,
    plan: &Plan,
    path: &[u8],
    verified: &mut Attributes,
    bytes: &[u8],
    stopped: &impl Fn() -> bool,
    metadata_allowed: &impl Fn() -> Result<()>,
) -> Result<()> {
    let (selection, ids) = plan
        .metadata()
        .ok_or_else(|| io::Error::other("The reviewed archive metadata is unavailable"))?;
    let metadata = UploadMetadata {
        uid: ids.0,
        gid: ids.1,
        mode: selection.mode(),
    };
    let rechecked = Cell::new(false);
    let recheck = || -> Result<()> {
        if plan.request.resolve_metadata(false, metadata_allowed)? != Some(ids) {
            return Err(io::Error::other(
                "The reviewed server account IDs changed. Review the archive again.",
            )
            .into());
        }
        Ok(())
    };
    let before_change = || {
        if stopped() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Server archive cancelled",
            ));
        }
        let result = if rechecked.replace(true) {
            metadata_allowed()
        } else {
            recheck()
        };
        result.map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error.message))
    };
    client
        .set_private_upload_metadata(
            path,
            verified,
            Cursor::new(bytes),
            metadata,
            stopped,
            before_change,
        )
        .map_err(|error| {
            Failure::from(error)
                .context(" · Could not verify the requested archive ownership or permissions")
        })?;
    if !rechecked.get() {
        recheck()?;
    }
    Ok(())
}

fn cleanup(
    client: &mut Client,
    folder: &Folder,
    stage: &RemoteStage,
    length: u64,
    metadata_payload: Option<Attributes>,
) -> Result<()> {
    folder.verify(client, false)?;
    stage.verify(client)?;
    if let Some(attrs) = remote_existing(client, &stage.payload)?
        && !metadata_payload.map_or_else(
            || private_payload(stage, attrs, length),
            |verified| unchanged(verified, attrs),
        )
    {
        return Err(io::Error::other(
            "The private archive payload could not be verified for cleanup",
        )
        .into());
    }
    stage.remove(client)
}

fn private_payload(stage: &RemoteStage, attrs: Attributes, maximum: u64) -> bool {
    attrs.kind() == FileKind::File
        && attrs.uid == Some(stage.uid)
        && attrs.gid == Some(stage.gid)
        && attrs.permissions.is_some_and(|mode| mode & 0o7777 == 0o600)
        && attrs.size.is_some_and(|size| size <= maximum)
}

fn refresh(
    client: &mut Client,
    path: &[u8],
    control: &Control,
    note: &mut String,
) -> Option<Arc<Directory>> {
    match list_remote(client, path) {
        Ok(directory) if directory.path == Location::Remote(path.to_vec()) => {
            Some(Arc::new(directory))
        }
        Ok(_) => {
            note.push_str(" · The selected server folder changed before refresh");
            None
        }
        Err(error) => {
            if error.disconnected {
                control.abort();
            }
            note.push_str(&format!(" · refresh failed ({})", error.message));
            None
        }
    }
}

fn absent(client: &mut Client, destination: &[u8], stopped: &impl Fn() -> bool) -> Result<()> {
    running(stopped)?;
    if remote_existing(client, destination)?.is_some() {
        return Err(io::Error::other("The server archive destination already exists").into());
    }
    Ok(())
}

fn check_route(request: &Request, control: &Control) -> Result<()> {
    if !request.bound(control) {
        return Err(io::Error::other("The reviewed SSH connection changed").into());
    }
    Ok(())
}

fn supported(client: &Client) -> Result<()> {
    if !client.supports_hardlink() {
        return Err(
            io::Error::other("Server archive requires SFTP no-replace hard-link support").into(),
        );
    }
    Ok(())
}

fn running(stopped: &impl Fn() -> bool) -> Result<()> {
    if stopped() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "Server archive cancelled").into());
    }
    Ok(())
}

fn stale() -> io::Error {
    io::Error::other(
        "The selected server archive sources changed or could not be verified. Refresh and review again.",
    )
}

fn archive_error(error: io::Error) -> Failure {
    let disconnected = error.kind() == io::ErrorKind::ConnectionAborted;
    let mut failure = Failure::from(error);
    failure.disconnected = disconnected;
    failure
}

fn mark_disconnect<T>(result: Result<T>, control: &Control) -> Result<T> {
    result.map_err(|mut error| {
        error.disconnected |= control.is_closed();
        error
    })
}
