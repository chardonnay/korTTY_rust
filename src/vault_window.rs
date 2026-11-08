use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use iced::widget::{
    button, column, container, pick_list, row, scrollable, space, text, text_input,
};

use iced::{Alignment, Element, Length, Task};
use kortty_vault::{
    Credential, CredentialInput, CredentialKind, MAX_MASTER_PASSWORD_BYTES,
    MIN_MASTER_PASSWORD_BYTES, Secret, Vault, valid_archive_password,
};

use crate::connection_window::CredentialChoice;
use crate::ssh_dialog::LoginTarget;
use crate::workspace::environment_access::{self, Catalog, DEFAULT_ENVIRONMENT};

pub(crate) const VAULT_LOCKED: &str = "Unlock the vault before connecting.";

#[derive(Clone)]
pub struct SecretEdit(Secret);

impl SecretEdit {
    fn new(value: String) -> Self {
        Self(Secret::new(value))
    }
}

impl fmt::Debug for SecretEdit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    FocusUnlock,
    Primary(SecretEdit),
    Confirm(SecretEdit),
    NewMaster(SecretEdit),
    Unlock,
    Lock,
    NewEntry,
    Select(String),
    EditSelected,
    Field(Field),
    SaveEntry,
    DeleteEntry,
    ConfirmDelete,
    ChangeMaster,
    Backup,
    ManageEnvironments,
    Environment(iced::window::Id, environment_access::Message),
    Restore,
    BackupPath(String),
    SubmitMaster,
    SubmitBackup,
    CancelEdit,
    Close,
    KeepEditing,
    DiscardAndClose,
    Completed(Completion),
}

#[derive(Clone, Debug)]
pub enum Field {
    Label(String),
    Username(String),
    HostPattern(String),
    Environment(String),
    Description(String),
    Kind(Kind),
    Secret(SecretEdit),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Password,
    KeyPassphrase,
    ArchivePassword,
}

impl Kind {
    fn native(self) -> CredentialKind {
        match self {
            Self::Password => CredentialKind::Password,
            Self::KeyPassphrase => CredentialKind::KeyPassphrase,
            Self::ArchivePassword => CredentialKind::ArchivePassword,
        }
    }

    fn from_native(kind: CredentialKind) -> Self {
        match kind {
            CredentialKind::Password => Self::Password,
            CredentialKind::KeyPassphrase => Self::KeyPassphrase,
            CredentialKind::ArchivePassword => Self::ArchivePassword,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Password => "SSH password",
            Self::KeyPassphrase => "SSH key passphrase",
            Self::ArchivePassword => "ZIP archive password",
        })
    }
}

#[derive(Clone)]
pub struct Completion(Arc<Mutex<Option<Finished>>>);

impl Completion {
    #[cfg(test)]
    pub(crate) fn unlock_fixture(path: &Path, password: &[u8], expected_success: bool) -> Self {
        // Test double for Iced's completion delivery. The encrypted vault is opened normally.
        let result = Vault::unlock(path, password);
        if expected_success {
            assert!(
                result.is_ok(),
                "the fixture vault could not be unlocked: {:?}",
                result.as_ref().err()
            );
        } else {
            assert!(
                matches!(result, Err(kortty_vault::Error::Authentication)),
                "the fixture must reject only the wrong password: {:?}",
                result.as_ref().err()
            );
        }
        let status = result
            .as_ref()
            .map(|_| "Vault unlocked.")
            .map_err(ToString::to_string);
        Self(Arc::new(Mutex::new(Some(Finished {
            vault: result.ok(),
            status,
            selected: None,
        }))))
    }
}

impl fmt::Debug for Completion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VaultCompletion")
            .finish_non_exhaustive()
    }
}

struct Finished {
    vault: Option<Vault>,
    status: Result<&'static str, String>,
    selected: Option<String>,
}

pub struct VaultWindow {
    path: PathBuf,
    exists: bool,
    vault: Option<Vault>,
    primary: Secret,
    confirm: Secret,
    next_master: Secret,
    draft: Option<EntryDraft>,
    operation: Option<Operation>,
    backup_path: String,
    selected: Option<String>,
    status: Option<String>,
    busy: bool,
    confirming_close: bool,
    unlock_prompt: Option<UnlockPrompt>,
}

struct UnlockPrompt {
    owner: iced::window::Id,
    input: iced::widget::Id,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    ChangeMaster,
    Backup,
    Delete,
    Restore,
}

struct EntryDraft {
    id: Option<String>,
    label: String,
    username: String,
    host_pattern: String,
    environment: String,
    description: String,
    kind: Kind,
    secret: Secret,
    changed: bool,
}

impl VaultWindow {
    #[cfg(test)]
    pub(crate) fn with_unlocked_fixture(path: PathBuf, vault: Vault) -> Self {
        let mut window = Self::open(path).expect("fixture vault exists");
        window.vault = Some(vault);
        window
    }

    pub fn open(path: PathBuf) -> Result<Self, String> {
        let exists = Vault::exists(&path).map_err(|error| error.to_string())?;
        Ok(Self {
            path,
            exists,
            vault: None,
            primary: Secret::default(),
            confirm: Secret::default(),
            next_master: Secret::default(),
            draft: None,
            operation: None,
            backup_path: String::new(),
            selected: None,
            status: None,
            busy: false,
            confirming_close: false,
            unlock_prompt: None,
        })
    }

    pub(crate) fn can_unlock_existing(&self) -> bool {
        self.exists && self.vault.is_none() && !self.busy
    }

    pub(crate) fn open_unlock_prompt(&mut self, owner: iced::window::Id) {
        self.clear_form();
        self.status = None;
        self.unlock_prompt = Some(UnlockPrompt {
            owner,
            input: iced::widget::Id::unique(),
        });
    }

    pub(crate) fn unlock_prompt_owner(&self) -> Option<iced::window::Id> {
        self.unlock_prompt.as_ref().map(|prompt| prompt.owner)
    }

    pub(crate) fn unlock_prompt_input(&self) -> Option<iced::widget::Id> {
        self.unlock_prompt
            .as_ref()
            .map(|prompt| prompt.input.clone())
    }

    pub(crate) fn unlocked_prompt(&self) -> bool {
        self.unlock_prompt.is_some() && self.vault.is_some()
    }

    pub(crate) fn is_unlocked(&self) -> bool {
        self.vault.is_some()
    }

    pub(crate) fn close_form(&mut self) {
        self.clear_form();
        if self.unlock_prompt.take().is_some() && self.vault.is_none() {
            self.status = None;
        }
    }

    pub fn choices(&self, kind: Kind) -> Vec<CredentialChoice> {
        self.choices_for(kind, None)
    }

    pub fn choices_for(&self, kind: Kind, endpoint: Option<&LoginTarget>) -> Vec<CredentialChoice> {
        self.vault.as_ref().map_or_else(Vec::new, |vault| {
            vault
                .entries()
                .filter(|entry| {
                    entry.kind == kind.native()
                        && endpoint.is_none_or(|endpoint| matches_endpoint(entry, endpoint))
                })
                .map(|entry| CredentialChoice {
                    id: entry.id.clone(),
                    label: entry.label.clone(),
                    username: entry.username.clone(),
                })
                .collect()
        })
    }

    pub fn resolve(
        &self,
        id: &str,
        kind: Kind,
        endpoint: &LoginTarget,
    ) -> Result<Secret, &'static str> {
        let vault = self.vault.as_ref().ok_or(VAULT_LOCKED)?;
        resolve_credential(vault, id, kind, endpoint)
    }

    pub(crate) fn archive_password(&self, id: &str) -> Result<Secret, &'static str> {
        let vault = self
            .vault
            .as_ref()
            .ok_or("Unlock the vault to use the saved archive password.")?;
        vault
            .archive_password(id)
            .map(|password| Secret::new(password.to_owned()))
            .ok_or("The selected ZIP archive password is missing or has the wrong type.")
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub(crate) fn can_manage_environments(&self) -> bool {
        self.vault.is_some()
            && !self.busy
            && !self.confirming_close
            && self.draft.is_none()
            && self.operation.is_none()
    }

    pub(crate) fn take_for_ssh_keys(&mut self) -> Option<Vault> {
        if !self.can_manage_environments() {
            return None;
        }
        self.busy = true;
        self.vault.take()
    }

    pub(crate) fn restore_after_ssh_keys(&mut self, vault: Vault) {
        self.vault = Some(vault);
        self.busy = false;
    }

    pub(crate) fn environment_usage(&self, id: &str) -> Option<usize> {
        Some(
            self.vault
                .as_ref()?
                .entries()
                .filter(|entry| entry.environment == id)
                .count(),
        )
    }

    pub(crate) fn credential_environment(&self, id: &str) -> Option<&str> {
        let entry = self.vault.as_ref()?.get(id)?;
        (entry.kind == CredentialKind::Password && !entry.environment.is_empty())
            .then_some(entry.environment.as_str())
    }

    pub fn request_close(&mut self) -> bool {
        if self.busy {
            self.status = Some("A vault operation is in progress.".into());
            return false;
        }
        self.confirming_close = self.draft.as_ref().is_some_and(|draft| draft.changed);
        !self.confirming_close
    }

    pub fn clear_form(&mut self) {
        self.primary = Secret::default();
        self.confirm = Secret::default();
        self.next_master = Secret::default();
        self.draft = None;
        self.operation = None;
        self.backup_path.clear();
        self.confirming_close = false;
    }

    fn unlock(&mut self) -> Task<Message> {
        if !self.can_open() {
            return Task::none();
        }
        let primary = std::mem::take(&mut self.primary);
        self.confirm = Secret::default();
        let path = self.path.clone();
        let existing = self.exists;
        let backup = (self.operation == Some(Operation::Restore))
            .then(|| PathBuf::from(self.backup_path.trim()));
        self.busy = true;
        Task::future(async move {
            let (result, status) = if existing {
                (
                    Vault::unlock(&path, primary.as_bytes()).map_err(|error| error.to_string()),
                    "Vault unlocked.",
                )
            } else {
                let result = prepare_directory(&path).map_err(|error| error.to_string());
                if let Some(backup) = backup {
                    (
                        result.and_then(|()| {
                            Vault::restore_from(&path, &backup, primary.as_bytes())
                                .map_err(|error| error.to_string())
                        }),
                        "Vault restored.",
                    )
                } else {
                    (
                        result.and_then(|()| {
                            Vault::create(&path, primary.as_bytes())
                                .map_err(|error| error.to_string())
                        }),
                        "Vault created.",
                    )
                }
            };
            let result = match result {
                Ok(vault) => Finished {
                    vault: Some(vault),
                    status: Ok(status),
                    selected: None,
                },
                Err(error) => Finished {
                    vault: None,
                    status: Err(error),
                    selected: None,
                },
            };
            Message::Completed(Completion(Arc::new(Mutex::new(Some(result)))))
        })
    }

    fn can_open(&self) -> bool {
        !self.primary.is_empty()
            && if self.exists {
                true
            } else if self.unlock_prompt.is_some() {
                false
            } else if self.operation == Some(Operation::Restore) {
                Path::new(self.backup_path.trim()).is_absolute()
            } else {
                self.master_confirmed(&self.primary)
            }
    }

    fn mutate(
        &mut self,
        operation: impl FnOnce(
            &mut Vault,
        ) -> Result<(Option<String>, &'static str), kortty_vault::Error>
        + Send
        + 'static,
    ) -> Task<Message> {
        let Some(mut vault) = self.vault.take() else {
            return Task::none();
        };
        let previous = self.selected.clone();
        self.busy = true;
        Task::future(async move {
            let (selected, status) = match operation(&mut vault) {
                Ok((id, status)) => (id.or(previous), Ok(status)),
                Err(error) => (previous, Err(error.to_string())),
            };
            let result = Finished {
                vault: Some(vault),
                status,
                selected,
            };
            Message::Completed(Completion(Arc::new(Mutex::new(Some(result)))))
        })
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        if self.unlock_prompt.is_some()
            && !matches!(
                message,
                Message::Primary(_) | Message::Unlock | Message::Completed(_)
            )
        {
            return Task::none();
        }
        if self.busy && !matches!(message, Message::Completed(_)) {
            return Task::none();
        }
        if self.confirming_close && !matches!(message, Message::KeepEditing) {
            return Task::none();
        }
        match message {
            Message::Primary(value) => {
                self.primary = bound_secret(value.0, MAX_MASTER_PASSWORD_BYTES)
            }
            Message::Confirm(value) => {
                self.confirm = bound_secret(value.0, MAX_MASTER_PASSWORD_BYTES)
            }
            Message::NewMaster(value) => {
                self.next_master = bound_secret(value.0, MAX_MASTER_PASSWORD_BYTES)
            }
            Message::Unlock if self.vault.is_none() => return self.unlock(),
            Message::Lock => {
                self.vault = None;
                self.clear_form();
                self.selected = None;
                self.status = Some("Vault locked.".into());
            }
            Message::NewEntry if self.operation.is_none() && self.draft.is_none() => {
                if self.vault.is_some() {
                    self.draft = Some(EntryDraft::new());
                }
            }
            Message::Select(id) if self.draft.is_none() && self.operation.is_none() => {
                self.selected = Some(id);
            }
            Message::EditSelected if self.draft.is_none() && self.operation.is_none() => {
                self.draft = self.selected_entry().map(EntryDraft::from_entry);
            }
            Message::Field(field) => {
                if let Some(draft) = &mut self.draft {
                    draft.update(field);
                }
            }
            Message::SaveEntry => {
                let Some(draft) = self.draft.as_mut().filter(|draft| draft.valid()) else {
                    return Task::none();
                };
                let update_secret = std::mem::take(&mut draft.secret);
                let input = draft.input();
                return self.mutate(move |vault| {
                    let secret = if update_secret.is_empty() {
                        Secret::new(
                            input
                                .id
                                .as_deref()
                                .and_then(|id| vault.secret(id))
                                .unwrap_or_default()
                                .to_owned(),
                        )
                    } else {
                        update_secret
                    };
                    vault
                        .save(input, secret)
                        .map(|id| (Some(id), "Credential saved."))
                });
            }
            Message::DeleteEntry if self.selected_entry().is_some() && self.draft.is_none() => {
                self.operation = Some(Operation::Delete);
            }
            Message::ConfirmDelete if self.operation == Some(Operation::Delete) => {
                self.operation = None;
                if let Some(id) = self.selected.clone() {
                    return self.mutate(move |vault| {
                        vault.delete(&id).map(|_| (None, "Credential deleted."))
                    });
                }
            }
            Message::ChangeMaster if self.draft.is_none() && self.operation.is_none() => {
                self.operation = Some(Operation::ChangeMaster);
            }
            Message::Backup if self.draft.is_none() && self.operation.is_none() => {
                self.operation = Some(Operation::Backup);
            }
            Message::Restore if !self.exists && self.vault.is_none() => {
                self.clear_form();
                self.operation = Some(Operation::Restore);
            }
            Message::BackupPath(value) => self.backup_path = bound(value, 4096),
            Message::SubmitMaster if self.operation == Some(Operation::ChangeMaster) => {
                if self.primary.is_empty() || !self.master_confirmed(&self.next_master) {
                    return Task::none();
                }
                let old = std::mem::take(&mut self.primary);
                let new = std::mem::take(&mut self.next_master);
                self.confirm = Secret::default();
                self.operation = None;
                return self.mutate(move |vault| {
                    vault
                        .change_master_password(old.as_bytes(), new.as_bytes())
                        .map(|()| (None, "Master password changed."))
                });
            }
            Message::SubmitBackup if self.operation == Some(Operation::Backup) => {
                let path = PathBuf::from(self.backup_path.trim());
                if !path.is_absolute() {
                    return Task::none();
                }
                self.operation = None;
                self.backup_path.clear();
                return self.mutate(move |vault| {
                    vault
                        .backup_to(&path)
                        .map(|()| (None, "Encrypted vault backup saved."))
                });
            }
            Message::CancelEdit => self.clear_form(),
            Message::KeepEditing => self.confirming_close = false,
            Message::Completed(reply) => {
                let mut slot = reply.0.lock().unwrap_or_else(|error| error.into_inner());
                let Some(finished) = slot.take() else {
                    return Task::none();
                };
                self.busy = false;
                if finished.vault.is_some() {
                    self.exists = true;
                } else if let Ok(exists) = Vault::exists(&self.path) {
                    self.exists = exists;
                }
                if finished.status.is_ok() {
                    self.clear_form();
                }
                self.vault = finished.vault;
                self.selected = finished.selected.filter(|id| {
                    self.vault
                        .as_ref()
                        .is_some_and(|vault| vault.get(id).is_some())
                });
                self.status = Some(
                    finished
                        .status
                        .map(str::to_owned)
                        .unwrap_or_else(|error| format!("Vault operation failed ({error})")),
                );
            }
            _ => {}
        }
        Task::none()
    }

    fn selected_entry(&self) -> Option<&Credential> {
        self.vault.as_ref()?.get(self.selected.as_deref()?)
    }

    fn master_confirmed(&self, master: &Secret) -> bool {
        master.len() >= MIN_MASTER_PASSWORD_BYTES && master.as_str() == self.confirm.as_str()
    }

    pub(crate) fn view<'a>(&'a self, environments: &'a Catalog) -> Element<'a, Message> {
        if let Some(prompt) = &self.unlock_prompt {
            return self.unlock_prompt_view(prompt);
        }
        let mut contents = column![
            row![
                text("Credential vault").size(23),
                space::horizontal(),
                button("Close").on_press(Message::Close)
            ]
            .align_y(Alignment::Center),
        ]
        .padding(14)
        .spacing(10)
        .height(Length::Fill);
        if let Some(status) = &self.status {
            contents = contents.push(text(status).size(13));
        }
        if self.busy {
            return contents.push(text("Vault operation in progress…")).into();
        }
        if self.confirming_close {
            return contents
                .push(
                    row![
                        text("Discard the unsaved credential changes?"),
                        button("Keep editing").on_press(Message::KeepEditing),
                        button("Discard and close")
                            .style(button::danger)
                            .on_press(Message::DiscardAndClose),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                )
                .into();
        }
        if let Some(vault) = &self.vault {
            let editing = self.operation.is_some() || self.draft.is_some();
            let selected = self.selected_entry().is_some() && !editing;
            contents = contents.push(
                row![
                    button("New credential")
                        .on_press_maybe((!editing).then_some(Message::NewEntry)),
                    button("Edit").on_press_maybe(selected.then_some(Message::EditSelected)),
                    button("Delete")
                        .style(button::danger)
                        .on_press_maybe(selected.then_some(Message::DeleteEntry)),
                    button("Change master password")
                        .on_press_maybe((!editing).then_some(Message::ChangeMaster)),
                    button("Encrypted backup")
                        .on_press_maybe((!editing).then_some(Message::Backup)),
                    button("Manage Environments…").on_press_maybe(
                        self.can_manage_environments()
                            .then_some(Message::ManageEnvironments)
                    ),
                    button("Lock").on_press_maybe((!editing).then_some(Message::Lock)),
                ]
                .spacing(6)
                .wrap(),
            );
            let mut entries = column![].spacing(6);
            for entry in vault.entries() {
                let environment = environments.display_label(&entry.environment);
                entries = entries.push(
                    button(
                        column![
                            text(&entry.label),
                            row![
                                (!entry.username.is_empty())
                                    .then(|| text(&entry.username).size(12)),
                                (!environment.is_empty()).then(|| text(environment).size(12)),
                            ]
                            .spacing(8),
                        ]
                        .spacing(3),
                    )
                    .style(if self.selected.as_deref() == Some(&entry.id) {
                        button::primary
                    } else {
                        button::secondary
                    })
                    .width(Length::Fill)
                    .on_press_maybe((!editing).then(|| Message::Select(entry.id.clone()))),
                );
            }
            let right = self.unlocked_details(environments);
            return contents
                .push(
                    row![
                        scrollable(entries).width(Length::FillPortion(1)),
                        scrollable(container(right).width(Length::Fill))
                            .width(Length::FillPortion(2)),
                    ]
                    .spacing(12)
                    .height(Length::Fill),
                )
                .into();
        }
        let submit = self.can_open().then_some(Message::Unlock);
        if !self.exists && self.operation == Some(Operation::Restore) {
            return contents
                .push(text("Restore an encrypted vault backup").size(18))
                .push(
                    text_input("Absolute path to the backup file", &self.backup_path)
                        .on_input(Message::BackupPath),
                )
                .push(
                    text_input("Backup master password", &self.primary)
                        .secure(true)
                        .on_input(|value| Message::Primary(SecretEdit::new(value)))
                        .on_submit_maybe(submit.clone()),
                )
                .push(
                    row![
                        button("Restore vault").on_press_maybe(submit),
                        button("Cancel").on_press(Message::CancelEdit),
                    ]
                    .spacing(8),
                )
                .into();
        }
        contents = contents
            .push(
                text(if self.exists {
                    "Unlock the vault"
                } else {
                    "Create a new vault"
                })
                .size(18),
            )
            .push(
                text_input("Master password", &self.primary)
                    .secure(true)
                    .on_input(|value| Message::Primary(SecretEdit::new(value)))
                    .on_submit_maybe(submit.clone()),
            );
        if !self.exists {
            contents = contents
                .push(
                    text_input("Repeat master password", &self.confirm)
                        .secure(true)
                        .on_input(|value| Message::Confirm(SecretEdit::new(value)))
                        .on_submit_maybe(submit.clone()),
                )
                .push(
                    text(format!(
                        "Use at least {MIN_MASTER_PASSWORD_BYTES} UTF-8 bytes."
                    ))
                    .size(13),
                );
        }
        let mut actions = row![
            button(if self.exists {
                "Unlock"
            } else {
                "Create vault"
            })
            .on_press_maybe(submit)
        ]
        .spacing(8);
        if !self.exists {
            actions = actions.push(button("Restore encrypted backup").on_press(Message::Restore));
        }
        contents.push(actions).into()
    }

    fn unlock_prompt_view<'a>(&'a self, prompt: &'a UnlockPrompt) -> Element<'a, Message> {
        let submit = (self.can_unlock_existing() && self.can_open()).then_some(Message::Unlock);
        let mut contents = column![text("Enter the master password to unlock the vault").size(18)];
        if self.busy {
            contents = contents.push(text("Unlocking…"));
        } else {
            contents = contents.push(
                text_input("Master password", &self.primary)
                    .id(prompt.input.clone())
                    .secure(true)
                    .on_input(|value| Message::Primary(SecretEdit::new(value)))
                    .on_submit_maybe(submit.clone()),
            );
        }
        if let Some(status) = &self.status
            && !self.busy
        {
            contents = contents.push(text(status).size(13));
        }
        contents
            .push(
                row![
                    space::horizontal(),
                    button("Unlock").on_press_maybe(submit),
                    button("Cancel").on_press_maybe((!self.busy).then_some(Message::Close)),
                ]
                .spacing(8),
            )
            .padding(14)
            .spacing(10)
            .into()
    }

    fn unlocked_details<'a>(&'a self, environments: &'a Catalog) -> Element<'a, Message> {
        match self.operation {
            Some(Operation::Delete) => {
                return column![
                    text("Delete this credential from the vault?"),
                    text("Connections that refer to it will need a different credential.").size(13),
                    row![
                        button("Cancel").on_press(Message::CancelEdit),
                        button("Delete credential")
                            .style(button::danger)
                            .on_press(Message::ConfirmDelete),
                    ]
                    .spacing(8),
                ]
                .spacing(8)
                .into();
            }
            Some(Operation::ChangeMaster) => {
                return column![
                    text_input("Current master password", &self.primary)
                        .secure(true)
                        .on_input(|value| Message::Primary(SecretEdit::new(value))),
                    text_input("New master password", &self.next_master)
                        .secure(true)
                        .on_input(|value| Message::NewMaster(SecretEdit::new(value))),
                    text_input("Repeat new master password", &self.confirm)
                        .secure(true)
                        .on_input(|value| Message::Confirm(SecretEdit::new(value))),
                    text(format!(
                        "Use at least {MIN_MASTER_PASSWORD_BYTES} UTF-8 bytes."
                    ))
                    .size(13),
                    row![
                        button("Change password").on_press_maybe(
                            (!self.primary.is_empty() && self.master_confirmed(&self.next_master))
                                .then_some(Message::SubmitMaster)
                        ),
                        button("Cancel").on_press(Message::CancelEdit)
                    ]
                    .spacing(8),
                ]
                .spacing(8)
                .into();
            }
            Some(Operation::Backup) => {
                return column![
                    text_input("Absolute path to a new backup file", &self.backup_path)
                        .on_input(Message::BackupPath),
                    row![
                        button("Save encrypted backup").on_press_maybe(
                            Path::new(self.backup_path.trim())
                                .is_absolute()
                                .then_some(Message::SubmitBackup)
                        ),
                        button("Cancel").on_press(Message::CancelEdit),
                    ]
                    .spacing(8),
                ]
                .spacing(8)
                .into();
            }
            Some(Operation::Restore) | None => {}
        }
        if let Some(draft) = &self.draft {
            return draft.view(environments);
        }
        if let Some(entry) = self.selected_entry() {
            return column![
                text(&entry.label).size(20),
                text(Kind::from_native(entry.kind).to_string()),
                text(format!("Username  {}", entry.username)),
                text(format!(
                    "Host pattern  {}",
                    if entry.server_pattern.is_empty() {
                        "Any host"
                    } else {
                        &entry.server_pattern
                    }
                )),
                text(format!(
                    "Environment  {}",
                    environments.display_label(&entry.environment)
                )),
                text(&entry.description),
            ]
            .spacing(8)
            .into();
        }
        text("Select a credential or create one.").into()
    }
}

pub(crate) fn resolve_credential(
    vault: &Vault,
    id: &str,
    kind: Kind,
    endpoint: &LoginTarget,
) -> Result<Secret, &'static str> {
    let entry = vault
        .get(id)
        .ok_or("The selected vault credential is missing.")?;
    if kind == Kind::ArchivePassword
        || entry.kind != kind.native()
        || !matches_endpoint(entry, endpoint)
    {
        return Err("The vault credential is outside the scope of this SSH login.");
    }
    vault
        .secret(id)
        .map(|secret| Secret::new(secret.to_owned()))
        .ok_or("The selected vault credential is missing.")
}

fn matches_endpoint(entry: &Credential, endpoint: &LoginTarget) -> bool {
    entry.matches_host(&endpoint.host)
        && (entry.username.is_empty() || entry.username == endpoint.username)
}

impl EntryDraft {
    fn new() -> Self {
        Self {
            id: None,
            label: String::new(),
            username: String::new(),
            host_pattern: String::new(),
            environment: DEFAULT_ENVIRONMENT.to_owned(),
            description: String::new(),
            kind: Kind::Password,
            secret: Secret::default(),
            changed: false,
        }
    }

    fn from_entry(entry: &Credential) -> Self {
        Self {
            id: Some(entry.id.clone()),
            label: entry.label.clone(),
            username: entry.username.clone(),
            host_pattern: entry.server_pattern.clone(),
            environment: entry.environment.clone(),
            description: entry.description.clone(),
            kind: Kind::from_native(entry.kind),
            ..Self::new()
        }
    }

    fn valid(&self) -> bool {
        !self.label.trim().is_empty()
            && (self.id.is_some() && self.secret.is_empty()
                || if self.kind == Kind::ArchivePassword {
                    valid_archive_password(&self.secret)
                } else {
                    !self.secret.is_empty()
                })
    }

    fn update(&mut self, field: Field) {
        match field {
            Field::Label(value) => self.label = bound(value, 160),
            Field::Username(value) => self.username = bound(value, 255),
            Field::HostPattern(value) => self.host_pattern = bound(value, 256),
            Field::Environment(value) => self.environment = bound(value, 128),
            Field::Description(value) => self.description = bound(value, 1024),
            Field::Kind(kind) => {
                let archive = Kind::ArchivePassword;
                if kind != self.kind && (kind == archive || self.kind == archive) {
                    if self.id.is_some() {
                        return;
                    }
                    self.secret = Secret::default();
                    if kind == archive && self.environment == DEFAULT_ENVIRONMENT {
                        self.environment.clear();
                    } else if self.kind == archive && self.environment.is_empty() {
                        self.environment = DEFAULT_ENVIRONMENT.to_owned();
                    }
                }
                self.kind = kind;
            }
            Field::Secret(secret) => self.secret = bound_secret(secret.0, 4096),
        }
        self.changed = true;
    }

    fn input(&self) -> CredentialInput {
        CredentialInput {
            id: self.id.clone(),
            label: self.label.trim().to_owned(),
            username: if self.kind == Kind::ArchivePassword {
                String::new()
            } else {
                self.username.trim().to_owned()
            },
            kind: self.kind.native(),
            server_pattern: if self.kind == Kind::ArchivePassword {
                String::new()
            } else {
                self.host_pattern.trim().to_owned()
            },
            environment: self.environment.trim().to_owned(),
            description: self.description.trim().to_owned(),
        }
    }

    fn view<'a>(&'a self, environments: &'a Catalog) -> Element<'a, Message> {
        let kinds: &[Kind] = if self.id.is_none() {
            &[Kind::Password, Kind::KeyPassphrase, Kind::ArchivePassword]
        } else if self.kind == Kind::ArchivePassword {
            &[Kind::ArchivePassword]
        } else {
            &[Kind::Password, Kind::KeyPassphrase]
        };
        let scope: Element<'_, Message> = if self.kind == Kind::ArchivePassword {
            text("Used by encrypted ZIP jobs while the vault is unlocked.")
                .size(13)
                .into()
        } else {
            column![
                text_input("Username (optional scope)", &self.username)
                    .on_input(|value| Message::Field(Field::Username(value))),
                text_input("Host pattern (optional, * wildcard)", &self.host_pattern)
                    .on_input(|value| Message::Field(Field::HostPattern(value))),
            ]
            .spacing(6)
            .into()
        };
        let choices = environments.choices(&self.environment);
        let selection = environments.selection(&choices, &self.environment);
        column![
            pick_list(kinds, Some(self.kind), |value| Message::Field(Field::Kind(
                value
            ))),
            text_input("Credential name", &self.label)
                .on_input(|value| Message::Field(Field::Label(value))),
            scope,
            row![
                text("Environment").width(92),
                pick_list(choices, selection, |choice| {
                    Message::Field(Field::Environment(choice.id))
                })
            ]
            .spacing(6)
            .align_y(Alignment::Center),
            text_input("Description (optional)", &self.description)
                .on_input(|value| Message::Field(Field::Description(value))),
            text_input(
                if self.id.is_some() {
                    "New secret (blank keeps the stored value)"
                } else {
                    "Secret"
                },
                &self.secret
            )
            .secure(true)
            .on_input(|value| Message::Field(Field::Secret(SecretEdit::new(value)))),
            row![
                button("Save credential")
                    .on_press_maybe(self.valid().then_some(Message::SaveEntry)),
                button("Cancel").on_press(Message::CancelEdit),
            ]
            .spacing(8),
        ]
        .spacing(8)
        .into()
    }
}

fn prepare_directory(path: &Path) -> std::io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| std::io::Error::other("Vault path has no directory"))?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(directory)
}

fn bound(value: String, max: usize) -> String {
    if value.len() <= max {
        value
    } else {
        String::new()
    }
}

fn bound_secret(value: Secret, max: usize) -> Secret {
    if value.len() <= max {
        value
    } else {
        Secret::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_resolution_requires_an_unlocked_matching_entry() {
        let mut random = [0; 16];
        getrandom::fill(&mut random).unwrap();
        let name = u128::from_le_bytes(random);
        let root = std::env::temp_dir().join(format!("kortty-vault-ui-{name:032x}"));
        let path = root.join("vault.bin");
        prepare_directory(&path).unwrap();
        let mut window = VaultWindow::open(path.clone()).unwrap();
        let mut vault = Vault::create(&path, b"app-test-master-password").unwrap();
        let id = vault
            .save(
                CredentialInput {
                    label: "Test entry".into(),
                    username: "operator".into(),
                    server_pattern: "*.example.test".into(),
                    ..CredentialInput::default()
                },
                Secret::new("app-test-secret".into()),
            )
            .unwrap();
        let archive_id = vault
            .save(
                CredentialInput {
                    label: "ZIP archive".into(),
                    kind: CredentialKind::ArchivePassword,
                    ..CredentialInput::default()
                },
                Secret::new("app-test-archive-password".into()),
            )
            .unwrap();
        let mut endpoint = LoginTarget {
            host: "server.example.test.".into(),
            port: 22,
            username: "operator".into(),
        };
        assert!(window.resolve(&id, Kind::Password, &endpoint).is_err());
        assert!(window.archive_password(&archive_id).is_err());
        window.vault = Some(vault);
        assert_eq!(window.choices(Kind::ArchivePassword)[0].id, archive_id);
        assert_eq!(
            window.archive_password(&archive_id).unwrap().as_str(),
            "app-test-archive-password",
        );
        assert!(window.archive_password(&id).is_err());
        assert!(
            window
                .resolve(&archive_id, Kind::Password, &endpoint)
                .is_err()
        );
        assert_eq!(
            window
                .resolve(&id, Kind::Password, &endpoint)
                .unwrap()
                .as_str(),
            "app-test-secret"
        );
        assert!(window.resolve(&id, Kind::KeyPassphrase, &endpoint).is_err());
        endpoint.username = "another-user".into();
        assert!(window.resolve(&id, Kind::Password, &endpoint).is_err());
        endpoint.username = "operator".into();
        endpoint.host = "unapproved.test".into();
        assert!(window.resolve(&id, Kind::Password, &endpoint).is_err());
        let _ = window.update(Message::Lock);
        assert!(window.choices(Kind::Password).is_empty());
        assert!(window.choices(Kind::ArchivePassword).is_empty());
        assert!(window.archive_password(&archive_id).is_err());
        drop(window);
        fs::remove_dir_all(root).unwrap();
    }
}
