use crate::protocol::MAX_RESOLVE_PIDS;

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

#[derive(Clone, Copy, Eq, PartialEq)]
struct Process {
    parent: u32,
    started: (u64, u64),
}

pub(super) fn ancestors() -> Vec<u32> {
    let current = std::process::id();
    #[cfg(target_os = "linux")]
    if !procfs_uses_our_pid_namespace(current) {
        return Vec::new();
    }
    #[cfg(unix)]
    let lookup = process;
    #[cfg(windows)]
    let lookup = match windows::Snapshot::new() {
        Some(snapshot) => move |pid| snapshot.process(pid),
        None => return Vec::new(),
    };
    walk(current, lookup)
}

fn walk(current: u32, lookup: impl Fn(u32) -> Option<Process>) -> Vec<u32> {
    let Some(mut child) = lookup(current) else {
        return Vec::new();
    };
    let mut result = vec![current];
    let mut child_pid = current;
    while result.len() < MAX_RESOLVE_PIDS && child.parent != 0 && !result.contains(&child.parent) {
        let Some(parent) = lookup(child.parent) else {
            break;
        };
        // A parent PID can outlive its link in a process-table snapshot. A
        // process started after its alleged child cannot be that parent.
        if parent.started > child.started || lookup(child_pid) != Some(child) {
            break;
        }
        child_pid = child.parent;
        result.push(child_pid);
        child = parent;
    }
    result
}

#[cfg(target_os = "linux")]
fn procfs_uses_our_pid_namespace(current: u32) -> bool {
    use std::io::Read as _;

    let Ok(status) = std::fs::File::open("/proc/self/status") else {
        return false;
    };
    let mut record = Vec::new();
    if status.take(16 * 1024).read_to_end(&mut record).is_err() {
        return false;
    }
    procfs_status_matches(current, &record)
}

#[cfg(target_os = "linux")]
fn procfs_status_matches(current: u32, record: &[u8]) -> bool {
    // A procfs mounted in an outer PID namespace lists another ID before the
    // current namespace ID, even if the two numbers happen to coincide.
    let fields = record
        .split_inclusive(|&byte| byte == b'\n')
        .find_map(|line| {
            std::str::from_utf8(line.strip_prefix(b"NStgid:")?.strip_suffix(b"\n")?).ok()
        });
    fields.is_some_and(|fields| {
        let mut ids = fields.split_ascii_whitespace();
        ids.next().and_then(|id| id.parse::<u32>().ok()) == Some(current) && ids.next().is_none()
    })
}

#[cfg(target_os = "linux")]
fn process(pid: u32) -> Option<Process> {
    use std::io::Read as _;

    const STAT_LIMIT: u64 = 4096;
    let mut record = Vec::new();
    std::fs::File::open(format!("/proc/{pid}/stat"))
        .ok()?
        .take(STAT_LIMIT + 1)
        .read_to_end(&mut record)
        .ok()?;
    if record.len() as u64 > STAT_LIMIT {
        return None;
    }
    parse_linux_stat(pid, &record)
}

#[cfg(target_os = "linux")]
fn parse_linux_stat(pid: u32, record: &[u8]) -> Option<Process> {
    let start = record.iter().position(|&byte| byte == b' ')?;
    let end = record.iter().rposition(|&byte| byte == b')')?;
    let found = std::str::from_utf8(&record[..start])
        .ok()?
        .parse::<u32>()
        .ok()?;
    if found != pid || end <= start + 1 || record.get(start + 1) != Some(&b'(') {
        return None;
    }
    // comm can contain spaces, right parentheses and non-UTF-8 bytes. Fields
    // after its final delimiter are ASCII. PPID is field 4, starttime is 22.
    let mut fields = std::str::from_utf8(&record[end + 1..])
        .ok()?
        .split_ascii_whitespace();
    let state = fields.next()?;
    if matches!(state, "Z" | "X" | "x") {
        return None;
    }
    let parent = fields.next()?.parse().ok()?;
    let started = fields.nth(17)?.parse().ok()?;
    Some(Process {
        parent,
        started: (started, 0),
    })
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub(super) fn same_pid_namespace(
    stream: &std::os::unix::net::UnixStream,
    endpoint_pid: u32,
) -> bool {
    use std::os::{fd::AsRawFd as _, unix::fs::MetadataExt as _};

    let mut credentials = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let mut length = size;
    // SAFETY: the connected socket remains live and the buffer and its size
    // are writable. ucred contains only integer fields, including PID zero
    // when the peer is invisible in the caller's PID namespace.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if result != 0 || length != size {
        return false;
    }
    // SAFETY: getsockopt succeeded and filled the entire ucred.
    let pid = unsafe { credentials.assume_init() }.pid;
    if pid <= 0 || pid as u32 != endpoint_pid {
        return false;
    }
    let own = std::fs::metadata("/proc/self/ns/pid");
    let peer = std::fs::metadata(format!("/proc/{pid}/ns/pid"));
    matches!((own, peer), (Ok(own), Ok(peer)) if (own.dev(), own.ino()) == (peer.dev(), peer.ino()))
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn process(pid: u32) -> Option<Process> {
    let process_id = libc::pid_t::try_from(pid).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: proc_pidinfo receives a writable buffer of the exact size of
    // the SDK's PROC_PIDTBSDINFO record.
    let count = unsafe {
        libc::proc_pidinfo(
            process_id,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if count != size {
        return None;
    }
    // SAFETY: libproc reports that it filled the complete record.
    let info = unsafe { info.assume_init() };
    if info.pbi_pid != pid || info.pbi_status == libc::SZOMB {
        return None;
    }
    Some(Process {
        parent: info.pbi_ppid,
        started: (info.pbi_start_tvsec, info.pbi_start_tvusec),
    })
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn process(_pid: u32) -> Option<Process> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_ancestry_starts_with_this_process_and_is_bounded() {
        let pids = ancestors();
        assert_eq!(pids.first().copied(), Some(std::process::id()));
        assert!(pids.len() <= MAX_RESOLVE_PIDS);
    }

    #[test]
    fn rejects_reused_parent_ids_and_stops_cycles() {
        // Test double for process-table entries supplied by the OS.
        let lookup = |pid| match pid {
            2 => Some(Process {
                parent: 3,
                started: (1, 0),
            }),
            3 => Some(Process {
                parent: 0,
                started: (2, 0),
            }),
            _ => None,
        };
        assert_eq!(walk(2, lookup), [2]);
        let cycle = |pid| {
            Some(Process {
                parent: if pid == 2 { 3 } else { 2 },
                started: (1, 0),
            })
        };
        assert_eq!(walk(2, cycle), [2, 3]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_rejects_procfs_mounted_in_another_namespace() {
        assert!(procfs_status_matches(23, b"Name:\tcli\nNStgid:\t23\n"));
        assert!(!procfs_status_matches(23, b"Name:\tcli\nNStgid:\t23\t23\n"));
        assert!(!procfs_status_matches(23, b"Name:\tcli\nNStgid:\t23"));
        assert!(!procfs_status_matches(23, b"Name:\tcli\nNStgid:\t42\n"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_stat_keeps_kernel_field_offsets_with_unusual_process_names() {
        let mut record = b"23 (name )\xff with spaces) S 17".to_vec();
        record.extend_from_slice(b" 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 99 0");
        let info = parse_linux_stat(23, &record).unwrap();
        assert_eq!(info.parent, 17);
        assert_eq!(info.started, (99, 0));
        assert!(parse_linux_stat(22, &record).is_none());
    }
}
