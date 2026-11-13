use std::process::Child;
use std::time::Duration;

/// Resident memory and cumulative CPU time of a process.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessUsage {
    pub resident_bytes: Option<u64>,
    pub cpu_time: Option<Duration>,
}

/// Measures the calling process without opening a process by a reusable ID on Windows.
pub fn current_process_usage() -> ProcessUsage {
    #[cfg(unix)]
    return pid_usage(std::process::id());
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        // SAFETY: the process pseudo-handle is valid in the calling process.
        handle_usage(unsafe { GetCurrentProcess() })
    }
}

/// Measures a child before another caller can wait on or release it.
/// A child which has already exited has no current measurement.
pub fn child_usage(child: &mut Child) -> ProcessUsage {
    if !matches!(child.try_wait(), Ok(None)) {
        return ProcessUsage::default();
    }
    #[cfg(unix)]
    return pid_usage(child.id());
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle as _;
        handle_usage(child.as_raw_handle())
    }
}

#[cfg(target_os = "linux")]
fn pid_usage(pid: u32) -> ProcessUsage {
    use std::io::Read as _;
    use std::sync::OnceLock;

    static UNITS: OnceLock<(Option<u64>, Option<u64>)> = OnceLock::new();
    let (page_size, ticks_per_second) = *UNITS.get_or_init(|| {
        fn positive(key: libc::c_int) -> Option<u64> {
            // SAFETY: the requested sysconf names are constants with no pointer arguments.
            u64::try_from(unsafe { libc::sysconf(key) })
                .ok()
                .filter(|&value| value != 0)
        }
        (positive(libc::_SC_PAGESIZE), positive(libc::_SC_CLK_TCK))
    });
    if !procfs_uses_our_pid_namespace() {
        return ProcessUsage::default();
    }
    let Ok(file) = std::fs::File::open(format!("/proc/{pid}/stat")) else {
        return ProcessUsage::default();
    };
    let mut stat = Vec::new();
    if file.take(4097).read_to_end(&mut stat).is_err() || stat.len() > 4096 {
        return ProcessUsage::default();
    }
    let Some(fields) = stat.iter().rposition(|&byte| byte == b')') else {
        return ProcessUsage::default();
    };
    let Ok(fields) = std::str::from_utf8(&stat[fields + 1..]) else {
        return ProcessUsage::default();
    };
    // Fields 14, 15 and 24, relative to the state field (3) after the command name.
    let mut fields = fields.split_ascii_whitespace();
    let user = fields.nth(11).and_then(|value| value.parse::<u64>().ok());
    let kernel = fields.next().and_then(|value| value.parse::<u64>().ok());
    let resident_bytes = fields
        .nth(8)
        .and_then(|value| value.parse::<u64>().ok())
        .zip(page_size)
        .and_then(|(pages, size)| pages.checked_mul(size));
    let cpu_time = user
        .zip(kernel)
        .and_then(|(user, kernel)| user.checked_add(kernel))
        .zip(ticks_per_second)
        .map(|(ticks, per_second)| {
            let nanos = u128::from(ticks % per_second) * 1_000_000_000 / u128::from(per_second);
            Duration::new(ticks / per_second, nanos as u32)
        });
    ProcessUsage {
        resident_bytes,
        cpu_time,
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn procfs_uses_our_pid_namespace() -> bool {
    use std::io::{BufRead as _, BufReader, Read as _};

    let Ok(status) = std::fs::File::open("/proc/self/status") else {
        return false;
    };
    let pid = std::process::id();
    // An outer procfs can resolve the same numeric PID to an unrelated process.
    BufReader::new(status.take(16 * 1024))
        .lines()
        .map_while(Result::ok)
        .find_map(|line| {
            let mut ids = line.strip_prefix("NStgid:")?.split_ascii_whitespace();
            Some(ids.next().and_then(|id| id.parse().ok()) == Some(pid) && ids.next().is_none())
        })
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
pub(crate) fn pid_usage(pid: u32) -> ProcessUsage {
    use std::mem::{MaybeUninit, size_of};
    use std::num::NonZeroU64;
    use std::ptr::null_mut;
    use std::sync::OnceLock;

    static TICKS_PER_SECOND: OnceLock<Option<NonZeroU64>> = OnceLock::new();
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return ProcessUsage::default();
    };
    let mut info = MaybeUninit::<libc::proc_taskinfo>::uninit();
    let size = size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: the output has the type and size specified by PROC_PIDTASKINFO.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return ProcessUsage::default();
    }
    // SAFETY: libproc returned the full record size.
    let info = unsafe { info.assume_init() };
    let ticks_per_second = *TICKS_PER_SECOND.get_or_init(|| {
        // PROC_PIDTASKINFO reports kernel clock ticks. Rosetta can present a
        // different timebase to mach_timebase_info in a translated process.
        let mut frequency = 0_u64;
        let mut length = size_of::<u64>();
        // SAFETY: the name is NUL-terminated, the output is a writable 64-bit
        // buffer of the specified length, and there is no input value.
        let status = unsafe {
            libc::sysctlbyname(
                c"hw.tbfrequency".as_ptr(),
                (&raw mut frequency).cast(),
                &mut length,
                null_mut(),
                0,
            )
        };
        if status != 0 || length != size_of::<u64>() {
            return None;
        }
        NonZeroU64::new(frequency)
    });
    let cpu_time = ticks_per_second.and_then(|frequency| {
        let ticks = u128::from(info.pti_total_user) + u128::from(info.pti_total_system);
        let nanos = ticks * 1_000_000_000 / u128::from(frequency.get());
        u64::try_from(nanos).ok().map(Duration::from_nanos)
    });
    ProcessUsage {
        resident_bytes: Some(info.pti_resident_size),
        cpu_time,
    }
}

#[cfg(windows)]
pub(crate) fn handle_usage(handle: std::os::windows::io::RawHandle) -> ProcessUsage {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetProcessTimes;

    let size = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    let mut memory = PROCESS_MEMORY_COUNTERS {
        cb: size,
        ..PROCESS_MEMORY_COUNTERS::default()
    };
    // SAFETY: callers hold the live process handle and memory has the documented size.
    let resident_bytes = (unsafe { K32GetProcessMemoryInfo(handle, &mut memory, size) } != 0)
        .then_some(memory.WorkingSetSize as u64);
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: the process handle remains live and each output is writable.
    let available =
        unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) } != 0;
    let ticks =
        |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    let cpu_time = available.then(|| {
        let total = ticks(user).saturating_add(ticks(kernel));
        Duration::new(total / 10_000_000, (total % 10_000_000) as u32 * 100)
    });
    ProcessUsage {
        resident_bytes,
        cpu_time,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_process_usage_measures_memory_and_cpu() {
        let usage = current_process_usage();
        assert!(usage.resident_bytes.is_some_and(|bytes| bytes > 0));
        assert!(usage.cpu_time.is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_cpu_usage_matches_getrusage() {
        fn reference() -> Duration {
            let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
            // SAFETY: the output buffer has the declared getrusage type and size.
            assert_eq!(
                unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
                0
            );
            // SAFETY: getrusage initialized the result on success.
            let usage = unsafe { usage.assume_init() };
            let time = |value: libc::timeval| {
                Duration::from_secs(value.tv_sec as u64)
                    + Duration::from_micros(value.tv_usec as u64)
            };
            time(usage.ru_utime) + time(usage.ru_stime)
        }

        let sample = || pid_usage(std::process::id()).cpu_time.unwrap();
        let before_low = reference();
        let before = sample();
        let before_high = reference();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let after_low = loop {
            for _ in 0..1024 {
                std::hint::spin_loop();
            }
            let elapsed = reference();
            if elapsed - before_high >= Duration::from_millis(20) {
                break elapsed;
            }
            assert!(std::time::Instant::now() < deadline, "CPU test did not run");
        };
        let after = sample();
        let after_high = reference();

        let measured = after - before;
        let tolerance = Duration::from_millis(2);
        assert!(measured + tolerance >= after_low - before_high);
        assert!(measured <= after_high - before_low + tolerance);
    }
}
