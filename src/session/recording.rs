use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use async_channel::Sender;
use kortty_recording::{CaptureOptions, Recorder, State};
use kortty_terminal::{Color, Palette, ScreenSnapshot, Terminal};

use crate::terminal_theme::{Appearance, BoldTextMode};

use super::GridSize;

const QUEUE_SIZE: usize = 4;
const TICK: Duration = Duration::from_millis(250);
const FLUSH: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum RecordingStatus {
    Unavailable,
    #[default]
    Idle,
    Starting(PathBuf),
    Capturing {
        path: PathBuf,
        paused: bool,
    },
    Stopping(PathBuf),
    Saved(PathBuf),
    Failed {
        path: Option<PathBuf>,
        error: String,
    },
}

impl RecordingStatus {
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Starting(path)
            | Self::Stopping(path)
            | Self::Saved(path)
            | Self::Capturing { path, .. } => Some(path),
            Self::Failed { path, .. } => path.as_deref(),
            _ => None,
        }
    }

    fn active(&self) -> bool {
        matches!(
            self,
            Self::Starting(_) | Self::Capturing { .. } | Self::Stopping(_)
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordingCommandError {
    Unavailable,
    Busy,
    Closed,
}

impl fmt::Display for RecordingCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "Recording requires a live stream terminal",
            Self::Busy => "The terminal is busy with another recording operation",
            Self::Closed => "The terminal has closed",
        })
    }
}

#[derive(Default)]
pub(super) struct Control {
    report: Mutex<(u64, RecordingStatus)>,
    active: AtomicU64,
    overflow: AtomicU64,
}

impl Control {
    pub(super) fn terminal_ended(&self, redraw: &Sender<()>) {
        let mut report = self
            .report
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if report.1.active() {
            report.1 = RecordingStatus::Failed {
                path: None,
                error: "The terminal ended before recording could finish".into(),
            };
            let _ = redraw.try_send(());
        }
    }

    pub(super) fn status(&self) -> RecordingStatus {
        self.report
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .1
            .clone()
    }

    pub(super) fn request_start(
        &self,
        path: PathBuf,
        options: CaptureOptions,
        send: impl FnOnce(Command) -> Result<(), RecordingCommandError>,
    ) -> Result<(), RecordingCommandError> {
        let mut report = self
            .report
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if report.1.active() {
            return Err(RecordingCommandError::Busy);
        }
        let id = report.0.wrapping_add(1).max(1);
        send(Command::Start(Request {
            id,
            path: path.clone(),
            options,
        }))?;
        *report = (id, RecordingStatus::Starting(path));
        Ok(())
    }

    pub(super) fn request_stop(
        &self,
        send: impl FnOnce(Command) -> Result<(), RecordingCommandError>,
    ) -> Result<(), RecordingCommandError> {
        let mut report = self
            .report
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (RecordingStatus::Starting(path) | RecordingStatus::Capturing { path, .. }) = &report.1
        else {
            return Err(RecordingCommandError::Busy);
        };
        let path = path.clone();
        send(Command::Stop(report.0))?;
        report.1 = RecordingStatus::Stopping(path);
        Ok(())
    }

    pub(super) fn generation(&self) -> u64 {
        self.active.load(Ordering::Acquire)
    }

    pub(super) fn overflow(&self, id: u64) {
        self.overflow.store(id, Ordering::Release);
    }

    fn deactivate(&self, id: u64) {
        let _ = self
            .active
            .compare_exchange(id, 0, Ordering::AcqRel, Ordering::Acquire);
    }

    fn publish(&self, id: u64, status: RecordingStatus, redraw: &Sender<()>) {
        let mut report = self
            .report
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if report.0 != id
            || report.1 == status
            || matches!(
                (&report.1, &status),
                (
                    RecordingStatus::Stopping(_),
                    RecordingStatus::Capturing { .. }
                )
            )
        {
            return;
        }
        report.1 = status;
        let _ = redraw.try_send(());
    }
}

pub(super) enum Command {
    Start(Request),
    Stop(u64),
}

pub(super) struct Request {
    id: u64,
    path: PathBuf,
    options: CaptureOptions,
}

enum Sample {
    Screen(ScreenSnapshot, (u16, u16), Option<RecordedAppearance>),
    InputActivity,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct RecordedAppearance {
    bold_text_mode: BoldTextMode,
    palette: Palette,
}

impl From<Appearance> for RecordedAppearance {
    fn from(appearance: Appearance) -> Self {
        Self {
            bold_text_mode: appearance.bold_text_mode,
            palette: appearance.palette,
        }
    }
}

impl RecordedAppearance {
    fn apply(self, screen: &mut ScreenSnapshot) {
        let built_in = Palette::default();
        if self.bold_text_mode.is_default() && self.palette == built_in {
            return;
        }
        let resolve = |color| {
            if let Color::Indexed(index @ 0..=15) = color {
                let [red, green, blue] = self.palette.rgb(index);
                if [red, green, blue] != built_in.rgb(index) {
                    return Color::Rgb(red, green, blue);
                }
            }
            color
        };
        for cell in &mut screen.cells {
            let style = &mut cell.style;
            style.foreground = resolve(self.bold_text_mode.foreground(*style));
            style.background = resolve(style.background);
            style.bold &= self.bold_text_mode.uses_bold_font();
        }
    }
}

struct End {
    at: Duration,
    warning: Option<String>,
}

struct Capture {
    id: u64,
    started: Instant,
    samples: SyncSender<(Duration, Sample)>,
    end: Arc<Mutex<Option<End>>>,
    worker: JoinHandle<()>,
    appearance: Option<RecordedAppearance>,
}

/// The terminal actor submits owned frames without waiting on the file worker.
pub(super) struct Actor {
    control: Arc<Control>,
    redraw: Sender<()>,
    active: Option<Capture>,
    closing: Vec<JoinHandle<()>>,
}

impl Actor {
    pub(super) fn new(control: Arc<Control>, redraw: Sender<()>) -> Self {
        Self {
            control,
            redraw,
            active: None,
            closing: Vec::new(),
        }
    }

    pub(super) fn command(
        &mut self,
        command: Command,
        terminal: &Terminal,
        grid: GridSize,
        appearance: Appearance,
    ) {
        match command {
            Command::Start(request) => {
                self.stop(None);
                self.closing.retain(|worker| !worker.is_finished());
                self.start(request, appearance);
                self.frame(terminal, grid, appearance);
            }
            Command::Stop(id) if self.active.as_ref().is_some_and(|capture| capture.id == id) => {
                self.stop(None);
            }
            Command::Stop(_) => {}
        }
    }

    pub(super) fn appearance_changed(&self, appearance: Appearance) -> bool {
        self.active
            .as_ref()
            .and_then(|active| active.appearance)
            .is_some_and(|recorded| recorded != appearance.into())
    }

    pub(super) fn frame(&mut self, terminal: &Terminal, grid: GridSize, appearance: Appearance) {
        let Some(active) = self
            .active
            .as_mut()
            .filter(|active| self.control.generation() == active.id)
        else {
            return;
        };
        let style = active.appearance.as_mut().map(|recorded| {
            *recorded = appearance.into();
            *recorded
        });
        self.submit(
            Instant::now(),
            Sample::Screen(
                terminal.snapshot(),
                (grid.pixel_width, grid.pixel_height),
                style,
            ),
        );
    }

    pub(super) fn activity(&mut self, id: u64, at: Instant) {
        if self.accepting() && self.control.generation() == id {
            self.submit(at, Sample::InputActivity);
        }
    }

    pub(super) fn check_overflow(&mut self) {
        if let Some(active) = &self.active
            && self.control.overflow.load(Ordering::Acquire) == active.id
        {
            self.stop(Some(
                "The recording queue filled. The saved prefix is available.".into(),
            ));
        }
    }

    pub(super) fn stop(&mut self, warning: Option<String>) {
        if let Some(active) = self.active.take() {
            self.control.deactivate(active.id);
            *active.end.lock().unwrap_or_else(|error| error.into_inner()) = Some(End {
                at: active.started.elapsed(),
                warning,
            });
            drop(active.samples);
            self.closing.push(active.worker);
        }
    }

    fn accepting(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|capture| self.control.generation() == capture.id)
    }

    fn start(&mut self, request: Request, appearance: Appearance) {
        let id = request.id;
        let appearance = request.options.include_style.then_some(appearance.into());
        let started = Instant::now();
        let (samples, incoming) = mpsc::sync_channel(QUEUE_SIZE);
        let end = Arc::new(Mutex::new(None));
        let closing = Arc::clone(&end);
        let control = Arc::clone(&self.control);
        let redraw = self.redraw.clone();
        // Activate before spawning so an early disk failure cannot be overwritten.
        self.control.active.store(id, Ordering::Release);
        match thread::Builder::new()
            .name("kortty-recording".into())
            .spawn(move || {
                let mut opened = false;
                let result = write_recording(
                    &request,
                    started,
                    incoming,
                    &closing,
                    &control,
                    &redraw,
                    &mut opened,
                );
                control.deactivate(id);
                let status = match result {
                    Ok(None) => RecordingStatus::Saved(request.path),
                    Ok(Some(error)) => RecordingStatus::Failed {
                        path: Some(request.path),
                        error,
                    },
                    Err(error) => RecordingStatus::Failed {
                        path: opened.then_some(request.path),
                        error: error.to_string(),
                    },
                };
                control.publish(id, status, &redraw);
            }) {
            Ok(worker) => {
                self.active = Some(Capture {
                    id,
                    started,
                    samples,
                    end,
                    worker,
                    appearance,
                })
            }
            Err(error) => {
                self.control.deactivate(id);
                self.control.publish(
                    id,
                    RecordingStatus::Failed {
                        path: None,
                        error: error.to_string(),
                    },
                    &self.redraw,
                );
            }
        }
    }

    fn submit(&mut self, at: Instant, sample: Sample) {
        let Some(active) = &self.active else { return };
        match active
            .samples
            .try_send((at.saturating_duration_since(active.started), sample))
        {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                self.stop(Some(
                    "The recording queue filled. The saved prefix is available.".into(),
                ));
            }
            Err(mpsc::TrySendError::Disconnected(_)) => self.stop(None),
        }
    }
}

impl Drop for Actor {
    fn drop(&mut self) {
        self.stop(None);
        for worker in self.closing.drain(..) {
            let _ = worker.join();
        }
    }
}

fn write_recording(
    request: &Request,
    started: Instant,
    samples: Receiver<(Duration, Sample)>,
    end: &Mutex<Option<End>>,
    control: &Control,
    redraw: &Sender<()>,
    opened: &mut bool,
) -> Result<Option<String>, kortty_recording::Error> {
    if let Some(parent) = request.path.parent() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder.create(parent)?;
    }
    let mut writer = Recorder::create(&request.path, request.options)?;
    *opened = true;
    writer.start(Duration::ZERO)?;
    let mut at = Duration::ZERO;
    let mut flushed = Instant::now();
    let mut announced = None;
    loop {
        match samples.recv_timeout(TICK) {
            Ok((stamp, sample)) => {
                at = at.max(stamp);
                match sample {
                    Sample::Screen(mut screen, pixels, appearance) => {
                        if let Some(appearance) = appearance {
                            appearance.apply(&mut screen);
                        }
                        writer.record_screen(at, 0, &screen, pixels)?;
                    }
                    Sample::InputActivity => writer.input_activity(at)?,
                }
            }
            Err(RecvTimeoutError::Timeout) => at = at.max(started.elapsed()),
            Err(RecvTimeoutError::Disconnected) => break,
        }
        writer.check_idle(at)?;
        let paused = writer.state() == State::AutoPaused;
        if announced != Some(paused) {
            control.publish(
                request.id,
                RecordingStatus::Capturing {
                    path: request.path.clone(),
                    paused,
                },
                redraw,
            );
            announced = Some(paused);
        }
        if flushed.elapsed() >= FLUSH {
            writer.flush()?;
            flushed = Instant::now();
        }
    }
    let end = end.lock().unwrap_or_else(|error| error.into_inner()).take();
    let stamp = end.as_ref().map_or(at, |end| at.max(end.at));
    writer.finish(stamp)?.get_ref().sync_all()?;
    Ok(end.and_then(|end| end.warning))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold_text_recording_replays_a_live_mode_change_with_the_active_palette() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("mode.korttyrec");
        let (redraw, _) = async_channel::bounded(1);
        let mut actor = Actor::new(Arc::new(Control::default()), redraw);
        let mut terminal = Terminal::new(8, 2);
        terminal.advance(b"\x1b[1;31mB\x1b[0m");
        let grid = GridSize::default();
        let original = Appearance::default();
        actor.command(
            Command::Start(Request {
                id: 1,
                path: path.clone(),
                options: CaptureOptions {
                    include_style: true,
                    idle_after: None,
                },
            }),
            &terminal,
            grid,
            original,
        );
        assert!(!actor.appearance_changed(original));

        let mut colors = crate::terminal_theme::Colors::default();
        colors.ansi[9] = Some(crate::terminal_theme::Rgb::from_hex(0x1a8bce));
        let changed = Appearance {
            bold_text_mode: BoldTextMode::BrightColor,
            ..colors.appearance(None)
        };
        assert!(actor.appearance_changed(changed));
        actor.frame(&terminal, grid, changed);
        assert!(!actor.appearance_changed(changed));
        actor.command(Command::Stop(1), &terminal, grid, changed);
        drop(actor);

        let mut replay = kortty_recording::Replay::open(path).unwrap();
        assert!(replay.is_complete());
        assert_eq!(replay.frame_count(), 2);
        let first = replay.frame(0).unwrap().unwrap();
        let second = replay.frame(1).unwrap().unwrap();
        assert!(first.screen.cells[0].style.bold);
        assert_eq!(first.screen.cells[0].style.foreground, Color::Indexed(1));
        assert!(!second.screen.cells[0].style.bold);
        assert_eq!(
            second.screen.cells[0].style.foreground,
            Color::Rgb(0x1a, 0x8b, 0xce)
        );
        assert_eq!(second.screen.cells[0].text, "B");
    }

    #[test]
    fn a_full_disk_queue_closes_capture_and_keeps_its_accepted_prefix() {
        let (redraw, _) = async_channel::bounded(1);
        let control = Arc::new(Control::default());
        control.active.store(7, Ordering::Release);
        let mut actor = Actor::new(Arc::clone(&control), redraw);
        let (samples, incoming) = mpsc::sync_channel(1);
        let end = Arc::new(Mutex::new(None));
        actor.active = Some(Capture {
            id: 7,
            started: Instant::now(),
            samples,
            end: Arc::clone(&end),
            // Test double: the receiver is deliberately not polled until overflow.
            worker: thread::spawn(|| {}),
            appearance: None,
        });
        actor.activity(7, Instant::now());
        actor.activity(7, Instant::now());
        assert_eq!(control.generation(), 0);
        assert!(actor.active.is_none());
        assert!(matches!(incoming.recv().unwrap().1, Sample::InputActivity));
        assert!(incoming.recv().is_err());
        assert!(end.lock().unwrap().as_ref().unwrap().warning.is_some());
    }
}
