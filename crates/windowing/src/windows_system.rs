use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

/// Returns total physical memory exposed to Windows in bytes.
pub fn physical_memory_bytes() -> Option<u64> {
    let mut status = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: The struct is writable and dwLength specifies the Win32 ABI size.
    let result = unsafe { GlobalMemoryStatusEx(&mut status) };
    (result != 0 && status.ullTotalPhys != 0).then_some(status.ullTotalPhys)
}
