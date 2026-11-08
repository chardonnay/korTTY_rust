use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::{button, column, row, scrollable, space, text, text_input};
use iced::{Element, Length};

use crate::profile::projects::{ProjectFile, ProjectFolder};
use crate::sftp_window::{Entry, EntryKind, Location, list_local};

const PAGE_SIZE: usize = 60;
const MAX_PATH: usize = 4096;

#[derive(Clone, Debug)]
pub enum Event {
    Folder(String),
    List,
    Parent,
    Page(bool),
    Select(u64, usize),
    Filename(String),
    Catalogue(String),
    Continue,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Open,
    Save,
}

#[derive(Debug)]
pub struct Listing {
    folder: ProjectFolder,
    entries: Vec<Entry>,
}

impl Listing {
    pub(super) fn read(path: &Path) -> io::Result<Self> {
        let folder = ProjectFolder::inspect(path)?;
        let directory = list_local(folder.path())?;
        folder.require_current()?;
        let entries = directory
            .entries
            .into_iter()
            .filter(|entry| {
                let path = directory.path.with(&entry.name);
                entry.kind == EntryKind::Directory
                    || (entry.kind == EntryKind::File
                        && matches!(path, Some(Location::Local(path)) if ProjectFile::has_extension(&path)))
            })
            .collect();
        Ok(Self { folder, entries })
    }
}

pub(super) struct Browser {
    mode: Mode,
    folder: String,
    filename: String,
    catalogue: String,
    page: usize,
    listing: Option<(u64, Arc<Listing>)>,
}

pub(super) enum Intent {
    List(PathBuf),
    Open(ProjectFolder, ProjectFile),
    Save(ProjectFolder, String),
}

impl Browser {
    pub(super) fn new(mode: Mode) -> Self {
        Self {
            mode,
            folder: std::env::home_dir()
                .and_then(|path| path.to_str().map(str::to_owned))
                .unwrap_or_default(),
            filename: String::new(),
            catalogue: String::new(),
            page: 0,
            listing: None,
        }
    }

    pub(super) fn title(&self) -> &'static str {
        match self.mode {
            Mode::Open => "Open Project",
            Mode::Save => "Save Project",
        }
    }

    pub(super) fn listed(&mut self, ticket: u64, listing: Arc<Listing>) {
        self.folder = listing.folder.path().to_string_lossy().into_owned();
        self.listing = Some((ticket, listing));
        self.page = 0;
    }

    pub(super) fn update(&mut self, event: Event) -> io::Result<Option<Intent>> {
        let intent = match event {
            Event::Folder(path) if path.len() <= MAX_PATH => {
                self.folder = path;
                self.listing = None;
                None
            }
            Event::List => Some(Intent::List(PathBuf::from(self.folder.trim()))),
            Event::Parent => self.listing.as_ref().and_then(|(_, listing)| {
                listing
                    .folder
                    .path()
                    .parent()
                    .map(|path| Intent::List(path.to_owned()))
            }),
            Event::Page(forward) => {
                self.page = if forward {
                    (self.page + 1).min(self.max_page())
                } else {
                    self.page.saturating_sub(1)
                };
                None
            }
            Event::Select(ticket, index) => self.select(ticket, index)?,
            Event::Filename(value) if value.len() <= 240 => {
                self.filename = value;
                None
            }
            Event::Catalogue(path) if path.len() <= MAX_PATH => {
                self.catalogue = path;
                None
            }
            Event::Continue if self.mode == Mode::Save && !self.filename.trim().is_empty() => {
                self.listing.as_ref().map(|(_, listing)| {
                    Intent::Save(listing.folder.clone(), self.filename.trim().to_owned())
                })
            }
            _ => None,
        };
        if matches!(intent, Some(Intent::List(_))) {
            self.listing = None;
        }
        Ok(intent)
    }

    fn max_page(&self) -> usize {
        self.listing.as_ref().map_or(0, |(_, listing)| {
            listing.entries.len().saturating_sub(1) / PAGE_SIZE
        })
    }

    fn select(&mut self, ticket: u64, index: usize) -> io::Result<Option<Intent>> {
        let Some((_, listing)) = self
            .listing
            .as_ref()
            .filter(|(active, _)| *active == ticket && index / PAGE_SIZE == self.page)
        else {
            return Ok(None);
        };
        let Some(entry) = listing.entries.get(index) else {
            return Ok(None);
        };
        let Some(Location::Local(path)) =
            Location::Local(listing.folder.path().to_owned()).with(&entry.name)
        else {
            return Ok(None);
        };
        if entry.kind == EntryKind::Directory {
            return Ok(Some(Intent::List(path)));
        }
        match self.mode {
            Mode::Open => {
                let catalogue = (!self.catalogue.trim().is_empty())
                    .then(|| PathBuf::from(self.catalogue.trim()));
                let file = ProjectFile::new(path, catalogue)?;
                Ok(Some(Intent::Open(listing.folder.clone(), file)))
            }
            Mode::Save => {
                if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                    self.filename = name.to_owned();
                }
                Ok(None)
            }
        }
    }

    pub(super) fn view(&self, loading: bool) -> Element<'_, Event> {
        let parent = self
            .listing
            .as_ref()
            .is_some_and(|(_, listing)| listing.folder.path().parent().is_some());
        let folder = row![
            button("Parent folder").on_press_maybe(parent.then_some(Event::Parent)),
            text_input("Absolute folder path", &self.folder)
                .on_input_maybe((!loading).then_some(Event::Folder))
                .on_submit_maybe((!loading).then_some(Event::List)),
            button("Go").on_press_maybe((!loading).then_some(Event::List)),
        ]
        .spacing(8);
        let mut entries = column![].spacing(2);
        if let Some((ticket, listing)) = &self.listing {
            if listing.entries.is_empty() {
                entries =
                    entries.push(text("This folder contains no project files or subfolders."));
            }
            for (index, entry) in listing
                .entries
                .iter()
                .enumerate()
                .skip(self.page * PAGE_SIZE)
                .take(PAGE_SIZE)
            {
                let label = if entry.kind == EntryKind::Directory {
                    format!("{}/", entry.display)
                } else {
                    entry.display.clone()
                };
                entries = entries.push(
                    button(text(label))
                        .style(button::text)
                        .width(Length::Fill)
                        .on_press(Event::Select(*ticket, index)),
                );
            }
        } else if loading {
            entries = entries.push(text("Loading…"));
        }
        let controls = row![
            button("Previous").on_press_maybe((self.page > 0).then_some(Event::Page(false))),
            button("Next")
                .on_press_maybe((self.page < self.max_page()).then_some(Event::Page(true))),
        ]
        .spacing(8);
        let fields: Element<'_, Event> = match self.mode {
            Mode::Open => text_input("Java connections.xml path (optional)", &self.catalogue)
                .on_input_maybe((!loading).then_some(Event::Catalogue))
                .into(),
            Mode::Save => {
                let ready = !loading && self.listing.is_some() && !self.filename.trim().is_empty();
                row![
                    text_input("Project filename", &self.filename)
                        .on_input_maybe((!loading).then_some(Event::Filename))
                        .on_submit_maybe(ready.then_some(Event::Continue)),
                    space::horizontal().width(8),
                    button("Project settings…").on_press_maybe(ready.then_some(Event::Continue)),
                ]
                .into()
            }
        };
        column![folder, scrollable(entries).height(290), controls, fields]
            .spacing(8)
            .into()
    }
}
