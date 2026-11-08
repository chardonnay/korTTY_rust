//! Bounded application diagnostics containing only fixed, non-session events.

use std::fs::{File, FileTimes};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use jiff::civil::Date;
use jiff::tz::TimeZone;

use crate::private_files::{Directory, FileStamp};

use super::{Effective, Format};

const ACTIVE: &str = "kortty.log";
const MAX_ENTRIES: usize = 16_384;
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const QUEUE_CAPACITY: usize = 1024;
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug)]
pub(crate) enum Event {
    Started,
    Stopping,
    ConfigurationApplied,
    WindowOpened,
    WindowClosed,
    SessionStarted,
    SessionStopped,
    PolicyRejected,
    OperationFailed,
}

impl Event {
    fn message(self) -> &'static str {
        match self {
            Self::Started => "Application started",
            Self::Stopping => "Application stopping",
            Self::ConfigurationApplied => "Application logging configuration applied",
            Self::WindowOpened => "Application window opened",
            Self::WindowClosed => "Application window closed",
            Self::SessionStarted => "Terminal session started",
            Self::SessionStopped => "Terminal session stopped",
            Self::PolicyRejected => "Operation rejected by managed policy",
            Self::OperationFailed => "Application operation failed",
        }
    }

    fn level(self) -> &'static str {
        match self {
            Self::PolicyRejected => "WARN",
            Self::OperationFailed => "ERROR",
            _ => "INFO",
        }
    }
}

pub(crate) struct Prepared {
    pub(super) directory: Arc<Directory>,
    settings: Effective,
}

impl Prepared {
    pub(crate) fn new(settings: &Effective) -> io::Result<Self> {
        if settings.retention_days > super::MAX_RETENTION_DAYS {
            return Err(invalid("Application log retention exceeds 3650 days"));
        }
        Ok(Self {
            directory: Arc::new(open_directory(&settings.directory, true)?),
            settings: settings.clone(),
        })
    }
}

enum Command {
    Record(Event, SystemTime),
    Wake,
}

pub(crate) struct Writer {
    directory: Arc<Directory>,
    sender: Option<SyncSender<Command>>,
    completion: async_channel::Receiver<Result<(), String>>,
    worker: JoinHandle<()>,
    settings: Arc<Mutex<Effective>>,
    configured: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
}

impl Writer {
    pub(crate) fn start(prepared: Prepared) -> io::Result<Self> {
        Self::start_log(Log::open(prepared, SystemTime::now())?)
    }

    fn start_log(log: Log) -> io::Result<Self> {
        let directory = Arc::clone(&log.prepared.directory);
        let settings = Arc::new(Mutex::new(log.prepared.settings.clone()));
        let configured = Arc::new(AtomicBool::new(false));
        let closing = Arc::clone(&log.closing);
        let error = Arc::new(Mutex::new(None));
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let (complete, completion) = async_channel::bounded(1);
        let state = (
            Arc::clone(&settings),
            Arc::clone(&configured),
            Arc::clone(&error),
        );
        let worker = thread::Builder::new()
            .name("kortty-app-log".into())
            .spawn(move || {
                let result = run(log, receiver, state.0, state.1, state.2);
                let _ = complete.try_send(result.map_err(|error| error.to_string()));
            })?;
        Ok(Self {
            directory,
            sender: Some(sender),
            completion,
            worker,
            settings,
            configured,
            closing,
            error,
        })
    }

    pub(crate) fn record(&self, event: Event) -> bool {
        let Some(sender) = &self.sender else {
            return false;
        };
        match sender.try_send(Command::Record(event, SystemTime::now())) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                *self
                    .error
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) =
                    Some("Application log event queue is full".into());
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    pub(crate) fn configure(&self, settings: Effective) {
        *self
            .settings
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = settings;
        self.configured.store(true, Ordering::Release);
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(Command::Wake);
        }
    }

    pub(crate) fn take_error(&self) -> Option<String> {
        self.error
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.worker.is_finished()
    }

    pub(super) fn directory(&self) -> Arc<Directory> {
        Arc::clone(&self.directory)
    }

    pub(crate) fn close(&mut self) -> async_channel::Receiver<Result<(), String>> {
        self.closing.store(true, Ordering::Release);
        self.sender.take();
        self.completion.clone()
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::Release);
        self.sender.take();
    }
}

fn run(
    mut log: Log,
    receiver: Receiver<Command>,
    settings: Arc<Mutex<Effective>>,
    configured: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
) -> io::Result<()> {
    let mut first_error = None;
    let mut report = |result: io::Result<()>| {
        if let Err(problem) = result {
            // Shutdown deliberately cancels optional maintenance while preserving the raw archive.
            if problem.kind() != io::ErrorKind::Interrupted {
                first_error.get_or_insert_with(|| problem.to_string());
                *error.lock().unwrap_or_else(|poison| poison.into_inner()) =
                    Some(problem.to_string());
            }
        }
    };
    let mut activated = false;
    loop {
        let command = if activated {
            receiver.recv_timeout(MAINTENANCE_INTERVAL)
        } else {
            receiver.recv().map_err(|_| RecvTimeoutError::Disconnected)
        };
        if configured.swap(false, Ordering::AcqRel) {
            let next = settings
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone();
            report(log.configure(next).and_then(|()| {
                if activated {
                    log.maintain(SystemTime::now())
                } else {
                    Ok(())
                }
            }));
        }
        match command {
            Ok(Command::Record(event, at)) => {
                let first_record = !activated;
                activated = true;
                let day = log.day;
                report(log.append(event, at));
                if first_record || day != log.day {
                    report(log.maintain(SystemTime::now()));
                }
            }
            Ok(Command::Wake) => {}
            Err(RecvTimeoutError::Timeout) => report(log.maintain(SystemTime::now())),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    report(log.active.sync_all());
    // A concurrent fork can retain the open file description until exec.
    report(log.active.unlock());
    drop(log);
    match first_error {
        Some(error) => Err(io::Error::other(error)),
        None => Ok(()),
    }
}

fn open_directory(path: &Path, create: bool) -> io::Result<Directory> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Application logs require an absolute child directory"))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("Application log directory has an invalid name"))?;
    Directory::open(parent, create)?.private_child(name, create)
}

struct Log {
    prepared: Prepared,
    active: File,
    stamp: FileStamp,
    day: Date,
    timezone: TimeZone,
    closing: Arc<AtomicBool>,
}

impl Log {
    fn open(prepared: Prepared, now: SystemTime) -> io::Result<Self> {
        let active = prepared.directory.open_lock(ACTIVE)?;
        active.try_lock().map_err(io::Error::from)?;
        let stamp = FileStamp::read(&active)?;
        let timezone = TimeZone::system();
        let day = local_day(
            if active.metadata()?.len() == 0 {
                now
            } else {
                stamp.modified
            },
            &timezone,
        )?;
        let log = Self {
            prepared,
            active,
            stamp,
            day,
            timezone,
            closing: Arc::new(AtomicBool::new(false)),
        };
        log.require_current()?;
        Ok(log)
    }

    fn configure(&mut self, settings: Effective) -> io::Result<()> {
        let directory = open_directory(&settings.directory, false)?;
        if !self.prepared.directory.same_as(&directory)? {
            return Err(invalid(
                "A different application log directory requires a new writer",
            ));
        }
        self.prepared.settings = settings;
        Ok(())
    }

    fn require_current(&self) -> io::Result<()> {
        if !self
            .prepared
            .directory
            .same_as(&open_directory(&self.prepared.settings.directory, false)?)?
        {
            return Err(invalid("Application log directory changed"));
        }
        self.stamp.require_unchanged(&self.active)?;
        self.stamp
            .require_unchanged(&self.prepared.directory.open_private_file(ACTIVE)?)
    }

    fn append(&mut self, event: Event, now: SystemTime) -> io::Result<()> {
        self.require_current()?;
        let timestamp = jiff::Timestamp::try_from(now)
            .map_err(io::Error::other)?
            .to_zoned(self.timezone.clone());
        if timestamp.date() > self.day {
            self.rotate(timestamp.date())?;
        }
        let mut bytes = match self.prepared.settings.format {
            Format::Text => format!(
                "{} [kortty-app-log] {:<5} kortty - {}",
                timestamp.strftime("%Y-%m-%d %H:%M:%S.%3f"),
                event.level(),
                event.message()
            )
            .into_bytes(),
            Format::Json => serde_json::to_vec(&serde_json::json!({
                "timestamp": timestamp.strftime("%Y-%m-%d %H:%M:%S.%3f").to_string(),
                "level": event.level(),
                "logger": "kortty",
                "message": event.message(),
            }))
            .map_err(io::Error::other)?,
        };
        bytes.push(b'\n');
        let length = self.active.metadata()?.len();
        let result = self
            .active
            .seek(SeekFrom::End(0))
            .and_then(|_| self.active.write_all(&bytes))
            .and_then(|()| self.active.flush());
        if let Err(error) = result {
            let _ = self.rollback_append(length);
            return Err(error);
        }
        self.stamp = FileStamp::read(&self.active)?;
        Ok(())
    }

    fn rollback_append(&mut self, length: u64) -> io::Result<()> {
        if !self
            .prepared
            .directory
            .same_as(&open_directory(&self.prepared.settings.directory, false)?)?
        {
            return Err(invalid(
                "Application log directory changed during a failed write",
            ));
        }
        let current = self.prepared.directory.open_private_file(ACTIVE)?;
        // Both stamps are read after our failed append. They include the identity
        // as well as the partial extension, so a replaced file cannot be truncated.
        FileStamp::read(&self.active)?.require_unchanged(&current)?;
        if self.active.metadata()?.len() < length {
            return Err(invalid(
                "Application log was shortened during a failed write",
            ));
        }
        self.active.set_len(length)?;
        self.stamp = FileStamp::read(&self.active)?;
        Ok(())
    }

    fn rotate(&mut self, day: Date) -> io::Result<()> {
        self.require_current()?;
        self.active.sync_all()?;
        if self.active.metadata()?.len() > 0 {
            let name = format!("kortty.{}.log", self.day);
            let published = self.prepared.directory.publish_checked(
                &name,
                |output| {
                    copy_bounded(&mut self.active.try_clone()?, output, None)?;
                    output.set_times(FileTimes::new().set_modified(self.stamp.modified))
                },
                || self.require_current(),
            );
            match published {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if !self.archive_matches_active(&name)? {
                        return Err(error);
                    }
                }
                result => result?,
            }
            self.require_current()?;
            self.active.set_len(0)?;
            self.active.sync_all()?;
            self.stamp = FileStamp::read(&self.active)?;
        }
        self.day = day;
        Ok(())
    }

    fn archive_matches_active(&self, name: &str) -> io::Result<bool> {
        let mut archived = self.prepared.directory.open_private_file(name)?;
        let stamp = FileStamp::read(&archived)?;
        let length = self.active.metadata()?.len();
        if length > MAX_ARCHIVE_BYTES || archived.metadata()?.len() != length {
            return Ok(false);
        }
        let mut active = self.active.try_clone()?;
        active.seek(SeekFrom::Start(0))?;
        let mut remaining = length;
        let mut left = [0; 32 * 1024];
        let mut right = [0; 32 * 1024];
        while remaining > 0 {
            let count = remaining.min(left.len() as u64) as usize;
            archived.read_exact(&mut left[..count])?;
            active.read_exact(&mut right[..count])?;
            if left[..count] != right[..count] {
                return Ok(false);
            }
            remaining -= count as u64;
        }
        self.require_current()?;
        stamp.require_unchanged(&archived)?;
        stamp.require_unchanged(&self.prepared.directory.open_private_file(name)?)?;
        Ok(true)
    }

    fn maintain(&self, now: SystemTime) -> io::Result<()> {
        self.require_current()?;
        if self.closing.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut archives = Vec::new();
        for name in self.prepared.directory.names(MAX_ENTRIES)? {
            if self.closing.load(Ordering::Acquire) {
                break;
            }
            let Some(name) = name.to_str().filter(|name| archive_date(name).is_some()) else {
                continue;
            };
            let file = self.prepared.directory.open_private_file(name)?;
            let stamp = FileStamp::read(&file)?;
            let age = now.duration_since(stamp.modified).unwrap_or_default();
            if self.prepared.settings.retention_days > 0
                && age > DAY.saturating_mul(u32::from(self.prepared.settings.retention_days))
            {
                self.remove_archive(name, &stamp)?;
                continue;
            }
            if self.prepared.settings.compress
                && name.ends_with(".log")
                && age > DAY
                && !self.prepared.directory.has_entry(&format!("{name}.gz"))?
            {
                self.compress(name, file, &stamp)?;
                let name = format!("{name}.gz");
                let file = self.prepared.directory.open_private_file(&name)?;
                archives.push(Archive {
                    name,
                    stamp: FileStamp::read(&file)?,
                    bytes: file.metadata()?.len(),
                });
            } else {
                archives.push(Archive {
                    name: name.to_owned(),
                    stamp,
                    bytes: file.metadata()?.len(),
                });
            }
        }
        archives.sort_by(|left, right| {
            left.stamp
                .modified
                .cmp(&right.stamp.modified)
                .then_with(|| left.name.cmp(&right.name))
        });
        let settings = &self.prepared.settings;
        let mut count = archives.len();
        let mut bytes = archives
            .iter()
            .fold(0_u64, |total, archive| total.saturating_add(archive.bytes));
        for archive in archives {
            if self.closing.load(Ordering::Acquire) {
                break;
            }
            let excess_count =
                settings.rotation_max_files > 0 && count > settings.rotation_max_files as usize;
            let excess_bytes = settings.rotation_total_size_mb > 0
                && bytes > u64::from(settings.rotation_total_size_mb) * 1024 * 1024;
            if !excess_count && !excess_bytes {
                break;
            }
            self.remove_archive(&archive.name, &archive.stamp)?;
            count -= 1;
            bytes = bytes.saturating_sub(archive.bytes);
        }
        self.prepared.directory.sync()
    }

    fn compress(&self, name: &str, file: File, stamp: &FileStamp) -> io::Result<()> {
        let destination = format!("{name}.gz");
        let mut input = file.try_clone()?;
        self.prepared.directory.publish_checked(
            &destination,
            |output| {
                let mut encoder =
                    flate2::write::GzEncoder::new(&mut *output, flate2::Compression::default());
                copy_bounded(&mut input, &mut encoder, Some(&self.closing))?;
                encoder.finish()?;
                output.set_times(FileTimes::new().set_modified(stamp.modified))
            },
            || {
                self.require_current()?;
                stamp.require_unchanged(&file)?;
                stamp.require_unchanged(&self.prepared.directory.open_private_file(name)?)
            },
        )?;
        drop(file);
        self.remove_archive(name, stamp)
    }

    fn remove_archive(&self, name: &str, stamp: &FileStamp) -> io::Result<()> {
        self.require_current()?;
        stamp.require_unchanged(&self.prepared.directory.open_private_file(name)?)?;
        self.prepared.directory.remove_unchanged(name, stamp)
    }
}

struct Archive {
    name: String,
    stamp: FileStamp,
    bytes: u64,
}

fn archive_date(name: &str) -> Option<Date> {
    let suffix = name.strip_prefix("kortty.")?;
    let date = suffix
        .strip_suffix(".gz")
        .unwrap_or(suffix)
        .strip_suffix(".log")?;
    if date.len() != 10 {
        return None;
    }
    date.parse().ok()
}

fn copy_bounded(
    input: &mut File,
    output: &mut impl Write,
    closing: Option<&AtomicBool>,
) -> io::Result<()> {
    if input.metadata()?.len() > MAX_ARCHIVE_BYTES {
        return Err(invalid(
            "Application log archive exceeds the maintenance limit",
        ));
    }
    input.seek(SeekFrom::Start(0))?;
    let mut bytes = 0_u64;
    let mut buffer = [0; 32 * 1024];
    loop {
        if closing.is_some_and(|closing| closing.load(Ordering::Acquire)) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Application log maintenance cancelled for shutdown",
            ));
        }
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes += read as u64;
        if bytes > MAX_ARCHIVE_BYTES {
            return Err(invalid("Application log grew during maintenance"));
        }
        output.write_all(&buffer[..read])?;
    }
    Ok(())
}

fn local_day(now: SystemTime, timezone: &TimeZone) -> io::Result<Date> {
    Ok(jiff::Timestamp::try_from(now)
        .map_err(io::Error::other)?
        .to_zoned(timezone.clone())
        .date())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;
