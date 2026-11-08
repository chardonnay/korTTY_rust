use std::fs;
use std::io;
use std::mem::MaybeUninit;
use std::net::TcpStream;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt as _;
use std::process::Command;

use super::{
    check_landlock_abi, close_for_exec, create_ruleset, enforce, no_new_privileges, open_probe,
    require_denied, seccomp, system_result, unprivileged_user,
};

const FIRST_TRANSPORT_FD: RawFd = 64;
// Linux UAPI LANDLOCK_ACCESS_NET_BIND_TCP and LANDLOCK_ACCESS_NET_CONNECT_TCP.
const DENY_TCP_BIND_CONNECT: u64 = 0b11;

pub(crate) fn check_available() -> io::Result<()> {
    unprivileged_user()?;
    check_landlock_abi()
}

pub(crate) fn attach(command: &mut Command, tcp: TcpStream) -> io::Result<u32> {
    check_available()?;
    require_tcp(tcp.as_raw_fd())?;
    tcp.peer_addr()?;
    let transport = duplicate_transport(tcp.as_raw_fd())?;
    let descriptor = transport.as_raw_fd();
    command.env_clear().current_dir("/");
    // SAFETY: the closure owns its socket. After fork it only uses direct
    // descriptor syscalls, numeric comparisons and OS error capture.
    unsafe {
        command.pre_exec(move || {
            close_for_exec()?;
            no_new_privileges()?;
            // Keep the std exec error channel close-on-exec. Only the
            // approved transport gets its close-on-exec bit cleared.
            system_result(libc::fcntl(transport.as_raw_fd(), libc::F_SETFD, 0).into())?;
            Ok(())
        });
    }
    Ok(descriptor as u32)
}

pub(crate) fn enter(descriptor: u32) -> io::Result<TcpStream> {
    check_available()?;
    require_pipe(libc::STDIN_FILENO, libc::O_RDONLY)?;
    require_pipe(libc::STDOUT_FILENO, libc::O_WRONLY)?;
    require_pipe(libc::STDERR_FILENO, libc::O_RDONLY)?;
    let descriptor = RawFd::try_from(descriptor)
        .ok()
        .filter(|&fd| fd >= FIRST_TRANSPORT_FD)
        .ok_or_else(invalid_transport)?;
    // Keep the inherited descriptor owned by the worker process. Duplicating
    // it prevents adopting an arbitrary caller-owned descriptor as a TcpStream.
    let socket = duplicate_transport(descriptor)?;
    require_tcp(socket.as_raw_fd())?;
    let tcp = TcpStream::from(socket);
    tcp.peer_addr()?;
    let ruleset = create_ruleset(DENY_TCP_BIND_CONNECT)?;
    let filter = seccomp::worker_filter();
    open_probe(c"/proc/self/status", 0)?;
    // Landlock ABI 6 only confines the current thread and future descendants.
    // Fail if a caller started any sibling before entering the sandbox.
    {
        let mut tasks = fs::read_dir("/proc/self/task")?;
        if tasks.next().transpose()?.is_none() || tasks.next().transpose()?.is_some() {
            return Err(io::Error::other(
                "SSH sandbox must start before worker threads",
            ));
        }
    }
    // SAFETY: PR_SET_DUMPABLE affects this process and takes a flag plus zeros.
    system_result(
        unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0_u64, 0_u64, 0_u64, 0_u64) }.into(),
    )?;
    // Zero filesystem or network rules grant any access within the handled rights.
    enforce(&ruleset)?;
    require_denied(c"/proc/self/status", 0)?;
    seccomp::apply(&filter)?;
    prove_socket_denied()?;
    Ok(tcp)
}

fn duplicate_transport(descriptor: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: fcntl validates the descriptor and returns a new owned duplicate.
    let duplicate = system_result(
        unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, FIRST_TRANSPORT_FD) }.into(),
    )?;
    // SAFETY: fcntl succeeded, and this call alone owns the new descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate as RawFd) })
}

fn require_tcp(descriptor: RawFd) -> io::Result<()> {
    for (option, expected) in [
        (libc::SO_TYPE, libc::SOCK_STREAM),
        (libc::SO_PROTOCOL, libc::IPPROTO_TCP),
    ] {
        let mut value = 0;
        let mut length = size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: the output buffer and socklen_t are live and writable.
        system_result(
            unsafe {
                libc::getsockopt(
                    descriptor,
                    libc::SOL_SOCKET,
                    option,
                    (&raw mut value).cast(),
                    &mut length,
                )
            }
            .into(),
        )?;
        if value != expected || length as usize != size_of::<libc::c_int>() {
            return Err(invalid_transport());
        }
    }
    Ok(())
}

fn require_pipe(descriptor: RawFd, access: libc::c_int) -> io::Result<()> {
    let mut status = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat validates the descriptor and fills the live output buffer.
    system_result(unsafe { libc::fstat(descriptor, status.as_mut_ptr()) }.into())?;
    // SAFETY: a successful fstat initialized the stat buffer.
    let mode = unsafe { status.assume_init().st_mode };
    // SAFETY: fcntl only checks flags on this numeric descriptor.
    let flags = system_result(unsafe { libc::fcntl(descriptor, libc::F_GETFL) }.into())?;
    if mode & libc::S_IFMT != libc::S_IFIFO
        || flags & libc::O_ACCMODE as libc::c_long != access.into()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SSH sandbox requires private standard pipes",
        ));
    }
    Ok(())
}

fn invalid_transport() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid inherited SSH transport",
    )
}

fn prove_socket_denied() -> io::Result<()> {
    // SAFETY: the arguments name an ordinary IPv4 TCP socket. Close it if an
    // unexpected permissive policy ever lets it be created.
    let descriptor = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, libc::IPPROTO_TCP) };
    if descriptor >= 0 {
        // SAFETY: socket returned this new owned descriptor.
        drop(unsafe { OwnedFd::from_raw_fd(descriptor) });
        return Err(io::Error::other("SSH sandbox left ambient TCP enabled"));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(libc::EPERM) {
        return Err(error);
    }
    Ok(())
}
