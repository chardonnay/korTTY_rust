use std::fs::File;

use crate::JobProcessIdentity;

#[cfg(target_os = "linux")]
pub(super) fn find<T>(
    master: &File,
    shell: u32,
    previous: Option<JobProcessIdentity>,
    inspect: impl Fn(&str, &[&str]) -> Option<T>,
) -> Option<(JobProcessIdentity, T)> {
    use std::os::fd::AsRawFd as _;

    const MAX_PROCESSES: usize = 8192;
    if !crate::process_usage::procfs_uses_our_pid_namespace() {
        return None;
    }
    let foreground = || {
        // SAFETY: the actor owns the open master for the duration of the query.
        u32::try_from(unsafe { libc::tcgetpgrp(master.as_raw_fd()) })
            .ok()
            .filter(|&group| group != 0)
    };
    let group = foreground()?;
    let examine = |pid| linux_inspect(pid, shell, group, &inspect);
    let found = previous
        .and_then(|id| examine(id.pid).filter(|(found, _)| *found == id))
        .or_else(|| examine(group))
        .or_else(|| {
            std::fs::read_dir("/proc")
                .ok()?
                .take(MAX_PROCESSES)
                .flatten()
                .find_map(|entry| {
                    let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
                    examine((pid != group).then_some(pid)?)
                })
        })?;
    (foreground() == Some(group)).then_some(found)
}

#[cfg(target_os = "linux")]
fn linux_inspect<T>(
    pid: u32,
    shell: u32,
    group: u32,
    inspect: &impl Fn(&str, &[&str]) -> Option<T>,
) -> Option<(JobProcessIdentity, T)> {
    use std::io::Read as _;

    fn read_bounded(path: String, max: u64) -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        File::open(path)
            .ok()?
            .take(max + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() as u64 <= max).then_some(bytes)
    }

    let member = || parse_linux_member(&read_bounded(format!("/proc/{pid}/stat"), 4096)?);
    let (actual_group, session, started) = member()?;
    if actual_group != group || session != shell {
        return None;
    }
    let image = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let name = image.to_string_lossy();
    let found = inspect(&name, &[]).or_else(|| {
        let args = read_bounded(format!("/proc/{pid}/cmdline"), 32 * 1024)?;
        let args = String::from_utf8_lossy(&args);
        let args = args
            .trim_end_matches('\0')
            .split('\0')
            .skip(1)
            .take(64)
            .collect::<Vec<_>>();
        inspect(&name, &args)
    })?;
    (member() == Some((group, shell, started))).then_some((
        JobProcessIdentity {
            pid,
            started: (started, 0),
        },
        found,
    ))
}

#[cfg(target_os = "linux")]
fn parse_linux_member(record: &[u8]) -> Option<(u32, u32, u64)> {
    // The command name can contain spaces, right parentheses and non-UTF-8 bytes.
    let end = record.iter().rposition(|&byte| byte == b')')?;
    let mut fields = std::str::from_utf8(&record[end + 1..])
        .ok()?
        .split_ascii_whitespace();
    if matches!(fields.next()?, "Z" | "X" | "x") {
        return None;
    }
    let group = fields.nth(1)?.parse().ok()?;
    let session = fields.next()?.parse().ok()?;
    let started = fields.nth(15)?.parse().ok()?;
    Some((group, session, started))
}

#[cfg(target_os = "macos")]
pub(super) fn find<T>(
    _master: &File,
    shell: u32,
    previous: Option<JobProcessIdentity>,
    inspect: impl Fn(&str, &[&str]) -> Option<T>,
) -> Option<(JobProcessIdentity, T)> {
    fn info(pid: libc::pid_t) -> Option<libc::proc_bsdinfo> {
        let mut record = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
        let size = size_of::<libc::proc_bsdinfo>() as i32;
        // SAFETY: the output has the SDK's structure and writable size for this flavor.
        if unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                record.as_mut_ptr().cast(),
                size,
            )
        } != size
        {
            return None;
        }
        // SAFETY: libproc reported that the entire record was initialized.
        let record = unsafe { record.assume_init() };
        (record.pbi_pid == pid as u32 && record.pbi_status != libc::SZOMB).then_some(record)
    }

    let shell = libc::pid_t::try_from(shell).ok()?;
    let anchor = info(shell)?;
    let group = libc::pid_t::try_from(anchor.e_tpgid).ok()?;
    if group <= 0 {
        return None;
    }
    let examine = |pid| {
        if pid <= 0 {
            return None;
        }
        let member = || {
            let record = info(pid)?;
            // SAFETY: getsid only queries the numeric PID's process session.
            (unsafe { libc::getsid(pid) } == shell
                && record.pbi_pgid == group as u32
                && record.e_tdev == anchor.e_tdev)
                .then_some((record.pbi_start_tvsec, record.pbi_start_tvusec))
        };
        let started = member()?;
        let mut path = [0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: path owns the full writable buffer for the duration of the query.
        let length =
            unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
        let end = path
            .iter()
            .position(|&byte| byte == 0)
            .filter(|&end| end > 0 && length > 0)?;
        let found = inspect(&String::from_utf8_lossy(&path[..end]), &[])?;
        (member() == Some(started)).then_some((
            JobProcessIdentity {
                pid: pid as u32,
                started,
            },
            found,
        ))
    };
    let found = previous
        .and_then(|id| examine(id.pid as libc::pid_t).filter(|(found, _)| *found == id))
        .or_else(|| {
            let mut members = [0_i32; 1024];
            // SAFETY: the output buffer holds the given byte capacity for process IDs.
            let count = unsafe {
                libc::proc_listpgrppids(
                    group,
                    members.as_mut_ptr().cast(),
                    size_of_val(&members) as i32,
                )
            };
            members
                .get(..usize::try_from(count).ok()?)?
                .iter()
                .copied()
                .find_map(examine)
        })?;
    info(shell)
        .is_some_and(|current| current.e_tpgid == anchor.e_tpgid)
        .then_some(found)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn job_process_stat_keeps_offsets_after_unusual_process_names() {
        let mut record = b"23 (name )\xff with spaces) S 17 23 21".to_vec();
        record.extend_from_slice(b" 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 99");
        assert_eq!(parse_linux_member(&record), Some((23, 21, 99)));
        assert_eq!(parse_linux_member(b"24 (gone) Z 17 23 21"), None);
    }
}
