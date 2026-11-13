//! Copy the approved tree privately, then commit it through the pinned SSH helper.

use kortty_server_archive::rsync_tree::{
    self as protocol, Decision, Inventory, MAX_DEPTH, Manifest, Prepare, Reply, hash,
};
use kortty_ssh::{SshReader, SshWriter};

use super::*;

pub(super) const UNCERTAIN: &str = "The rsync directory publication could not be confirmed. Check the server destination before retrying.";
const IO_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) fn inventory(path: &Path) -> io::Result<Inventory> {
    read_tree(path, None, &|| false)
}

pub(super) fn apply<C: Fn() -> bool>(
    transfer: Transfer<'_, C>,
    listener: UnixListener,
    socket: &Path,
    authorize: impl Fn() -> Result<(), Failure>,
) -> Result<String, Failure> {
    let action = transfer.action;
    let stopped = transfer.stopped;
    let expected = action
        .approval
        .as_ref()
        .and_then(|approval| approval.directory_upload.as_ref())
        .ok_or_else(|| Failure::blocked("Review and approve this rsync directory upload first."))?
        .inventory;
    authorize()?;
    let scratch = transfer.temp.directory().map_err(|_| failed())?;
    let local = scratch
        .create_child(OsStr::new(PAYLOAD))
        .map_err(|_| failed())?;
    let copied = read_tree(&action.local, Some(&local), stopped).map_err(|_| changed_source())?;
    if copied != expected {
        return Err(changed_source());
    }
    require_running(stopped).map_err(|_| transfer_stopped())?;
    authorize()?;
    let mut helper = Helper::open(&transfer, stopped)?;
    let prepare = Prepare {
        destination: action.remote.as_str().to_owned(),
        expected,
    };
    helper.send(&prepare, stopped).map_err(|_| failed())?;
    let stage = match helper.receive(stopped) {
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
            socket,
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
        if !matches!(helper.receive(stopped), Ok(Reply::Verified)) {
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
        .send(&Decision::Commit, stopped)
        .map_err(|_| Failure::failed(UNCERTAIN))?;
    match helper.receive(stopped) {
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

struct Helper {
    _session: ExecLease,
    input: SshWriter,
    output: SshReader,
    diagnostics: Option<SshReader>,
    diagnostic_bytes: usize,
    final_reply_received: bool,
}

impl Helper {
    fn open<C: Fn() -> bool>(transfer: &Transfer<'_, C>, stopped: &C) -> Result<Self, Failure> {
        require_running(stopped).map_err(|_| transfer_stopped())?;
        let cancel = transfer.cancellation.clone();
        let authorize = transfer.execution_guard.clone().ok_or_else(failed)?;
        let mut session = transfer
            .exec
            .open_guarded(
                protocol::COMMAND,
                move || cancel.is_requested(),
                move || authorize(),
            )
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
        })
    }

    fn send(
        &mut self,
        request: &impl serde::Serialize,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<()> {
        let deadline = Instant::now() + IO_TIMEOUT;
        let mut frame = Vec::new();
        protocol::write_frame(&mut frame, request)?;
        let mut remaining = frame.as_slice();
        while !remaining.is_empty() {
            require_running(stopped)?;
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match self.input.write_nonblocking(remaining) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => remaining = &remaining[count..],
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn receive(&mut self, stopped: &impl Fn() -> bool) -> io::Result<Reply> {
        let reply = protocol::read_frame(HelperReader {
            helper: self,
            stopped,
            deadline: Instant::now() + IO_TIMEOUT,
        })?;
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
            && matches!(self.receive(&stopped), Ok(Reply::Aborted))
    }
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

fn read_tree(
    path: &Path,
    target: Option<&local_tree::Directory>,
    stopped: &impl Fn() -> bool,
) -> io::Result<Inventory> {
    let source = local_tree::Directory::open_path(path)?;
    let mut manifest = Manifest::default();
    walk(&source, target, "", 0, &mut manifest, stopped)?;
    if !source.same_as(&local_tree::Directory::open_path(path)?)? {
        return Err(changed());
    }
    Ok(manifest.finish())
}

fn walk(
    source: &local_tree::Directory,
    target: Option<&local_tree::Directory>,
    prefix: &str,
    depth: usize,
    manifest: &mut Manifest,
    stopped: &impl Fn() -> bool,
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
                if directory.file().metadata()?.dev() != source.file().metadata()?.dev() {
                    return Err(io::Error::other("The rsync source crosses a filesystem"));
                }
                manifest.add(&path, None)?;
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

fn directory_stamp(directory: &local_tree::Directory) -> io::Result<(u64, u64, i64, i64)> {
    let metadata = directory.file().metadata()?;
    Ok((
        metadata.dev(),
        metadata.ino(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    ))
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
