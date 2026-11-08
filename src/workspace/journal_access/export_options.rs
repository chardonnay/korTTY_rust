//! Native options for exporting the selected journals.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use iced::Task;
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use zeroize::Zeroizing;

use crate::session_journal::export::{
    DEFAULT_TOLERANCE_MINUTES, Filter, MAX_TOLERANCE_MINUTES, MAX_TOPIC_BYTES, MAX_WINDOWS,
    MarkerMode, Options, Snapshot, TimeWindow, parse_time_of_day,
};
use crate::session_journal::export_output::Format;
use crate::snippets::recipients::Choice as Recipient;
use crate::ssh_dialog::Password;

const MAX_PREVIEW_JOURNALS: usize = 10;
const MAX_PREVIEW_MARKERS: usize = 256;
const TOPIC_DEBOUNCE: Duration = Duration::from_millis(150);
const MAX_PASSWORD_BYTES: usize = crate::snippets::archive::ZipSettings::MAX_PASSWORD_BYTES;

mod view;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub enum Event {
    IncludeScreenshots(bool),
    IncludeChats(bool),
    AddWindow,
    ClearWindows,
    RemoveWindow(u64),
    Bound(u64, Bound, String),
    Tolerance(String),
    StepTolerance(bool),
    Topic(String),
    TopicRegex(bool),
    TopicAi(bool),
    MarkerMode(MarkerMode),
    Marker(String, bool),
    Protection(ProtectionMode),
    Password(Password),
    Repeat(Password),
    Recipient(Recipient),
    Counted(u64, Counted),
    PickRange,
    Confirm,
    Cancel,
}

#[derive(Clone, Copy, Debug)]
pub enum Bound {
    FromDate,
    FromTime,
    ToDate,
    ToTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtectionMode {
    None,
    Password,
    OpenPgp,
}

impl fmt::Display for ProtectionMode {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::None => "No encryption",
            Self::Password => "Protect with a password (ZIP, AES-256)",
            Self::OpenPgp => "Encrypt with GPG",
        })
    }
}

pub(super) enum ProtectionChoice {
    None,
    Password(Zeroizing<String>),
    OpenPgp(Recipient),
}

pub(super) struct Choice {
    pub format: Format,
    pub options: Options,
    pub protection: ProtectionChoice,
    pub ai_topic: bool,
}

#[derive(Clone, Debug)]
pub enum Counted {
    InvalidFilter,
    Valid,
    Entries {
        kept: usize,
        total: usize,
        journals: usize,
    },
    Failed(String),
    WorkerFailed,
    Cancelled,
}

enum PreviewSources {
    Loading,
    Unavailable,
    Loaded(Arc<[Snapshot]>),
    Failed(String),
}

struct PreviewJob {
    revision: u64,
    cancelled: Arc<AtomicBool>,
}

pub(super) struct Draft {
    format: Format,
    can_pick_range: bool,
    journals: usize,
    include_screenshots: bool,
    include_chats: bool,
    windows: Vec<WindowDraft>,
    next_window: u64,
    tolerance: String,
    topic: String,
    topic_regex: bool,
    topic_ai_available: bool,
    topic_ai: bool,
    marker_mode: MarkerMode,
    marker_ids: HashSet<String>,
    markers: Vec<(String, String)>,
    marker_list_limited: bool,
    zone: TimeZone,
    protection: ProtectionMode,
    recipients: Vec<Recipient>,
    recipient: Option<Recipient>,
    password: Password,
    repeat: Password,
    password_too_long: bool,
    repeat_too_long: bool,
    preview_sources: PreviewSources,
    preview_job: Option<PreviewJob>,
    preview: Option<Counted>,
    revision: u64,
    earliest_preview: Instant,
}

impl Draft {
    pub(super) fn new(format: Format, journals: usize, mut recipients: Vec<Recipient>) -> Self {
        recipients.retain(|recipient| !recipient.revoked());
        Self {
            format,
            can_pick_range: false,
            journals,
            include_screenshots: true,
            include_chats: true,
            windows: Vec::new(),
            next_window: 0,
            tolerance: DEFAULT_TOLERANCE_MINUTES.to_string(),
            topic: String::new(),
            topic_regex: false,
            topic_ai_available: false,
            topic_ai: false,
            marker_mode: MarkerMode::All,
            marker_ids: HashSet::new(),
            markers: Vec::new(),
            marker_list_limited: false,
            zone: TimeZone::system(),
            protection: ProtectionMode::None,
            recipients,
            recipient: None,
            password: Password::default(),
            repeat: Password::default(),
            password_too_long: false,
            repeat_too_long: false,
            preview_sources: if (1..=MAX_PREVIEW_JOURNALS).contains(&journals) {
                PreviewSources::Loading
            } else {
                PreviewSources::Unavailable
            },
            preview_job: None,
            preview: None,
            revision: 0,
            earliest_preview: Instant::now(),
        }
    }

    pub(super) fn format(&self) -> Format {
        self.format
    }

    pub(super) fn set_topic_ai_available(&mut self, available: bool) {
        self.topic_ai_available = available;
        if !available {
            self.topic_ai = false;
        }
    }

    pub(super) fn set_viewer_ranges(
        &mut self,
        can_pick_range: bool,
        ranges: &[(Timestamp, Timestamp)],
    ) {
        self.can_pick_range = can_pick_range;
        for &(start, end) in ranges.iter().take(MAX_WINDOWS) {
            let from = start.to_zoned(self.zone.clone());
            let to = end.to_zoned(self.zone.clone());
            self.windows.push(WindowDraft {
                id: self.next_window,
                from_date: from.strftime("%F").to_string(),
                from_time: from.strftime("%R").to_string(),
                to_date: to.strftime("%F").to_string(),
                to_time: to.strftime("%R").to_string(),
            });
            self.next_window += 1;
        }
    }

    pub(super) fn wants_preview(&self) -> bool {
        matches!(self.preview_sources, PreviewSources::Loading)
    }

    pub(super) fn set_preview(&mut self, result: Result<Arc<[Snapshot]>, String>) -> Task<Event> {
        if !self.wants_preview() {
            return Task::none();
        }
        self.preview_sources = match result {
            Ok(snapshots) => {
                let mut seen = HashSet::new();
                for marker in snapshots.iter().flat_map(Snapshot::used_markers) {
                    if !seen.insert(marker.id.as_str()) {
                        continue;
                    }
                    if self.markers.len() == MAX_PREVIEW_MARKERS {
                        self.marker_list_limited = true;
                        break;
                    }
                    self.markers
                        .push((marker.id.clone(), super::label(&marker.name, 120)));
                }
                PreviewSources::Loaded(snapshots)
            }
            Err(problem) => PreviewSources::Failed(problem),
        };
        self.refresh_preview(Duration::ZERO)
    }

    pub(super) fn update(&mut self, event: Event) -> Task<Event> {
        let affects_filter = !matches!(
            &event,
            Event::IncludeScreenshots(_)
                | Event::IncludeChats(_)
                | Event::Protection(_)
                | Event::Password(_)
                | Event::Repeat(_)
                | Event::Recipient(_)
        );
        match event {
            Event::Counted(revision, result) => {
                if self.preview_job.as_ref().map(|job| job.revision) != Some(revision) {
                    return Task::none();
                }
                self.preview_job = None;
                if revision == self.revision {
                    self.preview = Some(result);
                }
                return self.count_if_idle();
            }
            Event::IncludeScreenshots(include) if self.format.offers_screenshots() => {
                self.include_screenshots = include;
            }
            Event::IncludeChats(include) => self.include_chats = include,
            Event::Protection(mode) => {
                self.protection = mode;
                if mode != ProtectionMode::Password {
                    self.password = Password::default();
                    self.repeat = Password::default();
                    self.password_too_long = false;
                    self.repeat_too_long = false;
                }
            }
            Event::Password(value) if self.protection == ProtectionMode::Password => {
                self.password_too_long = value.as_str().len() > MAX_PASSWORD_BYTES;
                self.password = if self.password_too_long {
                    Password::default()
                } else {
                    value
                };
            }
            Event::Repeat(value) if self.protection == ProtectionMode::Password => {
                self.repeat_too_long = value.as_str().len() > MAX_PASSWORD_BYTES;
                self.repeat = if self.repeat_too_long {
                    Password::default()
                } else {
                    value
                };
            }
            Event::Recipient(recipient) if self.protection == ProtectionMode::OpenPgp => {
                if self.recipients.contains(&recipient) {
                    self.recipient = Some(recipient);
                }
            }
            Event::AddWindow if self.windows.len() < MAX_WINDOWS => {
                self.next_window = self.next_window.wrapping_add(1);
                self.windows.push(WindowDraft::new(self.next_window));
            }
            Event::ClearWindows if !self.windows.is_empty() => self.windows.clear(),
            Event::RemoveWindow(id) if self.windows.iter().any(|window| window.id == id) => {
                self.windows.retain(|window| window.id != id);
            }
            Event::Bound(id, bound, value)
                if value.len() <= 24 && !value.chars().any(char::is_control) =>
            {
                let Some(window) = self.windows.iter_mut().find(|window| window.id == id) else {
                    return Task::none();
                };
                *window.field_mut(bound) = value;
            }
            Event::Tolerance(value) if value.len() <= 3 && !value.chars().any(char::is_control) => {
                self.tolerance = value;
            }
            Event::StepTolerance(up) => {
                let tolerance = self.tolerance().unwrap_or(DEFAULT_TOLERANCE_MINUTES);
                let next = if up {
                    (tolerance + 1).min(MAX_TOLERANCE_MINUTES)
                } else {
                    tolerance.saturating_sub(1)
                };
                self.tolerance = next.to_string();
            }
            Event::Topic(value) if value.len() <= MAX_TOPIC_BYTES => {
                self.topic = value;
                return self.refresh_preview(TOPIC_DEBOUNCE);
            }
            Event::TopicRegex(regex) if !self.topic_ai => self.topic_regex = regex,
            Event::TopicAi(enabled) if self.topic_ai_available => {
                self.topic_ai = enabled;
                if enabled {
                    self.topic_regex = false;
                }
            }
            Event::MarkerMode(mode) if mode == MarkerMode::All || !self.markers.is_empty() => {
                self.marker_mode = mode;
            }
            Event::Marker(id, checked)
                if self.marker_mode == MarkerMode::Selected
                    && self.markers.iter().any(|(marker, _)| marker == &id) =>
            {
                if checked {
                    self.marker_ids.insert(id);
                } else {
                    self.marker_ids.remove(&id);
                }
            }
            _ => return Task::none(),
        }
        if affects_filter {
            self.refresh_preview(Duration::ZERO)
        } else {
            Task::none()
        }
    }

    pub(super) fn choice(&self) -> Option<Choice> {
        let filter = self.filter().ok()?;
        if !self.can_confirm(&filter) {
            return None;
        }
        let protection = match self.protection {
            ProtectionMode::None => ProtectionChoice::None,
            ProtectionMode::Password => {
                ProtectionChoice::Password(Zeroizing::new(self.password.as_str().to_owned()))
            }
            ProtectionMode::OpenPgp => ProtectionChoice::OpenPgp(self.recipient.clone()?),
        };
        Some(Choice {
            format: self.format,
            ai_topic: self.uses_ai_topic(&filter),
            options: Options {
                include_screenshots: self.include_screenshots,
                include_chats: self.include_chats,
                filter,
                ..Options::default()
            },
            protection,
        })
    }

    fn uses_ai_topic(&self, filter: &Filter) -> bool {
        self.topic_ai && !filter.topic.is_empty()
    }

    fn can_confirm(&self, filter: &Filter) -> bool {
        if self.journals == 0
            || matches!(self.preview, Some(Counted::InvalidFilter))
            || (needs_regex_check(filter)
                && !matches!(
                    self.preview,
                    Some(Counted::Valid | Counted::Entries { .. } | Counted::Failed(_))
                ))
            || matches!(self.preview, Some(Counted::Entries { kept: 0, journals, .. })
                if journals == self.journals && filter.is_active())
        {
            return false;
        }
        match self.protection {
            ProtectionMode::None => true,
            ProtectionMode::Password => self.passwords_match(),
            ProtectionMode::OpenPgp => self
                .recipient
                .as_ref()
                .is_some_and(|recipient| self.recipients.contains(recipient)),
        }
    }

    fn passwords_match(&self) -> bool {
        let password = self.password.as_str();
        !self.password_too_long
            && !self.repeat_too_long
            && !password.is_empty()
            && password == self.repeat.as_str()
    }

    fn tolerance(&self) -> Option<u32> {
        if !self.tolerance.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        self.tolerance
            .parse()
            .ok()
            .filter(|&minutes| minutes <= MAX_TOLERANCE_MINUTES)
    }

    fn filter(&self) -> Result<Filter, &'static str> {
        let windows = self
            .windows
            .iter()
            .map(WindowDraft::parse)
            .collect::<Result<Vec<_>, _>>()?;
        let marker_mode = if self.marker_mode == MarkerMode::Selected && self.marker_ids.is_empty()
        {
            MarkerMode::All
        } else {
            self.marker_mode
        };
        Ok(Filter {
            windows: windows.into_iter().flatten().collect(),
            tolerance_minutes: self
                .tolerance()
                .ok_or("Enter a tolerance from 0 to 120 minutes.")?,
            topic: self
                .topic
                .trim_matches(|character: char| character <= ' ')
                .to_owned(),
            topic_regex: self.topic_regex,
            marker_mode,
            marker_ids: self.marker_ids.clone(),
            zone: self.zone.clone(),
        })
    }

    fn refresh_preview(&mut self, delay: Duration) -> Task<Event> {
        self.revision = self.revision.wrapping_add(1);
        self.earliest_preview = Instant::now() + delay;
        self.preview = None;
        self.cancel_preview();
        self.count_if_idle()
    }

    fn cancel_preview(&self) {
        if let Some(job) = &self.preview_job {
            job.cancelled.store(true, Ordering::Relaxed);
        }
    }

    fn count_if_idle(&mut self) -> Task<Event> {
        if self.preview_job.is_some() || self.preview.is_some() {
            return Task::none();
        }
        let Ok(filter) = self.filter() else {
            return Task::none();
        };
        if self.uses_ai_topic(&filter) {
            return Task::none();
        }
        let snapshots = match &self.preview_sources {
            PreviewSources::Loaded(snapshots) => Some(Arc::clone(snapshots)),
            _ => None,
        };
        if snapshots.is_none() && !needs_regex_check(&filter) {
            return Task::none();
        }
        let revision = self.revision;
        let earliest = self.earliest_preview;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.preview_job = Some(PreviewJob {
            revision,
            cancelled: Arc::clone(&cancelled),
        });
        Task::future(async move {
            let (send, receive) = async_channel::bounded(1);
            let spawn = std::thread::Builder::new()
                .name("kortty-journal-export-preview".into())
                .spawn(move || {
                    std::thread::sleep(earliest.saturating_duration_since(Instant::now()));
                    let result = count(snapshots.as_deref(), &filter, &cancelled);
                    let _ = send.try_send(result);
                });
            let result = if spawn.is_ok() {
                receive.recv().await.unwrap_or(Counted::WorkerFailed)
            } else {
                Counted::WorkerFailed
            };
            Event::Counted(revision, result)
        })
    }
}

impl Drop for Draft {
    fn drop(&mut self) {
        self.cancel_preview();
    }
}

fn needs_regex_check(filter: &Filter) -> bool {
    filter.topic_regex && !filter.topic.is_empty()
}

fn count(snapshots: Option<&[Snapshot]>, filter: &Filter, cancelled: &AtomicBool) -> Counted {
    if cancelled.load(Ordering::Relaxed) {
        return Counted::Cancelled;
    }
    if filter.validate().is_err() {
        return Counted::InvalidFilter;
    }
    let Some(snapshots) = snapshots else {
        return Counted::Valid;
    };
    let (mut kept, mut total) = (0, 0);
    for snapshot in snapshots {
        if cancelled.load(Ordering::Relaxed) {
            return Counted::Cancelled;
        }
        match snapshot.preview(filter) {
            Ok(preview) => {
                kept += preview.kept_entries;
                total += preview.total_entries;
            }
            Err(problem) => return Counted::Failed(problem.to_string()),
        }
    }
    Counted::Entries {
        kept,
        total,
        journals: snapshots.len(),
    }
}

struct WindowDraft {
    id: u64,
    from_date: String,
    from_time: String,
    to_date: String,
    to_time: String,
}

impl WindowDraft {
    fn new(id: u64) -> Self {
        Self {
            id,
            from_date: String::new(),
            from_time: String::new(),
            to_date: String::new(),
            to_time: String::new(),
        }
    }

    fn field_mut(&mut self, bound: Bound) -> &mut String {
        match bound {
            Bound::FromDate => &mut self.from_date,
            Bound::FromTime => &mut self.from_time,
            Bound::ToDate => &mut self.to_date,
            Bound::ToTime => &mut self.to_time,
        }
    }

    fn parse(&self) -> Result<Option<TimeWindow>, &'static str> {
        let from_date = parse_date(&self.from_date)?;
        let to_date = parse_date(&self.to_date)?;
        if matches!((from_date, to_date), (Some(from), Some(to)) if from > to) {
            return Err("The start date must not be after the end date.");
        }
        let from_time =
            parse_time_of_day(&self.from_time).map_err(|_| "Enter a valid start time.")?;
        let to_time = parse_time_of_day(&self.to_time).map_err(|_| "Enter a valid end time.")?;
        Ok(
            (from_date.is_some() || from_time.is_some() || to_date.is_some() || to_time.is_some())
                .then_some(TimeWindow {
                    from_date,
                    from_time,
                    to_date,
                    to_time,
                }),
        )
    }
}

fn parse_date(value: &str) -> Result<Option<Date>, &'static str> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() != 10
        || !value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 4 | 7) {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
    {
        return Err("Enter dates as YYYY-MM-DD or leave them blank.");
    }
    value
        .parse()
        .map(Some)
        .map_err(|_| "Enter a valid date as YYYY-MM-DD.")
}
