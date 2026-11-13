use std::collections::HashSet;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use iced::widget::{
    button, checkbox, column, pick_list, row, scrollable, text, text_editor, text_input,
};
use iced::{Element, Length};
use zeroize::Zeroizing;

use crate::snippets::archive::{
    Action, FolderSelection, FolderTarget, OpenPgpSettings, Outcome, ZipSettings,
};
use crate::snippets::recipients::Choice;
use crate::snippets::{Library, Snippet, SnippetId, folders};

#[derive(Clone, Copy)]
pub(super) enum Mode {
    Import,
    Export,
}

#[derive(Clone)]
pub enum Message {
    Paths(text_editor::Action),
    Path(String),
    Selected(SnippetId, bool),
    Target(Target),
    ZipExtension(String),
    ZipEncryption(bool),
    ZipPassword(Zeroizing<String>, bool),
    CertificatePath(String),
    Fingerprint(String),
    SavedRecipient(Choice),
    ManualCertificate,
    Start,
    Cancel,
    Finished(Result<Outcome, String>),
}

pub(super) enum Page {
    Import(text_editor::Content),
    Export(Box<Export>),
    Running,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub enum Target {
    #[default]
    Structured,
    Directory,
    Zip,
    OpenPgp,
}

impl Target {
    const ALL: [Self; 4] = [Self::Structured, Self::Directory, Self::Zip, Self::OpenPgp];
    const FILES: [Self; 3] = [Self::Directory, Self::Zip, Self::OpenPgp];

    fn hint(self) -> &'static str {
        match self {
            Self::Structured => "Enter a new absolute path ending in .json, .xml, .yaml, or .yml.",
            Self::Directory => {
                "Enter an absolute path for a new directory. Selected snippets keep their folder structure."
            }
            Self::Zip => {
                "Enter a new absolute ZIP file path. An empty extension override keeps each filename and folder."
            }
            Self::OpenPgp => {
                "Enter a new absolute .zip.gpg path. The entire ZIP, including filenames, is encrypted."
            }
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Structured => "JSON, XML or YAML",
            Self::Directory => "Folder",
            Self::Zip => "ZIP",
            Self::OpenPgp => "OpenPGP-encrypted ZIP",
        })
    }
}

#[derive(Default)]
pub(super) struct Export {
    path: String,
    selected: HashSet<SnippetId>,
    folder: Option<FolderSelection>,
    target: Target,
    extension: String,
    encrypted: bool,
    password: Zeroizing<String>,
    confirmation: Zeroizing<String>,
    certificate: String,
    fingerprint: String,
    recipients: Vec<Choice>,
    recipient_error: Option<String>,
    saved_recipient: Option<Choice>,
    error: Option<String>,
}

impl Export {
    fn can_start(&self) -> bool {
        !self.path.is_empty()
            && self.folder.as_ref().map_or_else(
                || !self.selected.is_empty(),
                |folder| self.target != Target::Structured || folder.snippets().next().is_some(),
            )
            && match self.target {
                Target::Zip => {
                    !self.encrypted
                        || (!self.password.is_empty() && self.password == self.confirmation)
                }
                Target::OpenPgp => self.saved_recipient.as_ref().map_or_else(
                    || !self.certificate.is_empty() && !self.fingerprint.is_empty(),
                    |choice| !choice.revoked(),
                ),
                _ => true,
            }
    }

    fn selected_entries(&self, library: &Library) -> Vec<Snippet> {
        library
            .entries()
            .iter()
            .filter(|entry| self.selected.contains(&entry.id()))
            .cloned()
            .collect()
    }

    fn file_action(&self, path: PathBuf, library: &Library, target: FolderTarget) -> Action {
        if let Some(folder) = self.folder.as_ref() {
            return Action::ExportFolder(path, folder.clone(), target);
        }
        let entries = self.selected_entries(library);
        match target {
            FolderTarget::Directory => Action::ExportDirectory(path, entries),
            FolderTarget::Zip(settings) => Action::ExportZip(path, entries, settings),
            FolderTarget::OpenPgp(settings) => Action::ExportOpenPgp(path, entries, settings),
        }
    }

    pub(super) fn set_recipients(&mut self, result: Result<Vec<Choice>, String>) {
        match result {
            Ok(recipients) => {
                self.recipients = recipients;
                self.recipient_error = None;
            }
            Err(error) => self.recipient_error = Some(error),
        }
    }

    fn update(
        &mut self,
        message: Message,
        library: &Library,
        recipient_path: &Path,
    ) -> Option<Action> {
        self.error = None;
        match message {
            Message::Path(value) if value.len() <= 8192 => self.path = value,
            Message::Selected(id, checked) if self.folder.is_none() => {
                if checked && library.get(id).is_some() {
                    self.selected.insert(id);
                } else {
                    self.selected.remove(&id);
                }
            }
            Message::Target(target) => {
                self.target = target;
                self.clear_password();
            }
            Message::ZipExtension(extension)
                if self.folder.is_none()
                    && matches!(self.target, Target::Zip | Target::OpenPgp)
                    && extension.len() <= 128 =>
            {
                self.extension = extension;
            }
            Message::ZipEncryption(enabled) if self.target == Target::Zip => {
                self.clear_password();
                self.encrypted = enabled;
            }
            Message::ZipPassword(password, confirmation)
                if self.target == Target::Zip
                    && self.encrypted
                    && password.len() <= ZipSettings::MAX_PASSWORD_BYTES =>
            {
                if confirmation {
                    self.confirmation = password;
                } else {
                    self.password = password;
                }
            }
            Message::CertificatePath(path)
                if self.target == Target::OpenPgp && path.len() <= 8192 =>
            {
                self.certificate = path;
            }
            Message::Fingerprint(fingerprint)
                if self.target == Target::OpenPgp && fingerprint.len() <= 128 =>
            {
                self.fingerprint = fingerprint;
            }
            Message::SavedRecipient(choice)
                if self.target == Target::OpenPgp && self.recipients.contains(&choice) =>
            {
                self.saved_recipient = Some(choice);
            }
            Message::ManualCertificate if self.target == Target::OpenPgp => {
                self.saved_recipient = None;
            }
            Message::Start if self.can_start() => {
                let path = self.path.as_str().into();
                return match self.target {
                    Target::Structured => {
                        let entries = self.folder.as_ref().map_or_else(
                            || self.selected_entries(library),
                            |folder| folder.snippets().cloned().collect(),
                        );
                        Some(Action::Export(path, entries))
                    }
                    Target::Directory => {
                        Some(self.file_action(path, library, FolderTarget::Directory))
                    }
                    Target::Zip => {
                        let password = self.encrypted.then(|| self.password.clone());
                        match ZipSettings::new(&self.extension, password) {
                            Ok(settings) => {
                                Some(self.file_action(path, library, FolderTarget::Zip(settings)))
                            }
                            Err(error) => {
                                self.error = Some(error.to_string());
                                None
                            }
                        }
                    }
                    Target::OpenPgp => {
                        let settings = match &self.saved_recipient {
                            Some(choice) => OpenPgpSettings::saved(
                                &self.extension,
                                recipient_path.to_path_buf(),
                                choice.clone(),
                            ),
                            None => OpenPgpSettings::new(
                                &self.extension,
                                self.certificate.as_str().into(),
                                &self.fingerprint,
                            ),
                        };
                        match settings {
                            Ok(settings) => Some(self.file_action(
                                path,
                                library,
                                FolderTarget::OpenPgp(settings),
                            )),
                            Err(error) => {
                                self.error = Some(error.to_string());
                                None
                            }
                        }
                    }
                };
            }
            _ => {}
        }
        None
    }

    fn clear_password(&mut self) {
        self.encrypted = false;
        self.password = Zeroizing::default();
        self.confirmation = Zeroizing::default();
    }

    fn view(&self, library: &Library) -> Element<'_, Message> {
        let mut choices = column![].spacing(4);
        let (title, targets): (&str, &[Target]) = if let Some(folder) = self.folder.as_ref() {
            let count = folder.snippets().len();
            choices = choices
                .push(text(format!("{} · {count} snippets", folder.label())))
                .push(text("Folder and ZIP exports include empty sub-folders.").size(13));
            let targets = if count == 0 {
                &Target::FILES[..]
            } else {
                &Target::ALL[..]
            };
            ("Export folder", targets)
        } else {
            for entry in library.entries() {
                let id = entry.id();
                choices = choices.push(
                    checkbox(self.selected.contains(&id))
                        .label(entry.label())
                        .on_toggle(move |checked| Message::Selected(id, checked)),
                );
            }
            ("Export snippets", &Target::ALL)
        };
        let mut form = column![
            text(title).size(20),
            pick_list(targets, Some(self.target), Message::Target),
            text(self.target.hint()),
            text_input("Export path", &self.path).on_input(Message::Path),
        ]
        .spacing(8);
        if self.folder.is_none() && matches!(self.target, Target::Zip | Target::OpenPgp) {
            form = form.push(
                text_input("Optional filename extension", &self.extension)
                    .on_input(Message::ZipExtension),
            );
        }
        if self.target == Target::Zip {
            form = form.push(
                checkbox(self.encrypted)
                    .label("Encrypt ZIP contents with AES-256")
                    .on_toggle(Message::ZipEncryption),
            );
            if self.encrypted {
                form = form
                    .push(
                        row![
                            text_input("ZIP password", &self.password)
                                .secure(true)
                                .on_input(|value| Message::ZipPassword(
                                    Zeroizing::new(value),
                                    false
                                )),
                            text_input("Repeat ZIP password", &self.confirmation)
                                .secure(true)
                                .on_input(|value| Message::ZipPassword(
                                    Zeroizing::new(value),
                                    true
                                )),
                        ]
                        .spacing(8),
                    )
                    .push(text("ZIP filenames remain visible.").size(13));
            }
        }
        if self.target == Target::OpenPgp {
            if !self.recipients.is_empty() {
                form = form.push(
                    row![
                        pick_list(
                            self.recipients.as_slice(),
                            self.saved_recipient.as_ref(),
                            Message::SavedRecipient,
                        )
                        .placeholder("Select a saved recipient")
                        .width(Length::Fill),
                        button("Use certificate file").on_press_maybe(
                            self.saved_recipient
                                .as_ref()
                                .map(|_| Message::ManualCertificate),
                        ),
                    ]
                    .spacing(8),
                );
            }
            if let Some(error) = self.recipient_error.as_ref() {
                form = form.push(text(format!("Saved recipients unavailable ({error})")).size(13));
            }
            if self.saved_recipient.is_none() {
                form = form
                    .push(
                        text_input("Absolute public certificate path", &self.certificate)
                            .on_input(Message::CertificatePath),
                    )
                    .push(
                        text_input(
                            "Full fingerprint verified with the recipient",
                            &self.fingerprint,
                        )
                        .on_input(Message::Fingerprint),
                    );
            }
        }
        if let Some(error) = self.error.as_ref() {
            form = form.push(text(error));
        }
        form.push(scrollable(choices).height(Length::Fill))
            .push(
                row![
                    button("Cancel").on_press(Message::Cancel),
                    button("Export").on_press_maybe(self.can_start().then_some(Message::Start)),
                ]
                .spacing(8),
            )
            .into()
    }
}

impl Page {
    pub(super) fn new(mode: Mode, current: Option<SnippetId>, library: &Library) -> Self {
        match mode {
            Mode::Import => Self::Import(text_editor::Content::new()),
            Mode::Export => Self::Export(Box::new(Export {
                selected: library
                    .entries()
                    .iter()
                    .filter(|entry| current.is_none_or(|id| entry.id() == id))
                    .map(|entry| entry.id())
                    .collect(),
                ..Export::default()
            })),
        }
    }

    pub(super) fn new_folder(folder: Option<folders::Id>, library: &Library) -> io::Result<Self> {
        Ok(Self::Export(Box::new(Export {
            folder: Some(FolderSelection::new(folder, library)?),
            target: Target::Zip,
            ..Export::default()
        })))
    }

    pub(super) fn update(
        &mut self,
        message: Message,
        library: &Library,
        recipient_path: &Path,
    ) -> Option<Action> {
        const MAX_PATH_INPUT_BYTES: usize = 1024 * 1024;
        let action = match (self, message) {
            (Self::Import(paths), Message::Paths(action)) => {
                if matches!(&action, text_editor::Action::Edit(text_editor::Edit::Paste(text)) if text.len() > MAX_PATH_INPUT_BYTES)
                {
                    return None;
                }
                if action.is_edit() {
                    let previous = paths.text();
                    let cursor = paths.cursor();
                    paths.perform(action);
                    if paths.text().len() > MAX_PATH_INPUT_BYTES {
                        *paths = text_editor::Content::with_text(&previous);
                        paths.move_to(cursor);
                    }
                } else {
                    paths.perform(action);
                }
                return None;
            }
            (Self::Import(paths), Message::Start) => Action::Import(
                paths
                    .text()
                    .lines()
                    .filter(|line| !line.is_empty())
                    .map(Into::into)
                    .collect(),
            ),
            (Self::Export(form), message) => return form.update(message, library, recipient_path),
            _ => return None,
        };
        Some(action)
    }

    pub(super) fn view(&self, library: &Library) -> Element<'_, Message> {
        let form = match self {
            Self::Import(paths) => column![
                text("Import snippets").size(20),
                text(
                    "Enter one absolute path per line. Choose JSON, XML or YAML exports, a Java snippets.xml library, or individual text files."
                ),
                text_editor(paths)
                    .placeholder("Absolute file paths")
                    .on_action(Message::Paths)
                    .height(Length::Fill),
                row![
                    button("Cancel").on_press(Message::Cancel),
                    button("Import").on_press(Message::Start),
                ]
                .spacing(8),
            ],
            Self::Export(form) => column![form.view(library)],
            Self::Running => column![text("Reading or writing snippet files…")],
        };
        form.spacing(8).padding(16).width(Length::Fill).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippets::{Input, Metadata, tests::Profile};

    #[test]
    fn folder_page_exports_only_its_snippets_with_library_relative_paths() {
        let profile = Profile::new();
        let mut library = Library::open(profile.path()).unwrap();
        let parent = library.create_folder(None, "Parent").unwrap();
        let child = library.create_folder(Some(parent), "Child").unwrap();
        for (name, directory) in [("Selected", "Parent/Child"), ("Outside", "Parent")] {
            library
                .save_with_metadata(
                    None,
                    Input {
                        name: name.into(),
                        language: "bash".into(),
                        category: String::new(),
                        content: "echo test\n".into(),
                    },
                    Some(Metadata {
                        folder_path: directory.into(),
                        ..Metadata::default()
                    }),
                )
                .unwrap();
        }
        for extension in ["json", "xml", "yaml"] {
            let mut page = Page::new_folder(Some(child), &library).unwrap();
            let recipient_path = Path::new("");
            let target = profile.path().with_file_name(format!("export.{extension}"));
            assert!(
                page.update(
                    Message::Target(Target::Structured),
                    &library,
                    recipient_path
                )
                .is_none()
            );
            assert!(
                page.update(
                    Message::Path(target.to_string_lossy().into_owned()),
                    &library,
                    recipient_path
                )
                .is_none()
            );
            let action = page
                .update(Message::Start, &library, recipient_path)
                .unwrap();
            assert!(matches!(action.run(), Ok(Outcome::Exported(1))));
            let exported = std::fs::read_to_string(target).unwrap();
            assert!(exported.contains("Parent/Child"));
            assert!(exported.contains("Selected"));
            assert!(!exported.contains("Outside"));
        }
    }
}
