//! Per-window live session journal beside the terminal tabs.

use std::collections::VecDeque;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use iced::futures::{StreamExt, stream::BoxStream};
use iced::widget::space;
use iced::{Element, Point, Subscription, Task, window};

use crate::session_journal::{
    self, Capture, CaptureRecord, LinkField, LinkTarget, LiveCursor, LivePage, TimelineEntry,
    viewer,
};

use super::journal_access::screenshot_export::Displayed;
use super::journal_appearance_access::Target as AppearanceTarget;
use super::journal_capture_access::{DockSource, Note};
use super::journal_thumbnail::{Key as ImageKey, Loaded as Thumbnails};
use super::{Message as Desktop, Window, Workspace};

mod images;
mod view;

use view::{LinkContext, Panel, PanelActions, State};
pub(super) use view::{Placement, Preferences};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const SAVE_WIDTH_DELAY: Duration = Duration::from_millis(350);
const TERMINAL_RESERVE: f32 = 260.0;
const MIN_VISIBLE_WIDTH: f32 = 150.0;
const MAX_LOG_BYTES: usize = 512 * 1024;
const MAX_LOG_RECORDS: usize = 5000;
const MAX_TIMELINE: usize = 80;

#[derive(Clone, Debug)]
pub enum Message {
    Place(Placement),
    Toggle,
    Resize(Placement, f32),
    FinishResize(Placement),
    Bound(u64, u64, Action),
    Loaded(u64, Result<Arc<ReadResult>, String>),
    ThumbnailVisible(u64, u64, ImageKey, bool),
    ThumbnailsLoaded(Arc<Thumbnails>),
    ScreenshotLoaded(u64, Result<Arc<Displayed>, String>),
}

#[derive(Clone, Debug)]
pub enum Action {
    Note(Note),
    Screenshot,
    Open,
    OpenAgentChat {
        entry: String,
    },
    OpenLink {
        entry: String,
        field: LinkField,
        target: LinkTarget,
    },
    ToggleLog,
    Refresh,
    ShowScreenshot(ImageKey),
    HideScreenshot(ImageKey),
    ScreenshotMenu {
        key: ImageKey,
        full: bool,
        point: Point,
    },
    DismissScreenshotMenu {
        key: ImageKey,
        full: bool,
    },
    StepScreenshotChoice {
        key: ImageKey,
        full: bool,
        forward: bool,
    },
    ChooseScreenshot {
        key: ImageKey,
        full: bool,
        choice: super::journal_image_menu::Choice,
    },
}

pub(super) struct Dock {
    preferences: Preferences,
    last_side: Placement,
    bound: Option<Bound>,
    pending: Option<PendingRead>,
    images: images::Images,
    resized_at: Option<Instant>,
}

impl Dock {
    pub(super) fn update_journal_title(
        &mut self,
        aliases: &[PathBuf],
        expected_id: &str,
        title: &str,
    ) {
        if let Some(bound) = &mut self.bound
            && bound.source.capture.id() == expected_id
            && aliases
                .iter()
                .any(|path| path == bound.source.capture.directory())
        {
            bound.source.title = title.to_owned();
        }
    }
    pub(super) fn new(preferences: Preferences) -> Self {
        Self {
            last_side: match preferences.placement {
                Placement::Hidden => Placement::Right,
                side => side,
            },
            preferences,
            bound: None,
            pending: None,
            images: images::Images::default(),
            resized_at: None,
        }
    }

    fn placement(&self) -> Placement {
        self.preferences.placement
    }

    fn place(&mut self, next: Placement) -> bool {
        if self.placement() == next {
            return false;
        }
        self.preferences.placement = next;
        if next == Placement::Hidden {
            self.bound = None;
            self.invalidate_read();
            self.clear_images();
        } else {
            self.last_side = next;
        }
        self.resized_at = None;
        true
    }

    fn bind(&mut self, source: DockSource) {
        if self
            .bound
            .as_ref()
            .is_some_and(|current| current.source.run == source.run && !current.detached)
        {
            return;
        }
        self.bound = Some(Bound::new(source));
        self.invalidate_read();
        self.clear_images();
    }

    fn invalidate_read(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.current = false;
        }
    }

    fn clear_images(&mut self) {
        self.images.thumbnails.clear();
        if let Some(bound) = &mut self.bound {
            bound.images = images::State::default();
        }
    }
}

#[derive(Clone, Copy)]
struct PendingRead {
    ticket: u64,
    current: bool,
}

struct Bound {
    source: DockSource,
    state: State,
    detached: bool,
    loaded: Option<Revision>,
    refresh: bool,
    timeline: Timeline,
    timeline_error: Option<String>,
    log: Tail,
    log_error: Option<String>,
    log_open: bool,
    notice: Option<String>,
    images: images::State,
}

impl Bound {
    fn new(source: DockSource) -> Self {
        Self {
            source,
            state: State::Live,
            detached: false,
            loaded: None,
            refresh: false,
            timeline: Timeline::default(),
            timeline_error: None,
            log: Tail::default(),
            log_error: None,
            log_open: false,
            notice: None,
            images: images::State::default(),
        }
    }

    fn stamp(&self) -> (u64, u64) {
        (self.source.tab, self.source.run)
    }

    fn revision(&self) -> Revision {
        Revision::of(&self.source.capture)
    }

    fn request(&self) -> Option<ReadRequest> {
        if self.detached {
            return None;
        }
        let revision = self.revision();
        let force = self.refresh || self.loaded.is_none();
        let timeline = force
            || self.loaded.is_some_and(|last| {
                revision.timeline != last.timeline || revision.closed != last.closed
            });
        let tail = force
            || self.log.more
            || self
                .loaded
                .is_some_and(|last| revision.log != last.log || revision.closed != last.closed);
        if !timeline && !tail {
            return None;
        }
        Some(ReadRequest {
            revision,
            timeline,
            tail,
            cursor: (!force).then(|| self.log.cursor.clone()).flatten(),
        })
    }

    fn integrate(&mut self, result: &ReadResult) {
        self.loaded = Some(result.revision);
        if let Some(timeline) = &result.timeline {
            match timeline {
                Ok(timeline) => {
                    self.timeline = timeline.clone();
                    self.images.retain(&self.timeline.entries);
                    self.timeline_error = None;
                }
                Err(error) => self.timeline_error = Some(error.clone()),
            }
        }
        if let Some(tail) = &result.tail {
            match tail {
                Ok(tail) => {
                    self.log.integrate(tail);
                    if self.log_open {
                        self.log.render_text();
                    }
                    self.log_error = None;
                }
                Err(error) => {
                    self.log.more = false;
                    self.log_error = Some(error.clone());
                }
            }
        }
        self.notice = self
            .timeline_error
            .as_ref()
            .or(self.log_error.as_ref())
            .cloned()
            .or_else(|| {
                (self.log.skipped != 0)
                    .then(|| format!("{} unreadable live log records skipped.", self.log.skipped))
            });
    }
}

#[derive(Clone, Default)]
struct Timeline {
    entries: Arc<Vec<TimelineEntry>>,
    limited: bool,
}

#[derive(Default)]
struct Tail {
    records: VecDeque<CaptureRecord>,
    bytes: usize,
    cursor: Option<LiveCursor>,
    text: String,
    skipped: usize,
    more: bool,
}

impl Tail {
    fn integrate(&mut self, page: &LivePage) {
        if page.reset {
            self.records.clear();
            self.bytes = 0;
            self.skipped = 0;
        }
        self.cursor = Some(page.cursor.clone());
        self.more = page.more;
        self.skipped = self.skipped.saturating_add(page.skipped_lines);
        for record in &page.records {
            self.bytes += record.text.len();
            self.records.push_back(record.clone());
            while self.bytes > MAX_LOG_BYTES || self.records.len() > MAX_LOG_RECORDS {
                if let Some(oldest) = self.records.pop_front() {
                    self.bytes -= oldest.text.len();
                }
            }
        }
    }

    fn render_text(&mut self) {
        self.text = view::tail_text(self.records.iter());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Revision {
    log: u64,
    timeline: usize,
    closed: bool,
}

impl Revision {
    fn of(capture: &Capture) -> Self {
        let (log, timeline, closed) = capture.published();
        Self {
            log,
            timeline,
            closed,
        }
    }
}

struct ReadRequest {
    revision: Revision,
    timeline: bool,
    tail: bool,
    cursor: Option<LiveCursor>,
}

pub struct ReadResult {
    revision: Revision,
    timeline: Option<Result<Timeline, String>>,
    tail: Option<Result<LivePage, String>>,
}

impl fmt::Debug for ReadResult {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("LiveJournalRead")
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

impl ReadRequest {
    fn read(self, capture: &session_journal::Capture) -> ReadResult {
        let directory = capture.directory();
        let timeline = self.timeline.then(|| {
            session_journal::read(directory)
                .and_then(|mut document| {
                    if document.summary.id != capture.id() {
                        return Err(std::io::Error::other("The journal changed."));
                    }
                    let older = document.timeline.len().saturating_sub(MAX_TIMELINE);
                    let entries = document.timeline.split_off(older);
                    Ok(Timeline {
                        entries: Arc::new(entries),
                        limited: document.timeline_limited || older != 0,
                    })
                })
                .map_err(|error| format!("Could not read the journal timeline ({error})."))
        });
        let tail = self.tail.then(|| {
            capture
                .live_tail(self.cursor.as_ref())
                .map_err(|error| format!("Could not read the live log ({error})."))
        });
        ReadResult {
            revision: self.revision,
            timeline,
            tail,
        }
    }
}

impl Workspace {
    fn dock_journal_entry_actions_enabled(&self, owner: window::Id, dock: &Dock) -> bool {
        dock.placement() != Placement::Hidden
            && !self.exiting
            && !self.scheduler_draining
            && !self.terminal_only_fullscreen_active(owner)
            && self.journal_allowed()
            && self.palette_can_open(owner)
            && !self.palette.is_open(owner)
    }

    fn dock_journal_links_enabled(&self, owner: window::Id, dock: &Dock) -> bool {
        self.journal_links_allowed() && self.dock_journal_entry_actions_enabled(owner, dock)
    }

    pub(super) fn journal_dock_placement(&self, owner: window::Id) -> Placement {
        self.windows
            .get(&owner)
            .map_or(Placement::Hidden, |window| window.journal_dock.placement())
    }

    pub(super) fn journal_dock_appearance_current(
        &self,
        owner: window::Id,
        tab: u64,
        run: u64,
    ) -> bool {
        self.journal_allowed()
            && !self.exiting
            && !self.scheduler_draining
            && !self.terminal_only_fullscreen_active(owner)
            && self.palette_can_open(owner)
            && !self.palette.is_open(owner)
            && self.windows.get(&owner).is_some_and(|window| {
                let dock = &window.journal_dock;
                dock.placement() != Placement::Hidden
                    && dock
                        .bound
                        .as_ref()
                        .is_some_and(|bound| bound.stamp() == (tab, run))
            })
    }

    pub(super) fn refresh_journal_entry_docks(&mut self, source: &viewer::Source) -> Task<Desktop> {
        let mut changed = false;
        for window in self.windows.values_mut() {
            if let Some(bound) = &mut window.journal_dock.bound
                && viewer::Source::capture(&bound.source.capture).same_journal(source)
            {
                bound.refresh = true;
                changed = true;
            }
        }
        if changed {
            self.sync_journal_docks()
        } else {
            Task::none()
        }
    }

    pub(super) fn journal_dock_subscription(&self) -> Subscription<Desktop> {
        if !self.journal_allowed() || self.exiting || self.scheduler_draining {
            return Subscription::none();
        }
        let mut poll = DockPoll::default();
        for (&owner, window) in &self.windows {
            let dock = &window.journal_dock;
            poll.catch_up |= dock.resized_at.is_some();
            if let Some(bound) = dock.bound.as_ref().filter(|bound| !bound.detached)
                && !self.terminal_only_fullscreen_active(owner)
            {
                let revision = bound.revision();
                poll.catch_up |= dock.pending.is_none()
                    && (bound.loaded != Some(revision) || bound.refresh || bound.log.more);
                if !revision.closed {
                    poll.captures.push(Publication {
                        run: bound.source.run,
                        capture: Arc::clone(&bound.source.capture),
                        seen: revision,
                    });
                }
            }
        }
        if !poll.catch_up && poll.captures.is_empty() {
            return Subscription::none();
        }
        poll.captures.sort_unstable_by_key(|capture| capture.run);
        Subscription::run_with(poll, clock)
    }

    pub(super) fn sync_journal_docks(&mut self) -> Task<Desktop> {
        if !self.journal_allowed() || self.exiting || self.scheduler_draining {
            for window in self.windows.values_mut() {
                window.journal_dock.clear_images();
            }
            return Task::none();
        }
        let fullscreen = self
            .windows
            .keys()
            .copied()
            .filter(|&owner| self.terminal_only_fullscreen_active(owner))
            .collect::<Vec<_>>();
        for owner in fullscreen {
            if let Some(window) = self.windows.get_mut(&owner) {
                window.journal_dock.clear_images();
            }
        }
        let owners: Vec<_> = self
            .windows
            .iter()
            .filter_map(|(&owner, window)| {
                (window.journal_dock.placement() != Placement::Hidden
                    && !self.terminal_only_fullscreen_active(owner))
                .then_some(owner)
            })
            .collect();
        let mut tasks = Vec::new();
        for owner in owners {
            let active = self.active_dock_journal(owner);
            if let Some(active) = active {
                self.windows
                    .get_mut(&owner)
                    .expect("journal owner exists")
                    .journal_dock
                    .bind(active);
            }
            if let Some((tab, run)) = self.windows[&owner]
                .journal_dock
                .bound
                .as_ref()
                .map(Bound::stamp)
            {
                let present = self.windows[&owner]
                    .terminal_tabs()
                    .any(|current| current.id == tab);
                let live = present && self.dock_journal_live(owner, tab, run);
                let moved = !present && self.journal_captures.contains_key(&tab);
                let state = if live {
                    State::Live
                } else if present {
                    State::Stopped
                } else {
                    State::TabClosed
                };
                let dock = &mut self
                    .windows
                    .get_mut(&owner)
                    .expect("journal owner exists")
                    .journal_dock;
                if let Some(bound) = &mut dock.bound {
                    bound.state = state;
                    bound.detached |= moved;
                }
                if moved {
                    // A read started before the move can still observe later writes.
                    dock.invalidate_read();
                }
                tasks.push(self.read_journal_dock(owner));
            }
        }
        Task::batch(tasks)
    }

    pub(super) fn journal_dock_tick(&mut self) -> Task<Desktop> {
        let owners: Vec<_> = self
            .windows
            .iter()
            .filter_map(|(&id, window)| {
                window
                    .journal_dock
                    .resized_at
                    .filter(|at| at.elapsed() >= SAVE_WIDTH_DELAY)
                    .map(|_| id)
            })
            .collect();
        for owner in owners {
            self.persist_journal_dock(owner);
        }
        self.sync_journal_docks()
    }

    pub(super) fn flush_journal_dock_resize(&mut self, owner: window::Id) {
        if self
            .windows
            .get(&owner)
            .is_some_and(|window| window.journal_dock.resized_at.is_some())
        {
            self.persist_journal_dock(owner);
        }
    }

    pub(super) fn flush_last_journal_dock_resize(&mut self) {
        let owner = self
            .windows
            .iter()
            .filter_map(|(&id, window)| window.journal_dock.resized_at.map(|at| (at, id)))
            .max_by_key(|(at, _)| *at)
            .map(|(_, id)| id);
        if let Some(owner) = owner {
            self.persist_journal_dock(owner);
        }
    }

    fn persist_journal_dock(&mut self, owner: window::Id) {
        let Some(window) = self.windows.get_mut(&owner) else {
            return;
        };
        window.journal_dock.resized_at = None;
        self.window_settings.journal_dock = window.journal_dock.preferences;
        self.save_window_settings(Some(owner));
    }

    pub(super) fn update_journal_dock(
        &mut self,
        owner: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        let allowed = self.journal_allowed();
        if !matches!(
            message,
            Message::Loaded(..) | Message::ThumbnailsLoaded(..) | Message::ScreenshotLoaded(..)
        ) && (!allowed || self.terminal_only_fullscreen_active(owner))
        {
            return Task::none();
        }
        let Some(window) = self.windows.get_mut(&owner) else {
            return Task::none();
        };
        let dock = &mut window.journal_dock;
        match message {
            Message::Place(side) => {
                let next = if dock.placement() == side {
                    Placement::Hidden
                } else {
                    side
                };
                if dock.place(next) {
                    self.persist_journal_dock(owner);
                }
            }
            Message::Toggle => {
                let next = if dock.placement() == Placement::Hidden {
                    dock.last_side
                } else {
                    Placement::Hidden
                };
                if dock.place(next) {
                    self.persist_journal_dock(owner);
                }
            }
            Message::Resize(side, width)
                if dock.placement() == side && side != Placement::Hidden && width.is_finite() =>
            {
                let width = view::clamp_width(width);
                if width != dock.preferences.width {
                    dock.preferences.width = width;
                    dock.resized_at = Some(Instant::now());
                }
            }
            Message::FinishResize(side) if dock.placement() == side => {
                self.flush_journal_dock_resize(owner);
            }
            Message::Bound(tab, run, action) => {
                return self.update_bound_journal(owner, tab, run, action);
            }
            Message::ThumbnailVisible(tab, run, key, visible) => {
                return self.set_dock_thumbnail_visibility(owner, tab, run, key, visible);
            }
            Message::ThumbnailsLoaded(loaded) => {
                return self.dock_thumbnails_loaded(owner, &loaded);
            }
            Message::ScreenshotLoaded(ticket, result) => {
                return self.dock_screenshot_loaded(owner, ticket, result);
            }
            Message::Loaded(ticket, result) => {
                let Some(pending) = dock.pending.filter(|read| read.ticket == ticket) else {
                    return Task::none();
                };
                dock.pending = None;
                if let Some(bound) = dock.bound.as_mut().filter(|_| allowed && pending.current) {
                    match result {
                        Ok(result) => bound.integrate(&result),
                        Err(error) => {
                            bound.loaded = Some(bound.revision());
                            bound.log.more = false;
                            bound.notice = Some(error);
                        }
                    }
                }
                return self.request_dock_thumbnails(owner);
            }
            _ => {}
        }
        Task::none()
    }

    fn update_bound_journal(
        &mut self,
        owner: window::Id,
        tab: u64,
        run: u64,
        action: Action,
    ) -> Task<Desktop> {
        if self.owner_dialog(owner).is_some() {
            return Task::none();
        }
        let links_enabled = matches!(&action, Action::OpenLink { .. })
            && self
                .windows
                .get(&owner)
                .is_some_and(|window| self.dock_journal_links_enabled(owner, &window.journal_dock));
        let chat_enabled = matches!(&action, Action::OpenAgentChat { .. })
            && self.windows.get(&owner).is_some_and(|window| {
                self.dock_journal_entry_actions_enabled(owner, &window.journal_dock)
            });
        let Some(dock) = self
            .windows
            .get_mut(&owner)
            .map(|window| &mut window.journal_dock)
        else {
            return Task::none();
        };
        let Some(bound) = dock
            .bound
            .as_mut()
            .filter(|bound| bound.stamp() == (tab, run))
        else {
            return Task::none();
        };
        match action {
            Action::Note(note) => return self.dock_journal_note(owner, tab, run, note),
            Action::Screenshot => return self.dock_journal_screenshot(owner, tab, run),
            Action::Open => {
                let source = viewer::Source::capture(&bound.source.capture);
                let name = bound.source.title.clone();
                return self.open_journal_viewer(owner, source, name);
            }
            Action::OpenAgentChat { entry } => {
                if chat_enabled
                    && bound
                        .timeline
                        .entries
                        .iter()
                        .any(|candidate| candidate.id == entry && candidate.has_agent_chat())
                {
                    let source = viewer::Source::capture(&bound.source.capture);
                    return self.start_journal_agent_chat(owner, source, entry, None);
                }
            }
            Action::OpenLink {
                entry,
                field,
                target,
            } => {
                if links_enabled && visible_link(&bound.timeline.entries, &entry, field, &target) {
                    return self.open_journal_link(owner, target);
                }
            }
            Action::ToggleLog => {
                bound.log_open = !bound.log_open;
                if bound.log_open {
                    bound.log.render_text();
                } else {
                    bound.log.text.clear();
                }
            }
            Action::Refresh if !bound.detached => {
                bound.refresh = true;
                // Retry unavailable images even when the reread has the same timeline revision.
                dock.images.thumbnails.clear();
            }
            Action::Refresh => {}
            Action::ShowScreenshot(_)
            | Action::HideScreenshot(_)
            | Action::ScreenshotMenu { .. }
            | Action::DismissScreenshotMenu { .. }
            | Action::StepScreenshotChoice { .. }
            | Action::ChooseScreenshot { .. } => {
                return self.update_dock_screenshot(owner, tab, run, action);
            }
        }
        Task::none()
    }

    fn read_journal_dock(&mut self, owner: window::Id) -> Task<Desktop> {
        let dock = &self.windows[&owner].journal_dock;
        if dock.pending.is_some() {
            return Task::none();
        }
        let Some(request) = dock.bound.as_ref().and_then(Bound::request) else {
            return Task::none();
        };
        let ticket = self.allocate();
        let dock = &mut self
            .windows
            .get_mut(&owner)
            .expect("journal owner exists")
            .journal_dock;
        let bound = dock.bound.as_mut().expect("checked journal binding");
        bound.refresh = false;
        let capture = Arc::clone(&bound.source.capture);
        let (send, receive) = async_channel::bounded(1);
        let (signal, done) = async_channel::bounded(1);
        let worker = thread::Builder::new()
            .name("kortty-journal-dock-reader".into())
            .spawn(move || {
                let result = request.read(&capture);
                let _ = send.try_send(Arc::new(result));
                drop(signal);
            });
        if let Err(error) = worker {
            bound.loaded = Some(bound.revision());
            bound.log.more = false;
            bound.notice = Some(format!("Could not start the journal reader ({error})."));
            return Task::none();
        }
        dock.pending = Some(PendingRead {
            ticket,
            current: true,
        });
        self.track_session_shutdown(done);
        Task::future(async move {
            let result = receive
                .recv()
                .await
                .map_err(|_| "The journal reader stopped.".to_owned());
            Desktop::JournalDock(owner, Message::Loaded(ticket, result))
        })
    }

    pub(super) fn journal_dock_view<'a>(
        &'a self,
        owner: window::Id,
        window: &'a Window,
        panes: Element<'a, Desktop>,
        fitted_width: Option<f32>,
    ) -> Element<'a, Desktop> {
        let dock = &window.journal_dock;
        let position = if self.journal_allowed() && !self.terminal_only_fullscreen_active(owner) {
            dock.placement()
        } else {
            Placement::Hidden
        };
        let available = window.content_bounds().width;
        let (_, remaining) =
            self.local_dock
                .tab_drop_coordinates(available, 0.0, f64::from(available));
        let width = fitted_width.unwrap_or_else(|| {
            f32::from(dock.preferences.width)
                .min((remaining as f32 - TERMINAL_RESERVE).max(MIN_VISIBLE_WIDTH))
        });
        let dispatch = move |message| Desktop::JournalDock(owner, message);
        let panel = if position == Placement::Hidden {
            space::Space::new().into()
        } else {
            let bound = dock.bound.as_ref();
            let message = move |action| {
                bound.map(|bound| {
                    dispatch(Message::Bound(bound.source.tab, bound.source.run, action))
                })
            };
            let note = bound
                .filter(|bound| self.dock_note_enabled(owner, bound.source.tab, bound.source.run))
                .and_then(|_| message(Action::Note(Note::Edit)));
            let screenshot = bound
                .filter(|bound| {
                    self.dock_screenshot_enabled(owner, bound.source.tab, bound.source.run)
                })
                .and_then(|_| message(Action::Screenshot));
            let panel = bound.map(|bound| {
                let (tab, run) = bound.stamp();
                let editor = self.dock_journal_note_editor(owner, tab, run, move |note| {
                    dispatch(Message::Bound(tab, run, Action::Note(note)))
                });
                Panel {
                    title: &bound.source.title,
                    state: bound.state,
                    timeline: &bound.timeline.entries,
                    timeline_limited: bound.timeline.limited,
                    tail: bound.log_open.then_some(bound.log.text.as_str()),
                    notice: bound.notice.as_deref(),
                    scroll_key: run,
                    note_editor: editor,
                    links: self
                        .dock_journal_links_enabled(owner, dock)
                        .then_some(LinkContext { owner, tab, run }),
                    chat: self
                        .dock_journal_entry_actions_enabled(owner, dock)
                        .then_some(LinkContext { owner, tab, run }),
                    images: images::widget::Context {
                        owner,
                        tab,
                        run,
                        thumbnails: &dock.images.thumbnails,
                        state: &bound.images,
                        enabled: self.dock_screenshot_actions_allowed(owner, tab, run),
                        analysis: self.journal_screenshot_analysis_available(),
                    },
                }
            });
            let appearance = bound.map(|bound| {
                let (tab, run) = bound.stamp();
                let target = AppearanceTarget::Dock { owner, tab, run };
                (target, self.journal_appearance_page(target))
            });
            let more = appearance.as_ref().map(|(target, appearance)| {
                view::more_actions(
                    *target,
                    appearance,
                    bound
                        .filter(|bound| !bound.detached)
                        .and_then(|_| message(Action::Refresh)),
                )
            });
            view::panel(
                panel,
                PanelActions {
                    hide: dispatch(Message::Place(position)),
                    note,
                    screenshot,
                    automatic_screenshot_ai: bound.and_then(|bound| {
                        let (tab, run) = bound.stamp();
                        self.journal_screenshot_ai_run_control(owner, tab, run)
                    }),
                    open_viewer: message(Action::Open),
                    toggle_log: message(Action::ToggleLog),
                    more,
                },
                appearance.map(|(_, appearance)| appearance.content),
            )
        };
        super::local_dock::dock_terminal(
            view::side(position),
            width,
            fitted_width.map_or(width, |_| f32::from(dock.preferences.width)),
            panel,
            panes,
            move |width| dispatch(Message::Resize(position, width)),
            dispatch(Message::FinishResize(position)),
        )
    }

    pub(super) fn visible_journal_dock_width(&self, owner: window::Id, window: &Window) -> f32 {
        if self.journal_allowed()
            && !self.terminal_only_fullscreen_active(owner)
            && window.journal_dock.placement() != Placement::Hidden
        {
            f32::from(window.journal_dock.preferences.width)
        } else {
            0.0
        }
    }
}

fn visible_link(
    entries: &[TimelineEntry],
    id: &str,
    field: LinkField,
    target: &LinkTarget,
) -> bool {
    view::visible_entries(entries).iter().any(|entry| {
        entry.id == id
            && entry
                .link_spans(field, view::CARD_TEXT_LIMIT)
                .iter()
                .any(|span| span.target == Some(target))
    })
}

#[derive(Default)]
struct DockPoll {
    captures: Vec<Publication>,
    catch_up: bool,
}

#[derive(Clone)]
struct Publication {
    run: u64,
    capture: Arc<Capture>,
    seen: Revision,
}

impl Publication {
    fn changed(&mut self) -> bool {
        let revision = Revision::of(&self.capture);
        std::mem::replace(&mut self.seen, revision) != revision
    }
}

impl Hash for DockPoll {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.catch_up.hash(state);
        self.captures.len().hash(state);
        for capture in &self.captures {
            capture.run.hash(state);
        }
    }
}

fn clock(poll: &DockPoll) -> BoxStream<'static, Desktop> {
    let mut captures = poll.captures.clone();
    let catch_up = poll.catch_up;
    let (sender, receiver) = async_channel::bounded(1);
    let _ = thread::Builder::new()
        .name("kortty-journal-dock".into())
        .spawn(move || {
            while !sender.is_closed() {
                // An unchanged journal does not need to wake and redraw every window.
                let mut changed = catch_up;
                for capture in &mut captures {
                    changed |= capture.changed();
                }
                if changed {
                    let _ = sender.try_send(());
                }
                thread::sleep(POLL_INTERVAL);
            }
        });
    receiver.map(|()| Desktop::JournalDockTick).boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_links_match_only_the_current_visible_field_and_target() {
        use std::{fs, path::PathBuf};

        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let folder = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-dock-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&folder).unwrap();
        let scratch = Scratch(folder);
        let xml = format!(
            r#"<session-journal formatVersion="1" id="dock-fixture"><meta/><entries>
            <entry><id>note</id><userNote>https://example.test/note</userNote></entry>
            <entry><id>summary</id><kind>AI_SUMMARY</kind><text>{} https://example.test/note</text></entry>
            </entries></session-journal>"#,
            "x".repeat(view::CARD_TEXT_LIMIT - 2)
        );
        fs::write(scratch.0.join("journal.xml"), xml).unwrap();
        let mut entries = session_journal::read(&scratch.0).unwrap().timeline;
        let target = entries[0]
            .link_spans(LinkField::Note, view::CARD_TEXT_LIMIT)
            .into_iter()
            .find_map(|span| span.target.cloned())
            .unwrap();
        assert!(visible_link(&entries, "note", LinkField::Note, &target));
        assert!(!visible_link(&entries, "note", LinkField::Text, &target));
        assert!(!visible_link(&entries, "summary", LinkField::Text, &target));

        let summary = entries[1].clone();
        entries.resize(MAX_TIMELINE + 1, summary);
        assert!(!visible_link(&entries, "note", LinkField::Note, &target));
    }

    #[test]
    fn hiding_invalidates_a_read_but_keeps_its_worker_slot_until_completion() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let dock = &mut workspace.windows.get_mut(&owner).unwrap().journal_dock;
        assert!(dock.place(Placement::Right));
        dock.pending = Some(PendingRead {
            ticket: 42,
            current: true,
        });
        assert!(dock.place(Placement::Hidden));
        assert!(dock.place(Placement::Left));
        assert!(matches!(
            dock.pending,
            Some(PendingRead {
                ticket: 42,
                current: false
            })
        ));

        let _ = workspace.update_journal_dock(owner, Message::Loaded(41, Err(String::new())));
        assert!(workspace.windows[&owner].journal_dock.pending.is_some());
        let _ = workspace.update_journal_dock(owner, Message::Loaded(42, Err(String::new())));
        assert!(workspace.windows[&owner].journal_dock.pending.is_none());
    }
}
