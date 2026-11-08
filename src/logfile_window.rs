use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::keyboard::Modifiers;
use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, space, text, text_input,
    tooltip,
};
use iced::{Color, Element, Font, Length, Task};

use crate::profile;
use crate::terminal_theme::Rgb;

mod badge_editor;
#[cfg(test)]
pub(crate) use badge_editor::Message as BadgeMessage;
pub(crate) mod catalog;
mod deletion;
pub(crate) mod document;
mod range;
pub(crate) mod zstandard;

use catalog::{Compression, Entry, Format, Report, badges};
use document::{Document, ExportFormat, PAGE_LINES, Page};

const CATALOG_PAGE: usize = 100;
pub(crate) const MANAGER_TITLE: &str = "Logfile Manager";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileType {
    Any,
    Format(Format),
}

impl fmt::Display for FileType {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::Any => "All types",
            Self::Format(format) => format.label(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Packing {
    Any,
    Compression(Compression),
}

impl fmt::Display for Packing {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::Any => "All compression",
            Self::Compression(compression) => compression.label(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DateRange {
    Any,
    Today,
    Week,
    Month,
}

impl fmt::Display for DateRange {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::Any => "All dates",
            Self::Today => "Today",
            Self::Week => "Last 7 days",
            Self::Month => "Last 30 days",
        })
    }
}

impl DateRange {
    fn fields(self, today: jiff::civil::Date) -> range::Fields {
        let days = match self {
            Self::Any => return range::Fields::default(),
            Self::Today => 0,
            Self::Week => 6,
            Self::Month => 29,
        };
        let earliest = today
            .checked_sub(jiff::Span::new().days(days))
            .unwrap_or(today);
        range::Fields::dates(earliest, today)
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Close,
    View,
    Refresh,
    Scanned(Result<Report, String>),
    Select(PathBuf),
    ToggleSelection(PathBuf, bool),
    ClearSelection,
    Filter(String),
    FileType(FileType),
    Packing(Packing),
    Date(DateRange),
    DateField(range::Field, String),
    ClearFilters,
    CatalogPage(bool),
    AddFolder,
    OpenFolder,
    FolderPrepared(Result<PathBuf, String>),
    FolderOpened(Result<(), String>),
    ForgetFolder,
    EditBadges,
    Badge(badge_editor::Message),
    BadgeClose(u64, bool),
    BadgesReady(Result<Arc<badges::Prepared>, String>),
    BadgesApplied {
        report: Option<Report>,
        notice: String,
        keep_draft: bool,
    },
    ToggleHold,
    Delete,
    DeleteFinished(String),
    DeleteScanned {
        report: Result<Report, String>,
        notice: String,
    },
    Export,
    PromptPath(String),
    ExportFormat(ExportFormat),
    ExportPacking(Packing),
    ExportDateField(range::Field, String),
    ExportUseRange(bool),
    Confirm,
    Dismiss,
    CancelWork,
    WorkFailed(String),
    Changed(Result<&'static str, String>),
    Opened(Result<Box<Opened>, String>),
    Page(Result<Page, String>, bool),
    FindText(String),
    Find(bool),
    Found(Result<Option<(usize, Page)>, String>),
    GoLine(String),
    Jump,
    PageTurn(bool),
    Bottom,
}

#[derive(Clone, Debug)]
pub struct Opened {
    entry: Entry,
    document: Arc<Document>,
    page: Page,
}

struct Manager {
    entries: Vec<Entry>,
    selected: BTreeSet<PathBuf>,
    selection_anchor: Option<PathBuf>,
    modifiers: Modifiers,
    filter: String,
    file_type: FileType,
    packing: Packing,
    date: Option<DateRange>,
    range: range::Fields,
    page: usize,
}

impl Default for Manager {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            selected: BTreeSet::new(),
            selection_anchor: None,
            modifiers: Modifiers::empty(),
            filter: String::new(),
            file_type: FileType::Any,
            packing: Packing::Any,
            date: None,
            range: range::Fields::default(),
            page: 0,
        }
    }
}

impl Manager {
    fn filtered(&self) -> impl Iterator<Item = &Entry> {
        let query = self.filter.trim().to_lowercase();
        let range = self.range.evaluate().catalog();
        let now = (!range.is_open()).then(jiff::Zoned::now);
        self.entries
            .iter()
            .filter(move |entry| {
                self.file_type == FileType::Any
                    || self.file_type == FileType::Format(entry.format())
            })
            .filter(move |entry| {
                self.packing == Packing::Any
                    || self.packing == Packing::Compression(entry.compression())
            })
            .filter(move |entry| {
                now.as_ref()
                    .is_none_or(|now| range.overlaps(entry.started_at(), entry.ended_at(now)))
            })
            .filter(move |entry| query.is_empty() || entry.matches(&query))
    }

    fn selected(&self) -> Option<&Entry> {
        if self.selected.len() != 1 {
            return None;
        }
        self.page_entries()
            .find(|entry| self.selected.contains(&entry.path))
    }

    fn first_selected(&self) -> Option<&Entry> {
        self.selected_entries().next()
    }

    fn selected_entries(&self) -> impl Iterator<Item = &Entry> {
        self.filtered()
            .filter(|entry| self.selected.contains(&entry.path))
    }

    fn page_entries(&self) -> impl Iterator<Item = &Entry> {
        self.filtered()
            .skip(self.page * CATALOG_PAGE)
            .take(CATALOG_PAGE)
    }

    fn select(&mut self, path: PathBuf, toggle: Option<bool>) {
        if !self.page_entries().any(|entry| entry.path == path) {
            return;
        }
        let extend = self.modifiers.command();
        if toggle.is_none()
            && self.modifiers.shift()
            && let Some(anchor) = &self.selection_anchor
        {
            let paths: Vec<_> = self.filtered().map(|entry| &entry.path).collect();
            if let (Some(first), Some(last)) = (
                paths.iter().position(|&candidate| candidate == anchor),
                paths.iter().position(|&candidate| candidate == &path),
            ) {
                let range = paths[first.min(last)..=first.max(last)]
                    .iter()
                    .map(|path| (*path).clone())
                    .collect::<Vec<_>>();
                if !extend {
                    self.selected.clear();
                }
                self.selected.extend(range);
                return;
            }
        }
        if toggle.is_none() && !extend {
            self.selected.clear();
        }
        if toggle.unwrap_or_else(|| !self.selected.contains(&path)) {
            self.selected.insert(path.clone());
        } else {
            self.selected.remove(&path);
        }
        self.selection_anchor = Some(path);
    }

    fn reset_filter_page(&mut self) {
        self.page = 0;
        let selected = self
            .filtered()
            .filter(|entry| self.selected.contains(&entry.path))
            .map(|entry| entry.path.clone())
            .collect();
        self.selected = selected;
        if self
            .selection_anchor
            .as_ref()
            .is_some_and(|path| !self.selected.contains(path))
        {
            self.selection_anchor = None;
        }
    }

    fn receive_entries(&mut self, entries: Vec<Entry>) {
        let previous = std::mem::replace(&mut self.entries, entries);
        let prior: BTreeMap<_, _> = previous.iter().map(|entry| (&entry.path, entry)).collect();
        let keep = self
            .filtered()
            .filter(|entry| {
                self.selected.contains(&entry.path)
                    && prior
                        .get(&entry.path)
                        .is_some_and(|previous| previous.same_log(entry))
            })
            .map(|entry| entry.path.clone())
            .collect::<BTreeSet<_>>();
        self.selected = keep;
        if self
            .selection_anchor
            .as_ref()
            .is_some_and(|path| !self.selected.contains(path))
        {
            self.selection_anchor = None;
        }
        self.page = self
            .page
            .min(self.filtered().count().saturating_sub(1) / CATALOG_PAGE);
    }
}

struct Viewer {
    entry: Entry,
    document: Option<Arc<Document>>,
    page: Option<Page>,
    query: String,
    hit: Option<usize>,
    go_line: String,
    scroll: iced::widget::Id,
}

enum Mode {
    Manager(Manager),
    Viewer(Box<Viewer>),
}

enum Prompt {
    AddFolder(String),
    Badges(Box<badge_editor::Editor>),
    Delete(deletion::Batch),
    Export(Box<ExportDraft>),
}

struct ExportDraft {
    entry: Entry,
    path: String,
    format: ExportFormat,
    packing: Compression,
    range: range::Fields,
    use_range: bool,
}

impl ExportDraft {
    fn active_range(&self) -> Option<range::Range> {
        if self.use_range {
            self.range.evaluate().export()
        } else {
            Some(range::Range::default())
        }
    }

    fn suggested_path(&self) -> String {
        self.entry.export_path(
            self.format,
            self.packing,
            self.active_range().unwrap_or_default(),
        )
    }

    fn edit(&mut self, change: impl FnOnce(&mut Self)) {
        let generated = self.path == self.suggested_path();
        change(self);
        if generated {
            self.path = self.suggested_path();
        }
    }
}

pub struct Window {
    mode: Mode,
    prompt: Option<Prompt>,
    badge_close: Option<u64>,
    pending: Option<Arc<AtomicBool>>,
    notice: String,
}

impl Window {
    pub fn manager() -> Self {
        Self {
            mode: Mode::Manager(Manager::default()),
            prompt: None,
            badge_close: None,
            pending: None,
            notice: String::new(),
        }
    }

    pub fn viewer_for_selection(&self) -> Option<Self> {
        if self.pending.is_some() || self.prompt.is_some() {
            return None;
        }
        let Mode::Manager(manager) = &self.mode else {
            return None;
        };
        let entry = manager.selected()?.clone();
        Some(Self {
            mode: Mode::Viewer(Box::new(Viewer {
                entry,
                document: None,
                page: None,
                query: String::new(),
                hit: None,
                go_line: String::new(),
                scroll: iced::widget::Id::unique(),
            })),
            prompt: None,
            badge_close: None,
            pending: None,
            notice: String::new(),
        })
    }

    pub fn is_manager(&self) -> bool {
        matches!(self.mode, Mode::Manager(_))
    }

    pub(crate) fn viewer_path(&self) -> Option<&Path> {
        match &self.mode {
            Mode::Viewer(viewer) => Some(&viewer.entry.path),
            Mode::Manager(_) => None,
        }
    }

    pub(crate) fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn operation_active(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|flag| !flag.load(Ordering::Relaxed))
    }

    pub(crate) fn badge_dirty(&self) -> bool {
        matches!(&self.prompt, Some(Prompt::Badges(editor)) if editor.dirty())
    }

    pub(crate) fn badge_editor_open(&self) -> bool {
        matches!(self.prompt, Some(Prompt::Badges(_)))
    }

    pub(crate) fn badge_needs_close_decision(&self) -> bool {
        self.badge_editor_open() && (self.busy() || self.badge_dirty())
    }

    pub(crate) fn badge_close_token(&self) -> Option<u64> {
        self.badge_close
    }

    pub(crate) fn prompt_badge_close(&mut self, token: u64) {
        if self.badge_dirty() && !self.busy() {
            self.badge_close = Some(token);
        }
    }

    pub(crate) fn cancel_badge_close(&mut self) {
        self.badge_close = None;
    }

    #[cfg(test)]
    pub(crate) fn open_badge_editor_for_test(
        &mut self,
        root: &Path,
        entry: &Entry,
    ) -> io::Result<()> {
        let prepared = badges::prepare(root, vec![badges::Target::from_entry(entry)], &|| false)?;
        let _ = self.update(Message::BadgesReady(Ok(Arc::new(prepared))));
        Ok(())
    }

    pub(crate) fn modifiers(&mut self, modifiers: Modifiers) {
        if let Mode::Manager(manager) = &mut self.mode {
            manager.modifiers = modifiers;
        }
    }

    pub(crate) fn escape_action(&self) -> Message {
        if let Some(token) = self.badge_close {
            Message::BadgeClose(token, false)
        } else if self.busy() {
            Message::CancelWork
        } else if self.prompt.is_some() {
            Message::Dismiss
        } else {
            Message::Close
        }
    }

    pub fn title(&self) -> String {
        match &self.mode {
            Mode::Manager(_) => MANAGER_TITLE.into(),
            Mode::Viewer(viewer) => format!("Logfile Viewer — {}", viewer.entry.filename()),
        }
    }

    pub fn initialize(&mut self) -> Task<Message> {
        self.refresh()
    }

    fn refresh(&mut self) -> Task<Message> {
        match &self.mode {
            Mode::Manager(_) => self.work("Loading logfiles…", |cancelled| {
                Message::Scanned(result(
                    profile::root().and_then(|root| catalog::scan(&root, cancelled)),
                ))
            }),
            Mode::Viewer(viewer) => {
                let entry = viewer.entry.clone();
                let first = viewer.page.as_ref().map_or(0, |page| page.first);
                let already_open = viewer.document.is_some();
                self.work("Reading logfile…", move |cancelled| {
                    let open = || -> io::Result<_> {
                        let entry = if already_open {
                            entry.refresh()?
                        } else {
                            entry
                        };
                        let document = Arc::new(Document::open(&entry, cancelled)?);
                        let page = document.page(first, cancelled)?;
                        Ok(Box::new(Opened {
                            entry,
                            document,
                            page,
                        }))
                    };
                    Message::Opened(result(open()))
                })
            }
        }
    }

    fn work(
        &mut self,
        status: &'static str,
        run: impl FnOnce(&dyn Fn() -> bool) -> Message + Send + 'static,
    ) -> Task<Message> {
        let flag = Arc::new(AtomicBool::new(false));
        self.pending = Some(Arc::clone(&flag));
        self.notice = status.into();
        let (send, receive) = async_channel::bounded(1);
        if let Err(error) = std::thread::Builder::new()
            .name("kortty-logfile".into())
            .spawn(move || {
                let message = run(&|| flag.load(Ordering::Relaxed));
                let _ = send.try_send(message);
            })
        {
            self.pending = None;
            self.notice = error.to_string();
            return Task::none();
        }
        Task::future(async move {
            receive.recv().await.unwrap_or_else(|_| {
                Message::WorkFailed("The logfile operation stopped unexpectedly.".into())
            })
        })
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::WorkFailed(error) => {
                self.pending = None;
                if let Some(Prompt::Badges(editor)) = &mut self.prompt {
                    editor.report_apply_failure(error.clone());
                }
                self.notice = error;
            }
            Message::Scanned(scanned) => {
                self.pending = None;
                match scanned {
                    Ok(report) => {
                        if let Mode::Manager(manager) = &mut self.mode {
                            manager.receive_entries(report.entries);
                            self.notice = if report.skipped != 0 || report.bounded {
                                format!(
                                    "{} logfiles · {} unavailable entries{}",
                                    manager.entries.len(),
                                    report.skipped,
                                    if report.bounded {
                                        " · catalog limit reached"
                                    } else {
                                        ""
                                    }
                                )
                            } else {
                                format!("{} logfiles", manager.entries.len())
                            };
                        }
                    }
                    Err(error) => self.notice = error,
                }
            }
            Message::Opened(opened) => {
                self.pending = None;
                match opened {
                    Ok(opened) => {
                        if let Mode::Viewer(viewer) = &mut self.mode {
                            viewer.entry = opened.entry;
                            viewer.document = Some(opened.document);
                            viewer.page = Some(opened.page);
                            viewer.hit = None;
                            self.notice.clear();
                        }
                    }
                    Err(error) => self.notice = error,
                }
            }
            Message::Page(page, bottom) => {
                self.pending = None;
                match page {
                    Ok(page) => {
                        if let Mode::Viewer(viewer) = &mut self.mode {
                            viewer.page = Some(page);
                            self.notice.clear();
                            return scroll_page(viewer, bottom);
                        }
                    }
                    Err(error) => self.notice = error,
                }
            }
            Message::Found(found) => {
                self.pending = None;
                match found {
                    Ok(Some((line, page))) => {
                        if let Mode::Viewer(viewer) = &mut self.mode {
                            viewer.page = Some(page);
                            viewer.hit = Some(line);
                            self.notice = format!("Found at line {}", line + 1);
                            return scroll_page(viewer, false);
                        }
                    }
                    Ok(None) => self.notice = "No matching displayed text.".into(),
                    Err(error) => self.notice = error,
                }
            }
            Message::Changed(changed) => {
                self.pending = None;
                match changed {
                    Ok("Exported logfile.") => self.notice = "Exported logfile.".into(),
                    Ok(_) => return self.refresh(),
                    Err(error) => self.notice = error,
                }
            }
            Message::DeleteFinished(notice) => {
                return self.work("Refreshing logfiles…", move |cancelled| {
                    let report = profile::root().and_then(|root| catalog::scan(&root, cancelled));
                    Message::DeleteScanned {
                        report: result(report),
                        notice,
                    }
                });
            }
            Message::DeleteScanned { report, mut notice } => {
                self.pending = None;
                match report {
                    Ok(report) => {
                        if let Mode::Manager(manager) = &mut self.mode {
                            manager.receive_entries(report.entries);
                        }
                    }
                    Err(error) => {
                        notice.push_str(&format!(" Could not refresh the logfile list: {error}"));
                    }
                }
                self.notice = notice;
            }
            Message::FolderOpened(opened) => {
                self.pending = None;
                self.notice = opened
                    .err()
                    .unwrap_or_else(|| "Opened the logfile folder.".into());
            }
            Message::BadgesReady(result) => {
                let cancelled = self
                    .pending
                    .take()
                    .is_some_and(|flag| flag.load(Ordering::Relaxed));
                match result {
                    _ if cancelled => self.notice = "Loading logfile badges was cancelled.".into(),
                    Ok(prepared) => {
                        self.notice.clear();
                        self.prompt = Some(Prompt::Badges(Box::new(badge_editor::Editor::new(
                            prepared,
                        ))));
                    }
                    Err(error) => self.notice = error,
                }
            }
            Message::BadgesApplied {
                report,
                notice,
                keep_draft,
            } => {
                self.pending = None;
                if let (Some(report), Mode::Manager(manager)) = (report, &mut self.mode) {
                    manager.receive_entries(report.entries);
                }
                if keep_draft {
                    if let Some(Prompt::Badges(editor)) = &mut self.prompt {
                        editor.report_apply_failure(notice.clone());
                    }
                } else if self.badge_editor_open() {
                    self.prompt = None;
                }
                self.notice = notice;
            }
            Message::CancelWork => {
                if let Some(flag) = &self.pending {
                    flag.store(true, Ordering::Relaxed);
                    self.notice = "Cancelling…".into();
                }
            }
            _ if self.pending.is_some() => {}
            _ if self.badge_close.is_some() => {}
            Message::Dismiss => self.prompt = None,
            Message::Confirm => return self.confirm(),
            Message::Badge(message) => {
                if let Some(Prompt::Badges(editor)) = &mut self.prompt {
                    editor.update(message);
                }
            }
            Message::PromptPath(value) if value.len() <= 4096 => match &mut self.prompt {
                Some(Prompt::AddFolder(path)) => *path = value,
                Some(Prompt::Export(draft)) => draft.path = value,
                _ => {}
            },
            Message::ExportFormat(value) => {
                if let Some(Prompt::Export(draft)) = &mut self.prompt {
                    draft.edit(|draft| draft.format = value);
                }
            }
            Message::ExportPacking(Packing::Compression(value)) => {
                if let Some(Prompt::Export(draft)) = &mut self.prompt {
                    draft.edit(|draft| draft.packing = value);
                }
            }
            Message::ExportDateField(field, value) => {
                if let Some(Prompt::Export(draft)) = &mut self.prompt {
                    draft.edit(|draft| draft.range.set(field, value));
                }
            }
            Message::ExportUseRange(enabled) => {
                if let Some(Prompt::Export(draft)) = &mut self.prompt {
                    draft.edit(|draft| draft.use_range = enabled);
                }
            }
            _ if self.prompt.is_some() => {}
            Message::Refresh => return self.refresh(),
            message => match &mut self.mode {
                Mode::Manager(manager) => match message {
                    Message::Select(path) => manager.select(path, None),
                    Message::ToggleSelection(path, selected) => {
                        manager.select(path, Some(selected))
                    }
                    Message::ClearSelection => {
                        manager.selected.clear();
                        manager.selection_anchor = None;
                    }
                    Message::Filter(value) if value.len() <= 512 => {
                        manager.filter = value;
                        manager.reset_filter_page();
                    }
                    Message::FileType(value) => {
                        manager.file_type = value;
                        manager.reset_filter_page();
                    }
                    Message::Packing(value) => {
                        manager.packing = value;
                        manager.reset_filter_page();
                    }
                    Message::Date(value) => {
                        manager.date = Some(value);
                        manager.range = value.fields(jiff::Zoned::now().date());
                        manager.reset_filter_page();
                    }
                    Message::DateField(field, value) => {
                        manager.date = None;
                        manager.range.set(field, value);
                        manager.reset_filter_page();
                    }
                    Message::ClearFilters => {
                        manager.filter.clear();
                        manager.file_type = FileType::Any;
                        manager.packing = Packing::Any;
                        manager.date = None;
                        manager.range = range::Fields::default();
                        manager.reset_filter_page();
                    }
                    Message::CatalogPage(true)
                        if (manager.page + 1) * CATALOG_PAGE < manager.filtered().count() =>
                    {
                        manager.page += 1;
                    }
                    Message::CatalogPage(false) => manager.page = manager.page.saturating_sub(1),
                    Message::AddFolder => self.prompt = Some(Prompt::AddFolder(String::new())),
                    Message::OpenFolder => {
                        if let Some(entry) = manager.first_selected().cloned() {
                            return self.work("Opening logfile folder…", move |cancelled| {
                                Message::FolderPrepared(result(
                                    catalog::check_cancel(&cancelled)
                                        .and_then(|()| entry.checked_folder()),
                                ))
                            });
                        }
                    }
                    Message::ForgetFolder => {
                        if let Some(folder) = manager
                            .first_selected()
                            .and_then(Entry::forgettable_folder)
                            .map(Path::to_path_buf)
                        {
                            return self.work("Forgetting logfile folder…", move |cancelled| {
                                let forget = || {
                                    catalog::check_cancel(&cancelled)?;
                                    catalog::forget_folder(&profile::root()?, &folder)?;
                                    Ok("Forgot logfile folder.")
                                };
                                Message::Changed(result(forget()))
                            });
                        }
                    }
                    Message::EditBadges => {
                        let targets = manager
                            .filtered()
                            .filter(|entry| manager.selected.contains(&entry.path))
                            .map(badges::Target::from_entry)
                            .collect::<Vec<_>>();
                        if !targets.is_empty() {
                            return self.work("Loading logfile badges…", move |cancelled| {
                                Message::BadgesReady(result(profile::root().and_then(|root| {
                                    badges::prepare(&root, targets, cancelled).map(Arc::new)
                                })))
                            });
                        }
                    }
                    Message::ToggleHold => {
                        if let Some((entry, held)) = manager
                            .selected()
                            .filter(|entry| !entry.live)
                            .and_then(|entry| entry.hold().map(|held| (entry.clone(), held)))
                        {
                            return self.work("Updating hold…", move |cancelled| {
                                let change = || {
                                    catalog::check_cancel(&cancelled)?;
                                    entry.set_hold(!held)?;
                                    Ok("Updated logfile hold.")
                                };
                                Message::Changed(result(change()))
                            });
                        }
                    }
                    Message::Delete => {
                        let batch = deletion::Batch::plan(manager.selected_entries());
                        if batch.is_empty() {
                            self.notice = batch.skipped();
                        } else {
                            self.prompt = Some(Prompt::Delete(batch));
                        }
                    }
                    Message::Export => {
                        let preset = manager.range.evaluate().catalog();
                        self.prompt = manager
                            .selected()
                            .filter(|entry| !entry.live)
                            .cloned()
                            .map(|entry| export_prompt(entry, preset))
                    }
                    _ => {}
                },
                Mode::Viewer(viewer) => match message {
                    Message::FindText(value) if value.len() <= 512 => {
                        viewer.query = value;
                        viewer.hit = None;
                    }
                    Message::GoLine(value) if value.len() <= 16 => viewer.go_line = value,
                    Message::Find(backwards) => {
                        if let Some(document) = viewer.document.clone() {
                            let query = viewer.query.clone();
                            let after = viewer.hit.or_else(|| {
                                viewer
                                    .page
                                    .as_ref()
                                    .and_then(|page| page.first.checked_sub(1))
                            });
                            return self.work("Searching logfile…", move |cancelled| {
                                let search = || -> io::Result<_> {
                                    document
                                        .search(&query, after, backwards, cancelled)?
                                        .map(|line| {
                                            document.page(line, cancelled).map(|page| (line, page))
                                        })
                                        .transpose()
                                };
                                Message::Found(result(search()))
                            });
                        }
                    }
                    Message::Jump => {
                        if let (Ok(line), Some(document)) =
                            (viewer.go_line.parse::<usize>(), viewer.document.clone())
                            && (1..=document.lines()).contains(&line)
                        {
                            return self.page(document, line - 1, false);
                        }
                        self.notice = "Enter a line number within this logfile.".into();
                    }
                    Message::PageTurn(forward) => {
                        if let (Some(document), Some(page)) =
                            (viewer.document.clone(), &viewer.page)
                        {
                            let first = if forward {
                                page.first + PAGE_LINES
                            } else {
                                page.first.saturating_sub(PAGE_LINES)
                            };
                            return self.page(document, first, false);
                        }
                    }
                    Message::Bottom => {
                        if let Some(document) = viewer.document.clone() {
                            let first = document.lines().saturating_sub(PAGE_LINES);
                            return self.page(document, first, true);
                        }
                    }
                    Message::Export if !viewer.entry.live && viewer.document.is_some() => {
                        self.prompt =
                            Some(export_prompt(viewer.entry.clone(), range::Range::default()));
                    }
                    _ => {}
                },
            },
        }
        Task::none()
    }

    fn page(&mut self, document: Arc<Document>, line: usize, bottom: bool) -> Task<Message> {
        self.work("Loading page…", move |cancelled| {
            Message::Page(result(document.page(line, cancelled)), bottom)
        })
    }

    fn confirm(&mut self) -> Task<Message> {
        if matches!(&self.prompt, Some(Prompt::Export(draft)) if draft.active_range().is_none() || draft.path.trim().is_empty())
        {
            return Task::none();
        }
        if let Some(Prompt::Badges(editor)) = &self.prompt {
            if !editor.can_apply() {
                return Task::none();
            }
            let (source, library, selected) = editor.choice();
            return self.work("Applying logfile badges…", move |cancelled| {
                let root = match profile::root() {
                    Ok(root) => root,
                    Err(error) => {
                        return Message::BadgesApplied {
                            report: None,
                            notice: error.to_string(),
                            keep_draft: true,
                        };
                    }
                };
                let (mut notice, keep_draft) =
                    match source.apply(&root, &library, &selected, cancelled) {
                        Ok(outcome) => (outcome.notice(), false),
                        Err(error) => (format!("Could not apply logfile badges: {error}"), true),
                    };
                let report = match catalog::scan(&root, cancelled) {
                    Ok(report) => Some(report),
                    Err(_) if cancelled() => {
                        notice.push_str(" Refresh to show the current logfile badges.");
                        None
                    }
                    Err(error) => {
                        notice.push_str(&format!(" Could not refresh the logfile list: {error}"));
                        None
                    }
                };
                Message::BadgesApplied {
                    report,
                    notice,
                    keep_draft,
                }
            });
        }
        match self.prompt.take() {
            Some(Prompt::AddFolder(path)) => self.work("Adding folder…", move |cancelled| {
                let change = || {
                    catalog::check_cancel(&cancelled)?;
                    catalog::add_folder(&profile::root()?, &PathBuf::from(path))?;
                    Ok("Added logfile folder.")
                };
                Message::Changed(result(change()))
            }),
            Some(Prompt::Delete(batch)) => self.work("Deleting logfiles…", move |cancelled| {
                Message::DeleteFinished(batch.run(cancelled))
            }),
            Some(Prompt::Export(draft)) => self.work("Exporting logfile…", move |cancelled| {
                Message::Changed(result(
                    document::export_range(
                        &draft.entry,
                        Path::new(&draft.path),
                        draft.format,
                        draft.packing,
                        draft.active_range().unwrap_or_default(),
                        cancelled,
                    )
                    .map(|()| "Exported logfile."),
                ))
            }),
            Some(Prompt::Badges(_)) | None => Task::none(),
        }
    }

    pub fn view(&self, foreground: Rgb) -> Element<'_, Message> {
        if let Some(token) = self.badge_close {
            let prompt = column![
                text("Discard unsaved logfile badge changes?").size(18),
                row![
                    button("Keep Editing").on_press(Message::BadgeClose(token, false)),
                    button("Discard and Close").on_press(Message::BadgeClose(token, true)),
                ]
                .spacing(8),
            ]
            .spacing(12);
            return container(container(prompt).padding(16).style(container::bordered_box))
                .center(Length::Fill)
                .into();
        }
        let idle = self.pending.is_none();
        if idle && let Some(Prompt::Badges(editor)) = &self.prompt {
            return container(editor.view(foreground))
                .padding(12)
                .center(Length::Fill)
                .into();
        }
        if idle && let Some(prompt @ Prompt::Export(_)) = &self.prompt {
            return container(container(prompt_view(prompt, foreground)).max_width(640))
                .padding(12)
                .center(Length::Fill)
                .into();
        }
        let usable = idle && self.prompt.is_none();
        let body = match &self.mode {
            Mode::Manager(manager) => manager_view(manager, usable, foreground),
            Mode::Viewer(viewer) => viewer_view(viewer, usable),
        };
        let mut footer = row![
            text(&self.notice).width(Length::Fill),
            action("Close", Message::Close, true)
        ]
        .align_y(iced::Alignment::Center)
        .spacing(10);
        if !idle {
            footer = footer.push(action("Cancel", Message::CancelWork, true));
        }
        let content = column![body, footer].spacing(12).padding(14);
        container(if idle && let Some(prompt) = &self.prompt {
            content.push(prompt_view(prompt, foreground))
        } else {
            content
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        if let Some(flag) = &self.pending {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

fn result<T>(result: io::Result<T>) -> Result<T, String> {
    result.map_err(|error| {
        if error.kind() == io::ErrorKind::Interrupted {
            "Cancelled.".into()
        } else {
            error.to_string()
        }
    })
}

fn export_prompt(entry: Entry, preset: range::Range) -> Prompt {
    let format = ExportFormat::Unchanged;
    let packing = Compression::None;
    let range =
        range::Fields::for_export(preset, entry.started_at(), jiff::Zoned::now().datetime());
    let mut draft = ExportDraft {
        entry,
        path: String::new(),
        format,
        packing,
        range,
        use_range: !preset.is_open(),
    };
    draft.path = draft.suggested_path();
    Prompt::Export(Box::new(draft))
}

fn action(label: &str, message: Message, enabled: bool) -> iced::widget::Button<'_, Message> {
    button(label).on_press_maybe(enabled.then_some(message))
}

fn date_bound<'a>(
    fields: &'a range::Fields,
    evaluation: &range::Evaluation,
    from: bool,
    enabled: bool,
    edit: fn(range::Field, String) -> Message,
) -> iced::widget::Row<'a, Message> {
    let (title, date, time) = if from {
        ("From", range::Field::FromDate, range::Field::FromTime)
    } else {
        ("To", range::Field::ToDate, range::Field::ToTime)
    };
    let field = |kind, prompt: &'static str, width| {
        let invalid = enabled && evaluation.invalid(kind);
        text_input(prompt, fields.text(kind))
            .on_input_maybe(enabled.then_some(move |value| edit(kind, value)))
            .width(width)
            .style(move |theme, status| {
                let mut style = text_input::default(theme, status);
                if invalid {
                    style.border.color = Color::from_rgb8(239, 68, 68);
                }
                style
            })
    };
    row![
        text(title).size(13),
        field(date, "YYYY-MM-DD", 112.0),
        field(time, "HH:MM", 74.0),
    ]
    .spacing(5)
    .align_y(iced::Alignment::Center)
}

fn manager_view(manager: &Manager, usable: bool, foreground: Rgb) -> Element<'_, Message> {
    let selected = manager.selected();
    let first = manager.first_selected();
    let selected_count = manager.selected.len();
    let closed = selected.is_some_and(|entry| !entry.live);
    let hold = selected.and_then(Entry::hold);
    let commands = row![
        action("Refresh", Message::Refresh, usable),
        action("View", Message::View, usable && selected.is_some()),
        action("Export…", Message::Export, usable && closed),
        action(
            "Badges…",
            Message::EditBadges,
            usable && selected_count != 0
        ),
        action(
            if hold == Some(true) {
                "Remove Hold"
            } else {
                "Hold"
            },
            Message::ToggleHold,
            usable && closed && hold.is_some()
        ),
        action(
            "Delete…",
            Message::Delete,
            usable && selected_count != 0 && manager.selected_entries().any(deletion::eligible)
        ),
        action(
            "Open Folder",
            Message::OpenFolder,
            usable && first.and_then(Entry::folder).is_some(),
        ),
        action("Add Folder…", Message::AddFolder, usable),
        action(
            "Forget Folder",
            Message::ForgetFolder,
            usable && first.and_then(Entry::forgettable_folder).is_some(),
        ),
    ]
    .spacing(7)
    .wrap();

    let dates = manager.range.evaluate();
    let filters = row![
        text_input("Search file, connection, badge, or folder", &manager.filter)
            .on_input(Message::Filter)
            .width(235),
        date_bound(&manager.range, &dates, true, usable, Message::DateField),
        date_bound(&manager.range, &dates, false, usable, Message::DateField),
        pick_list(
            [
                FileType::Any,
                FileType::Format(Format::Text),
                FileType::Format(Format::Xml),
                FileType::Format(Format::Json)
            ],
            Some(manager.file_type),
            Message::FileType
        ),
        pick_list(
            [
                Packing::Any,
                Packing::Compression(Compression::None),
                Packing::Compression(Compression::Gzip),
                Packing::Compression(Compression::Bzip2),
                Packing::Compression(Compression::Zstd)
            ],
            Some(manager.packing),
            Message::Packing
        ),
        pick_list(
            [
                DateRange::Any,
                DateRange::Today,
                DateRange::Week,
                DateRange::Month
            ],
            manager.date,
            Message::Date
        )
        .placeholder("Quick range"),
        action("Clear Filters", Message::ClearFilters, usable),
    ]
    .align_y(iced::Alignment::Center)
    .spacing(8)
    .wrap();

    let fields = [
        "File",
        "Connection",
        "Badges",
        "Started",
        "Size",
        "Type",
        "Compression",
        "Deletable from",
        "Folder",
    ];
    let header = row![
        space::horizontal().width(22),
        table_row(fields.map(str::to_owned), None)
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);
    let mut rows = column![header].spacing(2);
    let total = manager.filtered().count();
    for entry in manager.page_entries() {
        let selected = manager.selected.contains(&entry.path);
        let mut item = button(table_row(
            [
                entry.filename().into(),
                entry.connection().into(),
                String::new(),
                entry.started(),
                size_label(entry.size()),
                entry.format().label().into(),
                entry.compression().label().into(),
                entry.deletable(),
                entry
                    .path
                    .parent()
                    .map_or_else(String::new, |path| path.display().to_string()),
            ],
            Some(badge_cell(entry, foreground)),
        ))
        .on_press_maybe(usable.then(|| Message::Select(entry.path.clone())))
        .padding([4, 6]);
        if !selected {
            item = item.style(button::text);
        }
        let path = entry.path.clone();
        let check = checkbox(selected).on_toggle_maybe(
            usable.then_some(move |selected| Message::ToggleSelection(path.clone(), selected)),
        );
        rows = rows.push(
            row![container(check).width(22), item]
                .spacing(6)
                .align_y(iced::Alignment::Center),
        );
    }
    if total == 0 {
        rows = rows.push(text("No matching terminal logfiles.").size(14));
    }
    let table = scrollable(rows)
        .direction(scrollable::Direction::Both {
            vertical: scrollable::Scrollbar::default(),
            horizontal: scrollable::Scrollbar::default(),
        })
        .height(Length::Fill)
        .width(Length::Fill);
    let first = manager.page * CATALOG_PAGE;
    let label = if total == 0 {
        "0 logs".into()
    } else {
        format!(
            "{}–{} of {total}",
            first + 1,
            (first + CATALOG_PAGE).min(total)
        )
    };
    let paging = row![
        text(label).width(Length::Fill),
        text(format!("{selected_count} selected")).size(13),
        action(
            "Clear Selection",
            Message::ClearSelection,
            usable && selected_count != 0
        ),
        action(
            "Previous",
            Message::CatalogPage(false),
            usable && manager.page > 0
        ),
        action(
            "Next",
            Message::CatalogPage(true),
            usable && first + CATALOG_PAGE < total
        ),
    ]
    .align_y(iced::Alignment::Center)
    .spacing(8);
    let mut body = column![commands, filters].spacing(10).height(Length::Fill);
    if let Some(problem) = dates.problem() {
        body = body.push(text(problem).size(13));
    }
    body.push(container(table).style(container::bordered_box))
        .push(paging)
        .into()
}

fn table_row(
    fields: [String; 9],
    mut badges: Option<Element<'static, Message>>,
) -> iced::widget::Row<'static, Message> {
    let widths = [280.0, 142.0, 200.0, 150.0, 82.0, 86.0, 100.0, 144.0, 310.0];
    fields.into_iter().zip(widths).enumerate().fold(
        row![].spacing(8).align_y(iced::Alignment::Center),
        |row, (index, (field, width))| {
            let label = || text(field).wrapping(text::Wrapping::None).size(13).into();
            let field = if index == 2 {
                badges.take().unwrap_or_else(label)
            } else {
                label()
            };
            row.push(container(field).width(width).clip(true))
        },
    )
}

fn badge_cell(entry: &Entry, foreground: Rgb) -> Element<'static, Message> {
    let Some(badges) = entry.badge_labels() else {
        return text("Badges unreadable").size(13).into();
    };
    if badges.is_empty() {
        return space::horizontal().into();
    }
    let mut chips = row![].spacing(4).align_y(iced::Alignment::Center);
    for badge in badges.iter().take(2) {
        chips = chips.push(badge_editor::chip(badge, foreground));
    }
    if badges.len() > 2 {
        chips = chips.push(text(format!("+{}", badges.len() - 2)).size(13));
    }
    tooltip(
        chips,
        text(entry.badges().chars().take(320).collect::<String>()).size(13),
        tooltip::Position::Top,
    )
    .into()
}

fn size_label(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MiB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn scroll_page(viewer: &Viewer, bottom: bool) -> Task<Message> {
    iced::widget::operation::snap_to(
        viewer.scroll.clone(),
        scrollable::RelativeOffset {
            x: None,
            y: Some(if bottom { 1.0 } else { 0.0 }),
        },
    )
}

fn viewer_view(viewer: &Viewer, usable: bool) -> Element<'_, Message> {
    let current = viewer.page.as_ref().map_or(0, |page| page.first);
    let total = viewer
        .document
        .as_ref()
        .map_or(0, |document| document.lines());
    let find = row![
        text_input("Find text", &viewer.query)
            .on_input(Message::FindText)
            .on_submit_maybe((usable && total > 0).then_some(Message::Find(false)))
            .width(Length::Fill),
        action(
            "Previous Match",
            Message::Find(true),
            usable && total > 0 && !viewer.query.trim().is_empty()
        ),
        action(
            "Next Match",
            Message::Find(false),
            usable && total > 0 && !viewer.query.trim().is_empty()
        ),
        text_input("Line number", &viewer.go_line)
            .on_input(Message::GoLine)
            .on_submit_maybe((usable && total > 0).then_some(Message::Jump))
            .width(100),
        action("Go", Message::Jump, usable && total > 0),
    ]
    .spacing(7);
    let mut lines = column![].spacing(2).padding(6);
    if let Some(page) = &viewer.page {
        for (index, line) in page.lines.iter().enumerate() {
            let index = page.first + index;
            let content = row![
                text(format!("{:>7}", index + 1))
                    .width(70)
                    .font(Font::MONOSPACE)
                    .size(13),
                text(line)
                    .font(Font::MONOSPACE)
                    .wrapping(text::Wrapping::None)
                    .size(13),
            ]
            .spacing(8);
            if viewer.hit == Some(index) {
                lines = lines.push(container(content).style(container::bordered_box));
            } else {
                lines = lines.push(content);
            }
        }
    }
    let lines = scrollable(lines)
        .id(viewer.scroll.clone())
        .direction(scrollable::Direction::Both {
            vertical: scrollable::Scrollbar::default(),
            horizontal: scrollable::Scrollbar::default(),
        })
        .width(Length::Fill)
        .height(Length::Fill);
    let info = if total == 0 {
        "0 lines".into()
    } else {
        format!(
            "Lines {}–{} of {total}",
            current + 1,
            (current + PAGE_LINES).min(total)
        )
    };
    let buttons = row![
        text(info).width(Length::Fill),
        action(
            "Previous Page",
            Message::PageTurn(false),
            usable && current > 0
        ),
        action(
            "Next Page",
            Message::PageTurn(true),
            usable && current + PAGE_LINES < total
        ),
        action("Bottom", Message::Bottom, usable && total > 0),
        action("Refresh", Message::Refresh, usable),
        action(
            "Export…",
            Message::Export,
            usable && viewer.document.is_some() && !viewer.entry.live
        ),
    ]
    .spacing(7)
    .align_y(iced::Alignment::Center);
    column![
        text(viewer.entry.path.display().to_string()).size(13),
        find,
        container(lines)
            .style(container::bordered_box)
            .width(Length::Fill)
            .height(Length::Fill),
        buttons,
    ]
    .spacing(10)
    .height(Length::Fill)
    .into()
}

fn prompt_view(prompt: &Prompt, foreground: Rgb) -> Element<'_, Message> {
    let mut fields = column![].spacing(8);
    let (label, can_confirm) = match prompt {
        Prompt::Badges(editor) => return editor.view(foreground),
        Prompt::AddFolder(path) => {
            fields = fields
                .push(text_input("Absolute logfile folder", path).on_input(Message::PromptPath));
            ("Add Folder", !path.trim().is_empty())
        }
        Prompt::Delete(batch) => {
            fields = fields
                .push(text(batch.title()))
                .push(text("This cannot be undone.").size(13));
            let uncompressed = batch.uncompressed();
            if uncompressed != 0 {
                fields = fields.push(
                    text(format!(
                        "{uncompressed} uncompressed logfiles may still be in use by another instance."
                    ))
                    .size(13),
                );
            }
            let skipped = batch.skipped();
            if !skipped.is_empty() {
                fields = fields.push(text(skipped).size(13));
            }
            ("Delete", true)
        }
        Prompt::Export(draft) => {
            let evaluation = draft.range.evaluate();
            fields = fields.push(text("Export Logfile").size(20));
            fields = fields.push(
                text_input("Absolute path for a new file", &draft.path)
                    .on_input(Message::PromptPath),
            );
            fields = fields.push(
                row![
                    pick_list(ExportFormat::ALL, Some(draft.format), Message::ExportFormat),
                    pick_list(
                        [
                            Packing::Compression(Compression::None),
                            Packing::Compression(Compression::Gzip),
                            Packing::Compression(Compression::Bzip2),
                            Packing::Compression(Compression::Zstd),
                        ],
                        Some(Packing::Compression(draft.packing)),
                        Message::ExportPacking
                    ),
                ]
                .spacing(8),
            );
            fields = fields
                .push(
                    checkbox(draft.use_range)
                        .label("Limit to a time range")
                        .on_toggle(Message::ExportUseRange),
                )
                .push(
                    row![
                        date_bound(
                            &draft.range,
                            &evaluation,
                            true,
                            draft.use_range,
                            Message::ExportDateField
                        ),
                        date_bound(
                            &draft.range,
                            &evaluation,
                            false,
                            draft.use_range,
                            Message::ExportDateField
                        ),
                    ]
                    .spacing(12)
                    .wrap(),
                );
            if draft.use_range
                && let Some(problem) = evaluation.problem()
            {
                fields = fields.push(text(problem).size(13));
            }
            (
                "Export Logfile",
                !draft.path.trim().is_empty() && draft.active_range().is_some(),
            )
        }
    };
    let choices = row![
        action("Cancel", Message::Dismiss, true),
        action(label, Message::Confirm, can_confirm),
    ]
    .spacing(8);
    container(fields.push(choices))
        .style(container::bordered_box)
        .padding(12)
        .width(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests;
