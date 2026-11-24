use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread;

use async_channel::Receiver;
use jiff::civil::Date;
use jiff::tz::TimeZone;

use crate::logfile_window::catalog::{self, Format as DocumentFormat};
use crate::logfile_window::document::{Fields, write_entry, write_header};
use crate::sftp_window::{LocalSource, StagedDownload};

use super::capture::{Record, Shutdown};
use super::{Config, Format, MAX_LOG_BYTES};

const COMPRESSION_QUEUE: usize = 2;
const MAX_SEQUENCE: u16 = 999;

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
    thread::scope(|threads| {
        let folder = &folder;
        let (sender, archive) = if config.compress {
            let (send, pending) = mpsc::sync_channel(COMPRESSION_QUEUE);
            let days = config.retention_days;
            let worker = thread::Builder::new()
                .name("kortty-logfile-compress".into())
                .spawn_scoped(threads, move || {
                    let mut failure = None;
                    for record in pending {
                        if let Err(error) = folder.compress_closed(record, days, shutdown) {
                            failure.get_or_insert(error);
                        }
                    }
                    failure.map_or(Ok(()), Err)
                })?;
            (Some(send), Some(worker))
        } else {
            (None, None)
        };
        let mut writer = LogWriter {
            config,
            slug: super::slug(metadata.get("connection").map_or("", String::as_str)),
            metadata,
            format,
            folder,
            open: None,
            day: None,
            sequence: 1,
            part: 0,
            timezone,
            shutdown,
            compression: sender.as_ref(),
            compression_backlog: false,
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
        let backlog = writer.compression_backlog;
        drop(sender);
        let archive_result = archive.map_or(Ok(()), |worker| {
            worker.join().unwrap_or_else(|_| {
                Err(io::Error::other(
                    "The logfile compression worker stopped unexpectedly",
                ))
            })
        });
        combine_errors(combine_errors(result, cleanup), archive_result)?;
        if backlog {
            return Err(io::Error::other(
                "Compression could not keep up. The complete plain logfiles were kept.",
            ));
        }
        Ok(())
    })
}

struct LogWriter<'a> {
    config: Config,
    metadata: Fields,
    format: DocumentFormat,
    folder: &'a Folder,
    open: Option<Active>,
    day: Option<Date>,
    slug: String,
    sequence: u16,
    part: u32,
    timezone: TimeZone,
    shutdown: &'a Shutdown,
    compression: Option<&'a SyncSender<ClosedLog>>,
    compression_backlog: bool,
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
            open.bytes
                .saturating_add((line.len() + footer(self.format, true).len()) as u64)
                > u64::from(self.config.max_mib) * 1024 * 1024
        });
        if rotate_daily || rotate_size {
            self.close(true)?;
            line.clear();
            write_entry(&mut line, self.format, &record.fields, 0)?;
        }
        if self.open.is_none() {
            if self.part == 0 || rotate_daily {
                self.day = Some(at.date());
                self.part = 1;
            } else {
                self.part = self
                    .part
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("Logfile part limit reached"))?;
            }
            self.open = Some(self.open_part(at.datetime())?);
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

    fn open_part(&mut self, stamp: jiff::civil::DateTime) -> io::Result<Active> {
        for sequence in self.sequence..=MAX_SEQUENCE {
            let filename =
                super::filename(&self.slug, stamp, self.part, sequence, self.config.format);
            if let Some(active) = self.folder.begin(&filename, self.format, &self.metadata)? {
                self.sequence = sequence;
                return Ok(active);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "No free terminal logfile name in the session sequence range",
        ))
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
        if !complete {
            // A previous output error may have left a partial entry. Keep the
            // raw logfile for recovery and release only the owned marker.
            drop(writer);
            return self.folder.remove(&marker_name, marker);
        }
        let finish = |mut writer: BufWriter<File>| {
            writer.write_all(footer(self.format, lines != 0))?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
            writer.into_inner().map_err(|error| error.into_error())
        };
        match finish(writer) {
            Ok(file) => {
                if let Some(compression) = self.compression {
                    let job = ClosedLog {
                        filename,
                        file,
                        marker,
                        marker_name,
                    };
                    match compression.try_send(job) {
                        Ok(()) => Ok(()),
                        Err(TrySendError::Full(job) | TrySendError::Disconnected(job)) => {
                            self.compression_backlog = true;
                            self.folder.remove(&job.marker_name, job.marker)
                        }
                    }
                } else {
                    self.folder.remove(&marker_name, marker)
                }
            }
            Err(error) => combine_errors(Err(error), self.folder.remove(&marker_name, marker)),
        }
    }
}

fn footer(format: DocumentFormat, has_entries: bool) -> &'static [u8] {
    match format {
        DocumentFormat::Text => b"",
        DocumentFormat::Xml => b"</terminal-log>\n",
        DocumentFormat::Json if has_entries => b"\n  ]\n}\n",
        DocumentFormat::Json => b"  ]\n}\n",
    }
}

struct ClosedLog {
    filename: String,
    file: File,
    marker: File,
    marker_name: String,
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
            catalog::remember_logger_folder(profile, &path)?;
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
            let private = self.handle.metadata().is_ok_and(|metadata| {
                metadata.mode() & 0o077 == 0
                    && metadata.uid() == rustix::process::geteuid().as_raw()
            });
            #[cfg(target_os = "macos")]
            let private = private && kortty_files::require_no_extended_acl(&self.handle).is_ok();
            private
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

    fn name_exists(&self, name: &str) -> io::Result<bool> {
        #[cfg(unix)]
        let entry: io::Result<()> =
            rustix::fs::statat(&self.handle, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
                .map(|_| ())
                .map_err(Into::into);
        #[cfg(windows)]
        let entry = self
            .handle
            .open_entry_metadata(std::ffi::OsStr::new(name))
            .map(|_| ());
        match entry {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn begin(
        &self,
        filename: &str,
        format: DocumentFormat,
        metadata: &Fields,
    ) -> io::Result<Option<Active>> {
        let marker_name = format!(".{filename}.lock");
        let archive_name = super::compressed_filename(filename);
        if self.name_exists(&marker_name)? || self.name_exists(&archive_name)? {
            return Ok(None);
        }
        let mut header = Vec::new();
        write_header(&mut header, format, metadata)?;
        // Java also claims the plain name first. The sidecar must only be
        // created once this writer owns that name.
        let file = match self.create_file(filename) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(None),
            Err(error) => return Err(error),
        };
        #[cfg(unix)]
        let marker = self.create_file(&marker_name);
        #[cfg(windows)]
        let marker = self
            .handle
            .create_shared_lock_file(std::ffi::OsStr::new(&marker_name));
        let marker = match marker {
            Ok(marker) => marker,
            Err(error) => {
                let result = if error.kind() == io::ErrorKind::AlreadyExists {
                    Ok(None)
                } else {
                    Err(error)
                };
                return combine_errors(result, self.remove(filename, file));
            }
        };
        // A previous writer can publish its archive and release the plain name
        // between the first check and exclusive creation of the plain file.
        let archived = lock_marker(&marker).and_then(|()| self.name_exists(&archive_name));
        if !matches!(archived, Ok(false)) {
            let cleanup = combine_errors(
                self.remove(filename, file),
                self.remove(&marker_name, marker),
            );
            return combine_errors(archived.map(|_| None), cleanup);
        }
        let mut writer = BufWriter::new(file);
        if let Err(error) = writer.write_all(&header).and_then(|()| writer.flush()) {
            drop(writer);
            return combine_errors(Err(error), self.remove(&marker_name, marker));
        }
        Ok(Some(Active {
            writer,
            filename: filename.into(),
            marker,
            marker_name,
            bytes: header.len() as u64,
            lines: 0,
        }))
    }

    fn compress_closed(
        &self,
        record: ClosedLog,
        retention_days: u16,
        shutdown: &Shutdown,
    ) -> io::Result<()> {
        let result = self.compress(&record.filename, record.file, retention_days, &|| {
            shutdown.compression_expired()
        });
        combine_errors(result, self.remove(&record.marker_name, record.marker))
    }

    fn compress(
        &self,
        name: &str,
        file: File,
        retention_days: u16,
        cancelled: &impl Fn() -> bool,
    ) -> io::Result<()> {
        check_compression_budget(cancelled)?;
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
        if source.size() > MAX_LOG_BYTES {
            return Err(io::Error::other(
                "Terminal logfile exceeds the compression limit",
            ));
        }
        let archive_name = super::compressed_filename(name);
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
            flate2::write::GzEncoder::new(staging.writer()?, flate2::Compression::fast());
        let mut input = CompressInput {
            source: source.file().take(MAX_LOG_BYTES + 1),
            cancelled,
        };
        let copied = io::copy(&mut input, &mut encoder)?;
        if copied != source.size() || copied > MAX_LOG_BYTES {
            return Err(io::Error::other(
                "Terminal logfile size changed during compression",
            ));
        }
        check_compression_budget(cancelled)?;
        encoder.finish()?;
        let archive_identity = file_identity(staging.writer()?)?;
        source.verify()?;
        staging.publish(cancelled).map_err(|error| {
            if error.kind() == io::ErrorKind::Interrupted && cancelled() {
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

struct CompressInput<'a, R, C> {
    source: R,
    cancelled: &'a C,
}

impl<R: Read, C: Fn() -> bool> Read for CompressInput<'_, R, C> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        check_compression_budget(self.cancelled)?;
        self.source.read(bytes)
    }
}

fn check_compression_budget(cancelled: &impl Fn() -> bool) -> io::Result<()> {
    if cancelled() {
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

#[cfg(test)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippets::tests::Profile;

    #[test]
    fn concurrent_sessions_skip_an_existing_archive_and_claim_distinct_sequences() {
        let profile = Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let folder = Folder::open(&root, &Config::default()).unwrap();
        let at: jiff::Timestamp = "2025-01-02T10:00:00Z".parse().unwrap();
        let stamp = at
            .to_zoned(TimeZone::system())
            .strftime("%Y-%m-%d-%H-%M-%S")
            .to_string();
        let archived = folder.path.join(format!("{stamp}-Edge_west_2_1.log.gz"));
        fs::write(&archived, b"existing archive").unwrap();
        let simultaneous = std::sync::Barrier::new(3);
        thread::scope(|threads| {
            for line in ["session alpha", "session beta", "session gamma"] {
                let (send, records) = async_channel::bounded(1);
                send.send_blocking(Record {
                    at,
                    fields: Fields::from([("line".into(), line.into())]),
                })
                .unwrap();
                let root = &root;
                let simultaneous = &simultaneous;
                threads.spawn(move || {
                    simultaneous.wait();
                    drop(send);
                    run(
                        root,
                        Config {
                            compress: false,
                            ..Config::default()
                        },
                        Fields::from([("connection".into(), "Edge / west__2".into())]),
                        records,
                        &Shutdown::default(),
                    )
                    .unwrap();
                });
            }
        });
        let logs: String = (2..=4)
            .map(|sequence| {
                fs::read_to_string(
                    folder
                        .path
                        .join(format!("{stamp}-Edge_west_2_{sequence}.log")),
                )
                .unwrap()
            })
            .collect();
        for line in ["session alpha", "session beta", "session gamma"] {
            assert_eq!(logs.matches(line).count(), 1);
        }
        assert_eq!(fs::read(archived).unwrap(), b"existing archive");
        assert_eq!(fs::read_dir(&folder.path).unwrap().count(), 4);
    }

    #[test]
    fn rotated_parts_use_their_own_timestamps_and_keep_the_claimed_sequence() {
        let profile = Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let folder = Folder::open(&root, &Config::default()).unwrap();
        let times = [
            "2025-01-02T10:00:00Z",
            "2025-01-02T10:01:00Z",
            "2025-01-04T10:00:00Z",
        ]
        .map(|time| time.parse::<jiff::Timestamp>().unwrap());
        let [first, second, next_day] = times.map(|at| {
            at.to_zoned(TimeZone::system())
                .strftime("%Y-%m-%d-%H-%M-%S")
                .to_string()
        });
        let existing = folder.path.join(format!("{first}-Edge_1.log"));
        fs::write(&existing, b"existing logfile").unwrap();
        let (send, records) = async_channel::bounded(3);
        for (at, line) in times
            .into_iter()
            .zip(["first part", "size roll", "daily roll"])
        {
            send.send_blocking(Record {
                at,
                fields: Fields::from([("line".into(), line.into())]),
            })
            .unwrap();
        }
        drop(send);
        run(
            &root,
            Config {
                max_mib: 0, // Force size rotation without generating large test records.
                compress: false,
                ..Config::default()
            },
            Fields::from([("connection".into(), "Edge".into())]),
            records,
            &Shutdown::default(),
        )
        .unwrap();
        for (name, line) in [
            (format!("{first}-Edge_2.log"), "first part"),
            (format!("{second}-Edge_2.p2.log"), "size roll"),
            (format!("{next_day}-Edge_2.log"), "daily roll"),
        ] {
            assert!(
                fs::read_to_string(folder.path.join(name))
                    .unwrap()
                    .contains(line)
            );
        }
        assert_eq!(fs::read(existing).unwrap(), b"existing logfile");
        assert_eq!(fs::read_dir(&folder.path).unwrap().count(), 4);
    }

    #[test]
    #[ignore = "Writes a 1,000 MiB part and exercises native rotation, compression and paging"]
    fn java_limit_rotates_without_waiting_for_gzip_and_views_both_images() {
        use crate::logfile_window::document::{self, Document, ExportFormat};

        let profile = Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let log_directory = root.join("terminal-logs");
        let config = Config {
            max_mib: super::super::MAX_LOG_MIB,
            rotate_daily: false,
            ..Config::default()
        };
        let at: jiff::Timestamp = "2025-01-02T10:00:00Z".parse().unwrap();
        let timestamp = at
            .to_zoned(TimeZone::system())
            .strftime("%Y-%m-%d %H:%M:%S.%3f")
            .to_string();
        let metadata = Fields::from([("connection".into(), "large log fixture".into())]);
        let mut header = Vec::new();
        write_header(&mut header, DocumentFormat::Text, &metadata).unwrap();
        let mut remaining = MAX_LOG_BYTES - header.len() as u64;
        let overhead = timestamp.len() + 4;
        let padding = "x".repeat(15 * 1024);
        let (send, receive) = async_channel::bounded(64);
        let shutdown = Shutdown::default();
        let mut expected_rows = 1;
        let archive_path = thread::scope(|threads| {
            let writer = threads.spawn(|| run(&root, config, metadata, receive, &shutdown));
            let marker = "last record at Java boundary";
            let closing_record = (overhead + marker.len()) as u64;
            let mut generated_rows = 0;
            let send_record = |line: String| {
                send.send_blocking(Record {
                    at,
                    fields: Fields::from([("line".into(), line)]),
                })
                .unwrap();
            };
            while remaining > closing_record {
                let record = (remaining - closing_record).min((overhead + padding.len()) as u64);
                if record < overhead as u64 {
                    break;
                }
                send_record(padding[..record as usize - overhead].into());
                remaining -= record;
                generated_rows += 1;
            }
            let final_line = format!(
                "{marker}{}",
                "x".repeat((remaining - closing_record) as usize)
            );
            send_record(final_line);
            send_record("next rotated part".into());
            expected_rows += generated_rows + 1;

            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
            let (archive, part) = loop {
                let entries = catalog::scan(&root, || false).unwrap().entries;
                let archive = entries
                    .iter()
                    .find(|entry| entry.path.to_string_lossy().ends_with(".log.gz") && !entry.live);
                let part = entries
                    .iter()
                    .find(|entry| entry.path.to_string_lossy().ends_with(".p2.log"));
                if let (Some(archive), Some(part)) = (archive, part)
                    && fs::read_to_string(&part.path)
                        .unwrap()
                        .contains("next rotated part")
                {
                    break (archive.path.clone(), part.path.clone());
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "rotation and compression did not complete while the capture was open"
                );
                thread::sleep(std::time::Duration::from_millis(50));
            };
            assert!(part.is_file());
            drop(send);
            writer.join().unwrap().unwrap();
            archive
        });
        let entry = |path: &Path| {
            catalog::scan(&root, || false)
                .unwrap()
                .entries
                .into_iter()
                .find(|entry| entry.path == path)
                .unwrap()
        };
        let packed = entry(&archive_path);
        let documentation = Document::open(&packed, || false).unwrap();
        assert_eq!(documentation.lines(), expected_rows);
        assert!(
            documentation
                .page(expected_rows - 1, || false)
                .unwrap()
                .lines[0]
                .contains("last record at Java boundary")
        );
        drop(documentation);

        let plain = log_directory.join("2025-01-02-10-00-00-ssh_99.log");
        document::export(
            &packed,
            &plain,
            ExportFormat::Unchanged,
            catalog::Compression::None,
            || false,
        )
        .unwrap();
        assert_eq!(fs::metadata(&plain).unwrap().len(), MAX_LOG_BYTES);
        let plain_entry = entry(&plain);
        let documentation = Document::open(&plain_entry, || false).unwrap();
        assert_eq!(documentation.lines(), expected_rows);
        assert!(
            documentation
                .page(expected_rows - 1, || false)
                .unwrap()
                .lines[0]
                .contains("last record at Java boundary")
        );
        drop(documentation);

        let polls = std::cell::Cell::new(0);
        let cancelled = Document::open(&packed, || {
            polls.set(polls.get() + 1);
            polls.get() > 24
        })
        .unwrap_err();
        assert_eq!(cancelled.kind(), io::ErrorKind::Interrupted);

        let folder = Folder::open(&root, &Config::default()).unwrap();
        let before = fs::read_dir(&log_directory).unwrap().count();
        let compressed_reads = std::cell::Cell::new(0);
        let cancel_compression = || {
            compressed_reads.set(compressed_reads.get() + 1);
            compressed_reads.get() > 64
        };
        let result = folder.compress(
            plain.file_name().unwrap().to_str().unwrap(),
            File::open(&plain).unwrap(),
            0,
            &cancel_compression,
        );
        assert!(is_compression_timeout(&result.unwrap_err()));
        assert_eq!(fs::metadata(&plain).unwrap().len(), MAX_LOG_BYTES);
        assert!(!plain.with_extension("log.gz").exists());
        assert_eq!(fs::read_dir(&log_directory).unwrap().count(), before);
    }

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
                .unwrap()
                .is_none()
        );
        assert!(!folder.path.join(name).exists());
        folder.remove(&marker_name, marker).unwrap();
        fs::write(folder.path.join(name), b"other logfile").unwrap();
        assert!(
            folder
                .begin(name, DocumentFormat::Text, &Fields::new())
                .unwrap()
                .is_none()
        );
        assert!(!marker_path.exists());
        assert_eq!(fs::read(folder.path.join(name)).unwrap(), b"other logfile");
        fs::remove_file(folder.path.join(name)).unwrap();

        let active = folder
            .begin(name, DocumentFormat::Text, &Fields::new())
            .unwrap()
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
