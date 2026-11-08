use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use iced::widget::{button, column, container, pick_list, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length, Size, Task, window};
use kortty_vault::Secret;

use crate::connection_store::Store;
use crate::connection_store::import::{
    Access, Format, Outcome, Preview, ReadOutcome, supports_path,
};
use crate::connection_window::CredentialChoice;
use crate::private_files::FileStamp;
use crate::profile;
use crate::sftp_window::{Directory, EntryKind, Location, list_local};
use crate::snippets::archive::ZipSettings;
use crate::vault_window::Kind as CredentialKind;
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message, Workspace, WorkspaceMessage as Desktop, wrap};

mod options;

#[derive(Debug)]
pub(crate) struct LoadedPreview {
    preview: Arc<Preview>,
    store: Store,
}

const TITLE: &str = "Import Connections";
const PATH_INPUT: &str = "connection-import-path";
const KEY_INPUT: &str = "connection-import-key";
const PASSWORD_INPUT: &str = "connection-import-password";
const PAGE_SIZE: usize = 48;
const MAX_PATH: usize = 8192;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Unlock {
    #[default]
    None,
    Zip,
    OpenPgp,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BrowseTarget {
    #[default]
    Archive,
    PrivateKey,
}

#[derive(Clone)]
pub struct PasswordInput(Secret);

impl std::fmt::Debug for PasswordInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PasswordInput([redacted])")
    }
}

#[derive(Clone, Debug)]
pub enum Review {
    Preview(Arc<LoadedPreview>),
    ZipPasswordRequired(FileStamp),
    OpenPgpKeyRequired(FileStamp),
}

enum ReviewAccess {
    Plain,
    Zip(Secret),
    OpenPgp { key: PathBuf, passphrase: Secret },
}

impl ReviewAccess {
    fn as_access(&self) -> Access<'_> {
        match self {
            Self::Plain => Access::Plain,
            Self::Zip(password) => Access::ZipPassword(password),
            Self::OpenPgp { key, passphrase } => Access::OpenPgp {
                private_key: key,
                passphrase,
            },
        }
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    Focus,
    Cancel,
    Path(String),
    KeyPath(String),
    Password(u64, PasswordInput),
    SavedPassword(u64, Option<String>),
    InspectPath,
    Unlock(u64),
    Browse(BrowseTarget),
    Back,
    Parent,
    Page(bool),
    Select(u64, usize),
    Listed(u64, Result<Arc<Directory>, String>),
    Reviewed(u64, Result<Review, String>),
    Options(u64, options::Edit),
    Import(u64, u64),
    Committed(u64, Result<(Outcome, Box<Store>), String>),
    Refreshed(u64, Outcome, Result<Box<Store>, String>),
}

pub(super) struct Dialog {
    id: window::Id,
    owner: window::Id,
    path: String,
    source: Option<PathBuf>,
    source_identity: Option<FileStamp>,
    key_path: String,
    password: Secret,
    saved_password: Option<String>,
    unlock: Unlock,
    browsing: Option<Arc<Directory>>,
    browse_target: BrowseTarget,
    preview: Option<Arc<LoadedPreview>>,
    options: options::Draft,
    generation: u64,
    page: usize,
    phase: Phase,
    problem: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Ready,
    Loading,
    Saving,
}

impl Dialog {
    fn new(id: window::Id, owner: window::Id) -> Self {
        Self {
            id,
            owner,
            path: String::new(),
            source: None,
            source_identity: None,
            key_path: String::new(),
            password: Secret::default(),
            saved_password: None,
            unlock: Unlock::None,
            browsing: None,
            browse_target: BrowseTarget::Archive,
            preview: None,
            options: options::Draft::default(),
            generation: 0,
            page: 0,
            phase: Phase::Ready,
            problem: None,
        }
    }

    fn begin(&mut self, phase: Phase) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.phase = phase;
        self.problem = None;
        self.generation
    }

    fn forget_password(&mut self) {
        self.password = Secret::default();
        self.saved_password = None;
    }

    fn reset_source(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.source = None;
        self.source_identity = None;
        self.key_path.clear();
        self.forget_password();
        self.unlock = Unlock::None;
        self.browsing = None;
        self.preview = None;
        self.options = options::Draft::default();
        self.problem = None;
    }

    fn review_source(&mut self, path: PathBuf) -> Task<Desktop> {
        self.reset_source();
        self.source = Some(path.clone());
        self.review(path, ReviewAccess::Plain)
    }

    fn review(&mut self, path: PathBuf, access: ReviewAccess) -> Task<Desktop> {
        self.browsing = None;
        self.preview = None;
        let id = self.id;
        let expected = self.source_identity.clone();
        let generation = self.begin(Phase::Loading);
        Task::future(async move {
            let (sender, receiver) = async_channel::bounded(1);
            let worker = thread::Builder::new()
                .name("kortty-connection-import".into())
                .spawn(move || {
                    let result =
                        Preview::read_verified(&path, access.as_access(), expected.as_ref())
                            .and_then(|(result, identity)| match result {
                                ReadOutcome::Preview(preview) => {
                                    let store =
                                        profile::connections_path().and_then(Store::open)?;
                                    Ok(Review::Preview(Arc::new(LoadedPreview {
                                        preview: Arc::new(preview),
                                        store,
                                    })))
                                }
                                ReadOutcome::ZipPasswordRequired => {
                                    Ok(Review::ZipPasswordRequired(identity))
                                }
                                ReadOutcome::OpenPgpKeyRequired => {
                                    Ok(Review::OpenPgpKeyRequired(identity))
                                }
                            })
                            .map_err(|error| {
                                format!("Could not inspect the connection file ({error}).")
                            });
                    let _ = sender.try_send(result);
                });
            let result = match worker {
                Ok(_) => receiver
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The connection file reader stopped.".into())),
                Err(error) => Err(format!(
                    "Could not start the connection file reader ({error})."
                )),
            };
            event(id, Event::Reviewed(generation, result))
        })
    }

    fn browse_start(&mut self, target: BrowseTarget) -> Task<Desktop> {
        let text = match target {
            BrowseTarget::Archive => &self.path,
            BrowseTarget::PrivateKey => &self.key_path,
        };
        let folder = usable_path(text)
            .and_then(Path::parent)
            .or_else(|| self.source.as_deref().and_then(Path::parent))
            .map(Path::to_owned)
            .or_else(std::env::home_dir)
            .or_else(|| std::env::current_dir().ok());
        if target == BrowseTarget::Archive {
            self.reset_source();
        } else {
            self.forget_password();
        }
        if let Some(folder) = folder {
            return self.browse(folder, target);
        }
        self.problem = Some("Could not determine a starting folder.".into());
        Task::none()
    }

    fn browse(&mut self, folder: PathBuf, target: BrowseTarget) -> Task<Desktop> {
        self.browsing = None;
        self.browse_target = target;
        self.preview = None;
        self.page = 0;
        let id = self.id;
        let generation = self.begin(Phase::Loading);
        Task::future(async move {
            let result = list_local(&folder)
                .map(|mut directory| {
                    directory.entries.retain(|entry| {
                        entry.kind == EntryKind::Directory
                            || (entry.kind == EntryKind::File
                                && directory.path.with(&entry.name).is_some_and(
                                    |path| matches!(path, Location::Local(path) if target == BrowseTarget::PrivateKey || supports_path(&path)),
                                ))
                    });
                    Arc::new(directory)
                })
                .map_err(|error| format!("Could not open the folder ({error})."));
            event(id, Event::Listed(generation, result))
        })
    }

    fn choose_file(&mut self, path: PathBuf) -> Task<Desktop> {
        let Some(label) = path.to_str().filter(|value| usable_path(value).is_some()) else {
            self.problem = Some("Choose a file with a valid UTF-8 absolute path.".into());
            return Task::none();
        };
        match self.browse_target {
            BrowseTarget::Archive => {
                self.path = label.to_owned();
                self.review_source(path)
            }
            BrowseTarget::PrivateKey => {
                self.browsing = None;
                self.change_key_path(label.to_owned());
                iced::widget::operation::focus(PASSWORD_INPUT)
            }
        }
    }

    fn change_key_path(&mut self, path: String) {
        if path != self.key_path {
            self.key_path = path;
            self.forget_password();
            self.generation = self.generation.wrapping_add(1);
        }
        self.problem = None;
    }

    fn max_page(&self) -> usize {
        self.browsing
            .as_ref()
            .map_or(0, |list| list.entries.len().saturating_sub(1) / PAGE_SIZE)
    }

    fn view(
        &self,
        archive_passwords: Vec<CredentialChoice>,
        choices: options::Choices<'_>,
    ) -> Element<'_, Desktop> {
        let ready = self.phase == Phase::Ready;
        let id = self.id;
        let can_preview = ready && usable_path(&self.path).is_some();
        let path = text_input("Absolute path to a connection file", &self.path)
            .id(PATH_INPUT)
            .on_input_maybe(ready.then_some(Event::Path))
            .on_submit_maybe(can_preview.then_some(Event::InspectPath));
        let mut body = column![
            text(TITLE).size(22),
            text("SSH profiles from XML, PuTTY CM CSV, MobaXterm sessions, ZIP or OpenPGP"),
            row![
                path.width(Length::Fill),
                button("Browse…")
                    .on_press_maybe(ready.then_some(Event::Browse(BrowseTarget::Archive))),
                button("Preview").on_press_maybe(can_preview.then_some(Event::InspectPath)),
            ]
            .align_y(Alignment::Center)
            .spacing(8),
        ]
        .spacing(12);
        if let Some(list) = &self.browsing {
            body = body.push(self.browser_view(list));
        } else if let Some(preview) = &self.preview {
            body = body.push(self.preview_view(preview, &choices));
        } else if ready && self.unlock != Unlock::None {
            body = body.push(self.unlock_view(archive_passwords));
        } else {
            body = body.push(text(if ready {
                "Choose a connection file to review its profiles before saving."
            } else {
                "Reading and verifying…"
            }));
        }
        if let Some(problem) = &self.problem {
            body = body.push(text(problem));
        }
        body = body.push(
            row![
                button("Cancel")
                    .on_press_maybe((self.phase != Phase::Saving).then_some(Event::Cancel)),
                button(if self.phase == Phase::Saving {
                    "Importing…"
                } else {
                    "Import"
                })
                .on_press_maybe(
                    (ready && self.preview.is_some() && self.options.can_import(&choices))
                        .then_some(Event::Import(self.generation, self.options.revision()))
                ),
            ]
            .spacing(8),
        );
        let view: Element<'_, Event> = container(body)
            .padding(16)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        view.map(move |action| event(id, action))
    }

    fn unlock_view(&self, mut passwords: Vec<CredentialChoice>) -> Element<'_, Event> {
        let generation = self.generation;
        let saved = passwords
            .iter()
            .find(|choice| self.saved_password.as_deref() == Some(choice.id.as_str()))
            .cloned();
        let can_unlock = match self.unlock {
            Unlock::None => false,
            Unlock::Zip => saved.is_some() || !self.password.is_empty(),
            Unlock::OpenPgp => usable_path(&self.key_path).is_some(),
        };
        let unlock = can_unlock.then_some(Event::Unlock(generation));
        let mut fields = column![].spacing(8);
        if self.unlock == Unlock::OpenPgp {
            fields = fields.push(
                row![
                    text_input("Absolute path to the OpenPGP private key", &self.key_path)
                        .id(KEY_INPUT)
                        .on_input(Event::KeyPath)
                        .on_submit_maybe(unlock.clone())
                        .width(Length::Fill),
                    button("Browse key…").on_press(Event::Browse(BrowseTarget::PrivateKey)),
                ]
                .align_y(Alignment::Center)
                .spacing(8),
            );
        } else if !passwords.is_empty() {
            passwords.insert(
                0,
                CredentialChoice {
                    id: String::new(),
                    label: "Enter a password".into(),
                    username: String::new(),
                },
            );
            fields = fields.push(
                pick_list(passwords, saved.clone(), move |choice| {
                    Event::SavedPassword(generation, (!choice.id.is_empty()).then_some(choice.id))
                })
                .placeholder("Select a saved archive password…"),
            );
        }
        let placeholder = if self.unlock == Unlock::Zip {
            "ZIP password"
        } else {
            "Private key passphrase (optional)"
        };
        fields
            .push(
                row![
                    text_input(placeholder, &self.password)
                        .id(PASSWORD_INPUT)
                        .secure(true)
                        .on_input_maybe(saved.is_none().then_some(move |value| {
                            Event::Password(generation, PasswordInput(Secret::new(value)))
                        }))
                        .on_submit_maybe(unlock.clone())
                        .width(Length::Fill),
                    button("Unlock and preview").on_press_maybe(unlock),
                ]
                .align_y(Alignment::Center)
                .spacing(8),
            )
            .into()
    }

    fn browser_view(&self, list: &Directory) -> Element<'_, Event> {
        let start = self.page * PAGE_SIZE;
        let mut rows = column![].spacing(2);
        for (index, entry) in list.entries.iter().enumerate().skip(start).take(PAGE_SIZE) {
            let label = if entry.kind == EntryKind::Directory {
                format!("{}/", entry.display)
            } else {
                entry.display.clone()
            };
            rows = rows.push(
                button(text(label))
                    .style(button::text)
                    .on_press(Event::Select(self.generation, index)),
            );
        }
        if list.entries.is_empty() {
            let empty = if self.browse_target == BrowseTarget::PrivateKey {
                "No private key files or folders."
            } else {
                "No supported connection files or folders."
            };
            rows = rows.push(text(empty));
        }
        let parent = matches!(&list.path, Location::Local(path) if path.parent().is_some());
        column![
            text(list.path.display()),
            row![
                button("Back").on_press(Event::Back),
                button("Parent folder").on_press_maybe(parent.then_some(Event::Parent)),
                button("Previous").on_press_maybe((self.page > 0).then_some(Event::Page(false))),
                button("Next")
                    .on_press_maybe((self.page < self.max_page()).then_some(Event::Page(true))),
            ]
            .spacing(8),
            scrollable(rows).height(Length::Fill),
        ]
        .spacing(6)
        .height(Length::Fill)
        .into()
    }

    fn preview_view<'a>(
        &'a self,
        loaded: &'a LoadedPreview,
        choices: &options::Choices<'_>,
    ) -> Element<'a, Event> {
        let preview = &loaded.preview;
        let mut rows = column![].spacing(6);
        if let Some(prepared) = self.options.prepared() {
            for (entry, group) in prepared.entries() {
                let location = if group.is_empty() {
                    entry.login.endpoint.label()
                } else {
                    format!("{} · {group}", entry.login.endpoint.label())
                };
                rows = rows.push(column![text(&entry.name), text(location).size(13)]);
            }
        }
        let outcome = self
            .options
            .prepared()
            .map(|plan| plan.outcome())
            .unwrap_or_default();
        let mut summary = column![
            text(preview.format().label()),
            text(format!(
                "{} profiles ready · {} unsupported protocols · {} invalid rows · {} duplicate rows",
                outcome.imported, preview.unsupported, preview.invalid, preview.duplicates
            )),
        ];
        if preview.format() == Format::PuttyCm {
            summary = summary.push(text(format!(
                "Imported forwarding rules start disabled. {} invalid or duplicate rules skipped.",
                preview.skipped_tunnels,
            )));
        }
        if preview.format() == Format::Kortty
            && preview
                .entries()
                .any(|(entry, _)| !entry.tunnels.is_empty())
        {
            summary = summary.push(text(format!(
                "Imported forwarding requires separate approval before use. {} invalid or duplicate rules skipped.",
                preview.skipped_tunnels,
            )));
        }
        let content = summary
            .push(
                text(format!(
                    "{} already saved after applying the options",
                    outcome.already_saved
                ))
                .size(13),
            )
            .push(
                self.options
                    .view(loaded, choices, self.phase == Phase::Ready)
                    .map(move |edit| Event::Options(self.generation, edit)),
            )
            .push(text("Selected profiles").size(16))
            .push(rows)
            .spacing(8);
        scrollable(content).height(Length::Fill).into()
    }
}

fn usable_path(text: &str) -> Option<&Path> {
    let path = Path::new(text);
    (text.len() <= MAX_PATH && !crate::ssh_dialog::unsafe_text(text) && path.is_absolute())
        .then_some(path)
}

pub(in crate::workspace) fn event(id: window::Id, action: Event) -> Desktop {
    wrap(Message::ConnectionImport(id, action))
}

impl Workspace {
    pub(in crate::workspace) fn show_connection_import(
        &mut self,
        owner: window::Id,
    ) -> Task<Desktop> {
        if self.exiting || self.scheduler_draining || self.clipboard.policy.problem.is_some() {
            return Task::none();
        }
        let manager = self.connections_id == Some(owner)
            && self
                .connections
                .as_ref()
                .is_some_and(|manager| manager.can_import_connections());
        if !manager && !self.windows.contains_key(&owner) {
            return Task::none();
        }
        if let Some(dialog) = &self.chrome.connection_import {
            return window::gain_focus(dialog.id);
        }
        if let Some((id, focus)) = self.owner_dialog(owner) {
            return window::gain_focus(id).chain(Task::done(focus));
        }
        if let Some(window) = self.windows.get_mut(&owner) {
            window.cancel_paste();
        }
        let (id, open) = self.open_placed_dialog(
            DialogKind::ConnectionImport,
            window::Settings {
                size: Size::new(760.0, 525.0),
                min_size: Some(Size::new(520.0, 385.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.chrome.connection_import = Some(Dialog::new(id, owner));
        open.map(move |_| event(id, Event::Focus))
    }

    pub(in crate::workspace) fn connection_import_title(
        &self,
        id: window::Id,
    ) -> Option<&'static str> {
        self.chrome
            .connection_import
            .as_ref()
            .filter(|dialog| dialog.id == id)
            .map(|_| TITLE)
    }

    pub(in crate::workspace) fn connection_import_for_owner(
        &self,
        owner: window::Id,
    ) -> Option<window::Id> {
        self.chrome
            .connection_import
            .as_ref()
            .filter(|dialog| dialog.owner == owner)
            .map(|dialog| dialog.id)
    }

    pub(in crate::workspace) fn connection_import_open(&self) -> bool {
        self.chrome.connection_import.is_some()
    }

    pub(in crate::workspace) fn connection_import_saving(&self) -> Option<window::Id> {
        self.chrome
            .connection_import
            .as_ref()
            .filter(|dialog| dialog.phase == Phase::Saving)
            .map(|dialog| dialog.id)
    }

    pub(in crate::workspace) fn close_connection_import(
        &mut self,
        id: window::Id,
    ) -> Option<Task<Desktop>> {
        if let Some(dialog) = self.connection_import_for_owner(id) {
            return Some(window::gain_focus(dialog));
        }
        self.connection_import_title(id)?;
        if self.connection_import_saving() == Some(id) {
            return Some(Task::none());
        }
        let dialog = self.chrome.connection_import.take()?;
        Some(Task::batch([
            self.close_placed_dialog(id),
            window::gain_focus(dialog.owner),
        ]))
    }

    pub(in crate::workspace) fn connection_import_view(
        &self,
        id: window::Id,
    ) -> Option<Element<'_, Desktop>> {
        self.chrome
            .connection_import
            .as_ref()
            .filter(|dialog| dialog.id == id)
            .map(|dialog| {
                let passwords = if dialog.unlock == Unlock::Zip {
                    self.credential_choices(CredentialKind::ArchivePassword)
                } else {
                    Vec::new()
                };
                dialog.view(
                    passwords,
                    options::Choices {
                        passwords: &self.credential_choices(CredentialKind::Password),
                        keys: &self.ssh_key_choices,
                    },
                )
            })
    }

    pub(super) fn update_connection_import(
        &mut self,
        id: window::Id,
        action: Event,
    ) -> Task<Desktop> {
        let revision = self.connection_store_revision();
        let credentials = self.credential_choices(CredentialKind::Password);
        let choices = options::Choices {
            passwords: &credentials,
            keys: &self.ssh_key_choices,
        };
        let Some(dialog) = self
            .chrome
            .connection_import
            .as_mut()
            .filter(|dialog| dialog.id == id)
        else {
            return Task::none();
        };
        match action {
            Event::Focus => {
                let field = if dialog.browsing.is_some() {
                    PATH_INPUT
                } else {
                    match dialog.unlock {
                        Unlock::None => PATH_INPUT,
                        Unlock::Zip => PASSWORD_INPUT,
                        Unlock::OpenPgp => KEY_INPUT,
                    }
                };
                return window::gain_focus(id).chain(iced::widget::operation::focus(field));
            }
            Event::Cancel => return self.close_connection_import(id).unwrap_or_else(Task::none),
            Event::Listed(generation, result)
                if dialog.phase == Phase::Loading && generation == dialog.generation =>
            {
                dialog.phase = Phase::Ready;
                match result {
                    Ok(list) => dialog.browsing = Some(list),
                    Err(error) => dialog.problem = Some(error),
                }
            }
            Event::Reviewed(generation, result)
                if dialog.phase == Phase::Loading && generation == dialog.generation =>
            {
                dialog.phase = Phase::Ready;
                match result {
                    Ok(Review::Preview(preview)) => {
                        dialog.forget_password();
                        dialog.key_path.clear();
                        dialog.source_identity = None;
                        dialog.unlock = Unlock::None;
                        dialog.options = options::Draft::default();
                        dialog
                            .options
                            .refresh(&preview, &choices, self.vault.as_ref());
                        dialog.preview = Some(preview);
                    }
                    Ok(Review::ZipPasswordRequired(identity)) => {
                        dialog.unlock = Unlock::Zip;
                        dialog.source_identity = Some(identity);
                        return iced::widget::operation::focus(PASSWORD_INPUT);
                    }
                    Ok(Review::OpenPgpKeyRequired(identity)) => {
                        dialog.unlock = Unlock::OpenPgp;
                        dialog.source_identity = Some(identity);
                        return iced::widget::operation::focus(KEY_INPUT);
                    }
                    Err(error) => dialog.problem = Some(error),
                }
            }
            Event::Committed(saved_revision, result) if dialog.phase == Phase::Saving => {
                match result {
                    Ok((outcome, store)) if saved_revision == revision => {
                        return self.finish_connection_import(id, outcome, Ok(store));
                    }
                    Ok((outcome, _)) => return reload(id, revision, outcome),
                    Err(error) => {
                        dialog.phase = Phase::Ready;
                        dialog.problem = Some(error);
                    }
                }
            }
            Event::Refreshed(read_revision, outcome, result) if dialog.phase == Phase::Saving => {
                if read_revision != revision {
                    return reload(id, revision, outcome);
                }
                return self.finish_connection_import(id, outcome, result);
            }
            action if dialog.phase == Phase::Ready && self.clipboard.policy.problem.is_none() => {
                match action {
                    Event::Options(generation, edit) if generation == dialog.generation => {
                        if let Some(loaded) = &dialog.preview {
                            dialog
                                .options
                                .edit(edit, loaded, &choices, self.vault.as_ref());
                        }
                    }
                    Event::Path(path)
                        if path.len() <= MAX_PATH && !crate::ssh_dialog::unsafe_text(&path) =>
                    {
                        if dialog.path != path {
                            dialog.reset_source();
                            dialog.path = path;
                        }
                    }
                    Event::InspectPath if usable_path(&dialog.path).is_some() => {
                        return dialog.review_source(PathBuf::from(&dialog.path));
                    }
                    Event::KeyPath(path)
                        if dialog.unlock == Unlock::OpenPgp
                            && dialog.browsing.is_none()
                            && path.len() <= MAX_PATH
                            && !crate::ssh_dialog::unsafe_text(&path) =>
                    {
                        dialog.change_key_path(path);
                    }
                    Event::Password(generation, PasswordInput(password))
                        if generation == dialog.generation
                            && dialog.unlock != Unlock::None
                            && dialog.browsing.is_none() =>
                    {
                        dialog.forget_password();
                        if password.len() <= ZipSettings::MAX_PASSWORD_BYTES {
                            dialog.password = password;
                            dialog.problem = None;
                        } else {
                            dialog.problem =
                                Some("A password may contain at most 1024 UTF-8 bytes.".into());
                        }
                    }
                    Event::SavedPassword(generation, saved)
                        if generation == dialog.generation
                            && dialog.unlock == Unlock::Zip
                            && dialog.browsing.is_none() =>
                    {
                        dialog.forget_password();
                        dialog.saved_password = saved;
                        dialog.problem = None;
                    }
                    Event::Unlock(generation)
                        if generation == dialog.generation
                            && dialog.browsing.is_none()
                            && dialog.source_identity.is_some() =>
                    {
                        let Some(path) = dialog.source.clone() else {
                            return Task::none();
                        };
                        let access = match dialog.unlock {
                            Unlock::None => return Task::none(),
                            Unlock::Zip => {
                                let password = if let Some(saved) = dialog.saved_password.take() {
                                    match self
                                        .vault
                                        .as_ref()
                                        .ok_or("The vault is locked.")
                                        .and_then(|vault| vault.archive_password(&saved))
                                    {
                                        Ok(password) => password,
                                        Err(error) => {
                                            dialog.problem = Some(error.into());
                                            return Task::none();
                                        }
                                    }
                                } else {
                                    std::mem::take(&mut dialog.password)
                                };
                                if password.is_empty() {
                                    return Task::none();
                                }
                                ReviewAccess::Zip(password)
                            }
                            Unlock::OpenPgp => {
                                let Some(key) = usable_path(&dialog.key_path).map(Path::to_owned)
                                else {
                                    return Task::none();
                                };
                                ReviewAccess::OpenPgp {
                                    key,
                                    passphrase: std::mem::take(&mut dialog.password),
                                }
                            }
                        };
                        return dialog.review(path, access);
                    }
                    Event::Browse(target)
                        if target == BrowseTarget::Archive || dialog.unlock == Unlock::OpenPgp =>
                    {
                        return dialog.browse_start(target);
                    }
                    Event::Back if dialog.browsing.take().is_some() => {
                        let field = if dialog.browse_target == BrowseTarget::PrivateKey {
                            KEY_INPUT
                        } else {
                            PATH_INPUT
                        };
                        return iced::widget::operation::focus(field);
                    }
                    Event::Parent => {
                        if let Some(Location::Local(folder)) =
                            dialog.browsing.as_ref().map(|list| &list.path)
                            && let Some(parent) = folder.parent()
                        {
                            return dialog.browse(parent.to_owned(), dialog.browse_target);
                        }
                    }
                    Event::Page(forward) => {
                        dialog.page = if forward {
                            (dialog.page + 1).min(dialog.max_page())
                        } else {
                            dialog.page.saturating_sub(1)
                        };
                    }
                    Event::Select(generation, index)
                        if generation == dialog.generation && index / PAGE_SIZE == dialog.page =>
                    {
                        if let Some(list) = &dialog.browsing
                            && let Some(entry) = list.entries.get(index)
                            && let Some(Location::Local(path)) = list.path.with(&entry.name)
                        {
                            return if entry.kind == EntryKind::Directory {
                                dialog.browse(path, dialog.browse_target)
                            } else {
                                dialog.choose_file(path)
                            };
                        }
                    }
                    Event::Import(generation, options_revision)
                        if generation == dialog.generation
                            && options_revision == dialog.options.revision()
                            && !self.scheduler_draining
                            && !self.exiting
                            && dialog.preview.is_some() =>
                    {
                        let preview =
                            match dialog.options.commit_plan(&choices, self.vault.as_ref()) {
                                Ok(plan) => plan,
                                Err(error) => {
                                    dialog.problem = Some(error);
                                    return Task::none();
                                }
                            };
                        if self
                            .connections
                            .as_ref()
                            .is_some_and(crate::connection_window::ConnectionWindow::busy)
                        {
                            dialog.problem = Some(
                                    "A Connection Manager update is still in progress. Try Import again when it finishes."
                                        .into(),
                                );
                            return Task::none();
                        }
                        dialog.begin(Phase::Saving);
                        return Task::future(async move {
                            let result = profile::connections_path()
                                .and_then(Store::open)
                                .and_then(|mut store| {
                                    preview
                                        .apply(&mut store)
                                        .map(|outcome| (outcome, Box::new(store)))
                                })
                                .map_err(|error| {
                                    format!("Could not import connections ({error}).")
                                });
                            event(id, Event::Committed(revision, result))
                        });
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn finish_connection_import(
        &mut self,
        id: window::Id,
        outcome: Outcome,
        result: Result<Box<Store>, String>,
    ) -> Task<Desktop> {
        let Some(dialog) = self.chrome.connection_import.take() else {
            return Task::none();
        };
        let mut status = format!(
            "Imported {} connection(s). {} already saved.",
            outcome.imported, outcome.already_saved
        );
        match result {
            Ok(store) => self.update_connection_store(*store),
            Err(error) => {
                status.push(' ');
                status.push_str(&error);
            }
        }
        if self.connections_id == Some(dialog.owner)
            && let Some(manager) = &mut self.connections
        {
            manager.set_notice(status);
        } else {
            self.notice(dialog.owner, status);
        }
        Task::batch([
            self.close_placed_dialog(id),
            window::gain_focus(dialog.owner),
        ])
    }
}

fn reload(id: window::Id, revision: u64, outcome: Outcome) -> Task<Desktop> {
    Task::future(async move {
        let store = profile::connections_path()
            .and_then(Store::open)
            .map(Box::new)
            .map_err(|error| format!("Could not refresh saved connections ({error})."));
        event(id, Event::Refreshed(revision, outcome, store))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::chrome::{Action, Category, Entry};

    #[test]
    fn connection_import_menu_is_owner_modal_and_rejects_stale_file_results() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        assert!(
            Category::Connections
                .entries()
                .iter()
                .any(|entry| matches!(entry, Entry::Action(Action::ImportConnections)))
        );
        let _ = workspace.update_chrome(Message::Invoke(owner, Action::ImportConnections));
        let id = workspace.connection_import_for_owner(owner).unwrap();
        assert_eq!(workspace.connection_import_for_owner(other), None);
        assert_eq!(workspace.title(id), TITLE);
        assert!(workspace.connection_import_view(id).is_some());
        assert!(!workspace.chrome_enabled(owner, Action::ManageConnections));

        let dialog = workspace.chrome.connection_import.as_mut().unwrap();
        let generation = dialog.begin(Phase::Loading);
        // Test double for an asynchronous response from a superseded file read.
        let response = Event::Reviewed(
            generation.wrapping_sub(1),
            Ok(Review::Preview(Arc::new(LoadedPreview {
                preview: Arc::new(Preview::default()),
                store: Store::open(std::env::temp_dir().join("unused-import-test-catalog.json"))
                    .unwrap(),
            }))),
        );
        let _ = workspace.update(event(id, response));
        assert!(
            workspace
                .chrome
                .connection_import
                .as_ref()
                .unwrap()
                .preview
                .is_none()
        );
        let _ = workspace.update(event(id, Event::Cancel));
        assert!(!workspace.connection_import_open());
        assert!(workspace.chrome_enabled(owner, Action::ManageConnections));
    }

    #[test]
    fn connection_import_discards_archive_and_key_secrets_when_the_selection_changes() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.show_connection_import(owner);
        let id = workspace.connection_import_for_owner(owner).unwrap();
        let archive = std::env::temp_dir().join("protected-connections.zip");
        // An actual regular-file identity for the synthetic worker replies.
        let fixture = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
        let identity = FileStamp::read(&fixture).unwrap();
        let first_generation = {
            let dialog = workspace.chrome.connection_import.as_mut().unwrap();
            let _ = dialog.review_source(archive);
            dialog.generation
        };
        // Test double for completion of the file reader.
        let _ = workspace.update_connection_import(
            id,
            Event::Reviewed(
                first_generation,
                Ok(Review::ZipPasswordRequired(identity.clone())),
            ),
        );
        let password = Event::Password(
            first_generation,
            PasswordInput(Secret::new("fixture-archive-secret".into())),
        );
        assert!(!format!("{password:?}").contains("fixture-archive-secret"));
        let _ = workspace.update_connection_import(id, password.clone());
        let _ = workspace.update_connection_import(
            id,
            Event::SavedPassword(first_generation, Some("fixture-vault-id".into())),
        );
        let dialog = workspace.chrome.connection_import.as_ref().unwrap();
        assert!(dialog.password.is_empty());
        assert_eq!(dialog.saved_password.as_deref(), Some("fixture-vault-id"));

        let next = std::env::temp_dir().join("other-connections.gpg");
        let _ = workspace.update_connection_import(id, Event::Path(next.to_str().unwrap().into()));
        let _ = workspace.update_connection_import(id, password);
        let dialog = workspace.chrome.connection_import.as_ref().unwrap();
        assert!(dialog.source.is_none());
        assert_eq!(dialog.unlock, Unlock::None);
        assert!(dialog.password.is_empty());
        assert!(dialog.saved_password.is_none());

        let _ = workspace.update_connection_import(id, Event::InspectPath);
        let second_generation = workspace
            .chrome
            .connection_import
            .as_ref()
            .unwrap()
            .generation;
        // Test double for the OpenPGP key-required outcome.
        let _ = workspace.update_connection_import(
            id,
            Event::Reviewed(second_generation, Ok(Review::OpenPgpKeyRequired(identity))),
        );
        let key = std::env::temp_dir().join("first-key.asc");
        let _ =
            workspace.update_connection_import(id, Event::KeyPath(key.to_str().unwrap().into()));
        let key_generation = workspace
            .chrome
            .connection_import
            .as_ref()
            .unwrap()
            .generation;
        let passphrase = Event::Password(
            key_generation,
            PasswordInput(Secret::new("fixture-key-passphrase".into())),
        );
        let _ = workspace.update_connection_import(id, passphrase.clone());
        let changed = std::env::temp_dir().join("second-key.asc");
        let _ = workspace
            .update_connection_import(id, Event::KeyPath(changed.to_str().unwrap().into()));
        let _ = workspace.update_connection_import(id, passphrase);
        let dialog = workspace.chrome.connection_import.as_ref().unwrap();
        assert_eq!(dialog.unlock, Unlock::OpenPgp);
        assert!(dialog.password.is_empty());
        assert!(dialog.saved_password.is_none());
    }
}
