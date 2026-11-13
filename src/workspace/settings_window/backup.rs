use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::{button, column, pick_list, row, rule, scrollable, text, text_input};
use iced::{Alignment, Element, Length, Task, window};

use crate::connection_window::CredentialChoice;
use crate::profile_backup::preferences::{
    EncryptionMode, Interval, MAX_RETENTION, Preferences, validate_directory,
};
use crate::sftp_window::{Directory, Entry, EntryKind, EntryName, list_local};
use crate::snippets::recipients::Choice as Recipient;
use crate::vault_window::{Kind, VaultWindow};

use super::{Desktop, Dialog, Message, Section, Setting, Workspace, highlighted};

const MAX_DIRECTORY_BYTES: usize = 4096;
const PAGE_SIZE: usize = 60;
const DIRECTORY_ERROR: &str = "Enter an absolute backup folder without parent path components.";

#[derive(Clone, Debug)]
pub enum Edit {
    Retention(String),
    StepRetention(bool),
    Encryption(EncryptionMode),
    Credential(Option<String>),
    OpenVault,
    Recipient(Option<String>),
    OpenRecipients,
    Interval(Interval),
    Directory(String),
    Browse,
    BrowseParent,
    BrowseEntry(u64, usize),
    BrowsePage(bool),
    Browsed(u64, Result<Arc<Directory>, String>),
    UseFolder(u64),
    CloseBrowser,
}

#[derive(Default)]
pub(super) struct Editor {
    retention: Option<String>,
    directory: Option<Result<String, &'static str>>,
    browser: Option<Browser>,
    revision: u64,
}

struct Browser {
    revision: u64,
    folder: PathBuf,
    page: usize,
    listing: Option<Result<Arc<Directory>, String>>,
}

#[derive(Clone, Eq, PartialEq)]
struct Choice {
    id: Option<String>,
    label: String,
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

impl Editor {
    pub(super) fn browsing(&self) -> bool {
        self.browser.is_some()
    }

    pub(super) fn close_browser(&mut self) {
        self.browser = None;
    }

    pub(super) fn merge(
        &self,
        edit: &Preferences,
        initial: &Preferences,
        current: &Preferences,
        interval_managed: bool,
        directory_managed: bool,
    ) -> Result<Preferences, &'static str> {
        let mut next = current.clone();
        if let Some(retention) = self.retention.as_deref().map(parse_retention).transpose()?
            && retention != initial.retention
        {
            next.retention = retention;
        }
        if edit.credential != initial.credential {
            next.credential.clone_from(&edit.credential);
        }
        if edit.encryption != initial.encryption {
            next.encryption = edit.encryption;
        }
        if edit.recipient != initial.recipient {
            next.recipient.clone_from(&edit.recipient);
        }
        if !interval_managed && edit.interval != initial.interval {
            next.interval = edit.interval;
        }
        if !directory_managed && let Some(directory) = &self.directory {
            let directory = parse_directory(directory.as_deref().map_err(|error| *error)?)?;
            if directory != initial.directory {
                next.directory = directory;
            }
        }
        Ok(next)
    }

    fn retention_text(&self, current: u8) -> Cow<'_, str> {
        self.retention
            .as_deref()
            .map_or_else(|| Cow::Owned(current.to_string()), Cow::Borrowed)
    }

    fn directory_text<'a>(
        &'a self,
        draft: &'a Preferences,
        managed: Option<&'a Path>,
    ) -> Cow<'a, str> {
        if let Some(path) = managed {
            return path.to_string_lossy();
        }
        if let Some(directory) = &self.directory {
            return Cow::Borrowed(directory.as_deref().unwrap_or_default());
        }
        draft
            .directory
            .as_deref()
            .map(Path::to_string_lossy)
            .unwrap_or_default()
    }

    fn browse(&mut self, id: window::Id, folder: PathBuf) -> Task<Desktop> {
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        self.browser = Some(Browser {
            revision,
            folder: folder.clone(),
            page: 0,
            listing: None,
        });
        Task::perform(
            async move {
                list_folders(&folder)
                    .map(Arc::new)
                    .map_err(|error| error.to_string())
            },
            move |result| wrap(id, Edit::Browsed(revision, result)),
        )
    }

    fn update_browser(&mut self, id: window::Id, edit: Edit) -> Task<Desktop> {
        let Some(browser) = &mut self.browser else {
            return Task::none();
        };
        match edit {
            Edit::BrowseParent => {
                if let Some(parent) = browser.folder.parent().map(Path::to_path_buf) {
                    return self.browse(id, parent);
                }
            }
            Edit::BrowseEntry(revision, index) if revision == browser.revision => {
                if let Some(entry) = browser.folders().get(index)
                    && entry.kind == EntryKind::Directory
                    && let EntryName::Local(name) = &entry.name
                {
                    let folder = browser.folder.join(name);
                    return self.browse(id, folder);
                }
            }
            Edit::BrowsePage(forward) => {
                browser.page = if forward {
                    (browser.page + 1).min(browser.last_page())
                } else {
                    browser.page.saturating_sub(1)
                };
            }
            Edit::Browsed(revision, result) if revision == browser.revision => {
                browser.listing = Some(result);
            }
            Edit::UseFolder(revision)
                if revision == browser.revision && matches!(browser.listing, Some(Ok(_))) =>
            {
                if validate_directory(&browser.folder).is_ok()
                    && let Some(path) = browser.folder.to_str()
                {
                    self.directory = Some(Ok(path.to_owned()));
                    self.close_browser();
                } else {
                    browser.listing =
                        Some(Err("Choose a folder that fits in a UTF-8 path.".into()));
                }
            }
            Edit::CloseBrowser => self.close_browser(),
            _ => {}
        }
        Task::none()
    }
}

impl Workspace {
    pub(super) fn update_backup_settings(&mut self, id: window::Id, edit: Edit) -> Task<Desktop> {
        let Some(dialog) = self.settings.dialog_mut(id) else {
            return Task::none();
        };
        if dialog.section != Section::Backup || self.clipboard.policy.problem.is_some() {
            return Task::none();
        }
        let encryption = dialog.draft.backup.encryption;
        if matches!(edit, Edit::OpenVault) && encryption == EncryptionMode::Password {
            let owner = dialog.owner;
            return self.show_vault(owner);
        }
        if matches!(edit, Edit::OpenRecipients) && encryption == EncryptionMode::OpenPgp {
            return self.show_openpgp(id);
        }
        if encryption == EncryptionMode::Password
            && let Edit::Credential(credential) = edit
        {
            if credential.as_ref().is_none_or(|id| {
                self.credential_choices(Kind::ArchivePassword)
                    .iter()
                    .any(|choice| &choice.id == id)
            }) && let Some(dialog) = self.settings.dialog_mut(id)
            {
                dialog.draft.backup.credential = credential;
                dialog.problem = None;
            }
            return Task::none();
        }
        if encryption == EncryptionMode::OpenPgp
            && let Edit::Recipient(recipient) = edit
        {
            if recipient.as_ref().is_none_or(|fingerprint| {
                self.backup_recipient_choices()
                    .iter()
                    .any(|choice| !choice.revoked() && choice.fingerprint() == fingerprint)
            }) && let Some(dialog) = self.settings.dialog_mut(id)
            {
                dialog.draft.backup.recipient = recipient;
                dialog.problem = None;
            }
            return Task::none();
        }

        let policy = &self.clipboard.policy.backup;
        match edit {
            Edit::Retention(value) => {
                dialog.backup.retention = Some(if value.len() <= 3 {
                    value
                } else {
                    String::new()
                });
            }
            Edit::StepRetention(increase) => {
                let current =
                    parse_retention(&dialog.backup.retention_text(dialog.draft.backup.retention))
                        .unwrap_or(dialog.draft.backup.retention);
                let next = if increase {
                    current.saturating_add(1).min(MAX_RETENTION)
                } else {
                    current.saturating_sub(1)
                };
                dialog.backup.retention = Some(next.to_string());
            }
            Edit::Encryption(mode) => dialog.draft.backup.encryption = mode,
            Edit::Interval(interval) if policy.interval.is_none() => {
                dialog.draft.backup.interval = interval;
            }
            Edit::Directory(value) if policy.directory.is_none() => {
                dialog.backup.directory = Some(if value.len() <= MAX_DIRECTORY_BYTES {
                    Ok(value)
                } else {
                    Err("Enter a backup folder path of at most 4096 UTF-8 bytes.")
                });
            }
            Edit::Browse if policy.directory.is_none() => {
                let folder =
                    parse_directory(&dialog.backup.directory_text(&dialog.draft.backup, None))
                        .ok()
                        .flatten()
                        .or_else(|| std::env::home_dir().filter(|path| path.is_absolute()));
                if let Some(folder) = folder {
                    dialog.problem = None;
                    return dialog.backup.browse(id, folder);
                }
                dialog.problem = Some("Enter an absolute backup folder path.".into());
                return Task::none();
            }
            edit if policy.directory.is_none() => {
                return dialog.backup.update_browser(id, edit);
            }
            _ => return Task::none(),
        }
        dialog.problem = None;
        Task::none()
    }

    pub(super) fn backup_settings_page<'a>(&'a self, dialog: &'a Dialog) -> Element<'a, Desktop> {
        if self.clipboard.policy.problem.is_some() {
            return column![
                text("Backup Settings").size(19),
                text("Backups are unavailable while the managed policy cannot be loaded."),
            ]
            .spacing(12)
            .into();
        }

        let policy = &self.clipboard.policy.backup;
        let editor = &dialog.backup;
        let draft = &dialog.draft.backup;
        let id = dialog.id;
        if policy.directory.is_none()
            && let Some(browser) = &editor.browser
        {
            return browser.view(id);
        }

        let retention = editor.retention_text(draft.retention);
        let retained = parse_retention(&retention).ok();
        let retention: Element<'_, Desktop> = row![
            text("Maximum number of backups").width(225),
            button("−").on_press_maybe(
                (retained != Some(0)).then(|| wrap(id, Edit::StepRetention(false)))
            ),
            dialog.reveal_input(
                Setting::BackupRetention,
                true,
                text_input("0–100", &retention)
                    .width(60)
                    .on_input(move |value| wrap(id, Edit::Retention(value))),
            ),
            button("+").on_press_maybe(
                (retained != Some(MAX_RETENTION)).then(|| wrap(id, Edit::StepRetention(true)))
            ),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into();

        let (protection, credential_available) = self.backup_protection(draft, id);

        let interval = policy.interval.unwrap_or(draft.interval);
        let directory = editor.directory_text(draft, policy.directory.as_deref());
        let interval_control: Element<'_, Desktop> = if policy.interval.is_some() {
            text(interval.to_string()).into()
        } else {
            pick_list(Interval::ALL, Some(interval), move |interval| {
                wrap(id, Edit::Interval(interval))
            })
            .into()
        };
        let schedule = row![
            text("Schedule").width(225),
            interval_control,
            policy
                .interval
                .is_some()
                .then(|| text("Managed by policy").size(13)),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        let folder = row![
            text("Folder").width(225),
            dialog.reveal_input(
                Setting::AutomaticBackup,
                policy.directory.is_none(),
                text_input("Choose an existing folder", &directory).on_input_maybe(
                    policy
                        .directory
                        .is_none()
                        .then_some(move |value| wrap(id, Edit::Directory(value)))
                ),
            ),
            button("Browse…")
                .style(button::secondary)
                .on_press_maybe(policy.directory.is_none().then(|| wrap(id, Edit::Browse))),
            policy
                .directory
                .is_some()
                .then(|| text("Managed by policy").size(13)),
        ]
        .spacing(6)
        .align_y(Alignment::Center);

        let status = if policy.directory.is_none()
            && let Some(Err(error)) = editor.directory
        {
            Cow::Borrowed(error)
        } else {
            self.automatic_backup_status(draft, interval, &directory, credential_available)
        };
        let automatic: Element<'_, Desktop> = column![
            text("Automatic backup").size(18),
            text("kortty writes an encrypted backup on this schedule when the folder and selected encryption are available.").size(14),
            schedule,
            folder,
            text(status).size(14),
        ].spacing(8).into();
        let progress = &self.backup_progress;
        let last_backup = progress
            .last_backup
            .map_or_else(|| "No successful backup yet".to_owned(), local_timestamp);
        let last_directory = progress
            .last_directory
            .as_deref()
            .map(|path| format!("Last backup folder: {}", path.display()));
        column![
            text("Backup Settings").size(19),
            highlighted(dialog, Setting::BackupRetention, retention),
            text("0 = unlimited. Otherwise the oldest backups are automatically deleted.").size(13),
            rule::horizontal(1),
            text("Encryption (Required)").size(18),
            highlighted(dialog, Setting::BackupCredential, protection),
            rule::horizontal(1),
            highlighted(dialog, Setting::AutomaticBackup, automatic),
            rule::horizontal(1),
            text(format!("Last backup: {last_backup}")),
            last_directory.map(|directory| text(directory).size(13)),
        ]
        .spacing(10)
        .max_width(850)
        .into()
    }

    fn backup_protection(
        &self,
        draft: &Preferences,
        id: window::Id,
    ) -> (Element<'static, Desktop>, bool) {
        let (options, selected, label, manager, open, select, help) = match draft.encryption {
            EncryptionMode::Password => {
                let (options, selected) = credential_options(
                    self.credential_choices(Kind::ArchivePassword),
                    draft.credential.as_deref(),
                );
                (
                    options,
                    selected,
                    "Backup password credential",
                    "Open Vault…",
                    Edit::OpenVault,
                    Edit::Credential as fn(Option<String>) -> Edit,
                    "Keep the ZIP password somewhere you can access without this backup.",
                )
            }
            EncryptionMode::OpenPgp => {
                let (options, selected) =
                    recipient_options(self.backup_recipient_choices(), draft.recipient.as_deref());
                (
                    options,
                    selected,
                    "OpenPGP recipient",
                    "Manage Recipients…",
                    Edit::OpenRecipients,
                    Edit::Recipient as fn(Option<String>) -> Edit,
                    "Keep access to the recipient’s private key to restore this backup.",
                )
            }
        };
        let available = selected.id.is_some() && options.contains(&selected);
        let encryption = row![
            text("Backup Encryption").width(225),
            pick_list(EncryptionMode::ALL, Some(draft.encryption), move |mode| {
                wrap(id, Edit::Encryption(mode))
            }),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        let selected = row![
            text(label).width(225),
            pick_list(options, Some(selected), move |choice| wrap(
                id,
                select(choice.id)
            )),
            button(manager)
                .style(button::secondary)
                .on_press(wrap(id, open)),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        (
            column![encryption, selected, text(help).size(13)]
                .spacing(8)
                .into(),
            available,
        )
    }

    fn automatic_backup_status<'a>(
        &'a self,
        draft: &Preferences,
        interval: Interval,
        directory: &str,
        credential_available: bool,
    ) -> Cow<'a, str> {
        if interval == Interval::Off {
            return "Automatic backup is off.".into();
        }
        let directory = match parse_directory(directory) {
            Ok(Some(path)) => path,
            Ok(None) => return "Choose a folder before automatic backups can run.".into(),
            Err(error) => return error.into(),
        };
        let reference = match draft.encryption {
            EncryptionMode::Password if draft.credential.is_none() => {
                return "Select a ZIP archive password from the vault.".into();
            }
            EncryptionMode::OpenPgp if draft.recipient.is_none() => {
                return "Select an enrolled OpenPGP recipient.".into();
            }
            EncryptionMode::Password => draft.credential.as_ref(),
            EncryptionMode::OpenPgp => draft.recipient.as_ref(),
        };
        let policy = &self.clipboard.policy.backup;
        let saved = &self.backup_preferences;
        let saved_interval = policy.interval.unwrap_or(saved.interval);
        let saved_directory = policy.directory.as_ref().or(saved.directory.as_ref());
        if interval != saved_interval
            || Some(&directory) != saved_directory
            || draft.encryption != saved.encryption
            || reference
                != match saved.encryption {
                    EncryptionMode::Password => saved.credential.as_ref(),
                    EncryptionMode::OpenPgp => saved.recipient.as_ref(),
                }
        {
            return "Changes take effect after Save.".into();
        }
        if !credential_available && draft.encryption == EncryptionMode::OpenPgp {
            return "The selected OpenPGP recipient is unavailable or revoked.".into();
        }
        if let Some(status) = &self.backup_status {
            return Cow::Borrowed(status);
        }
        if !credential_available {
            if self
                .vault
                .as_ref()
                .is_some_and(VaultWindow::can_manage_environments)
            {
                return "The selected ZIP archive password is unavailable in the vault.".into();
            }
            return "Waiting for the vault to be unlocked.".into();
        }
        self.backup_progress.last_auto.map_or_else(
            || "No automatic backup yet.".into(),
            |when| format!("Last automatic backup: {}", local_timestamp(when)).into(),
        )
    }
}

impl Browser {
    fn folders(&self) -> &[Entry] {
        self.listing
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map_or(&[], |directory| directory.entries.as_slice())
    }

    fn last_page(&self) -> usize {
        self.folders().len().saturating_sub(1) / PAGE_SIZE
    }

    fn view(&self, id: window::Id) -> Element<'_, Desktop> {
        let mut entries = column![].spacing(2);
        for (index, entry) in self
            .folders()
            .iter()
            .enumerate()
            .skip(self.page * PAGE_SIZE)
            .take(PAGE_SIZE)
        {
            entries = entries.push(
                button(entry.display.as_str())
                    .width(Length::Fill)
                    .style(button::text)
                    .on_press(wrap(id, Edit::BrowseEntry(self.revision, index))),
            );
        }
        let status = match &self.listing {
            None => Some("Loading folders…"),
            Some(Err(error)) => Some(error.as_str()),
            Some(Ok(_)) if self.folders().is_empty() => Some("No subfolders"),
            Some(Ok(_)) => None,
        };
        column![
            text("Select Backup Folder").size(19),
            text(self.folder.to_string_lossy()),
            row![
                button("Parent folder")
                    .on_press_maybe(self.folder.parent().map(|_| wrap(id, Edit::BrowseParent))),
                button("◀")
                    .on_press_maybe((self.page > 0).then(|| wrap(id, Edit::BrowsePage(false)))),
                button("▶").on_press_maybe(
                    (self.page < self.last_page()).then(|| wrap(id, Edit::BrowsePage(true)))
                ),
            ]
            .spacing(6),
            status.map(|status| text(status).size(14)),
            scrollable(entries).height(Length::Fixed(250.0)),
            row![
                button("Back")
                    .style(button::secondary)
                    .on_press(wrap(id, Edit::CloseBrowser)),
                button("Use This Folder").on_press_maybe(
                    matches!(self.listing, Some(Ok(_)))
                        .then(|| wrap(id, Edit::UseFolder(self.revision)))
                ),
            ]
            .spacing(6),
        ]
        .spacing(10)
        .into()
    }
}

fn list_folders(path: &Path) -> io::Result<Directory> {
    let mut directory = list_local(path)?;
    directory
        .entries
        .retain(|entry| entry.kind == EntryKind::Directory);
    Ok(directory)
}

fn parse_retention(value: &str) -> Result<u8, &'static str> {
    value
        .parse::<u8>()
        .ok()
        .filter(|&value| value <= MAX_RETENTION)
        .ok_or("Enter a maximum backup count from 0 to 100.")
}

fn parse_directory(value: &str) -> Result<Option<PathBuf>, &'static str> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    let path = Path::new(value);
    validate_directory(path).map_err(|_| DIRECTORY_ERROR)?;
    Ok(Some(path.to_path_buf()))
}

fn credential_options(
    available: Vec<CredentialChoice>,
    current: Option<&str>,
) -> (Vec<Choice>, Choice) {
    let mut label_counts = HashMap::new();
    for credential in &available {
        *label_counts
            .entry(credential.label.as_str())
            .or_insert(0usize) += 1;
    }
    let mut choices = vec![Choice {
        id: None,
        label: "No stored password".into(),
    }];
    choices.extend(available.iter().map(|credential| {
        let label = if label_counts.get(credential.label.as_str()).copied() == Some(1) {
            credential.label.clone()
        } else {
            format!("{} ({})", credential.label, credential.id)
        };
        Choice {
            id: Some(credential.id.clone()),
            label,
        }
    }));
    choices[1..].sort_by(|first, second| first.label.cmp(&second.label));
    let selected = choices
        .iter()
        .find(|choice| choice.id.as_deref() == current)
        .cloned()
        .unwrap_or_else(|| Choice {
            id: current.map(str::to_owned),
            label: "Saved vault password (locked or unavailable)".into(),
        });
    (choices, selected)
}

fn recipient_options(available: Vec<Recipient>, current: Option<&str>) -> (Vec<Choice>, Choice) {
    let mut choices = vec![Choice {
        id: None,
        label: "No recipient selected".into(),
    }];
    choices.extend(
        available
            .into_iter()
            .filter(|recipient| !recipient.revoked())
            .map(|recipient| Choice {
                id: Some(recipient.fingerprint().into()),
                label: recipient.to_string(),
            }),
    );
    choices[1..].sort_by(|first, second| first.label.cmp(&second.label));
    let selected = choices
        .iter()
        .find(|choice| choice.id.as_deref() == current)
        .cloned()
        .unwrap_or_else(|| Choice {
            id: current.map(str::to_owned),
            label: "Saved OpenPGP recipient (unavailable)".into(),
        });
    (choices, selected)
}

fn local_timestamp(when: jiff::Timestamp) -> String {
    when.to_zoned(jiff::tz::TimeZone::system())
        .strftime("%d.%m.%Y %H:%M:%S")
        .to_string()
}

fn wrap(id: window::Id, edit: Edit) -> Desktop {
    Desktop::Settings(Message::Backup(id, edit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::settings_window::search_matches;

    fn settings() -> (Workspace, window::Id) {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Backup));
        let id = workspace.settings.0.as_ref().unwrap().id;
        (workspace, id)
    }

    fn edit(workspace: &mut Workspace, id: window::Id, edit: Edit) {
        let _ = workspace.update_settings(Message::Backup(id, edit));
    }

    #[test]
    fn backup_settings_are_searchable_and_cancel_discards_drafts() {
        assert_eq!(
            &Section::ALL[4..11],
            &[
                Section::Terminal,
                Section::Messaging,
                Section::Video,
                Section::Backup,
                Section::Logging,
                Section::Export,
                Section::Window,
            ],
        );
        assert_eq!(
            search_matches("backup retention").collect::<Vec<_>>(),
            [Setting::BackupRetention],
        );
        let (mut workspace, id) = settings();
        assert!(workspace.settings_view(id).is_some());
        let defaults = Preferences::default();
        assert_eq!(defaults.retention, 10);
        assert_eq!(defaults.interval, Interval::Weekly);

        edit(&mut workspace, id, Edit::Retention("0".into()));
        edit(&mut workspace, id, Edit::Interval(Interval::Daily));
        edit(
            &mut workspace,
            id,
            Edit::Directory("relative/folder".into()),
        );
        edit(&mut workspace, id, Edit::Credential(Some("a".repeat(32))));
        let _ = workspace.update_settings(Message::FontSize(id, 42));
        let _ = workspace.save_settings_to(id, |_| panic!("invalid backup reached persistence"));
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.section, Section::Backup);
        assert_eq!(dialog.problem.as_deref(), Some(DIRECTORY_ERROR));
        assert_eq!(dialog.draft.backup.credential, None);

        let _ = workspace.update_settings(Message::Cancel(id));
        assert!(workspace.settings.title(id).is_none());
        assert_eq!(workspace.backup_preferences, defaults);
        assert_eq!(
            workspace.window_settings,
            super::super::WindowSettings::default()
        );
    }

    #[test]
    fn backup_settings_managed_fields_are_independently_locked_on_update_and_save() {
        let (mut workspace, id) = settings();
        let local_folder = std::env::temp_dir().join("local-folder");
        let managed_folder = std::env::temp_dir().join("managed-folder");
        workspace.clipboard.policy.backup.interval = Some(Interval::Monthly);
        edit(&mut workspace, id, Edit::Interval(Interval::Off));
        edit(
            &mut workspace,
            id,
            Edit::Directory(local_folder.to_str().unwrap().to_owned()),
        );
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.draft.backup.interval, Interval::Weekly);
        let editable_directory = dialog
            .backup
            .merge(
                &dialog.draft.backup,
                &dialog.initial.backup,
                &workspace.backup_preferences,
                true,
                false,
            )
            .unwrap();
        assert_eq!(editable_directory.directory, Some(local_folder));
        assert_eq!(editable_directory.interval, Interval::Weekly);

        workspace.clipboard.policy.backup.interval = None;
        workspace.clipboard.policy.backup.directory = Some(managed_folder.clone());
        edit(&mut workspace, id, Edit::Directory("stale input".into()));
        edit(&mut workspace, id, Edit::Browse);
        edit(&mut workspace, id, Edit::Interval(Interval::Off));
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert!(!dialog.backup.browsing());
        assert_eq!(
            dialog
                .backup
                .directory_text(&dialog.draft.backup, Some(&managed_folder)),
            managed_folder.to_string_lossy(),
        );
        let editable_interval = dialog
            .backup
            .merge(
                &dialog.draft.backup,
                &dialog.initial.backup,
                &workspace.backup_preferences,
                false,
                true,
            )
            .unwrap();
        assert_eq!(editable_interval.directory, None);
        assert_eq!(editable_interval.interval, Interval::Off);
    }

    #[test]
    fn backup_settings_picker_accepts_only_current_listed_folder() {
        let (mut workspace, id) = settings();
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let directory = std::env::temp_dir().join(format!(
            "kortty-backup-picker-{:016x}",
            u64::from_ne_bytes(nonce),
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::create_dir(directory.join("child")).unwrap();
        std::fs::write(directory.join("regular-file"), b"fixture").unwrap();
        let listing = Arc::new(list_folders(&directory).unwrap());
        std::fs::remove_dir_all(&directory).unwrap();

        edit(
            &mut workspace,
            id,
            Edit::Directory(directory.to_str().unwrap().to_owned()),
        );
        edit(&mut workspace, id, Edit::Browse);
        let revision = workspace.settings.0.as_ref().unwrap().backup.revision;
        edit(
            &mut workspace,
            id,
            Edit::Browsed(revision.wrapping_sub(1), Ok(Arc::clone(&listing))),
        );
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert!(dialog.backup.browser.as_ref().unwrap().listing.is_none());
        edit(&mut workspace, id, Edit::Browsed(revision, Ok(listing)));
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(
            dialog
                .backup
                .browser
                .as_ref()
                .unwrap()
                .folders()
                .iter()
                .map(|entry| entry.display.as_str())
                .collect::<Vec<_>>(),
            ["child"],
        );
        edit(
            &mut workspace,
            id,
            Edit::UseFolder(revision.wrapping_sub(1)),
        );
        assert!(workspace.settings.0.as_ref().unwrap().backup.browsing());
        edit(&mut workspace, id, Edit::UseFolder(revision));
        assert!(!workspace.settings.0.as_ref().unwrap().backup.browsing());
        assert!(workspace.backup_preferences.directory.is_none());
    }

    #[test]
    fn backup_settings_retention_and_oversized_directory_are_validated_before_save() {
        let defaults = Preferences::default();
        let editor = Editor {
            retention: Some("100".into()),
            directory: Some(Err("path too long")),
            ..Editor::default()
        };
        assert_eq!(
            editor.merge(&defaults, &defaults, &defaults, false, false),
            Err("path too long"),
        );
        let managed = editor
            .merge(&defaults, &defaults, &defaults, false, true)
            .unwrap();
        assert_eq!(managed.retention, 100);
        assert!(parse_retention("101").is_err());
        assert!(parse_retention("").is_err());
    }

    #[test]
    fn backup_settings_switches_encryption_and_retains_the_inactive_reference() {
        let initial = Preferences {
            credential: Some("a".repeat(32)),
            recipient: Some("A".repeat(40)),
            ..Preferences::default()
        };
        let mut workspace = Workspace {
            backup_preferences: initial.clone(),
            ..Workspace::default()
        };
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Backup));
        let id = workspace.settings.0.as_ref().unwrap().id;

        edit(
            &mut workspace,
            id,
            Edit::Encryption(EncryptionMode::OpenPgp),
        );
        edit(&mut workspace, id, Edit::Credential(None));
        edit(&mut workspace, id, Edit::Recipient(Some("B".repeat(40))));
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.draft.backup.credential, initial.credential);
        assert_eq!(dialog.draft.backup.recipient, initial.recipient);
        let next = dialog
            .backup
            .merge(
                &dialog.draft.backup,
                &dialog.initial.backup,
                &workspace.backup_preferences,
                false,
                false,
            )
            .unwrap();
        assert_eq!(next.encryption, EncryptionMode::OpenPgp);
        assert!(workspace.settings_view(id).is_some());
        assert_eq!(workspace.backup_preferences, initial);

        edit(&mut workspace, id, Edit::Recipient(None));
        edit(
            &mut workspace,
            id,
            Edit::Encryption(EncryptionMode::Password),
        );
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert!(dialog.draft.backup.recipient.is_none());
        assert_eq!(dialog.draft.backup.credential, initial.credential);
        let _ = workspace.update_settings(Message::Cancel(id));
        assert_eq!(workspace.backup_preferences, initial);
    }

    #[test]
    fn backup_settings_openpgp_manager_keeps_the_settings_draft_and_owner_open() {
        let (mut workspace, settings) = settings();
        let owner = workspace.settings.0.as_ref().unwrap().owner;
        edit(
            &mut workspace,
            settings,
            Edit::Encryption(EncryptionMode::OpenPgp),
        );
        edit(&mut workspace, settings, Edit::OpenRecipients);

        let recipients = workspace.openpgp_for_source(settings).unwrap();
        assert_ne!(recipients, settings);
        assert_eq!(workspace.owner_dialog(owner).unwrap().0, recipients);
        assert!(workspace.snippets.is_empty());
        let _ =
            workspace.save_settings_to(settings, |_| panic!("modal settings reached persistence"));
        let _ = workspace.update_settings(Message::Cancel(settings));
        let _ = workspace.update(Desktop::Window(owner, window::Event::CloseRequested));

        assert!(workspace.windows.contains_key(&owner));
        let draft = &workspace.settings.0.as_ref().unwrap().draft;
        assert_eq!(draft.backup.encryption, EncryptionMode::OpenPgp);
    }
}
