use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};

use mio::windows::NamedPipe;
use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_READ_ATTRIBUTES,
    FILE_READ_DATA, FILE_TYPE_PIPE, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, GetFileType,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX, PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND,
    SECURITY_ANONYMOUS, SECURITY_SQOS_PRESENT, SYNCHRONIZE,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeInfo, GetNamedPipeServerProcessId,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_SERVER_END, PIPE_TYPE_BYTE, PIPE_TYPE_MESSAGE,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use crate::platform::{owned_handle, pipe_name};

use super::{bool_result, policy};

pub(super) enum DesktopEnd {
    Send,
    Receive,
    Duplex,
}

pub(super) fn pair(end: DesktopEnd) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (access, child_access, flags, buffer) = match end {
        DesktopEnd::Send => (PIPE_ACCESS_OUTBOUND, FILE_READ_DATA, 0, 4 * 1024),
        DesktopEnd::Receive => (PIPE_ACCESS_INBOUND, FILE_WRITE_DATA, 0, 4 * 1024),
        DesktopEnd::Duplex => (
            PIPE_ACCESS_DUPLEX,
            FILE_READ_DATA | FILE_WRITE_DATA,
            FILE_FLAG_OVERLAPPED,
            64 * 1024,
        ),
    };
    let name = pipe_name()?;
    let desktop = policy::with_private_pipe(|security| {
        // SAFETY: name is terminated, and the exact-user descriptor remains
        // alive for this call. This is the first and only local pipe instance.
        owned_handle(unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                access | flags | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                buffer,
                buffer,
                0,
                security,
            )
        })
    })?;
    // SAFETY: this opens the client of our live server instance. The worker
    // inherits only the client, which has no pipe-server impersonation rights.
    let worker = owned_handle(unsafe {
        CreateFileW(
            name.as_ptr(),
            child_access | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | SYNCHRONIZE,
            0,
            null(),
            OPEN_EXISTING,
            flags | SECURITY_SQOS_PRESENT | SECURITY_ANONYMOUS,
            null_mut(),
        )
    })?;
    // Opening the client connects the instance. If another process won the
    // connection race, neither its server nor its client can pass this check.
    if endpoints(desktop.as_raw_handle())? != (std::process::id(), std::process::id())
        || endpoints(worker.as_raw_handle())? != (std::process::id(), std::process::id())
    {
        return Err(invalid_pipe());
    }
    Ok((desktop, worker))
}

pub(super) fn receive(descriptor: u32) -> io::Result<NamedPipe> {
    let inherited = descriptor as i32 as isize as HANDLE;
    let owner = validate_client(inherited)?;
    let mut seen = [inherited; 4];
    let standards = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];
    for (index, standard) in standards.into_iter().enumerate() {
        // SAFETY: this queries the current process's standard handle table.
        let handle = unsafe { GetStdHandle(standard) };
        if seen[..=index].contains(&handle) || validate_client(handle)? != owner {
            return Err(invalid_pipe());
        }
        seen[index + 1] = handle;
    }

    let mut duplicate = null_mut();
    // SAFETY: DuplicateHandle validates and duplicates this numeric handle
    // within our current process. The new handle is not inheritable. The
    // original stays process-owned until the worker exits.
    bool_result(unsafe {
        let process = GetCurrentProcess();
        DuplicateHandle(
            process,
            inherited,
            process,
            &mut duplicate,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    })?;
    let duplicate = owned_handle(duplicate)?;
    // SAFETY: this is a live owned named-pipe client handle. The launcher
    // opened it with FILE_FLAG_OVERLAPPED, which IOCP registration also requires.
    Ok(unsafe { NamedPipe::from_raw_handle(duplicate.into_raw_handle()) })
}

fn validate_client(inherited: HANDLE) -> io::Result<u32> {
    // SAFETY: Win32 checks this numeric handle before inspecting its type.
    if unsafe { GetFileType(inherited) } != FILE_TYPE_PIPE {
        return Err(invalid_pipe());
    }
    let mut flags = 0;
    // SAFETY: the only output pointer is writable. A wrong or closed handle
    // causes a normal API failure.
    bool_result(unsafe {
        GetNamedPipeInfo(inherited, &mut flags, null_mut(), null_mut(), null_mut())
    })?;
    let (client, server) = endpoints(inherited)?;
    if flags & (PIPE_SERVER_END | PIPE_TYPE_MESSAGE) != 0
        || client == 0
        || client != server
        || client == std::process::id()
    {
        return Err(invalid_pipe());
    }
    Ok(client)
}

fn endpoints(pipe: HANDLE) -> io::Result<(u32, u32)> {
    let (mut client, mut server) = (0, 0);
    // SAFETY: the functions validate the handle and write to the live PID outputs.
    unsafe {
        bool_result(GetNamedPipeClientProcessId(pipe, &mut client))?;
        bool_result(GetNamedPipeServerProcessId(pipe, &mut server))?;
    }
    Ok((client, server))
}

fn invalid_pipe() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "SSH worker relay must be a private local named-pipe client",
    )
}
