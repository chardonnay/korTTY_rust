use std::env;
use std::ffi::{OsStr, OsString, c_void};
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ,
    GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
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
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateEventW, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, ResetEvent, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, SetEvent, TerminateProcess, UpdateProcThreadAttribute,
    WaitForMultipleObjects, WaitForSingleObject,
};

use crate::{LaunchOptions, LocalShell, Size, closed};

const PIPE_BUFFER: u32 = 64 * 1024;
const REAP_TIMEOUT_MS: u32 = 5000;

type SharedHandle = Arc<Mutex<Option<OwnedHandle>>>;
type SharedConsole = Arc<Mutex<Option<Console>>>;

pub(crate) struct Control {
    process: Arc<OwnedHandle>,
    shell_pid: u32,
    job: Option<OwnedHandle>,
    console: SharedConsole,
    stop: Arc<OwnedHandle>,
    output: SharedHandle,
    input: SharedHandle,
    monitor: Option<JoinHandle<io::Result<()>>>,
}

impl Control {
    pub(crate) fn shell_pid(&self) -> u32 {
        self.shell_pid
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
        let (process, shell_pid) = match spawn_shell(&console, &job, term, options) {
            Ok((process, pid)) => (Arc::new(process), pid),
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
            job: Some(job),
            console: Arc::new(Mutex::new(Some(console))),
            stop: Arc::clone(&stop),
            output: Arc::clone(&output.handle),
            input: Arc::clone(&input.handle),
            monitor: None,
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
        Ok(Some(ExitStatus::from_raw(code)))
    }

    pub(crate) fn shutdown(&mut self) -> io::Result<()> {
        // SAFETY: stop is the live manual-reset event shared by both I/O paths.
        bool_result(unsafe { SetEvent(self.stop.as_raw_handle()) })?;
        close_pipe(&self.output);
        close_pipe(&self.input);
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
    fn for_console(console: &Console) -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: the sizing call takes no storage and writes the required size.
        unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut bytes) };
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0_usize; bytes.div_ceil(size_of::<usize>())];
        // SAFETY: storage is pointer-aligned, has at least bytes bytes, and
        // does not move after initialization.
        bool_result(unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), 1, 0, &mut bytes)
        })?;
        let mut attributes = Self(storage);
        // SAFETY: the attribute list and the pseudoconsole are live. This
        // particular attribute takes the HPCON value as its pointer argument.
        bool_result(unsafe {
            UpdateProcThreadAttribute(
                attributes.as_mut_ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                console.0 as *const c_void,
                size_of::<HPCON>(),
                null_mut(),
                null(),
            )
        })?;
        Ok(attributes)
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
    console: &Console,
    job: &OwnedHandle,
    term: &str,
    options: &LaunchOptions,
) -> io::Result<(OwnedHandle, u32)> {
    let shell = system_shell(options.shell)?;
    let environment = shell_environment(term)?;
    let directory = options.working_directory.as_ref().map(|path| {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    });
    let mut attributes = ProcessAttributes::for_console(console)?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    // With default handles Windows can duplicate the parent's redirected I/O
    // even when bInheritHandles is false. Null handles let ConPTY supply all three.
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.lpAttributeList = attributes.as_mut_ptr();
    let mut process = PROCESS_INFORMATION::default();
    // SAFETY: shell is a NUL-terminated absolute path, startup's attribute
    // list and environment block are initialized, and process is writable.
    // No handles are inherited.
    bool_result(unsafe {
        CreateProcessW(
            shell.as_ptr(),
            null_mut(),
            null(),
            null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
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

fn shell_environment(term: &str) -> io::Result<Vec<u16>> {
    let mut variables: Vec<(Vec<u16>, OsString)> = env::vars_os()
        .filter(|(name, _)| !name.eq_ignore_ascii_case(OsStr::new("TERM")))
        .map(|(name, value)| (name.encode_wide().collect(), value))
        .collect();
    variables.push(("TERM".encode_utf16().collect(), term.into()));
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

fn kill_on_close_job() -> io::Result<OwnedHandle> {
    // SAFETY: null attributes and name request a private, non-inheritable job.
    let job = owned_handle(unsafe { CreateJobObjectW(null(), null()) })?;
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
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

fn system_shell(preset: LocalShell) -> io::Result<Vec<u16>> {
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
    Ok(shell.as_os_str().encode_wide().chain([0]).collect())
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
