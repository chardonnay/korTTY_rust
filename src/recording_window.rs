use std::fmt;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iced::widget::{
    button, canvas, checkbox, column, container, image, pick_list, row, scrollable, text,
    text_input,
};
use iced::{Element, Length, Task};
use kortty_recording::{Replay, ReplayFrame};

use crate::private_files::FileStamp;

mod catalog;
mod java_import;
pub(crate) mod manager;
pub(crate) mod preferences;
pub(crate) mod render;
pub(crate) mod screenshots;
mod tiles;
mod timeline;
mod video_export;

const MAX_PATH: usize = 4096;
pub(crate) const MANAGER_TITLE: &str = "Video Manager";
const SPEEDS: [u8; 20] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
];

#[derive(Clone, Debug)]
pub enum Message {
    Close,
    Manager(manager::Message),
    Opened(Result<Opened, String>),
    ScrubStarted,
    Scrub(f64),
    ScrubFinished,
    PlayPause,
    Stop,
    Speed(u8),
    JumpText(String),
    Jump,
    PanePage(usize),
    Sought(Result<Option<Decoded>, String>),
    StepFrame(Duration),
    ShowPng,
    HidePng,
    PngLoaded(Result<PngPreview, String>),
    ExportPath(String),
    ExportStart(String),
    ExportEnd(String),
    ExportFormat(video_export::Format),
    ExportColors(bool),
    Export,
    ExportPng,
    Exported(Result<PathBuf, String>),
    CancelRangeExport,
    RangeProgress(u64, video_export::Progress),
    RangeExported(u64, Result<video_export::Saved, String>),
}

#[derive(Clone)]
pub struct Opened {
    source: Arc<Mutex<Replay>>,
    source_stamp: Option<FileStamp>,
    first: Option<Decoded>,
    duration: Duration,
    frame_count: usize,
    pane_count: usize,
    complete: bool,
}

impl fmt::Debug for Opened {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Opened")
            .field("duration", &self.duration)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct Decoded {
    frame: Arc<ReplayFrame>,
    index: usize,
    previous: Option<Duration>,
    next: Option<Duration>,
    tiles: tiles::Page,
}

#[derive(Clone, Debug)]
pub struct PngPreview {
    handle: image::Handle,
    width: u32,
    height: u32,
}

impl fmt::Debug for Decoded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Decoded")
            .field("time", &self.frame.playback_at)
            .finish_non_exhaustive()
    }
}

impl Decoded {
    fn covers(&self, at: Duration) -> bool {
        self.frame.playback_at <= at && self.next.is_none_or(|next| at < next)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Job {
    Open,
    Seek {
        at: Duration,
        page: usize,
        preview: bool,
        superseded: bool,
    },
    Screenshot {
        show: bool,
    },
    Export,
    ExportPng,
    ExportRange,
}

pub struct RecordingWindow {
    manager: Option<manager::Manager>,
    path: Option<PathBuf>,
    export_only: bool,
    source: Option<Arc<Mutex<Replay>>>,
    source_stamp: Option<FileStamp>,
    shown: Option<Decoded>,
    journal_screenshots: Option<PathBuf>,
    png: Option<PngPreview>,
    duration: Duration,
    frame_count: usize,
    pane_count: usize,
    pane_page: usize,
    position: Duration,
    playback: Option<(Instant, Duration)>,
    scrub_resume: Option<bool>,
    speed: u8,
    jump: String,
    export: String,
    export_start: String,
    export_end: String,
    video: video_export::State,
    job: Option<Job>,
    notice: Option<String>,
    canvas: canvas::Cache,
    tile_canvases: [canvas::Cache; tiles::PAGE_SIZE],
}

impl RecordingWindow {
    fn new(path: Option<PathBuf>) -> Self {
        Self {
            manager: None,
            path,
            export_only: false,
            source: None,
            source_stamp: None,
            shown: None,
            journal_screenshots: None,
            png: None,
            duration: Duration::ZERO,
            frame_count: 0,
            pane_count: 0,
            pane_page: 0,
            position: Duration::ZERO,
            playback: None,
            scrub_resume: None,
            speed: 1,
            jump: String::new(),
            export: String::new(),
            export_start: String::new(),
            export_end: String::new(),
            video: video_export::State::default(),
            job: None,
            notice: None,
            canvas: canvas::Cache::new(),
            tile_canvases: std::array::from_fn(|_| canvas::Cache::new()),
        }
    }

    pub(crate) fn manager(
        settings: &manager::Settings,
        allowed: bool,
        notice: Option<String>,
        selected: Option<PathBuf>,
    ) -> Self {
        let mut window = Self::new(None);
        let mut manager = manager::Manager::new(settings, allowed, notice);
        manager.preselect(selected);
        window.manager = Some(manager);
        window
    }

    pub(crate) fn replay(path: PathBuf, export_only: bool) -> Self {
        let mut window = Self::new(Some(path));
        window.export_only = export_only;
        window
    }

    pub(crate) fn manager_mut(&mut self) -> Option<&mut manager::Manager> {
        self.manager.as_mut()
    }

    pub(crate) fn manager_state(&self) -> Option<&manager::Manager> {
        self.manager.as_ref()
    }

    pub(crate) fn set_video_encoder(&mut self, executable: Option<video_export::Selection>) {
        if self.path.is_some() && self.journal_screenshots.is_none() {
            self.video.configure(executable);
        }
    }

    pub(crate) fn take_recording_job_done(&mut self) -> Option<async_channel::Receiver<()>> {
        self.manager
            .as_mut()
            .and_then(manager::Manager::take_import_done)
            .or_else(|| self.video.take_done())
    }

    pub(crate) fn cancel_recording_jobs(&mut self) {
        if let Some(manager) = &mut self.manager {
            manager.cancel_import();
        }
        self.video.cancel();
    }

    pub(crate) fn is_manager(&self) -> bool {
        self.manager.is_some()
    }

    pub(crate) fn is_export(&self) -> bool {
        self.export_only
    }

    pub(crate) fn title(&self) -> &'static str {
        if self.is_manager() {
            MANAGER_TITLE
        } else if self.journal_screenshots.is_some() {
            "Journal Recording"
        } else if self.export_only {
            "Export options"
        } else {
            "Replay viewer"
        }
    }

    pub fn journal(path: PathBuf) -> Self {
        let directory = screenshots::attachment_dir(&path);
        let mut window = Self::new(Some(path));
        window.journal_screenshots = Some(directory);
        window
    }

    pub fn initialize(&mut self) -> Task<Message> {
        if let Some(manager) = self.manager.as_mut() {
            return manager.refresh().map(Message::Manager);
        }
        let Some(path) = self.path.clone() else {
            return Task::none();
        };
        self.job = Some(Job::Open);
        Task::future(async move { Message::Opened(open_file(&path)) })
    }

    pub fn playing(&self) -> bool {
        self.playback.is_some()
    }

    pub fn tick(&mut self, now: Instant) -> Task<Message> {
        let Some((started, position)) = self.playback else {
            return Task::none();
        };
        self.position = (position + now.saturating_duration_since(started) * u32::from(self.speed))
            .min(self.duration);
        if self.position == self.duration {
            self.playback = None;
        }
        self.seek()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        if let Message::Manager(message) = message {
            return self.manager.as_mut().map_or_else(Task::none, |manager| {
                manager.update(message).map(Message::Manager)
            });
        }
        match message {
            Message::Close => {}
            Message::Opened(result) if self.job == Some(Job::Open) => {
                self.job = None;
                match result {
                    Ok(opened) => {
                        self.source = Some(opened.source);
                        self.source_stamp = opened.source_stamp;
                        self.shown = opened.first;
                        self.png = None;
                        self.duration = opened.duration;
                        self.frame_count = opened.frame_count;
                        self.pane_count = opened.pane_count;
                        self.position = Duration::ZERO;
                        self.canvas.clear();
                        self.notice = if !opened.complete {
                            Some("This recording has no closing marker. Complete frames are available.".into())
                        } else if self.shown.is_none() {
                            Some("This recording contains no terminal frames.".into())
                        } else {
                            None
                        };
                    }
                    Err(error) => self.notice = Some(error),
                }
            }
            Message::ScrubStarted if self.transport_available() && self.scrub_resume.is_none() => {
                self.scrub_resume = Some(self.playback.take().is_some());
                self.set_seek_target(self.position);
            }
            Message::Scrub(seconds) => {
                if let Ok(at) = Duration::try_from_secs_f64(seconds) {
                    return self.set_position(at);
                }
            }
            Message::ScrubFinished => {
                if self.scrub_resume.take() == Some(true)
                    && self.transport_available()
                    && self.position < self.duration
                {
                    self.playback = Some((Instant::now(), self.position));
                    return self.seek();
                }
            }
            Message::PlayPause if self.transport_available() && self.scrub_resume.is_none() => {
                if self.playing() {
                    self.playback = None;
                } else {
                    if self.position >= self.duration {
                        self.set_seek_target(Duration::ZERO);
                    }
                    self.playback = Some((Instant::now(), self.position));
                    return self.seek();
                }
            }
            Message::Stop if self.transport_available() => {
                self.playback = None;
                self.scrub_resume = None;
                return self.set_position(Duration::ZERO);
            }
            Message::Speed(speed) if SPEEDS.contains(&speed) => {
                self.speed = speed;
                if self.playing() {
                    self.playback = Some((Instant::now(), self.position));
                }
            }
            Message::JumpText(value) => self.jump = bounded(value, 40),
            Message::Jump => match parse_time(&self.jump, self.duration) {
                Some(at) => return self.set_position(at),
                None => self.notice = Some("Enter minutes or MM:SS within the recording.".into()),
            },
            Message::PanePage(page)
                if self.transport_available()
                    && page < self.pane_count.div_ceil(tiles::PAGE_SIZE) =>
            {
                self.pane_page = page;
                self.set_seek_target(self.position);
                return self.seek();
            }
            Message::Sought(result)
                if let Some(Job::Seek {
                    preview,
                    superseded,
                    ..
                }) = self.job =>
            {
                self.job = None;
                if superseded && !matches!(&result, Ok(Some(frame)) if self.shows_target(frame)) {
                    return self.seek();
                }
                match result {
                    Ok(frame) => {
                        if let Some(frame) = &frame {
                            frame.tiles.clear_changed_caches(
                                self.shown.as_ref().map(|shown| &shown.tiles),
                                &self.tile_canvases,
                            );
                        }
                        self.shown = frame;
                        self.png = None;
                        self.canvas.clear();
                        return if preview && !superseded {
                            self.show_png()
                        } else {
                            self.seek()
                        };
                    }
                    Err(error) => {
                        self.playback = None;
                        self.scrub_resume = None;
                        self.notice = Some(error);
                    }
                }
            }
            Message::StepFrame(at) if self.ready() => {
                self.position = at.min(self.duration);
                self.playback = None;
                self.scrub_resume = None;
                return self.seek_frame(self.png.is_some());
            }
            Message::ShowPng => return self.show_png(),
            Message::HidePng => {
                self.png = None;
                self.notice = None;
                match &mut self.job {
                    Some(Job::Seek { preview, .. }) => *preview = false,
                    Some(Job::Screenshot { show }) => *show = false,
                    _ => {}
                }
            }
            Message::PngLoaded(result) if let Some(Job::Screenshot { show }) = self.job => {
                self.job = None;
                if show {
                    match result {
                        Ok(png) => self.png = Some(png),
                        Err(error) => self.notice = Some(error),
                    }
                }
            }
            Message::ExportPath(path) => self.export = bounded(path, MAX_PATH),
            Message::ExportStart(value) => self.export_start = bounded(value, 40),
            Message::ExportEnd(value) => self.export_end = bounded(value, 40),
            Message::ExportFormat(format)
                if self.ready() && (!format.needs_encoder() || self.video.executable.is_some()) =>
            {
                self.video.format = format;
            }
            Message::ExportColors(include_color) if self.ready() => {
                self.video.colorless = !include_color
            }
            Message::Export => return self.export(),
            Message::ExportPng => return self.export_png(),
            Message::Exported(result) if matches!(self.job, Some(Job::Export | Job::ExportPng)) => {
                self.job = None;
                self.notice = Some(match result {
                    Ok(path) => format!("Exported {}", path.display()),
                    Err(error) => error,
                });
                return self.seek();
            }
            Message::CancelRangeExport if self.job == Some(Job::ExportRange) => self.video.cancel(),
            Message::RangeProgress(ticket, progress) => self.video.progress(ticket, progress),
            Message::RangeExported(ticket, result)
                if self.job == Some(Job::ExportRange) && self.video.finish(ticket) =>
            {
                self.job = None;
                self.notice = Some(match result {
                    Ok(saved) => format!(
                        "Exported {}.{}",
                        saved.path.display(),
                        if saved.sync_failed {
                            " The export was saved, but directory synchronization failed."
                        } else {
                            ""
                        },
                    ),
                    Err(error) => error,
                });
                return self.seek();
            }
            _ => {}
        }
        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        if let Some(manager) = &self.manager {
            return manager.view().map(Message::Manager);
        }
        let title = self
            .path
            .as_deref()
            .and_then(Path::file_name)
            .map_or_else(|| self.title().into(), |name| name.to_string_lossy());
        let header = row![
            text(title).size(16).width(Length::Fill),
            button("Close").on_press(Message::Close),
        ]
        .align_y(iced::Alignment::Center);
        let mut main = column![header].spacing(7).padding(12);
        if self.export_only {
            main = main.push(text("Choose a recording range to export").size(15));
            if self.source.is_some() {
                main = main.push(
                    text(format!("Recorded duration: {}", format_time(self.duration))).size(13),
                );
            }
            main = main.push(self.export_controls(false));
            return self.status(main).into();
        }
        let ready = self.ready();
        let available = self.transport_available();
        let mut transport = row![
            button(if self.playing() { "Pause" } else { "Play" }).on_press_maybe(
                (available && self.scrub_resume.is_none()).then_some(Message::PlayPause)
            ),
            button("Stop").on_press_maybe(available.then_some(Message::Stop)),
            text("Speed"),
            pick_list(SPEEDS, Some(self.speed), Message::Speed),
            text("×"),
            text(format!(
                "{} / {}",
                format_time(self.position),
                format_time(self.duration)
            )),
            text_input("MM:SS or minutes", &self.jump)
                .on_input(Message::JumpText)
                .on_submit_maybe(available.then_some(Message::Jump))
                .width(150),
            button("Jump").on_press_maybe(available.then_some(Message::Jump)),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(8);
        if let Some(shown) = &self.shown {
            transport = transport
                .push(text(format!("Frame {} / {}", shown.index + 1, self.frame_count)).size(13));
            if self.journal_screenshots.is_some() {
                let jump = |time: Option<Duration>| time.filter(|_| ready).map(Message::StepFrame);
                transport = transport
                    .push(button("Previous frame").on_press_maybe(jump(shown.previous)))
                    .push(button("Next frame").on_press_maybe(jump(shown.next)))
                    .push(
                        if self.png.is_some()
                            || matches!(self.job, Some(Job::Screenshot { show: true }))
                        {
                            button("Back to replay").on_press(Message::HidePng)
                        } else {
                            button("View PNG").on_press_maybe(ready.then_some(Message::ShowPng))
                        },
                    );
            }
        }
        main = main.push(transport.wrap());
        if available {
            main = main.push(timeline::view(self.position, self.duration));
            let pages = self.pane_count.div_ceil(tiles::PAGE_SIZE);
            if pages > 1 {
                main = main.push(
                    row![
                        button("Previous panes")
                            .on_press_maybe(self.pane_page.checked_sub(1).map(Message::PanePage)),
                        text(format!("Pane page {} / {pages}", self.pane_page + 1)).size(13),
                        button("Next panes").on_press_maybe(
                            (self.pane_page + 1 < pages)
                                .then_some(Message::PanePage(self.pane_page + 1))
                        ),
                    ]
                    .align_y(iced::Alignment::Center)
                    .spacing(8),
                );
            }
        }
        if let Some(preview) = &self.png {
            main = main.push(
                scrollable(
                    image(preview.handle.clone())
                        .width(preview.width as f32)
                        .height(preview.height as f32),
                )
                .direction(scrollable::Direction::Both {
                    horizontal: scrollable::Scrollbar::default(),
                    vertical: scrollable::Scrollbar::default(),
                })
                .width(Length::Fill)
                .height(Length::Fill),
            );
        } else if let Some(shown) = &self.shown {
            main = if self.pane_count > 1 {
                main.push(shown.tiles.view(&self.tile_canvases))
            } else {
                main.push(render::view(&shown.frame.screen, &self.canvas))
            };
        } else {
            main =
                main.push(container(text("No terminal frame is available.")).height(Length::Fill));
        }
        main = main.push(self.export_controls(true));
        self.status(main).height(Length::Fill).into()
    }

    fn export_controls(&self, include_png: bool) -> Element<'_, Message> {
        let ready = self.ready();
        let path_hint = match self.video.format {
            video_export::Format::Native => "Absolute export path",
            video_export::Format::Java => "Absolute .korttyrec.jsonl.gz export path",
            video_export::Format::Webm => "Absolute .webm export path",
            video_export::Format::Mkv => "Absolute .mkv export path",
        };
        let mut export = row![
            text_input(path_hint, &self.export).on_input(Message::ExportPath),
            text_input("From (optional)", &self.export_start)
                .on_input(Message::ExportStart)
                .width(125),
            text_input("To (optional)", &self.export_end)
                .on_input(Message::ExportEnd)
                .width(125),
            button("Export range")
                .on_press_maybe((ready && self.transport_available()).then_some(Message::Export)),
        ]
        .spacing(6);
        if include_png {
            export = export.push(
                button("Save frame PNG")
                    .on_press_maybe((ready && self.shown.is_some()).then_some(Message::ExportPng)),
            );
        }
        let mut controls = column![export.wrap()].spacing(5);
        let options = if self.video.executable.is_some() {
            video_export::Format::ALL
        } else {
            video_export::Format::WITHOUT_ENCODER
        };
        let format: Element<'_, Message> = if ready {
            pick_list(options, Some(self.video.format), Message::ExportFormat).into()
        } else {
            text(self.video.format.to_string()).into()
        };
        let color = (self.video.format != video_export::Format::Native).then(|| {
            checkbox(!self.video.colorless)
                .label("Include terminal colors and styles")
                .on_toggle_maybe(ready.then_some(Message::ExportColors))
        });
        controls = controls.push(
            row![format, color]
                .spacing(7)
                .align_y(iced::Alignment::Center),
        );
        if self.video.format == video_export::Format::Java && self.pane_count > 1 {
            controls = controls
                .push(text("Java replay combines the recorded panes into a single grid.").size(13));
        } else if self.video.executable.is_none() && self.export_only {
            controls = controls.push(
                text(
                    "Configure a local FFmpeg executable in Video Manager to enable video export.",
                )
                .size(13),
            );
        }
        if self.video.cancelable() {
            controls = controls.push(button("Cancel export").on_press(Message::CancelRangeExport));
        }
        controls.into()
    }

    fn status<'a>(
        &'a self,
        mut main: iced::widget::Column<'a, Message>,
    ) -> iced::widget::Column<'a, Message> {
        if let Some(notice) = &self.notice {
            main = main.push(text(notice).size(13));
        }
        if let Some(job) = self.job {
            main = main.push(
                text(match job {
                    Job::Open => "Opening recording…",
                    Job::Seek { .. } => "Loading frame…",
                    Job::Screenshot { .. } => "Opening terminal PNG…",
                    Job::Export => "Exporting recording…",
                    Job::ExportPng => "Exporting terminal PNG…",
                    Job::ExportRange => "Exporting recording range…",
                })
                .size(13),
            );
        }
        if let Some(status) = self.video.status() {
            main = main.push(text(status).size(13));
        }
        main
    }

    fn ready(&self) -> bool {
        self.job.is_none()
    }

    fn transport_available(&self) -> bool {
        self.duration != Duration::ZERO
            && self.png.is_none()
            && !matches!(self.job, Some(Job::Screenshot { .. }))
    }

    fn shows_target(&self, frame: &Decoded) -> bool {
        frame.tiles.number == self.pane_page && frame.covers(self.position)
    }

    fn set_position(&mut self, at: Duration) -> Task<Message> {
        if !self.transport_available() {
            return Task::none();
        }
        self.set_seek_target(at.min(self.duration));
        if self.position == self.duration {
            self.playback = None;
        } else if self.playing() {
            self.playback = Some((Instant::now(), self.position));
        }
        self.seek()
    }

    fn set_seek_target(&mut self, position: Duration) {
        self.position = position;
        // Only user navigation supersedes a decode. Regular playback ticks may advance
        // during decoding and can still display that intermediate frame.
        if let Some(Job::Seek {
            at,
            page,
            superseded,
            ..
        }) = &mut self.job
        {
            *superseded = position != *at || self.pane_page != *page;
        }
    }

    fn seek(&mut self) -> Task<Message> {
        self.seek_frame(false)
    }

    fn seek_frame(&mut self, preview: bool) -> Task<Message> {
        if !self.ready()
            || self
                .shown
                .as_ref()
                .is_none_or(|frame| self.shows_target(frame))
        {
            return Task::none();
        }
        let Some(source) = self.source.clone() else {
            return Task::none();
        };
        let at = self.position;
        let page = self.pane_page;
        let previous = self.shown.clone();
        self.job = Some(Job::Seek {
            at,
            page,
            preview,
            superseded: false,
        });
        Task::future(async move {
            Message::Sought(decode_page(
                &mut source.lock().unwrap_or_else(|error| error.into_inner()),
                at,
                page,
                previous.as_ref(),
            ))
        })
    }

    fn export(&mut self) -> Task<Message> {
        if !self.ready() {
            return Task::none();
        }
        let Some(source) = self.source.clone() else {
            return Task::none();
        };
        let Some(path) = absolute(&self.export) else {
            self.notice = Some("Enter an absolute export path.".into());
            return Task::none();
        };
        let start = if self.export_start.trim().is_empty() {
            Some(Duration::ZERO)
        } else {
            parse_time(&self.export_start, self.duration)
        };
        let end = if self.export_end.trim().is_empty() {
            Some(self.duration)
        } else {
            parse_time(&self.export_end, self.duration)
        };
        let (Some(start), Some(end)) = (start, end) else {
            self.notice = Some("Enter a valid export range in minutes or MM:SS.".into());
            return Task::none();
        };
        self.playback = None;
        self.scrub_resume = None;
        if self.video.format != video_export::Format::Native {
            let (Some(source_path), Some(source_stamp)) =
                (self.path.clone(), self.source_stamp.clone())
            else {
                self.notice =
                    Some("Reopen the recording to verify its source before exporting.".into());
                return Task::none();
            };
            let request = video_export::Request {
                source,
                source_path,
                source_stamp,
                output: path,
                range: start..end,
                format: self.video.format,
                include_color: !self.video.colorless,
                executable: self.video.executable.clone(),
            };
            return match self.video.launch(request) {
                Ok(task) => {
                    self.job = Some(Job::ExportRange);
                    self.notice = None;
                    task
                }
                Err(error) => {
                    self.notice = Some(error);
                    Task::none()
                }
            };
        }
        self.job = Some(Job::Export);
        Task::future(async move {
            let result = source
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .export_range(&path, start..end)
                .map(|()| path)
                .map_err(|error| error.to_string());
            Message::Exported(result)
        })
    }

    fn show_png(&mut self) -> Task<Message> {
        if !self.ready() {
            return Task::none();
        }
        let (Some(directory), Some(shown)) = (self.journal_screenshots.clone(), &self.shown) else {
            return Task::none();
        };
        let index = shown.index;
        self.playback = None;
        self.scrub_resume = None;
        self.notice = None;
        self.job = Some(Job::Screenshot { show: true });
        Task::future(async move { Message::PngLoaded(screenshots::preview(&directory, index)) })
    }

    fn export_png(&mut self) -> Task<Message> {
        if !self.ready() {
            return Task::none();
        }
        let Some(shown) = self.shown.clone() else {
            return Task::none();
        };
        let Some(path) = absolute(&self.export).filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
        }) else {
            self.notice = Some("Enter an absolute export path ending in .png.".into());
            return Task::none();
        };
        let directory = self.journal_screenshots.clone();
        self.playback = None;
        self.scrub_resume = None;
        self.job = Some(Job::ExportPng);
        Task::future(async move {
            let result = screenshots::export(
                directory.as_deref(),
                shown.index,
                &shown.frame.screen,
                &path,
            )
            .map(|()| path);
            Message::Exported(result)
        })
    }
}

fn open_source(path: &Path) -> Result<(Replay, std::fs::Metadata, Option<FileStamp>), String> {
    let file = catalog::open_regular(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    let stamp = FileStamp::read(&file).ok();
    let source = Replay::from_reader(BufReader::new(file)).map_err(|error| error.to_string())?;
    Ok((source, metadata, stamp))
}

fn open_managed(directory: &Path, entry: &catalog::Entry) -> Result<catalog::ManagedFile, String> {
    let (source, metadata, _) = open_source(&entry.path)?;
    entry.manage(directory, &metadata, source.is_complete())
}

fn open_file(path: &Path) -> Result<Opened, String> {
    let (mut source, _, source_stamp) = open_source(path)?;
    let duration = source.duration();
    let frame_count = source.frame_count();
    let pane_count = source.pane_count();
    let complete = source.is_complete();
    let first = decode(&mut source, Duration::ZERO)?;
    Ok(Opened {
        source: Arc::new(Mutex::new(source)),
        source_stamp,
        first,
        duration,
        frame_count,
        pane_count,
        complete,
    })
}

fn decode(source: &mut Replay, at: Duration) -> Result<Option<Decoded>, String> {
    decode_page(source, at, 0, None)
}

fn decode_page(
    source: &mut Replay,
    at: Duration,
    page: usize,
    previous: Option<&Decoded>,
) -> Result<Option<Decoded>, String> {
    let Some(index) = source.frame_index_at(at) else {
        return Ok(None);
    };
    let tiles = tiles::Page::load(source, at, page, previous.map(|frame| &frame.tiles))?;
    let frame = tiles
        .frame(index)
        .or_else(|| {
            previous
                .filter(|frame| frame.index == index)
                .map(|frame| Arc::clone(&frame.frame))
        })
        .map_or_else(|| decode_frame(source, index), Ok)?;
    Ok(Some(Decoded {
        frame,
        index,
        previous: index
            .checked_sub(1)
            .and_then(|previous| source.frame_time(previous)),
        next: source.frame_time(index + 1),
        tiles,
    }))
}

fn decode_frame(source: &mut Replay, index: usize) -> Result<Arc<ReplayFrame>, String> {
    source
        .frame(index)
        .map_err(|error| error.to_string())?
        .map(Arc::new)
        .ok_or_else(|| "Recording frame is unavailable.".into())
}

fn parse_time(text: &str, duration: Duration) -> Option<Duration> {
    let text = text.trim();
    let time = if let Some((minutes, seconds)) = text.split_once(':') {
        let minutes: u64 = minutes.trim().parse().ok()?;
        let seconds: u64 = seconds.trim().parse().ok()?;
        if seconds >= 60 {
            return None;
        }
        Duration::from_secs(minutes.checked_mul(60)?.checked_add(seconds)?)
    } else {
        let minutes: f64 = text.replace(',', ".").parse().ok()?;
        Duration::try_from_secs_f64(minutes * 60.0).ok()?
    };
    (time <= duration).then_some(time)
}

fn format_time(time: Duration) -> String {
    let seconds = time.as_secs();
    format!(
        "{}:{:02}.{:03}",
        seconds / 60,
        seconds % 60,
        time.subsec_millis()
    )
}

fn bounded(mut value: String, bytes: usize) -> String {
    value.truncate(value.floor_char_boundary(bytes));
    value
}

fn absolute(path: &str) -> Option<PathBuf> {
    let path = path.trim();
    let parsed = Path::new(path);
    (parsed.is_absolute() && !path.contains('\0')).then(|| parsed.to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs::{self, FileTimes, OpenOptions};
    use std::io::ErrorKind;
    use std::time::UNIX_EPOCH;

    use kortty_recording::{CaptureOptions, Recorder};
    use kortty_terminal::Terminal;

    use super::*;

    pub(super) struct TestDirectory(pub(super) PathBuf);

    impl TestDirectory {
        pub(super) fn new() -> Self {
            let mut random = [0; 16];
            getrandom::fill(&mut random).unwrap();
            let path = std::env::temp_dir().join(format!(
                "kortty-recording-test-{:032x}",
                u128::from_le_bytes(random)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn capture(path: &Path) -> Recorder {
        let mut recording = Recorder::create(path, CaptureOptions::default()).unwrap();
        let mut terminal = Terminal::new(12, 2);
        terminal.advance(b"test frame");
        recording.start(Duration::ZERO).unwrap();
        recording
            .record_screen(Duration::ZERO, 1, &terminal.snapshot(), (120, 20))
            .unwrap();
        recording
    }

    pub(super) fn completed(path: &Path) {
        capture(path).finish(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn journal_pngs_preserve_screen_colors_and_export_without_overwrite() {
        let directory = TestDirectory::new();
        let recording = directory.0.join("journal.korttyrec");
        let mut recorder = Recorder::create(
            &recording,
            CaptureOptions {
                include_style: true,
                idle_after: None,
            },
        )
        .unwrap();
        let mut terminal = Terminal::new(12, 2);
        terminal.advance(b"\x1b[31;48;2;7;21;233mA\x1b[0m");
        recorder.start(Duration::ZERO).unwrap();
        recorder
            .record_screen(Duration::ZERO, 1, &terminal.snapshot(), (120, 20))
            .unwrap();
        terminal.advance(b"\r\x1b[2K\x1b[32mB\x1b[0m");
        recorder
            .record_screen(Duration::from_secs(1), 1, &terminal.snapshot(), (120, 20))
            .unwrap();
        recorder.finish(Duration::from_secs(2)).unwrap();

        let result = screenshots::attach(&recording, || false).unwrap();
        assert_eq!(result.count, 2);
        assert!(!result.limited);
        let images = screenshots::attachment_dir(&recording);
        let attached = fs::read(images.join("001.png")).unwrap();
        assert!(screenshots::attach(&recording, || false).is_err());
        assert_eq!(fs::read(images.join("001.png")).unwrap(), attached);
        assert!(
            !recording
                .with_extension(&screenshots::STAGE_SUFFIX[1..])
                .exists()
        );
        let pixels = tiny_skia::Pixmap::decode_png(&attached).unwrap();
        let (pixels, _) = pixels.data().as_chunks::<4>();
        assert!(pixels.contains(&[7, 21, 233, 255]));
        assert!(
            pixels
                .iter()
                .any(|pixel| pixel[0] > 150 && pixel[1] < 15 && pixel[2] < 15)
        );
        let preview = screenshots::preview(&images, 0).unwrap();
        assert_eq!((preview.width, preview.height), (113, 46));
        let shown = open_file(&recording).unwrap().first.unwrap();
        let export = directory.0.join("export.png");
        screenshots::export(Some(&images), shown.index, &shown.frame.screen, &export).unwrap();
        assert_eq!(fs::read(&export).unwrap(), attached);
        assert!(screenshots::export(Some(&images), 1, &shown.frame.screen, &export).is_err());
        assert_eq!(fs::read(&export).unwrap(), attached);
        screenshots::remove(&images).unwrap();
        assert!(!images.exists());
    }

    #[test]
    fn jump_accepts_reference_minutes_and_clock_forms_within_duration() {
        let duration = Duration::from_secs(330);
        for (input, seconds) in [("5", 300), ("5:30", 330), ("2.5", 150), ("2,5", 150)] {
            assert_eq!(
                parse_time(input, duration),
                Some(Duration::from_secs(seconds))
            );
        }
        for invalid in ["", "-1", "1:60", "5:31", "1:2:3", "inf", "NaN"] {
            assert!(parse_time(invalid, duration).is_none());
        }
    }

    #[test]
    fn recording_scrub_keeps_latest_requested_frame_and_resumes_from_release() {
        let directory = TestDirectory::new();
        let path = directory.0.join("scrub.korttyrec");
        scrub_recording(&path);
        let mut viewer = RecordingWindow::replay(path.clone(), false);
        let _ = viewer.initialize();
        let _ = viewer.update(Message::Opened(open_file(&path)));
        assert_eq!(viewer.frame_count, 3);

        let _ = viewer.update(Message::PlayPause);
        let _ = viewer.update(Message::ScrubStarted);
        assert!(!viewer.playing());
        assert_eq!(viewer.scrub_resume, Some(true));
        let _ = viewer.update(Message::Scrub(1.0));
        let _ = viewer.update(Message::Scrub(2.0));
        let _ = viewer.tick(Instant::now() + Duration::from_secs(10));
        assert_eq!(viewer.position, Duration::from_secs(2));
        assert!(matches!(viewer.job, Some(Job::Seek { .. })));

        let source = Arc::clone(viewer.source.as_ref().unwrap());
        let first = decode(&mut source.lock().unwrap(), Duration::from_secs(1));
        let _ = viewer.update(Message::Sought(first));
        assert!(matches!(viewer.job, Some(Job::Seek { .. })));
        assert_eq!(viewer.shown.as_ref().unwrap().index, 0);
        let second = decode(&mut source.lock().unwrap(), Duration::from_secs(2));
        let _ = viewer.update(Message::Sought(second));
        assert_eq!(viewer.shown.as_ref().unwrap().index, 2);

        let _ = viewer.update(Message::ScrubFinished);
        let (started, from) = viewer.playback.unwrap();
        assert_eq!(from, Duration::from_secs(2));
        let _ = viewer.tick(started + Duration::from_millis(500));
        assert_eq!(viewer.position, Duration::from_millis(2500));
    }

    #[test]
    fn recording_scrub_ignores_the_error_from_a_superseded_corrupt_frame() {
        let directory = TestDirectory::new();
        let path = directory.0.join("damaged-scrub.korttyrec");
        scrub_recording(&path);
        let mut bytes = fs::read(&path).unwrap();
        let marker = "☃".as_bytes();
        let frame_cell = bytes
            .windows(marker.len())
            .position(|bytes| bytes == marker)
            .unwrap();
        bytes[frame_cell] = 0xff;
        fs::write(&path, bytes).unwrap();

        let mut viewer = RecordingWindow::replay(path.clone(), false);
        let _ = viewer.initialize();
        let _ = viewer.update(Message::Opened(open_file(&path)));
        let source = Arc::clone(viewer.source.as_ref().unwrap());
        let _ = viewer.update(Message::PlayPause);
        let _ = viewer.update(Message::ScrubStarted);
        let _ = viewer.update(Message::Scrub(1.0));
        let _ = viewer.update(Message::Scrub(2.0));
        let failed = decode(&mut source.lock().unwrap(), Duration::from_secs(1));
        assert!(failed.is_err());
        let _ = viewer.update(Message::Sought(failed));
        assert!(viewer.notice.is_none());
        assert_eq!(viewer.scrub_resume, Some(true));
        assert!(matches!(viewer.job, Some(Job::Seek { at, .. }) if at == Duration::from_secs(2)));

        let last = decode(&mut source.lock().unwrap(), Duration::from_secs(2));
        let _ = viewer.update(Message::Sought(last));
        assert_eq!(viewer.shown.as_ref().unwrap().index, 2);
        let _ = viewer.update(Message::ScrubFinished);
        assert!(viewer.playing());
    }

    fn scrub_recording(path: &Path) {
        let mut recorder = capture(path);
        for (seconds, value) in [(1, "☃"), (2, "latest")] {
            let mut terminal = Terminal::new(12, 2);
            terminal.advance(value.as_bytes());
            recorder
                .record_screen(
                    Duration::from_secs(seconds),
                    1,
                    &terminal.snapshot(),
                    (120, 20),
                )
                .unwrap();
        }
        recorder.finish(Duration::from_secs(3)).unwrap();
    }

    #[test]
    fn recording_playback_shows_intermediate_decodes_when_its_clock_advances() {
        let directory = TestDirectory::new();
        let path = directory.0.join("playback.korttyrec");
        scrub_recording(&path);
        let mut viewer = RecordingWindow::replay(path.clone(), false);
        let _ = viewer.initialize();
        let _ = viewer.update(Message::Opened(open_file(&path)));
        let _ = viewer.update(Message::PlayPause);
        let (started, _) = viewer.playback.unwrap();
        let _ = viewer.tick(started + Duration::from_secs(1));
        let _ = viewer.tick(started + Duration::from_secs(2));
        let source = Arc::clone(viewer.source.as_ref().unwrap());
        let first = decode(&mut source.lock().unwrap(), Duration::from_secs(1));
        let _ = viewer.update(Message::Sought(first));
        assert_eq!(viewer.shown.as_ref().unwrap().index, 1);
        assert_eq!(viewer.position, Duration::from_secs(2));
        assert!(matches!(viewer.job, Some(Job::Seek { at, .. }) if at == Duration::from_secs(2)));
    }

    #[test]
    fn recording_pane_page_switch_supersedes_an_in_flight_decode_at_the_same_time() {
        let directory = TestDirectory::new();
        let path = directory.0.join("pane-pages.korttyrec");
        let mut recorder = capture(&path);
        let mut terminal = Terminal::new(12, 2);
        for pane in 2..=5 {
            terminal.advance(b"X");
            recorder
                .record_screen(Duration::ZERO, pane, &terminal.snapshot(), (120, 20))
                .unwrap();
        }
        let at = Duration::from_secs(1);
        recorder
            .record_screen(at, 1, &terminal.snapshot(), (120, 20))
            .unwrap();
        recorder.finish(Duration::from_secs(2)).unwrap();

        let mut viewer = RecordingWindow::replay(path.clone(), false);
        let _ = viewer.initialize();
        let _ = viewer.update(Message::Opened(open_file(&path)));
        assert_eq!(viewer.pane_count, 5);
        assert_eq!(viewer.shown.as_ref().unwrap().tiles.number, 0);

        let source = Arc::clone(viewer.source.as_ref().unwrap());
        let _ = viewer.update(Message::Scrub(at.as_secs_f64()));
        let _ = viewer.update(Message::PanePage(1));
        let old_page = decode(&mut source.lock().unwrap(), at);
        let _ = viewer.update(Message::Sought(old_page));
        assert!(matches!(viewer.job, Some(Job::Seek { page: 1, .. })));
        assert_eq!(viewer.shown.as_ref().unwrap().tiles.number, 0);

        let new_page = decode_page(&mut source.lock().unwrap(), at, 1, None);
        let _ = viewer.update(Message::Sought(new_page));
        let shown = viewer.shown.as_ref().unwrap();
        assert_eq!(viewer.position, at);
        assert_eq!(shown.tiles.number, 1);
        assert_eq!(shown.tiles.frame(4).unwrap().pane, 5);
        assert_eq!(shown.frame.pane, 1);
    }

    #[test]
    fn recording_scrub_stays_paused_after_a_paused_drag_or_release_at_end() {
        let mut viewer = RecordingWindow::new(None);
        viewer.duration = Duration::from_secs(5);
        let _ = viewer.update(Message::ScrubStarted);
        let _ = viewer.update(Message::Scrub(2.0));
        let _ = viewer.update(Message::ScrubFinished);
        assert!(!viewer.playing());
        assert_eq!(viewer.position, Duration::from_secs(2));

        let _ = viewer.update(Message::PlayPause);
        let _ = viewer.update(Message::ScrubStarted);
        let _ = viewer.update(Message::Scrub(5.0));
        let _ = viewer.update(Message::ScrubFinished);
        assert!(!viewer.playing());
        assert_eq!(viewer.position, viewer.duration);
    }

    #[test]
    fn recording_back_to_replay_discards_both_pending_png_stages() {
        let directory = TestDirectory::new();
        let path = directory.0.join("preview.korttyrec");
        scrub_recording(&path);
        let _ = screenshots::attach(&path, || false).unwrap();
        let images = screenshots::attachment_dir(&path);
        let mut viewer = RecordingWindow::journal(path.clone());
        let _ = viewer.initialize();
        let _ = viewer.update(Message::Opened(open_file(&path)));
        let _ = viewer.update(Message::ShowPng);
        let _ = viewer.update(Message::PngLoaded(screenshots::preview(&images, 0)));
        assert!(viewer.png.is_some());

        let _ = viewer.update(Message::StepFrame(Duration::from_secs(1)));
        assert!(matches!(viewer.job, Some(Job::Seek { preview: true, .. })));
        let _ = viewer.update(Message::HidePng);
        let source = Arc::clone(viewer.source.as_ref().unwrap());
        let frame = decode(&mut source.lock().unwrap(), Duration::from_secs(1));
        let _ = viewer.update(Message::Sought(frame));
        assert!(viewer.ready());
        assert!(viewer.png.is_none());
        assert_eq!(viewer.shown.as_ref().unwrap().index, 1);

        let _ = viewer.update(Message::ShowPng);
        assert!(matches!(viewer.job, Some(Job::Screenshot { show: true })));
        let _ = viewer.update(Message::HidePng);
        assert!(matches!(viewer.job, Some(Job::Screenshot { show: false })));
        let result = screenshots::preview(&images, 1);
        assert!(result.is_ok());
        let _ = viewer.update(Message::PngLoaded(result));
        assert!(viewer.ready());
        assert!(viewer.png.is_none());
        assert!(viewer.notice.is_none());
    }

    #[test]
    fn rename_never_replaces_files_and_keeps_playback_open_until_confirmed_delete() {
        use catalog::Operation;

        let directory = TestDirectory::new();
        let original = directory.0.join("original.korttyrec");
        let renamed = directory.0.join("renamed.korttyrec");
        completed(&original);
        let bytes = fs::read(&original).unwrap();
        fs::write(&renamed, b"existing file").unwrap();

        let entry = catalog::list(&directory.0)
            .unwrap()
            .into_iter()
            .find(|entry| entry.path == original)
            .unwrap();
        let managed = open_managed(&directory.0, &entry).unwrap();
        for invalid in ["../outside", "folder\\name", "NUL.txt", "bad\0name"] {
            assert_eq!(
                managed
                    .clone()
                    .change(Operation::Rename(invalid.into()))
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidInput
            );
        }
        assert_eq!(
            managed
                .clone()
                .change(Operation::Rename("renamed".into()))
                .unwrap_err()
                .kind(),
            ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&original).unwrap(), bytes);
        assert_eq!(fs::read(&renamed).unwrap(), b"existing file");
        fs::remove_file(&renamed).unwrap();

        let mut viewer = RecordingWindow::replay(original.clone(), false);
        let _ = viewer.initialize();
        let _ = viewer.update(Message::Opened(open_file(&original)));
        let _ = viewer.update(Message::PlayPause);
        let _ = viewer.update(Message::Scrub(0.5));
        assert!(viewer.playing());
        assert_eq!(viewer.position, Duration::from_millis(500));
        let _ = viewer.update(Message::Stop);
        assert!(!viewer.playing());
        assert_eq!(viewer.position, Duration::ZERO);
        let change = managed
            .change(Operation::Rename("renamed.korttyrec".into()))
            .unwrap();
        assert_eq!(change.renamed.as_ref(), Some(&renamed));
        assert!(!original.exists());
        assert_eq!(fs::read(&renamed).unwrap(), bytes);
        let source = viewer.source.as_ref().unwrap();
        assert!(
            decode(&mut source.lock().unwrap(), Duration::ZERO)
                .unwrap()
                .is_some()
        );

        let entry = catalog::list(&directory.0).unwrap().pop().unwrap();
        open_managed(&directory.0, &entry)
            .unwrap()
            .change(Operation::Delete)
            .unwrap();
        assert!(!renamed.exists());
        assert!(catalog::list(&directory.0).unwrap().is_empty());
    }

    #[test]
    fn external_unfinished_and_replaced_recordings_are_protected() {
        let directory = TestDirectory::new();
        let external = TestDirectory::new();
        let outside = external.0.join("outside.korttyrec");
        completed(&outside);
        let outside = catalog::list(&external.0).unwrap().pop().unwrap();
        assert!(open_managed(&directory.0, &outside).is_err());

        let path = directory.0.join("live.korttyrec");
        let mut recording = capture(&path);
        recording.flush().unwrap();
        assert!(!open_file(&path).unwrap().complete);
        let unfinished = catalog::list(&directory.0).unwrap().pop().unwrap();
        assert!(open_managed(&directory.0, &unfinished).is_err());
        recording.finish(Duration::from_secs(1)).unwrap();
        assert!(open_managed(&directory.0, &unfinished).is_err());

        let entry = catalog::list(&directory.0).unwrap().pop().unwrap();
        let managed = open_managed(&directory.0, &entry).unwrap();
        let moved = directory.0.join("moved.korttyrec");
        fs::rename(&path, &moved).unwrap();
        let mut replacement = capture(&path);
        let mut terminal = Terminal::new(16, 3);
        terminal.advance(b"replacement frame");
        replacement
            .record_screen(
                Duration::from_millis(100),
                1,
                &terminal.snapshot(),
                (160, 30),
            )
            .unwrap();
        replacement.finish(Duration::from_secs(1)).unwrap();
        assert!(open_managed(&directory.0, &entry).is_err());
        assert!(managed.change(catalog::Operation::Delete).is_err());
        assert!(Replay::open(&path).unwrap().is_complete());
        assert!(Replay::open(moved).unwrap().is_complete());
    }

    #[test]
    fn catalog_sorts_by_modified_time_and_skips_other_file_types() {
        let directory = TestDirectory::new();
        let older = directory.0.join("z-old.korttyrec");
        let newer = directory.0.join("a-new.korttyrec");
        for (path, seconds) in [(&older, 10_000_000), (&newer, 20_000_000)] {
            completed(path);
            OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(seconds)))
                .unwrap();
        }
        fs::write(directory.0.join("unrelated.txt"), b"unrelated").unwrap();
        fs::create_dir(directory.0.join("directory.korttyrec")).unwrap();
        #[cfg(unix)]
        {
            let link = directory.0.join("link.korttyrec");
            std::os::unix::fs::symlink(&newer, &link).unwrap();
            assert!(open_file(&link).is_err());
        }
        let paths: Vec<_> = catalog::list(&directory.0)
            .unwrap()
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        assert_eq!(paths, [newer, older]);
    }
}
