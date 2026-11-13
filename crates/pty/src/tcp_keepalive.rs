//! The operating system's TCP keepalive flag on the desktop transport socket.

use std::io;
use std::net::TcpStream;

/// Shuts down a connected socket and wakes an outstanding Windows socket read or write.
#[cfg(windows)]
pub fn shutdown_tcp_io(socket: &TcpStream) {
    use std::net::Shutdown;
    use std::os::windows::io::AsRawSocket as _;
    use std::ptr::null;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::IO::CancelIoEx;

    let _ = socket.shutdown(Shutdown::Both);
    // SAFETY: the borrowed TcpStream keeps its overlapped Winsock handle live.
    // Passing a null operation cancels all outstanding I/O on this socket.
    unsafe { CancelIoEx(socket.as_raw_socket() as usize as HANDLE, null()) };
}

/// Sets `SO_KEEPALIVE` without changing the kernel's timeout or retry policy.
#[cfg(unix)]
pub fn set_tcp_keepalive(socket: &TcpStream, enabled: bool) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;

    let value = libc::c_int::from(enabled);
    // SAFETY: the borrowed stream owns the socket, and value stays live for the call.
    let status = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_KEEPALIVE,
            (&raw const value).cast(),
            size_of_val(&value) as libc::socklen_t,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Sets `SO_KEEPALIVE` without changing the kernel's timeout or retry policy.
#[cfg(windows)]
pub fn set_tcp_keepalive(socket: &TcpStream, enabled: bool) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket as _;
    use windows_sys::Win32::Networking::WinSock::{
        SO_KEEPALIVE, SOL_SOCKET, WSAGetLastError, setsockopt,
    };

    let value = i32::from(enabled);
    // SAFETY: the borrowed stream owns the Winsock socket. The option value is a live BOOL.
    let status = unsafe {
        setsockopt(
            socket.as_raw_socket() as usize,
            SOL_SOCKET,
            SO_KEEPALIVE,
            (&raw const value).cast(),
            size_of_val(&value) as i32,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        // SAFETY: reads this thread's Winsock error after a failed call.
        Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_and_clears_the_connected_tcp_socket_flag() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let _peer = listener.accept().unwrap();
        for enabled in [true, false] {
            set_tcp_keepalive(&client, enabled).unwrap();
            assert_eq!(socket_flag(&client), enabled);
        }
    }

    #[cfg(unix)]
    fn socket_flag(socket: &TcpStream) -> bool {
        use std::os::fd::AsRawFd as _;
        let mut value = 0i32;
        let mut length = size_of_val(&value) as libc::socklen_t;
        // SAFETY: both output slots are live and sized for the socket option.
        let status = unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_KEEPALIVE,
                (&raw mut value).cast(),
                &mut length,
            )
        };
        assert_eq!(status, 0);
        value != 0
    }

    #[cfg(windows)]
    fn socket_flag(socket: &TcpStream) -> bool {
        use std::os::windows::io::AsRawSocket as _;
        use windows_sys::Win32::Networking::WinSock::{SO_KEEPALIVE, SOL_SOCKET, getsockopt};
        let mut value = 0i32;
        let mut length = size_of_val(&value) as i32;
        // SAFETY: both output slots are live and sized for the socket option.
        let status = unsafe {
            getsockopt(
                socket.as_raw_socket() as usize,
                SOL_SOCKET,
                SO_KEEPALIVE,
                (&raw mut value).cast(),
                &mut length,
            )
        };
        assert_eq!(status, 0);
        value != 0
    }
}
