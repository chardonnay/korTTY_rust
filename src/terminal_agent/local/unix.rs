use std::io::{self, Read};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use rustix::{fs, process};
use zeroize::Zeroizing;

use super::super::runner::{COMMAND_LIMIT, Cancel, Captured, Outcome, POLL, Status};
use super::{Shell, exit_status};

pub(super) fn run(shell: &Shell, cwd: &Path, text: &str, cancel: &Cancel) -> io::Result<Outcome> {
    let mut command = Command::new(shell.executable());
    command
        .arg("-c")
        .arg(text)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut process = Process::new(command.spawn()?);
    let mut stdout = process.child.stdout.take().ok_or_else(missing_pipe)?;
    let mut stderr = process.child.stderr.take().ok_or_else(missing_pipe)?;
    for pipe in [stdout.as_fd(), stderr.as_fd()] {
        fs::fcntl_setfl(pipe, fs::fcntl_getfl(pipe)? | fs::OFlags::NONBLOCK)?;
    }
    let mut stdout = Output::new(&mut stdout);
    let mut stderr = Output::new(&mut stderr);
    let deadline = Instant::now() + COMMAND_LIMIT;
    let mut exit = None;
    let status = loop {
        if cancel.requested() {
            break Status::Cancelled;
        }
        if Instant::now() >= deadline {
            break Status::TimedOut;
        }
        let progressed = stdout.read() | stderr.read();
        if stdout.failed || stderr.failed {
            break Status::TransportFailed;
        }
        if exit.is_none() {
            exit = process.poll_exit()?;
        }
        if stdout.done
            && stderr.done
            && let Some(status) = exit
        {
            break status;
        }
        if !progressed {
            std::thread::sleep(POLL);
        }
    };
    drop(process);
    Ok(Outcome {
        status,
        stdout: stdout.captured,
        stderr: stderr.captured,
    })
}

fn missing_pipe() -> io::Error {
    io::Error::other("Local command output pipe was unavailable")
}

struct Process {
    child: Child,
    group: Option<process::Pid>,
}

impl Process {
    fn new(child: Child) -> Self {
        let group = process::Pid::from_raw(child.id().cast_signed());
        Self { child, group }
    }

    fn poll_exit(&mut self) -> io::Result<Option<Status>> {
        let group = self
            .group
            .ok_or_else(|| io::Error::other("Local command process group was unavailable"))?;
        let flags = process::WaitIdOptions::EXITED
            | process::WaitIdOptions::NOHANG
            | process::WaitIdOptions::NOWAIT;
        match process::waitid(process::WaitId::Pid(group), flags) {
            Ok(None) | Err(rustix::io::Errno::INTR) => return Ok(None),
            Err(error) => return Err(error.into()),
            Ok(Some(_)) => {}
        }
        // Keep the leader unreaped until all remaining processes in its owned group are stopped.
        let _ = process::kill_process_group(group, process::Signal::KILL);
        self.group = None;
        self.child.wait().map(exit_status).map(Some)
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if let Some(group) = self.group {
            // The leader has not been reaped, so its PID cannot be reused.
            let _ = process::kill_process_group(group, process::Signal::KILL);
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

struct Output<'a, R> {
    reader: &'a mut R,
    captured: Captured,
    done: bool,
    failed: bool,
}

impl<'a, R: Read> Output<'a, R> {
    fn new(reader: &'a mut R) -> Self {
        Self {
            reader,
            captured: Captured::default(),
            done: false,
            failed: false,
        }
    }

    fn read(&mut self) -> bool {
        if self.done {
            return false;
        }
        let mut buffer = Zeroizing::new([0; 8192]);
        match self.reader.read(&mut buffer[..]) {
            Ok(0) => self.done = true,
            Ok(count) => self.captured.push(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return true,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return false,
            Err(_) => self.failed = true,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::snippets::tests::Profile;

    #[test]
    fn agent_local_command_kills_owned_background_processes_before_returning() {
        let profile = Profile::new();
        let command = "( sleep 1; printf leaked > escaped ) >/dev/null 2>&1 & printf '%s\\n' \"$!\"; printf 'stderr marker' >&2; exit 7";
        let result = run(
            &Shell::Posix(PathBuf::from("/bin/sh")),
            &profile.0,
            command,
            &Cancel::default(),
        )
        .unwrap();
        assert_eq!(result.status, Status::Exited(7));
        let child = result.stdout.text().trim().parse::<u32>().unwrap();
        assert!(child > 1);
        assert_eq!(&*result.stderr.text(), "stderr marker");
        std::thread::sleep(Duration::from_millis(1200));
        assert!(
            !profile.0.join("escaped").exists(),
            "owned child {child} outlived the command"
        );
    }
}
