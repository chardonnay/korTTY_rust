//! Native backup dialogs. Only the reviewed import can be queued for startup.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use iced::{Size, Task, window};
use kortty_ssh::{ImportReport, ImportReview};
use kortty_vault::{MAX_ARCHIVE_PASSWORD_BYTES, MAX_MASTER_PASSWORD_BYTES, Secret, Vault};

use crate::profile_backup::preferences::{EncryptionMode, Preferences, Progress};
use crate::profile_backup::{
    self, BackupFormat, Exported, MIN_PASSWORD_BYTES, Preview, RestoreMode, current_name,
};
use crate::sftp_window::{Directory, EntryKind, EntryName, Location, list_local};
use crate::ssh_dialog::Password;
use crate::vault_window::VaultWindow;
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message as Desktop, Workspace, window_lifecycle};

mod host_pins;
#[cfg(test)]
mod tests;
mod view;

const MAX_PATH_BYTES: usize = 4096;
const PAGE_SIZE: usize = 80;
const MAX_VISIBLE_PREVIEW_ENTRIES: usize = 160;

#[derive(Debug, Clone)]
pub enum Message {
    OpenExport,
    OpenImport,
    Focus,
    Path(String),
    ImportFormat(ImportFormat),
    PrivateKeyPath(String),
    Password(Password),
    Repeat(Password),
    UseSavedPassword(bool),
    OpenVault,
    Browse,
    BrowsePrivateKey,
    BrowseParent,
    BrowseEntry(u64, usize),
    BrowsePage(bool),
    UseFolder(u64),
    Browsed(u64, Result<Arc<Directory>, String>),
    Submit,
    RestoreMode(RestoreMode),
    Restore(u64),
    JavaMasterPassword(u64, Password),
    ReviewJavaCredentials(u64, JavaCredentialReview),
    JavaCredentialsReviewed(u64, JavaCredentialsReply),
    ReviewHostPins(u64),
    SelectHostPin(u64, usize, bool),
    ApplyHostPins(u64),
    HostPinsPage(u64, bool),
    BackFromHostPins(u64),
    HostPinsFinished(u64, HostPinReply),
    Finished(u64, Reply),
    Back,
    Cancel,
    KeepEditing,
    Discard,
    Quit,
}

#[derive(Clone, Debug)]
pub enum HostPinReply {
    Reviewed(Result<Arc<ImportReview>, String>),
    Imported(Result<Arc<ImportReport>, String>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaCredentialReview {
    KeysWithoutPassphrases,
    WithVault,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ImportFormat {
    #[default]
    Detect,
    Password,
    OpenPgp,
    JavaPassword,
}

impl ImportFormat {
    const ALL: [Self; 4] = [
        Self::Detect,
        Self::Password,
        Self::OpenPgp,
        Self::JavaPassword,
    ];

    fn resolved(self, detected: Option<BackupFormat>) -> Option<BackupFormat> {
        match self {
            Self::Password => Some(BackupFormat::Password),
            Self::OpenPgp => Some(BackupFormat::OpenPgp),
            Self::JavaPassword => Some(BackupFormat::JavaPassword),
            Self::Detect => detected,
        }
    }
}

impl fmt::Display for ImportFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Detect => formatter.write_str("Detect from file contents"),
            Self::Password => fmt::Display::fmt(&EncryptionMode::Password, formatter),
            Self::OpenPgp => fmt::Display::fmt(&EncryptionMode::OpenPgp, formatter),
            Self::JavaPassword => fmt::Display::fmt(&BackupFormat::JavaPassword, formatter),
        }
    }
}

impl fmt::Display for RestoreMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::KeepExisting => "Keep existing files",
            Self::Replace => "Replace portable profile",
            Self::JavaAdditive => "Add Java profile data",
        })
    }
}

#[derive(Clone)]
pub struct Reply(Arc<Mutex<Option<Result<Outcome, String>>>>);

impl fmt::Debug for Reply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BackupReply(..)")
    }
}

enum Outcome {
    Detected(BackupFormat),
    Exported(Exported),
    Inspected(Preview),
    Queued(Queued),
}

#[derive(Default)]
pub(super) struct Tool(Option<Dialog>);

impl Tool {
    pub(super) fn is_open(&self) -> bool {
        self.0.is_some()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Export,
    Import,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BrowseTarget {
    Archive,
    PrivateKey,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Work {
    Detect,
    Export,
    Inspect,
    JavaCredentials,
    Queue,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Discard {
    Close,
    Quit,
}

struct Dialog {
    id: window::Id,
    owner: window::Id,
    kind: Kind,
    export_mode: EncryptionMode,
    recipient: Option<String>,
    import_format: ImportFormat,
    detected_format: Option<BackupFormat>,
    path: String,
    initial_path: String,
    private_key: String,
    password: Secret,
    password_valid: bool,
    java_master_password: Secret,
    repeat: Secret,
    saved_password: Option<String>,
    use_saved_password: bool,
    restore_mode: RestoreMode,
    phase: Phase,
    host_pins: Option<host_pins::Review>,
    imported_host_pins: Option<Arc<ImportReport>>,
    revision: u64,
    error: Option<String>,
    discard: Option<Discard>,
}

enum Phase {
    Form,
    Browse(Browser),
    Working(Work),
    Review(Preview),
    Exported(Exported),
    Queued(Queued),
}

struct Queued {
    count: usize,
    durable: bool,
    mode: RestoreMode,
    scheduler_notes: Vec<String>,
}

#[derive(Clone)]
pub struct JavaCredentialsReply(Arc<Mutex<Option<JavaCredentialsReviewed>>>);

impl fmt::Debug for JavaCredentialsReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JavaCredentialsReply(..)")
    }
}

struct JavaCredentialsReviewed {
    vault: Option<Vault>,
    review: Option<(Preview, Result<(), &'static str>)>,
}

struct JavaCredentialJob {
    vault: Option<Vault>,
    preview: Preview,
    source_password: Option<Secret>,
    root: PathBuf,
}

impl JavaCredentialJob {
    fn prepare(mut self) -> JavaCredentialsReviewed {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            profile_backup::prepare_java_credentials(
                &mut self.preview,
                self.source_password.as_ref(),
                self.vault.as_ref(),
                &self.root,
            )
        }));
        let used_source_password = self.source_password.is_some();
        JavaCredentialsReviewed {
            vault: self.vault,
            review: result.ok().map(|result| {
                (self.preview, result.map_err(|error| match error.kind() {
                    io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidInput if used_source_password => "Could not unlock the Java SSH credentials. Check the Java master password and try again.",
                    io::ErrorKind::InvalidData => "Could not read the Java SSH credential data.",
                    _ => "Could not prepare the Java SSH credentials for the current profile. Try again.",
                }))
            }),
        }
    }
}

struct Browser {
    target: BrowseTarget,
    folder: PathBuf,
    directory: Option<Arc<Directory>>,
    page: usize,
    loading: bool,
}

impl Dialog {
    fn new(id: window::Id, owner: window::Id, kind: Kind) -> Self {
        let path = if kind == Kind::Export {
            default_destination(None, EncryptionMode::Password)
        } else {
            String::new()
        };
        Self {
            id,
            owner,
            kind,
            export_mode: EncryptionMode::Password,
            recipient: None,
            import_format: ImportFormat::default(),
            detected_format: None,
            initial_path: path.clone(),
            path,
            private_key: String::new(),
            password: Secret::default(),
            password_valid: true,
            java_master_password: Secret::default(),
            repeat: Secret::default(),
            saved_password: None,
            use_saved_password: false,
            restore_mode: RestoreMode::KeepExisting,
            phase: Phase::Form,
            host_pins: None,
            imported_host_pins: None,
            revision: 0,
            error: None,
            discard: None,
        }
    }

    fn configure_export(&mut self, directory: Option<&Path>, preferences: &Preferences) {
        self.export_mode = preferences.encryption;
        self.path = default_destination(directory, self.export_mode);
        self.initial_path.clone_from(&self.path);
        match self.export_mode {
            EncryptionMode::Password => {
                self.saved_password.clone_from(&preferences.credential);
                self.use_saved_password = self.saved_password.is_some();
            }
            EncryptionMode::OpenPgp => self.recipient.clone_from(&preferences.recipient),
        }
    }

    fn encryption(&self) -> EncryptionMode {
        match self.kind {
            Kind::Export => self.export_mode,
            Kind::Import if self.resolved_import() == Some(BackupFormat::OpenPgp) => {
                EncryptionMode::OpenPgp
            }
            Kind::Import => EncryptionMode::Password,
        }
    }

    fn resolved_import(&self) -> Option<BackupFormat> {
        self.import_format.resolved(self.detected_format)
    }

    fn needs_detection(&self) -> bool {
        self.kind == Kind::Import && self.resolved_import().is_none()
    }

    fn edit_import_format(&mut self, format: ImportFormat) {
        let old = self.resolved_import();
        self.import_format = format;
        if self.resolved_import() != old {
            self.clear_import_credentials();
        }
    }

    fn edit_archive_path(&mut self, path: String) {
        if self.kind == Kind::Import && self.path != path {
            self.detected_format = None;
            self.clear_import_credentials();
        }
        self.path = path;
    }

    fn clear_import_credentials(&mut self) {
        self.password = Secret::default();
        self.password_valid = true;
        self.private_key.clear();
    }

    fn selected_password(&self) -> Option<&str> {
        self.saved_password
            .as_deref()
            .filter(|_| self.use_saved_password)
    }

    fn title(&self) -> &'static str {
        match (&self.phase, self.kind) {
            (Phase::Browse(browser), _) if browser.target == BrowseTarget::PrivateKey => {
                "Select OpenPGP Private Key"
            }
            (Phase::Browse(_), Kind::Export) => "Select Backup Destination",
            (Phase::Browse(_), Kind::Import) => "Select Backup File to Import",
            (Phase::Exported(_), _) => "Backup Created",
            (Phase::Queued(_), _) => "Restart required",
            (_, Kind::Export) => "Create Backup",
            (_, Kind::Import) => "Import Backup",
        }
    }

    fn busy(&self) -> bool {
        matches!(self.phase, Phase::Working(_))
            || self.host_pins.as_ref().is_some_and(host_pins::Review::busy)
    }

    fn dirty(&self) -> bool {
        match self.phase {
            Phase::Review(ref preview) => {
                self.imported_host_pins.is_none()
                    || preview.count() != 0
                    || preview.has_java_managed_keys()
                    || preview.has_java_passwords()
            }
            Phase::Form | Phase::Browse(_) => {
                self.path != self.initial_path
                    || self.import_format != ImportFormat::Detect
                    || !self.private_key.is_empty()
                    || !self.password.is_empty()
                    || !self.repeat.is_empty()
            }
            _ => false,
        }
    }

    fn input_path(&self) -> Option<&Path> {
        file_path(&self.path)
    }

    fn can_submit(&self) -> bool {
        if !matches!(self.phase, Phase::Form) || self.input_path().is_none() {
            return false;
        }
        if self.needs_detection() {
            return true;
        }
        match (self.kind, self.encryption()) {
            (Kind::Export, EncryptionMode::OpenPgp) => self.recipient.is_some(),
            (Kind::Import, EncryptionMode::OpenPgp) => {
                self.password_valid && file_path(&self.private_key).is_some()
            }
            (kind, EncryptionMode::Password) => {
                let minimum = if kind == Kind::Import
                    && self.resolved_import() == Some(BackupFormat::JavaPassword)
                {
                    1
                } else {
                    MIN_PASSWORD_BYTES
                };
                self.selected_password().is_some()
                    || (self.password_valid
                        && (minimum..=MAX_ARCHIVE_PASSWORD_BYTES).contains(&self.password.len())
                        && (kind == Kind::Import || self.password == self.repeat))
            }
        }
    }

    fn edit_password(&mut self, password: Password, repeat: bool) {
        let valid = password.as_str().len() <= MAX_ARCHIVE_PASSWORD_BYTES;
        let value = if valid {
            Secret::new(password.into_string())
        } else {
            let field = if self.encryption() == EncryptionMode::OpenPgp {
                "The private key passphrase"
            } else {
                "The backup password"
            };
            self.error = Some(format!(
                "{field} exceeds {MAX_ARCHIVE_PASSWORD_BYTES} UTF-8 bytes."
            ));
            Secret::default()
        };
        if repeat {
            self.repeat = value;
        } else {
            self.password = value;
            self.password_valid = valid;
        }
    }

    fn submit(&mut self, saved: Option<Result<Secret, &'static str>>) -> Task<Desktop> {
        if !self.can_submit() {
            return Task::none();
        }
        let Some(path) = self.input_path().map(Path::to_path_buf) else {
            return Task::none();
        };
        if self.needs_detection() {
            return self.start(Work::Detect, move || {
                profile_backup::detect_format(&path).map(Outcome::Detected)
            });
        }
        if self.encryption() == EncryptionMode::OpenPgp {
            return match self.kind {
                Kind::Export => {
                    let Some(recipient) = self.recipient.clone() else {
                        return Task::none();
                    };
                    self.start(Work::Export, move || {
                        profile_backup::export_openpgp(&path, &recipient).map(Outcome::Exported)
                    })
                }
                Kind::Import => {
                    let Some(private_key) = file_path(&self.private_key).map(Path::to_path_buf)
                    else {
                        return Task::none();
                    };
                    let passphrase = std::mem::take(&mut self.password);
                    self.start(Work::Inspect, move || {
                        profile_backup::inspect_openpgp(&path, &private_key, &passphrase)
                            .map(Outcome::Inspected)
                    })
                }
            };
        }
        let credential = self.selected_password().map(str::to_owned);
        let password = match (credential.as_ref(), saved) {
            (Some(_), Some(Ok(password))) => password,
            (Some(_), Some(Err(error))) => {
                self.error = Some(error.into());
                return Task::none();
            }
            (Some(_), None) => return Task::none(),
            (None, _) => std::mem::take(&mut self.password),
        };
        self.repeat = Secret::default();
        let java_password = self.resolved_import() == Some(BackupFormat::JavaPassword);
        match self.kind {
            Kind::Export => self.start(Work::Export, move || {
                profile_backup::export_profile(&path, &password, credential.as_deref())
                    .map(Outcome::Exported)
            }),
            Kind::Import => self.start(Work::Inspect, move || {
                if java_password {
                    profile_backup::inspect_java_password(&path, &password).map(Outcome::Inspected)
                } else {
                    profile_backup::inspect_backup(&path, &password).map(Outcome::Inspected)
                }
            }),
        }
    }

    fn restore(&mut self, revision: u64) -> Task<Desktop> {
        if revision != self.revision
            || !matches!(&self.phase, Phase::Review(preview) if preview.count() > 0)
        {
            return Task::none();
        }
        let Phase::Review(preview) = std::mem::replace(&mut self.phase, Phase::Form) else {
            return Task::none();
        };
        self.java_master_password = Secret::default();
        let count = preview.count();
        let mode = self.restore_mode;
        self.start(Work::Queue, move || {
            profile_backup::queue_restore(preview, mode).map(|result| {
                Outcome::Queued(Queued {
                    count,
                    durable: result.durable,
                    mode,
                    scheduler_notes: result.scheduler_notes,
                })
            })
        })
    }

    fn offers_java_credential_review(&self, mode: JavaCredentialReview) -> bool {
        let Phase::Review(preview) = &self.phase else {
            return false;
        };
        if preview.java_password_summary().is_some() || preview.java_key_summary().is_some() {
            return false;
        }
        match mode {
            JavaCredentialReview::KeysWithoutPassphrases => preview.has_java_managed_keys(),
            JavaCredentialReview::WithVault => {
                preview.has_java_passwords() || preview.has_java_managed_keys()
            }
        }
    }

    fn can_review_java_credentials(&self, revision: u64, mode: JavaCredentialReview) -> bool {
        revision == self.revision
            && self.offers_java_credential_review(mode)
            && (mode == JavaCredentialReview::KeysWithoutPassphrases
                || !self.java_master_password.is_empty())
    }

    fn edit_java_master_password(&mut self, revision: u64, password: Password) {
        if revision != self.revision
            || !self.offers_java_credential_review(JavaCredentialReview::WithVault)
        {
            return;
        }
        self.error = None;
        if password.as_str().len() > MAX_MASTER_PASSWORD_BYTES {
            self.java_master_password = Secret::default();
            self.error = Some(format!(
                "The Java master password exceeds {MAX_MASTER_PASSWORD_BYTES} UTF-8 bytes."
            ));
        } else {
            self.java_master_password = Secret::new(password.into_string());
        }
    }

    fn begin_java_credential_review(
        &mut self,
        vault: Option<Vault>,
        root: PathBuf,
    ) -> JavaCredentialJob {
        let Phase::Review(preview) =
            std::mem::replace(&mut self.phase, Phase::Working(Work::JavaCredentials))
        else {
            unreachable!("Java credential review requires a backup preview");
        };
        self.revision = self.revision.wrapping_add(1);
        self.error = None;
        let password = std::mem::take(&mut self.java_master_password);
        let source_password = vault.is_some().then_some(password);
        JavaCredentialJob {
            vault,
            preview,
            source_password,
            root,
        }
    }

    fn finish_java_credential_review(
        &mut self,
        revision: u64,
        review: Option<(Preview, Result<(), &'static str>)>,
    ) {
        if self.revision != revision || !matches!(self.phase, Phase::Working(Work::JavaCredentials))
        {
            return;
        }
        if let Some((preview, result)) = review {
            self.error = result.err().map(str::to_owned);
            self.phase = Phase::Review(preview);
        } else {
            self.error =
                Some("The SSH credential review stopped. Open the backup again to retry.".into());
            self.phase = Phase::Form;
        }
    }

    fn start(
        &mut self,
        action: Work,
        run: impl FnOnce() -> io::Result<Outcome> + Send + 'static,
    ) -> Task<Desktop> {
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        let id = self.id;
        self.phase = Phase::Working(action);
        self.error = None;
        Task::perform(
            async move {
                let (send, receive) = async_channel::bounded(1);
                let started = std::thread::Builder::new()
                    .name("kortty-profile-backup".into())
                    .spawn(move || {
                        let _ = send.send_blocking(run().map_err(|error| error.to_string()));
                    });
                if let Err(error) = started {
                    return Err(error.to_string());
                }
                receive
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The backup worker stopped.".into()))
            },
            move |result| {
                wrap(
                    id,
                    Message::Finished(revision, Reply(Arc::new(Mutex::new(Some(result))))),
                )
            },
        )
    }

    fn finish(&mut self, revision: u64, reply: Reply) -> Option<Progress> {
        if revision != self.revision {
            return None;
        }
        let Phase::Working(work) = self.phase else {
            return None;
        };
        if work == Work::JavaCredentials {
            return None;
        }
        let result = reply
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()?;
        self.phase = match (work, result) {
            (Work::Detect, Ok(Outcome::Detected(format))) => {
                self.detected_format = Some(format);
                Phase::Form
            }
            (Work::Export, Ok(Outcome::Exported(archive))) => Phase::Exported(archive),
            (Work::Inspect, Ok(Outcome::Inspected(preview))) => {
                self.restore_mode = if preview.java_review().is_some() {
                    RestoreMode::JavaAdditive
                } else {
                    RestoreMode::KeepExisting
                };
                Phase::Review(preview)
            }
            (Work::Queue, Ok(Outcome::Queued(queued))) => Phase::Queued(queued),
            (work, Err(error)) => {
                let action = match work {
                    Work::Detect => "Could not identify the backup format",
                    Work::Export => "Could not create backup",
                    Work::Inspect if self.encryption() == EncryptionMode::OpenPgp => {
                        "Could not decrypt and review OpenPGP backup"
                    }
                    Work::Inspect => "Could not authenticate and review backup",
                    Work::JavaCredentials => {
                        unreachable!("Java credentials have a separate worker reply")
                    }
                    Work::Queue => "Could not prepare restore",
                };
                self.error = Some(format!("{action} ({error})."));
                Phase::Form
            }
            _ => {
                self.error = Some("The backup worker returned an unexpected result.".into());
                Phase::Form
            }
        };
        match &self.phase {
            Phase::Exported(archive) => Some(archive.progress.clone()),
            _ => None,
        }
    }

    fn browse_start(
        &mut self,
        target: BrowseTarget,
        last_directory: Option<&Path>,
    ) -> Task<Desktop> {
        let path = match target {
            BrowseTarget::Archive => self.input_path(),
            BrowseTarget::PrivateKey => file_path(&self.private_key),
        };
        let folder = path
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| last_directory.map(Path::to_path_buf))
            .or_else(home);
        folder.map_or_else(Task::none, |folder| self.browse(folder, target))
    }

    fn browse(&mut self, folder: PathBuf, target: BrowseTarget) -> Task<Desktop> {
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        let id = self.id;
        self.error = None;
        self.phase = Phase::Browse(Browser {
            target,
            folder: folder.clone(),
            directory: None,
            page: 0,
            loading: true,
        });
        Task::perform(
            async move {
                list_local(&folder)
                    .map(Arc::new)
                    .map_err(|error| error.to_string())
            },
            move |result| wrap(id, Message::Browsed(revision, result)),
        )
    }

    fn choose_path(&mut self, path: PathBuf, target: BrowseTarget) {
        let Some(path) = path.to_str().filter(|path| path.len() <= MAX_PATH_BYTES) else {
            self.error = Some("Choose a path that can be represented as UTF-8.".into());
            return;
        };
        match target {
            BrowseTarget::Archive => self.edit_archive_path(path.to_owned()),
            BrowseTarget::PrivateKey => self.private_key = path.to_owned(),
        }
        self.phase = Phase::Form;
        self.error = None;
    }

    fn browse_message(&mut self, message: Message) -> Task<Desktop> {
        let Phase::Browse(browser) = &mut self.phase else {
            return Task::none();
        };
        let target = browser.target;
        let export_mode = self.export_mode;
        match message {
            Message::BrowseParent if !browser.loading => {
                let parent = browser.folder.parent().map(Path::to_path_buf);
                return parent.map_or_else(Task::none, |folder| self.browse(folder, target));
            }
            Message::BrowseEntry(revision, index) if revision == self.revision => {
                let Some(directory) = &browser.directory else {
                    return Task::none();
                };
                let Location::Local(folder) = &directory.path else {
                    return Task::none();
                };
                let Some(entry) = directory.entries.get(index) else {
                    return Task::none();
                };
                let EntryName::Local(name) = &entry.name else {
                    return Task::none();
                };
                let path = folder.join(name);
                if entry.kind == EntryKind::Directory {
                    return self.browse(path, target);
                }
                if self.kind == Kind::Import && entry.kind == EntryKind::File {
                    self.choose_path(path, target);
                    if target == BrowseTarget::Archive && self.needs_detection() {
                        return self.submit(None);
                    }
                }
            }
            Message::UseFolder(revision)
                if revision == self.revision
                    && self.kind == Kind::Export
                    && browser.directory.is_some() =>
            {
                let filename = Path::new(&self.path)
                    .file_name()
                    .unwrap_or_else(|| current_name(export_mode).as_ref());
                let destination = browser.folder.join(filename);
                self.choose_path(destination, target);
            }
            Message::BrowsePage(forward) => {
                let max_page = browser.directory.as_ref().map_or(0, |directory| {
                    directory.entries.len().saturating_sub(1) / PAGE_SIZE
                });
                browser.page = if forward {
                    (browser.page + 1).min(max_page)
                } else {
                    browser.page.saturating_sub(1)
                };
            }
            Message::Browsed(revision, result) if revision == self.revision => {
                browser.loading = false;
                match result {
                    Ok(directory) => browser.directory = Some(directory),
                    Err(error) => self.error = Some(format!("Could not open folder ({error}).")),
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn back(&mut self) {
        if matches!(self.phase, Phase::Browse(_) | Phase::Review(_)) {
            self.revision = self.revision.wrapping_add(1);
            self.phase = Phase::Form;
            self.error = None;
            self.java_master_password = Secret::default();
            self.imported_host_pins = None;
        }
    }
}

impl Workspace {
    fn review_java_credentials(
        &mut self,
        id: window::Id,
        revision: u64,
        mode: JavaCredentialReview,
    ) -> Task<Desktop> {
        let Some(dialog) = self.backup.0.as_mut().filter(|dialog| {
            dialog.id == id
                && dialog.discard.is_none()
                && dialog.host_pins.is_none()
                && dialog.can_review_java_credentials(revision, mode)
        }) else {
            return Task::none();
        };
        let Ok(root) = crate::profile::root() else {
            dialog.error =
                Some("Could not open the current profile for SSH credential review.".into());
            return Task::none();
        };
        let vault = match mode {
            JavaCredentialReview::KeysWithoutPassphrases => None,
            JavaCredentialReview::WithVault => {
                let Some(vault) = self.vault.as_mut().and_then(VaultWindow::take_for_ssh_keys)
                else {
                    dialog.java_master_password = Secret::default();
                    dialog.error = Some("Unlock the current credential vault and finish any edits before reviewing SSH credentials.".into());
                    return Task::none();
                };
                self.vault_change_revision = Arc::new(());
                Some(vault)
            }
        };
        let job = Arc::new(Mutex::new(Some(
            dialog.begin_java_credential_review(vault, root),
        )));
        let revision = dialog.revision;
        let (send, receive) = async_channel::bounded(1);
        let worker_job = Arc::clone(&job);
        let started = std::thread::Builder::new()
            .name("kortty-java-backup-credentials".into())
            .spawn(move || {
                let job = worker_job
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                    .expect("Java backup worker owns its job");
                let _ = send.send_blocking(job.prepare());
            });
        if started.is_err() {
            let job = job
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
                .expect("the unstarted Java backup job is available");
            self.finish_java_credentials(
                id,
                revision,
                JavaCredentialsReviewed {
                    vault: job.vault,
                    review: Some((
                        job.preview,
                        Err("Could not start SSH credential review. Try again."),
                    )),
                },
            );
            return Task::none();
        }
        Task::future(async move {
            match receive.recv().await {
                Ok(outcome) => wrap(
                    id,
                    Message::JavaCredentialsReviewed(
                        revision,
                        JavaCredentialsReply(Arc::new(Mutex::new(Some(outcome)))),
                    ),
                ),
                Err(_) => Desktop::Noop,
            }
        })
    }

    fn finish_java_credentials(
        &mut self,
        id: window::Id,
        revision: u64,
        outcome: JavaCredentialsReviewed,
    ) {
        if let (Some(window), Some(vault)) = (&mut self.vault, outcome.vault) {
            window.restore_after_ssh_keys(vault);
            self.vault_change_revision = Arc::new(());
        }
        if let Some(dialog) = self.backup.0.as_mut().filter(|dialog| dialog.id == id) {
            dialog.finish_java_credential_review(revision, outcome.review);
        }
    }

    fn backup_recipient_label(&self, fingerprint: &str) -> Option<String> {
        self.backup_recipient_choices()
            .into_iter()
            .find(|choice| !choice.revoked() && choice.fingerprint() == fingerprint)
            .map(|choice| choice.to_string())
    }

    fn show_backup(&mut self, owner: window::Id, kind: Kind) -> Task<Desktop> {
        if self.clipboard.policy.problem.is_some() {
            return Task::none();
        }
        if let Some(dialog) = &self.backup.0 {
            return window::gain_focus(dialog.id);
        }
        let Some(window) = self.windows.get_mut(&owner) else {
            return Task::none();
        };
        window.cancel_paste();
        let dialog_kind = match kind {
            Kind::Export => DialogKind::Backup,
            Kind::Import => DialogKind::RestoreBackup,
        };
        let (id, opened) = self.open_placed_dialog(
            dialog_kind,
            window::Settings {
                size: Size::new(740.0, 510.0),
                min_size: Some(Size::new(480.0, 375.0)),
                position: iced::window::Position::Centered,
                exit_on_close_request: false,
                ..iced::window::Settings::default()
            },
        );
        let mut dialog = Dialog::new(id, owner, kind);
        if kind == Kind::Export {
            dialog.configure_export(
                self.backup_progress.last_directory.as_deref(),
                &self.backup_preferences,
            );
        }
        self.backup.0 = Some(dialog);
        opened.map(|_| Desktop::Noop)
    }

    pub(super) fn backup_title(&self, id: window::Id) -> Option<&'static str> {
        self.backup
            .0
            .as_ref()
            .filter(|dialog| dialog.id == id)
            .map(Dialog::title)
    }

    pub(super) fn backup_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        self.backup
            .0
            .as_ref()
            .filter(|dialog| dialog.owner == owner)
            .map(|dialog| dialog.id)
    }

    pub(super) fn close_backup(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        let dialog = self.backup.0.as_mut().filter(|dialog| dialog.id == id)?;
        if dialog.busy() {
            return Some(Task::none());
        }
        if dialog.dirty() {
            dialog.discard.get_or_insert(Discard::Close);
            return Some(Task::none());
        }
        Some(self.finish_backup_close(true, false))
    }

    pub(super) fn close_backup_owner(&mut self, owner: window::Id) -> Task<Desktop> {
        if self
            .backup
            .0
            .as_ref()
            .is_some_and(|dialog| dialog.owner == owner && !dialog.busy())
        {
            self.finish_backup_close(false, false)
        } else {
            Task::none()
        }
    }

    pub(super) fn request_backup_quit(&mut self) -> Option<Task<Desktop>> {
        let dialog = self.backup.0.as_mut()?;
        if dialog.busy() {
            window_lifecycle::reply_to_native_quit(false);
        } else if dialog.dirty() {
            dialog.discard = Some(Discard::Quit);
        } else {
            return None;
        }
        Some(window::gain_focus(dialog.id))
    }

    fn finish_backup_close(&mut self, refocus: bool, quit: bool) -> Task<Desktop> {
        let Some(dialog) = self.backup.0.take() else {
            return Task::none();
        };
        let after = if quit
            || dialog.discard == Some(Discard::Quit)
            || self.should_exit_after_window_close()
        {
            self.quit()
        } else if refocus && self.windows.contains_key(&dialog.owner) {
            window::gain_focus(dialog.owner)
        } else {
            Task::none()
        };
        Task::batch([self.close_placed_dialog(dialog.id), after])
    }

    pub(super) fn update_backup(&mut self, id: window::Id, message: Message) -> Task<Desktop> {
        match message {
            Message::OpenExport => return self.show_backup(id, Kind::Export),
            Message::OpenImport => return self.show_backup(id, Kind::Import),
            Message::HostPinsFinished(revision, reply) => {
                let imported = self
                    .backup
                    .0
                    .as_mut()
                    .filter(|dialog| dialog.id == id)
                    .is_some_and(|dialog| dialog.finish_host_pins(revision, reply));
                return if imported {
                    self.refresh_known_hosts()
                } else {
                    Task::none()
                };
            }
            Message::ReviewHostPins(revision) if self.clipboard.policy.problem.is_none() => {
                return self
                    .backup
                    .0
                    .as_mut()
                    .filter(|dialog| dialog.id == id)
                    .map_or_else(Task::none, |dialog| dialog.review_host_pins(revision));
            }
            Message::ReviewJavaCredentials(revision, mode) => {
                return self.review_java_credentials(id, revision, mode);
            }
            Message::JavaCredentialsReviewed(revision, reply) => {
                if let Some(outcome) = reply
                    .0
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                {
                    self.finish_java_credentials(id, revision, outcome);
                }
                return Task::none();
            }
            _ => {}
        }
        let saved = matches!(message, Message::Submit)
            .then(|| {
                self.backup
                    .0
                    .as_ref()
                    .filter(|dialog| dialog.id == id)
                    .and_then(Dialog::selected_password)
                    .map(|id| self.backup_archive_password(id))
            })
            .flatten();
        let unavailable_recipient = matches!(message, Message::Submit)
            && self.backup.0.as_ref().is_some_and(|dialog| {
                dialog.id == id
                    && dialog.recipient.as_ref().is_some_and(|fingerprint| {
                        self.backup_recipient_label(fingerprint).is_none()
                    })
            });
        let Some(dialog) = self.backup.0.as_mut().filter(|dialog| dialog.id == id) else {
            return Task::none();
        };
        if matches!(message, Message::Focus) {
            return window::gain_focus(id);
        }
        if let Message::Finished(revision, reply) = message {
            if let Some(progress) = dialog.finish(revision, reply) {
                self.apply_backup_progress(progress);
            }
            return Task::none();
        }
        if dialog.busy() {
            return Task::none();
        }
        if dialog.discard.is_some() {
            return match message {
                Message::Discard => self.finish_backup_close(true, false),
                Message::KeepEditing | Message::Cancel => {
                    if dialog.discard.take() == Some(Discard::Quit) {
                        window_lifecycle::reply_to_native_quit(false);
                    }
                    Task::none()
                }
                _ => Task::none(),
            };
        }
        if dialog.host_pins.is_some() {
            return dialog.update_host_pins(message);
        }
        match message {
            Message::Cancel => return self.finish_backup_close(true, false),
            Message::OpenVault
                if dialog.selected_password().is_some()
                    || dialog.offers_java_credential_review(JavaCredentialReview::WithVault) =>
            {
                let owner = dialog.owner;
                return self.show_vault(owner);
            }
            Message::JavaMasterPassword(revision, password) => {
                dialog.edit_java_master_password(revision, password)
            }
            Message::Back => dialog.back(),
            Message::Quit if matches!(dialog.phase, Phase::Queued(_)) => {
                return self.finish_backup_close(false, true);
            }
            Message::RestoreMode(mode)
                if mode != RestoreMode::JavaAdditive
                    && matches!(&dialog.phase, Phase::Review(preview) if preview.java_review().is_none()) =>
            {
                if dialog.restore_mode != mode {
                    dialog.restore_mode = mode;
                    dialog.revision = dialog.revision.wrapping_add(1);
                }
            }
            Message::Restore(revision) => return dialog.restore(revision),
            message if matches!(dialog.phase, Phase::Browse(_)) => {
                return dialog.browse_message(message);
            }
            message if matches!(dialog.phase, Phase::Form) => {
                dialog.error = None;
                match message {
                    Message::Path(path) => {
                        let path = bounded_path(path, "backup", &mut dialog.error);
                        dialog.edit_archive_path(path);
                    }
                    Message::ImportFormat(format) if dialog.kind == Kind::Import => {
                        dialog.edit_import_format(format);
                    }
                    Message::PrivateKeyPath(path)
                        if dialog.kind == Kind::Import
                            && dialog.encryption() == EncryptionMode::OpenPgp =>
                    {
                        dialog.private_key = bounded_path(path, "private key", &mut dialog.error);
                    }
                    Message::Password(password)
                        if !dialog.needs_detection()
                            && (dialog.kind == Kind::Import
                                || dialog.export_mode == EncryptionMode::Password) =>
                    {
                        dialog.edit_password(password, false);
                    }
                    Message::Repeat(password)
                        if dialog.kind == Kind::Export
                            && dialog.export_mode == EncryptionMode::Password =>
                    {
                        dialog.edit_password(password, true);
                    }
                    Message::UseSavedPassword(use_saved) if dialog.saved_password.is_some() => {
                        dialog.use_saved_password = use_saved;
                        if use_saved {
                            dialog.password = Secret::default();
                            dialog.repeat = Secret::default();
                            dialog.password_valid = true;
                        }
                    }
                    Message::Browse => {
                        return dialog.browse_start(
                            BrowseTarget::Archive,
                            self.backup_progress.last_directory.as_deref(),
                        );
                    }
                    Message::BrowsePrivateKey
                        if dialog.kind == Kind::Import
                            && dialog.encryption() == EncryptionMode::OpenPgp =>
                    {
                        return dialog.browse_start(BrowseTarget::PrivateKey, None);
                    }
                    Message::Submit => {
                        if unavailable_recipient {
                            dialog.error = Some(
                                "The configured OpenPGP recipient is unavailable or revoked."
                                    .into(),
                            );
                        } else {
                            return dialog.submit(saved);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Task::none()
    }
}

fn default_destination(directory: Option<&Path>, encryption: EncryptionMode) -> String {
    let filename = current_name(encryption);
    directory
        .map(|directory| directory.join(filename))
        .or_else(|| home().map(|home| home.join(filename)))
        .and_then(|path| path.into_os_string().into_string().ok())
        .unwrap_or_default()
}

fn file_path(text: &str) -> Option<&Path> {
    let path = Path::new(text);
    (path.is_absolute() && path.file_name().is_some() && !text.chars().any(char::is_control))
        .then_some(path)
}

fn bounded_path(path: String, field: &str, error: &mut Option<String>) -> String {
    if path.len() <= MAX_PATH_BYTES {
        path
    } else {
        *error = Some(format!(
            "The {field} path exceeds {MAX_PATH_BYTES} UTF-8 bytes."
        ));
        String::new()
    }
}

fn home() -> Option<PathBuf> {
    std::env::home_dir().filter(|path| path.is_absolute())
}

fn wrap(id: window::Id, message: Message) -> Desktop {
    Desktop::Backup(id, message)
}
