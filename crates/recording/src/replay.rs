use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::codec::{self, EVENT_HEADER, HEADER, Kind, MAX_EVENTS, MAX_FILE_BYTES};
use crate::{CaptureOptions, Error, Recorder, ReplayFrame, State};

const FALLBACK_FINAL_FRAME: Duration = Duration::from_secs(1);
static EXPORT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
struct IndexedFrame {
    offset: u64,
    length: usize,
    recorded_at: Duration,
    playback_at: Duration,
}

/// A bounded seek index and an open source. Frames are decoded on demand.
pub struct Replay<R = BufReader<File>> {
    reader: R,
    frames: Vec<IndexedFrame>,
    duration: Duration,
    complete: bool,
}

impl Replay {
    /// Opens an existing native replay file without loading its screens into memory.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        if !path.metadata()?.is_file() {
            return Err(Error::Invalid("replay source must be a regular file"));
        }
        let file = File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(Error::Invalid("replay source must be a regular file"));
        }
        Self::from_reader(BufReader::new(file))
    }
}

impl<R: Read + Seek> Replay<R> {
    /// Indexes a native stream. A missing or partial final event marks it incomplete.
    pub fn from_reader(mut reader: R) -> Result<Self, Error> {
        let length = reader.seek(SeekFrom::End(0))?;
        if length > MAX_FILE_BYTES {
            return Err(Error::Limit("recording file size"));
        }
        if length < HEADER.len() as u64 {
            return Err(Error::Format("truncated recording header"));
        }
        reader.seek(SeekFrom::Start(0))?;
        let mut magic = [0; HEADER.len()];
        reader.read_exact(&mut magic)?;
        if &magic != HEADER {
            return Err(Error::Format("unsupported recording header or version"));
        }

        let mut frames = Vec::new();
        let mut offset = HEADER.len() as u64;
        let mut previous_at = 0;
        let mut playback_at = Duration::ZERO;
        let mut state = State::Idle;
        let mut complete = false;
        for event_index in 0..=MAX_EVENTS {
            if length - offset < EVENT_HEADER as u64 {
                break;
            }
            if event_index == MAX_EVENTS {
                return Err(Error::Limit("event count"));
            }
            let event = codec::read_event(&mut reader)?;
            let payload = offset + EVENT_HEADER as u64;
            offset = payload + event.length as u64;
            if offset > length {
                break;
            }
            let delta = event
                .at
                .checked_sub(previous_at)
                .ok_or(Error::Format("nonmonotonic event timestamp"))?;
            if state == State::Recording {
                playback_at += Duration::from_nanos(delta);
            }
            previous_at = event.at;
            match (state, event.kind) {
                (State::Idle | State::Stopped, Kind::Start) => state = State::Recording,
                (State::Recording, Kind::Pause) => state = State::AutoPaused,
                (State::AutoPaused, Kind::Resume) => state = State::Recording,
                (State::Recording | State::AutoPaused, Kind::Stop) => state = State::Stopped,
                (State::Recording, Kind::InputActivity) => {}
                (State::Recording, Kind::Screen) => {
                    frames.push(IndexedFrame {
                        offset: payload,
                        length: event.length,
                        recorded_at: Duration::from_nanos(event.at),
                        playback_at,
                    });
                    reader.seek(SeekFrom::Start(offset))?;
                }
                (State::Idle | State::Stopped, Kind::End) => {
                    if offset != length {
                        return Err(Error::Format("bytes after the recording end"));
                    }
                    complete = true;
                    break;
                }
                _ => return Err(Error::Format("unexpected recording state transition")),
            }
        }

        let mut duration = Duration::ZERO;
        if let Some(first) = frames.first().map(|frame| frame.playback_at) {
            duration = playback_at - first;
            for frame in &mut frames {
                frame.playback_at -= first;
            }
            if let Some(last) = frames.last()
                && duration == last.playback_at
            {
                duration += FALLBACK_FINAL_FRAME;
            }
        }
        Ok(Self {
            reader,
            frames,
            duration,
            complete,
        })
    }

    /// Total seekable play time, omitting paused and stopped intervals.
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// Number of indexed screen changes.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Playback timestamp of a frame by index.
    pub fn frame_time(&self, index: usize) -> Option<Duration> {
        self.frames.get(index).map(|frame| frame.playback_at)
    }

    /// Index of the latest screen at or before a playback time.
    pub fn frame_index_at(&self, at: Duration) -> Option<usize> {
        (!self.frames.is_empty()).then(|| self.index_at(at))
    }

    /// Whether the recorder wrote its closing marker.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// Decodes the screen by zero-based index, returning `None` if out of range.
    pub fn frame(&mut self, index: usize) -> Result<Option<ReplayFrame>, Error> {
        let Some(entry) = self.frames.get(index).copied() else {
            return Ok(None);
        };
        let payload = self.payload(entry)?;
        let (pane, screen) = codec::decode_screen(&payload)?;
        Ok(Some(ReplayFrame {
            pane,
            recorded_at: entry.recorded_at,
            playback_at: entry.playback_at,
            screen,
        }))
    }

    /// Retrieves the latest screen at or before a playback time. Equal timestamps retain event order.
    pub fn seek(&mut self, at: Duration) -> Result<Option<ReplayFrame>, Error> {
        self.frame(self.index_at(at))
    }

    /// Writes a self-contained native replay of a nonempty playback range to a new file.
    ///
    /// The export starts with the frame visible at `range.start` and rebases time to zero.
    /// Publication requires same-volume hard-link support and rejects an existing destination.
    pub fn export_range(
        &mut self,
        path: impl AsRef<Path>,
        range: Range<Duration>,
    ) -> Result<(), Error> {
        if self.frames.is_empty() || range.start >= range.end || range.end > self.duration {
            return Err(Error::Invalid(
                "export range is outside the replay timeline",
            ));
        }
        let duration = range.end - range.start;
        codec::nanos(duration)?;
        let destination = path.as_ref();
        let stage = ExportStage::new(destination)?;
        let mut recorder = Recorder::create(stage.file(), CaptureOptions::default())?;
        recorder.start(Duration::ZERO)?;
        let first = self.index_at(range.start);
        for index in first..self.frames.len() {
            let entry = self.frames[index];
            if entry.playback_at >= range.end {
                break;
            }
            let payload = self.payload(entry)?;
            codec::decode_screen(&payload)?;
            recorder.exported_frame(entry.playback_at.saturating_sub(range.start), &payload)?;
        }
        let writer = recorder.finish(duration)?;
        writer.get_ref().sync_all()?;
        drop(writer);
        fs::hard_link(stage.file(), destination)?;
        Ok(())
    }

    fn index_at(&self, at: Duration) -> usize {
        self.frames
            .partition_point(|frame| frame.playback_at <= at)
            .saturating_sub(1)
    }

    fn payload(&mut self, frame: IndexedFrame) -> Result<Vec<u8>, Error> {
        self.reader.seek(SeekFrom::Start(frame.offset))?;
        let mut payload = vec![0; frame.length];
        self.reader.read_exact(&mut payload)?;
        Ok(payload)
    }
}

struct ExportStage(PathBuf);

impl ExportStage {
    fn new(destination: &Path) -> io::Result<Self> {
        let parent = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt as _;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        for _ in 0..16 {
            let sequence = EXPORT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let directory =
                parent.join(format!(".kortty-export-{}-{sequence}", std::process::id()));
            match builder.create(&directory) {
                Ok(()) => return Ok(Self(directory)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot reserve a recording export directory",
        ))
    }

    fn file(&self) -> PathBuf {
        self.0.join("capture")
    }
}

impl Drop for ExportStage {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.file());
        let _ = fs::remove_dir(&self.0);
    }
}
