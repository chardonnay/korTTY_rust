//! Native destination selection with a verified directory identity.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::{button, column, container, row, scrollable, space, text, text_input};
use iced::{Element, Length, Task, window};

use crate::private_files::{Directory, EntryIdentity};
use crate::session_journal::export_output::{Destination, valid_component};
use crate::sftp_window::{Entry, EntryKind, EntryName, Location, list_local};

use super::{Desktop, JournalMessage, Message};

mod selection;
pub(in crate::workspace) use selection::{ReviewedSelection, Selection};

const PAGE_SIZE: usize = 80;
const MAX_PATH_BYTES: usize = 4096;
const MAX_NAME_BYTES: usize = 240;

#[derive(Clone, Debug)]
pub enum Event {
    Folder(String),
    List,
    Parent,
    Open(u64, usize),
    Page(bool),
    Name(String),
    Listed(u64, Result<Arc<Listing>, String>),
    Save,
    Cancel,
}

#[derive(Debug)]
pub struct Listing {
    path: PathBuf,
    identity: EntryIdentity,
    entries: Vec<Entry>,
}

pub(in crate::workspace) struct Browser {
    folder: String,
    filename: String,
    suffix: &'static str,
    revision: u64,
    loading: bool,
    listing: Option<Arc<Listing>>,
    page: usize,
    problem: Option<String>,
    route: fn(Event) -> JournalMessage,
}

impl Browser {
    #[cfg(test)]
    pub(in crate::workspace::journal_access) fn complete_listing_for_test(
        &self,
        path: &Path,
    ) -> Event {
        // Test double for Iced delivery. Resolve the actual directory and its identity.
        Event::Listed(
            self.revision,
            list(path, self.suffix)
                .map(Arc::new)
                .map_err(|error| error.to_string()),
        )
    }

    pub(in crate::workspace) fn new(filename: String, suffix: &'static str) -> Self {
        Self {
            folder: std::env::home_dir()
                .filter(|path| path.is_absolute())
                .and_then(|path| path.to_str().map(str::to_owned))
                .unwrap_or_default(),
            filename,
            suffix,
            revision: 0,
            loading: false,
            listing: None,
            page: 0,
            problem: None,
            route: |event| JournalMessage::Export(Message::Browser(event)),
        }
    }

    pub(in crate::workspace::journal_access) fn with_route(
        mut self,
        route: fn(Event) -> JournalMessage,
    ) -> Self {
        self.route = route;
        self
    }

    pub(in crate::workspace::journal_access) fn destination(&self) -> io::Result<Destination> {
        let selected = self.selection()?;
        Destination::new(selected.path().to_owned(), selected.identity())
    }

    pub(in crate::workspace) fn selection(&self) -> io::Result<Selection> {
        let listing = self
            .listing
            .as_ref()
            .filter(|_| !self.loading)
            .ok_or_else(|| io::Error::other("Choose an export folder."))?;
        let name = filename(&self.filename, self.suffix)?;
        Ok(Selection::new(listing.path.join(name), listing.identity))
    }

    pub(in crate::workspace) fn set_problem(&mut self, problem: String) {
        self.problem = Some(problem);
    }

    pub(in crate::workspace::journal_access) fn update(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Desktop> {
        let route = self.route;
        self.update_event(event)
            .map(move |event| Desktop::SessionJournal(id, route(event)))
    }

    pub(in crate::workspace) fn update_event(&mut self, event: Event) -> Task<Event> {
        match event {
            Event::Folder(value) if !self.loading && value.len() <= MAX_PATH_BYTES => {
                self.folder = value;
                self.listing = None;
                self.problem = None;
            }
            Event::Name(value) if value.len() <= MAX_NAME_BYTES => {
                self.filename = value;
                self.problem = None;
            }
            Event::List if !self.loading => {
                let path = PathBuf::from(self.folder.trim());
                if !path.is_absolute() || self.folder.chars().any(char::is_control) {
                    self.problem = Some("Enter an absolute folder path.".into());
                    return Task::none();
                }
                return self.list(path);
            }
            Event::Parent if !self.loading => {
                let path = self
                    .listing
                    .as_ref()
                    .and_then(|listing| listing.path.parent());
                if let Some(path) = path {
                    return self.list(path.to_owned());
                }
            }
            Event::Open(revision, index) if !self.loading && revision == self.revision => {
                let child = self.listing.as_ref().and_then(|listing| {
                    let entry = listing.entries.get(index)?;
                    match (&entry.name, entry.kind) {
                        (EntryName::Local(name), EntryKind::Directory) => {
                            Some(listing.path.join(name))
                        }
                        _ => None,
                    }
                });
                if let Some(child) = child {
                    return self.list(child);
                }
            }
            Event::Page(forward) => {
                let maximum = self.listing.as_ref().map_or(0, |listing| {
                    listing.entries.len().saturating_sub(1) / PAGE_SIZE
                });
                self.page = if forward {
                    (self.page + 1).min(maximum)
                } else {
                    self.page.saturating_sub(1)
                };
            }
            Event::Listed(revision, result) if revision == self.revision && self.loading => {
                self.loading = false;
                match result {
                    Ok(listing) => {
                        self.folder = listing.path.to_string_lossy().into_owned();
                        self.listing = Some(listing);
                    }
                    Err(problem) => self.problem = Some(problem),
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn list(&mut self, path: PathBuf) -> Task<Event> {
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        self.loading = true;
        self.listing = None;
        self.page = 0;
        self.problem = None;
        let suffix = self.suffix;
        Task::future(async move {
            let (send, receive) = async_channel::bounded(1);
            let worker = std::thread::Builder::new()
                .name("kortty-export-folder".into())
                .spawn(move || {
                    let _ = send.try_send(
                        list(&path, suffix)
                            .map(Arc::new)
                            .map_err(|error| error.to_string()),
                    );
                });
            let result = match worker {
                Ok(_) => receive
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("Export folder reader stopped.".into())),
                Err(error) => Err(error.to_string()),
            };
            Event::Listed(revision, result)
        })
    }

    pub(super) fn view(&self) -> Element<'_, Event> {
        self.view_with_title("Save Session Journal")
    }

    pub(in crate::workspace) fn view_with_title(&self, title: &'static str) -> Element<'_, Event> {
        let mut folder = text_input("Absolute folder path", &self.folder);
        if !self.loading {
            folder = folder.on_input(Event::Folder).on_submit(Event::List);
        }
        let mut rows = column![].spacing(3);
        if self.loading {
            rows = rows.push(text("Reading folder…"));
        }
        if let Some(listing) = &self.listing {
            for (index, entry) in listing
                .entries
                .iter()
                .enumerate()
                .skip(self.page * PAGE_SIZE)
                .take(PAGE_SIZE)
            {
                rows = if entry.kind == EntryKind::Directory {
                    rows.push(
                        button(text(super::super::label(&entry.display, 200)))
                            .style(button::text)
                            .on_press(Event::Open(self.revision, index)),
                    )
                } else {
                    rows.push(text(super::super::label(&entry.display, 200)).size(13))
                };
            }
        }
        let next = self
            .listing
            .as_ref()
            .is_some_and(|listing| (self.page + 1) * PAGE_SIZE < listing.entries.len());
        container(
            column![
                text(title).size(19),
                row![
                    button("Up").on_press_maybe(
                        self.listing
                            .as_ref()
                            .and_then(|listing| listing.path.parent())
                            .map(|_| Event::Parent)
                    ),
                    folder,
                    button("Go").on_press_maybe((!self.loading).then_some(Event::List)),
                ]
                .spacing(8),
                scrollable(rows).height(Length::Fill),
                (self.page > 0 || next).then(|| row![
                    button("Previous")
                        .on_press_maybe((self.page > 0).then_some(Event::Page(false))),
                    button("Next").on_press_maybe(next.then_some(Event::Page(true))),
                ]
                .spacing(8)),
                row![
                    text("File name"),
                    text_input("Export file name", &self.filename)
                        .on_input(Event::Name)
                        .on_submit(Event::Save),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                self.problem
                    .as_deref()
                    .map(|problem| text(super::super::display(problem, 8192)).size(13)),
                row![
                    space::horizontal(),
                    button("Cancel").on_press(Event::Cancel),
                    button("Save")
                        .on_press_maybe(self.destination().is_ok().then_some(Event::Save)),
                ]
                .spacing(8),
            ]
            .spacing(10),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(16)
        .into()
    }
}

fn list(path: &Path, suffix: &str) -> io::Result<Listing> {
    let path = path.canonicalize()?;
    let directory = Directory::open(&path, false)?;
    let listing = list_local(&path)?;
    if listing.path != Location::Local(path.clone())
        || !directory.same_as(&Directory::open(&path, false)?)?
    {
        return Err(io::Error::other(
            "The export folder changed. Choose it again.",
        ));
    }
    let entries = listing
        .entries
        .into_iter()
        .filter(|entry| {
            !entry.hidden
                && (entry.kind == EntryKind::Directory
                    || entry.kind == EntryKind::File && has_suffix(&entry.display, suffix))
        })
        .collect();
    Ok(Listing {
        path,
        identity: directory.identity()?,
        entries,
    })
}

fn has_suffix(name: &str, suffix: &str) -> bool {
    name.len()
        .checked_sub(suffix.len())
        .and_then(|start| name.get(start..))
        .is_some_and(|ending| ending.eq_ignore_ascii_case(suffix))
}

fn filename(value: &str, suffix: &str) -> io::Result<String> {
    let value = value.trim();
    let stem = if has_suffix(value, suffix) {
        &value[..value.len() - suffix.len()]
    } else {
        value
    };
    if value.starts_with('.') || value.len() > MAX_NAME_BYTES || !valid_component(stem) {
        return Err(io::Error::other("Enter a single, valid export file name."));
    }
    Ok(if has_suffix(value, suffix) {
        value.to_owned()
    } else {
        format!("{value}{suffix}")
    })
}
