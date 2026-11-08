//! Cancellable I/O for the two desktop endpoints of the Windows SSH relay.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::windows::io::{AsRawHandle, AsRawSocket, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED, HANDLE,
};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent};
use windows_sys::core::BOOL;

/// Owns one endpoint of the desktop's SSH relay.
#[derive(Clone)]
pub struct WindowsRelayIo(Arc<Mutex<Option<Arc<Endpoint>>>>);

impl WindowsRelayIo {
    pub(super) fn pipe(handle: OwnedHandle) -> Self {
        Self::new(Endpoint::Pipe(handle))
    }

    /// Owns a connected TCP stream created by the Rust standard library on
    /// Windows, which opens its sockets for overlapped I/O.
    pub fn tcp(stream: TcpStream) -> Self {
        Self::new(Endpoint::Tcp(stream))
    }

    fn new(endpoint: Endpoint) -> Self {
        Self(Arc::new(Mutex::new(Some(Arc::new(endpoint)))))
    }

    /// Creates an independent reader with its own overlapped completion event.
    pub fn reader(&self) -> io::Result<WindowsRelayReader> {
        Operation::new(self.clone()).map(WindowsRelayReader)
    }

    /// Creates an independent writer with its own overlapped completion event.
    pub fn writer(&self) -> io::Result<WindowsRelayWriter> {
        Operation::new(self.clone()).map(WindowsRelayWriter)
    }

    /// Prevents further I/O and cancels pending operations. The endpoint
    /// remains alive only until those operations have observed completion.
    pub fn close(&self) {
        let mut shared = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(endpoint) = shared.take() {
            // SAFETY: this endpoint is owned here. Issuing I/O uses the same
            // lock, so no operation can start after this cancellation.
            unsafe { CancelIoEx(endpoint.raw(), null()) };
            if let Endpoint::Tcp(socket) = endpoint.as_ref() {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
    }
}

enum Endpoint {
    Pipe(OwnedHandle),
    Tcp(TcpStream),
}

impl Endpoint {
    fn raw(&self) -> HANDLE {
        match self {
            Self::Pipe(pipe) => pipe.as_raw_handle(),
            // ReadFile, WriteFile and CancelIoEx operate on overlapped IFS
            // socket handles. TcpStream still owns and closes the socket.
            Self::Tcp(socket) => socket.as_raw_socket() as usize as HANDLE,
        }
    }
}

/// Blocking read over a separately cancellable overlapped operation.
pub struct WindowsRelayReader(Operation);

impl Read for WindowsRelayReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let length = buffer.len().min(u32::MAX as usize) as u32;
        let result = self.0.run(|pipe, operation| {
            // SAFETY: buffer and operation remain live until completion.
            unsafe { ReadFile(pipe, buffer.as_mut_ptr(), length, null_mut(), operation) }
        });
        match result {
            Err(error)
                if error.raw_os_error().is_some_and(|code| {
                    [ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED]
                        .contains(&(code as u32))
                }) =>
            {
                Ok(0)
            }
            result => result,
        }
    }
}

/// Blocking write over a separately cancellable overlapped operation.
pub struct WindowsRelayWriter(Operation);

impl Write for WindowsRelayWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let length = buffer.len().min(u32::MAX as usize) as u32;
        self.0.run(|pipe, operation| {
            // SAFETY: buffer and operation remain live until completion.
            unsafe { WriteFile(pipe, buffer.as_ptr(), length, null_mut(), operation) }
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        // Each write has already waited for its kernel completion.
        Ok(())
    }
}

struct Operation {
    endpoint: WindowsRelayIo,
    event: OwnedHandle,
}

impl Operation {
    fn new(endpoint: WindowsRelayIo) -> io::Result<Self> {
        // SAFETY: an unnamed noninheritable manual-reset event requires no
        // input buffers. A non-null result transfers handle ownership.
        let handle = unsafe { CreateEventW(null(), 1, 0, null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let event = unsafe { OwnedHandle::from_raw_handle(handle) };
        Ok(Self { endpoint, event })
    }

    fn run(&mut self, start: impl FnOnce(HANDLE, *mut OVERLAPPED) -> BOOL) -> io::Result<usize> {
        let event = self.event.as_raw_handle();
        let mut storage = OVERLAPPED {
            hEvent: event,
            ..OVERLAPPED::default()
        };
        // The kernel can update this record until the completion is observed.
        let operation = &raw mut storage;
        // SAFETY: this operation exclusively owns the event until completion.
        if unsafe { ResetEvent(event) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let endpoint = {
            let shared = self
                .endpoint
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let endpoint = shared.as_ref().ok_or(io::ErrorKind::BrokenPipe)?;
            if start(endpoint.raw(), operation) == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                    return Err(error);
                }
            }
            Arc::clone(endpoint)
        };
        let mut transferred = 0;
        // SAFETY: the endpoint was opened for overlapped I/O. The operation,
        // event, endpoint and caller buffer stay live until completion, even
        // when another thread cancels the operation.
        if unsafe { GetOverlappedResult(endpoint.raw(), operation, &mut transferred, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(transferred as usize)
    }
}
