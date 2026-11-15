use std::collections::HashSet;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kortty_jobs::{Cancellation, JournalRecording, RunId, RunOutcome};
use kortty_recording::{CaptureOptions, FILE_EXTENSION, Recorder};
use kortty_ssh::{SshSession, WindowSize};
use kortty_terminal::{ScreenSnapshot, Style, Terminal};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{CANCELLATION_POLL, COMMAND_TIMEOUT, Completion, Failure, check_cancelled};
use crate::clipboard_policy::ClipboardPolicy;
use crate::recording_window::screenshots;
use crate::session_journal::automation::Recorder as JournalRecorder;

mod journal_screen;

const SCREEN_TICK: Duration = Duration::from_millis(500);
const CHANGE_GAP: Duration = Duration::from_secs(2);
const INTERRUPT_GRACE: Duration = Duration::from_secs(2);
const INTERMEDIATE_RECORDING_BUDGET: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    pub columns: u16,
    pub rows: u16,
    pub interval_seconds: u16,
    pub capture_changes: bool,
    pub max_frames: u16,
    pub limit_seconds: u32,
    pub limit_is_success: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            columns: 120,
            rows: 40,
            interval_seconds: 10,
            capture_changes: true,
            max_frames: 30,
            limit_seconds: 0,
            limit_is_success: false,
        }
    }
}

impl Options {
    pub fn validate(self) -> Result<Self, String> {
        if (40..=400).contains(&self.columns)
            && (10..=200).contains(&self.rows)
            && self.interval_seconds <= 3600
            && (1..=500).contains(&self.max_frames)
            && self.limit_seconds <= 86_400
        {
            Ok(self)
        } else {
            Err("Virtual terminal: enter 40–400 columns, 10–200 rows, 0–3600 seconds between frames, 1–500 frames and a 0–86400 second runtime limit".into())
        }
    }

    pub fn size(self) -> WindowSize {
        WindowSize {
            columns: self.columns.into(),
            rows: self.rows.into(),
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

pub(super) fn supervise(
    id: RunId,
    session: &mut SshSession,
    cancellation: &Cancellation,
    options: Options,
    capture: Option<Capture>,
    journal: Option<(&super::automation::Plan, &JournalRecorder)>,
) -> Completion {
    let mut screen = Screen::new(options, capture, journal);
    let (result, limit_detail, drained) = run(session, cancellation, options, &mut screen);
    let mut completion = Completion::from_result(id, result);
    if let Some(detail) = limit_detail {
        completion.append_detail(detail);
    }
    match screen.finish(drained) {
        Ok(Some(capture)) => {
            completion.recording = Some(capture.name);
            completion.append_detail(&format!(
                "Virtual terminal: {} × {}, {} saved frames",
                options.columns, options.rows, capture.frames
            ));
            let cancelled_at_finish = cancellation.is_requested();
            match screenshots::attach(&capture.path, || {
                !cancelled_at_finish && cancellation.is_requested()
            }) {
                Ok(summary) if summary.limited => completion.append_detail(&format!(
                    "{} PNG screenshots saved. Additional frames are available in replay",
                    summary.count
                )),
                Ok(summary) => {
                    completion.append_detail(&format!("{} PNG screenshots saved", summary.count))
                }
                Err(_) => completion
                    .append_detail("Could not attach PNG screenshots. Replay is available"),
            }
        }
        Ok(None) => completion.append_detail("Virtual terminal produced no visible output"),
        Err(_) => completion.append_detail("Could not save the virtual terminal recording"),
    }
    completion
}

fn run(
    session: &mut SshSession,
    cancellation: &Cancellation,
    options: Options,
    screen: &mut Screen<'_>,
) -> (Result<u32, Failure>, Option<&'static str>, bool) {
    let result = read_until_done(session, cancellation, options, screen);
    let exit_code = session.exit_code();
    if !matches!(result, Ok(false)) {
        session.abort_handle().abort();
    }
    let closed = session.close();
    if let Err(error) = check_cancelled(cancellation) {
        return (Err(error), None, false);
    }
    match result {
        Err(failure) => (Err(failure), None, false),
        Ok(true) => {
            if options.limit_is_success {
                (
                    Ok(0),
                    Some("Virtual terminal reached its runtime limit"),
                    false,
                )
            } else {
                (
                    Err(Failure {
                        outcome: RunOutcome::Cancelled,
                        detail: "Virtual terminal reached its runtime limit",
                    }),
                    None,
                    false,
                )
            }
        }
        Ok(false) => (
            exit_code.ok_or_else(|| {
                if closed.is_err() {
                    Failure::failed("SSH virtual terminal transport failed")
                } else {
                    Failure::failed("SSH command ended without an exit status")
                }
            }),
            None,
            closed.is_ok() && exit_code.is_some(),
        ),
    }
}

fn read_until_done(
    session: &mut SshSession,
    cancellation: &Cancellation,
    options: Options,
    screen: &mut Screen<'_>,
) -> Result<bool, Failure> {
    let mut reader = session.take_reader().ok_or_else(super::missing_stream)?;
    let mut writer = session.take_writer().ok_or_else(super::missing_stream)?;
    let configured = Duration::from_secs(u64::from(options.limit_seconds));
    let limit = if configured.is_zero() {
        COMMAND_TIMEOUT
    } else {
        COMMAND_TIMEOUT.min(configured)
    };
    let deadline = screen.started + limit;
    let mut limited = false;
    let mut interrupted = false;
    let mut bytes = [0; 8192];
    loop {
        check_cancelled(cancellation)?;
        let now = Instant::now();
        limited |= now >= deadline;
        let next_deadline = if limited {
            deadline + INTERRUPT_GRACE
        } else {
            deadline
        };
        if now >= next_deadline {
            return Ok(true);
        }
        if limited && !interrupted {
            interrupted = !matches!(writer.write_nonblocking(&[3]), Err(error) if error.kind() == io::ErrorKind::WouldBlock);
        }
        let timeout = (next_deadline - now).min(CANCELLATION_POLL);
        match reader.read_timeout(&mut bytes, timeout) {
            Ok(0) => return Ok(limited),
            Ok(count) => screen.advance(&bytes[..count]),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
            Err(_) => return Err(Failure::failed("SSH virtual terminal output failed")),
        }
        screen.tick(screen.started.elapsed());
    }
}

struct Screen<'a> {
    terminal: Terminal,
    capture: Option<Capture>,
    journal: Option<JournalFrames<'a>>,
    started: Instant,
    last_tick: Duration,
    last_attempt: Duration,
    frames: u16,
    options: Options,
    changed: bool,
    recording_failed: bool,
}

struct JournalFrames<'a> {
    permit: JournalPermit,
    recorder: &'a JournalRecorder,
    frames: u16,
    previous: Option<[u8; 32]>,
}

#[derive(Clone)]
struct JournalPermit {
    plan: super::automation::Plan,
    revoked: Arc<AtomicBool>,
}

impl JournalPermit {
    fn current(&self) -> bool {
        !self.revoked.load(Ordering::Acquire) && self.accepts(&ClipboardPolicy::load())
    }

    fn accepts(&self, policy: &ClipboardPolicy) -> bool {
        if !policy.recording_allowed || !self.plan.can_capture(policy) {
            self.revoked.store(true, Ordering::Release);
        }
        !self.revoked.load(Ordering::Acquire)
    }
}

impl JournalFrames<'_> {
    fn capture(&mut self, terminal: &Terminal, visible: bool, final_frame: bool, drained: bool) {
        if self.frames == 0 && !visible {
            return;
        }
        if !self.permit.current() {
            return;
        }
        let Some(png) = journal_screen::render(terminal, self.recorder.redactor(), drained) else {
            return;
        };
        let digest = Sha256::digest(&png).into();
        if self.previous == Some(digest) {
            return;
        }
        let permit = self.permit.clone();
        if self
            .recorder
            .try_screenshot(png, final_frame, move || permit.current())
            .is_ok()
        {
            self.previous = Some(digest);
            self.frames += 1;
        }
    }
}

struct FinishedCapture {
    name: JournalRecording,
    frames: u16,
    path: PathBuf,
}

impl<'a> Screen<'a> {
    fn new(
        options: Options,
        capture: Option<Capture>,
        journal: Option<(&super::automation::Plan, &'a JournalRecorder)>,
    ) -> Self {
        let mut terminal = Terminal::new(options.columns, options.rows);
        terminal.set_strict_escape_filter(true);
        let journal = journal.map(|(plan, recorder)| {
            super::automation::attach(&mut terminal, recorder, false);
            JournalFrames {
                permit: JournalPermit {
                    plan: plan.clone(),
                    revoked: Arc::default(),
                },
                recorder,
                frames: 0,
                previous: None,
            }
        });
        Self {
            terminal,
            recording_failed: capture.is_none(),
            capture,
            journal,
            started: Instant::now(),
            last_tick: Duration::ZERO,
            last_attempt: Duration::ZERO,
            frames: 0,
            options,
            changed: false,
        }
    }

    fn advance(&mut self, bytes: &[u8]) {
        self.terminal.advance(bytes);
        // This unattended terminal never lets remote output generate input.
        let _ = self.terminal.take_responses();
        self.changed = true;
    }

    fn tick(&mut self, elapsed: Duration) {
        if !self.changed || elapsed - self.last_tick < SCREEN_TICK {
            return;
        }
        self.last_tick = elapsed;
        let interval = Duration::from_secs(u64::from(self.options.interval_seconds));
        let since_attempt = elapsed - self.last_attempt;
        if (!interval.is_zero() && since_attempt >= interval)
            || (self.options.capture_changes && since_attempt >= CHANGE_GAP)
        {
            self.snapshot(elapsed, false, false);
        }
    }

    fn snapshot(&mut self, at: Duration, final_frame: bool, drained: bool) {
        self.last_attempt = at;
        self.changed = false;
        let journal_due = self
            .journal
            .as_ref()
            .is_some_and(|journal| final_frame || journal.frames < self.options.max_frames - 1);
        let replay_due = self.capture.as_ref().is_some_and(|capture| {
            final_frame
                || !self.recording_failed
                    && self.frames < self.options.max_frames - 1
                    && capture.bytes_written() < INTERMEDIATE_RECORDING_BUDGET
        });
        if !journal_due && !replay_due {
            return;
        }
        let screen = self.terminal.snapshot();
        let visible = screen.cells.iter().any(|cell| {
            !cell.style.hidden && !cell.text.trim().is_empty() || cell.style != Style::default()
        });
        if journal_due && let Some(journal) = &mut self.journal {
            journal.capture(&self.terminal, visible, final_frame, drained);
        }
        if replay_due && (visible || self.frames != 0) {
            self.recording_failed = self.record_screen(at, &screen).is_err();
        }
    }

    fn record_screen(
        &mut self,
        at: Duration,
        screen: &ScreenSnapshot,
    ) -> Result<(), kortty_recording::Error> {
        let recorder = self
            .capture
            .as_mut()
            .and_then(|capture| capture.recorder.as_mut())
            .ok_or(kortty_recording::Error::Poisoned)?;
        if recorder.record_screen(at, 1, screen, (0, 0))? {
            self.frames += 1;
        }
        Ok(())
    }

    fn finish(mut self, drained: bool) -> Result<Option<FinishedCapture>, kortty_recording::Error> {
        self.terminal.set_journal_observer(None);
        let now = self.started.elapsed();
        self.snapshot(now, true, drained);
        if self.recording_failed {
            return Err(kortty_recording::Error::Poisoned);
        }
        if self.frames == 0 {
            return Ok(None);
        }
        let capture = self
            .capture
            .as_mut()
            .ok_or(kortty_recording::Error::Poisoned)?;
        capture.finish(now).map(|name| {
            Some(FinishedCapture {
                name,
                frames: self.frames,
                path: capture.path.clone(),
            })
        })
    }
}

pub(super) struct Capture {
    path: PathBuf,
    name: JournalRecording,
    recorder: Option<Recorder>,
    finished: bool,
}

impl Capture {
    pub(super) fn create(directory: &Path, id: RunId) -> io::Result<Self> {
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt as _;
            let mut builder = DirBuilder::new();
            builder.mode(0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = DirBuilder::new();
        match builder.create(directory) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            result => result?,
        }
        if !fs::symlink_metadata(directory)?.is_dir() {
            return Err(io::Error::other(
                "virtual terminal recording directory is unsafe",
            ));
        }
        let mut name = [0; 16];
        getrandom::fill(&mut name).map_err(io::Error::other)?;
        let path = path_for(directory, id, name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        let mut capture = Self {
            path,
            name,
            recorder: None,
            finished: false,
        };
        let mut recorder = Recorder::new(
            BufWriter::new(file),
            CaptureOptions {
                include_style: true,
                idle_after: None,
            },
        )
        .map_err(io::Error::other)?;
        recorder.start(Duration::ZERO).map_err(io::Error::other)?;
        capture.recorder = Some(recorder);
        Ok(capture)
    }

    fn bytes_written(&self) -> u64 {
        self.recorder.as_ref().map_or(0, Recorder::bytes_written)
    }

    fn finish(&mut self, at: Duration) -> Result<JournalRecording, kortty_recording::Error> {
        let recorder = self
            .recorder
            .take()
            .ok_or(kortty_recording::Error::Poisoned)?;
        recorder.finish(at)?.get_ref().sync_all()?;
        #[cfg(unix)]
        {
            let directory = self.path.parent().ok_or(kortty_recording::Error::Invalid(
                "missing recording directory",
            ))?;
            std::fs::File::open(directory)?.sync_all()?;
            let profile = directory.parent().ok_or(kortty_recording::Error::Invalid(
                "missing scheduler profile directory",
            ))?;
            std::fs::File::open(profile)?.sync_all()?;
        }
        self.finished = true;
        Ok(self.name)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if !self.finished {
            self.recorder.take();
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub(crate) fn path_for(directory: &Path, id: RunId, name: JournalRecording) -> PathBuf {
    directory.join(format!(
        "run-{id}-{:032x}.{FILE_EXTENSION}",
        u128::from_be_bytes(name)
    ))
}

pub(crate) fn remove_recording(
    directory: &Path,
    id: RunId,
    name: JournalRecording,
) -> io::Result<()> {
    let result = fs::symlink_metadata(directory).and_then(|metadata| {
        if !metadata.is_dir() {
            return Err(io::Error::other(
                "virtual terminal recording directory is unsafe",
            ));
        }
        let path = path_for(directory, id, name);
        let recording = match fs::remove_file(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        };
        let attachments = screenshots::remove(&screenshots::attachment_dir(&path));
        recording.and(attachments)
    });
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Called only after the exclusive scheduler profile has been acquired, before it starts work.
pub(crate) fn remove_orphans(
    directory: &Path,
    retained: impl Iterator<Item = (RunId, JournalRecording)>,
) {
    if !fs::symlink_metadata(directory).is_ok_and(|metadata| metadata.is_dir()) {
        return;
    }
    let Ok(files) = fs::read_dir(directory) else {
        return;
    };
    let retained: HashSet<_> = retained
        .flat_map(|(id, name)| {
            let path = path_for(directory, id, name);
            let screenshots = screenshots::attachment_dir(&path);
            [path, screenshots]
        })
        .collect();
    for file in files.flatten() {
        let path = file.path();
        if retained.contains(&path) {
            continue;
        }
        let Ok(kind) = file.file_type() else { continue };
        let name = file.file_name();
        let Some(name) = name.to_str() else { continue };
        if kind.is_file()
            && path.extension().is_some_and(|ext| ext == FILE_EXTENSION)
            && path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(managed_name)
        {
            let _ = fs::remove_file(path);
        } else if kind.is_dir()
            && [screenshots::DIRECTORY_SUFFIX, screenshots::STAGE_SUFFIX]
                .into_iter()
                .any(|suffix| name.strip_suffix(suffix).is_some_and(managed_name))
        {
            let _ = screenshots::remove(&path);
        }
    }
}

fn managed_name(stem: &str) -> bool {
    stem.strip_prefix("run-")
        .and_then(|name| name.split_once('-'))
        .is_some_and(|(id, token)| {
            !id.starts_with('0')
                && id.bytes().all(|byte| byte.is_ascii_digit())
                && id.parse::<u64>().is_ok()
                && token.len() == 32
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use crate::session_journal::automation::{Caps, CaptureSettings, Kind, Run};
    use crate::session_journal::tree::RunStatus;
    use crate::snippets::tests::Profile;
    use crate::terminal_logging::Policy;

    use super::*;

    fn plan(profile: &Profile) -> super::super::automation::Plan {
        super::super::automation::Plan::new(
            CaptureSettings {
                root: profile.0.join("journals"),
                format: Default::default(),
                max_parts: None,
                markers: Default::default(),
                stopped: Arc::default(),
            },
            Run::new(
                Kind::Job,
                "nightly".into(),
                "Nightly validation".into(),
                "COMMAND",
                Timestamp::now(),
                crate::session_journal::automation::Options {
                    enabled: true,
                    ..Default::default()
                },
                Caps::default(),
            )
            .unwrap(),
            Policy::default(),
        )
    }

    #[test]
    fn virtual_terminal_journal_permission_stays_revoked_for_queued_frames() {
        let fixture = Profile::new();
        let permit = JournalPermit {
            plan: plan(&fixture),
            revoked: Arc::default(),
        };
        let queued = permit.clone();
        let permitted = ClipboardPolicy::default();
        let denied = crate::clipboard_policy::parse(
            "[meta]\nschema-version = 1\n[[rule]]\n[rule.security]\nallow-terminal-recording = false",
        )
        .unwrap();
        assert!(permit.accepts(&permitted));
        assert!(!queued.accepts(&denied));
        assert!(!permit.accepts(&permitted));
        assert!(!queued.accepts(&permitted));
    }

    #[test]
    fn virtual_terminal_journal_capture_reserves_final_frame_without_replay() {
        let fixture = Profile::new();
        let plan = plan(&fixture);
        let (_, request) = super::super::tests::scheduled_request();
        let recorder = plan
            .start(&request.action, None, None, &ClipboardPolicy::load())
            .unwrap()
            .unwrap();
        recorder.command("printf 'first and final frames'");
        let mut screen = Screen::new(
            Options {
                columns: 40,
                rows: 10,
                interval_seconds: 1,
                capture_changes: false,
                max_frames: 2,
                ..Options::default()
            },
            None,
            Some((&plan, &recorder)),
        );
        screen.advance(b"first phase\r\n");
        screen.tick(Duration::from_millis(500));
        assert_eq!(screen.journal.as_ref().unwrap().frames, 0);
        screen.tick(Duration::from_secs(1));
        assert_eq!(screen.journal.as_ref().unwrap().frames, 1);
        screen.advance(b"last phase\r\n");
        screen.tick(Duration::from_secs(2));
        assert_eq!(screen.journal.as_ref().unwrap().frames, 1);
        assert!(screen.finish(true).is_err());

        let artifact = recorder.finish(RunStatus::Success, Some(0));
        let document = artifact.source.read(None).unwrap().unwrap();
        assert_eq!(
            document
                .timeline
                .iter()
                .filter(|entry| entry.kind == "SCREENSHOT")
                .map(|entry| entry.text.as_str())
                .collect::<Vec<_>>(),
            [
                "Virtual terminal screenshot",
                "Final virtual terminal screenshot"
            ]
        );
        assert!(artifact.complete);
    }

    #[test]
    fn virtual_terminal_journal_change_capture_skips_a_duplicate_final_frame() {
        let fixture = Profile::new();
        let plan = plan(&fixture);
        let (_, request) = super::super::tests::scheduled_request();
        let recorder = plan
            .start(&request.action, None, None, &ClipboardPolicy::load())
            .unwrap()
            .unwrap();
        let mut screen = Screen::new(
            Options {
                columns: 40,
                rows: 10,
                interval_seconds: 0,
                capture_changes: true,
                ..Options::default()
            },
            None,
            Some((&plan, &recorder)),
        );
        screen.advance(b"job output\r\n");
        screen.tick(Duration::from_secs(1));
        assert_eq!(screen.journal.as_ref().unwrap().frames, 0);
        screen.tick(CHANGE_GAP);
        assert_eq!(screen.journal.as_ref().unwrap().frames, 1);
        assert!(screen.finish(true).is_err());

        let artifact = recorder.finish(RunStatus::Success, Some(0));
        let journal = artifact.source.read(None).unwrap().unwrap();
        assert_eq!(
            journal
                .timeline
                .iter()
                .filter(|entry| entry.kind == "SCREENSHOT")
                .count(),
            1
        );
    }
}
