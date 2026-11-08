//! Native local public-certificate picker.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::{button, column, row, scrollable, space, text};
use iced::{Alignment, Element, Length, Task, window};

use crate::sftp_window::{Directory, EntryKind, Location, list_local};

use super::{Desktop, Message as Parent, PageMessage, Window, Workspace, file_task, wrap};

const PAGE_SIZE: usize = 80;
const MAX_PATH_BYTES: usize = 8192;

#[derive(Clone)]
pub enum Message {
    Back,
    Parent,
    Page(bool),
    Select(u64, usize),
    Listed(u64, Result<Arc<Directory>, String>),
}

pub(super) struct Browser {
    token: u64,
    folder: PathBuf,
    page: usize,
    listing: Option<Result<Arc<Directory>, String>>,
}

impl Browser {
    fn count(&self) -> usize {
        self.directory()
            .map_or(0, |directory| directory.entries.len())
    }

    fn directory(&self) -> Option<&Directory> {
        self.listing.as_ref()?.as_ref().ok().map(Arc::as_ref)
    }

    fn max_page(&self) -> usize {
        self.count().saturating_sub(1) / PAGE_SIZE
    }

    fn selection(&self, token: u64, index: usize) -> Option<(PathBuf, EntryKind)> {
        if token != self.token || index / PAGE_SIZE != self.page {
            return None;
        }
        let directory = self.directory()?;
        let entry = directory.entries.get(index)?;
        let Location::Local(path) = directory.path.with(&entry.name)? else {
            return None;
        };
        Some((path, entry.kind))
    }

    pub(super) fn view(&self) -> Element<'_, Message> {
        let mut entries = column![].spacing(2);
        match &self.listing {
            None => entries = entries.push(text("Loading folder…")),
            Some(Err(error)) => {
                entries = entries.push(text(format!("Could not open folder ({error}).")))
            }
            Some(Ok(directory)) if directory.entries.is_empty() => {
                entries = entries.push(text("This folder is empty."));
            }
            Some(Ok(directory)) => {
                for (index, entry) in directory
                    .entries
                    .iter()
                    .enumerate()
                    .skip(self.page * PAGE_SIZE)
                    .take(PAGE_SIZE)
                {
                    let name = if entry.kind == EntryKind::Directory {
                        format!("{}/", entry.display)
                    } else {
                        entry.display.clone()
                    };
                    entries = entries.push(
                        button(text(name))
                            .style(button::text)
                            .width(Length::Fill)
                            .on_press_maybe(
                                entry
                                    .kind
                                    .transferable()
                                    .then_some(Message::Select(self.token, index)),
                            ),
                    );
                }
            }
        }
        column![
            text("Choose a public certificate").size(20),
            row![
                button("Parent folder").on_press_maybe(
                    (self.listing.is_some() && self.folder.parent().is_some())
                        .then_some(Message::Parent)
                ),
                text(Location::Local(self.folder.clone()).display()).size(14),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            scrollable(entries).height(Length::Fill),
            row![
                button("Previous").on_press_maybe((self.page > 0).then_some(Message::Page(false))),
                button("Next")
                    .on_press_maybe((self.page < self.max_page()).then_some(Message::Page(true))),
                space::horizontal(),
                button("Back").on_press(Message::Back),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .padding(16)
        .spacing(8)
        .height(Length::Fill)
        .into()
    }
}

impl Workspace {
    pub(super) fn start_openpgp_browser(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(state) = self.openpgp.get(&id).filter(|state| {
            state.closing.is_none()
                && state.hosted_close.is_none()
                && state.page.can_browse_certificate()
                && state.browser_work.is_none()
        }) else {
            return Task::none();
        };
        let folder = state
            .page
            .certificate_path()
            .and_then(Path::parent)
            .map(Path::to_owned)
            .or_else(std::env::home_dir)
            .or_else(|| state.path.parent().map(Path::to_owned));
        folder.map_or_else(Task::none, |folder| self.list_openpgp_folder(id, folder))
    }

    pub(super) fn update_openpgp_browser(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        let Some(state) = self.openpgp.get_mut(&id) else {
            return Task::none();
        };
        if let Message::Listed(token, result) = message {
            if state.browser_work == Some(token) {
                state.browser_work = None;
                if let Some(browser) = state
                    .browser
                    .as_mut()
                    .filter(|browser| browser.token == token)
                {
                    if let Ok(directory) = &result
                        && let Location::Local(folder) = &directory.path
                    {
                        browser.folder.clone_from(folder);
                    }
                    browser.listing = Some(result);
                }
            }
            return Task::none();
        }
        if self.exiting || self.scheduler_draining {
            return Task::none();
        }
        let Some(browser) = state.browser.as_mut() else {
            return Task::none();
        };
        match message {
            Message::Back => state.browser = None,
            Message::Parent if browser.listing.is_some() => {
                let folder = browser.folder.parent().map(Path::to_owned);
                return folder
                    .map_or_else(Task::none, |folder| self.list_openpgp_folder(id, folder));
            }
            Message::Page(forward) => {
                browser.page = if forward {
                    (browser.page + 1).min(browser.max_page())
                } else {
                    browser.page.saturating_sub(1)
                };
            }
            Message::Select(token, index) => {
                if let Some((path, kind)) = browser.selection(token, index) {
                    match kind {
                        EntryKind::Directory => return self.list_openpgp_folder(id, path),
                        EntryKind::File => return state.select_certificate(path),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn list_openpgp_folder(&mut self, id: window::Id, folder: PathBuf) -> Task<Desktop> {
        let token = self.allocate();
        let Some(state) = self
            .openpgp
            .get_mut(&id)
            .filter(|state| state.browser_work.is_none())
        else {
            return Task::none();
        };
        state.browser_work = Some(token);
        state.browser = Some(Browser {
            token,
            folder: folder.clone(),
            page: 0,
            listing: None,
        });
        file_task(
            move || list_local(&folder).map(Arc::new),
            move |result| wrap(id, Parent::Browser(Message::Listed(token, result))),
        )
    }
}

impl Window {
    fn select_certificate(&mut self, path: PathBuf) -> Task<Desktop> {
        let Some(path) = path.to_str().filter(|path| path.len() <= MAX_PATH_BYTES) else {
            return Task::none();
        };
        if !self.page.can_browse_certificate() {
            return Task::none();
        }
        self.browser = None;
        self.page
            .update(PageMessage::Certificate(path.to_owned()), &self.path);
        self.preview_certificate()
    }
}
