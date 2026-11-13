//! Unix CLI launch with explicit argv, environment and executable semantics.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::ptr::null;

const MAX_ARGUMENTS: usize = 24;

/// Launches an already approved executable with piped stdin/stdout and its own process group.
/// Uses execve so a malformed image returns ENOEXEC without invoking a shell interpreter.
pub fn spawn_cli(
    executable: &Path,
    argv0: &Path,
    arguments: &[String],
    directory: &Path,
) -> io::Result<Child> {
    spawn(executable, argv0, arguments, directory, Stdio::null())
}

/// Launches an already approved native executable, retaining its diagnostic stream.
pub fn spawn_cli_with_diagnostics(
    executable: &Path,
    argv0: &Path,
    arguments: &[String],
    directory: &Path,
) -> io::Result<Child> {
    spawn(executable, argv0, arguments, directory, Stdio::piped())
}

fn spawn(
    executable: &Path,
    argv0: &Path,
    arguments: &[String],
    directory: &Path,
    diagnostics: Stdio,
) -> io::Result<Child> {
    if !executable.is_absolute()
        || !argv0.is_absolute()
        || !directory.is_absolute()
        || arguments.len() > MAX_ARGUMENTS
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid CLI executable, working directory or argument count",
        ));
    }
    let mut command = Command::new(executable);
    command
        .env_clear()
        .current_dir(directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(diagnostics)
        .process_group(0);

    let executable = CString::new(executable.as_os_str().as_bytes())?;
    let mut argv = Vec::with_capacity(arguments.len() + 1);
    argv.push(CString::new(argv0.as_os_str().as_bytes())?);
    for argument in arguments {
        argv.push(CString::new(argument.as_bytes())?);
    }
    let mut temporary = b"TMPDIR=".to_vec();
    temporary.extend_from_slice(directory.as_os_str().as_bytes());
    let temporary = CString::new(temporary)?;

    // SAFETY: All owned C strings were built in the parent. The post-fork closure only forms
    // fixed stack arrays of pointers to them, then calls async-signal-safe execve. On failure it
    // returns the raw OS error to Command's existing exec-error pipe, without trying execvp.
    unsafe {
        command.pre_exec(move || {
            let mut pointers = [null(); MAX_ARGUMENTS + 2];
            for (pointer, argument) in pointers.iter_mut().zip(&argv) {
                *pointer = argument.as_ptr();
            }
            let environment = [temporary.as_ptr(), null()];
            libc::execve(executable.as_ptr(), pointers.as_ptr(), environment.as_ptr());
            Err(io::Error::last_os_error())
        });
    }
    command.spawn()
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn unix_cli_refuses_malformed_native_image_without_shell_fallback() -> io::Result<()> {
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("kortty-cli-execve-{}-{time}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&root)?;
        let scratch = Scratch(root);
        #[cfg(target_os = "linux")]
        let magic = b"\x7fELF\n";
        #[cfg(target_os = "macos")]
        let magic = b"\xcf\xfa\xed\xfe\n";
        for (name, prefix) in [
            ("text", &b"# No interpreter declaration\n"[..]),
            ("image", &magic[..]),
        ] {
            let malformed = scratch.0.join(name);
            let mut image = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o700)
                .open(&malformed)?;
            image.write_all(prefix)?;
            image.write_all(b"printf 'interpreter fallback' > fallback-marker\nexit 0\n")?;
            drop(image);

            // A concurrent test can briefly inherit the writable descriptor between fork and exec.
            let deadline = Instant::now() + Duration::from_secs(2);
            let error = loop {
                match spawn_cli(&malformed, &malformed, &[], &scratch.0) {
                    Err(error)
                        if error.raw_os_error() == Some(libc::ETXTBSY)
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => break error,
                    Ok(mut child) => {
                        let _ = child.wait();
                        panic!("the malformed CLI image unexpectedly launched");
                    }
                }
            };
            let code = error.raw_os_error();
            let rejected = code == Some(libc::ENOEXEC);
            #[cfg(target_os = "macos")]
            let rejected = rejected
                || name == "image"
                    && matches!(
                        code,
                        Some(libc::EBADEXEC | libc::EBADARCH | libc::EBADMACHO)
                    );
            assert!(rejected, "unexpected image rejection: {error}");
            assert!(!scratch.0.join("fallback-marker").exists());
        }
        Ok(())
    }
}
