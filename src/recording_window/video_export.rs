//! Checked Java interchange and optional FFmpeg export of native recording ranges.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_channel::Receiver;
use iced::Task;
use kortty_recording::Replay;

use crate::clipboard_policy::ClipboardPolicy;
use crate::private_files::{Directory, FileStamp, nonce};
use crate::profile;

use super::{Message, catalog, preferences::Preferences, render};

mod approval;
mod encoder;
mod java;
#[cfg(test)]
mod tests;

pub(crate) use approval::Selection;
#[cfg(test)]
pub(super) use approval::fixture_encoder;
pub(super) use approval::{fingerprint, resolve_encoder, valid_encoder_path};
mod timing;

const MAX_STAGE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_VIDEO_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RENDER_PIXELS: u64 = 2 * 1024 * 1024 * 1024;
const RENDER_TIMEOUT: Duration = Duration::from_secs(5 * 60);
static EXPORT_RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Format {
    #[default]
    Native,
    Java,
    Webm,
    Mkv,
}

impl Format {
    pub(super) const ALL: &'static [Self] = &[Self::Native, Self::Java, Self::Webm, Self::Mkv];
    pub(super) const WITHOUT_ENCODER: &'static [Self] = &[Self::Native, Self::Java];

    pub(super) fn needs_encoder(self) -> bool {
        matches!(self, Self::Webm | Self::Mkv)
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Native => kortty_recording::FILE_EXTENSION,
            Self::Java => "korttyrec.jsonl.gz",
            Self::Webm => "webm",
            Self::Mkv => "mkv",
        }
    }
}

impl std::fmt::Display for Format {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Native => "Native replay",
            Self::Java => "Java replay (GZIP)",
            Self::Webm => "WebM / VP9",
            Self::Mkv => "MKV / FFV1",
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Progress {
    Preparing,
    Writing { ready: usize, total: usize },
    Rendering { ready: usize, total: usize },
    Encoding { elapsed_ms: u64, total_ms: u64 },
    Publishing,
}

#[derive(Clone, Debug)]
pub struct Saved {
    pub(super) path: PathBuf,
    pub(super) sync_failed: bool,
}

pub(super) struct Request {
    pub(super) source: Arc<Mutex<Replay>>,
    pub(super) source_path: PathBuf,
    pub(super) source_stamp: FileStamp,
    pub(super) output: PathBuf,
    pub(super) range: Range<Duration>,
    pub(super) format: Format,
    pub(super) include_color: bool,
    pub(super) executable: Option<Selection>,
}

#[derive(Default)]
pub(super) struct State {
    pub(super) executable: Option<Selection>,
    pub(super) format: Format,
    pub(super) colorless: bool,
    running: Option<Running>,
    ticket: u64,
}

struct Running {
    format: Format,
    cancel: Option<Cancel>,
    done: Option<Receiver<()>>,
    progress: Progress,
}

struct Cancel(Arc<AtomicBool>);

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

struct Permit;

impl Permit {
    fn acquire() -> Result<Self, String> {
        EXPORT_RUNNING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "Another recording range export is already running.")?;
        Ok(Self)
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        EXPORT_RUNNING.store(false, Ordering::Release);
    }
}

impl State {
    pub(super) fn configure(&mut self, executable: Option<Selection>) {
        if self.executable != executable {
            if self
                .running
                .as_ref()
                .is_some_and(|running| running.format.needs_encoder())
            {
                self.cancel();
            }
            self.executable = executable;
            if self.format.needs_encoder() && self.executable.is_none() {
                self.format = Format::Native;
            }
        }
    }

    pub(super) fn launch(&mut self, request: Request) -> Result<Task<Message>, String> {
        let permit = Permit::acquire()?;
        let format = request.format;
        let canceled = Arc::new(AtomicBool::new(false));
        let cancel = Cancel(Arc::clone(&canceled));
        let (result, receive) = async_channel::bounded(1);
        let (finished, done) = async_channel::bounded(1);
        let (report, progress) = async_channel::bounded(1);
        std::thread::Builder::new()
            .name("kortty-recording-range-export".into())
            .spawn(move || {
                let progress = |value| {
                    let _ = report.try_send(value);
                };
                let exported = if format == Format::Java {
                    java::export(&request, &canceled, progress)
                } else {
                    profile::root().and_then(|root| {
                        export(&request, &root, &canceled, progress, || {
                            authorize(request.executable.as_ref().ok_or_else(|| {
                                invalid("no reviewed FFmpeg executable is available")
                            })?)
                        })
                    })
                };
                let exported = exported.map_err(|error| {
                    if canceled.load(Ordering::Acquire) {
                        "Recording export canceled.".into()
                    } else {
                        format!("Could not export the recording ({error}).")
                    }
                });
                drop(permit);
                let _ = result.try_send(exported);
                drop(finished);
            })
            .map_err(|_| "Could not start the recording export worker.")?;
        self.ticket = self.ticket.wrapping_add(1);
        let ticket = self.ticket;
        self.running = Some(Running {
            format,
            cancel: Some(cancel),
            done: Some(done),
            progress: Progress::Preparing,
        });
        Ok(Task::batch([
            Task::perform(
                async move {
                    receive
                        .recv()
                        .await
                        .unwrap_or_else(|_| Err("The recording export worker stopped.".into()))
                },
                move |result| Message::RangeExported(ticket, result),
            ),
            Task::run(progress, move |progress| {
                Message::RangeProgress(ticket, progress)
            }),
        ]))
    }

    pub(super) fn cancel(&mut self) {
        if let Some(running) = &mut self.running {
            running.cancel = None;
        }
    }

    pub(super) fn take_done(&mut self) -> Option<Receiver<()>> {
        self.running.as_mut()?.done.take()
    }

    pub(super) fn progress(&mut self, ticket: u64, progress: Progress) {
        if ticket == self.ticket
            && let Some(running) = &mut self.running
        {
            running.progress = progress;
        }
    }

    pub(super) fn finish(&mut self, ticket: u64) -> bool {
        ticket == self.ticket && self.running.take().is_some()
    }

    pub(super) fn cancelable(&self) -> bool {
        self.running
            .as_ref()
            .is_some_and(|running| running.cancel.is_some())
    }

    pub(super) fn status(&self) -> Option<String> {
        let running = self.running.as_ref()?;
        Some(if running.cancel.is_none() {
            "Canceling recording export…".into()
        } else {
            match running.progress {
                Progress::Preparing => "Preparing the recording range…".into(),
                Progress::Writing { ready, total } => {
                    format!("Writing Java replay frames · {ready} / {total}")
                }
                Progress::Rendering { ready, total } => {
                    format!("Rendering frames · {ready} / {total}")
                }
                Progress::Encoding {
                    elapsed_ms,
                    total_ms,
                } => format!(
                    "Encoding video · {}%",
                    elapsed_ms.saturating_mul(100) / total_ms.max(1)
                ),
                Progress::Publishing => "Publishing the verified export…".into(),
            }
        })
    }
}

fn authorize(executable: &Selection) -> io::Result<()> {
    let policy = ClipboardPolicy::load();
    if policy.problem.is_some() || !policy.recording_allowed {
        return Err(invalid(
            "video export is disabled by managed recording policy",
        ));
    }
    if Preferences::load()?.encoder().as_ref() != Some(executable) {
        return Err(invalid("the saved FFmpeg configuration changed"));
    }
    Ok(())
}

fn export(
    request: &Request,
    root: &Path,
    canceled: &AtomicBool,
    mut progress: impl FnMut(Progress),
    authorize: impl Fn() -> io::Result<()>,
) -> io::Result<Saved> {
    active(canceled)?;
    authorize()?;
    if !request.format.needs_encoder()
        || !request.output.is_absolute()
        || request
            .output
            .components()
            .any(|part| part == Component::ParentDir)
        || !request
            .output
            .extension()
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case(request.format.extension()))
    {
        return Err(invalid(
            "choose an absolute export path with the selected video extension",
        ));
    }
    let parent = request
        .output
        .parent()
        .ok_or_else(|| invalid("missing video output directory"))?;
    let name = request
        .output
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| invalid("invalid video filename"))?;
    let destination = Directory::open(parent, false)?;
    if destination.has_entry(name)? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "a file already exists at the selected export path",
        ));
    }
    let executable = request
        .executable
        .as_ref()
        .ok_or_else(|| invalid("no reviewed FFmpeg executable is available"))?;
    let encoder = approval::Proof::verify(executable, canceled)?;
    request
        .source_stamp
        .require_unchanged(&catalog::open_regular(&request.source_path)?)?;
    let scratch = Scratch::new(root)?;
    let millis = {
        let mut replay = request
            .source
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let timeline = timing::Timeline::new(&replay, request.range.clone(), canceled)?;
        let layout = timing::Layout::new(&mut replay, &timeline, canceled)?;
        let pixels = u64::from(layout.dimensions.0) * u64::from(layout.dimensions.1);
        if pixels * timeline.samples.len() as u64 > MAX_RENDER_PIXELS {
            return Err(invalid(
                "video range exceeds the total rendered pixel limit",
            ));
        }
        render_frames(
            &mut replay,
            &scratch,
            &layout,
            &timeline,
            request.include_color,
            canceled,
            &mut progress,
        )?;
        timeline.millis
    };
    authorize()?;
    request
        .source_stamp
        .require_unchanged(&catalog::open_regular(&request.source_path)?)?;
    active(canceled)?;
    let (mut input, encoded_stamp) = encoder::run(
        &encoder,
        &scratch.path,
        request.format,
        millis,
        canceled,
        &mut progress,
    )?;
    progress(Progress::Publishing);
    destination.publish_checked_readable(
        name,
        |file| {
            let mut buffer = vec![0; 64 * 1024];
            let mut bytes = 0;
            loop {
                active(canceled)?;
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                bytes += count as u64;
                if bytes > MAX_VIDEO_BYTES {
                    return Err(invalid("encoded video exceeds its size limit"));
                }
                file.write_all(&buffer[..count])?;
            }
            encoded_stamp.require_unchanged(&input)
        },
        || {
            authorize()?;
            encoder.recheck()?;
            request
                .source_stamp
                .require_unchanged(&catalog::open_regular(&request.source_path)?)?;
            if !destination.same_as(&Directory::open(parent, false)?)? {
                return Err(invalid("the selected video output directory changed"));
            }
            active(canceled)
        },
    )?;
    Ok(Saved {
        path: request.output.clone(),
        sync_failed: destination.sync().is_err(),
    })
}

fn render_frames(
    replay: &mut Replay,
    scratch: &Scratch,
    layout: &timing::Layout,
    timeline: &timing::Timeline,
    include_color: bool,
    canceled: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> io::Result<()> {
    let start = Instant::now();
    let total = timeline.samples.len();
    let mut panes = vec![None; replay.pane_count()];
    let mut bytes = 0u64;
    let mut manifest = String::from("ffconcat version 1.0\n");
    for (number, sample) in timeline.samples.iter().enumerate() {
        active(canceled)?;
        if start.elapsed() >= RENDER_TIMEOUT {
            return Err(invalid("video frame rendering timed out"));
        }
        for (pane, index) in panes.iter_mut().zip(timing::indices(replay, sample.at)) {
            if let Some(index) = index
                && pane.as_ref().is_none_or(|(previous, _)| *previous != index)
            {
                *pane = Some((index, timing::decode(replay, index)?));
            }
        }
        let png = render::video_png(
            layout.dimensions,
            layout
                .origins
                .iter()
                .copied()
                .zip(&panes)
                .filter_map(|(origin, pane)| {
                    pane.as_ref().map(|(_, frame)| (origin, &frame.screen))
                }),
            include_color,
        )
        .map_err(io::Error::other)?;
        bytes += png.len() as u64;
        if bytes > MAX_STAGE_BYTES {
            return Err(invalid("video frames exceed the temporary storage limit"));
        }
        let filename = frame_name(number);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(scratch.path.join(&filename))?;
        file.write_all(&png)?;
        let next = timeline
            .samples
            .get(number + 1)
            .map(|next| next.millis)
            .or_else(|| timeline.hold_last().then_some(timeline.millis - 1));
        append_still(
            &mut manifest,
            &filename,
            next.map(|end| end - sample.millis),
        );
        progress(Progress::Rendering {
            ready: number + 1,
            total,
        });
    }
    if timeline.hold_last() {
        append_still(&mut manifest, &frame_name(total - 1), None);
    }
    scratch
        .directory
        .as_ref()
        .expect("active scratch directory")
        .publish_copy("frames.ffconcat", manifest.as_bytes())?;
    active(canceled)
}

fn frame_name(number: usize) -> String {
    format!("frame-{number:06}.png")
}

fn append_still(manifest: &mut String, name: &str, millis: Option<u64>) {
    use std::fmt::Write as _;

    writeln!(manifest, "file {name}\noption framerate 1000").expect("write to string");
    if let Some(millis) = millis {
        writeln!(manifest, "duration {}.{:03}", millis / 1000, millis % 1000)
            .expect("write to string");
    }
}

struct Scratch {
    path: PathBuf,
    directory: Option<Directory>,
}

impl Scratch {
    fn new(root: &Path) -> io::Result<Self> {
        let name = format!(".video-export-{}", nonce()?);
        let directory =
            Directory::open(root, false)?.create_private_subdirectory(OsStr::new(&name))?;
        Ok(Self {
            path: root.join(name),
            directory: Some(directory),
        })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        self.directory = None;
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn active(canceled: &AtomicBool) -> io::Result<()> {
    if canceled.load(Ordering::Acquire) {
        return Err(io::Error::other("video export canceled"));
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
