use std::fs::File;
use std::os::fd::AsRawFd as _;

use crate::DirectoryNameRules;

/// Returns known name lookup rules for the held directory's filesystem.
pub fn directory_name_rules(directory: &File) -> Option<DirectoryNameRules> {
    // SAFETY: the open descriptor and writable statfs buffer are live for the call.
    let fs = unsafe {
        let mut fs = std::mem::zeroed::<libc::statfs>();
        if libc::fstatfs(directory.as_raw_fd(), &mut fs) != 0 {
            return None;
        }
        fs
    };
    #[cfg(target_os = "linux")]
    {
        let kind = fs.f_type as u32;
        match kind {
            0x5846_5342 => Some(DirectoryNameRules::Folded), // XFS can use case-insensitive lookup.
            0xef53 | 0xf2f5_2010 | 0x0102_1994 | 0x9123_683e => {
                // ext, F2FS and tmpfs use a per-directory casefold flag. Btrfs also reports inode flags.
                const FS_CASEFOLD_FL: libc::c_int = 0x4000_0000;
                let mut flags = 0 as libc::c_int;
                // SAFETY: the descriptor and flags buffer are live for the ioctl.
                if unsafe {
                    libc::ioctl(directory.as_raw_fd(), libc::FS_IOC_GETFLAGS, &raw mut flags)
                } == 0
                {
                    Some(if flags & FS_CASEFOLD_FL == 0 {
                        DirectoryNameRules::Literal
                    } else {
                        DirectoryNameRules::Folded
                    })
                } else if kind == 0x0102_1994
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOTTY)
                {
                    // Older tmpfs versions have bytewise lookup and no inode-flag ioctl.
                    Some(DirectoryNameRules::Literal)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
    #[cfg(target_os = "macos")]
    {
        let kind = fs.f_fstypename.map(|byte| byte as u8);
        let kind = std::ffi::CStr::from_bytes_until_nul(&kind).ok()?;
        if !matches!(kind.to_bytes(), b"apfs" | b"hfs") {
            return None;
        }
        // SAFETY: fpathconf only reads the options of the live directory descriptor.
        match unsafe { libc::fpathconf(directory.as_raw_fd(), libc::_PC_CASE_SENSITIVE) } {
            0 => Some(DirectoryNameRules::Folded),
            1 => Some(DirectoryNameRules::Normalized),
            _ => None,
        }
    }
}
