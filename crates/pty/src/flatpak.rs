//! Local host processes for the Flatpak package.

use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::shell_integration::unix::{Startup, default_snippet, trusted_permissions};
use crate::{LaunchOptions, LocalShell};

mod supervised;
pub use supervised::{HostCommandLease, spawn_approved_command};

const SPAWN: &str = "/usr/bin/flatpak-spawn";
const CONTEXT_LIMIT: usize = 16 * 1024;
const CONTEXT_TIMEOUT: Duration = Duration::from_secs(4);
const HOST_CONTEXT: &str = r#"
set -eu
dir=${1:-${HOME:-/}}
CDPATH= cd -P -- "$dir"
program=$2
if [ -z "$program" ]; then
    program=${SHELL:-/bin/bash}
    case "$program" in
        /*) ;;
        *) program=/bin/bash ;;
    esac
    if [ ! -f "$program" ] || [ ! -x "$program" ]; then
        program=/bin/sh
    fi
fi
case "$program" in
    /*) ;;
    */*) program="$PWD/$program" ;;
    *) program=$(command -v -- "$program") ;;
esac
case "$program" in
    /*) ;;
    *) program="$PWD/$program" ;;
esac
[ -f "$program" ] && [ -x "$program" ]
printf '%s\000%s\000%s\000' "$program" "$PWD" "${ZDOTDIR:-}"
"#;

/// Flatpak creates this file in the sandbox mount namespace.
pub fn running() -> bool {
    Path::new("/.flatpak-info").is_file()
}

pub(crate) fn routes_to_host(options: &LaunchOptions) -> bool {
    options.sandbox.is_none() && running()
}

/// Builds a host command for an explicitly selected executable and directory.
pub fn host_command(program: &Path, directory: &Path) -> io::Result<Command> {
    let mut command = launcher(directory)?;
    if !program.is_absolute() {
        return Err(invalid("Flatpak host executable must be absolute"));
    }
    command.arg(program);
    Ok(command)
}

/// Verifies that the application and the host refer to the same directory.
pub fn shared_directory(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(invalid("Flatpak host directory must be absolute"));
    }
    let canonical = path.canonicalize()?;
    let metadata = canonical.metadata()?;
    if !metadata.is_dir() {
        return Err(invalid("Flatpak host path is not a directory"));
    }
    let mut command = host_command(Path::new("/usr/bin/stat"), Path::new("/"))?;
    command
        .args(["-L", "-c", "%d:%i", "--"])
        .arg(path)
        .arg(&canonical);
    let expected = format!("{0}:{1}\n{0}:{1}\n", metadata.dev(), metadata.ino());
    if probe(&mut command)? != expected.as_bytes() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the host directory is not shared with the Flatpak application",
        ));
    }
    Ok(canonical)
}

fn launcher(directory: &Path) -> io::Result<Command> {
    if !directory.is_absolute() {
        return Err(invalid("Flatpak host working directory must be absolute"));
    }
    let mut command = Command::new(SPAWN);
    command.args(["--host", "--watch-bus"]);
    let mut argument = OsString::from("--directory=");
    argument.push(directory);
    command.arg(argument);
    Ok(command)
}

pub(crate) fn pty_command(
    options: &LaunchOptions,
    term: &str,
) -> io::Result<(Command, Option<Startup>)> {
    let (requested, arguments) = match &options.command {
        Some(words) => (words[0].as_str(), words[1..].to_vec()),
        None => match options.shell {
            LocalShell::Default => ("", vec!["-l".into()]),
            LocalShell::Bash => ("bash", Vec::new()),
            LocalShell::Zsh => ("zsh", Vec::new()),
            _ => return Err(invalid("the selected local shell requires Windows")),
        },
    };
    let context = context(requested, options.working_directory.as_deref())?;
    let mut launcher = launcher(&context.directory)?;
    environment_argument(&mut launcher, OsStr::new("TERM"), OsStr::new(term));
    for name in ["COLORTERM", "LANG"] {
        if let Some(value) = env::var_os(name).filter(|value| !value.is_empty()) {
            environment_argument(&mut launcher, OsStr::new(name), &value);
        }
    }
    let mut program = Command::new(&context.program);
    let startup = if options.shell_integration {
        let snippet = default_snippet(context.program.as_os_str()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "the selected host shell does not support automatic integration",
            )
        })?;
        let cache = env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute() && path.is_dir())
            .ok_or_else(|| invalid("Flatpak host-visible cache directory is unavailable"))?;
        let startup = Startup::prepare_at(
            &mut program,
            snippet,
            &arguments,
            &startup_directory(&cache)?,
            context.zdotdir.as_deref(),
        )?;
        for (name, value) in program.get_envs() {
            environment_argument(&mut launcher, name, value.unwrap_or(OsStr::new("")));
        }
        Some(startup)
    } else {
        program.args(arguments);
        None
    };
    launcher.arg(program.get_program()).args(program.get_args());
    Ok((launcher, startup))
}

fn startup_directory(cache: &Path) -> io::Result<PathBuf> {
    let canonical = cache.canonicalize()?;
    let local = canonical.metadata()?;
    let expected = format!("{}:{}", local.dev(), local.ino());
    let mut command = host_command(Path::new("/usr/bin/stat"), Path::new("/"))?;
    command
        .args(["-c", "%d:%i %u %f", "--"])
        .args(canonical.ancestors());
    let output = probe(&mut command)?;
    let output = std::str::from_utf8(&output)
        .map_err(|_| invalid("invalid Flatpak host startup directory metadata"))?;
    let mut rows = output.lines();
    // Flatpak remaps ownership on synthetic ancestors such as /home. Check
    // permissions on the host, and require the cache itself to be shared.
    for (index, _) in canonical.ancestors().enumerate() {
        let safe = rows.next().is_some_and(|row| {
            let fields = row.split_ascii_whitespace().collect::<Vec<_>>();
            let [identity, owner, mode] = fields.as_slice() else {
                return false;
            };
            let (Ok(owner), Ok(mode)) = (owner.parse(), u32::from_str_radix(mode, 16)) else {
                return false;
            };
            (index != 0 || *identity == expected)
                && mode & libc::S_IFMT == libc::S_IFDIR
                && trusted_permissions(owner, mode)
        });
        if !safe {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe Flatpak host startup directory",
            ));
        }
    }
    Ok(canonical)
}

fn environment_argument(command: &mut Command, name: &OsStr, value: &OsStr) {
    if let (Some(name), Some(value)) = (name.to_str(), value.to_str()) {
        command.arg(format!("--env={name}={value}"));
    }
}

struct Context {
    program: PathBuf,
    directory: PathBuf,
    zdotdir: Option<OsString>,
}

fn context(program: &str, directory: Option<&Path>) -> io::Result<Context> {
    let mut command = host_command(Path::new("/bin/sh"), Path::new("/"))?;
    command
        .args(["-c", HOST_CONTEXT, "kortty-host-context"])
        .arg(directory.map_or(OsStr::new(""), Path::as_os_str))
        .arg(program);
    parse_context(&probe(&mut command)?)
}

fn parse_context(bytes: &[u8]) -> io::Result<Context> {
    let invalid_context = || invalid("invalid Flatpak host shell configuration");
    let [program, directory, zdotdir, []] = bytes.split(|byte| *byte == 0).collect::<Vec<_>>()[..]
    else {
        return Err(invalid_context());
    };
    let absolute = |bytes: &[u8]| {
        let path = Path::new(OsStr::from_bytes(bytes));
        path.is_absolute()
            .then(|| path.to_owned())
            .ok_or_else(invalid_context)
    };
    Ok(Context {
        program: absolute(program)?,
        directory: absolute(directory)?,
        zdotdir: (!zdotdir.is_empty()).then(|| OsStr::from_bytes(zdotdir).to_owned()),
    })
}

fn probe(command: &mut Command) -> io::Result<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let result = (|| {
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing Flatpak host command output"))?;
        set_nonblocking(stdout.as_raw_fd())?;
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        let deadline = Instant::now() + CONTEXT_TIMEOUT;
        while Instant::now() < deadline {
            match stdout.read(&mut buffer) {
                Ok(count) if count > 0 => {
                    if bytes.len() + count > CONTEXT_LIMIT {
                        return Err(invalid("Flatpak host shell configuration is too large"));
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                    continue;
                }
                Ok(_) => {
                    if let Some(status) = child.try_wait()? {
                        return if status.success() {
                            Ok(bytes)
                        } else {
                            Err(io::Error::other(
                                "Flatpak could not verify the host shell or working directory",
                            ))
                        };
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Flatpak host shell lookup timed out",
        ))
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn set_nonblocking(fd: std::os::fd::RawFd) -> io::Result<()> {
    // SAFETY: the caller holds the descriptor and fcntl changes only its status flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
