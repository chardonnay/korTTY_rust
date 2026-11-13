//! Bounded SFTP-only search and exact, read-only result navigation.

use std::fmt;
use std::io::{self, Read, Write};
use std::sync::Arc;

use globset::{Candidate, GlobBuilder, GlobMatcher};
use iced::widget::{self, button, column, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length, Task};
use kortty_ssh::sftp::{Attributes, FileKind, Sftp};

use super::tree::snapshot::{
    self, DirectoryStamp, Entry as SnapshotEntry, Kind, Manifest, Relative,
};
use super::{Directory, Entry, EntryName, Location, bounded, worker};

const MAX_PATTERN_BYTES: usize = 512;
const MAX_HITS: usize = 1024;
const PAGE_SIZE: usize = 60;

enum Matcher {
    Substring(String),
    Wildcard(Box<GlobMatcher>),
}

impl Matcher {
    fn parse(value: &str) -> Result<Self, &'static str> {
        if value.contains(['*', '?', '[', '{']) {
            if value.len() > MAX_PATTERN_BYTES || value.contains(['/', '\\', '\0']) {
                return Err("Use a filename wildcard of at most 512 bytes");
            }
            let pattern = GlobBuilder::new(value)
                .literal_separator(true)
                .case_insensitive(true)
                .build()
                .map_err(|_| "Invalid filename wildcard")?;
            Ok(Self::Wildcard(Box::new(pattern.compile_matcher())))
        } else {
            Ok(Self::Substring(value.to_lowercase()))
        }
    }

    fn matches(&self, raw: &[u8], display: &str) -> bool {
        match self {
            Self::Substring(needle) => needle.is_empty() || display.to_lowercase().contains(needle),
            Self::Wildcard(glob) => glob.is_match_candidate(&Candidate::from_bytes(raw)),
        }
    }
}

#[derive(Default)]
pub(super) struct Filter {
    input: String,
    matcher: Option<Matcher>,
}

impl Filter {
    pub(super) fn edit(&mut self, value: String) {
        self.input = bounded(value);
        self.matcher = Some(
            Matcher::parse(&self.input)
                .unwrap_or_else(|_| Matcher::Substring(self.input.to_lowercase())),
        );
    }

    pub(super) fn input(&self) -> &str {
        &self.input
    }

    pub(super) fn matches(&self, entry: &Entry) -> bool {
        let raw = match &entry.name {
            EntryName::Local(name) => name.as_encoded_bytes(),
            EntryName::Remote(name) => name.as_slice(),
        };
        self.matcher
            .as_ref()
            .is_none_or(|matcher| matcher.matches(raw, &entry.display))
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Input(u64, String),
    Start(u64),
    Page(u64, usize),
    Open(u64, usize),
}

pub(super) struct Setup {
    ticket: u64,
    root: Vec<u8>,
    input: String,
    hide_hidden: bool,
    error: Option<&'static str>,
}

impl Setup {
    pub(super) fn new(ticket: u64, root: Vec<u8>, input: &str, hide_hidden: bool) -> Self {
        let input = if input.len() <= MAX_PATTERN_BYTES {
            input.to_owned()
        } else {
            String::new()
        };
        Self {
            ticket,
            root,
            input,
            hide_hidden,
            error: None,
        }
    }

    pub(super) fn edit(&mut self, ticket: u64, input: String) {
        if ticket == self.ticket {
            if input.len() <= MAX_PATTERN_BYTES {
                self.input = input;
                self.error = None;
            } else {
                self.error = Some("Search is limited to 512 bytes");
            }
        }
    }

    pub(super) fn start(&mut self, ticket: u64) -> Option<Request> {
        if self.ticket != ticket || self.error.is_some() {
            return None;
        }
        if self.input.trim().is_empty() {
            self.error = Some("Enter a filename fragment or wildcard");
            return None;
        }
        match Matcher::parse(&self.input) {
            Ok(matcher) => Some(Request {
                root: self.root.clone(),
                matcher,
                hide_hidden: self.hide_hidden,
            }),
            Err(error) => {
                self.error = Some(error);
                None
            }
        }
    }

    pub(super) fn view(&self) -> Element<'_, super::compare::Message> {
        use super::compare::Message as Outer;

        let ticket = self.ticket;
        let mut form = column![
            text("Search server folders").size(19),
            text(self.root.escape_ascii().to_string()).wrapping(text::Wrapping::WordOrGlyph),
            text_input("Filename fragment or wildcard, such as *.log", &self.input)
                .on_input(move |value| Outer::Search(Message::Input(ticket, value)))
                .on_submit(Outer::Search(Message::Start(ticket))),
            text(if self.hide_hidden {
                "Hidden names and their subfolders are excluded."
            } else {
                "Hidden names are included."
            }),
        ]
        .spacing(8);
        if let Some(error) = self.error {
            form = form.push(text(error));
        }
        form.push(
            row![
                button("Search").on_press(Outer::Search(Message::Start(ticket))),
                button("Cancel").on_press(Outer::Close),
            ]
            .spacing(8),
        )
        .padding(14)
        .into()
    }
}

pub(crate) struct Request {
    root: Vec<u8>,
    matcher: Matcher,
    hide_hidden: bool,
}

pub struct Review {
    manifest: Manifest,
    hits: Vec<Relative>,
    total: usize,
}

impl fmt::Debug for Review {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("SftpSearch")
            .field("matches", &self.total)
            .finish_non_exhaustive()
    }
}

pub(super) fn read(
    client: &mut Sftp<impl Read, impl Write>,
    request: Request,
    cancelled: impl Fn() -> bool,
) -> Result<Review, worker::Failure> {
    let Request {
        root,
        matcher,
        hide_hidden,
    } = request;
    let manifest = snapshot::read_remote_search(
        client,
        root,
        |path| {
            hide_hidden
                && path
                    .rsplit(|&byte| byte == b'/')
                    .next()
                    .is_some_and(|name| name.starts_with(b"."))
        },
        &cancelled,
    )?;
    let mut hits = Vec::new();
    let mut total = 0;
    for (path, entry) in &manifest.entries {
        if cancelled() {
            return Err(io::Error::other("Server search cancelled").into());
        }
        let raw = path.basename();
        if entry.kind() != Kind::Blocked && matcher.matches(raw, &String::from_utf8_lossy(raw)) {
            total += 1;
            if hits.len() < MAX_HITS {
                hits.push(path.clone());
            }
        }
    }
    Ok(Review {
        manifest,
        hits,
        total,
    })
}

pub(super) struct View {
    ticket: u64,
    review: Arc<Review>,
    page: usize,
    scroll: widget::Id,
}

impl View {
    pub(super) fn new(ticket: u64, review: Arc<Review>) -> Self {
        Self {
            ticket,
            review,
            page: 0,
            scroll: widget::Id::unique(),
        }
    }

    pub(super) fn update(&mut self, ticket: u64, page: usize) -> Task<super::compare::Message> {
        if self.ticket != ticket || page >= self.pages() {
            return Task::none();
        }
        self.page = page;
        widget::operation::scroll_to(
            self.scroll.clone(),
            widget::operation::AbsoluteOffset { x: 0.0, y: 0.0 },
        )
    }

    fn pages(&self) -> usize {
        self.review.hits.len().div_ceil(PAGE_SIZE).max(1)
    }

    pub(super) fn view(&self) -> Element<'_, super::compare::Message> {
        use super::compare::Message as Outer;

        let pages = self.pages();
        let rows = self
            .review
            .hits
            .iter()
            .enumerate()
            .skip(self.page * PAGE_SIZE)
            .take(PAGE_SIZE)
            .fold(column![].spacing(3), |rows, (index, path)| {
                let label = if self.review.manifest.entries[path].kind() == Kind::Folder {
                    "Open folder"
                } else {
                    "Show file"
                };
                rows.push(
                    row![
                        text(path.review_label())
                            .width(Length::Fill)
                            .wrapping(text::Wrapping::WordOrGlyph),
                        button(label).on_press(Outer::Search(Message::Open(self.ticket, index))),
                    ]
                    .align_y(Alignment::Center)
                    .spacing(8),
                )
            });
        column![
            text("Server search results").size(19),
            text(format!(
                "{} matches · {} shown",
                self.review.total,
                self.review.hits.len()
            )),
            scrollable(rows)
                .id(self.scroll.clone())
                .height(Length::Fill),
            row![
                button("Previous").on_press_maybe(
                    self.page
                        .checked_sub(1)
                        .map(|page| Outer::Search(Message::Page(self.ticket, page)))
                ),
                text(format!("{} / {pages}", self.page + 1)),
                button("Next").on_press_maybe(
                    (self.page + 1 < pages)
                        .then_some(Outer::Search(Message::Page(self.ticket, self.page + 1)))
                ),
                button("Close").on_press(Outer::Close),
            ]
            .align_y(Alignment::Center)
            .spacing(8),
        ]
        .height(Length::Fill)
        .padding(14)
        .spacing(8)
        .into()
    }

    pub(super) fn hit(&self, ticket: u64, index: usize) -> Option<Hit> {
        if self.ticket != ticket {
            return None;
        }
        let path = self.review.hits.get(index)?;
        let manifest = &self.review.manifest;
        let SnapshotEntry::Remote(attributes) = manifest.entries.get(path)? else {
            return None;
        };
        let Location::Remote(target) = manifest.location(path).ok()? else {
            return None;
        };
        Some(Hit {
            root: manifest.root_directory(),
            parent: manifest.parent(path)?,
            target,
            attributes: *attributes,
            name: path.basename().to_vec(),
        })
    }
}

pub(crate) struct Hit {
    root: (Location, DirectoryStamp),
    parent: (Location, DirectoryStamp),
    target: Vec<u8>,
    attributes: Attributes,
    name: Vec<u8>,
}

impl Hit {
    fn verify(&self, client: &mut Sftp<impl Read, impl Write>) -> Result<(), worker::Failure> {
        self.root.1.verify(client, &self.root.0, true)?;
        if self.parent.0 != self.root.0 {
            self.parent.1.verify(client, &self.parent.0, true)?;
        }
        if !worker::unchanged(self.attributes, client.lstat(&self.target)?) {
            return Err(io::Error::other(
                "The search result changed. Search again before opening it.",
            )
            .into());
        }
        if self.attributes.kind() == FileKind::Directory
            && client.canonicalize(&self.target)? != self.target
        {
            return Err(io::Error::other("The search result folder changed").into());
        }
        Ok(())
    }

    pub(super) fn reveal<R: Read, W: Write>(
        self,
        client: &mut Sftp<R, W>,
        list: impl FnOnce(&mut Sftp<R, W>, &Location) -> Result<Arc<Directory>, worker::Failure>,
    ) -> Result<(Arc<Directory>, Option<EntryName>), worker::Failure> {
        self.verify(client)?;
        let is_folder = self.attributes.kind() == FileKind::Directory;
        let location = if is_folder {
            Location::Remote(self.target.clone())
        } else {
            self.parent.0.clone()
        };
        let directory = list(client, &location)?;
        self.verify(client)?;
        let selected = (!is_folder).then_some(EntryName::Remote(self.name));
        if directory.path != location
            || selected.as_ref().is_some_and(|name| {
                !directory
                    .entries
                    .iter()
                    .any(|entry| entry.name == *name && entry.kind == super::EntryKind::File)
            })
        {
            return Err(
                io::Error::other("The search result is no longer in the expected folder").into(),
            );
        }
        Ok((directory, selected))
    }
}
