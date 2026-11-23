//! Shared local file and folder selection for AI Manager.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::{button, checkbox, column, operation, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length, Task};

use crate::sftp_window::{Directory, EntryKind, Location, list_local};

const PAGE_SIZE: usize = 64;
const MAX_PATH_BYTES: usize = 4096;
pub(super) const MAX_MARKDOWN_FILES: usize = 32;

#[derive(Clone)]
pub(crate) enum Message {
    Folder(String),
    Go,
    Up,
    Listed(u64, Result<Arc<Directory>, String>),
    Entry(u64, usize),
    Page(bool),
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Mode {
    #[default]
    Markdown,
    ExportFolder,
    KnowledgeFiles(usize),
    KnowledgeFolder,
}

impl Mode {
    fn file_limit(self) -> usize {
        match self {
            Self::Markdown => MAX_MARKDOWN_FILES,
            Self::KnowledgeFiles(limit) => limit,
            Self::ExportFolder | Self::KnowledgeFolder => 0,
        }
    }

    fn allows_file(self, path: &Path) -> bool {
        match self {
            Self::Markdown => markdown_path(path),
            Self::KnowledgeFiles(_) => super::local::knowledge_file_supported(path),
            Self::ExportFolder | Self::KnowledgeFolder => false,
        }
    }
}

#[derive(Default)]
pub(super) struct Chooser {
    mode: Mode,
    folder: String,
    directory: Option<Arc<Directory>>,
    page: usize,
    generation: u64,
    loading: bool,
    selected: Vec<PathBuf>,
    problem: Option<String>,
}

impl Chooser {
    pub(super) fn open(
        mode: Mode,
        start: Option<PathBuf>,
        generation: &mut u64,
    ) -> (Self, Task<Message>) {
        let mut chooser = Self {
            mode,
            ..Self::default()
        };
        let start = start
            .or_else(std::env::home_dir)
            .or_else(|| std::env::current_dir().ok());
        let task = if let Some(folder) = start {
            chooser.browse(folder, generation)
        } else {
            chooser.problem = Some("Could not determine a starting folder.".into());
            Task::none()
        };
        (chooser, task)
    }

    pub(super) fn selection(&self) -> &[PathBuf] {
        &self.selected
    }

    pub(super) fn update(&mut self, message: Message, generation: &mut u64) -> Task<Message> {
        match message {
            Message::Listed(ticket, result) if self.loading && ticket == self.generation => {
                self.loading = false;
                match result {
                    Ok(directory) => {
                        if let Location::Local(path) = &directory.path {
                            self.folder = display_path(path).into_owned();
                        }
                        self.directory = Some(directory);
                        return operation::snap_to(list_id(), operation::RelativeOffset::START);
                    }
                    Err(error) => self.problem = Some(error),
                }
            }
            _ if self.loading => {}
            Message::Folder(folder) => {
                self.folder = folder[..folder.floor_char_boundary(MAX_PATH_BYTES)].to_owned();
                self.problem = None;
            }
            Message::Go => {
                let folder = PathBuf::from(self.folder.trim());
                if !folder.is_absolute() {
                    self.problem = Some("Enter an absolute folder path.".into());
                    return Task::none();
                }
                return self.browse(folder, generation);
            }
            Message::Up => {
                if let Some(parent) = self.parent() {
                    return self.browse(parent.to_path_buf(), generation);
                }
            }
            Message::Entry(ticket, index)
                if ticket == self.generation && index / PAGE_SIZE == self.page =>
            {
                if let Some(directory) = &self.directory
                    && let Some(entry) = directory.entries.get(index)
                    && let Some(Location::Local(path)) = directory.path.with(&entry.name)
                {
                    if entry.kind == EntryKind::Directory {
                        return self.browse(path, generation);
                    }
                    if entry.kind == EntryKind::File && self.mode.allows_file(&path) {
                        self.toggle(path);
                    }
                }
            }
            Message::Page(forward) => {
                self.page = if forward {
                    (self.page + 1).min(self.last_page())
                } else {
                    self.page.saturating_sub(1)
                };
                return operation::snap_to(list_id(), operation::RelativeOffset::START);
            }
            _ => {}
        }
        Task::none()
    }

    fn browse(&mut self, folder: PathBuf, generation: &mut u64) -> Task<Message> {
        *generation = generation.wrapping_add(1);
        self.generation = *generation;
        self.loading = true;
        self.page = 0;
        self.directory = None;
        self.selected.clear();
        self.problem = None;
        let ticket = self.generation;
        let mode = self.mode;
        Task::future(async move {
            let result = list_local(&folder)
                .map(|mut directory| {
                    directory.entries.retain(|entry| {
                        entry.kind == EntryKind::Directory
                            || entry.kind == EntryKind::File
                                && matches!(
                                    directory.path.with(&entry.name),
                                    Some(Location::Local(path)) if mode.allows_file(&path)
                                )
                    });
                    Arc::new(directory)
                })
                .map_err(|error| format!("Could not open the folder ({error})."));
            Message::Listed(ticket, result)
        })
    }

    fn toggle(&mut self, path: PathBuf) {
        self.problem = None;
        if let Some(index) = self.selected.iter().position(|selected| selected == &path) {
            self.selected.remove(index);
        } else if self.selected.len() < self.mode.file_limit() {
            self.selected.push(path);
        } else {
            self.problem = Some(format!("Choose up to {} files.", self.mode.file_limit()));
        }
    }

    fn parent(&self) -> Option<&Path> {
        self.current_folder()?.parent()
    }

    fn current_folder(&self) -> Option<&Path> {
        let Location::Local(folder) = &self.directory.as_ref()?.path else {
            return None;
        };
        Some(folder)
    }

    pub(super) fn chosen_folder(&self) -> Option<&Path> {
        self.current_folder()
            .filter(|_| self.chooses_folder())
            .filter(|path| path.to_str().is_some() && display_path(path) == self.folder)
    }

    pub(super) fn chooses_folder(&self) -> bool {
        matches!(self.mode, Mode::ExportFolder | Mode::KnowledgeFolder)
    }

    fn last_page(&self) -> usize {
        self.directory.as_ref().map_or(0, |directory| {
            directory.entries.len().saturating_sub(1) / PAGE_SIZE
        })
    }

    pub(super) fn view(&self, importing: bool) -> Element<'_, Message> {
        let enabled = !self.loading && !importing;
        let folder = text_input("Absolute folder path", &self.folder)
            .on_input_maybe(enabled.then_some(Message::Folder))
            .on_submit_maybe(enabled.then_some(Message::Go));
        let navigation = row![
            folder,
            crate::workspace::dialog_design::secondary_action("Go")
                .on_press_maybe(enabled.then_some(Message::Go)),
            crate::workspace::dialog_design::secondary_action("Up")
                .on_press_maybe((enabled && self.parent().is_some()).then_some(Message::Up)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let mut entries = column![].spacing(5).width(Length::Fill);
        if self.loading {
            entries = entries.push(text(match self.mode {
                Mode::Markdown => "Loading folders and Markdown files…",
                Mode::KnowledgeFiles(_) => "Loading folders and supported files…",
                Mode::ExportFolder | Mode::KnowledgeFolder => "Loading folders…",
            }));
        } else if let Some(directory) = &self.directory {
            let start = self.page * PAGE_SIZE;
            for (index, entry) in directory
                .entries
                .iter()
                .enumerate()
                .skip(start)
                .take(PAGE_SIZE)
            {
                let message = Message::Entry(self.generation, index);
                if entry.kind == EntryKind::Directory {
                    entries = entries.push(
                        button(
                            text(format!("{}/", entry.display))
                                .wrapping(text::Wrapping::WordOrGlyph),
                        )
                        .style(button::text)
                        .on_press_maybe(enabled.then_some(message)),
                    );
                } else if let Some(Location::Local(path)) = directory.path.with(&entry.name) {
                    entries = entries.push(
                        checkbox(self.selected.contains(&path))
                            .label(&entry.display)
                            .on_toggle_maybe(enabled.then_some(move |_| message.clone())),
                    );
                }
            }
            if directory.entries.is_empty() {
                entries = entries.push(text(match self.mode {
                    Mode::Markdown => "No Markdown files or folders.",
                    Mode::KnowledgeFiles(_) => "No supported files or subfolders.",
                    Mode::ExportFolder | Mode::KnowledgeFolder => "No subfolders.",
                }));
            }
        }
        let last_page = self.last_page();
        let pages = (last_page > 0).then(|| {
            row![
                button("Previous page")
                    .on_press_maybe((enabled && self.page > 0).then_some(Message::Page(false)),),
                text(format!("Page {} of {}", self.page + 1, last_page + 1)).size(13),
                button("Next page").on_press_maybe(
                    (enabled && self.page < last_page).then_some(Message::Page(true)),
                ),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .wrap()
        });
        let (title, description) = match self.mode {
            Mode::Markdown => (
                "Import Markdown",
                format!("Select up to {MAX_MARKDOWN_FILES} .md or .markdown files in this folder."),
            ),
            Mode::ExportFolder => (
                "Choose export folder",
                "The selected skills will be exported to the current folder.".into(),
            ),
            Mode::KnowledgeFiles(_) => (
                "Add files",
                "Select the files to review for this knowledge store.".into(),
            ),
            Mode::KnowledgeFolder => (
                "Add folder",
                "Open the folder you want to review for this knowledge store.".into(),
            ),
        };
        column![
            text(title).size(19),
            navigation,
            text(description).size(13),
            scrollable(entries).id(list_id()).height(Length::Fill),
            pages,
            self.problem
                .as_deref()
                .map(|problem| text(problem).size(13)),
        ]
        .spacing(8)
        .height(Length::Fill)
        .into()
    }
}

pub(super) fn display_path(path: &Path) -> Cow<'_, str> {
    let label = path.to_string_lossy();
    #[cfg(windows)]
    if let Some(rest) = label.strip_prefix(r"\\?\UNC\") {
        return Cow::Owned(format!(r"\\{rest}"));
    } else if let Some(rest) = label.strip_prefix(r"\\?\")
        && matches!(rest.as_bytes(), [drive, b':', b'\\', ..] if drive.is_ascii_alphabetic())
    {
        return Cow::Owned(rest.to_owned());
    }
    label
}

fn list_id() -> iced::widget::Id {
    iced::widget::Id::new("ai-manager-file-chooser-list")
}

fn markdown_path(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        let extension = extension.as_encoded_bytes();
        extension.eq_ignore_ascii_case(b"md") || extension.eq_ignore_ascii_case(b"markdown")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_markdown_chooser_enforces_the_selection_limit_and_allows_deselection() {
        let mut chooser = Chooser::default();
        for index in 0..MAX_MARKDOWN_FILES {
            chooser.toggle(PathBuf::from(format!("{index}.md")));
        }
        let extra = PathBuf::from("extra.md");
        chooser.toggle(extra.clone());
        assert_eq!(chooser.selection().len(), MAX_MARKDOWN_FILES);
        assert!(!chooser.selection().contains(&extra));
        assert!(chooser.problem.is_some());
        chooser.toggle(PathBuf::from("0.md"));
        chooser.toggle(extra.clone());
        assert_eq!(chooser.selection().len(), MAX_MARKDOWN_FILES);
        assert_eq!(chooser.selection().last(), Some(&extra));
        assert!(chooser.problem.is_none());
    }
}
