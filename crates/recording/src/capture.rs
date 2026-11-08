use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Duration;

use kortty_terminal::ScreenSnapshot;

use crate::codec::{self, EVENT_HEADER, HEADER, Kind, MAX_EVENTS, MAX_FILE_BYTES};
use crate::{CaptureOptions, Error, State};

const MAX_CACHED_PANES: usize = 128;
const MAX_CACHED_BYTES: usize = 16 * 1024 * 1024;

/// Writes one recording. Times are elapsed monotonic durations from a caller-owned clock.
pub struct Recorder<W = BufWriter<File>> {
    writer: W,
    state: State,
    include_style: bool,
    idle_after: Option<u64>,
    last_at: u64,
    last_activity: u64,
    bytes: u64,
    events: usize,
    previous: HashMap<u64, Vec<u8>>,
    cached_bytes: usize,
    failed: bool,
}

impl Recorder {
    /// Exclusively creates a recording file. Unix files request mode `0600`.
    pub fn create(path: impl AsRef<Path>, options: CaptureOptions) -> Result<Self, Error> {
        idle_duration(options)?;
        let mut open = OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            open.mode(0o600);
        }
        let mut recorder = Self::new(BufWriter::new(open.open(path)?), options)?;
        recorder.flush()?;
        Ok(recorder)
    }
}

impl<W: Write> Recorder<W> {
    /// Writes a new native recording header to the supplied sink.
    pub fn new(mut writer: W, options: CaptureOptions) -> Result<Self, Error> {
        let idle_after = idle_duration(options)?;
        writer.write_all(HEADER)?;
        Ok(Self {
            writer,
            state: State::Idle,
            include_style: options.include_style,
            idle_after,
            last_at: 0,
            last_activity: 0,
            bytes: HEADER.len() as u64,
            events: 0,
            previous: HashMap::new(),
            cached_bytes: 0,
            failed: false,
        })
    }

    /// Current segment state.
    pub fn state(&self) -> State {
        self.state
    }

    /// Bytes accepted by the writer, including buffered data and event headers.
    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }

    /// Starts a new segment after creation or a manual stop.
    pub fn start(&mut self, at: Duration) -> Result<(), Error> {
        let at = self.timestamp(at)?;
        if self.is_active() {
            return Ok(());
        }
        self.ensure_capacity(0, 1, 2)?;
        self.write(Kind::Start, at, &[])?;
        self.state = State::Recording;
        self.last_activity = at;
        self.clear_cache();
        Ok(())
    }

    /// Records a changed visible grid. Returns `true` if a frame was written.
    pub fn record_screen(
        &mut self,
        at: Duration,
        pane: u64,
        screen: &ScreenSnapshot,
        pixels: (u16, u16),
    ) -> Result<bool, Error> {
        let at = self.timestamp(at)?;
        if !self.is_active() {
            return Ok(false);
        }
        let payload = codec::encode_screen(pane, screen, pixels, self.include_style)?;
        if self.previous.get(&pane).is_some_and(|old| old == &payload) {
            return Ok(false);
        }
        self.activity(Kind::Screen, at, &payload)?;
        self.cache(pane, payload);
        Ok(true)
    }

    /// Marks user activity without accepting input bytes, lengths or key names.
    pub fn input_activity(&mut self, at: Duration) -> Result<(), Error> {
        let at = self.timestamp(at)?;
        if self.is_active() {
            self.activity(Kind::InputActivity, at, &[])?;
        }
        Ok(())
    }

    /// Records an idle pause at the given time when the configured threshold has elapsed.
    pub fn check_idle(&mut self, at: Duration) -> Result<(), Error> {
        let at = self.timestamp(at)?;
        if self.state == State::Recording
            && self
                .idle_after
                .is_some_and(|idle| at - self.last_activity >= idle)
        {
            self.ensure_capacity(0, 1, 2)?;
            self.write(Kind::Pause, at, &[])?;
            self.state = State::AutoPaused;
        }
        Ok(())
    }

    /// Stops a segment and flushes its events. A later start appends a new segment.
    pub fn stop(&mut self, at: Duration) -> Result<(), Error> {
        let at = self.timestamp(at)?;
        if self.is_active() {
            self.ensure_capacity(0, 1, 1)?;
            self.write(Kind::Stop, at, &[])?;
            self.state = State::Stopped;
            self.clear_cache();
            self.flush()?;
        }
        Ok(())
    }

    /// Closes any active segment, writes the end marker and flushes the sink.
    pub fn finish(mut self, at: Duration) -> Result<W, Error> {
        let at = self.timestamp(at)?;
        let active = self.is_active();
        self.ensure_capacity(0, 1 + usize::from(active), 0)?;
        if active {
            self.write(Kind::Stop, at, &[])?;
        }
        self.write(Kind::End, at, &[])?;
        self.flush()?;
        Ok(self.writer)
    }

    /// Makes buffered events readable without ending the recording or syncing the disk.
    pub fn flush(&mut self) -> Result<(), Error> {
        if self.failed {
            return Err(Error::Poisoned);
        }
        self.writer.flush().map_err(|error| {
            self.failed = true;
            Error::Io(error)
        })
    }

    pub(crate) fn exported_frame(&mut self, at: Duration, payload: &[u8]) -> Result<(), Error> {
        let at = self.timestamp(at)?;
        self.activity(Kind::Screen, at, payload)
    }

    fn activity(&mut self, kind: Kind, at: u64, payload: &[u8]) -> Result<(), Error> {
        let resume = self.state == State::AutoPaused;
        self.ensure_capacity(payload.len(), 1 + usize::from(resume), 2)?;
        if resume {
            self.write(Kind::Resume, at, &[])?;
            self.state = State::Recording;
        }
        self.write(kind, at, payload)?;
        self.last_activity = at;
        Ok(())
    }

    fn is_active(&self) -> bool {
        matches!(self.state, State::Recording | State::AutoPaused)
    }

    fn timestamp(&mut self, at: Duration) -> Result<u64, Error> {
        if self.failed {
            return Err(Error::Poisoned);
        }
        let at = codec::nanos(at)?;
        if at < self.last_at {
            return Err(Error::Invalid("timestamps must be monotonic"));
        }
        self.last_at = at;
        Ok(at)
    }

    fn ensure_capacity(&self, payload: usize, events: usize, footer: usize) -> Result<(), Error> {
        if self.events + events + footer > MAX_EVENTS
            || self.bytes + (payload + (events + footer) * EVENT_HEADER) as u64 > MAX_FILE_BYTES
        {
            return Err(Error::Limit("recording file size or event count"));
        }
        Ok(())
    }

    fn write(&mut self, kind: Kind, at: u64, payload: &[u8]) -> Result<(), Error> {
        if let Err(error) = codec::write_event(&mut self.writer, kind, at, payload) {
            self.failed = true;
            return Err(error);
        }
        self.events += 1;
        self.bytes += (EVENT_HEADER + payload.len()) as u64;
        Ok(())
    }

    fn cache(&mut self, pane: u64, payload: Vec<u8>) {
        if let Some(old) = self.previous.remove(&pane) {
            self.cached_bytes -= old.len();
        }
        if self.previous.len() >= MAX_CACHED_PANES
            || self.cached_bytes + payload.len() > MAX_CACHED_BYTES
        {
            self.clear_cache();
        }
        self.cached_bytes += payload.len();
        self.previous.insert(pane, payload);
    }

    fn clear_cache(&mut self) {
        self.previous.clear();
        self.cached_bytes = 0;
    }
}

fn idle_duration(options: CaptureOptions) -> Result<Option<u64>, Error> {
    let idle = options.idle_after.map(codec::nanos).transpose()?;
    if idle == Some(0) {
        return Err(Error::Invalid("idle threshold must be positive"));
    }
    Ok(idle)
}
