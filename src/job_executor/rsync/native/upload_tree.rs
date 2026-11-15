//! Copy the approved tree privately, then commit it through the pinned SSH helper.

use kortty_server_archive::rsync_tree::{
    self as protocol, Decision, Inventory, MAX_DEPTH, Manifest, Prepare, Reply, hash,
};
use kortty_ssh::{SshReader, SshWriter};
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
use protocol::projection;
use serde::de::DeserializeOwned;
use std::sync::Arc;

use super::*;

pub(super) const UNCERTAIN: &str = "The rsync directory publication could not be confirmed. Check the server destination before retrying.";
const IO_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_REPLY_BYTES: usize = 4096;
pub(super) type PhasePermission = Arc<dyn Fn() -> bool + Send + Sync>;

pub(crate) fn inventory(path: &Path) -> io::Result<Inventory> {
    read_tree(path, None, &|| false)
}

pub(super) fn apply<C: Fn() -> bool>(
    transfer: Transfer<'_, C>,
    listener: Listener,
    bridge_endpoint: &str,
    authorize: impl Fn() -> Result<(), Failure>,
) -> Result<String, Failure> {
    authorize()?;
    let expected = stage_input(&transfer)?;
    apply_staged(transfer, listener, bridge_endpoint, authorize, expected)
}

pub(super) fn apply_staged<C: Fn() -> bool>(
    transfer: Transfer<'_, C>,
    listener: Listener,
    bridge_endpoint: &str,
    authorize: impl Fn() -> Result<(), Failure>,
    expected: Inventory,
) -> Result<String, Failure> {
    let action = transfer.action;
    let stopped = transfer.stopped;
    require_running(stopped).map_err(|_| transfer_stopped())?;
    authorize()?;
    let mut helper = Helper::open(&transfer, stopped)?;
    let cancelled = transfer.cancellation.clone();
    let guard = transfer.execution_guard.clone().ok_or_else(failed)?;
    let permitted: PhasePermission = Arc::new(move || !cancelled.is_requested() && guard().is_ok());
    let prepare = Prepare {
        destination: action.remote.as_str().to_owned(),
        expected,
    };
    helper.send(&prepare, stopped).map_err(|_| failed())?;
    let stage = match helper.receive_new(stopped) {
        Ok(Reply::Prepared { stage }) if protocol::valid_stage(&stage) => stage,
        Ok(Reply::Failed { .. }) => {
            return Err(Failure::blocked(
                "The server helper could not prepare the new rsync directory. Check the destination and filesystem.",
            ));
        }
        _ => {
            return Err(Failure::blocked(
                "The server did not start the directory upload helper. Check the korTTY helper version.",
            ));
        }
    };
    let parent = action.remote.parent().trim_end_matches('/');
    let payload = format!("{parent}/{stage}/{PAYLOAD}");
    let result = (|| {
        authorize()?;
        transfer.run_checked(
            listener,
            bridge_endpoint,
            &payload,
            true,
            ReceiveCheck {
                protocol_limit: expected.bytes + PROTOCOL_OVERHEAD,
                check_output: || Ok(()),
            },
        )?;
        require_running(stopped).map_err(|_| transfer_stopped())?;
        authorize()?;
        helper
            .send(&Decision::Verify, stopped)
            .map_err(|_| failed())?;
        if !matches!(helper.receive_new(stopped), Ok(Reply::Verified)) {
            return Err(Failure::failed(
                "The server could not verify the received rsync directory.",
            ));
        }
        require_running(stopped).map_err(|_| transfer_stopped())?;
        authorize()
    })();
    if let Err(error) = result {
        if helper.final_reply_received {
            return Err(error);
        }
        return Err(if helper.abort() {
            error
        } else {
            Failure {
                outcome: error.outcome,
                detail: "Rsync stopped before publication. A private server staging folder may remain.",
            }
        });
    }
    helper
        .send_guarded(&Decision::Commit, stopped, &permitted)
        .map_err(|_| Failure::failed(UNCERTAIN))?;
    match helper.receive_new(stopped) {
        Ok(Reply::Published {
            destination,
            inventory,
        }) if destination == prepare.destination && inventory == expected => Ok(format!(
            "Uploaded {} verified files and {} folders ({} bytes) via native rsync and SSH.",
            expected.files,
            expected.entries - expected.files,
            expected.bytes,
        )),
        Ok(Reply::Failed {
            outcome_unknown: false,
            ..
        }) => Err(Failure::failed(
            "The server refused the verified rsync publication. Check the destination and private stage.",
        )),
        _ => Err(Failure::failed(UNCERTAIN)),
    }
}

pub(super) fn stage_input<C: Fn() -> bool>(
    transfer: &Transfer<'_, C>,
) -> Result<Inventory, Failure> {
    let action = transfer.action;
    let expected = action
        .approval
        .as_ref()
        .and_then(|approval| approval.directory_upload.as_ref())
        .ok_or_else(|| Failure::blocked("Review and approve this rsync directory upload first."))?
        .inventory;
    if stage_current_input(transfer)? != expected {
        return Err(changed_source());
    }
    Ok(expected)
}

pub(super) fn stage_current_input<C: Fn() -> bool>(
    transfer: &Transfer<'_, C>,
) -> Result<Inventory, Failure> {
    stage_bounded_input(transfer, Manifest::default())
}

pub(super) fn stage_bounded_input<C: Fn() -> bool>(
    transfer: &Transfer<'_, C>,
    mut manifest: Manifest,
) -> Result<Inventory, Failure> {
    let scratch = transfer.temp.directory().map_err(|_| failed())?;
    let local = create_scratch_child(scratch, OsStr::new(PAYLOAD)).map_err(|_| failed())?;
    scan_tree(
        &transfer.action.local,
        Some(&local),
        &mut manifest,
        transfer.stopped,
        &mut |_, _| {},
    )
    .map_err(|_| changed_source())?;
    Ok(manifest.finish())
}

pub(super) struct Helper {
    _session: ExecLease,
    input: SshWriter,
    output: SshReader,
    diagnostics: Option<SshReader>,
    diagnostic_bytes: usize,
    final_reply_received: bool,
    pending_reply: Vec<u8>,
}

impl Helper {
    pub(super) fn open_privileged(
        exec: &ExecOpener,
        cancellation: impl Fn() -> bool + Send + Sync + 'static,
        authority: ExecutionGuard,
    ) -> Result<Self, Failure> {
        Self::open_command(
            exec,
            kortty_server_archive::privileged_rsync::COMMAND,
            cancellation,
            authority,
        )
    }

    pub(super) fn send_privileged(
        &mut self,
        request: &kortty_server_archive::privileged_rsync::Request,
        stopped: &impl Fn() -> bool,
        permitted: &PhasePermission,
    ) -> io::Result<()> {
        let mut frame = Vec::new();
        kortty_server_archive::privileged_rsync::write_request(&mut frame, request)?;
        self.send_bytes(&frame, stopped, Some(permitted))
    }

    pub(super) fn receive_privileged(
        &mut self,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<kortty_server_archive::privileged_rsync::Reply> {
        self.receive_bounded(
            stopped,
            kortty_server_archive::privileged_rsync::FRAME_BYTES,
        )
    }

    fn open<C: Fn() -> bool>(transfer: &Transfer<'_, C>, stopped: &C) -> Result<Self, Failure> {
        require_running(stopped).map_err(|_| transfer_stopped())?;
        let cancel = transfer.cancellation.clone();
        let authorize = transfer.execution_guard.clone().ok_or_else(failed)?;
        Self::open_command(
            &transfer.exec,
            protocol::COMMAND,
            move || cancel.is_requested(),
            authorize,
        )
    }

    pub(super) fn open_existing(
        exec: &ExecOpener,
        cancellation: impl Fn() -> bool + Send + Sync + 'static,
        authority: ExecutionGuard,
    ) -> Result<Self, Failure> {
        Self::open_command(exec, protocol::EXISTING_COMMAND, cancellation, authority)
    }

    pub(super) fn open_existing_v2(
        exec: &ExecOpener,
        cancellation: impl Fn() -> bool + Send + Sync + 'static,
        authority: ExecutionGuard,
    ) -> Result<Self, Failure> {
        Self::open_command(exec, protocol::EXISTING_V2_COMMAND, cancellation, authority)
    }

    pub(super) fn open_existing_v3(
        exec: &ExecOpener,
        cancellation: impl Fn() -> bool + Send + Sync + 'static,
        authority: ExecutionGuard,
    ) -> Result<Self, Failure> {
        Self::open_command(exec, protocol::EXISTING_V3_COMMAND, cancellation, authority)
    }

    fn open_command(
        exec: &ExecOpener,
        command: &str,
        cancellation: impl Fn() -> bool + Send + Sync + 'static,
        authority: ExecutionGuard,
    ) -> Result<Self, Failure> {
        let mut session = exec
            .open_guarded(command, cancellation, move || authority())
            .map_err(|_| Failure::failed("Could not open the native rsync tree helper."))?;
        let input = session.take_writer().ok_or_else(failed)?;
        let output = session.take_reader().ok_or_else(failed)?;
        let diagnostics = session.take_stderr().ok_or_else(failed)?;
        Ok(Self {
            _session: session,
            input,
            output,
            diagnostics: Some(diagnostics),
            diagnostic_bytes: 0,
            final_reply_received: false,
            pending_reply: Vec::new(),
        })
    }

    pub(super) fn send(
        &mut self,
        request: &impl serde::Serialize,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<()> {
        self.send_frame(request, stopped, None)
    }

    pub(super) fn send_guarded(
        &mut self,
        request: &impl serde::Serialize,
        stopped: &impl Fn() -> bool,
        permitted: &PhasePermission,
    ) -> io::Result<()> {
        self.send_frame(request, stopped, Some(permitted))
    }

    pub(super) fn send_existing_v2(
        &mut self,
        request: &protocol::ExistingRequestV2,
        stopped: &impl Fn() -> bool,
        permitted: &PhasePermission,
    ) -> io::Result<()> {
        let mut frame = Vec::new();
        protocol::write_existing_v2_request(&mut frame, request)?;
        self.send_bytes(&frame, stopped, Some(permitted))
    }

    fn send_frame(
        &mut self,
        request: &impl serde::Serialize,
        stopped: &impl Fn() -> bool,
        permitted: Option<&PhasePermission>,
    ) -> io::Result<()> {
        let mut frame = Vec::new();
        protocol::write_frame(&mut frame, request)?;
        self.send_bytes(&frame, stopped, permitted)
    }

    fn send_bytes(
        &mut self,
        mut remaining: &[u8],
        stopped: &impl Fn() -> bool,
        permitted: Option<&PhasePermission>,
    ) -> io::Result<()> {
        let deadline = Instant::now() + IO_TIMEOUT;
        while !remaining.is_empty() {
            require_running(stopped)?;
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let written = if let Some(permitted) = permitted {
                self.input
                    .write_guarded_nonblocking(remaining, Arc::clone(permitted))
            } else {
                self.input.write_nonblocking(remaining)
            };
            match written {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => remaining = &remaining[count..],
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub(super) fn receive<R: DeserializeOwned>(
        &mut self,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<R> {
        self.receive_bounded(stopped, MAX_REPLY_BYTES)
    }

    fn receive_bounded<R: DeserializeOwned>(
        &mut self,
        stopped: &impl Fn() -> bool,
        maximum: usize,
    ) -> io::Result<R> {
        let mut pending = std::mem::take(&mut self.pending_reply);
        let result = read_response(
            &mut pending,
            HelperReader {
                helper: self,
                stopped,
                deadline: Instant::now() + IO_TIMEOUT,
            },
            maximum,
        );
        self.pending_reply = pending;
        result
    }

    fn receive_new(&mut self, stopped: &impl Fn() -> bool) -> io::Result<Reply> {
        let reply = self.receive(stopped)?;
        self.final_reply_received = matches!(
            &reply,
            Reply::Published { .. } | Reply::Aborted | Reply::Failed { .. }
        );
        Ok(reply)
    }

    fn abort(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(1);
        let stopped = || Instant::now() >= deadline;
        self.send(&Decision::Abort, &stopped).is_ok()
            && matches!(self.receive_new(&stopped), Ok(Reply::Aborted))
    }
}

fn read_response<R: DeserializeOwned>(
    pending: &mut Vec<u8>,
    mut input: impl Read,
    maximum: usize,
) -> io::Result<R> {
    // Preserve partial frames across a stopped read, so cleanup can still read the
    // quarantine acknowledgement followed by the restoration result.
    let mut buffer = [0; MAX_REPLY_BYTES];
    let mut fill = |pending: &mut Vec<u8>, until: usize| -> io::Result<()> {
        while pending.len() < until {
            let length = (until - pending.len()).min(buffer.len());
            let count = input.read(&mut buffer[..length])?;
            if count == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            pending.extend_from_slice(&buffer[..count]);
        }
        Ok(())
    };
    fill(pending, 4)?;
    let size = u32::from_be_bytes(pending[..4].try_into().expect("read frame length")) as usize;
    if !(1..=maximum).contains(&size) {
        pending.clear();
        return Err(io::ErrorKind::InvalidData.into());
    }
    fill(pending, 4 + size)?;
    let reply = serde_json::from_slice(&pending[4..])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid rsync tree message"));
    pending.clear();
    reply
}

#[cfg(test)]
#[test]
fn rsync_helper_can_resume_a_partial_reply_after_cancellation() {
    let mut pending = Vec::new();
    let mut frame = Vec::new();
    protocol::write_frame(&mut frame, &Reply::Verified).unwrap();
    for chunk in [&frame[..2], &frame[2..7]] {
        assert_eq!(
            read_response::<Reply>(&mut pending, chunk, MAX_REPLY_BYTES)
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
    assert!(matches!(
        read_response(&mut pending, &frame[7..], MAX_REPLY_BYTES),
        Ok(Reply::Verified)
    ));
    assert!(pending.is_empty());
}

struct HelperReader<'a, F> {
    helper: &'a mut Helper,
    stopped: &'a F,
    deadline: Instant,
}

impl<F: Fn() -> bool> Read for HelperReader<'_, F> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            require_running(self.stopped)?;
            if Instant::now() >= self.deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            if let Some(reader) = &mut self.helper.diagnostics {
                match reader.read_timeout(&mut [0; 512], POLL) {
                    Ok(0) => self.helper.diagnostics = None,
                    Ok(count) => self.helper.diagnostic_bytes += count,
                    Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                    Err(error) => return Err(error),
                }
                if self.helper.diagnostic_bytes > 8 * 1024 {
                    return Err(io::Error::other(
                        "Rsync helper exceeded its diagnostic limit",
                    ));
                }
            }
            match self.helper.output.read_timeout(bytes, POLL) {
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                result => return result,
            }
        }
    }
}

pub(super) fn read_tree(
    path: &Path,
    target: Option<&local_tree::Directory>,
    stopped: &impl Fn() -> bool,
) -> io::Result<Inventory> {
    let mut manifest = Manifest::default();
    scan_tree(path, target, &mut manifest, stopped, &mut |_, _| {})?;
    Ok(manifest.finish())
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(super) fn projected_entries(
    path: &Path,
    target: Option<&local_tree::Directory>,
    budget: &mut Manifest,
    stopped: &impl Fn() -> bool,
) -> io::Result<Vec<projection::Entry>> {
    let mut entries = Vec::new();
    scan_tree(path, target, budget, stopped, &mut |relative, file| {
        entries.push(projection::Entry {
            relative: relative.into(),
            file,
        });
    })?;
    Ok(entries)
}

fn scan_tree(
    path: &Path,
    target: Option<&local_tree::Directory>,
    manifest: &mut Manifest,
    stopped: &impl Fn() -> bool,
    record: &mut impl FnMut(&str, Option<(u64, [u8; 32])>),
) -> io::Result<()> {
    let source = local_tree::Directory::open_path(path)?;
    walk(&source, target, "", 0, manifest, stopped, record)?;
    if !source.same_as(&local_tree::Directory::open_path(path)?)? {
        return Err(changed());
    }
    Ok(())
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(super) fn checked_inventory(
    root: &local_tree::Directory,
    stopped: &impl Fn() -> bool,
) -> io::Result<Inventory> {
    let mut manifest = Manifest::default();
    walk(root, None, "", 0, &mut manifest, stopped, &mut |_, _| {})?;
    Ok(manifest.finish())
}

fn walk(
    source: &local_tree::Directory,
    target: Option<&local_tree::Directory>,
    prefix: &str,
    depth: usize,
    manifest: &mut Manifest,
    stopped: &impl Fn() -> bool,
    record: &mut impl FnMut(&str, Option<(u64, [u8; 32])>),
) -> io::Result<()> {
    require_running(stopped)?;
    if depth > MAX_DEPTH {
        return Err(io::Error::other(
            "The rsync directory exceeds its depth limit",
        ));
    }
    let stamp = directory_stamp(source)?;
    let mut names = source.names(manifest.remaining_entries())?;
    names.sort_unstable();
    for name in names {
        require_running(stopped)?;
        let leaf = name
            .to_str()
            .filter(|name| protocol::valid_name(name))
            .ok_or_else(|| io::Error::other("The rsync source contains an unsafe name"))?;
        let path = if prefix.is_empty() {
            leaf.to_owned()
        } else {
            format!("{prefix}/{leaf}")
        };
        match local_tree::entry(source, &name)? {
            local_tree::Entry::Directory(directory) => {
                if directory_stamp(&directory)?.0 != stamp.0 {
                    return Err(io::Error::other("The rsync source crosses a filesystem"));
                }
                manifest.add(&path, None)?;
                record(&path, None);
                let output = target
                    .map(|target| target.create_child(&name))
                    .transpose()?;
                walk(
                    &directory,
                    output.as_ref(),
                    &path,
                    depth + 1,
                    manifest,
                    stopped,
                    record,
                )?;
                match local_tree::entry(source, &name)? {
                    local_tree::Entry::Directory(current) if directory.same_as(&current)? => {}
                    _ => return Err(changed()),
                }
            }
            local_tree::Entry::File(file) => {
                require_single_link(&file)?;
                let stamp = FileStamp::read(&file)?;
                let size = file.metadata()?.len();
                if size > manifest.remaining_bytes() {
                    return Err(io::Error::other(
                        "The rsync directory exceeds its byte limit",
                    ));
                }
                let output = target.map(|target| target.create_file(&name)).transpose()?;
                let mut input = CopyInput {
                    file: &file,
                    output,
                    stopped,
                };
                let digest = hash(&mut input, size)?;
                if let Some(output) = input.output {
                    output.sync_all()?;
                }
                stamp.require_unchanged(&file)?;
                match local_tree::entry(source, &name)? {
                    local_tree::Entry::File(current) => stamp.require_unchanged(&current)?,
                    _ => return Err(changed()),
                }
                manifest.add(&path, Some((size, digest)))?;
                record(&path, Some((size, digest)));
            }
            local_tree::Entry::Skip => {
                return Err(io::Error::other(
                    "Rsync upload requires regular files and directories",
                ));
            }
        }
    }
    if directory_stamp(source)? != stamp {
        return Err(changed());
    }
    #[cfg(unix)]
    if let Some(target) = target {
        target.file().sync_all()?;
    }
    Ok(())
}

struct CopyInput<'a, F> {
    file: &'a File,
    output: Option<File>,
    stopped: &'a F,
}

impl<F: Fn() -> bool> Read for CopyInput<'_, F> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        require_running(self.stopped)?;
        let count = self.file.read(bytes)?;
        if let Some(output) = &mut self.output {
            output.write_all(&bytes[..count])?;
        }
        Ok(count)
    }
}

#[cfg(unix)]
fn directory_stamp(directory: &local_tree::Directory) -> io::Result<(u64, u64, i64, i64)> {
    let metadata = directory.file().metadata()?;
    Ok((
        metadata.dev(),
        metadata.ino(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    ))
}

#[cfg(windows)]
fn directory_stamp(
    directory: &local_tree::Directory,
) -> io::Result<(u64, [u8; 16], std::time::SystemTime)> {
    let (volume, identity) = directory.identity()?;
    Ok((volume, identity, directory.metadata()?.modified()?))
}

fn changed() -> io::Error {
    io::Error::other("The local rsync directory changed while it was being checked")
}

fn changed_source() -> Failure {
    Failure::blocked(
        "The local rsync directory changed or exceeded a review limit. Review it again.",
    )
}

fn failed() -> Failure {
    Failure::failed("Could not prepare the private rsync directory upload.")
}
