use std::fmt;

use iced::widget::{button, column, pick_list, row, text, text_editor, text_input};
use iced::{Element, Length};

use crate::snippets::{Library, MAX_OPERATING_SYSTEM_BYTES, Metadata};

#[derive(Clone)]
pub enum Message {
    Description(text_editor::Action),
    Tags(text_editor::Action),
    Folder(String),
    OperatingSystem(String),
    FileName(String),
    Mode(FileMode),
    Apply,
    Cancel,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum FileMode {
    Auto,
    Executable,
    Regular,
}

impl FileMode {
    const ALL: [Self; 3] = [Self::Auto, Self::Executable, Self::Regular];

    fn value(self) -> Option<bool> {
        match self {
            Self::Auto => None,
            Self::Executable => Some(true),
            Self::Regular => Some(false),
        }
    }
}

impl fmt::Display for FileMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "Automatic permissions",
            Self::Executable => "Executable file",
            Self::Regular => "Regular file",
        })
    }
}

pub(super) struct Form {
    metadata: Metadata,
    description: text_editor::Content,
    tags: text_editor::Content,
    error: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
struct FolderChoice(String);

impl fmt::Display for FolderChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(if self.0.is_empty() {
            "Top level"
        } else {
            &self.0
        })
    }
}

impl Form {
    pub(super) fn new(metadata: &Metadata) -> Self {
        Self {
            metadata: metadata.clone(),
            description: text_editor::Content::with_text(&metadata.description),
            tags: text_editor::Content::with_text(&metadata.tags.join("\n")),
            error: None,
        }
    }

    pub(super) fn changed(&self, original: &Metadata) -> bool {
        &self.metadata != original
    }

    pub(super) fn rebase_folder(&mut self, old: &str, new: &str) {
        if self.metadata.folder_path == old && old != new {
            self.metadata.folder_path = new.to_owned();
        }
    }

    pub(super) fn update(&mut self, message: Message) -> Option<Metadata> {
        self.error = None;
        match message {
            Message::Folder(folder) if folder.len() <= 1024 => self.metadata.folder_path = folder,
            Message::OperatingSystem(system) if system.len() <= MAX_OPERATING_SYSTEM_BYTES => {
                self.metadata.operating_system = (!system.is_empty()).then_some(system);
            }
            Message::FileName(name) if name.len() <= 255 => {
                self.metadata.file_name = (!name.is_empty()).then_some(name);
            }
            Message::Mode(mode) => self.metadata.executable = mode.value(),
            Message::Description(action) => {
                if let Some(value) = edit(&mut self.description, action, 16 * 1024) {
                    self.metadata.description = value;
                }
            }
            Message::Tags(action) => {
                if let Some(value) = edit(&mut self.tags, action, 17 * 1024) {
                    self.metadata.tags = value
                        .lines()
                        .map(str::trim)
                        .filter(|tag| !tag.is_empty())
                        .map(str::to_owned)
                        .collect();
                }
            }
            Message::Apply => {
                self.metadata.folder_path = self.metadata.folder_path.trim().into();
                self.metadata.operating_system = self
                    .metadata
                    .operating_system
                    .take()
                    .map(|system| system.trim().to_owned())
                    .filter(|system| !system.is_empty());
                self.metadata.file_name = self
                    .metadata
                    .file_name
                    .take()
                    .map(|name| name.trim().to_owned())
                    .filter(|name| !name.is_empty());
                match self.metadata.validate() {
                    Ok(()) => return Some(self.metadata.clone()),
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
            _ => {}
        }
        None
    }

    pub(super) fn view(&self, library: &Library) -> Element<'_, Message> {
        let mode = match self.metadata.executable {
            None => FileMode::Auto,
            Some(true) => FileMode::Executable,
            Some(false) => FileMode::Regular,
        };
        let selected = FolderChoice(self.metadata.folder_path.clone());
        let mut folders: Vec<_> = library
            .folders()
            .iter()
            .map(|folder| FolderChoice(folder.path.clone()))
            .collect();
        if !selected.0.is_empty() && !folders.contains(&selected) {
            folders.push(selected.clone());
        }
        folders.sort_by_cached_key(|folder| folder.0.to_lowercase());
        folders.insert(0, FolderChoice(String::new()));
        let mut form = column![
            text("Snippet details").size(20),
            row![
                text("Folder"),
                pick_list(folders, Some(selected), |folder| Message::Folder(folder.0))
                    .width(Length::Fill)
            ]
            .spacing(8),
            row![
                text_input(
                    "Optional export filename",
                    self.metadata.file_name.as_deref().unwrap_or_default()
                )
                .on_input(Message::FileName),
                pick_list(FileMode::ALL, Some(mode), Message::Mode),
            ]
            .spacing(8),
            text_input(
                "Optional operating system",
                self.metadata
                    .operating_system
                    .as_deref()
                    .unwrap_or_default(),
            )
            .on_input(Message::OperatingSystem),
            text("Description"),
            text_editor(&self.description)
                .height(Length::Fill)
                .on_action(Message::Description),
            text("Tags, one per line"),
            text_editor(&self.tags)
                .height(Length::Fill)
                .on_action(Message::Tags),
        ]
        .spacing(8);
        if let Some(error) = self.error.as_ref() {
            form = form.push(text(error));
        }
        form.push(
            row![
                button("Cancel").on_press(Message::Cancel),
                button("Apply").on_press(Message::Apply),
            ]
            .spacing(8),
        )
        .padding(16)
        .height(Length::Fill)
        .into()
    }
}

fn edit(
    content: &mut text_editor::Content,
    action: text_editor::Action,
    limit: usize,
) -> Option<String> {
    if !action.is_edit() {
        content.perform(action);
        return None;
    }
    if matches!(&action, text_editor::Action::Edit(text_editor::Edit::Paste(value)) if value.len() > limit)
    {
        return None;
    }
    let previous = content.text();
    let cursor = content.cursor();
    content.perform(action);
    let value = content.text();
    if value.len() > limit {
        *content = text_editor::Content::with_text(&previous);
        content.move_to(cursor);
        return None;
    }
    (previous != value).then_some(value)
}
