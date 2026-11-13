//! Starts PSEC children from a trusted process already attached to ConPTY.

use std::env;
use std::ffi::{OsStr, OsString, c_void};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use kortty_files::tree::{Directory, is_reparse_point};
use windows_sys::Win32::Foundation::{
    ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_OVERLAPPED,
    FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_READ, FILE_WRITE_DATA, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_ANONYMOUS, SECURITY_SQOS_PRESENT, SYNCHRONIZE,
    WriteFile,
};
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE,
};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForMultipleObjects};
use windows_sys::Win32::UI::Shell::{
    FOLDERID_LocalAppData, FOLDERID_Profile, FOLDERID_RoamingAppData,
};
use windows_sys::core::GUID;

use crate::worker_sandbox::with_private_pipe;
use crate::{LaunchOptions, LocalShell, Sandbox, validate_local_command};

use super::{
    Console, ProcessLaunch, REAP_TIMEOUT_MS, bool_result, custom_command_line, custom_program,
    event, kill_on_close_job, owned_handle, pipe_name, probe_sandbox, sandbox, shell_environment,
    signaled, spawn_process, spawn_shell, terminate_job,
};

/// Internal command line mode of a trusted Windows terminal host executable.
pub const WINDOWS_LOCAL_SANDBOX_BROKER_ARGUMENT: &str = "--local-pty-sandbox-broker";

const REPORT_LIMIT: usize = 4096;
const REPORT_ACK: u8 = 1;

pub(super) struct SessionFiles {
    _scratch: sandbox::Scratch,
    _broker_file: File,
    _installation: Directory,
}

pub(super) fn spawn(
    console: &Console,
    job: &OwnedHandle,
    term: &str,
    options: &LaunchOptions,
) -> io::Result<(OwnedHandle, u32, SessionFiles)> {
    let workspace = options
        .working_directory
        .as_deref()
        .ok_or_else(invalid_arguments)?;
    let profile = &options
        .sandbox
        .as_ref()
        .ok_or_else(invalid_arguments)?
        .profile_root;
    let executable = env::current_exe()?;
    // The installed GUI executable cannot inherit console handles. Its
    // release bundle includes this existing console-subsystem helper.
    let helper = executable.with_file_name("kortty-ssh-worker.exe");
    let program = match fs::symlink_metadata(&helper) {
        Ok(_) => helper,
        Err(error) if error.kind() == io::ErrorKind::NotFound => executable,
        Err(error) => return Err(error),
    };
    let install = sandbox::installation_directory(&program)?;
    if sandbox::overlaps(&sandbox::canonical(workspace)?, &install) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The sandbox workspace overlaps the Windows sandbox broker installation directory",
        ));
    }
    let installation = Directory::open_reviewed_removal_parent(&install)?;
    let program = install.join(program.file_name().ok_or_else(invalid_arguments)?);
    let broker_file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&program)?;
    let metadata = broker_file.metadata()?;
    if !metadata.is_file() || is_reparse_point(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The Windows sandbox broker must be an ordinary executable file",
        ));
    }
    let scratch = sandbox::Scratch::new()?;
    let name = pipe_name()?;
    let report = report_pipe(&name)?;
    let pipe_name = String::from_utf16_lossy(&name[..name.len() - 1]);
    let shell = match options.shell {
        LocalShell::Default => "default",
        LocalShell::CommandPrompt => "cmd",
        LocalShell::PowerShell => "powershell",
    };
    let mut arguments = vec![
        WINDOWS_LOCAL_SANDBOX_BROKER_ARGUMENT.to_owned(),
        pipe_name,
        std::process::id().to_string(),
        term.to_owned(),
        unicode(workspace.as_os_str())?.to_owned(),
        unicode(profile.as_os_str())?.to_owned(),
        unicode(scratch.path().as_os_str())?.to_owned(),
        shell.to_owned(),
    ];
    if let Some(command) = &options.command {
        let image = custom_program(&command[0], Some(workspace))?;
        arguments.push(unicode(image.as_os_str())?.to_owned());
        arguments.extend(command[1..].iter().cloned());
    }
    let mut program: Vec<_> = program.as_os_str().encode_wide().collect();
    let command_line = custom_command_line(&program, &arguments)?;
    program.push(0);
    let mut environment = sandbox::system_environment()?;
    // The native PSEC runtime provisions a Windows AppContainer profile under
    // the registered host folders. The launcher's test or portable profile
    // overrides are separate from these operating-system paths.
    environment.extend(host_profile_environment()?);
    for key in ["TEMP", "TMP"] {
        environment.push((key, env::var_os(key)));
    }
    let environment = shell_environment(term, &environment, false)?;
    let (process, broker_pid) = spawn_process(
        job,
        ProcessLaunch {
            program: &program,
            command_line: Some(command_line),
            environment: &environment,
            directory: Some(&install),
            console: Some(console),
            sandbox: None,
            inherit_console: false,
        },
    )?;
    match read_pid(&report, &process, broker_pid) {
        Ok(pid) => Ok((
            process,
            pid,
            SessionFiles {
                _scratch: scratch,
                _broker_file: broker_file,
                _installation: installation,
            },
        )),
        Err(error) => {
            let _ = terminate_job(job);
            let _ = signaled(&process, REAP_TIMEOUT_MS);
            Err(error)
        }
    }
}

type FindFolder = unsafe extern "system" fn(*const GUID, u32, HANDLE, *mut *mut u16) -> i32;
type FreeFolder = unsafe extern "system" fn(*const c_void);

fn host_profile_environment() -> io::Result<Vec<(&'static str, Option<OsString>)>> {
    // Load these only on the trusted host. Statically importing Shell32 can
    // make console applications fail to start with the Win32k mitigation.
    let shell = sandbox::Module::load_system("shell32.dll")?;
    let com = sandbox::Module::load_system("combase.dll")?;
    // SAFETY: the named System32 exports have the declared ABI and remain loaded here.
    let (find, free) = unsafe {
        (
            std::mem::transmute::<sandbox::Export, FindFolder>(
                shell.export(c"SHGetKnownFolderPath")?,
            ),
            std::mem::transmute::<sandbox::Export, FreeFolder>(com.export(c"CoTaskMemFree")?),
        )
    };
    [
        ("USERPROFILE", FOLDERID_Profile),
        ("APPDATA", FOLDERID_RoamingAppData),
        ("LOCALAPPDATA", FOLDERID_LocalAppData),
    ]
    .into_iter()
    .map(|(name, id)| Ok((name, Some(known_folder(find, free, &id)?))))
    .collect()
}

fn known_folder(find: FindFolder, free: FreeFolder, folder: &GUID) -> io::Result<OsString> {
    let mut path = null_mut();
    // SAFETY: folder is initialized, the null token selects the current user,
    // and path is writable. The returned COM allocation is caller-owned.
    let result = unsafe { find(folder, 0, null_mut(), &mut path) };
    let output = if result >= 0 && !path.is_null() {
        // SAFETY: a successful call returns a live, NUL-terminated UTF-16 path.
        let wide = unsafe {
            std::slice::from_raw_parts(
                path,
                windows_sys::Win32::Globalization::lstrlenW(path) as usize,
            )
        };
        Ok(OsString::from_wide(wide))
    } else {
        Err(io::Error::other(format!(
            "Could not locate a Windows sandbox host folder (HRESULT {result:#010x})"
        )))
    };
    // SAFETY: Windows transfers the allocation even on failure, or returns null.
    unsafe { free(path.cast()) };
    output
}

/// Handles the internal broker mode before starting the graphical application.
pub fn run_windows_local_sandbox_broker(
    mut args: impl Iterator<Item = OsString>,
) -> io::Result<i32> {
    let pipe = argument(&mut args)?;
    let parent: u32 = unicode(&argument(&mut args)?)?
        .parse()
        .map_err(|_| invalid_arguments())?;
    let mut report = connect_report(unicode(&pipe)?, parent)?;
    match prepare(&mut args) {
        Ok((session, job, child, pid)) => {
            let mut message = vec![0];
            message.extend(pid.to_le_bytes());
            send_report(&mut report, &message)?;
            drop(report);
            signaled(&child, INFINITE)?;
            let mut code = 0;
            // SAFETY: child is the signaled process handle and code is writable.
            bool_result(unsafe { GetExitCodeProcess(child.as_raw_handle(), &mut code) })?;
            terminate_job(&job)?;
            drop(job);
            drop(session);
            Ok(code as i32)
        }
        Err(error) => {
            let mut message = error.to_string();
            message.truncate(message.floor_char_boundary(REPORT_LIMIT - 1));
            let tag = match error.kind() {
                io::ErrorKind::Unsupported => 1,
                io::ErrorKind::InvalidInput => 2,
                io::ErrorKind::PermissionDenied => 3,
                io::ErrorKind::NotFound => 4,
                _ => 5,
            };
            let mut bytes = vec![tag];
            bytes.extend(message.as_bytes());
            send_report(&mut report, &bytes)?;
            Ok(1)
        }
    }
}

fn prepare(
    args: &mut impl Iterator<Item = OsString>,
) -> io::Result<(sandbox::Session, OwnedHandle, OwnedHandle, u32)> {
    let term = argument(args)?;
    let workspace = PathBuf::from(argument(args)?);
    let profile = PathBuf::from(argument(args)?);
    let scratch = PathBuf::from(argument(args)?);
    let preset = argument(args)?;
    let shell = match unicode(&preset)? {
        "default" | "cmd" => LocalShell::CommandPrompt,
        "powershell" => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Windows PowerShell requires Win32k system calls; choose Command Prompt or a compatible custom command for the sandbox",
            ));
        }
        _ => return Err(invalid_arguments()),
    };
    let words: Vec<_> = args
        .map(|word| unicode(&word).map(str::to_owned))
        .collect::<io::Result<_>>()?;
    let command = if words.is_empty() {
        None
    } else {
        validate_local_command(&words)?;
        Some(words)
    };
    let options = LaunchOptions {
        shell,
        command,
        working_directory: Some(workspace.clone()),
        sandbox: Some(Sandbox {
            profile_root: profile.clone(),
        }),
        shell_integration: false,
    };
    let session = sandbox::Session::new(&workspace, &profile, &scratch)?;
    verify_console()?;
    // SAFETY: the callback has the documented ABI and lives until broker exit.
    // A callback is process-local, so the shell still receives its normal interrupts.
    bool_result(unsafe { SetConsoleCtrlHandler(Some(keep_broker_running), 1) })?;
    probe_sandbox(&session, unicode(&term)?)?;
    let job = kill_on_close_job()?;
    let (process, pid, _) = spawn_shell(None, &job, unicode(&term)?, &options, Some(&session))?;
    Ok((session, job, process, pid))
}

unsafe extern "system" fn keep_broker_running(event: u32) -> i32 {
    i32::from(matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT))
}

fn verify_console() -> io::Result<()> {
    let mut mode = 0;
    for stream in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE] {
        // SAFETY: the standard console handle is process-local and mode is writable.
        if unsafe { GetConsoleMode(GetStdHandle(stream), &mut mode) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Windows sandbox broker could not attach to the pseudoconsole",
            ));
        }
    }
    Ok(())
}

fn read_pid(report: &OwnedHandle, process: &OwnedHandle, broker_pid: u32) -> io::Result<u32> {
    let deadline = Instant::now() + Duration::from_secs(15);
    report_io(report, process, deadline, true, |pipe, operation| {
        // SAFETY: the server handle and overlapped operation remain live through completion.
        unsafe { ConnectNamedPipe(pipe, operation) }
    })?;
    let mut client = 0;
    // SAFETY: report is the connected server handle and client is writable.
    bool_result(unsafe { GetNamedPipeClientProcessId(report.as_raw_handle(), &mut client) })?;
    if client != broker_pid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Unexpected Windows sandbox broker pipe client",
        ));
    }
    let mut bytes = [0; REPORT_LIMIT];
    let length = report_io(report, process, deadline, false, |pipe, operation| {
        // SAFETY: the buffer and overlapped operation remain live through completion.
        unsafe {
            ReadFile(
                pipe,
                bytes.as_mut_ptr(),
                REPORT_LIMIT as u32,
                null_mut(),
                operation,
            )
        }
    })?;
    let result = decode_report(&bytes[..length as usize]);
    let ack = [REPORT_ACK];
    let count = report_io(report, process, deadline, false, |pipe, operation| {
        // SAFETY: the single-byte buffer and operation remain live through completion.
        unsafe { WriteFile(pipe, ack.as_ptr(), 1, null_mut(), operation) }
    })?;
    if count != 1 {
        return Err(io::Error::other(
            "Could not acknowledge Windows sandbox broker status",
        ));
    }
    result
}

fn decode_report(bytes: &[u8]) -> io::Result<u32> {
    match bytes {
        [0, a, b, c, d] => {
            let pid = u32::from_le_bytes([*a, *b, *c, *d]);
            if pid != 0 {
                return Ok(pid);
            }
        }
        [tag @ 1..=5, message @ ..] => {
            let kind = match tag {
                1 => io::ErrorKind::Unsupported,
                2 => io::ErrorKind::InvalidInput,
                3 => io::ErrorKind::PermissionDenied,
                4 => io::ErrorKind::NotFound,
                _ => io::ErrorKind::Other,
            };
            return Err(io::Error::new(
                kind,
                String::from_utf8_lossy(message).into_owned(),
            ));
        }
        _ => {}
    }
    Err(io::Error::other(
        "Windows sandbox broker returned invalid launch status",
    ))
}

fn report_io(
    report: &OwnedHandle,
    process: &OwnedHandle,
    deadline: Instant,
    connecting: bool,
    start: impl FnOnce(HANDLE, &mut OVERLAPPED) -> i32,
) -> io::Result<u32> {
    let complete = event()?;
    let pipe = report.as_raw_handle();
    let mut operation = OVERLAPPED {
        hEvent: complete.as_raw_handle(),
        ..Default::default()
    };
    let mut count = 0;
    if let Err(error) = bool_result(start(pipe, &mut operation)) {
        match error.raw_os_error().map(|code| code as u32) {
            Some(ERROR_PIPE_CONNECTED) if connecting => return Ok(0),
            Some(ERROR_IO_PENDING) => {}
            _ => return Err(error),
        }
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u32;
        let wait = [complete.as_raw_handle(), process.as_raw_handle()];
        // SAFETY: both handles and the array remain live through the bounded wait.
        let result = unsafe { WaitForMultipleObjects(2, wait.as_ptr(), 0, remaining) };
        if result != WAIT_OBJECT_0 {
            let error = match result {
                WAIT_TIMEOUT => io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Windows sandbox broker did not report its launch status",
                ),
                value if value == WAIT_OBJECT_0 + 1 => io::Error::other(
                    "Windows sandbox broker exited before reporting its launch status",
                ),
                _ => io::Error::last_os_error(),
            };
            // SAFETY: cancel and reap the pending operation before releasing its buffers.
            unsafe {
                CancelIoEx(pipe, &operation);
                GetOverlappedResult(pipe, &operation, &mut count, 1);
            }
            return Err(error);
        }
    }
    // SAFETY: the operation completed, and the pipe and output count remain live.
    bool_result(unsafe { GetOverlappedResult(pipe, &operation, &mut count, 0) })?;
    Ok(count)
}

fn report_pipe(name: &[u16]) -> io::Result<OwnedHandle> {
    with_private_pipe(|security| {
        // SAFETY: the terminated name and private non-inheritable descriptor are live.
        owned_handle(unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                REPORT_LIMIT as u32,
                REPORT_LIMIT as u32,
                0,
                security,
            )
        })
    })
}

fn connect_report(name: &str, expected_server: u32) -> io::Result<File> {
    if expected_server == 0
        || !name.strip_prefix(r"\\.\pipe\kortty-").is_some_and(|nonce| {
            nonce.len() == 32 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        return Err(invalid_arguments());
    }
    let name: Vec<_> = name.encode_utf16().chain([0]).collect();
    // SAFETY: the terminated name identifies a local pipe. Anonymous security
    // QoS prevents the server from impersonating this client.
    let report = owned_handle(unsafe {
        CreateFileW(
            name.as_ptr(),
            FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            0,
            null(),
            OPEN_EXISTING,
            SECURITY_SQOS_PRESENT | SECURITY_ANONYMOUS,
            null_mut(),
        )
    })?;
    let mut server = 0;
    // SAFETY: report is a live client pipe and server is writable.
    bool_result(unsafe { GetNamedPipeServerProcessId(report.as_raw_handle(), &mut server) })?;
    if server != expected_server {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Unexpected Windows sandbox broker pipe server",
        ));
    }
    Ok(File::from(report))
}

fn send_report(report: &mut File, bytes: &[u8]) -> io::Result<()> {
    report.write_all(bytes)?;
    // Keep the client connected until the parent has authenticated its PID and read the message.
    let mut ack = [0];
    report.read_exact(&mut ack)?;
    if ack == [REPORT_ACK] {
        Ok(())
    } else {
        Err(io::Error::other(
            "Invalid Windows sandbox broker status acknowledgment",
        ))
    }
}

fn argument(args: &mut impl Iterator<Item = OsString>) -> io::Result<OsString> {
    args.next().ok_or_else(invalid_arguments)
}

fn unicode(value: &OsStr) -> io::Result<&str> {
    value.to_str().ok_or_else(invalid_arguments)
}

fn invalid_arguments() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "Invalid Windows local sandbox broker arguments",
    )
}
