use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use async_channel::Receiver;
use jiff::civil::Date;
use jiff::tz::TimeZone;

use crate::logfile_window::catalog::{self, Format as DocumentFormat};
use crate::logfile_window::document::{Fields, write_entry, write_header};
use crate::sftp_window::{LocalSource, StagedDownload};

use super::capture::{Record, Shutdown};
use super::{Config, Format};

pub(super) fn run(
    profile: &Path,
    config: Config,
    metadata: Fields,
    records: Receiver<Record>,
    shutdown: &Shutdown,
) -> io::Result<()> {
    let Ok(first) = records.recv_blocking() else {
        return Ok(());
    };
    let folder = Folder::open(profile, &config)?;
    let timezone = TimeZone::system();
    let format = match config.format {
        Format::Text => DocumentFormat::Text,
        Format::Xml => DocumentFormat::Xml,
        Format::Json => DocumentFormat::Json,
    };
    let mut writer = LogWriter {
        config,
        metadata,
        format,
        folder,
        open: None,
        day: None,
        base: String::new(),
        part: 0,
        timezone,
        shutdown,
        skipped_compression: false,
    };
    if writer.folder.retention {
        writer
            .folder
            .sweep(first.at.to_zoned(writer.timezone.clone()).date(), || {
                shutdown.started()
            });
    }
    let result = (|| {
        writer.append(first)?;
        while let Ok(record) = records.recv_blocking() {
            writer.append(record)?;
        }
        Ok(())
    })();
    let cleanup = writer.close(result.is_ok());
    combine_errors(result, cleanup)?;
    if writer.skipped_compression {
        return Err(compression_timeout());
    }
    Ok(())
}

struct LogWriter<'a> {
    config: Config,
    metadata: Fields,
    format: DocumentFormat,
    folder: Folder,
    open: Option<Active>,
    day: Option<Date>,
    base: String,
    part: u32,
    timezone: TimeZone,
    shutdown: &'a Shutdown,
    skipped_compression: bool,
}

impl LogWriter<'_> {
    fn append(&mut self, mut record: Record) -> io::Result<()> {
        let at = record.at.to_zoned(self.timezone.clone());
        record.fields.insert(
            "timestamp".into(),
            at.strftime("%Y-%m-%d %H:%M:%S.%3f").to_string(),
        );
        let mut line = Vec::new();
        let count = self.open.as_ref().map_or(0, |open| open.lines);
        write_entry(&mut line, self.format, &record.fields, count)?;
        let rotate_daily =
            self.config.rotate_daily && self.day.is_some_and(|date| at.date() > date);
        let rotate_size = self.open.as_ref().is_some_and(|open| {
            open.bytes.saturating_add(line.len() as u64 + 24)
                > u64::from(self.config.max_mib) * 1024 * 1024
        });
        if rotate_daily || rotate_size {
            self.close(true)?;
            line.clear();
            write_entry(&mut line, self.format, &record.fields, 0)?;
        }
        if self.open.is_none() {
            if self.base.is_empty() || rotate_daily {
                self.base = fresh_base(&at)?;
                self.day = Some(at.date());
                self.part = 1;
            } else {
                self.part = self
                    .part
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("Logfile part limit reached"))?;
            }
            let extension = match self.format {
                DocumentFormat::Text => "log",
                DocumentFormat::Xml => "xml",
                DocumentFormat::Json => "json",
            };
            let part = if self.part <= 1 {
                String::new()
            } else {
                format!(".p{}", self.part)
            };
            let filename = format!("{}{part}.{extension}", self.base);
            self.open = Some(self.folder.begin(&filename, self.format, &self.metadata)?);
            if rotate_daily && self.folder.retention {
                self.folder.sweep(at.date(), || self.shutdown.started());
            }
        }
        let open = self
            .open
            .as_mut()
            .ok_or_else(|| io::Error::other("Missing terminal logfile writer"))?;
        open.writer.write_all(&line)?;
        open.writer.flush()?;
        open.bytes += line.len() as u64;
        open.lines += 1;
        Ok(())
    }

    fn close(&mut self, complete: bool) -> io::Result<()> {
        let Some(Active {
            writer,
            filename,
            marker,
            marker_name,
            lines,
            ..
        }) = self.open.take()
        else {
            return Ok(());
        };
        let result = if complete {
            self.finalize(writer, &filename, lines)
        } else {
            // A previous output error may have left a partial entry. Keep the
            // raw logfile for recovery and release only the owned marker.
            drop(writer);
            Ok(())
        };
        combine_errors(result, self.folder.remove(&marker_name, marker))
    }

    fn finalize(
        &mut self,
        mut writer: BufWriter<File>,
        filename: &str,
        lines: usize,
    ) -> io::Result<()> {
        match self.format {
            DocumentFormat::Text => {}
            DocumentFormat::Xml => writer.write_all(b"</terminal-log>\n")?,
            DocumentFormat::Json => {
                if lines != 0 {
                    writer.write_all(b"\n")?;
                }
                writer.write_all(b"  ]\n}\n")?;
            }
        }
        writer.flush()?;
        writer.get_ref().sync_all()?;
        let file = writer.into_inner().map_err(|error| error.into_error())?;
        if self.config.compress
            && let Err(error) =
                self.folder
                    .compress(filename, file, self.config.retention_days, self.shutdown)
        {
            if is_compression_timeout(&error) {
                self.skipped_compression = true;
            } else {
                return Err(error);
            }
        }
        Ok(())
    }
}

struct Active {
    writer: BufWriter<File>,
    filename: String,
    marker: File,
    marker_name: String,
    bytes: u64,
    lines: usize,
}

struct Folder {
    path: PathBuf,
    retention: bool,
    #[cfg(unix)]
    handle: File,
    #[cfg(windows)]
    handle: kortty_files::tree::Directory,
}

impl Folder {
    fn open(profile: &Path, config: &Config) -> io::Result<Self> {
        if !config.folder.is_empty() {
            let path = config.directory(profile).canonicalize()?;
            let folder = Self::existing(&path)?;
            catalog::add_folder(profile, &path)?;
            return Ok(folder);
        }
        fs::create_dir_all(profile)?;
        let parent = Self::existing(&profile.canonicalize()?)?;
        if let Err(error) = parent.create_folder("terminal-logs")
            && error.kind() != io::ErrorKind::AlreadyExists
        {
            return Err(error);
        }
        let mut folder = parent.open_folder("terminal-logs")?;
        folder.retention = folder.private();
        Ok(folder)
    }

    fn open_folder(&self, name: &str) -> io::Result<Self> {
        #[cfg(unix)]
        let handle = {
            use rustix::fs::{Mode, OFlags, openat};
            File::from(openat(
                &self.handle,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?)
        };
        #[cfg(windows)]
        let handle = {
            let file = self
                .handle
                .read_child(std::ffi::OsStr::new(name))?
                .ok_or_else(|| {
                    io::Error::other("The default logfile directory is a reparse point")
                })?;
            kortty_files::tree::Directory::from_file(file)?
        };
        Ok(Self {
            path: self.path.join(name),
            retention: false,
            handle,
        })
    }

    fn existing(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        let handle = {
            use rustix::fs::{Mode, OFlags, open};
            File::from(open(
                path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?)
        };
        #[cfg(windows)]
        let handle = kortty_files::tree::Directory::open_path(path)?;
        Ok(Self {
            path: path.into(),
            retention: false,
            handle,
        })
    }

    fn private(&self) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if !self.handle.metadata().is_ok_and(|metadata| {
                metadata.mode() & 0o077 == 0
                    && metadata.uid() == rustix::process::geteuid().as_raw()
            }) {
                return false;
            }
            #[cfg(target_os = "macos")]
            return kortty_files::require_no_extended_acl(&self.handle).is_ok();
            #[cfg(not(target_os = "macos"))]
            true
        }
        #[cfg(windows)]
        self.handle.require_owner_only().is_ok()
    }

    fn has_source_parent(&self, source: &LocalSource) -> io::Result<bool> {
        #[cfg(unix)]
        {
            let parent = rustix::fs::fstat(&self.handle)?;
            let actual = rustix::fs::fstat(source.parent_file())?;
            Ok((parent.st_dev, parent.st_ino) == (actual.st_dev, actual.st_ino))
        }
        #[cfg(windows)]
        self.handle.same_as(source.parent_directory())
    }

    fn sweep(&self, now: Date, closing: impl Fn() -> bool) {
        if closing() || !self.private() {
            return;
        }
        let Ok(entries) = fs::read_dir(&self.path) else {
            return;
        };
        for entry in entries.take(4096).flatten() {
            if closing() {
                break;
            }
            let path = entry.path();
            if path.extension().and_then(std::ffi::OsStr::to_str) != Some("gz") {
                continue;
            }
            if let Ok(source) = LocalSource::open(&path)
                && self.has_source_parent(&source).unwrap_or(false)
            {
                let _ = catalog::expire_owned_archive(&path, &source, now);
            }
        }
    }

    fn create_folder(&self, name: &str) -> io::Result<()> {
        #[cfg(unix)]
        return Ok(rustix::fs::mkdirat(
            &self.handle,
            name,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR | rustix::fs::Mode::XUSR,
        )?);
        #[cfg(windows)]
        self.handle
            .create_child(std::ffi::OsStr::new(name))
            .map(|_| ())
    }

    fn create_file(&self, name: &str) -> io::Result<File> {
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags, openat};
            let file = File::from(openat(
                &self.handle,
                name,
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )?);
            #[cfg(target_os = "macos")]
            if let Err(error) = kortty_files::require_no_extended_acl(&file) {
                let _ = self.remove(name, file);
                return Err(error);
            }
            Ok(file)
        }
        #[cfg(windows)]
        self.handle
            .create_read_write_file(std::ffi::OsStr::new(name))
    }

    fn begin(
        &self,
        filename: &str,
        format: DocumentFormat,
        metadata: &Fields,
    ) -> io::Result<Active> {
        let mut header = Vec::new();
        write_header(&mut header, format, metadata)?;
        let marker_name = format!(".{filename}.lock");
        #[cfg(unix)]
        let marker = self.create_file(&marker_name)?;
        #[cfg(windows)]
        let marker = self
            .handle
            .create_shared_lock_file(std::ffi::OsStr::new(&marker_name))?;
        let open = || -> io::Result<_> {
            lock_marker(&marker)?;
            let mut writer = BufWriter::new(self.create_file(filename)?);
            writer.write_all(&header)?;
            writer.flush()?;
            Ok(writer)
        };
        let writer = match open() {
            Ok(writer) => writer,
            Err(error) => return combine_errors(Err(error), self.remove(&marker_name, marker)),
        };
        Ok(Active {
            writer,
            filename: filename.into(),
            marker,
            marker_name,
            bytes: header.len() as u64,
            lines: 0,
        })
    }

    fn compress(
        &self,
        name: &str,
        file: File,
        retention_days: u16,
        shutdown: &Shutdown,
    ) -> io::Result<()> {
        check_compression_budget(shutdown)?;
        let identity = file_identity(&file)?;
        #[cfg(windows)]
        drop(file);
        let path = self.path.join(name);
        let source = LocalSource::open(&path)?;
        if !self.has_source_parent(&source)? {
            return Err(io::Error::other(
                "The logfile folder moved before compression",
            ));
        }
        if file_identity(source.file())? != identity {
            return Err(io::Error::other(
                "Terminal logfile moved before compression",
            ));
        }
        let archive_name = format!("{name}.gz");
        let destination = self.path.join(&archive_name);
        let staging = StagedDownload::create(&destination)?;
        if !staging.shares_parent_with(&source)? {
            return Err(io::Error::other(
                "The logfile folder moved while staging compression",
            ));
        }
        #[cfg(target_os = "macos")]
        staging.require_no_extended_acl()?;
        let mut encoder =
            flate2::write::GzEncoder::new(staging.writer()?, flate2::Compression::default());
        let mut input = CompressInput {
            source: source.file().take(256 * 1024 * 1024 + 1),
            shutdown,
        };
        let copied = io::copy(&mut input, &mut encoder)?;
        if copied != source.size() || copied > 256 * 1024 * 1024 {
            return Err(io::Error::other(
                "Terminal logfile size changed during compression",
            ));
        }
        check_compression_budget(shutdown)?;
        encoder.finish()?;
        let archive_identity = file_identity(staging.writer()?)?;
        source.verify()?;
        staging
            .publish(|| shutdown.compression_expired())
            .map_err(|error| {
                if error.kind() == io::ErrorKind::Interrupted && shutdown.compression_expired() {
                    compression_timeout()
                } else {
                    error
                }
            })?;
        drop(source);
        #[cfg(unix)]
        self.remove(name, file)?;
        #[cfg(windows)]
        {
            let file = self.handle.child_for_removal(std::ffi::OsStr::new(name))?;
            if file_identity(&file)? != identity {
                return Err(io::Error::other("Terminal logfile moved after compression"));
            }
            self.remove(name, file)?;
        }
        if self.retention && retention_days != 0 && self.private() {
            let archive = LocalSource::open(&destination)?;
            if self.has_source_parent(&archive)?
                && file_identity(archive.file())? == archive_identity
            {
                let marker = self.create_file(&catalog::archive_owner_name(&archive_name))?;
                catalog::write_archive_owner(&marker, archive.file(), retention_days)?;
            }
        }
        Ok(())
    }

    fn remove(&self, name: &str, file: File) -> io::Result<()> {
        #[cfg(unix)]
        return catalog::remove_open_capture(&self.handle, &self.path.join(name), &file);
        #[cfg(windows)]
        {
            let _ = name;
            kortty_files::tree::remove(file)
        }
    }
}

struct CompressInput<'a, R> {
    source: R,
    shutdown: &'a Shutdown,
}

impl<R: Read> Read for CompressInput<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        check_compression_budget(self.shutdown)?;
        self.source.read(bytes)
    }
}

fn check_compression_budget(shutdown: &Shutdown) -> io::Result<()> {
    if shutdown.compression_expired() {
        return Err(compression_timeout());
    }
    Ok(())
}

fn combine_errors<T>(result: io::Result<T>, cleanup: io::Result<()>) -> io::Result<T> {
    match (result, cleanup) {
        (result, Ok(())) => result,
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(error), Err(cleanup)) => Err(io::Error::new(
            error.kind(),
            format!("{error}. Logfile cleanup also failed ({cleanup})"),
        )),
    }
}

#[derive(Debug)]
struct CompressionTimeout;

impl std::fmt::Display for CompressionTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Compression exceeded the shutdown budget. The complete plain logfile was kept")
    }
}

impl std::error::Error for CompressionTimeout {}

fn compression_timeout() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, CompressionTimeout)
}

fn is_compression_timeout(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|reason| reason.is::<CompressionTimeout>())
}

fn lock_marker(file: &File) -> io::Result<()> {
    // Java FileChannel uses fcntl on Unix, not flock. Closing another descriptor
    // on this inode in the same process would release the POSIX record lock.
    #[cfg(unix)]
    return Ok(rustix::fs::fcntl_lock(
        file,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )?);
    #[cfg(windows)]
    Ok(file.try_lock()?)
}

#[cfg(unix)]
fn file_identity(file: &File) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
use kortty_files::tree::file_identity;

fn fresh_base(at: &jiff::Zoned) -> io::Result<String> {
    let mut nonce = [0; 4];
    getrandom::fill(&mut nonce).map_err(io::Error::other)?;
    let sequence = u32::from_le_bytes(nonce) % 1_000_000_000 + 1;
    Ok(format!(
        "{}-ssh_{sequence}",
        at.strftime("%Y-%m-%d-%H-%M-%S")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippets::tests::Profile;

    #[test]
    fn native_lock_spans_capture_and_a_marker_collision_prevents_writing() {
        const MARKER: &str = "KORTTY_TEST_LIVE_LOG_MARKER";
        const LOCKED: &str = "KORTTY_TEST_LIVE_LOG_LOCKED";
        if let Some(marker) = std::env::var_os(MARKER) {
            let marker = File::options().read(true).write(true).open(marker).unwrap();
            match std::env::var_os(LOCKED)
                .as_deref()
                .and_then(std::ffi::OsStr::to_str)
            {
                Some("yes") => {
                    let error = lock_marker(&marker).unwrap_err();
                    assert!(
                        matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::PermissionDenied
                        ),
                        "{error}"
                    );
                }
                Some("no") => lock_marker(&marker).unwrap(),
                _ => panic!("missing test probe mode"),
            }
            return;
        }
        let profile = Profile::new();
        let folder = Folder::open(&profile.0.canonicalize().unwrap(), &Config::default()).unwrap();
        let name = "2020-01-01-00-00-00-ssh_5.log";
        let marker_name = format!(".{name}.lock");
        let marker_path = folder.path.join(&marker_name);
        let marker = folder.create_file(&marker_name).unwrap();
        assert!(
            folder
                .begin(name, DocumentFormat::Text, &Fields::new())
                .is_err()
        );
        assert!(!folder.path.join(name).exists());
        folder.remove(&marker_name, marker).unwrap();
        fs::write(folder.path.join(name), b"other logfile").unwrap();
        assert!(
            folder
                .begin(name, DocumentFormat::Text, &Fields::new())
                .is_err()
        );
        assert!(!marker_path.exists());
        assert_eq!(fs::read(folder.path.join(name)).unwrap(), b"other logfile");
        fs::remove_file(folder.path.join(name)).unwrap();

        let active = folder
            .begin(name, DocumentFormat::Text, &Fields::new())
            .unwrap();
        let source = LocalSource::open(&folder.path.join(name)).unwrap();
        assert!(source.sibling_exists_or_uncertain(std::ffi::OsStr::new(&marker_name)));
        let probe = |locked| {
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "terminal_logging::writer::tests::native_lock_spans_capture_and_a_marker_collision_prevents_writing"])
                .env(MARKER, &marker_path).env(LOCKED, locked)
                .output().unwrap();
            assert!(
                child.status.success(),
                "{}",
                String::from_utf8_lossy(&child.stdout)
            );
        };
        probe("yes");
        drop(active);
        probe("no");
    }

    #[test]
    fn writer_error_releases_its_marker_and_keeps_the_partial_log() {
        let profile = Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let (send, records) = async_channel::bounded(2);
        for line in ["captured", "\0"] {
            send.try_send(Record {
                at: jiff::Timestamp::now(),
                fields: Fields::from([("line".into(), line.into())]),
            })
            .unwrap();
        }
        send.close();
        let config = Config {
            format: Format::Xml,
            ..Config::default()
        };
        assert!(run(&root, config, Fields::new(), records, &Shutdown::default()).is_err());
        let files: Vec<_> = fs::read_dir(root.join("terminal-logs"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(files.len(), 1, "{files:?}");
        assert_eq!(files[0].extension(), Some(std::ffi::OsStr::new("xml")));
        assert!(fs::read_to_string(&files[0]).unwrap().contains("captured"));
    }

    #[test]
    fn compression_deadline_still_drains_all_rotated_records_and_closes_plain_logs() {
        let profile = Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let (send, records) = async_channel::bounded(2);
        for (at, line) in [
            ("2025-01-02T10:00:00Z", "earlier"),
            ("2025-01-04T10:00:00Z", "later"),
        ] {
            send.try_send(Record {
                at: at.parse().unwrap(),
                fields: Fields::from([("line".into(), line.into())]),
            })
            .unwrap();
        }
        send.close();
        let shutdown = Shutdown::default();
        shutdown.expire_compression();
        let config = Config {
            format: Format::Json,
            ..Config::default()
        };
        let result = run(&root, config, Fields::new(), records, &shutdown);
        assert!(is_compression_timeout(&result.unwrap_err()));
        let mut lines = Vec::new();
        for entry in fs::read_dir(root.join("terminal-logs")).unwrap() {
            let path = entry.unwrap().path();
            assert_eq!(path.extension(), Some(std::ffi::OsStr::new("json")));
            let log: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            lines.push(log["entries"][0]["line"].as_str().unwrap().to_owned());
        }
        lines.sort_unstable();
        assert_eq!(lines, ["earlier", "later"]);
    }

    #[test]
    fn retention_checks_owned_identity_and_hold_in_the_private_default_folder() {
        let profile = Profile::new();
        let folder = Folder::open(&profile.0.canonicalize().unwrap(), &Config::default()).unwrap();
        assert!(folder.retention);
        let owned = "2020-01-01-00-00-00-ssh_1.log.gz";
        let unrelated = "2020-01-01-00-00-00-ssh_2.log.gz";
        let held = "2020-01-01-00-00-00-ssh_3.log.gz";
        let replaced = "2020-01-01-00-00-00-ssh_4.log.gz";
        for name in [owned, unrelated, held, replaced] {
            let mut file = folder.create_file(name).unwrap();
            file.write_all(b"old archive").unwrap();
            file.sync_all().unwrap();
            drop(file);
            if name != unrelated {
                let file = LocalSource::open(&folder.path.join(name)).unwrap();
                let marker = folder
                    .create_file(&catalog::archive_owner_name(name))
                    .unwrap();
                catalog::write_archive_owner(&marker, file.file(), 30).unwrap();
            }
        }
        fs::write(folder.path.join(".2020-01-01-00-00-00-ssh_3.log.badges.json"),
            br##"[{"id":"hold","name":"Hold","icon":"security","color":"#c08020","style":"SOFT"}]"##).unwrap();
        fs::rename(folder.path.join(replaced), folder.path.join("original")).unwrap();
        fs::write(folder.path.join(replaced), b"replacement").unwrap();

        folder.sweep("2020-01-31".parse().unwrap(), || false);
        assert!(folder.path.join(owned).is_file());
        folder.sweep("2020-02-01".parse().unwrap(), || false);
        assert!(!folder.path.join(owned).exists());
        assert!(folder.path.join(unrelated).is_file());
        assert!(folder.path.join(held).is_file());
        assert_eq!(
            fs::read(folder.path.join(replaced)).unwrap(),
            b"replacement"
        );
    }
}
