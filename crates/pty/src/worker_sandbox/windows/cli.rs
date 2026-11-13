//! Direct native process launch with private pipes and a Job assigned before execution.

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::{self, PipeReader, PipeWriter};
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, OwnedHandle};
use std::path::Path;
use std::ptr::null_mut;

use super::{
    Attributes, DUPLICATE_SAME_ACCESS, DuplicateHandle, GetCurrentProcess, GetExitCodeProcess,
    JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, PROC_THREAD_ATTRIBUTE_PARENT_PROCESS, REAP_TIMEOUT_MS,
    ResumeThread, bool_result, job, launch_in, pipe, policy, signaled, wait, wide,
};

/// A direct, user-trusted process with a non-breakaway lifetime Job. This is not a sandbox.
pub struct WindowsCli {
    process: OwnedHandle,
    thread: Option<OwnedHandle>,
    job: Option<OwnedHandle>,
    pub input: Option<PipeWriter>,
    pub output: Option<PipeReader>,
    pub diagnostics: Option<PipeReader>,
}

impl WindowsCli {
    pub fn prepare(executable: &Path, arguments: &[String], directory: &Path) -> io::Result<Self> {
        Self::prepare_io(executable, arguments, directory, false)
    }

    /// Prepares an approved executable and separately captures its diagnostic stream.
    pub fn prepare_with_diagnostics(
        executable: &Path,
        arguments: &[String],
        directory: &Path,
    ) -> io::Result<Self> {
        Self::prepare_io(executable, arguments, directory, true)
    }

    fn prepare_io(
        executable: &Path,
        arguments: &[String],
        directory: &Path,
        capture_diagnostics: bool,
    ) -> io::Result<Self> {
        let path = executable.as_os_str();
        let cwd = directory.as_os_str();
        if !executable.is_absolute()
            || !directory.is_absolute()
            || path.encode_wide().any(|unit| matches!(unit, 0 | 34))
            || cwd.encode_wide().any(|unit| unit == 0)
            || arguments.iter().any(|argument| argument.contains('\0'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid CLI paths or arguments",
            ));
        }
        let windows = policy::Environment::read()?;
        let mut environment: Vec<_> = "SystemRoot=".encode_utf16().collect();
        environment.extend_from_slice(&windows.system_root);
        for name in ["TEMP=", "TMP="] {
            environment.extend(name.encode_utf16());
            environment.extend(wide(cwd));
        }
        environment.push(0);
        let directory = wide(cwd);

        let (input, child_input) = pipe::pair(pipe::DesktopEnd::Send)?;
        let (output, child_output) = pipe::pair(pipe::DesktopEnd::Receive)?;
        let (diagnostics, child_error) = if capture_diagnostics {
            let (diagnostics, child_error) = pipe::pair(pipe::DesktopEnd::Receive)?;
            (Some(diagnostics.into()), child_error)
        } else {
            (
                None,
                OpenOptions::new().write(true).open(r"\\.\NUL")?.into(),
            )
        };

        // The inheritable pipe handles only exist in this suspended parent. They cannot leak to
        // other processes started by the desktop while this command is being prepared.
        let helper_job = job(JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK)?;
        let helper_jobs = [helper_job.as_raw_handle()];
        let mut attributes = Attributes::new(1)?;
        attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &helper_jobs)?;
        let helper = launch_in(
            &std::env::current_exe()?,
            None,
            &environment,
            &directory,
            None,
            attributes,
        )?;
        let parent = helper.process.as_raw_handle();
        let mut pipes = [null_mut(); 3];
        for (source, destination) in [
            child_input.as_raw_handle(),
            child_output.as_raw_handle(),
            child_error.as_raw_handle(),
        ]
        .into_iter()
        .zip(&mut pipes)
        {
            // SAFETY: all source handles and the suspended recipient are live. The helper's
            // Job owns the lifetime of all handle copies in that process.
            bool_result(unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    source,
                    parent,
                    destination,
                    0,
                    1,
                    DUPLICATE_SAME_ACCESS,
                )
            })?;
        }

        let lifetime = job(0)?;
        let jobs = [lifetime.as_raw_handle()];
        let mut attributes = Attributes::new(3)?;
        attributes.set(PROC_THREAD_ATTRIBUTE_PARENT_PROCESS, &parent)?;
        attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &pipes)?;
        attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
        let mut command = Vec::new();
        append_argument(&mut command, path);
        for argument in arguments {
            command.push(b' ' as u16);
            append_argument(&mut command, OsStr::new(argument));
        }
        command.push(0);
        if command.len() > 32_767 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "CLI arguments exceed the Windows launch limit",
            ));
        }
        let child = launch_in(
            executable,
            Some(&mut command),
            &environment,
            &directory,
            Some(pipes),
            attributes,
        )?;
        let process = Self {
            process: child.process,
            thread: Some(child.thread),
            job: Some(lifetime),
            input: Some(input.into()),
            output: Some(output.into()),
            diagnostics,
        };
        drop((child_input, child_output, child_error, helper_job));
        wait(&helper.process, REAP_TIMEOUT_MS)?;
        Ok(process)
    }

    pub fn resume(&mut self) -> io::Result<()> {
        let thread = self
            .thread
            .take()
            .ok_or_else(|| io::Error::other("The CLI was already resumed"))?;
        // SAFETY: the primary thread belongs to our suspended child, already in its Job.
        if unsafe { ResumeThread(thread.as_raw_handle()) } != 1 {
            return Err(io::Error::other("Could not resume the CLI process"));
        }
        Ok(())
    }

    pub fn try_success(&self) -> io::Result<Option<bool>> {
        if !signaled(&self.process, 0)? {
            return Ok(None);
        }
        let mut code = 0;
        // SAFETY: the process handle and output pointer are both live.
        bool_result(unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) })?;
        Ok(Some(code == 0))
    }
}

impl Drop for WindowsCli {
    fn drop(&mut self) {
        drop(self.job.take());
        let _ = wait(&self.process, REAP_TIMEOUT_MS);
    }
}

/// Escape for the standard Windows argv convention. No command interpreter is involved.
fn append_argument(command: &mut Vec<u16>, argument: &OsStr) {
    let quote = b'"' as u16;
    let slash = b'\\' as u16;
    command.push(quote);
    let mut slashes = 0;
    for unit in argument.encode_wide() {
        if unit == slash {
            slashes += 1;
        } else {
            command.extend(std::iter::repeat_n(
                slash,
                if unit == quote {
                    2 * slashes + 1
                } else {
                    slashes
                },
            ));
            command.push(unit);
            slashes = 0;
        }
    }
    command.extend(std::iter::repeat_n(slash, 2 * slashes));
    command.push(quote);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_cli_quotes_each_literal_argument() {
        let mut command = Vec::new();
        append_argument(&mut command, OsStr::new(r#"a b\"c\"#));
        assert_eq!(String::from_utf16(&command).unwrap(), r#""a b\\\"c\\""#);
    }
}
