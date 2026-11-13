use std::io::{self, Read, Write as _};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd as _;
use std::path::Path;
use std::process::Child;

use rustix::fd::AsFd;
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};
use zeroize::Zeroizing;

use super::{
    Capture, Error, MAX_DIAGNOSTICS, POLL, ProcessInput, ProcessOutput, active, input::Launch,
};

#[cfg(target_os = "linux")]
type Image = std::fs::File;
#[cfg(target_os = "macos")]
type Image = super::macos::Image;

struct Process {
    child: Child,
    group: Pid,
}

impl Process {
    fn start(
        program: &Launch,
        image: &Image,
        directory: &Path,
        capture: Capture,
    ) -> Result<Self, Error> {
        // Linux resolves this pathname through the already verified descriptor in the child.
        // Leave CLOEXEC set. The kernel resolves the ELF image before closing that descriptor.
        #[cfg(target_os = "linux")]
        let descriptor = format!("/proc/self/fd/{}", image.as_raw_fd());
        #[cfg(target_os = "linux")]
        let executable = Path::new(&descriptor);
        #[cfg(target_os = "macos")]
        let executable = image.path();
        let spawn = if capture.diagnostics() {
            kortty_pty::spawn_cli_with_diagnostics
        } else {
            kortty_pty::spawn_cli
        };
        let child = spawn(executable, &program.path, &program.arguments, directory)
            .map_err(|_| Error::Review("Could not start the approved CLI executable.".into()))?;
        let group = Pid::from_child(&child);
        Ok(Self { child, group })
    }

    fn try_success(&self) -> Result<Option<bool>, Error> {
        // Keep the leader unreaped while we own the group ID. Drop can terminate the remaining
        // group without targeting a recycled ID, even after the leader has exited.
        match waitid(
            WaitId::Pid(self.group),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(status) => Ok(status.map(|status| status.exit_status() == Some(0))),
            Err(rustix::io::Errno::INTR) => Ok(None),
            Err(_) => Err(Error::Service),
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // This stops the owned process group. A user-trusted program can deliberately leave it.
        let _ = kill_process_group(self.group, Signal::KILL);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn nonblocking(descriptor: impl AsFd) -> Result<(), Error> {
    let flags = fcntl_getfl(&descriptor).map_err(|_| Error::Service)?;
    fcntl_setfl(descriptor, flags | OFlags::NONBLOCK).map_err(|_| Error::Service)
}

pub(super) fn run(
    program: &Launch,
    image: &Image,
    directory: &Path,
    input: ProcessInput<'_>,
    authorize: impl FnOnce() -> Result<(), Error>,
) -> Result<ProcessOutput, Error> {
    let ProcessInput {
        stdin: prompt,
        deadline,
        canceled,
        capture,
    } = input;
    authorize()?;
    let mut process = Process::start(program, image, directory, capture)?;
    let input = process.child.stdin.take().ok_or(Error::Service)?;
    let mut stdout = Pipe::new(process.child.stdout.take().ok_or(Error::Service)?)?;
    let mut diagnostics = process.child.stderr.take().map(Pipe::new).transpose()?;
    nonblocking(&input)?;
    let mut input = Some(input);
    let mut written = 0;
    let mut buffer = Zeroizing::new([0; 8192]);
    loop {
        active(deadline, canceled)?;
        let mut progress = false;
        if let Some(writer) = &mut input {
            match writer.write(&prompt.as_bytes()[written..]) {
                Ok(count) => {
                    written += count;
                    progress |= count > 0;
                    if written == prompt.len() {
                        input = None;
                    }
                }
                Err(error) if retry(&error) => {}
                Err(error)
                    if capture.diagnostics() && error.kind() == io::ErrorKind::BrokenPipe =>
                {
                    input = None
                }
                Err(_) => return Err(Error::Service),
            }
        }
        progress |= stdout.read(
            &mut *buffer,
            capture.output_limit() + usize::from(capture.discard_stdout_tail()),
            (!capture.discard_stdout_tail()).then(|| capture.overflow()),
        )?;
        if let Some(diagnostics) = &mut diagnostics {
            progress |= diagnostics.read(&mut *buffer, MAX_DIAGNOSTICS, None)?;
        }
        if let Some(success) = process.try_success()? {
            if !success && !capture.diagnostics() {
                return Err(Error::Review("The CLI exited unsuccessfully.".into()));
            }
            if stdout.eof && diagnostics.as_ref().is_none_or(|pipe| pipe.eof) && input.is_none() {
                if success && written != prompt.len() {
                    return Err(Error::Service);
                }
                return Ok(ProcessOutput {
                    stdout: stdout.bytes,
                    diagnostics: diagnostics.map_or_else(Zeroizing::default, |pipe| pipe.bytes),
                    success,
                    credential: None,
                });
            }
        }
        if !progress {
            std::thread::sleep(POLL);
        }
    }
}

struct Pipe<R> {
    reader: R,
    bytes: Zeroizing<Vec<u8>>,
    eof: bool,
}

impl<R: Read + AsFd> Pipe<R> {
    fn new(reader: R) -> Result<Self, Error> {
        nonblocking(&reader)?;
        Ok(Self {
            reader,
            bytes: Zeroizing::default(),
            eof: false,
        })
    }

    fn read(
        &mut self,
        buffer: &mut [u8],
        limit: usize,
        overflow: Option<Error>,
    ) -> Result<bool, Error> {
        if self.eof {
            return Ok(false);
        }
        match self.reader.read(buffer) {
            Ok(0) => {
                self.eof = true;
                Ok(true)
            }
            Ok(count) => {
                let remaining = limit - self.bytes.len();
                if count > remaining
                    && let Some(error) = overflow
                {
                    return Err(error);
                }
                self.bytes
                    .extend_from_slice(&buffer[..count.min(remaining)]);
                Ok(true)
            }
            Err(error) if retry(&error) => Ok(false),
            Err(_) => Err(Error::Service),
        }
    }
}

fn retry(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}
