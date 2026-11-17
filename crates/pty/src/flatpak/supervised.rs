//! Keeps an approved host command's process group tied to its caller.

use std::io::{self, PipeReader, PipeWriter, Read, pipe};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Stdio};

use super::{invalid, launcher, set_nonblocking};

const RUN: &str = r#"
identity=$(/usr/bin/stat -L -c '%d:%i' -- .) || exit 125
[ "$identity" = "$1" ] || exit 125
owner=$$
(
    exec 4>&-
    IFS= read -r ignored <&3 || :
    kill -KILL "-$owner"
) </dev/null &
exec 3<&-
/bin/sh -c "$2" 4>&- &
wait "$!"
code=$?
printf '%s\n' "$code" >&4
kill -KILL "-$owner"
"#;

/// Owns the host process group's cancellation signal and private exit result.
///
/// Closing the write end triggers the group watchdog even if the D-Bus
/// forwarding process has already exited. User commands get neither pipe.
pub struct HostCommandLease {
    _lifetime: PipeWriter,
    status: PipeReader,
}

impl HostCommandLease {
    /// Reads the host command's exit code once the Flatpak process has exited.
    pub fn exit_code(&mut self) -> io::Result<Option<u32>> {
        let mut bytes = [0; 16];
        let count = match self.status.read(&mut bytes) {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(std::str::from_utf8(&bytes[..count])
            .ok()
            .and_then(|text| text.strip_suffix('\n'))
            .and_then(|text| text.parse().ok()))
    }
}

/// Starts a reviewed command on the host with its own supervised process group.
pub fn spawn_approved_command(
    directory: &Path,
    command: &str,
) -> io::Result<(Child, HostCommandLease)> {
    let metadata = directory.metadata()?;
    if !metadata.is_dir() {
        return Err(invalid(
            "the reviewed Flatpak host working directory is unavailable",
        ));
    }
    let identity = format!("{}:{}", metadata.dev(), metadata.ino());
    let (watch, lifetime) = pipe()?;
    let (status, report) = pipe()?;
    set_nonblocking(status.as_raw_fd())?;
    let watch = relocated(&watch)?;
    let report = relocated(&report)?;
    let mut launch = launcher(directory)?;
    launch
        .args([
            "--forward-fd=3",
            "--forward-fd=4",
            "/bin/sh",
            "-c",
            RUN,
            "kortty-host-command",
        ])
        .arg(identity)
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    // SAFETY: the owned inputs stay live until spawn. They were duplicated
    // above the fixed destinations. The child only calls async-signal-safe dup2.
    unsafe {
        launch.pre_exec(move || {
            if libc::dup2(watch.as_raw_fd(), 3) < 0 || libc::dup2(report.as_raw_fd(), 4) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = launch.spawn()?;
    Ok((
        child,
        HostCommandLease {
            _lifetime: lifetime,
            status,
        },
    ))
}

fn relocated(file: &impl AsRawFd) -> io::Result<OwnedFd> {
    // SAFETY: the input descriptor is live and fcntl only returns a duplicate.
    let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 16) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fcntl returned a new owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}
