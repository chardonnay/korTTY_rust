use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::{button, column, row, scrollable, space, text, text_input};
use iced::{Alignment, Element, Length};

use crate::sftp_window::{Directory, EntryKind, Location};

const PAGE_SIZE: usize = 80;
const MAX_PATH: usize = 8192;

#[derive(Clone, Debug)]
pub enum Message {
    Back,
    Parent,
    Page(bool),
    PathChanged(String),
    OpenPath,
    Select(u64, usize),
}

pub(super) struct Browser {
    token: u64,
    pub(super) folder: PathBuf,
    page: usize,
    listing: Option<Result<Vec<Entry>, String>>,
    path: String,
    pub(super) problem: Option<&'static str>,
}

struct Entry {
    path: PathBuf,
    label: String,
    kind: EntryKind,
}

impl Browser {
    pub(super) fn new(token: u64, folder: PathBuf) -> Self {
        Self {
            token,
            folder,
            page: 0,
            listing: None,
            path: String::new(),
            problem: None,
        }
    }

    pub(super) fn listed(&mut self, token: u64, result: Result<Arc<Directory>, String>) {
        if self.token != token {
            return;
        }
        self.listing = Some(result.map(|directory| {
            if let Location::Local(folder) = &directory.path {
                self.folder.clone_from(folder);
            }
            directory
                .entries
                .iter()
                .filter_map(|entry| {
                    let Location::Local(path) = directory.path.with(&entry.name)? else {
                        return None;
                    };
                    let label = match entry.kind {
                        EntryKind::Directory => format!("{}/", entry.display),
                        EntryKind::File if image_extension(&path) => entry.display.clone(),
                        _ => return None,
                    };
                    Some(Entry {
                        path,
                        label,
                        kind: entry.kind,
                    })
                })
                .collect()
        }));
    }

    pub(super) fn set_path(&mut self, path: String) {
        if path.len() <= MAX_PATH && !path.contains('\0') {
            self.path = path;
            self.problem = None;
        }
    }

    pub(super) fn typed_path(&self) -> Option<PathBuf> {
        let path = Path::new(&self.path);
        path.is_absolute().then(|| path.to_owned())
    }

    fn max_page(&self) -> usize {
        self.listing
            .as_ref()
            .and_then(|listing| listing.as_ref().ok())
            .map_or(0, |entries| entries.len().saturating_sub(1) / PAGE_SIZE)
    }

    pub(super) fn step(&mut self, forward: bool) {
        self.page = if forward {
            (self.page + 1).min(self.max_page())
        } else {
            self.page.saturating_sub(1)
        };
    }

    pub(super) fn selection(&self, token: u64, index: usize) -> Option<(PathBuf, EntryKind)> {
        if token != self.token || index / PAGE_SIZE != self.page {
            return None;
        }
        let entry = self.listing.as_ref()?.as_ref().ok()?.get(index)?;
        Some((entry.path.clone(), entry.kind))
    }

    pub(super) fn view(&self) -> Element<'_, Message> {
        let mut rows = column![].spacing(2);
        match &self.listing {
            None => rows = rows.push(text("Loading folder…")),
            Some(Err(error)) => rows = rows.push(text(format!("Could not open folder ({error})."))),
            Some(Ok(entries)) if entries.is_empty() => {
                rows = rows.push(text("No image files or folders."))
            }
            Some(Ok(entries)) => {
                for (index, entry) in entries
                    .iter()
                    .enumerate()
                    .skip(self.page * PAGE_SIZE)
                    .take(PAGE_SIZE)
                {
                    rows = rows.push(
                        button(text(entry.label.as_str()))
                            .style(button::text)
                            .width(Length::Fill)
                            .on_press(Message::Select(self.token, index)),
                    );
                }
            }
        }
        let mut body = column![
            text("Choose an image").size(20),
            row![
                button("Parent folder").on_press_maybe(
                    (self.folder.parent().is_some() && self.listing.is_some())
                        .then_some(Message::Parent)
                ),
                text(Location::Local(self.folder.clone()).display()).width(Length::Fill),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            scrollable(rows).height(Length::Fill),
            row![
                button("Previous").on_press_maybe((self.page > 0).then_some(Message::Page(false))),
                button("Next")
                    .on_press_maybe((self.page < self.max_page()).then_some(Message::Page(true))),
                space::horizontal(),
                button("Back").on_press(Message::Back),
            ]
            .spacing(8),
            row![
                text_input("Absolute image path", &self.path)
                    .on_input(Message::PathChanged)
                    .on_submit(Message::OpenPath),
                button("Open").on_press_maybe(self.typed_path().map(|_| Message::OpenPath)),
            ]
            .spacing(8),
        ];
        if let Some(problem) = self.problem {
            body = body.push(text(problem));
        }
        body.padding(16).spacing(8).height(Length::Fill).into()
    }
}

fn image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["png", "jpg", "jpeg", "gif", "bmp", "wbmp"]
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}
