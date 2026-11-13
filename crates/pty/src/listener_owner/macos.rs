//! Exact TCP ownership through the macOS process descriptor table.

use std::io;
use std::mem::{MaybeUninit, offset_of, size_of, size_of_val};
use std::net::{Ipv4Addr, SocketAddrV4};

const MAX_DESCRIPTORS: usize = 4096;
const PROC_PIDFDSOCKETINFO: libc::c_int = 3;
const PROC_FP_SHARED: u32 = 1;
const SOCKINFO_TCP: libc::c_int = 2;
const INI_IPV4: u8 = 1;
const TSI_S_LISTEN: libc::c_int = 1;
const TSI_S_ESTABLISHED: libc::c_int = 4;

// The C layouts come from XNU bsd/sys/proc_info.h. Only the file status and
// TCP member of the protocol union are used.
#[repr(C)]
struct FileInfo {
    _open_flags: u32,
    status: u32,
    _offset: i64,
    _kind: i32,
    _guard_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Ipv4InIpv6 {
    _padding: [u32; 3],
    address: [u8; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TcpInfo {
    foreign_port: i32,
    local_port: i32,
    _generation: u64,
    _flags: u32,
    _flow: u32,
    ip_version: u8,
    _ttl: u8,
    _reserved: u32,
    foreign_address: Ipv4InIpv6,
    local_address: Ipv4InIpv6,
    _address_options: [u32; 4],
    state: i32,
    _timers: [i32; 4],
    _mss: i32,
    _tcp_flags: u32,
    _tcp_reserved: u32,
    _control_block: u64,
}

// The largest member is un_sockinfo, with two handles and two address unions.
const PROTOCOL_WORDS: usize =
    (2 * size_of::<u64>() + 2 * libc::SOCK_MAXADDRLEN as usize).div_ceil(size_of::<u64>());

#[repr(C)]
union ProtocolInfo {
    tcp: TcpInfo,
    _storage: [u64; PROTOCOL_WORDS],
}

#[repr(C)]
struct SocketFdInfo {
    file: FileInfo,
    _stat: libc::vinfo_stat,
    _socket: u64,
    _control_block: u64,
    socket_type: i32,
    protocol: i32,
    family: i32,
    options: i16,
    _socket_fields: [u16; 7],
    _out_of_band_mark: u32,
    _socket_buffers: [u32; 12],
    kind: i32,
    _reserved: u32,
    details: ProtocolInfo,
}

const _: () = {
    assert!(size_of::<SocketFdInfo>() == 792);
    assert!(offset_of!(SocketFdInfo, details) == 264);
    assert!(offset_of!(TcpInfo, state) == 80);
};

pub(super) fn owns_loopback_listener(pid: u32, address: SocketAddrV4) -> io::Result<bool> {
    owns_socket(pid, address, None)
}

pub(super) fn owns_loopback_connection(
    pid: u32,
    local: SocketAddrV4,
    remote: SocketAddrV4,
) -> io::Result<bool> {
    owns_socket(pid, local, Some(remote))
}

fn owns_socket(pid: u32, local: SocketAddrV4, remote: Option<SocketAddrV4>) -> io::Result<bool> {
    let pid = i32::try_from(pid)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid process ID"))?;
    let mut records = [const { MaybeUninit::<libc::proc_fdinfo>::uninit() }; MAX_DESCRIPTORS];
    let bytes = size_of_val(&records);
    // SAFETY: PROC_PIDLISTFDS writes complete proc_fdinfo records into this array.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDLISTFDS,
            0,
            records.as_mut_ptr().cast(),
            bytes as libc::c_int,
        )
    };
    if written <= 0 {
        return Err(io::Error::last_os_error());
    }
    let written = written as usize;
    if written >= bytes || !written.is_multiple_of(size_of::<libc::proc_fdinfo>()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Process descriptor table is incomplete",
        ));
    }

    let mut found = false;
    for record in &records[..written / size_of::<libc::proc_fdinfo>()] {
        // SAFETY: the result length bounds this reference to a complete record.
        let record = unsafe { record.assume_init_ref() };
        if record.proc_fdtype != libc::PROX_FDTYPE_SOCKET as u32 {
            continue;
        }
        let Some(info) = socket_info(pid, record.proc_fd)? else {
            continue;
        };
        if !matches_socket(&info, local, remote) {
            continue;
        }
        if found
            || info.file.status & PROC_FP_SHARED != 0
            || (remote.is_none() && i32::from(info.options) & libc::SO_REUSEPORT != 0)
        {
            return Ok(false);
        }
        found = true;
    }
    Ok(found)
}

fn socket_info(pid: i32, descriptor: i32) -> io::Result<Option<SocketFdInfo>> {
    let mut info = MaybeUninit::<SocketFdInfo>::uninit();
    let size = size_of::<SocketFdInfo>() as libc::c_int;
    // SAFETY: the output buffer has the layout and size selected by the socket flavor.
    let written = unsafe {
        libc::proc_pidfdinfo(
            pid,
            descriptor,
            PROC_PIDFDSOCKETINFO,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written == size {
        // SAFETY: XNU returned the complete socket descriptor record.
        return Ok(Some(unsafe { info.assume_init() }));
    }
    if written <= 0 {
        let error = io::Error::last_os_error();
        // A descriptor can disappear or be reused after the list call.
        if matches!(error.raw_os_error(), Some(libc::EBADF | libc::ENOTSOCK)) {
            return Ok(None);
        }
        return Err(error);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Socket descriptor record has an unexpected size",
    ))
}

fn matches_socket(info: &SocketFdInfo, local: SocketAddrV4, remote: Option<SocketAddrV4>) -> bool {
    if info.kind != SOCKINFO_TCP
        || info.family != libc::AF_INET
        || info.socket_type != libc::SOCK_STREAM
        || !matches!(info.protocol, 0 | libc::IPPROTO_TCP)
    {
        return false;
    }
    // SAFETY: XNU marks the TCP member of the union with SOCKINFO_TCP.
    let tcp = unsafe { info.details.tcp };
    let matches_endpoint = |address: Ipv4InIpv6, port, expected: SocketAddrV4| {
        address.address == expected.ip().octets()
            && u16::try_from(port) == Ok(expected.port().to_be())
    };
    if tcp.ip_version & INI_IPV4 == 0 || !matches_endpoint(tcp.local_address, tcp.local_port, local)
    {
        return false;
    }
    match remote {
        Some(remote) => {
            tcp.state == TSI_S_ESTABLISHED
                && matches_endpoint(tcp.foreign_address, tcp.foreign_port, remote)
        }
        None => {
            tcp.state == TSI_S_LISTEN
                && tcp.foreign_port == 0
                && tcp.foreign_address.address == Ipv4Addr::UNSPECIFIED.octets()
        }
    }
}
