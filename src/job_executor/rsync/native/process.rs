use std::io::{self, Read};
use std::os::fd::AsFd as _;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use rustix::{fs, process};

use super::POLL;

pub(super) struct Process {
    child: Child,
    group: Option<process::Pid>,
}

impl Process {
    pub(super) fn start(command: &mut Command) -> io::Result<Self> {
        let child = command.process_group(0).spawn()?;
        let group = process::Pid::from_raw(child.id().cast_signed());
        Ok(Self { child, group })
    }

    pub(super) fn poll(&mut self) -> io::Result<Option<ExitStatus>> {
        let group = self
            .group
            .ok_or_else(|| io::Error::other("The rsync process already exited"))?;
        let flags = process::WaitIdOptions::EXITED
            | process::WaitIdOptions::NOHANG
            | process::WaitIdOptions::NOWAIT;
        match process::waitid(process::WaitId::Pid(group), flags) {
            Ok(None) | Err(rustix::io::Errno::INTR) => return Ok(None),
            Err(error) => return Err(error.into()),
            Ok(Some(_)) => {}
        }
        self.stop_group();
        self.child.wait().map(Some)
    }

    fn stop_group(&mut self) {
        if let Some(group) = self.group.take() {
            // The leader remains unreaped while signalling its owned group.
            let _ = process::kill_process_group(group, process::Signal::KILL);
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop_group();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(super) fn check_version(program: &Path, cancelled: &impl Fn() -> bool) -> io::Result<()> {
    const LIMIT: usize = 16 * 1024;
    let mut child = Process::start(
        Command::new(program)
            .env_clear()
            .env("LANG", "C")
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )?;
    let mut stdout = child.child.stdout.take().ok_or_else(unsupported)?;
    fs::fcntl_setfl(
        stdout.as_fd(),
        fs::fcntl_getfl(stdout.as_fd())? | fs::OFlags::NONBLOCK,
    )?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut captured = Vec::new();
    let mut buffer = [0; 1024];
    let mut status = None;
    let mut eof = false;
    while !eof || status.is_none() {
        if cancelled() || Instant::now() >= deadline {
            return Err(io::Error::other("The approved rsync version check stopped"));
        }
        if !eof {
            match stdout.read(&mut buffer) {
                Ok(0) => eof = true,
                Ok(count) => {
                    if captured.len() + count > LIMIT {
                        return Err(unsupported());
                    }
                    captured.extend_from_slice(&buffer[..count]);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        if status.is_none() {
            status = child.poll()?;
        }
        if !eof || status.is_none() {
            std::thread::sleep(POLL);
        }
    }
    if status.is_some_and(|status| status.success()) && supported(&captured) {
        Ok(())
    } else {
        Err(unsupported())
    }
}

fn supported(output: &[u8]) -> bool {
    let Ok(output) = std::str::from_utf8(output) else {
        return false;
    };
    let Some(line) = output.lines().next() else {
        return false;
    };
    let mut fields = line.split_whitespace();
    if fields.next() != Some("rsync") || fields.next() != Some("version") {
        return false;
    }
    let Some(version) = fields.next() else {
        return false;
    };
    let version = if let Some((number, suffix)) = version.split_once('-') {
        if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return false;
        }
        number
    } else {
        version
    };
    let mut numbers = version.split('.').map(str::parse::<u32>);
    matches!((numbers.next(), numbers.next(), numbers.next(), numbers.next()),
        (Some(Ok(3)), Some(Ok(minor)), Some(Ok(patch)), None) if minor > 4 || minor == 4 && patch >= 1)
}

fn unsupported() -> io::Error {
    io::Error::other("Approve a native rsync 3.4.1 or newer executable from the rsync 3 series")
}

#[cfg(test)]
mod tests {
    use super::supported;

    #[test]
    fn native_rsync_requires_a_recent_version_with_file_list_checks() {
        assert!(supported(b"rsync  version 3.4.1  protocol version 32\n"));
        assert!(supported(b"rsync  version 3.5.0  protocol version 32\n"));
        assert!(supported(
            b"rsync  version 3.5.1-g315819c5  protocol version 33\n"
        ));
        assert!(!supported(b"rsync  version 3.2.7  protocol version 31\n"));
        assert!(!supported(b"openrsync: protocol version 29\n"));
    }
}
