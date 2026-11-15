use std::borrow::Cow;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
use std::cell::Cell;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::Shutdown;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use kortty_jobs::{Cancellation, RunOutcome, RunRequest};
use kortty_ssh::sftp::{Attributes, FileKind};
use kortty_ssh::{ExecLease, ExecOpener, SshTarget, TrustStore};
use pgp::crypto::hash::HashAlgorithm;

#[cfg(unix)]
use crate::private_files::Directory;
use crate::private_files::{FileStamp, nonce, require_single_link};
use crate::scheduler_window::{SshCommand, rsync, sync};
use crate::sftp_window::{StagedDownload, local_tree};

use super::super::{Completion, ExecutionPolicy, Failure, check_cancelled, remote};

mod batch;
mod bridge;
mod existing;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
mod local_existing;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
mod local_file;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
mod mixed;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
mod privileged;
mod process;
mod recurring;
mod tree;
mod upload;
mod upload_tree;
#[cfg(windows)]
mod windows_program;
#[cfg(all(test, windows))]
mod windows_tests;
pub(crate) use bridge::{ARGUMENT as BRIDGE_ARGUMENT, serve as serve_bridge};
use bridge::{Listener, Stream};
pub(super) use existing::review as review_existing;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(super) use local_existing::review as review_local_existing;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(super) use mixed::review as review_mixed;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(super) use privileged::review as review_privileged;
pub(crate) use upload::digest as upload_digest;
pub(crate) use upload_tree::inventory as upload_tree_inventory;
#[cfg(windows)]
use windows_program::fingerprint;

const MAX_FILE: u64 = rsync::MAX_FILE_BYTES;
const PROTOCOL_OVERHEAD: u64 = 16 * 1024 * 1024;
const MAX_PROTOCOL: u64 = MAX_FILE + PROTOCOL_OVERHEAD;
const MAX_PROGRAM: u64 = 64 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(20);
const PAYLOAD: &str = "payload";

pub(crate) fn program_digest(program: &Path) -> io::Result<[u8; 32]> {
    fingerprint(program, &|| false)
}

#[cfg(unix)]
fn fingerprint(program: &Path, cancelled: &impl Fn() -> bool) -> io::Result<[u8; 32]> {
    let parent_path = program.parent().ok_or_else(changed_program)?;
    let name = program
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(changed_program)?;
    let parent = Directory::open_owner_controlled(parent_path)?;
    let file = parent.open_file(name)?;
    #[cfg(target_os = "macos")]
    kortty_files::require_no_granting_extended_acl(&file)?;
    let mut input = &file;
    let metadata = input.metadata()?;
    let euid = rustix::process::geteuid().as_raw();
    if metadata.len() > MAX_PROGRAM
        || (metadata.uid() != euid && metadata.uid() != 0)
        || metadata.mode() & 0o6022 != 0
        || metadata.mode() & 0o111 == 0
    {
        return Err(io::Error::other(
            "Choose an owner-controlled native rsync executable below 64 MiB",
        ));
    }
    let stamp = FileStamp::read(input)?;
    let mut magic = [0; 4];
    input.read_exact(&mut magic)?;
    if !matches!(
        &magic,
        b"\x7fELF"
            | b"\xcf\xfa\xed\xfe"
            | b"\xfe\xed\xfa\xcf"
            | b"\xca\xfe\xba\xbe"
            | b"\xca\xfe\xba\xbf"
    ) {
        return Err(io::Error::other("Choose a native rsync executable"));
    }
    let mut digest = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(io::Error::other)?;
    digest.update(&magic);
    let mut buffer = [0; 32 * 1024];
    let mut count = 4;
    loop {
        require_running(cancelled)?;
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        count += read as u64;
        if count > metadata.len() {
            return Err(changed_program());
        }
        digest.update(&buffer[..read]);
    }
    stamp.require_unchanged(&file)?;
    let current = Directory::open_owner_controlled(parent_path)?;
    if !parent.same_as(&current)? || count != metadata.len() {
        return Err(changed_program());
    }
    stamp.require_unchanged(&current.open_file(name)?)?;
    Ok(digest
        .finalize()
        .as_ref()
        .try_into()
        .expect("SHA-256 produces 32 bytes"))
}

pub(in crate::job_executor) fn run(
    request: &RunRequest<SshCommand>,
    action: &rsync::Transfer,
    uploading: bool,
    policy: &ExecutionPolicy,
    target: SshTarget,
    jumps: Vec<SshTarget>,
    trust: &TrustStore,
) -> Result<Completion, Failure> {
    action.validate(uploading).map_err(Failure::blocked)?;
    let review_required =
        || Failure::blocked("Approve the exact rsync executable and saved SSH route first.");
    let reference = request
        .action
        .connection
        .as_ref()
        .ok_or_else(review_required)?;
    let grouped = policy.group_scope.is_some();
    let updates_existing = action.uses_existing_destination();
    let elevated = action.uses_passwordless_sudo();
    let current_review = elevated || updates_existing;
    if elevated && !policy.sudo_allowed {
        return Err(Failure::blocked("Managed policy forbids privileged rsync."));
    }
    if current_review {
        policy.require_rsync_approval(request.id, &request.job, reference)?;
    }
    if cfg!(windows) || uploading || action.directory || grouped {
        authorize_transfer(reference, elevated)?;
    }
    let approval = action.approval.as_ref().ok_or_else(review_required)?;
    if &approval.reference != reference || approval.program != action.program {
        return Err(review_required());
    }
    let recurring = policy
        .rsync
        .as_ref()
        .and_then(crate::scheduler_window::rsync_authority::RunContext::standing)
        .map(|lease| recurring::Route { lease, reference });
    if action.standing.is_some()
        && matches!(
            request.trigger,
            kortty_jobs::RunTrigger::Scheduled | kortty_jobs::RunTrigger::Dependency
        )
        && recurring.is_none()
    {
        return Err(Failure::blocked(
            "A recurring rsync run requires its current firing approval.",
        ));
    }
    #[cfg(windows)]
    if !uploading && !action.directory && approval.batch.is_none() && approval.download.is_none() {
        return Err(review_required());
    }
    let pins = sync::pins(reference, trust).map_err(|_| review_required())?;
    if sync::fingerprints(&pins) != approval.pins {
        return Err(Failure::blocked(
            "The approved rsync route host keys changed.",
        ));
    }
    let cancelled = || request.cancellation.is_requested();
    let changed = || Failure::blocked("The approved rsync program changed or is unavailable.");
    #[cfg(windows)]
    let program_bundle =
        windows_program::Prepared::new(&action.program, approval.digest, &cancelled);
    #[cfg(windows)]
    let executable = {
        check_cancelled(&request.cancellation)?;
        program_bundle.as_ref().map_err(|_| changed())?.program()
    };
    #[cfg(unix)]
    let executable = {
        let digest = fingerprint(&action.program, &cancelled);
        check_cancelled(&request.cancellation)?;
        if digest.ok() != Some(approval.digest) {
            return Err(changed());
        }
        action.program.as_path()
    };
    let version = process::check_version(executable, &cancelled);
    check_cancelled(&request.cancellation)?;
    version.map_err(|_| Failure::blocked("The approved local executable must be native rsync 3.4.1 or later in the rsync 3 series."))?;
    let batch = approval.batch.as_deref();
    let members = match batch {
        Some(batch) => batch.transfers(approval).map(Cow::Owned).collect(),
        None => vec![Cow::Borrowed(action)],
    };
    let destinations = if current_review {
        Vec::new()
    } else {
        members
            .iter()
            .map(|member| Destination::prepare(member, uploading))
            .collect::<Result<Vec<_>, _>>()?
    };
    let scoped = cfg!(windows) || grouped || batch.is_some() || current_review;
    let guard = (uploading || scoped).then(|| {
        let policy = policy.clone();
        let action = request.action.clone();
        let reference = reference.clone();
        let run = request.id;
        let job = request.job.clone();
        let reviewed_pins = scoped.then(|| (trust.clone(), pins.clone()));
        let guard = move || {
            policy
                .require_target_groups(&action)
                .and_then(|()| authorize_transfer(&reference, elevated))
                .and_then(|()| {
                    if current_review {
                        policy.require_rsync_approval(run, &job, &reference)
                    } else {
                        Ok(())
                    }
                })
                .map_err(|failure| {
                    io::Error::new(io::ErrorKind::PermissionDenied, failure.detail)
                })?;
            if let Some((trust, pins)) = &reviewed_pins
                && !sync::pins(&reference, trust).is_ok_and(|current| current == *pins)
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "The approved rsync route host keys changed.",
                ));
            }
            Ok(())
        };
        std::sync::Arc::new(guard) as ExecutionGuard
    });
    let session = if guard.is_some() {
        remote::sync::open_pinned_with(target, jumps, &pins, |target, jumps, pinned| {
            remote::open_guarded_subsystem(
                target,
                jumps,
                pinned,
                elevated || (uploading && action.directory),
                cancelled,
                || require_authority(guard.as_ref()),
            )
        })
    } else {
        remote::sync::open_pinned_for_job(target, jumps, &pins, cancelled, request, policy)
    };
    check_cancelled(&request.cancellation)?;
    let mut session = session?;
    let exec = session.exec_opener().ok_or_else(|| {
        Failure::failed("The pinned SSH session cannot open the remote rsync process.")
    })?;
    let context = TransferContext {
        exec,
        cancellation: &request.cancellation,
        reference,
        guard,
        executable,
        recurring,
    };
    let outcome = remote::sync::supervise(&mut session, &cancelled, |client, stopped, _| {
        let stopped = || stopped() || policy.require_target_groups(&request.action).is_err();
        if action.mixed.is_some() {
            #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
            return mixed::run(client, &context, action, uploading, request.id, &stopped);
            #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
            return Err(Failure::blocked(
                "Mixed rsync transfers are unavailable on this platform.",
            ));
        }
        if elevated {
            #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
            return privileged::run(client, &context, action, request.id, &stopped);
            #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
            return Err(Failure::blocked(
                "Privileged rsync is unavailable on this platform.",
            ));
        }
        if action.existing.is_some() {
            let (temp, listener, endpoint) = stage_bridge(&context, &stopped)?;
            if let Some(recurring) = context.recurring {
                let destination = action.existing_destination().map_err(Failure::blocked)?;
                let snapshot = recurring.remote(&destination)?;
                let (completion, successor) = existing::apply_recurring(
                    context.transfer(action, &temp, &stopped),
                    listener,
                    &endpoint,
                    request.id,
                    snapshot,
                    |incoming| recurring.reserve([incoming]),
                    |removed| recurring.arm(removed),
                )?;
                return Ok(recurring.confirm(
                    completion,
                    successor.map(|snapshot| rsync::standing::Receiver::Remote {
                        destination,
                        snapshot,
                    }),
                ));
            }
            return existing::apply(
                context.transfer(action, &temp, &stopped),
                listener,
                &endpoint,
                request.id,
            );
        }
        if action.existing_download.is_some() {
            #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
            return if action.directory {
                run_existing_download(client, &context, action, request.id, &stopped)
            } else {
                local_file::run(client, &context, action, None, request.id, &stopped)
            };
            #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
            return Err(Failure::blocked(
                "Existing local rsync receivers are unavailable on this platform.",
            ));
        }
        if batch.is_some() {
            let sources = batch::preflight(client, &members, &context, uploading, &stopped)?;
            return Ok(batch::run(
                client,
                &context,
                members,
                destinations,
                sources,
                request.id,
                &stopped,
            ));
        }
        let detail = run_one(
            client,
            &context,
            action,
            destinations.into_iter().next().expect("one destination"),
            None,
            &stopped,
        )?;
        let mut completion = Completion::from_result(request.id, Ok(0));
        completion.detail = Some(detail);
        Ok(completion)
    });
    match outcome {
        Ok(Ok(completion)) => Ok(completion),
        Ok(Err(error)) if matches!(error.detail, upload_tree::UNCERTAIN | tree::UNCERTAIN) => {
            Err(error)
        }
        Ok(Err(error)) | Err(error) => {
            check_cancelled(&request.cancellation)?;
            policy.require_target_groups(&request.action)?;
            Err(error)
        }
    }
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn run_existing_download(
    client: &mut crate::sftp_window::scheduled::Client,
    context: &TransferContext<'_>,
    action: &rsync::Transfer,
    id: kortty_jobs::RunId,
    stopped: &impl Fn() -> bool,
) -> Result<Completion, Failure> {
    let approval = action
        .approval
        .as_ref()
        .and_then(|approval| approval.existing_download.as_deref())
        .ok_or_else(|| Failure::blocked("Review the exact existing local rsync directory."))?;
    let authorize = || {
        check_cancelled(context.cancellation)?;
        require_authority(context.guard.as_ref())
    };
    require_running(stopped).map_err(|_| transfer_stopped())?;
    authorize()?;
    action
        .require_safe_existing_local_program()
        .map_err(Failure::blocked)?;
    let snapshot = match context.recurring {
        Some(recurring) => recurring.local(&approval.destination)?,
        None => approval.snapshot,
    };
    let receiver = local_existing::Receiver::prepare(&approval.destination, snapshot, stopped)
        .map_err(|_| {
            Failure::blocked("The reviewed local rsync directory changed or is unavailable.")
        })?;
    let (temp, listener, endpoint) = stage_bridge(context, stopped)?;
    let environment = context.transfer(action, &temp, stopped);
    let preflight = match context.recurring {
        Some(recurring) => {
            let remaining = recurring.remaining()?;
            let manifest = tree::Manifest::read_limited(
                client,
                &action.remote,
                tree::Limits {
                    entries: remaining.entries as usize + 1,
                    bytes: remaining.bytes,
                },
                stopped,
            )?;
            if !manifest.is_directory() {
                return Err(Failure::blocked(
                    "Choose an ordinary server directory for the rsync download.",
                ));
            }
            recurring.reserve([manifest.inventory()?])?;
            Some(manifest)
        }
        None => None,
    };
    let (incoming, manifest) = tree::receive_existing(
        client,
        environment,
        listener,
        &endpoint,
        context.reference,
        preflight,
    )?;
    let local_existing::Published {
        mut completion,
        successor,
    } = receiver.publish(
        local_existing::Incoming {
            directory: &incoming,
            inventory: manifest.inventory()?,
            delete: &if approval.settings.delete {
                kortty_server_archive::rsync_tree::DeleteScope::All
            } else {
                kortty_server_archive::rsync_tree::DeleteScope::Retain
            },
        },
        stopped,
        &authorize,
        id,
        |removed| context.recurring.map_or(Ok(()), |route| route.arm(removed)),
    )?;
    if let Some(recurring) = context.recurring {
        completion = recurring.confirm(
            completion,
            successor.map(|snapshot| rsync::standing::Receiver::Local {
                destination: approval.destination.clone(),
                snapshot,
            }),
        );
    }
    if completion.outcome == RunOutcome::Success {
        completion.append_detail(&manifest.download_detail());
    }
    Ok(completion)
}

enum Destination {
    Server,
    File(StagedDownload),
    Tree(tree::Destination),
}

impl Destination {
    fn prepare(action: &rsync::Transfer, uploading: bool) -> Result<Self, Failure> {
        if uploading {
            return Ok(Self::Server);
        }
        if action.directory {
            return tree::Destination::create(&action.local).map(Self::Tree);
        }
        StagedDownload::create(&action.local)
            .map(Self::File)
            .map_err(|_| {
                Failure::blocked("Choose a new local destination in an existing checked folder.")
            })
    }
}

struct TransferContext<'a> {
    exec: ExecOpener,
    cancellation: &'a Cancellation,
    reference: &'a crate::scheduler_window::ConnectionRef,
    guard: Option<ExecutionGuard>,
    executable: &'a Path,
    recurring: Option<recurring::Route<'a>>,
}

pub(super) type ExecutionGuard = std::sync::Arc<dyn Fn() -> io::Result<()> + Send + Sync>;

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn authority_revoked(checked: &Cell<(Instant, bool)>, guard: &ExecutionGuard) -> bool {
    let mut state = checked.get();
    if state.1 && state.0.elapsed() >= Duration::from_millis(250) {
        state = (Instant::now(), guard().is_ok());
        checked.set(state);
    }
    !state.1
}

impl TransferContext<'_> {
    fn transfer<'a, C>(
        &'a self,
        action: &'a rsync::Transfer,
        temp: &'a Scratch,
        stopped: &'a C,
    ) -> Transfer<'a, C> {
        Transfer {
            action,
            exec: self.exec.clone(),
            cancellation: self.cancellation.clone(),
            temp,
            stopped,
            execution_guard: self.guard.clone(),
            executable: self.executable,
        }
    }
}

fn require_authority(guard: Option<&ExecutionGuard>) -> Result<(), Failure> {
    if let Some(guard) = guard {
        guard()
            .map_err(|_| Failure::blocked("The reviewed rsync route or managed policy changed."))?;
    }
    Ok(())
}

fn stage_bridge(
    context: &TransferContext<'_>,
    stopped: &impl Fn() -> bool,
) -> Result<(Scratch, Listener, String), Failure> {
    require_running(stopped).map_err(|_| Failure::failed("Rsync stopped before publication."))?;
    require_authority(context.guard.as_ref())?;
    let temp = Scratch::new(&std::env::temp_dir())
        .map_err(|_| Failure::failed("Could not create a private rsync staging folder."))?;
    let (listener, endpoint) = bridge::bind(&temp.path)
        .map_err(|_| Failure::failed("Could not bind the private rsync protocol bridge."))?;
    Ok((temp, listener, endpoint))
}

fn run_one(
    client: &mut crate::sftp_window::scheduled::Client,
    context: &TransferContext<'_>,
    action: &rsync::Transfer,
    destination: Destination,
    reviewed_source: Option<batch::Source>,
    stopped: &impl Fn() -> bool,
) -> Result<String, Failure> {
    let (temp, listener, endpoint) = stage_bridge(context, stopped)?;
    let environment = context.transfer(action, &temp, stopped);
    let stage = match destination {
        Destination::Tree(destination) => {
            let before = match reviewed_source {
                Some(batch::Source::Directory(manifest)) => Some(manifest),
                _ => None,
            };
            return tree::apply(
                client,
                environment,
                listener,
                &endpoint,
                context.reference,
                destination,
                before,
            );
        }
        Destination::Server if action.directory => {
            return upload_tree::apply(environment, listener, &endpoint, || {
                require_authority(context.guard.as_ref())
            });
        }
        Destination::Server => {
            return upload::apply(client, environment, listener, &endpoint, context.reference);
        }
        Destination::File(stage) => stage,
    };
    let path = action.remote.as_str();
    let metadata = remote::verify_entry(client, path)?;
    if let Some(batch::Source::File(previous)) = reviewed_source
        && !same_source(previous, metadata)
    {
        return Err(Failure::failed("The batch source changed after preflight."));
    }
    let size = checked_size(metadata)?;
    environment.run(listener, &endpoint, path, false)?;
    let mut payload = temp
        .payload()
        .map_err(|_| Failure::failed("Rsync did not return one checked regular file."))?;
    let stamp = FileStamp::read(&payload)
        .map_err(|_| Failure::failed("Could not verify the staged rsync file."))?;
    if payload
        .metadata()
        .map_err(|_| Failure::failed("Could not verify the staged rsync file."))?
        .len()
        != size
    {
        return Err(Failure::failed(
            "The rsync file size changed during the download.",
        ));
    }
    let mut sink = VerifyAndCopy {
        source: &mut payload,
        output: stage
            .writer()
            .map_err(|_| Failure::failed("Could not write the private download stage."))?,
        cancelled: stopped,
    };
    client
        .download_regular(path.as_bytes(), metadata, &mut sink)
        .map_err(|_| {
            Failure::failed("The staged rsync file differs from the checked server file.")
        })?;
    stamp
        .require_unchanged(&payload)
        .map_err(|_| Failure::failed("The staged rsync file changed during verification."))?;
    remote::verify_canonical(client, path)?;
    require_authority(context.guard.as_ref())?;
    stage
        .publish(|| stopped() || require_authority(context.guard.as_ref()).is_err())
        .map_err(|_| Failure::failed("Could not publish the new rsync destination."))?;
    Ok(format!(
        "Downloaded {size} verified bytes via native rsync and SSH."
    ))
}

fn same_source(left: Attributes, right: Attributes) -> bool {
    Attributes {
        accessed: None,
        ..left
    } == Attributes {
        accessed: None,
        ..right
    }
}
fn checked_size(attributes: Attributes) -> Result<u64, Failure> {
    if attributes.kind() == FileKind::File
        && attributes.modified.is_some()
        && let Some(size) = attributes.size.filter(|size| *size <= MAX_FILE)
    {
        return Ok(size);
    }
    Err(Failure::blocked(
        "Rsync downloads require a regular server file with size and modification time, up to 256 MiB.",
    ))
}

struct Scratch {
    path: PathBuf,
    parent: local_tree::Directory,
    stage: Option<local_tree::Directory>,
    name: OsString,
}

impl Scratch {
    fn new(root: &Path) -> io::Result<Self> {
        let root = root.canonicalize()?;
        let parent = local_tree::Directory::open_path(&root)?;
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(io::Error::other)?;
        let name = OsString::from(format!("kr-{}", URL_SAFE_NO_PAD.encode(random)));
        let stage = create_scratch_child(&parent, &name)?;
        Ok(Self {
            path: root.join(&name),
            parent,
            stage: Some(stage),
            name,
        })
    }

    fn directory(&self) -> io::Result<&local_tree::Directory> {
        let stage = self.stage.as_ref().expect("active rsync stage");
        if !stage.same_as(&local_tree::Directory::open_path(&self.path)?)? {
            return Err(io::Error::other("The private rsync staging folder changed"));
        }
        #[cfg(unix)]
        if stage.file().metadata()?.mode() & 0o077 != 0 {
            return Err(io::Error::other("The private rsync staging folder changed"));
        }
        #[cfg(windows)]
        stage.require_owner_only()?;
        Ok(stage)
    }

    fn payload(&self) -> io::Result<File> {
        let local_tree::Entry::File(file) =
            local_tree::entry(self.directory()?, OsStr::new(PAYLOAD))?
        else {
            return Err(io::Error::other("The rsync output was not a regular file"));
        };
        require_single_link(&file)?;
        Ok(file)
    }
}

fn create_scratch_child(
    parent: &local_tree::Directory,
    name: &OsStr,
) -> io::Result<local_tree::Directory> {
    #[cfg(unix)]
    return parent.create_child(name);
    #[cfg(windows)]
    return parent.create_backup_directory(name);
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(stage) = self.stage.take() {
            use kortty_server_archive::rsync_tree::{MAX_DEPTH, projection::MAX_SOURCES};

            // A receiver can hold both the incoming and verified trees beneath its private root.
            let entries = 2 * rsync::MAX_TREE_ENTRIES + MAX_SOURCES + 4;
            let _ = local_tree::discard_bounded(
                &self.parent,
                &self.name,
                stage,
                MAX_DEPTH + 1,
                entries,
            );
        }
    }
}

struct Transfer<'a, C> {
    action: &'a rsync::Transfer,
    exec: ExecOpener,
    cancellation: Cancellation,
    temp: &'a Scratch,
    stopped: &'a C,
    execution_guard: Option<ExecutionGuard>,
    executable: &'a Path,
}

struct ReceiveCheck<F> {
    protocol_limit: u64,
    check_output: F,
}

impl<C: Fn() -> bool> Transfer<'_, C> {
    fn run(
        self,
        listener: Listener,
        bridge_endpoint: &str,
        remote: &str,
        uploading: bool,
    ) -> Result<(), Failure> {
        self.run_checked(
            listener,
            bridge_endpoint,
            remote,
            uploading,
            ReceiveCheck {
                protocol_limit: MAX_PROTOCOL,
                check_output: || Ok(()),
            },
        )
    }

    fn run_checked(
        self,
        listener: Listener,
        bridge_endpoint: &str,
        remote: &str,
        uploading: bool,
        receive: ReceiveCheck<impl FnMut() -> Result<(), Failure>>,
    ) -> Result<(), Failure> {
        let Self {
            action,
            exec,
            cancellation,
            temp,
            stopped,
            execution_guard,
            executable,
        } = self;
        let setup = || Failure::failed("Could not start the approved local rsync client.");
        #[cfg(unix)]
        let changed = || Failure::blocked("The approved rsync executable changed.");
        #[cfg(unix)]
        if fingerprint(executable, stopped).map_err(|_| changed())?
            != action.approval.as_ref().ok_or_else(changed)?.digest
        {
            return Err(changed());
        }
        listener.set_nonblocking(true).map_err(|_| setup())?;
        let token = nonce().map_err(|_| setup())?;
        let program = bridge_executable().map_err(|_| setup())?;
        let shell = bridge::shell(&program, bridge_endpoint).map_err(|_| setup())?;
        temp.directory().map_err(|_| setup())?;
        let mut command = Command::new(executable);
        #[cfg(unix)]
        if action.directory {
            let file_limit = u32::try_from(MAX_FILE).map_err(|_| setup())?;
            kortty_pty::limit_child_file_size(&mut command, file_limit).map_err(|_| setup())?;
        }
        command
            .current_dir(&temp.path)
            .env_clear()
            .env("LANG", "C")
            .env(bridge::TOKEN_ENV, &token)
            .args([
                "--max-size=268435456",
                "--no-links",
                "--no-specials",
                "--no-devices",
                "--no-relative",
            ]);
        #[cfg(windows)]
        command
            .env(bridge::OWNER_ENV, std::process::id().to_string())
            // MSYS must pass the checked server path to the native bridge literally.
            .env("MSYS2_ARG_CONV_EXCL", "*");
        if action.directory {
            command.args(["--recursive", "--no-inc-recursive"]);
        } else {
            command.arg("--no-recursive");
        }
        if uploading {
            command.arg("--secluded-args");
        }
        // The raw stage is private and must remain removable even if the source is read-only.
        command.arg(if action.directory {
            "--chmod=D700,F600"
        } else {
            "--chmod=F600"
        });
        let suffix = if action.directory { "/" } else { "" };
        let endpoint = format!("{}:{remote}{suffix}", bridge::HOST);
        let local = format!("./{PAYLOAD}{suffix}");
        let paths = if uploading {
            [local, endpoint]
        } else {
            [endpoint, local]
        };
        let mut child = process::Process::start(
            command
                .arg("-e")
                .arg(shell)
                .arg("--")
                .args(paths)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .map_err(|_| setup())?;
        let mut stream = accept(&listener, &mut child, stopped).map_err(|_| {
            Failure::failed("The local rsync client did not open the native bridge.")
        })?;
        let mode = if uploading && action.directory {
            bridge::Mode::UploadTree
        } else if uploading {
            bridge::Mode::Upload
        } else if action.directory {
            bridge::Mode::Directory
        } else {
            bridge::Mode::File
        };
        let (command, preamble) = bridge::command(&mut stream, &token, remote, mode, stopped)
            .map_err(|_| {
                Failure::failed("The local rsync client supplied unsupported bridge arguments.")
            })?;
        require_running(stopped).map_err(|_| transfer_stopped())?;
        let cancelled = move || cancellation.is_requested();
        let session = if let Some(authorize) = execution_guard {
            exec.open_guarded(&command, cancelled, move || authorize())
        } else {
            exec.open_cancellable(&command, cancelled)
        };
        let mut session = session
            .map_err(|_| Failure::failed("Could not open the approved remote rsync process."))?;
        let result = relay(&mut session, stream, preamble, &mut child, stopped, receive);
        session.close();
        result
    }
}

fn bridge_executable() -> io::Result<PathBuf> {
    #[cfg(test)]
    if let Some(executable) = std::env::var_os("KORTTY_RSYNC_TEST_BRIDGE") {
        return Ok(PathBuf::from(executable));
    }
    std::env::current_exe()
}

fn accept(
    listener: &Listener,
    child: &mut process::Process,
    stopped: &impl Fn() -> bool,
) -> io::Result<Stream> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        require_running(stopped)?;
        if child.poll()?.is_some() || Instant::now() >= deadline {
            return Err(io::Error::other("Rsync did not open the native SSH bridge"));
        }
        match listener.accept() {
            Ok((stream, _)) => {
                #[cfg(windows)]
                if !child.owns_bridge_peer(&stream)? {
                    continue;
                }
                return Ok(stream);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        thread::sleep(POLL);
    }
}

fn relay(
    session: &mut ExecLease,
    stream: Stream,
    preamble: Vec<u8>,
    child: &mut process::Process,
    stopped: &impl Fn() -> bool,
    ReceiveCheck {
        protocol_limit,
        mut check_output,
    }: ReceiveCheck<impl FnMut() -> Result<(), Failure>>,
) -> Result<(), Failure> {
    let unavailable = || Failure::failed("The rsync SSH streams were unavailable.");
    let mut remote_input = session.take_writer().ok_or_else(unavailable)?;
    let remote_output = session.take_reader().ok_or_else(unavailable)?;
    let remote_errors = session.take_stderr().ok_or_else(unavailable)?;
    let local_input = stream.try_clone().map_err(|_| unavailable())?;
    let mut local_output = stream.try_clone().map_err(|_| unavailable())?;
    let abort = session.abort_handle();
    thread::scope(|scope| {
        let input = scope.spawn(move || {
            remote_input.write_all(&preamble)?;
            forward_input(local_input, &mut remote_input, protocol_limit)?;
            match remote_input.close_input() {
                Err(error) if error.kind() != io::ErrorKind::BrokenPipe => Err(error),
                _ => Ok(()),
            }
        });
        let output = scope.spawn(move || {
            let result = bounded_copy(remote_output, &mut local_output, protocol_limit);
            let _ = local_output.shutdown(Shutdown::Write);
            result
        });
        let errors = scope.spawn(move || bounded_copy(remote_errors, &mut io::sink(), 64 * 1024));
        let result = (|| {
            let mut status = None;
            loop {
                require_running(stopped).map_err(|_| transfer_stopped())?;
                check_output()?;
                if status.is_none() {
                    status = child.poll().map_err(|_| {
                        Failure::failed("Could not inspect the local rsync exit status.")
                    })?;
                }
                if let Some(exit) = status.filter(|status| !status.success()) {
                    return Err(Failure {
                        outcome: RunOutcome::Failed {
                            exit_code: exit.code(),
                        },
                        detail: "The local rsync client exited before completing the transfer.",
                    });
                }
                if let Some(exit) = session.exit_code().filter(|exit| *exit != 0) {
                    return Err(Failure {
                        outcome: RunOutcome::Failed {
                            exit_code: i32::try_from(exit).ok(),
                        },
                        detail: "The remote rsync process exited before completing the transfer.",
                    });
                }
                if status.is_some()
                    && input.is_finished()
                    && output.is_finished()
                    && errors.is_finished()
                {
                    if session.exit_code() == Some(0) {
                        return Ok(());
                    }
                    if abort.is_closed() {
                        return Err(Failure::failed(
                            "The remote rsync exit status was unavailable.",
                        ));
                    }
                }
                thread::sleep(POLL);
            }
        })();
        if result.is_err() {
            abort.abort();
            let _ = stream.shutdown(Shutdown::Both);
        }
        let relay = |join: std::thread::Result<io::Result<()>>, detail| {
            if matches!(join, Ok(Ok(()))) {
                Ok(())
            } else {
                Err(Failure::failed(detail))
            }
        };
        let input = relay(
            input.join(),
            "The rsync bridge could not deliver local input.",
        );
        let output = relay(
            output.join(),
            "The rsync bridge could not deliver remote output.",
        );
        let errors = relay(
            errors.join(),
            "The rsync remote diagnostic stream failed or exceeded its limit.",
        );
        result.and(input).and(output).and(errors)
    })
}

fn transfer_stopped() -> Failure {
    Failure::failed("The rsync protocol stopped before completing the transfer.")
}

fn forward_input(input: impl Read, output: &mut impl Write, limit: u64) -> io::Result<()> {
    struct RemoteInput<'a, W>(&'a mut W, bool);

    impl<W: Write> Write for RemoteInput<'_, W> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.1 {
                return Ok(bytes.len());
            }
            match self.0.write(bytes) {
                // The sender may close after acknowledging success while the
                // local client still has trailing control bytes in the pipe.
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                    self.1 = true;
                    Ok(bytes.len())
                }
                result => result,
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    // SSH queues and wakes on every write. The caller sends an ordered EOF;
    // an extra flush acknowledgement can race the already-exited sender.
    bounded_copy(input, &mut RemoteInput(output, false), limit)
}

fn bounded_copy(input: impl Read, output: &mut impl Write, limit: u64) -> io::Result<()> {
    if io::copy(&mut input.take(limit + 1), output)? > limit {
        return Err(io::Error::other(
            "The rsync protocol exceeded its byte limit",
        ));
    }
    output.flush()
}

struct VerifyAndCopy<'a, C, W> {
    source: &'a mut File,
    output: W,
    cancelled: &'a C,
}

impl<C: Fn() -> bool, W: Write> Write for VerifyAndCopy<'_, C, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        require_running(self.cancelled)?;
        let mut expected = [0; 8192];
        for chunk in bytes.chunks(expected.len()) {
            self.source.read_exact(&mut expected[..chunk.len()])?;
            if chunk != &expected[..chunk.len()] {
                return Err(io::Error::other(
                    "The staged rsync file differs from the checked server file",
                ));
            }
            self.output.write_all(&expected[..chunk.len()])?;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

fn require_running(stopped: &impl Fn() -> bool) -> io::Result<()> {
    if stopped() {
        Err(io::Error::other("Rsync stopped"))
    } else {
        Ok(())
    }
}

fn changed_program() -> io::Error {
    io::Error::other("The approved rsync executable changed")
}

fn authorize(reference: &crate::scheduler_window::ConnectionRef) -> Result<(), Failure> {
    authorize_transfer(reference, false)
}

fn authorize_transfer(
    reference: &crate::scheduler_window::ConnectionRef,
    elevated: bool,
) -> Result<(), Failure> {
    let policy = crate::clipboard_policy::ClipboardPolicy::load();
    if policy.problem.is_none()
        && policy.file_transfer_allowed
        && (!elevated || policy.scheduler_sudo_allowed)
        && policy.minimum_isolation.is_none()
        && super::super::connection::reference_allowed(reference, policy.server_access())
    {
        Ok(())
    } else {
        Err(Failure::blocked(
            "The managed policy blocks this rsync transfer.",
        ))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::fs::{self, Permissions};
    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    #[cfg(unix)]
    use std::os::unix::net::SocketAddr;

    use super::*;

    #[test]
    fn rsync_input_allows_a_closed_sender_without_discarding_other_errors_or_limits() {
        struct WriteError(io::ErrorKind);

        impl Write for WriteError {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(self.0.into())
            }

            fn flush(&mut self) -> io::Result<()> {
                panic!("SSH input is ordered by the closing EOF, without a flush barrier");
            }
        }

        struct ReadError;

        impl Read for ReadError {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }

        let mut closed = WriteError(io::ErrorKind::BrokenPipe);
        assert!(forward_input(&b"trailing control"[..], &mut closed, 16).is_ok());
        assert!(forward_input(io::repeat(0), &mut closed, 16).is_err());
        assert_eq!(
            forward_input(ReadError, &mut closed, 16)
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(
            forward_input(
                &b"data"[..],
                &mut WriteError(io::ErrorKind::InvalidData),
                16
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    #[cfg(unix)]
    fn compact_rsync_scratch_name_fits_the_unix_socket_and_cleans_up() {
        let profile = crate::snippets::tests::Profile::new();
        let scratch = Scratch::new(&profile.0).unwrap();
        let name = scratch.name.to_str().unwrap();
        let encoded = name.strip_prefix("kr-").unwrap();
        assert_eq!(encoded.len(), 22);
        assert_eq!(URL_SAFE_NO_PAD.decode(encoded).unwrap().len(), 16);
        let long_root = PathBuf::from(format!("/{}", "r".repeat(69)));
        let socket = long_root.join(name).join("s");
        assert_eq!(socket.as_os_str().as_encoded_bytes().len(), 98);
        assert!(SocketAddr::from_pathname(socket).is_ok());
        assert!(SocketAddr::from_pathname(long_root.join("x".repeat(64)).join("s")).is_err());
        let path = scratch.path.clone();
        fs::write(path.join(PAYLOAD), b"scratch data").unwrap();
        drop(scratch);
        assert!(!path.exists());
    }

    #[test]
    #[cfg(unix)]
    fn rsync_program_approval_requires_a_protected_nonsymlinked_parent() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let parent = root.join("executables");
        fs::create_dir(&parent).unwrap();
        let program = parent.join("rsync");
        fs::write(&program, b"\x7fELF digest fixture").unwrap();
        fs::set_permissions(&program, Permissions::from_mode(0o700)).unwrap();
        assert!(program_digest(&program).is_ok());
        let alias = root.join("alias");
        symlink(&parent, &alias).unwrap();
        assert!(program_digest(&alias.join("rsync")).is_err());
        fs::set_permissions(&parent, Permissions::from_mode(0o770)).unwrap();
        assert!(program_digest(&program).is_err());
    }
}
