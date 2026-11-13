use std::env;
use std::ffi::{OsStr, OsString, c_void};
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ,
    GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Globalization::{
    CP_OEMCP, CSTR_EQUAL, CompareStringOrdinal, MultiByteToWideChar,
};
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND, ReadFile, WriteFile,
};
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateEventW, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, ResetEvent, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, SetEvent, TerminateProcess, UpdateProcThreadAttribute,
    WaitForMultipleObjects, WaitForSingleObject,
};

use crate::shell_integration::windows::{self as shell_integration, Startup};
use crate::{JobProcessIdentity, LaunchOptions, LocalShell, Size, closed};

#[path = "windows/job_processes.rs"]
mod job_processes;
#[path = "windows/sandbox.rs"]
mod sandbox;
#[path = "windows/sandbox_broker.rs"]
mod sandbox_broker;
pub use sandbox_broker::{WINDOWS_LOCAL_SANDBOX_BROKER_ARGUMENT, run_windows_local_sandbox_broker};

const PIPE_BUFFER: u32 = 64 * 1024;
const REAP_TIMEOUT_MS: u32 = 5000;

type SharedHandle = Arc<Mutex<Option<OwnedHandle>>>;
type SharedConsole = Arc<Mutex<Option<Console>>>;

pub(crate) struct Control {
    process: Arc<OwnedHandle>,
    shell_pid: u32,
    job: Option<Arc<OwnedHandle>>,
    console: SharedConsole,
    stop: Arc<OwnedHandle>,
    output: SharedHandle,
    input: SharedHandle,
    monitor: Option<JoinHandle<io::Result<()>>>,
    startup: Option<Box<Startup>>,
    sandbox: Option<Box<sandbox_broker::SessionFiles>>,
}

#[derive(Clone)]
pub(crate) struct JobProcessProbe {
    shell: Weak<OwnedHandle>,
    job: Weak<OwnedHandle>,
}

impl JobProcessProbe {
    pub(crate) fn find<T>(
        &self,
        previous: Option<JobProcessIdentity>,
        inspect: impl Fn(&str, &[&str]) -> Option<T>,
    ) -> Option<(JobProcessIdentity, T)> {
        let shell = self.shell.upgrade()?;
        let job = self.job.upgrade()?;
        if !matches!(signaled(&shell, 0), Ok(false)) {
            return None;
        }
        let found = job_processes::find(&job, previous, inspect)?;
        matches!(signaled(&shell, 0), Ok(false)).then_some(found)
    }
}

impl Control {
    pub(crate) fn sandbox_backend(&self) -> Option<&'static str> {
        self.sandbox.as_ref().map(|_| sandbox::BACKEND)
    }

    pub(crate) fn shell_pid(&self) -> u32 {
        self.shell_pid
    }

    pub(crate) fn shell_is_only_process(&self) -> bool {
        self.sandbox.is_none()
            && self.job.as_ref().is_some_and(|job| {
                matches!(signaled(&self.process, 0), Ok(false))
                    && job_processes::only_process(job, self.shell_pid)
                    && matches!(signaled(&self.process, 0), Ok(false))
            })
    }

    pub(crate) fn job_process_probe(&self) -> Option<JobProcessProbe> {
        Some(JobProcessProbe {
            shell: Arc::downgrade(&self.process),
            job: Arc::downgrade(self.job.as_ref()?),
        })
    }

    pub(crate) fn spawn(
        size: Size,
        term: &str,
        options: &LaunchOptions,
    ) -> io::Result<(Self, PtyReader, PtyWriter)> {
        let stop = Arc::new(event()?);
        let (output, console_output) = HostPipe::new(PipeDirection::Output, Arc::clone(&stop))?;
        let (input, console_input) = HostPipe::new(PipeDirection::Input, Arc::clone(&stop))?;
        let job = kill_on_close_job()?;
        let console = Console::new(size, &console_input, &console_output)?;
        let launched = if options.sandbox.is_some() {
            sandbox_broker::spawn(&console, &job, term, options)
                .map(|(process, pid, scratch)| (process, pid, None, Some(Box::new(scratch))))
        } else {
            spawn_shell(Some(&console), &job, term, options, None)
                .map(|(process, pid, startup)| (process, pid, startup, None))
        };
        let (process, shell_pid, startup, sandbox) = match launched {
            Ok((process, pid, startup, sandbox)) => (Arc::new(process), pid, startup, sandbox),
            Err(error) => {
                // Older ConPTY versions can wait for their output pipe on close.
                drop(output);
                drop(input);
                drop(console_output);
                drop(console_input);
                return Err(error);
            }
        };
        drop(console_input);
        drop(console_output);

        let mut control = Self {
            process: Arc::clone(&process),
            shell_pid,
            job: Some(Arc::new(job)),
            console: Arc::new(Mutex::new(Some(console))),
            stop: Arc::clone(&stop),
            output: Arc::clone(&output.handle),
            input: Arc::clone(&input.handle),
            monitor: None,
            startup: startup.map(Box::new),
            sandbox,
        };
        let console = Arc::clone(&control.console);
        let monitor = thread::Builder::new()
            .name("kortty-conpty-exit".into())
            .spawn(move || {
                // Natural exit still has a final terminal frame. Close ConPTY
                // independently so the reader can drain it until the pipe breaks.
                if wait_for_either(&stop, &process)? {
                    close_console(&console);
                }
                Ok(())
            });
        match monitor {
            Ok(monitor) => control.monitor = Some(monitor),
            Err(error) => {
                let _ = control.shutdown();
                return Err(error);
            }
        }
        Ok((control, PtyReader(output), PtyWriter(input)))
    }

    pub(crate) fn resize(&mut self, size: Size, _width: u16, _height: u16) -> io::Result<()> {
        let console = self.console.lock().unwrap_or_else(|err| err.into_inner());
        let handle = console.as_ref().ok_or_else(closed)?.0;
        // SAFETY: the console mutex prevents ClosePseudoConsole during resize.
        hresult(unsafe { ResizePseudoConsole(handle, coordinate(size)) })
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if !signaled(&self.process, 0)? {
            return Ok(None);
        }
        let mut code = 0;
        // SAFETY: process is a live handle and code is writable.
        bool_result(unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) })?;
        self.startup.take();
        Ok(Some(ExitStatus::from_raw(code)))
    }

    pub(crate) fn shutdown(&mut self) -> io::Result<()> {
        // SAFETY: stop is the live manual-reset event shared by both I/O paths.
        bool_result(unsafe { SetEvent(self.stop.as_raw_handle()) })?;
        close_pipe(&self.output);
        close_pipe(&self.input);
        let termination = if self.sandbox.is_some() {
            self.job.as_ref().map(|job| terminate_job(job)).transpose()
        } else {
            Ok(None)
        };
        self.job.take();
        close_console(&self.console);
        let monitor = self.monitor.take().map(JoinHandle::join);
        if !signaled(&self.process, REAP_TIMEOUT_MS)? {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "shell did not exit",
            ));
        }
        if let Some(result) = monitor {
            result.map_err(|_| io::Error::other("pseudoconsole exit monitor failed"))??;
        }
        self.startup.take();
        self.sandbox.take();
        termination?;
        Ok(())
    }
}

/// Reads output bytes from the local shell. Shutdown interrupts a blocked read.
pub struct PtyReader(HostPipe);

impl Read for PtyReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let length = buffer.len().min(u32::MAX as usize) as u32;
        if length == 0 {
            return Ok(0);
        }
        match self.0.transfer(|pipe, operation| {
            // SAFETY: buffer remains borrowed until transfer completes or cancels I/O.
            unsafe { ReadFile(pipe, buffer.as_mut_ptr(), length, null_mut(), operation) }
        }) {
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(0),
            result => result,
        }
    }
}

/// Writes input bytes to the local shell. Shutdown interrupts a blocked write.
pub struct PtyWriter(HostPipe);

impl Write for PtyWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let length = buffer.len().min(u32::MAX as usize) as u32;
        if length == 0 {
            return Ok(0);
        }
        self.0.transfer(|pipe, operation| {
            // SAFETY: buffer remains borrowed until transfer completes or cancels I/O.
            unsafe { WriteFile(pipe, buffer.as_ptr(), length, null_mut(), operation) }
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

enum PipeDirection {
    Input,
    Output,
}

struct HostPipe {
    handle: SharedHandle,
    stop: Arc<OwnedHandle>,
    completed: OwnedHandle,
}

impl HostPipe {
    fn new(direction: PipeDirection, stop: Arc<OwnedHandle>) -> io::Result<(Self, OwnedHandle)> {
        let (host_access, console_access) = match direction {
            PipeDirection::Input => (PIPE_ACCESS_OUTBOUND, GENERIC_READ),
            PipeDirection::Output => (PIPE_ACCESS_INBOUND, GENERIC_WRITE),
        };
        let name = pipe_name()?;
        // SAFETY: name is NUL-terminated and the default attributes make the
        // handle non-inheritable. Only the host side uses overlapped I/O.
        let host = owned_handle(unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                host_access | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                PIPE_BUFFER,
                PIPE_BUFFER,
                0,
                null(),
            )
        })?;
        // SAFETY: name identifies the host pipe just created. The console side
        // is a synchronous non-inheritable client handle as ConPTY requires.
        let console = owned_handle(unsafe {
            CreateFileW(
                name.as_ptr(),
                console_access,
                0,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        })?;
        Ok((
            Self {
                handle: Arc::new(Mutex::new(Some(host))),
                stop,
                completed: event()?,
            },
            console,
        ))
    }

    fn transfer(&self, start: impl FnOnce(HANDLE, &mut OVERLAPPED) -> i32) -> io::Result<usize> {
        let handle = self.handle.lock().unwrap_or_else(|err| err.into_inner());
        let pipe = handle.as_ref().ok_or_else(closed)?.as_raw_handle();
        if signaled(&self.stop, 0)? {
            return Err(closed());
        }
        // SAFETY: completed is a live manual-reset event used only by this endpoint.
        bool_result(unsafe { ResetEvent(self.completed.as_raw_handle()) })?;
        let mut operation = OVERLAPPED {
            hEvent: self.completed.as_raw_handle(),
            ..Default::default()
        };
        let mut count = 0;
        if let Err(error) = bool_result(start(pipe, &mut operation)) {
            if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(pipe_error(error));
            }
            let completed = wait_for_either(&self.stop, &self.completed);
            if !matches!(completed, Ok(true)) {
                // SAFETY: the operation has been started on this live handle
                // and still owns its buffers and event until GetOverlappedResult.
                unsafe {
                    CancelIoEx(pipe, &operation);
                    GetOverlappedResult(pipe, &operation, &mut count, 1);
                }
                completed?;
                return Err(closed());
            }
        }
        // SAFETY: the operation completed synchronously or signaled completion.
        bool_result(unsafe { GetOverlappedResult(pipe, &operation, &mut count, 0) })
            .map_err(pipe_error)?;
        Ok(count as usize)
    }
}

impl Drop for HostPipe {
    fn drop(&mut self) {
        close_pipe(&self.handle);
    }
}

struct Console(HPCON);

impl Console {
    fn new(size: Size, input: &OwnedHandle, output: &OwnedHandle) -> io::Result<Self> {
        let mut handle = 0;
        // SAFETY: the input and output are live synchronous pipe endpoints.
        hresult(unsafe {
            CreatePseudoConsole(
                coordinate(size),
                input.as_raw_handle(),
                output.as_raw_handle(),
                0,
                &mut handle,
            )
        })?;
        Ok(Self(handle))
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        // SAFETY: this guard owns the HPCON from CreatePseudoConsole. Its
        // output pipe is either being drained or has been closed.
        unsafe { ClosePseudoConsole(self.0) };
    }
}

struct ProcessAttributes(Vec<usize>);

impl ProcessAttributes {
    fn new(console: Option<&Console>, sandbox_handle: Option<&HANDLE>) -> io::Result<Self> {
        let count = u32::from(console.is_some()) + u32::from(sandbox_handle.is_some());
        let mut bytes = 0;
        // SAFETY: the sizing call takes no storage and writes the required size.
        unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &mut bytes) };
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0_usize; bytes.div_ceil(size_of::<usize>())];
        // SAFETY: storage is pointer-aligned, has at least bytes bytes, and
        // does not move after initialization.
        bool_result(unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), count, 0, &mut bytes)
        })?;
        let mut attributes = Self(storage);
        if let Some(console) = console {
            // SAFETY: the pseudoconsole attribute takes the HPCON value itself.
            unsafe {
                attributes.set(
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                    console.0 as *const c_void,
                    size_of::<HPCON>(),
                )
            }?;
        }
        if let Some(handle) = sandbox_handle {
            // SAFETY: this attribute takes a pointer to the environment handle's storage.
            unsafe {
                attributes.set(
                    sandbox::PROCESS_ATTRIBUTE,
                    std::ptr::from_ref(handle).cast(),
                    size_of::<HANDLE>(),
                )
            }?;
        }
        Ok(attributes)
    }

    unsafe fn set(
        &mut self,
        attribute: usize,
        value: *const c_void,
        bytes: usize,
    ) -> io::Result<()> {
        // SAFETY: the list is live and the caller provides a valid attribute value.
        bool_result(unsafe {
            UpdateProcThreadAttribute(
                self.as_mut_ptr(),
                0,
                attribute,
                value,
                bytes,
                null_mut(),
                null(),
            )
        })
    }

    fn as_mut_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.0.as_mut_ptr().cast()
    }
}

impl Drop for ProcessAttributes {
    fn drop(&mut self) {
        // SAFETY: initialization succeeded and this guard owns the attribute list.
        unsafe { DeleteProcThreadAttributeList(self.as_mut_ptr()) };
    }
}

fn spawn_shell(
    console: Option<&Console>,
    job: &OwnedHandle,
    term: &str,
    options: &LaunchOptions,
    sandbox: Option<&sandbox::Session>,
) -> io::Result<(OwnedHandle, u32, Option<Startup>)> {
    let directory = sandbox
        .map(sandbox::Session::workspace)
        .or(options.working_directory.as_deref());
    let program = if let Some(words) = &options.command {
        custom_program(&words[0], directory)?
    } else {
        shell_program(options.shell)?
    };
    let arguments = options
        .command
        .as_ref()
        .map_or(&[][..], |words| &words[1..]);
    let mut integration = if options.shell_integration && sandbox.is_none() {
        Some(shell_integration::prepare(
            &program,
            arguments,
            options.command.as_ref().map(|words| words[0].as_str()),
        )?)
    } else {
        None
    };
    let (shell, command_line) = {
        // CreateProcessW accepts mixed separators, but cmd.exe can interpret
        // a forward slash in its executable name as command syntax.
        let mut image: Vec<u16> = program
            .as_os_str()
            .encode_wide()
            .map(|unit| {
                if unit == u16::from(b'/') {
                    u16::from(b'\\')
                } else {
                    unit
                }
            })
            .collect();
        let command_line = if let Some(integration) = &integration {
            Some(custom_command_line(&image, &integration.arguments)?)
        } else if options.command.is_some() {
            Some(custom_command_line(&image, arguments)?)
        } else if sandbox.is_some() {
            Some(custom_command_line(&image, &["/d".into()])?)
        } else {
            None
        };
        image.push(0);
        (image, command_line)
    };
    let environment = if let Some(sandbox) = sandbox {
        shell_environment(term, &sandbox.environment(false)?, false)?
    } else {
        shell_environment(
            term,
            integration
                .as_ref()
                .map_or(&[], |prepared| &prepared.environment),
            true,
        )?
    };
    let (child, pid) = spawn_process(
        job,
        ProcessLaunch {
            program: &shell,
            command_line,
            environment: &environment,
            directory,
            console,
            sandbox,
            inherit_console: sandbox.is_some(),
        },
    )?;
    Ok((
        child,
        pid,
        integration
            .as_mut()
            .and_then(|prepared| prepared.startup.take()),
    ))
}

struct ProcessLaunch<'a> {
    program: &'a [u16],
    command_line: Option<Vec<u16>>,
    environment: &'a [u16],
    directory: Option<&'a Path>,
    console: Option<&'a Console>,
    sandbox: Option<&'a sandbox::Session>,
    inherit_console: bool,
}

fn spawn_process(
    job: &OwnedHandle,
    mut launch: ProcessLaunch<'_>,
) -> io::Result<(OwnedHandle, u32)> {
    let directory = launch.directory.map(|path| {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    });
    let sandbox_handle = launch.sandbox.map(sandbox::Session::handle);
    let mut attributes = ProcessAttributes::new(launch.console, sandbox_handle.as_ref())?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    // With default handles Windows can duplicate the parent's redirected I/O
    // even when bInheritHandles is false. Null handles let ConPTY supply all three.
    if launch.sandbox.is_none() {
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    }
    startup.lpAttributeList = attributes.as_mut_ptr();
    let mut process = PROCESS_INFORMATION::default();
    let mut flags = EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT;
    if launch.console.is_none() && !launch.inherit_console {
        flags |= CREATE_NO_WINDOW;
    }
    // SAFETY: shell is a NUL-terminated absolute path. The optional command
    // line is writable and NUL-terminated. The startup attribute list and
    // environment block are initialized, and process is writable. No handles
    // are inherited.
    bool_result(unsafe {
        CreateProcessW(
            launch.program.as_ptr(),
            launch
                .command_line
                .as_mut()
                .map_or(null_mut(), Vec::as_mut_ptr),
            null(),
            null(),
            0,
            flags,
            launch.environment.as_ptr().cast(),
            directory.as_ref().map_or(null(), |path| path.as_ptr()),
            &startup.StartupInfo,
            &mut process,
        )
    })?;
    let child = owned_handle(process.hProcess)?;
    let initial_thread = owned_handle(process.hThread)?;
    // SAFETY: the child is still suspended and both handles are live.
    let started = bool_result(unsafe {
        AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle())
    })
    .and_then(|()| {
        if launch.sandbox.is_some() {
            sandbox::verify_process(&child)?;
        }
        Ok(())
    })
    .and_then(|()| {
        // SAFETY: this is the live primary thread of our suspended child.
        if unsafe { ResumeThread(initial_thread.as_raw_handle()) } == u32::MAX {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    });
    if let Err(error) = started {
        // SAFETY: the process is our still-suspended child, even if job assignment failed.
        unsafe { TerminateProcess(child.as_raw_handle(), 1) };
        let _ = signaled(&child, REAP_TIMEOUT_MS);
        return Err(error);
    }
    Ok((child, process.dwProcessId))
}

fn probe_sandbox(sandbox: &sandbox::Session, term: &str) -> io::Result<()> {
    let job = kill_on_close_job()?;
    let program = shell_program(LocalShell::CommandPrompt)?;
    let command_line = format!(
        r#""{}" /d /q /u /v:off /c "(if /i not "%CD%"=="%KORTTY_PROBE_WORKSPACE%" exit /b 70) & (type "%KORTTY_PROBE_DENIED%">"%KORTTY_PROBE_TEMP%\blocked"&&exit /b 71) & (echo kortty>"%KORTTY_PROBE_TEMP%\probe"||exit /b 72) & (set TEMP>"%KORTTY_PROBE_TEMP%\native-temp"&&set TMP>>"%KORTTY_PROBE_TEMP%\native-temp"||exit /b 73) & exit /b 0""#,
        program.display(),
    );
    let program: Vec<_> = program.as_os_str().encode_wide().chain([0]).collect();
    let command_line = command_line.encode_utf16().chain([0]).collect();
    let environment = shell_environment(term, &sandbox.environment(true)?, false)?;
    let (process, _) = spawn_process(
        &job,
        ProcessLaunch {
            program: &program,
            command_line: Some(command_line),
            environment: &environment,
            directory: Some(sandbox.workspace()),
            console: None,
            sandbox: Some(sandbox),
            inherit_console: false,
        },
    )?;
    let exited = signaled(&process, REAP_TIMEOUT_MS);
    if !matches!(exited, Ok(true)) {
        drop(job);
        let _ = signaled(&process, REAP_TIMEOUT_MS);
        exited?;
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Windows sandbox probe did not exit",
        ));
    }
    let mut code = 0;
    // SAFETY: the probe is an owned process and code is writable.
    bool_result(unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) })?;
    if code != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("Windows sandbox filesystem probe failed (exit {code:#x})"),
        ));
    }
    sandbox.verify_probe()
}

fn custom_program(program: &str, directory: Option<&Path>) -> io::Result<PathBuf> {
    let mut program = PathBuf::from(program);
    match program.extension() {
        None => {
            program.set_extension("exe");
        }
        Some(extension) if extension.eq_ignore_ascii_case(OsStr::new("exe")) => {}
        Some(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a Windows custom command must name an .exe program",
            ));
        }
    }

    if !program.is_absolute() {
        if program.components().count() == 1 && program.file_name().is_some() {
            if let Some(path) = env::var_os("PATH")
                && let Some(resolved) = env::split_paths(&path)
                    .filter(|directory| directory.is_absolute())
                    .map(|directory| directory.join(&program))
                    .find(|candidate| candidate.is_file())
            {
                return Ok(resolved);
            }
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the custom executable was not found on PATH",
            ));
        }
        let base = match directory {
            Some(directory) => directory.to_owned(),
            None => env::current_dir()?,
        };
        program = base.join(program);
    }
    if !program.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the custom executable path must resolve to an absolute path",
        ));
    }
    Ok(program)
}

fn custom_command_line(program: &[u16], arguments: &[String]) -> io::Result<Vec<u16>> {
    let mut line = Vec::new();
    append_argument(&mut line, program.iter().copied());
    for argument in arguments {
        line.push(u16::from(b' '));
        append_argument(&mut line, argument.encode_utf16());
    }
    line.push(0);
    if line.len() > 32_767 || line[..line.len() - 1].contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the local command cannot be represented as a Windows command line",
        ));
    }
    Ok(line)
}

fn append_argument(line: &mut Vec<u16>, argument: impl Iterator<Item = u16> + Clone) {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    let mut probe = argument.clone().peekable();
    if probe.peek().is_some() && !probe.any(|unit| matches!(unit, 9 | 32 | QUOTE)) {
        line.extend(argument);
        return;
    }
    // Keep simple switches unquoted for programs such as cmd.exe. The C
    // runtime needs escaping for backslashes before quotes and empty values.
    line.push(QUOTE);
    let mut backslashes = 0;
    for unit in argument {
        match unit {
            BACKSLASH => backslashes += 1,
            QUOTE => {
                line.extend(std::iter::repeat_n(BACKSLASH, 2 * backslashes + 1));
                line.push(QUOTE);
                backslashes = 0;
            }
            _ => {
                line.extend(std::iter::repeat_n(BACKSLASH, backslashes));
                line.push(unit);
                backslashes = 0;
            }
        }
    }
    line.extend(std::iter::repeat_n(BACKSLASH, 2 * backslashes));
    line.push(QUOTE);
}

fn shell_environment(
    term: &str,
    overrides: &[(&str, Option<OsString>)],
    inherit: bool,
) -> io::Result<Vec<u16>> {
    let mut variables: Vec<(Vec<u16>, OsString)> = env::vars_os()
        .filter(|(name, _)| {
            inherit
                && !name.eq_ignore_ascii_case(OsStr::new("TERM"))
                && !overrides
                    .iter()
                    .any(|(key, _)| name.eq_ignore_ascii_case(OsStr::new(key)))
        })
        .map(|(name, value)| (name.encode_wide().collect(), value))
        .collect();
    variables.push(("TERM".encode_utf16().collect(), term.into()));
    variables.extend(overrides.iter().filter_map(|(name, value)| {
        value
            .clone()
            .map(|value| (name.encode_utf16().collect(), value))
    }));
    // Windows requires case-insensitive ordinal order for environment names.
    // Preserve special drive entries such as `=C:` from the parent block.
    let mut comparison_error = None;
    variables.sort_by(|(left, _), (right, _)| {
        // SAFETY: both buffers are valid UTF-16 code units of known length.
        // Windows environment names are bounded well below i32::MAX.
        let result = unsafe {
            CompareStringOrdinal(
                left.as_ptr(),
                left.len() as i32,
                right.as_ptr(),
                right.len() as i32,
                1,
            )
        };
        if result == 0 {
            comparison_error = Some(io::Error::last_os_error());
            return left.cmp(right);
        }
        result.cmp(&CSTR_EQUAL)
    });
    if let Some(error) = comparison_error {
        return Err(error);
    }

    let mut block = Vec::new();
    for (name, value) in variables {
        block.extend(name);
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

/// Keep the returned job alive while the directly launched child may run.
pub fn contain_child(child: &std::process::Child) -> io::Result<OwnedHandle> {
    let job = kill_on_close_job()?;
    // SAFETY: both handles refer to live objects held by this function and its caller.
    bool_result(unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) })?;
    Ok(job)
}

/// Limits a PDF parser to one process and a fixed memory budget before input is sent.
pub fn contain_pdf_worker(child: &std::process::Child) -> io::Result<OwnedHandle> {
    let job = limited_job(Some(512 * 1024 * 1024))?;
    // SAFETY: Both handles are live, and the child is waiting for the document pipe.
    bool_result(unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) })?;
    Ok(job)
}

/// Decodes bytes emitted by a detached native command prompt using the OEM code page.
pub fn decode_oem_output(bytes: &[u8]) -> io::Result<String> {
    if bytes.is_empty() {
        return Ok(String::new());
    }
    let bytes_len = i32::try_from(bytes.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    // SAFETY: the input is readable for bytes_len bytes. The first call only sizes the output.
    let len = unsafe { MultiByteToWideChar(CP_OEMCP, 0, bytes.as_ptr(), bytes_len, null_mut(), 0) };
    if len <= 0 {
        return Err(io::Error::last_os_error());
    }
    let mut output = vec![0; len as usize];
    // SAFETY: output has len writable UTF-16 units, sized by the same conversion above.
    let written = unsafe {
        MultiByteToWideChar(
            CP_OEMCP,
            0,
            bytes.as_ptr(),
            bytes_len,
            output.as_mut_ptr(),
            len,
        )
    };
    if written <= 0 {
        return Err(io::Error::last_os_error());
    }
    output.truncate(written as usize);
    Ok(String::from_utf16_lossy(&output))
}

fn kill_on_close_job() -> io::Result<OwnedHandle> {
    limited_job(None)
}

fn terminate_job(job: &OwnedHandle) -> io::Result<()> {
    // SAFETY: job is the caller's live, private job object.
    bool_result(unsafe { TerminateJobObject(job.as_raw_handle(), 1) })?;
    // Process termination is asynchronous. Keep the policy and temporary
    // directories alive until the entire job, including descendants, has exited.
    let deadline = Instant::now() + Duration::from_millis(REAP_TIMEOUT_MS.into());
    loop {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: job is live and accounting is the writable structure for this class.
        bool_result(unsafe {
            QueryInformationJobObject(
                job.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&raw mut accounting).cast(),
                size_of_val(&accounting) as u32,
                null_mut(),
            )
        })?;
        if accounting.ActiveProcesses == 0 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Windows sandbox job did not exit",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn limited_job(process_memory: Option<usize>) -> io::Result<OwnedHandle> {
    // SAFETY: null attributes and name request a private, non-inheritable job.
    let job = owned_handle(unsafe { CreateJobObjectW(null(), null()) })?;
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if let Some(bytes) = process_memory {
        limits.BasicLimitInformation.LimitFlags |=
            JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        limits.BasicLimitInformation.ActiveProcessLimit = 1;
        limits.ProcessMemoryLimit = bytes;
    }
    // SAFETY: the live job receives a properly initialized limit struct and its size.
    bool_result(unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    })?;
    Ok(job)
}

pub(crate) fn shell_program(preset: LocalShell) -> io::Result<PathBuf> {
    let mut directory = vec![0; 260];
    loop {
        // SAFETY: directory holds the supplied number of writable UTF-16 units.
        let len = unsafe { GetSystemDirectoryW(directory.as_mut_ptr(), directory.len() as u32) };
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        if (len as usize) < directory.len() {
            directory.truncate(len as usize);
            break;
        }
        directory.resize(len as usize, 0);
    }
    let directory = PathBuf::from(OsString::from_wide(&directory));
    let powershell = directory.join(r"WindowsPowerShell\v1.0\powershell.exe");
    let shell = match preset {
        LocalShell::Default if powershell.is_file() => powershell,
        LocalShell::PowerShell => powershell,
        LocalShell::Default | LocalShell::CommandPrompt => directory.join("cmd.exe"),
    };
    Ok(shell)
}

pub(crate) fn pipe_name() -> io::Result<Vec<u16>> {
    let mut nonce = [0; 16];
    // SAFETY: null selects the system CSPRNG and nonce is writable for its full length.
    let result = unsafe {
        BCryptGenRandom(
            null_mut(),
            nonce.as_mut_ptr(),
            nonce.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if result < 0 {
        return Err(io::Error::other(format!(
            "could not generate pipe name: {result:#x}"
        )));
    }
    let name = format!(r"\\.\pipe\kortty-{:032x}", u128::from_le_bytes(nonce));
    Ok(name.encode_utf16().chain([0]).collect())
}

fn event() -> io::Result<OwnedHandle> {
    // SAFETY: null attributes and name request a non-inheritable manual-reset event.
    owned_handle(unsafe { CreateEventW(null(), 1, 0, null()) })
}

pub(crate) fn owned_handle(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: callers supply a newly owned Win32 handle from a creation API.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn close_pipe(handle: &SharedHandle) {
    handle.lock().unwrap_or_else(|err| err.into_inner()).take();
}

fn close_console(console: &SharedConsole) {
    let handle = console.lock().unwrap_or_else(|err| err.into_inner()).take();
    drop(handle);
}

fn wait_for_either(first: &OwnedHandle, second: &OwnedHandle) -> io::Result<bool> {
    let handles = [first.as_raw_handle(), second.as_raw_handle()];
    // SAFETY: the two handles remain live throughout the wait.
    match unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE) } {
        WAIT_OBJECT_0 => Ok(false),
        result if result == WAIT_OBJECT_0 + 1 => Ok(true),
        _ => Err(io::Error::last_os_error()),
    }
}

fn signaled(handle: &OwnedHandle, timeout: u32) -> io::Result<bool> {
    // SAFETY: the handle remains live throughout the wait.
    match unsafe { WaitForSingleObject(handle.as_raw_handle(), timeout) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}

fn pipe_error(error: io::Error) -> io::Error {
    match error.raw_os_error().map(|value| value as u32) {
        Some(ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED) => closed(),
        _ => error,
    }
}

fn coordinate(size: Size) -> COORD {
    COORD {
        X: size.columns as i16,
        Y: size.rows as i16,
    }
}

fn bool_result(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn hresult(result: i32) -> io::Result<()> {
    if result >= 0 {
        Ok(())
    } else if ((result as u32 >> 16) & 0x1fff) == 7 {
        Err(io::Error::from_raw_os_error(result & 0xffff))
    } else {
        Err(io::Error::other(format!(
            "pseudoconsole HRESULT {result:#x}"
        )))
    }
}
