//! The native Saved Chats manager and its local profile library.

pub(crate) mod backup;
mod java_xml;
pub(super) mod store;
pub(super) mod swarm;
mod swarm_library;

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Element, Length, Size, Task, window};

use crate::terminal_logging::{Policy as RedactionPolicy, Redactor, Secrets};

use super::{Message as Desktop, Workspace, dialog_placement, tool_tabs};
use store::{Key, Listing, Record, Summary};

pub(super) const TITLE: &str = "Saved Chats";
const MAX_XML_PATH: usize = 4096;

#[derive(Clone)]
pub enum Message {
    Open,
    Close,
    Refresh,
    SwarmLibrary(bool),
    Select(String),
    OpenSelected,
    Rename,
    Delete,
    ImportXml,
    ExportXml,
    Title(String),
    XmlPath(String),
    Confirm,
    Cancel,
    Completed(u64, Result<Arc<Outcome>, String>),
}

impl fmt::Debug for Message {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Saved Chats message")
    }
}

pub struct Outcome(Change);

enum Change {
    List(u64, Listing),
    Open(Box<Record>),
    Renamed(Key, Box<Summary>),
    Deleted(Key, bool),
    Imported(java_xml::ImportReport),
    Exported(java_xml::ExportReport),
    Swarm(PathBuf, swarm_library::Change),
}

enum Operation {
    List(u64),
    Open(Key),
    Rename(Key, String),
    Delete(Key),
    JavaXml(XmlTransfer, PathBuf, Redactor),
    Swarm(swarm_library::Request),
}

impl Operation {
    fn writes(&self) -> bool {
        matches!(
            self,
            Self::Rename(..)
                | Self::Delete(..)
                | Self::JavaXml(XmlTransfer::Import, ..)
                | Self::Swarm(swarm_library::Request::Delete(_))
        )
    }

    fn run(self, root: &Path) -> io::Result<Change> {
        match self {
            Self::List(revision) => store::list(root).map(|list| Change::List(revision, list)),
            Self::Open(key) => store::load(root, &key).map(|record| Change::Open(Box::new(record))),
            Self::Rename(key, title) => {
                store::rename(root, &key, title).map(|row| Change::Renamed(key, Box::new(row)))
            }
            Self::Delete(key) => {
                store::delete(root, &key).map(|sync_failed| Change::Deleted(key, sync_failed))
            }
            Self::JavaXml(XmlTransfer::Import, path, redactor) => {
                java_xml::import(root, &path, &redactor).map(Change::Imported)
            }
            Self::JavaXml(XmlTransfer::Export, path, redactor) => {
                java_xml::export(root, &path, &redactor).map(Change::Exported)
            }
            Self::Swarm(request) => request
                .run(root)
                .map(|result| Change::Swarm(root.to_owned(), result)),
        }
    }
}

#[derive(Clone, Copy)]
enum XmlTransfer {
    Import,
    Export,
}

enum Decision {
    Rename(Key, String),
    Delete(Key, String),
    JavaXml(XmlTransfer, String),
}

pub(super) struct Browser {
    owner: window::Id,
    root: PathBuf,
    rows: Vec<Summary>,
    selected: Option<Key>,
    decision: Option<Decision>,
    pending: Option<u64>,
    unavailable: usize,
    notice: String,
    showing_swarm: bool,
    swarm: swarm_library::Library,
}

impl Browser {
    fn new(owner: window::Id, root: PathBuf) -> Self {
        Self {
            owner,
            root,
            rows: Vec::new(),
            selected: None,
            decision: None,
            pending: None,
            unavailable: 0,
            notice: String::new(),
            showing_swarm: false,
            swarm: swarm_library::Library::default(),
        }
    }

    fn view(&self, enabled: bool, swarm_enabled: bool, saving_chat: bool) -> Element<'_, Message> {
        if self.showing_swarm {
            return self
                .swarm
                .view(enabled, swarm_enabled, self.pending.is_some(), &self.notice);
        }
        let editable = enabled && self.pending.is_none();
        let normal = editable && self.decision.is_none();
        let selected = normal && self.selected.is_some() && !saving_chat;
        let header = row![
            cell("Title", 4),
            cell("Profile", 3),
            cell("Connection", 3),
            cell("Updated", 3)
        ]
        .spacing(8);
        let rows = column(self.rows.iter().map(|entry| {
            let display = date(entry.updated);
            button(
                row![
                    cell(&entry.title, 4),
                    cell(&entry.profile_name, 3),
                    cell(&entry.connection, 3),
                    cell(display, 3),
                ]
                .spacing(8),
            )
            .width(Length::Fill)
            .style(if self.selected.as_ref() == Some(&entry.key) {
                button::primary
            } else {
                button::secondary
            })
            .on_press_maybe(normal.then(|| Message::Select(entry.key.id.clone())))
            .into()
        }))
        .spacing(4);
        let toolbar = if let Some(decision) = &self.decision {
            let (prompt, field, confirm, valid) = match decision {
                Decision::Rename(_, title) => (
                    text("Rename saved chat"),
                    Some(text_input("Chat title", title).on_input(Message::Title)),
                    button("Rename"),
                    store::valid_title(title.trim()),
                ),
                Decision::Delete(_, title) => (
                    text(format!("Delete the saved conversation “{title}”?")),
                    None,
                    button("Delete").style(button::danger),
                    true,
                ),
                Decision::JavaXml(transfer, path) => {
                    let (description, placeholder, action) = match transfer {
                        XmlTransfer::Import => (
                            "Import saved conversations from a Java ai-chats.xml file.",
                            "Absolute path to the Java XML file",
                            "Import",
                        ),
                        XmlTransfer::Export => (
                            "Export all saved conversations and their text to a new Java XML file.",
                            "Absolute path to a new XML file",
                            "Export",
                        ),
                    };
                    let valid = xml_path(path).is_some();
                    let field = text_input(placeholder, path)
                        .on_input_maybe(editable.then_some(Message::XmlPath))
                        .on_submit_maybe((editable && valid).then_some(Message::Confirm));
                    (text(description), Some(field), button(action), valid)
                }
            };
            column![
                prompt,
                field,
                row![
                    confirm.on_press_maybe(
                        (editable && valid && !saving_chat).then_some(Message::Confirm)
                    ),
                    button("Cancel").on_press(Message::Cancel),
                ]
                .spacing(8)
            ]
            .spacing(8)
        } else {
            column![
                row![
                    button("Open").on_press_maybe(selected.then_some(Message::OpenSelected)),
                    button("Rename…").on_press_maybe(selected.then_some(Message::Rename)),
                    button("Delete…").on_press_maybe(selected.then_some(Message::Delete)),
                    button("Refresh").on_press_maybe(normal.then_some(Message::Refresh)),
                    button("Import XML…").on_press_maybe(normal.then_some(Message::ImportXml)),
                    button("Export XML…").on_press_maybe(normal.then_some(Message::ExportXml)),
                    button("Close")
                        .style(button::secondary)
                        .on_press(Message::Close),
                ]
                .spacing(8)
                .wrap()
            ]
        };
        column![
            text(TITLE).size(19),
            library_buttons(false, normal),
            container(header).padding([0, 6]),
            scrollable(
                column![
                    (self.rows.is_empty() && self.pending.is_none())
                        .then(|| text("No saved conversations.")),
                    rows,
                ]
                .spacing(8)
            )
            .height(Length::Fill),
            (!enabled).then(|| text("AI is disabled under the current settings.")),
            saving_chat.then(|| text("The selected conversation is being saved.")),
            (self.pending.is_some()).then(|| text("Working…")),
            (self.unavailable > 0).then(|| text(format!(
                "{} saved chat files could not be read.",
                self.unavailable
            ))
            .size(13)),
            (!self.notice.is_empty()).then(|| text(&self.notice).size(13)),
            toolbar,
        ]
        .spacing(10)
        .padding(14)
        .height(Length::Fill)
        .width(Length::Fill)
        .into()
    }

    fn action_key(&self) -> Option<&Key> {
        if self.showing_swarm {
            return None;
        }
        match &self.decision {
            Some(Decision::Rename(key, _) | Decision::Delete(key, _)) => Some(key),
            Some(Decision::JavaXml(..)) => None,
            None => self.selected.as_ref(),
        }
    }

    fn selected(&self) -> Option<&Summary> {
        let key = self.selected.as_ref()?;
        self.rows.iter().find(|row| &row.key == key)
    }

    fn install_list(&mut self, listing: &Listing) {
        self.rows.clone_from(&listing.chats);
        self.unavailable = listing.unavailable;
        self.selected = self.selected.as_ref().and_then(|selected| {
            self.rows
                .iter()
                .find(|row| row.key.same_chat(selected))
                .map(|row| row.key.clone())
        });
    }

    fn upsert(&mut self, entry: &Summary) {
        self.rows.retain(|row| !row.key.same_chat(&entry.key));
        self.rows.push(entry.clone());
        self.rows
            .sort_by(|a, b| (b.updated, &b.key.id).cmp(&(a.updated, &a.key.id)));
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| selected.same_chat(&entry.key))
        {
            self.selected = Some(entry.key.clone());
        }
    }

    fn remove(&mut self, key: &Key) {
        self.rows.retain(|row| !row.key.same_chat(key));
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| selected.same_chat(key))
        {
            self.selected = None;
        }
    }
}

fn library_buttons(showing_swarm: bool, enabled: bool) -> Element<'static, Message> {
    row![
        button("Terminal chats")
            .style(if showing_swarm {
                button::secondary
            } else {
                button::primary
            })
            .on_press_maybe((showing_swarm && enabled).then_some(Message::SwarmLibrary(false))),
        button("Swarm chats")
            .style(if showing_swarm {
                button::primary
            } else {
                button::secondary
            })
            .on_press_maybe((!showing_swarm && enabled).then_some(Message::SwarmLibrary(true))),
    ]
    .spacing(8)
    .into()
}

fn cell<'a>(
    label: impl iced::widget::text::IntoFragment<'a>,
    portion: u16,
) -> Element<'a, Message> {
    text(label)
        .size(14)
        .width(Length::FillPortion(portion))
        .into()
}

fn date(value: i64) -> String {
    jiff::Timestamp::from_millisecond(value).map_or_else(
        |_| "Unknown".into(),
        |time| {
            time.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        },
    )
}

fn xml_path(value: &str) -> Option<PathBuf> {
    let trimmed = value.trim();
    let name = trimmed.rsplit(std::path::is_separator).next()?;
    let path = Path::new(trimmed);
    (value.len() <= MAX_XML_PATH
        && !crate::ssh_dialog::unsafe_text(value)
        && !matches!(name, "" | "." | "..")
        && path.is_absolute()
        && path.file_name().is_some())
    .then(|| path.to_owned())
}

pub(crate) fn java_xml_redactor(policy: &RedactionPolicy) -> io::Result<Redactor> {
    Ok(Redactor::for_journal(
        Secrets::default().seal()?,
        policy.clone(),
    ))
}

fn import_notice(report: &java_xml::ImportReport) -> String {
    use std::fmt::Write as _;

    let count = report.saved.len();
    let suffix = if count == 1 { "" } else { "s" };
    let mut notice = format!("Imported {count} saved conversation{suffix}.");
    for (count, reason) in [
        (report.duplicates, "already present"),
        (report.full, "skipped because the library is full"),
        (report.unsupported, "unsupported"),
        (report.invalid, "invalid"),
        (report.failed, "could not be saved"),
    ] {
        if count != 0 {
            let _ = write!(notice, " {count} {reason}.");
        }
    }
    if report.saved.iter().any(|summary| summary.sync_failed) {
        notice.push_str(" The profile directory could not be synchronized.");
    }
    notice
}

impl Workspace {
    pub(super) fn saved_chats_title(&self, id: window::Id) -> Option<&'static str> {
        self.saved_chat_browsers.contains_key(&id).then_some(TITLE)
    }

    pub(super) fn saved_chats_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        let browser = self.saved_chat_browsers.get(&id)?;
        let saving_chat = browser
            .action_key()
            .is_some_and(|key| self.terminal_chat_write_pending(key));
        Some(
            browser
                .view(
                    self.ai_chat_enabled(),
                    self.swarm_chat_access_allowed(),
                    saving_chat,
                )
                .map(move |message| wrap(id, message)),
        )
    }

    pub(super) fn close_saved_chats(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        let owner = self.saved_chat_browsers.get(&id)?.owner;
        if self.hosted_tool_owner(id).is_some() {
            return Some(self.request_hosted_tool_close(id));
        }
        self.saved_chat_browsers.remove(&id);
        Some(self.close_placed_dialog_and_focus(id, owner))
    }

    pub(super) fn remove_saved_chat_browsers_for_owner(&mut self, owner: window::Id) {
        let detached = self
            .saved_chat_browsers
            .iter()
            .filter_map(|(&id, browser)| {
                (browser.owner == owner && self.hosted_tool_owner(id).is_none()).then_some(id)
            })
            .collect::<Vec<_>>();
        for id in detached {
            self.saved_chat_browsers.remove(&id);
        }
    }

    fn open_saved_chats(&mut self, owner: window::Id) -> Task<Desktop> {
        if self.exiting
            || self.scheduler_draining
            || !self.windows.contains_key(&owner)
            || !self.ai_chat_enabled()
        {
            return Task::none();
        }
        if let Some(id) = self.hosted_tool_in(owner, tool_tabs::Kind::SavedChats) {
            return self.focus_hosted_tool(id).unwrap_or_else(Task::none);
        }
        if let Some((&id, _)) = self.saved_chat_browsers.iter().find(|(id, browser)| {
            browser.owner == owner && self.placed_dialog_owner(**id) == Some(owner)
        }) {
            return window::minimize(id, false).chain(window::gain_focus(id));
        }
        let root = match crate::profile::root() {
            Ok(root) => root,
            Err(error) => {
                self.notice(
                    owner,
                    format!("Could not locate the saved chats profile ({error})."),
                );
                return Task::none();
            }
        };
        self.install_saved_chats(owner, root)
    }

    pub(super) fn install_saved_chats(
        &mut self,
        owner: window::Id,
        root: PathBuf,
    ) -> Task<Desktop> {
        let (id, task) = if self.window_settings.open_tool_windows_as_tabs {
            let id = window::Id::unique();
            (id, self.host_tool(owner, tool_tabs::Kind::SavedChats, id))
        } else {
            self.open_modeless_placed_dialog(
                owner,
                dialog_placement::Kind::SavedChats,
                window::Settings {
                    size: Size::new(860.0, 560.0),
                    min_size: Some(Size::new(620.0, 420.0)),
                    position: window::Position::Centered,
                    exit_on_close_request: false,
                    ..window::Settings::default()
                },
            )
        };
        self.saved_chat_browsers
            .insert(id, Browser::new(owner, root));
        Task::batch([task, self.list_saved_chats(id)])
    }

    fn list_saved_chats(&mut self, id: window::Id) -> Task<Desktop> {
        let revision = self.saved_chat_library_revision;
        let operation = if self
            .saved_chat_browsers
            .get(&id)
            .is_some_and(|browser| browser.showing_swarm)
        {
            Operation::Swarm(swarm_library::Request::List(revision))
        } else {
            Operation::List(revision)
        };
        self.saved_chat_operation(id, operation)
    }

    fn saved_chat_operation(&mut self, id: window::Id, operation: Operation) -> Task<Desktop> {
        let token = self.allocate();
        let Some(browser) = self.saved_chat_browsers.get_mut(&id) else {
            return Task::none();
        };
        if browser.pending.is_some() {
            return Task::none();
        }
        browser.pending = Some(token);
        browser.notice.clear();
        let root = browser.root.clone();
        if operation.writes() {
            self.saved_chat_mutations.insert(token, id);
        }
        Task::perform(
            async move {
                operation
                    .run(&root)
                    .map(|change| Arc::new(Outcome(change)))
                    .map_err(|error| error.to_string())
            },
            move |result| wrap(id, Message::Completed(token, result)),
        )
    }

    pub(super) fn record_saved_chat(&mut self, summary: &Summary) {
        self.saved_chat_library_revision += 1;
        for browser in self.saved_chat_browsers.values_mut() {
            browser.upsert(summary);
        }
    }

    pub(super) fn record_saved_swarm(&mut self, root: &Path, record: &swarm::Record) {
        if !swarm::load(root, &record.id).is_ok_and(|current| current.same_revision(record)) {
            return;
        }
        self.saved_chat_library_revision += 1;
        let summary = record.summary();
        for browser in self
            .saved_chat_browsers
            .values_mut()
            .filter(|browser| browser.root == root)
        {
            browser.swarm.upsert(summary.clone());
        }
    }

    fn complete_saved_chat_operation(
        &mut self,
        id: window::Id,
        token: u64,
        outcome: Result<Arc<Outcome>, String>,
    ) -> Task<Desktop> {
        // A confirmed write can outlive its browser. Reconcile peers only for
        // completions that match a write registered by this workspace.
        let mutation = self.saved_chat_mutations.get(&token) == Some(&id);
        if mutation {
            self.saved_chat_mutations.remove(&token);
        }
        let owner = self.hosted_tool_owner(id);
        let close_locked = self.hosted_close_locks(id);
        let outcome = outcome.as_deref().map(|outcome| &outcome.0);
        let opening_during_save = matches!(
            outcome,
            Ok(Change::Open(record)) if self.terminal_chat_write_pending(&record.summary.key)
        );
        let swarm_open_allowed = matches!(
            outcome,
            Ok(Change::Swarm(
                _,
                swarm_library::Change::Open(_) | swarm_library::Change::OpenArchive(_)
            ))
        ) && self.swarm_chat_access_allowed();
        let mut browser = self
            .saved_chat_browsers
            .get_mut(&id)
            .filter(|browser| browser.pending == Some(token));
        if let Some(browser) = browser.as_mut() {
            browser.pending = None;
        }
        match outcome {
            Ok(Change::List(revision, listing)) => {
                if let Some(browser) = browser {
                    if *revision != self.saved_chat_library_revision {
                        return self.list_saved_chats(id);
                    }
                    browser.install_list(listing);
                }
            }
            Ok(Change::Open(record)) => {
                if let Some(browser) = browser.filter(|_| !close_locked) {
                    if opening_during_save {
                        browser.notice =
                            "Wait for the conversation to finish saving, then open it again."
                                .into();
                        return Task::none();
                    }
                    if browser.selected.as_ref() != Some(&record.summary.key) {
                        browser.notice =
                            "The saved conversation changed while it was opening.".into();
                        return Task::none();
                    }
                    let owner = owner.unwrap_or(browser.owner);
                    let root = browser.root.clone();
                    return self.open_stored_terminal_chat(owner, root, record.as_ref().clone());
                }
            }
            Ok(Change::Renamed(old, summary)) if mutation => {
                if let Some(browser) = browser {
                    browser.decision = None;
                    if summary.sync_failed {
                        browser.notice =
                            "Conversation renamed, but the profile directory could not be synchronized."
                                .into();
                    }
                }
                self.terminal_chat_renamed(old, summary);
                self.record_saved_chat(summary);
                return self.synchronize_agent_journal_chat(&summary.key);
            }
            Ok(Change::Deleted(key, sync_failed)) if mutation => {
                if let Some(browser) = browser {
                    browser.decision = None;
                    if *sync_failed {
                        browser.notice =
                            "Conversation deleted, but the profile directory could not be synchronized."
                                .into();
                    }
                }
                self.saved_chat_library_revision += 1;
                for browser in self.saved_chat_browsers.values_mut() {
                    browser.remove(key);
                }
                self.terminal_chat_deleted(key);
            }
            Ok(Change::Imported(report)) if mutation => {
                if let Some(browser) = browser {
                    browser.decision = None;
                    browser.notice = import_notice(report);
                }
                for summary in &report.saved {
                    self.record_saved_chat(summary);
                }
            }
            Ok(Change::Exported(report)) => {
                if let Some(browser) = browser {
                    browser.decision = None;
                    let count = report.count;
                    let suffix = if count == 1 { "" } else { "s" };
                    browser.notice = format!("Exported {count} saved conversation{suffix}.");
                    if report.sync_failed {
                        browser
                            .notice
                            .push_str(" The output directory could not be synchronized.");
                    }
                    if report.cleanup_failed {
                        browser.notice.push_str(
                            " The private temporary export directory could not be removed.",
                        );
                    }
                }
            }
            Ok(Change::Swarm(root, change)) => match change {
                swarm_library::Change::List(revision, listing) => {
                    if let Some(browser) =
                        browser.filter(|browser| browser.showing_swarm && &browser.root == root)
                    {
                        if *revision != self.saved_chat_library_revision {
                            return self.list_saved_chats(id);
                        }
                        browser.swarm.install(listing);
                    }
                }
                swarm_library::Change::Open(record) => {
                    if let Some(browser) = browser.filter(|browser| {
                        !close_locked && browser.showing_swarm && &browser.root == root
                    }) {
                        if !swarm_open_allowed || !browser.swarm.matches_open(record) {
                            browser.notice = "The saved Swarm conversation is no longer available. Refresh the library.".into();
                            return Task::none();
                        }
                        let owner = owner.unwrap_or(browser.owner);
                        return self.open_saved_swarm(owner, root.clone(), record.as_ref().clone());
                    }
                }
                swarm_library::Change::OpenArchive(archive) => {
                    if let Some(browser) = browser.filter(|browser| {
                        !close_locked && browser.showing_swarm && &browser.root == root
                    }) && (!swarm_open_allowed || !browser.swarm.open_archive(archive))
                    {
                        browser.notice = "The saved Swarm conversation is no longer available. Refresh the library.".into();
                    }
                }
                swarm_library::Change::Deleted(chat, sync_failed) if mutation => {
                    self.saved_chat_library_revision += 1;
                    for browser in self
                        .saved_chat_browsers
                        .values_mut()
                        .filter(|browser| &browser.root == root)
                    {
                        browser.swarm.remove(chat);
                    }
                    if *sync_failed && let Some(browser) = self.saved_chat_browsers.get_mut(&id) {
                        browser.notice = "Swarm conversation deleted, but the profile directory could not be synchronized.".into();
                    }
                    return self.revoke_saved_swarm(root, chat);
                }
                _ => {}
            },
            Err(error) => {
                if let Some(browser) = browser {
                    browser.notice.clone_from(error);
                }
            }
            _ => {}
        }
        Task::none()
    }

    pub(super) fn update_saved_chats(&mut self, id: window::Id, message: Message) -> Task<Desktop> {
        if matches!(message, Message::Open) {
            return self.open_saved_chats(id);
        }
        if matches!(message, Message::Close) {
            return self.close_saved_chats(id).unwrap_or_else(Task::none);
        }
        if self.exiting {
            return Task::none();
        }
        if let Message::Completed(token, outcome) = message {
            return self.complete_saved_chat_operation(id, token, outcome);
        }
        if !self.ai_chat_enabled() || self.hosted_close_locks(id) {
            return Task::none();
        }
        let swarm_enabled = self.swarm_chat_access_allowed();
        if matches!(
            message,
            Message::OpenSelected | Message::Rename | Message::Delete | Message::Confirm
        ) && self
            .saved_chat_browsers
            .get(&id)
            .and_then(Browser::action_key)
            .is_some_and(|key| self.terminal_chat_write_pending(key))
        {
            return Task::none();
        }
        let Some(browser) = self.saved_chat_browsers.get_mut(&id) else {
            return Task::none();
        };
        if browser.pending.is_some() {
            return Task::none();
        }
        if let Message::SwarmLibrary(show) = message {
            if browser.decision.is_none()
                && !browser.swarm.confirming()
                && browser.showing_swarm != show
            {
                browser.swarm.close_archive();
                browser.showing_swarm = show;
                return self.list_saved_chats(id);
            }
            return Task::none();
        }
        if browser.showing_swarm {
            browser.notice.clear();
            return browser
                .swarm
                .update(message, self.saved_chat_library_revision, swarm_enabled)
                .map_or_else(Task::none, |request| {
                    self.saved_chat_operation(id, Operation::Swarm(request))
                });
        }
        if let Some(decision) = &mut browser.decision {
            match (message, decision) {
                (Message::Title(title), Decision::Rename(_, draft)) => {
                    *draft = title.chars().take(store::MAX_TITLE_CHARS).collect()
                }
                (Message::XmlPath(path), Decision::JavaXml(_, draft))
                    if path.len() <= MAX_XML_PATH =>
                {
                    *draft = path;
                    browser.notice.clear();
                }
                (Message::Cancel, _) => {
                    browser.decision = None;
                    browser.notice.clear();
                }
                (Message::Confirm, Decision::Rename(key, title))
                    if store::valid_title(title.trim()) =>
                {
                    let key = key.clone();
                    let title = title.trim().to_owned();
                    return match self.mask_saved_title(&title) {
                        Ok(title) => self.saved_chat_operation(id, Operation::Rename(key, title)),
                        Err(error) => {
                            self.saved_chat_browsers
                                .get_mut(&id)
                                .expect("active saved chats browser")
                                .notice = error;
                            Task::none()
                        }
                    };
                }
                (Message::Confirm, Decision::Delete(key, _)) => {
                    let key = key.clone();
                    return self.saved_chat_operation(id, Operation::Delete(key));
                }
                (Message::Confirm, Decision::JavaXml(transfer, path)) => {
                    let Some(path) = xml_path(path) else {
                        browser.notice = "Enter an absolute path to an XML file.".into();
                        return Task::none();
                    };
                    let redactor = match java_xml_redactor(&self.clipboard.policy.terminal_logging)
                    {
                        Ok(redactor) => redactor,
                        Err(error) => {
                            browser.notice = format!("Could not prepare chat redaction ({error}).");
                            return Task::none();
                        }
                    };
                    let operation = Operation::JavaXml(*transfer, path, redactor);
                    return self.saved_chat_operation(id, operation);
                }
                _ => {}
            }
            return Task::none();
        }
        match message {
            Message::Refresh => return self.list_saved_chats(id),
            Message::Select(id) => {
                browser.selected = browser
                    .rows
                    .iter()
                    .find(|row| row.key.id == id)
                    .map(|row| row.key.clone());
                browser.notice.clear();
            }
            Message::OpenSelected => {
                if let Some(key) = browser.selected.clone() {
                    return self.saved_chat_operation(id, Operation::Open(key));
                }
            }
            Message::Rename | Message::Delete => {
                if let Some(row) = browser.selected() {
                    browser.decision = Some(if matches!(message, Message::Rename) {
                        Decision::Rename(row.key.clone(), row.title.clone())
                    } else {
                        Decision::Delete(row.key.clone(), row.title.clone())
                    });
                }
            }
            Message::ImportXml | Message::ExportXml => {
                let transfer = if matches!(message, Message::ImportXml) {
                    XmlTransfer::Import
                } else {
                    XmlTransfer::Export
                };
                browser.decision = Some(Decision::JavaXml(transfer, String::new()));
                browser.notice.clear();
            }
            _ => {}
        }
        Task::none()
    }
}

fn wrap(id: window::Id, message: Message) -> Desktop {
    Desktop::SavedChats(id, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(title: &str, revision: &str) -> Summary {
        serde_json::from_value(serde_json::json!({
            "key": { "id": "chat", "revision": revision },
            "title": title,
            "created": 1,
            "updated": 1,
            "profile": 1,
            "profile_name": "Local",
            "connection": "Test",
        }))
        .unwrap()
    }

    fn install_browser(workspace: &mut Workspace) -> window::Id {
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.install_saved_chats(owner, PathBuf::new());
        workspace
            .hosted_tool_in(owner, tool_tabs::Kind::SavedChats)
            .unwrap()
    }

    fn complete(workspace: &mut Workspace, browser: window::Id, token: u64, change: Change) {
        let _ = workspace.update_saved_chats(
            browser,
            Message::Completed(token, Ok(Arc::new(Outcome(change)))),
        );
    }

    fn listing(revision: u64, chats: Vec<Summary>) -> Change {
        Change::List(
            revision,
            Listing {
                chats,
                unavailable: 0,
            },
        )
    }

    #[test]
    fn saved_chat_actions_wait_for_a_terminal_write_across_revisions() {
        use crate::workspace::terminal_ai::ChatWrite;

        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = true;
        let id = install_browser(&mut workspace);
        let original = summary("Conversation", "before-save");
        let published = summary("Conversation", "after-save");
        let mut other = summary("Other conversation", "independent");
        other.key.id = "other".into();
        let initial = workspace.saved_chat_browsers[&id].pending.unwrap();
        complete(
            &mut workspace,
            id,
            initial,
            listing(0, vec![original.clone(), other.clone()]),
        );
        let _ = workspace.update_saved_chats(id, Message::Select(original.key.id.clone()));
        let _ = workspace.update_saved_chats(id, Message::OpenSelected);
        let opening = workspace.saved_chat_browsers[&id].pending.unwrap();

        // A request to open the old revision overlaps the new write.
        let writer = window::Id::unique();
        let writing = workspace.allocate();
        workspace
            .terminal_chat_writes
            .insert(writing, ChatWrite::new(writer, Some(&original)));
        let record = serde_json::from_value(serde_json::json!({
            "version": 1, "summary": original, "turns": []
        }))
        .unwrap();
        complete(&mut workspace, id, opening, Change::Open(Box::new(record)));
        assert!(workspace.terminal_chats.is_empty());
        assert!(
            workspace.saved_chat_browsers[&id]
                .notice
                .contains("finish saving")
        );

        // Refresh can observe the new revision before the write acknowledgment arrives.
        let _ = workspace.update_saved_chats(id, Message::Refresh);
        let refreshing = workspace.saved_chat_browsers[&id].pending.unwrap();
        complete(
            &mut workspace,
            id,
            refreshing,
            listing(0, vec![published.clone(), other.clone()]),
        );
        assert!(workspace.saved_chat_browsers[&id].selected.as_ref() == Some(&published.key));
        for action in [Message::OpenSelected, Message::Rename, Message::Delete] {
            let _ = workspace.update_saved_chats(id, action);
            let browser = &workspace.saved_chat_browsers[&id];
            assert!(browser.pending.is_none() && browser.decision.is_none());
        }
        for decision in [
            Decision::Rename(original.key.clone(), "New title".into()),
            Decision::Delete(original.key, "Conversation".into()),
        ] {
            workspace.saved_chat_browsers.get_mut(&id).unwrap().decision = Some(decision);
            let _ = workspace.update_saved_chats(id, Message::Confirm);
            assert!(workspace.saved_chat_browsers[&id].pending.is_none());
            let _ = workspace.update_saved_chats(id, Message::Cancel);
        }

        let _ = workspace.update_saved_chats(id, Message::ExportXml);
        assert!(matches!(
            &workspace.saved_chat_browsers[&id].decision,
            Some(Decision::JavaXml(XmlTransfer::Export, _))
        ));
        let _ = workspace.update_saved_chats(id, Message::Cancel);
        let _ = workspace.update_saved_chats(id, Message::Select(other.key.id.clone()));
        let _ = workspace.update_saved_chats(id, Message::Rename);
        assert!(matches!(
            &workspace.saved_chat_browsers[&id].decision,
            Some(Decision::Rename(key, _)) if key == &other.key
        ));
    }

    #[test]
    fn saved_chat_java_xml_transfer_requires_confirmation_and_an_absolute_file() {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = true;
        let browser = install_browser(&mut workspace);
        let token = workspace.saved_chat_browsers[&browser].pending.unwrap();
        complete(&mut workspace, browser, token, listing(0, vec![]));

        let _ = workspace.update_saved_chats(browser, Message::ExportXml);
        let _ = workspace.update_saved_chats(browser, Message::XmlPath("ai-chats.xml".into()));
        let _ = workspace.update_saved_chats(browser, Message::Confirm);
        assert!(workspace.saved_chat_browsers[&browser].pending.is_none());

        let destination = std::env::current_dir().unwrap().join("ai-chats.xml");
        let path = destination.to_str().unwrap().to_owned();
        assert!(xml_path(&format!("{path}\n")).is_none());
        assert!(xml_path(&format!("{path}/.")).is_none());
        let _ = workspace.update_saved_chats(browser, Message::XmlPath(path));
        assert!(workspace.saved_chat_browsers[&browser].pending.is_none());
        let _ = workspace.update_saved_chats(browser, Message::Confirm);
        let token = workspace.saved_chat_browsers[&browser].pending.unwrap();
        assert!(workspace.saved_chat_mutations.is_empty());

        complete(
            &mut workspace,
            browser,
            token,
            Change::Exported(java_xml::ExportReport {
                count: 1,
                sync_failed: true,
                cleanup_failed: true,
            }),
        );
        let browser = &workspace.saved_chat_browsers[&browser];
        assert!(browser.decision.is_none());
        assert!(browser.notice.contains("Exported 1 saved conversation."));
        assert!(
            browser
                .notice
                .contains("output directory could not be synchronized")
        );
        assert!(
            browser
                .notice
                .contains("temporary export directory could not be removed")
        );
    }

    #[test]
    fn saved_chat_java_xml_import_reconciles_peers_after_the_browser_closes() {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = true;
        let writer = install_browser(&mut workspace);
        let peer = install_browser(&mut workspace);
        let initial = workspace.saved_chat_browsers[&writer].pending.unwrap();
        complete(&mut workspace, writer, initial, listing(0, vec![]));
        let source = std::env::current_dir().unwrap().join("ai-chats.xml");
        let _ = workspace.update_saved_chats(writer, Message::ImportXml);
        let _ = workspace.update_saved_chats(
            writer,
            Message::XmlPath(source.to_str().unwrap().to_owned()),
        );
        let _ = workspace.update_saved_chats(writer, Message::Confirm);
        let token = workspace.saved_chat_browsers[&writer].pending.unwrap();
        assert_eq!(workspace.saved_chat_mutations.get(&token), Some(&writer));
        let _ = workspace.close_saved_chats(writer);

        let mut saved = summary("Imported", "new");
        saved.sync_failed = true;
        let report = java_xml::ImportReport {
            saved: vec![saved.clone()],
            duplicates: 1,
            full: 0,
            failed: 0,
            unsupported: 1,
            invalid: 0,
        };
        let notice = import_notice(&report);
        assert!(notice.contains("Imported 1 saved conversation."));
        assert!(notice.contains("1 already present"));
        assert!(notice.contains("1 unsupported"));
        assert!(notice.contains("profile directory could not be synchronized"));
        complete(&mut workspace, writer, token, Change::Imported(report));
        assert!(workspace.saved_chat_mutations.is_empty());
        assert!(workspace.saved_chat_browsers[&peer].rows == [saved]);
    }

    #[test]
    fn saved_chat_rename_outlives_its_browser_and_invalidates_stale_listing() {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = true;
        let writer = install_browser(&mut workspace);
        let peer = install_browser(&mut workspace);
        let original = summary("Original", "old");
        let renamed = summary("Renamed", "new");
        let writer_list = workspace.saved_chat_browsers[&writer].pending.unwrap();
        let stale_list = workspace.saved_chat_browsers[&peer].pending.unwrap();
        complete(
            &mut workspace,
            writer,
            writer_list,
            listing(0, vec![original.clone()]),
        );

        let _ = workspace.saved_chat_operation(
            writer,
            Operation::Rename(original.key.clone(), renamed.title.clone()),
        );
        let rename = workspace.saved_chat_browsers[&writer].pending.unwrap();
        let _ = workspace.close_saved_chats(writer);
        assert!(!workspace.saved_chat_browsers.contains_key(&writer));
        complete(
            &mut workspace,
            writer,
            rename,
            Change::Renamed(original.key.clone(), Box::new(renamed.clone())),
        );
        complete(&mut workspace, peer, stale_list, listing(0, vec![original]));
        let browser = &workspace.saved_chat_browsers[&peer];
        assert_eq!(browser.rows.len(), 1);
        assert!(browser.rows[0] == renamed);
        let fresh_list = browser.pending.unwrap();
        assert_ne!(fresh_list, stale_list);
        assert!(workspace.saved_chat_mutations.is_empty());

        complete(&mut workspace, peer, fresh_list, listing(1, vec![renamed]));
        assert!(workspace.saved_chat_browsers[&peer].pending.is_none());
    }

    #[test]
    fn saved_chat_delete_outlives_its_owner_and_invalidates_a_delayed_open() {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = true;
        let peer = install_browser(&mut workspace);
        workspace.window_settings.open_tool_windows_as_tabs = false;
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.install_saved_chats(owner, PathBuf::new());
        let writer = workspace
            .saved_chat_browsers
            .keys()
            .copied()
            .find(|&id| workspace.placed_dialog_owner(id) == Some(owner))
            .unwrap();
        let original = summary("Original", "old");
        for id in [writer, peer] {
            let token = workspace.saved_chat_browsers[&id].pending.unwrap();
            complete(
                &mut workspace,
                id,
                token,
                listing(0, vec![original.clone()]),
            );
        }
        workspace
            .saved_chat_browsers
            .get_mut(&peer)
            .unwrap()
            .selected = Some(original.key.clone());
        let _ = workspace.saved_chat_operation(peer, Operation::Open(original.key.clone()));
        let open = workspace.saved_chat_browsers[&peer].pending.unwrap();
        let _ = workspace.saved_chat_operation(writer, Operation::Delete(original.key.clone()));
        let delete = workspace.saved_chat_browsers[&writer].pending.unwrap();

        let _ = workspace.close_window(owner);
        assert!(!workspace.saved_chat_browsers.contains_key(&writer));
        complete(
            &mut workspace,
            writer,
            delete,
            Change::Deleted(original.key.clone(), true),
        );
        let loaded: Record = serde_json::from_value(serde_json::json!({
            "version": 1, "summary": original, "turns": []
        }))
        .unwrap();
        complete(&mut workspace, peer, open, Change::Open(Box::new(loaded)));
        let browser = &workspace.saved_chat_browsers[&peer];
        assert!(browser.rows.is_empty());
        assert!(browser.pending.is_none());
        assert!(browser.notice.contains("changed while it was opening"));
    }
}
