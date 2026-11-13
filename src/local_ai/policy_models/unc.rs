//! Read-only acquisition from an explicitly configured Windows SMB path.

#[cfg(any(windows, test))]
use std::io::Read;
use std::io::{self, Write};
use std::sync::atomic::AtomicBool;
#[cfg(any(windows, test))]
use std::sync::atomic::Ordering;
use std::time::Instant;

#[cfg(any(windows, test))]
use super::super::managed_models::MAX_MODEL_BYTES;

/// Validates the ordinary UNC spelling before Windows normalizes the pathname.
pub(super) fn validate(source: &str) -> io::Result<()> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            r"A UNC model source requires an ordinary \\server\share\file path",
        )
    };
    if source.len() > 4096 {
        return Err(invalid());
    }
    let mut parts = source.strip_prefix(r"\\").ok_or_else(invalid)?.split('\\');
    let server = parts.next().ok_or_else(invalid)?;
    let share = parts.next().ok_or_else(invalid)?;
    let host = server.strip_suffix('.').unwrap_or(server);
    if host.len() > 253
        || !host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .chars()
                    .all(|ch| ch.is_alphanumeric() || matches!(ch, '-' | '_'))
        })
        || !ordinary_name(share)
        || ["DavWWWRoot", "IPC$", "pipe", "mailslot"]
            .iter()
            .any(|reserved| share.eq_ignore_ascii_case(reserved))
        || !parts.next().is_some_and(ordinary_name)
        || !parts.all(ordinary_name)
    {
        return Err(invalid());
    }
    Ok(())
}

fn ordinary_name(name: &str) -> bool {
    !name.is_empty()
        && !name.ends_with([' ', '.'])
        && name.encode_utf16().count() <= 255
        && !name.chars().any(|ch| {
            ch.is_control() || matches!(ch, '/' | ':' | '"' | '<' | '>' | '|' | '?' | '*')
        })
}

/// Reads an exact bounded SMB file into staging. The caller verifies the declared digest.
/// A synchronous Windows SMB call can outlast the application deadline.
pub(super) fn receive(
    source: &str,
    destination: &mut impl Write,
    cancelled: &AtomicBool,
    deadline: Instant,
    require_current: &dyn Fn() -> io::Result<()>,
) -> io::Result<u64> {
    validate(source)?;
    #[cfg(windows)]
    {
        use std::path::PathBuf;

        let path = PathBuf::from(format!(r"\\?\UNC\{}", &source[2..]));
        let check = || check_transfer(cancelled, deadline, require_current);
        let lease = kortty_files::tree::SmbReadLease::open(&path, check)?;
        let file = lease.file();
        let length = file.metadata()?.len();
        copy_exact(file, length, destination, check)
    }
    #[cfg(not(windows))]
    {
        let _ = (destination, cancelled, deadline, require_current);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "UNC model sources require the Windows SMB redirector",
        ))
    }
}

#[cfg(any(windows, test))]
fn check_transfer(
    cancelled: &AtomicBool,
    deadline: Instant,
    require_current: &dyn Fn() -> io::Result<()>,
) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "The UNC model transfer was cancelled",
        ));
    }
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "The UNC model transfer exceeded its time limit",
        ));
    }
    require_current()
}

#[cfg(any(windows, test))]
fn copy_exact(
    mut source: impl Read,
    length: u64,
    destination: &mut impl Write,
    check: impl Fn() -> io::Result<()>,
) -> io::Result<u64> {
    check()?;
    if !(1..=MAX_MODEL_BYTES).contains(&length) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "The UNC model file size is outside the supported limit",
        ));
    }
    let mut remaining = length;
    let mut buffer = vec![0; 1024 * 1024];
    while remaining != 0 {
        let limit = remaining.min(buffer.len() as u64) as usize;
        let count = source.read(&mut buffer[..limit])?;
        check()?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "The UNC model file ended before its declared size",
            ));
        }
        destination.write_all(&buffer[..count])?;
        remaining -= count as u64;
    }
    check()?;
    let extra = source.read(&mut buffer[..1])?;
    check()?;
    if extra != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "The UNC model file grew while being read",
        ));
    }
    Ok(length)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::time::Duration;

    use super::*;

    #[test]
    fn policy_unc_source_requires_an_ordinary_explicit_share_path() {
        for source in [
            r"\\models\release\model.gguf",
            r"\\192.0.2.15\Model Files\Stable\Mödell.gguf",
            r"\\server.example.\models$\fixture.gguf",
        ] {
            validate(source).unwrap();
        }
        for source in [
            r"\models\release\model.gguf",
            r"\\models\release",
            r"\\?\UNC\models\release\model.gguf",
            r"\\.\GLOBALROOT\model.gguf",
            r"\\models@SSL\release\model.gguf",
            r"\\models\dAvWwWrOoT\model.gguf",
            r"\\models\IPC$\model.gguf",
            r"\\models\pipe\model.gguf",
            r"\\models\release\..\model.gguf",
            r"\\models\release\.\model.gguf",
            r"\\models\release\\model.gguf",
            r"\\models\release\model.gguf:alternate",
            r"\\models\release\model.gguf.",
            r"\\models\release\model.gguf ",
            r"\\models\release\nested/model.gguf",
            r"\\models\release\model.gguf\",
        ] {
            assert_eq!(
                validate(source).unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{source}"
            );
        }
    }

    #[test]
    fn policy_unc_source_copy_checks_limits_lengths_and_current_policy() {
        let mut bytes = Vec::new();
        let ready = || Ok(());
        assert_eq!(copy_exact(&b"GGUF"[..], 4, &mut bytes, ready).unwrap(), 4);
        assert_eq!(bytes, b"GGUF");
        assert_eq!(
            copy_exact(&b""[..], MAX_MODEL_BYTES + 1, &mut Vec::new(), ready)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            copy_exact(&b"GGUF"[..], 5, &mut Vec::new(), ready)
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(
            copy_exact(&b"GGUF"[..], 3, &mut Vec::new(), ready)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );

        let checks = Cell::new(0);
        let revoked = || {
            checks.set(checks.get() + 1);
            if checks.get() == 1 {
                Ok(())
            } else {
                Err(io::ErrorKind::PermissionDenied.into())
            }
        };
        bytes.clear();
        assert_eq!(
            copy_exact(&b"GGUF"[..], 4, &mut bytes, revoked)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(bytes.is_empty());
        assert_eq!(
            check_transfer(
                &AtomicBool::new(true),
                Instant::now() + Duration::from_secs(60),
                &ready
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(
            check_transfer(&AtomicBool::new(false), Instant::now(), &ready)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }
}
