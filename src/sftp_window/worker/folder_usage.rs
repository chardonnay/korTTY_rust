//! Batched allocated size on the authenticated browser route.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use kortty::ssh_runtime::folder_usage::{DEADLINE, OUTPUT_LIMIT, command};
use kortty_ssh::sftp::{Attributes, FileKind, Sftp};

use crate::ssh_connection::{ExecOpener, SftpOpener};

use super::{Client, Control, Failure, Result, Worker, sync::Cancel};
use crate::sftp_window::browser::{self, Detail, SizeRequest, Sizes};
use crate::sftp_window::{Directory, EntryKind, EntryName, Location, MAX_SELECTION};

const POLL: Duration = Duration::from_millis(40);

#[cfg(test)]
pub(in crate::sftp_window) mod tests;

pub(in crate::sftp_window) struct Route {
    sftp: SftpOpener,
    exec: ExecOpener,
    owner: Weak<Control>,
    cancel: Cancel,
}

impl Route {
    pub(in crate::sftp_window) fn new(worker: &Worker) -> Option<Self> {
        let sftp = worker.transfer_opener()?;
        let exec = worker.exec_opener()?;
        exec.same_connection(&sftp.exec_opener()).then(|| Self {
            sftp,
            exec,
            owner: Arc::downgrade(&worker.control),
            cancel: Cancel::default(),
        })
    }

    pub(in crate::sftp_window) fn cancel(&self) -> &Cancel {
        &self.cancel
    }

    fn gate(&self) -> Gate {
        Gate {
            owner: self.owner.clone(),
            cancel: self.cancel.clone(),
            deadline: Arc::new(Mutex::new(Instant::now() + DEADLINE)),
        }
    }
}

#[derive(Clone)]
struct Gate {
    owner: Weak<Control>,
    cancel: Cancel,
    deadline: Arc<Mutex<Instant>>,
}

impl Gate {
    fn interval(&self) -> io::Result<Duration> {
        Ok(self.remaining()?.min(POLL))
    }

    fn remaining(&self) -> io::Result<Duration> {
        if self.cancel.stopped() {
            // SFTP's read_exact/write_all retry Interrupted automatically.
            return Err(io::Error::other("Folder size calculation cancelled."));
        }
        if self.owner.upgrade().is_none_or(|owner| owner.is_closed()) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "The original SFTP connection closed.",
            ));
        }
        let remaining = self
            .deadline
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Server folder measurement timed out.",
            ));
        }
        Ok(remaining)
    }

    fn stopped(&self) -> bool {
        self.interval().is_err()
    }

    fn restrict(&self, limit: Duration) {
        let mut deadline = self
            .deadline
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *deadline = (*deadline).min(Instant::now() + limit);
    }
}

struct Polling<T> {
    io: T,
    gate: Gate,
}

impl<T: Read> Read for Polling<T> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            self.gate.interval()?;
            match self.io.read(bytes) {
                Err(error) if retry_io(&error) => {}
                result => return result,
            }
        }
    }
}

impl<T: Write> Write for Polling<T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            self.gate.interval()?;
            match self.io.write(bytes) {
                Err(error) if retry_io(&error) => {}
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.gate.interval()?;
        self.io.flush()
    }
}

fn retry_io(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

pub(in crate::sftp_window) fn measure(
    browser_client: &mut Sftp<impl Read, impl Write>,
    request: SizeRequest,
    route: &Route,
) -> Result<Sizes> {
    let gate = route.gate();
    let result = measure_on_route(browser_client, request, route, &gate);
    // An auxiliary channel can close while the original browser lane is live.
    // Preserve that browser and its listing when a private measurement fails.
    result.map_err(|error| {
        if gate.owner.upgrade().is_none_or(|owner| owner.is_closed()) {
            error
        } else {
            Failure::from(io::Error::other(error.message))
        }
    })
}

#[derive(Clone, Copy, Debug)]
pub struct Estimate {
    pub(in crate::sftp_window) bytes: u64,
    pub(in crate::sftp_window) measured: usize,
    pub(in crate::sftp_window) partial: bool,
}

pub(in crate::sftp_window) fn estimate(
    source: &Directory,
    selected: &[EntryName],
    route: &Route,
) -> Option<Estimate> {
    // This advisory display uses the exact paths from the listing. Archive
    // review independently checks its source and destination over SFTP.
    let gate = route.gate();
    let paths = paths(source, selected).ok()?;
    let command = command(&paths).ok()?;
    let (exit, output) = run(&route.exec, &command, &paths, &gate).ok()??;
    let sizes = parse(&output.stdout, &paths, selected).ok()?;
    gate.interval().ok()?;
    if sizes.is_empty() {
        return None;
    }
    Some(Estimate {
        bytes: total(&sizes).ok()?,
        measured: sizes.len(),
        partial: exit != 0 || !output.stderr.is_empty() || sizes.len() != selected.len(),
    })
}

fn measure_on_route(
    browser_client: &mut Sftp<impl Read, impl Write>,
    request: SizeRequest,
    route: &Route,
    gate: &Gate,
) -> Result<Sizes> {
    let paths = paths(&request.source, &request.selected)?;
    let Ok(command) = command(&paths) else {
        return logical_fallback(browser_client, request, &route.sftp, gate);
    };
    // Use the browser channel for validation. A dropped SFTP lease can
    // still occupy the last server slot while the peer processes its close.
    let before = check_primary_roots(browser_client, &request, &paths, gate)?;
    let Some((exit, output)) = run(&route.exec, &command, &paths, gate)? else {
        return logical_fallback(browser_client, request, &route.sftp, gate);
    };
    let measured = parse(&output.stdout, &paths, &request.selected)?;
    if measured.is_empty() && (exit == 126 || exit == 127 || (exit != 0 && output.sftp_only())) {
        return logical_fallback(browser_client, request, &route.sftp, gate);
    }
    let after = check_primary_roots(browser_client, &request, &paths, gate)?;
    if !before
        .iter()
        .zip(&after)
        .all(|(before, after)| same_folder(*before, *after))
    {
        return Err(changed().into());
    }
    gate.interval()?;
    result(request, measured, exit != 0 || !output.stderr.is_empty())
}

struct PrivateSftp {
    client: Client,
    _lease: crate::ssh_connection::Channel,
}

fn open_private(opener: &SftpOpener, gate: &Gate) -> Result<Option<PrivateSftp>> {
    gate.interval()?;
    let Ok(mut channel) = opener.open_cancellable(|| gate.stopped()) else {
        gate.interval()?;
        return Ok(None);
    };
    let reader = channel.take_timed_reader(POLL).ok_or_else(missing_stream)?;
    let writer = channel.take_timed_writer(POLL).ok_or_else(missing_stream)?;
    let client = Sftp::new(
        Box::new(Polling {
            io: reader,
            gate: gate.clone(),
        }) as Box<dyn Read + Send>,
        Box::new(Polling {
            io: writer,
            gate: gate.clone(),
        }) as Box<dyn Write + Send>,
    )?;
    Ok(Some(PrivateSftp {
        client,
        _lease: channel,
    }))
}

fn logical_fallback(
    browser_client: &mut Sftp<impl Read, impl Write>,
    request: SizeRequest,
    opener: &SftpOpener,
    gate: &Gate,
) -> Result<Sizes> {
    gate.restrict(browser::SCAN_TIMEOUT);
    if let Some(mut private) = open_private(opener, gate)? {
        return browser::measure_logical(&mut private.client, request, || gate.stopped(), true);
    }
    // A server allowing only its original channel keeps the ordinary logical
    // scan, including the existing reconnect behavior when it is interrupted.
    let owner = gate
        .owner
        .upgrade()
        .ok_or_else(|| Failure::disconnected("The original SFTP connection closed."))?;
    let deadline = super::deadline::Guard::start_cancellable(
        &owner,
        browser::SCAN_TIMEOUT,
        gate.cancel.clone(),
    )?;
    let result = browser::measure_logical(browser_client, request, || gate.stopped(), true);
    if deadline.finish() {
        return Err(Failure::disconnected(
            "Folder size calculation timed out. Reconnect to browse the server.",
        ));
    }
    result
}

fn paths(source: &Directory, selected: &[EntryName]) -> io::Result<Vec<Vec<u8>>> {
    if selected.is_empty() || selected.len() > MAX_SELECTION {
        return Err(io::Error::other("Choose up to 256 folders to measure"));
    }
    selected
        .iter()
        .map(|name| match source.path.with(name) {
            Some(Location::Remote(path)) => Ok(path),
            _ => Err(changed()),
        })
        .collect()
}

fn check_primary_roots(
    client: &mut Sftp<impl Read, impl Write>,
    request: &SizeRequest,
    paths: &[Vec<u8>],
    gate: &Gate,
) -> Result<Vec<Attributes>> {
    let timeout = gate.remaining()?.min(browser::SCAN_TIMEOUT);
    let owner = gate
        .owner
        .upgrade()
        .ok_or_else(|| Failure::disconnected("The original SFTP connection closed."))?;
    let deadline = super::deadline::Guard::start_cancellable(&owner, timeout, gate.cancel.clone())?;
    let result = checked_roots(client, request, paths, gate);
    if deadline.finish() {
        return Err(Failure::disconnected(
            "Folder verification timed out. Reconnect to browse the server.",
        ));
    }
    result
}

fn checked_roots(
    client: &mut Sftp<impl Read, impl Write>,
    request: &SizeRequest,
    paths: &[Vec<u8>],
    gate: &Gate,
) -> Result<Vec<Attributes>> {
    let Location::Remote(parent) = &request.source.path else {
        return Err(changed().into());
    };
    gate.interval()?;
    if client.lstat(parent)?.kind() != FileKind::Directory
        || client.canonicalize(parent)? != *parent
    {
        return Err(changed().into());
    }
    request
        .selected
        .iter()
        .zip(paths)
        .map(|(name, path)| {
            gate.interval()?;
            let entry = request
                .source
                .entries
                .iter()
                .find(|entry| entry.name == *name && entry.kind == EntryKind::Directory)
                .ok_or_else(changed)?;
            let actual = client.lstat(path)?;
            if actual.kind() != FileKind::Directory
                || entry
                    .modified
                    .is_some_and(|value| actual.modified != Some(value))
                || entry
                    .mode
                    .is_some_and(|value| actual.permissions != Some(value))
                || entry.owner.is_some_and(|value| actual.uid != Some(value))
                || entry.group.is_some_and(|value| actual.gid != Some(value))
                || client.canonicalize(path)? != *path
            {
                return Err(changed().into());
            }
            Ok(actual)
        })
        .collect()
}

fn same_folder(before: Attributes, after: Attributes) -> bool {
    before.kind() == after.kind()
        && before.size == after.size
        && before.uid == after.uid
        && before.gid == after.gid
        && before.permissions == after.permissions
        && before.modified == after.modified
}

fn changed() -> io::Error {
    io::Error::other("The selected folder changed. Refresh the listing.")
}

fn missing_stream() -> io::Error {
    io::Error::other("A server measurement stream was unavailable.")
}

#[derive(Default)]
struct Output {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Output {
    fn push(&mut self, stream: u8, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > OUTPUT_LIMIT - self.stdout.len() - self.stderr.len() {
            return Err(io::Error::other(
                "The server measurement exceeded its output limit.",
            ));
        }
        match stream {
            0 => self.stdout.extend_from_slice(bytes),
            1 => self.stderr.extend_from_slice(bytes),
            _ => {
                return Err(io::Error::other(
                    "The isolated server returned an invalid stream tag.",
                ));
            }
        }
        Ok(())
    }

    fn sftp_only(&self) -> bool {
        const ONLY: &[u8] = b"This service allows sftp connections only.";
        [&self.stdout, &self.stderr].iter().any(|stream| {
            stream
                .split(|&byte| byte == b'\n')
                .any(|line| line.trim_ascii().eq_ignore_ascii_case(ONLY))
        })
    }
}

fn run(
    opener: &ExecOpener,
    command: &str,
    paths: &[Vec<u8>],
    gate: &Gate,
) -> Result<Option<(u32, Output)>> {
    gate.interval()?;
    if opener.verified_host_key().is_none() || opener.is_closed() {
        return Ok(None);
    }
    let mut output = Output::default();
    let exit = match opener {
        ExecOpener::Direct(opener) => {
            let during_open = gate.clone();
            let before_exec = gate.clone();
            let opened = opener.open_guarded(
                command,
                move || during_open.stopped(),
                move || before_exec.interval().map(|_| ()),
            );
            let Ok(mut channel) = opened else {
                gate.interval()?;
                return Ok(None);
            };
            let mut streams = [
                Some(channel.take_reader().ok_or_else(missing_stream)?),
                Some(channel.take_stderr().ok_or_else(missing_stream)?),
            ];
            let mut buffer = [0; 8192];
            while streams.iter().any(Option::is_some) {
                for (tag, stream) in streams.iter_mut().enumerate() {
                    let Some(reader) = stream else {
                        continue;
                    };
                    match reader.read_timeout(&mut buffer, gate.interval()?) {
                        Ok(0) => *stream = None,
                        Ok(n) => output.push(tag as u8, &buffer[..n])?,
                        Err(error) if retry_io(&error) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            loop {
                if let Some(exit) = channel.exit_code() {
                    break exit;
                }
                if channel.abort_handle().is_closed() {
                    return Err(io::Error::other(
                        "The server measurement ended without an exit status.",
                    )
                    .into());
                }
                thread::sleep(gate.interval()?);
            }
        }
        ExecOpener::Worker(opener) => {
            let opened = opener.open_folder_usage(paths, &|| gate.stopped());
            let Ok(mut channel) = opened else {
                gate.interval()?;
                return Ok(None);
            };
            let mut reader = channel.take_reader().ok_or_else(missing_stream)?;
            collect_tagged(&mut reader, &mut output, gate)?;
            channel.exit_code().ok_or_else(|| {
                io::Error::other("The isolated server measurement ended without an exit status.")
            })?
        }
    };
    Ok(Some((exit, output)))
}

fn collect_tagged(
    reader: &mut crate::ssh_worker::Reader,
    output: &mut Output,
    gate: &Gate,
) -> io::Result<()> {
    let mut header = [0; 3];
    let mut buffer = [0; 8192];
    while read_tagged(reader, &mut header, gate, true)? {
        let mut remaining = usize::from(u16::from_be_bytes([header[1], header[2]]));
        if remaining == 0 || header[0] > 1 {
            return Err(io::Error::other(
                "The isolated server returned invalid measurement output.",
            ));
        }
        while remaining != 0 {
            let count = remaining.min(buffer.len());
            read_tagged(reader, &mut buffer[..count], gate, false)?;
            output.push(header[0], &buffer[..count])?;
            remaining -= count;
        }
    }
    Ok(())
}

fn read_tagged(
    reader: &mut crate::ssh_worker::Reader,
    mut bytes: &mut [u8],
    gate: &Gate,
    mut may_end: bool,
) -> io::Result<bool> {
    while !bytes.is_empty() {
        match reader.read_timeout(bytes, gate.interval()?) {
            Ok(0) if may_end => return Ok(false),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => {
                bytes = &mut bytes[count..];
                may_end = false;
            }
            Err(error) if retry_io(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}

fn result(request: SizeRequest, bytes: HashMap<EntryName, u64>, incomplete: bool) -> Result<Sizes> {
    if bytes.is_empty() {
        return Err(io::Error::other("The server reported no readable folder sizes.").into());
    }
    let total = total(&bytes)?;
    let partial = incomplete || bytes.len() != request.selected.len();
    Ok(Sizes {
        source: request.source,
        requested: request.selected,
        bytes,
        total,
        detail: Detail::Allocated { partial },
    })
}

fn total(bytes: &HashMap<EntryName, u64>) -> io::Result<u64> {
    bytes
        .values()
        .try_fold(0_u64, |total, &value| total.checked_add(value))
        .ok_or_else(size_overflow)
}

fn parse(
    output: &[u8],
    paths: &[Vec<u8>],
    names: &[EntryName],
) -> io::Result<HashMap<EntryName, u64>> {
    let expected = paths
        .iter()
        .map(Vec::as_slice)
        .zip(names)
        .collect::<HashMap<_, _>>();
    let mut measured = HashMap::new();
    for line in output.split(|&byte| byte == b'\n') {
        let Some(tab) = line.iter().position(|&byte| byte == b'\t') else {
            continue;
        };
        let (number, path) = (&line[..tab], &line[tab + 1..]);
        let name = expected
            .get(path)
            .or_else(|| path.strip_suffix(b"/").and_then(|path| expected.get(path)));
        let Some(&name) = name else {
            continue;
        };
        let number = number.trim_ascii();
        if number.is_empty() || !number.iter().all(u8::is_ascii_digit) {
            continue;
        }
        let kib = std::str::from_utf8(number)
            .expect("validated ASCII digits")
            .parse::<u64>()
            .map_err(|_| size_overflow())?;
        let bytes = kib.checked_mul(1024).ok_or_else(size_overflow)?;
        if measured.insert(name.clone(), bytes).is_some() {
            return Err(io::Error::other("The server reported a folder size twice."));
        }
    }
    Ok(measured)
}

fn size_overflow() -> io::Error {
    io::Error::other("Folder size exceeds the supported range.")
}
