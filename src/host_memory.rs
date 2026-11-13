//! Physical memory visible to the host operating system.

#[cfg(windows)]
pub(crate) fn physical_bytes() -> Option<u64> {
    kortty_windowing::physical_memory_bytes()
}

#[cfg(target_os = "macos")]
pub(crate) fn physical_bytes() -> Option<u64> {
    kortty_macos::physical_memory_bytes()
}

#[cfg(target_os = "linux")]
pub(crate) fn physical_bytes() -> Option<u64> {
    use std::io::Read as _;

    let mut info = String::new();
    std::fs::File::open("/proc/meminfo")
        .ok()?
        .take(4096)
        .read_to_string(&mut info)
        .ok()?;
    parse_meminfo(&info)
}

#[cfg(any(target_os = "linux", test))]
fn parse_meminfo(info: &str) -> Option<u64> {
    let entry = info
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))?;
    let mut fields = entry.split_ascii_whitespace();
    let kilobytes = fields.next()?.parse::<u64>().ok()?;
    if kilobytes == 0 || fields.next()? != "kB" || fields.next().is_some() {
        return None;
    }
    kilobytes.checked_mul(1024)
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub(crate) fn physical_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_memory_uses_only_checked_physical_kilobytes() {
        assert_eq!(
            parse_meminfo("MemFree: 10 kB\nMemTotal: 8388608 kB\n"),
            Some(8 << 30)
        );
        for bad in [
            "MemFree: 4096 kB",
            "MemTotal: 0 kB",
            "MemTotal: 12 GB",
            "MemTotal: 10 kB extra",
            "MemTotal: 18446744073709551615 kB",
        ] {
            assert_eq!(parse_meminfo(bad), None);
        }
    }

    #[test]
    #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
    fn desktop_os_reports_physical_memory() {
        assert!(physical_bytes().is_some_and(|bytes| bytes >= 1 << 20));
    }
}
