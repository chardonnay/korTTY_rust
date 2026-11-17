//! Discovers installed Windows shell programs and resolves their exact argv.

use std::env;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Command, Stdio};
use std::ptr::null_mut;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::System::Pipes::PeekNamedPipe;
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::{LaunchOptions, LocalShell};

pub(super) fn launch_options(options: &LaunchOptions) -> io::Result<Option<LaunchOptions>> {
    if !matches!(
        options.shell,
        LocalShell::GitBash | LocalShell::Cygwin | LocalShell::Wsl
    ) {
        return Ok(None);
    }
    if options.shell == LocalShell::Wsl && options.sandbox.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the Windows shell sandbox cannot isolate a WSL distribution",
        ));
    }
    let executable = program(options.shell)?
        .into_os_string()
        .into_string()
        .map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "the shell path must be Unicode")
        })?;
    let mut command = vec![executable];
    if matches!(options.shell, LocalShell::GitBash | LocalShell::Cygwin) {
        command.extend(["--login".into(), "-i".into()]);
    }
    crate::validate_local_command(&command)?;
    let mut resolved = options.clone();
    resolved.shell = LocalShell::Default;
    resolved.command = Some(command);
    Ok(Some(resolved))
}

pub(super) fn program(preset: LocalShell) -> io::Result<PathBuf> {
    let executable = match preset {
        LocalShell::GitBash => git_bash(),
        LocalShell::Cygwin => cygwin(),
        LocalShell::Wsl => {
            let executable = super::system_directory()?.join("wsl.exe");
            static WSL_INSTALLED: OnceLock<bool> = OnceLock::new();
            (*WSL_INSTALLED
                .get_or_init(|| executable.is_file() && wsl_has_distribution(&executable)))
            .then_some(executable)
        }
        _ => None,
    };
    executable.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{preset} is not installed"),
        )
    })
}

fn environment_directories<'a>(names: &'a [&'a str]) -> impl Iterator<Item = PathBuf> + 'a {
    names
        .iter()
        .filter_map(|name| env::var_os(name).map(PathBuf::from))
}

fn git_bash() -> Option<PathBuf> {
    let path = env::var_os("PATH").unwrap_or_default();
    let common = environment_directories(&["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"])
        .map(|directory| directory.join("Git"));
    let per_user =
        environment_directories(&["LOCALAPPDATA"]).map(|directory| directory.join(r"Programs\Git"));
    let portable = env::split_paths(&path)
        .flat_map(|directory| [directory.parent().map(Path::to_owned), Some(directory)])
        .flatten();
    common
        .chain(per_user)
        .chain(portable)
        .find_map(|root| git_bash_at(&root))
}

fn git_bash_at(root: &Path) -> Option<PathBuf> {
    let executable = root.join(r"bin\bash.exe");
    (root.is_absolute()
        && executable.is_file()
        && (root.join(r"cmd\git.exe").is_file() || root.join("git-bash.exe").is_file()))
    .then_some(executable)
}

fn cygwin() -> Option<PathBuf> {
    let path = env::var_os("PATH").unwrap_or_default();
    let system = super::system_directory().ok();
    let drives = system
        .as_deref()
        .and_then(|directory| directory.ancestors().last())
        .into_iter()
        .chain([Path::new(r"C:\")]);
    let common = drives.flat_map(|drive| [drive.join(r"cygwin64\bin"), drive.join(r"cygwin\bin")]);
    common
        .chain(env::split_paths(&path))
        .find_map(|directory| cygwin_at(&directory))
}

fn cygwin_at(directory: &Path) -> Option<PathBuf> {
    let executable = directory.join("bash.exe");
    (directory.is_absolute() && directory.join("cygwin1.dll").is_file() && executable.is_file())
        .then_some(executable)
}

fn wsl_has_distribution(executable: &Path) -> bool {
    let Ok(mut child) = Command::new(executable)
        .args(["--list", "--quiet"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_millis(1200);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => {
                return status.success() && child.stdout.as_ref().is_some_and(has_distributions);
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

fn has_distributions(stdout: &ChildStdout) -> bool {
    let mut buffer = [0; 4096];
    let mut read = 0;
    // SAFETY: the pipe and the output buffers are live. The child exited, and
    // no other thread accesses this handle. The operation does not wait for EOF.
    let ok = unsafe {
        PeekNamedPipe(
            stdout.as_raw_handle(),
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &mut read,
            null_mut(),
            null_mut(),
        )
    };
    ok != 0 && has_distribution_name(&buffer[..read as usize])
}

fn has_distribution_name(bytes: &[u8]) -> bool {
    let output = if bytes.starts_with(&[0xff, 0xfe]) || bytes.contains(&0) {
        let units: Vec<_> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .copied()
            .map(u16::from_le_bytes)
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    output
        .chars()
        .any(|character| !character.is_whitespace() && !matches!(character, '\0' | '\u{feff}'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_distribution_probe_accepts_utf16_and_utf8_names() {
        assert!(!has_distribution_name(b""));
        assert!(!has_distribution_name(&[0xff, 0xfe, 13, 0, 10, 0]));
        assert!(has_distribution_name(b"Fedora\r\n"));
        let utf16: Vec<_> = "Ubuntu-24.04\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert!(has_distribution_name(&utf16));
    }
}
