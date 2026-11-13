//! Kernel-enforced write exclusion for Linux policy model weights.

use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use kortty_files::linux_verity::{self, has_verity};
use rustix::fs::{MemfdFlags, OFlags, SealFlags, fcntl_add_seals, fcntl_get_seals, fcntl_getfl};
use rustix::io::Errno;

const MAX_MEMFD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TOTAL_MEMFD_BYTES: u64 = 128 * 1024 * 1024;
const COPY_CHUNK: usize = 1024 * 1024;
const REQUIRED_SEALS: SealFlags = SealFlags::WRITE
    .union(SealFlags::GROW)
    .union(SealFlags::SHRINK)
    .union(SealFlags::SEAL);
static MEMFD_BYTES: AtomicU64 = AtomicU64::new(0);

pub(in crate::local_ai) struct Protected {
    file: File,
    memory: Option<Reservation>,
}

impl Protected {
    pub(in crate::local_ai) fn file(&self) -> &File {
        &self.file
    }

    pub(in crate::local_ai) fn is_memory_backed(&self) -> bool {
        self.memory.is_some()
    }
}

/// Acquire kernel write exclusion before the caller hashes the runtime input.
pub(in crate::local_ai) fn protect(
    file: &File,
    bytes: u64,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> io::Result<Protected> {
    active(cancelled, deadline)?;
    let metadata = file.metadata()?;
    let flags = fcntl_getfl(file)?;
    if !metadata.is_file()
        || metadata.len() != bytes
        || bytes == 0
        || flags.intersects(OFlags::WRONLY | OFlags::RDWR | OFlags::PATH)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected the exact read-only policy model file",
        ));
    }

    match enable_verity(file, cancelled, deadline) {
        Ok(()) => {
            // The kernel can spend time building the Merkle tree. A caller
            // that cancelled during the ioctl must never receive the file.
            active(cancelled, deadline)?;
            if file.metadata()?.len() != bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "The policy model size changed before write protection",
                ));
            }
            Ok(Protected {
                file: file.try_clone()?,
                memory: None,
            })
        }
        Err(error) if verity_unavailable(&error) => sealed_copy(file, bytes, cancelled, deadline),
        Err(error) => Err(error),
    }
}

fn enable_verity(file: &File, cancelled: &AtomicBool, deadline: Instant) -> io::Result<()> {
    if has_verity(file)? {
        return Ok(());
    }
    let page = u32::try_from(rustix::param::page_size()).map_err(io::Error::other)?;
    let block = u32::try_from(rustix::fs::fstatfs(file)?.f_bsize).map_err(io::Error::other)?;
    let block_size = page.min(block);
    if block_size < 1024 || !block_size.is_power_of_two() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "The filesystem reported an invalid fs-verity block size",
        ));
    }
    loop {
        active(cancelled, deadline)?;
        match linux_verity::enable_verity(file, block_size) {
            Ok(()) | Err(Errno::EXIST) => break,
            Err(Errno::BUSY) => {
                if has_verity(file)? {
                    return Ok(());
                }
                std::thread::sleep(
                    Duration::from_millis(50)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            Err(error) => return Err(error.into()),
        }
    }
    if !has_verity(file)? {
        return Err(io::Error::other(
            "The kernel did not confirm fs-verity on the policy model",
        ));
    }
    Ok(())
}

fn verity_unavailable(error: &io::Error) -> bool {
    [Errno::NOTTY, Errno::OPNOTSUPP, Errno::NOSYS]
        .into_iter()
        .any(|code| error.raw_os_error() == Some(code.raw_os_error()))
}

fn sealed_copy(
    source: &File,
    bytes: u64,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> io::Result<Protected> {
    active(cancelled, deadline)?;
    let reservation = Reservation::acquire(bytes)?;
    let flags = MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING;
    let fd = match rustix::fs::memfd_create("kortty-verified-gguf", flags | MemfdFlags::NOEXEC_SEAL)
    {
        // Older kernels do not recognize the non-executable seal flag.
        Err(Errno::INVAL) => rustix::fs::memfd_create("kortty-verified-gguf", flags),
        result => result,
    }?;
    let mut copy = File::from(fd);
    let mut buffer = vec![0; COPY_CHUNK];
    let mut offset = 0;
    while offset < bytes {
        active(cancelled, deadline)?;
        let limit = (bytes - offset).min(buffer.len() as u64) as usize;
        let count = match source.read_at(&mut buffer[..limit], offset) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "The policy model became shorter during protection",
            ));
        }
        copy.write_all(&buffer[..count])?;
        offset += count as u64;
    }
    if source.metadata()?.len() != bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "The policy model size changed during protection",
        ));
    }
    fcntl_add_seals(&copy, REQUIRED_SEALS)?;
    if !fcntl_get_seals(&copy)?.contains(REQUIRED_SEALS) || copy.metadata()?.len() != bytes {
        return Err(io::Error::other(
            "The kernel did not confirm the sealed policy model",
        ));
    }
    copy.seek(SeekFrom::Start(0))?;
    active(cancelled, deadline)?;
    Ok(Protected {
        file: copy,
        memory: Some(reservation),
    })
}

struct Reservation(u64);

impl Reservation {
    fn acquire(bytes: u64) -> io::Result<Self> {
        if bytes > MAX_MEMFD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "This policy model exceeds the 64 MiB fallback limit and requires an fs-verity cache filesystem",
            ));
        }
        MEMFD_BYTES
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(bytes)
                    .filter(|total| *total <= MAX_TOTAL_MEMFD_BYTES)
            })
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "The sealed policy model memory budget is full",
                )
            })?;
        Ok(Self(bytes))
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        MEMFD_BYTES.fetch_sub(self.0, Ordering::AcqRel);
    }
}

fn active(cancelled: &AtomicBool, deadline: Instant) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Policy model protection was cancelled",
        ))
    } else if Instant::now() >= deadline {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Policy model protection exceeded its time limit",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;
    use std::time::Duration;

    use super::*;

    #[test]
    fn policy_model_linux_memfd_keeps_exact_bytes_and_rejects_writes() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("weights.gguf");
        let original = b"reviewed weights";
        std::fs::write(&path, original).unwrap();
        let source = File::open(&path).unwrap();
        let protected = sealed_copy(
            &source,
            original.len() as u64,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(3),
        )
        .unwrap();

        assert!(
            fcntl_get_seals(protected.file())
                .unwrap()
                .contains(REQUIRED_SEALS)
        );
        assert!(protected.file().set_len(1).is_err());
        let mut reader = protected.file().try_clone().unwrap();
        assert!(reader.write_all(b"tampered").is_err());
        std::fs::write(&path, b"different bytes!").unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, original);
    }

    #[test]
    fn policy_model_linux_memfd_checks_size_and_cancellation_before_copy() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("weights.gguf");
        std::fs::write(&path, b"model").unwrap();
        let source = File::open(&path).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let error = sealed_copy(
            &source,
            MAX_MEMFD_BYTES + 1,
            &AtomicBool::new(false),
            deadline,
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        let error = protect(&source, 5, &AtomicBool::new(true), deadline)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        let error = protect(&source, 5, &AtomicBool::new(false), Instant::now())
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    #[ignore = "requires TMPDIR on a filesystem that can enable Linux fs-verity"]
    fn policy_model_linux_verity_keeps_the_exact_inode_write_protected() {
        use std::os::unix::fs::MetadataExt as _;
        use std::process::{Command, Stdio};

        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("weights.gguf");
        std::fs::write(&path, b"model").unwrap();
        let source = File::open(&path).unwrap();
        let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        let error = protect(
            &source,
            5,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(5),
        )
        .err()
        .unwrap();
        assert_eq!(error.raw_os_error(), Some(Errno::TXTBSY.raw_os_error()));
        drop(writer);
        let protected = protect(
            &source,
            5,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        assert!(has_verity(protected.file()).unwrap());
        assert_eq!(
            protected.file().metadata().unwrap().ino(),
            source.metadata().unwrap().ino()
        );
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        assert!(has_verity(&File::open(&path).unwrap()).unwrap());
        std::fs::rename(&path, profile.0.join("protected-original.gguf")).unwrap();
        std::fs::write(&path, b"other").unwrap();
        let child = Command::new("cat")
            .arg("/proc/self/fd/0")
            .stdin(Stdio::from(protected.file().try_clone().unwrap()))
            .output()
            .unwrap();
        assert!(child.status.success());
        assert_eq!(child.stdout, b"model");
        assert!(!has_verity(&File::open(path).unwrap()).unwrap());
    }
}
