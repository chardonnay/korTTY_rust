use std::io;
use std::mem::{align_of, offset_of, size_of};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4};
use std::slice;

use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP_STATE_ESTAB, MIB_TCP_STATE_LISTEN, MIB_TCP6ROW_OWNER_PID,
    MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_CLASS,
    TCP_TABLE_OWNER_PID_CONNECTIONS, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};

const MAX_TABLE_BYTES: u32 = 8 * 1024 * 1024;
const MAX_ATTEMPTS: usize = 4;

pub(super) fn owns_loopback_listener(pid: u32, address: SocketAddrV4) -> io::Result<bool> {
    let v4 = TcpTable::read(AF_INET, TCP_TABLE_OWNER_PID_LISTENER)?;
    let mut candidates = v4.ipv4_rows()?.iter().filter(|row| {
        let local = ipv4(row.dwLocalAddr, row.dwLocalPort);
        row.dwState == MIB_TCP_STATE_LISTEN as u32
            && local.port() == address.port()
            && (local.ip() == address.ip() || local.ip().is_unspecified())
    });
    let Some(candidate) = candidates.next() else {
        return Ok(false);
    };
    if ipv4(candidate.dwLocalAddr, candidate.dwLocalPort) != address
        || candidate.dwOwningPid != pid
        || candidates.next().is_some()
    {
        return Ok(false);
    }

    // The table does not report IPV6_V6ONLY. Treat a possible IPv4 alias as ambiguous.
    let v6 = TcpTable::read(AF_INET6, TCP_TABLE_OWNER_PID_LISTENER)?;
    Ok(!v6.ipv6_rows()?.iter().any(|row| {
        let ip = Ipv6Addr::from(row.ucLocalAddr);
        row.dwState == MIB_TCP_STATE_LISTEN as u32
            && network_port(row.dwLocalPort) == address.port()
            && (ip.is_unspecified() || ip.to_ipv4_mapped() == Some(*address.ip()))
    }))
}

pub(super) fn owns_loopback_connection(
    pid: u32,
    local: SocketAddrV4,
    remote: SocketAddrV4,
) -> io::Result<bool> {
    let table = TcpTable::read(AF_INET, TCP_TABLE_OWNER_PID_CONNECTIONS)?;
    let mut candidates = table.ipv4_rows()?.iter().filter(|row| {
        row.dwState == MIB_TCP_STATE_ESTAB as u32
            && ipv4(row.dwLocalAddr, row.dwLocalPort) == local
            && ipv4(row.dwRemoteAddr, row.dwRemotePort) == remote
    });
    Ok(candidates.next().is_some_and(|row| row.dwOwningPid == pid) && candidates.next().is_none())
}

fn ipv4(address: u32, port: u32) -> SocketAddrV4 {
    SocketAddrV4::new(Ipv4Addr::from(address.to_ne_bytes()), network_port(port))
}

fn network_port(port: u32) -> u16 {
    u16::from_be(port as u16)
}

struct TcpTable {
    words: Vec<u32>,
    bytes: usize,
}

impl TcpTable {
    fn read(family: u16, class: TCP_TABLE_CLASS) -> io::Result<Self> {
        let mut required = 4096;
        for _ in 0..MAX_ATTEMPTS {
            if !(4..=MAX_TABLE_BYTES).contains(&required) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "TCP owner table exceeds the buffer limit",
                ));
            }
            let mut words = vec![0u32; (required as usize).div_ceil(size_of::<u32>())];
            let capacity = words.len() * size_of::<u32>();
            required = capacity as u32;
            // SAFETY: the aligned buffer contains at least `required` writable bytes.
            // The family and class identify one of the documented owner-PID tables.
            let result = unsafe {
                GetExtendedTcpTable(
                    words.as_mut_ptr().cast(),
                    &mut required,
                    0,
                    u32::from(family),
                    class,
                    0,
                )
            };
            match result {
                NO_ERROR if required as usize <= capacity => {
                    return Ok(Self {
                        words,
                        bytes: required as usize,
                    });
                }
                ERROR_INSUFFICIENT_BUFFER => {}
                NO_ERROR => return Err(invalid_table()),
                error => return Err(io::Error::from_raw_os_error(error as i32)),
            }
        }
        Err(io::Error::other("TCP owner table kept growing"))
    }

    fn ipv4_rows(&self) -> io::Result<&[MIB_TCPROW_OWNER_PID]> {
        // SAFETY: the IPv4 owner-PID ABI uses rows made of integer fields.
        unsafe { self.rows(offset_of!(MIB_TCPTABLE_OWNER_PID, table)) }
    }

    fn ipv6_rows(&self) -> io::Result<&[MIB_TCP6ROW_OWNER_PID]> {
        // SAFETY: the IPv6 owner-PID ABI uses rows made of integer fields.
        unsafe { self.rows(offset_of!(MIB_TCP6TABLE_OWNER_PID, table)) }
    }

    unsafe fn rows<T>(&self, offset: usize) -> io::Result<&[T]> {
        let count = self.words[0] as usize;
        let Some(body_bytes) = self.bytes.checked_sub(offset) else {
            return Err(invalid_table());
        };
        if count > body_bytes / size_of::<T>()
            || align_of::<T>() > align_of::<u32>()
            || !offset.is_multiple_of(align_of::<T>())
        {
            return Err(invalid_table());
        }
        // SAFETY: size and alignment were checked and the caller supplies the ABI row type.
        // The initialized integer storage remains alive and immutable for the slice's lifetime.
        Ok(unsafe {
            slice::from_raw_parts(self.words.as_ptr().cast::<u8>().add(offset).cast(), count)
        })
    }
}

fn invalid_table() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid TCP owner table")
}
