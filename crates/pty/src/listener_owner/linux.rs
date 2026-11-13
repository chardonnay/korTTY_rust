use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

const MAX_TABLE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_DESCRIPTORS: usize = 32 * 1024;
const LISTEN: &str = "0A";
const ESTABLISHED: &str = "01";

pub(super) fn owns_loopback_listener(pid: u32, address: SocketAddrV4) -> io::Result<bool> {
    owns(pid, address, None)
}

pub(super) fn owns_loopback_connection(
    pid: u32,
    local: SocketAddrV4,
    remote: SocketAddrV4,
) -> io::Result<bool> {
    owns(pid, local, Some(remote))
}

fn owns(pid: u32, local: SocketAddrV4, remote: Option<SocketAddrV4>) -> io::Result<bool> {
    if !crate::process_usage::procfs_uses_our_pid_namespace() {
        return Err(io::Error::other(
            "procfs does not represent this PID namespace",
        ));
    }
    // Keep the opened task directory alive while checking its namespace, TCP table and FDs.
    // The numeric PID pathname must not be resolved again after this open.
    let result = (|| {
        let task = File::open(format!("/proc/{pid}"))?;
        let task_path = format!("/proc/self/fd/{}", task.as_raw_fd());
        inspect_task(Path::new(&task_path), local, remote)
    })();
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        result => result,
    }
}

fn inspect_task(
    task: &Path,
    local: SocketAddrV4,
    remote: Option<SocketAddrV4>,
) -> io::Result<bool> {
    let child_namespace = fs::metadata(task.join("ns/net"))?;
    let our_namespace = fs::metadata("/proc/self/ns/net")?;
    if (child_namespace.dev(), child_namespace.ino()) != (our_namespace.dev(), our_namespace.ino())
    {
        return Ok(false);
    }

    let address = ipv4_endpoint(local);
    let wildcard = ipv4_endpoint(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, local.port()));
    let peer = remote.map(ipv4_endpoint);
    let state = if peer.is_some() { ESTABLISHED } else { LISTEN };
    let Some((inode, EndpointMatch::Exact)) = matching_inode(
        File::open(task.join("net/tcp"))?,
        |bound, connected, row_state| {
            if row_state != state {
                return None;
            }
            match &peer {
                Some(peer) if bound == address && connected == peer => Some(EndpointMatch::Exact),
                None if bound == address => Some(EndpointMatch::Exact),
                None if bound == wildcard => Some(EndpointMatch::Alias),
                _ => None,
            }
        },
    )?
    else {
        return Ok(false);
    };

    if peer.is_none() {
        // A v6 wildcard may be dual-stack. Do not accept an ambiguous IPv4 listener.
        match File::open(task.join("net/tcp6")) {
            Ok(table) => {
                let unspecified = ipv6_endpoint(Ipv6Addr::UNSPECIFIED, local.port());
                let mapped = ipv6_endpoint(local.ip().to_ipv6_mapped(), local.port());
                if matching_inode(table, |bound, _, row_state| {
                    (row_state == LISTEN && (bound == unspecified || bound == mapped))
                        .then_some(EndpointMatch::Alias)
                })?
                .is_some()
                {
                    return Ok(false);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // The IPv6 table can be absent when the kernel has IPv6 disabled.
            }
            Err(error) => return Err(error),
        }
    }

    if inode == 0 {
        // A completed handshake awaiting accept has no userspace descriptor yet.
        return Ok(false);
    }
    let descriptor = format!("socket:[{inode}]");
    for (index, entry) in fs::read_dir(task.join("fd"))?.enumerate() {
        if index == MAX_DESCRIPTORS {
            return Err(io::Error::other(
                "process exceeds the descriptor scan limit",
            ));
        }
        match fs::read_link(entry?.path()) {
            Ok(target) if target.as_os_str() == descriptor.as_str() => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

enum EndpointMatch {
    Exact,
    Alias,
}

fn matching_inode(
    file: File,
    matches: impl Fn(&str, &str, &str) -> Option<EndpointMatch>,
) -> io::Result<Option<(u64, EndpointMatch)>> {
    let mut table = BufReader::new(file.take(MAX_TABLE_BYTES + 1));
    let mut line = String::new();
    let mut bytes = table.read_line(&mut line)?;
    if !line
        .split_ascii_whitespace()
        .any(|word| word == "local_address")
    {
        return Err(invalid_table());
    }

    let mut inode = None;
    loop {
        line.clear();
        let size = table.read_line(&mut line)?;
        bytes += size;
        if bytes as u64 > MAX_TABLE_BYTES {
            return Err(io::Error::other(
                "TCP ownership table exceeds the scan limit",
            ));
        }
        if size == 0 {
            return Ok(inode);
        }
        let mut fields = line.split_ascii_whitespace();
        let local = fields.nth(1).ok_or_else(invalid_table)?;
        let remote = fields.next().ok_or_else(invalid_table)?;
        let state = fields.next().ok_or_else(invalid_table)?;
        if let Some(endpoint) = matches(local, remote, state) {
            let entry = fields
                .nth(5)
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(invalid_table)?;
            if inode.replace((entry, endpoint)).is_some() {
                return Err(io::Error::other("ambiguous TCP socket ownership"));
            }
        }
    }
}

fn ipv4_endpoint(address: SocketAddrV4) -> String {
    // procfs prints each address word as a native integer.
    format!(
        "{:08X}:{:04X}",
        u32::from_ne_bytes(address.ip().octets()),
        address.port()
    )
}

fn ipv6_endpoint(address: Ipv6Addr, port: u16) -> String {
    let [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] = address.octets();
    format!(
        "{:08X}{:08X}{:08X}{:08X}:{port:04X}",
        u32::from_ne_bytes([a, b, c, d]),
        u32::from_ne_bytes([e, f, g, h]),
        u32::from_ne_bytes([i, j, k, l]),
        u32::from_ne_bytes([m, n, o, p]),
    )
}

fn invalid_table() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid TCP ownership table")
}
