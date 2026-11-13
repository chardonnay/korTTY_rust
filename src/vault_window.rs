use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, space, text, text_input,
};

use iced::{Alignment, Element, Length, Task};
use kortty_jobs::WebhookOutcome;
use kortty_ssh::{Endpoint, SensitiveLine};
use kortty_vault::{
    Credential, CredentialInput, CredentialKind, Error as VaultError, MAX_MASTER_PASSWORD_BYTES,
    MAX_QDRANT_API_KEY_BYTES, MAX_SUDO_GROUP_BYTES, MAX_SUDO_SCOPE_BYTES,
    MIN_MASTER_PASSWORD_BYTES, Secret, Vault, WebhookFormat, WebhookSettings, sudo_scope,
    valid_archive_password, valid_huggingface_token, valid_qdrant_api_key, valid_sudo_group,
    valid_sudo_password,
};

use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_window::CredentialChoice;
use crate::scheduler_window::webhooks;
use crate::ssh_dialog::LoginTarget;
use crate::terminal_logging::{RedactionSecrets, Secrets};
use crate::workspace::environment_access::{self, Catalog, DEFAULT_ENVIRONMENT};
use zeroize::Zeroizing;

mod password_command;
mod startup;
pub(crate) use password_command::CommandAuthority;
pub(crate) use startup::{StartupPassword, StartupResult};

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
    PasswordCommandInspected(PasswordCommandInspection),
    TestWebhook(WebhookTestSource),
    WebhookTestFinished(Arc<AtomicBool>, WebhookOutcome),
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
pub enum WebhookTestSource {
    Saved(String),
    Draft(Arc<()>),
}

#[derive(Clone, Debug)]
pub enum Field {
    Label(String),
    Username(String),
    HostPattern(String),
    Environment(String),
    Description(String),
    Kind(Kind),
    WebhookEnabled(bool),
    WebhookFormat(WebhookFormat),
    WebhookSummary(bool),
    Secret(SecretEdit),
    PasswordProgram(SecretEdit),
    PasswordArguments(SecretEdit),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Password,
    KeyPassphrase,
    ArchivePassword,
    HuggingFaceToken,
    QdrantApiKey,
    SudoPassword,
    SudoGroupPassword,
    JobWebhookUrl,
    PasswordCommand,
}

impl Kind {
    fn native(self) -> CredentialKind {
        match self {
            Self::Password => CredentialKind::Password,
            Self::KeyPassphrase => CredentialKind::KeyPassphrase,
            Self::ArchivePassword => CredentialKind::ArchivePassword,
            Self::HuggingFaceToken => CredentialKind::HuggingFaceToken,
            Self::QdrantApiKey => CredentialKind::QdrantApiKey,
            Self::SudoPassword => CredentialKind::SudoPassword,
            Self::SudoGroupPassword => CredentialKind::SudoGroupPassword,
            Self::JobWebhookUrl => CredentialKind::JobWebhookUrl,
            Self::PasswordCommand => CredentialKind::SshPasswordCommand,
        }
    }

    fn from_native(kind: CredentialKind) -> Self {
        match kind {
            CredentialKind::Password => Self::Password,
            CredentialKind::KeyPassphrase => Self::KeyPassphrase,
            CredentialKind::ArchivePassword => Self::ArchivePassword,
            CredentialKind::HuggingFaceToken => Self::HuggingFaceToken,
            CredentialKind::QdrantApiKey => Self::QdrantApiKey,
            CredentialKind::SudoPassword => Self::SudoPassword,
            CredentialKind::SudoGroupPassword => Self::SudoGroupPassword,
            CredentialKind::JobWebhookUrl => Self::JobWebhookUrl,
            CredentialKind::SshPasswordCommand => Self::PasswordCommand,
        }
    }

    fn has_login_scope(self) -> bool {
        self.native().is_ssh()
            || matches!(
                self,
                Self::SudoPassword | Self::SudoGroupPassword | Self::PasswordCommand
            )
    }

    fn has_environment(self) -> bool {
        self.native().is_ssh() || self == Self::ArchivePassword
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Password => "SSH password",
            Self::KeyPassphrase => "SSH key passphrase",
            Self::ArchivePassword => "ZIP archive password",
            Self::HuggingFaceToken => "Hugging Face access token",
            Self::QdrantApiKey => "Qdrant API key",
            Self::SudoPassword => "Scheduler sudo password",
            Self::SudoGroupPassword => "Scheduler group sudo password",
            Self::JobWebhookUrl => "Scheduler webhook URL",
            Self::PasswordCommand => "SSH password command",
        })
    }
}

#[derive(Clone)]
pub struct Completion(Arc<Mutex<Option<Finished>>>);

impl Completion {
    pub(crate) fn recovery_key_changed(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .is_some_and(|finished| finished.recovery_key_changed)
    }

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
            recovery_key_changed: false,
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
    recovery_key_changed: bool,
}

pub struct VaultWindow {
    path: PathBuf,
    exists: bool,
    vault: Option<Vault>,
    sudo_revision: Arc<()>,
    primary: Secret,
    confirm: Secret,
    next_master: Secret,
    draft: Option<EntryDraft>,
    operation: Option<Operation>,
    sudo_groups: Vec<String>,
    backup_path: String,
    selected: Option<String>,
    status: Option<String>,
    webhook_test: Option<WebhookTest>,
    webhook_test_status: Option<&'static str>,
    busy: bool,
    confirming_close: bool,
    unlock_prompt: Option<UnlockPrompt>,
}

#[derive(Clone, Default)]
pub(crate) struct JournalQuestionSecrets(Arc<[Zeroizing<String>]>);

impl JournalQuestionSecrets {
    pub(crate) fn has_multiline_values(&self) -> bool {
        self.0.iter().any(|secret| secret.contains(['\r', '\n']))
    }

    pub(crate) fn add_matches(&self, input: &str, target: &Secrets) {
        if self.0.is_empty() {
            return;
        }
        let normalized = input
            .chars()
            .any(|character| !crate::session_journal::is_display_character(character))
            .then(|| {
                let mut value = Zeroizing::new(input.replace("\r\n", "\n").replace('\r', "\n"));
                value.retain(crate::session_journal::is_display_character);
                value
            });
        for secret in self.0.iter() {
            if input.contains(secret.as_str())
                || normalized
                    .as_ref()
                    .is_some_and(|value| value.contains(secret.as_str()))
            {
                target.add(secret);
            }
        }
    }
}

struct WebhookTest {
    gate: Arc<AtomicBool>,
    result: async_channel::Receiver<WebhookOutcome>,
}

impl Drop for WebhookTest {
    fn drop(&mut self) {
        self.gate.store(false, Ordering::Release);
    }
}

#[derive(Clone)]
pub(crate) struct AgentSudoCredential {
    id: String,
    pub(crate) label: String,
    revision: Weak<()>,
}

impl AgentSudoCredential {
    pub(crate) fn active(&self) -> bool {
        self.revision.strong_count() != 0
    }
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
    webhook: WebhookSettings,
    webhook_revision: Arc<()>,
    secret: Secret,
    password_program: Secret,
    password_arguments: Secret,
    password_inspection: Option<Arc<()>>,
    changed: bool,
}

#[derive(Clone)]
pub struct PasswordCommandInspection(Arc<()>, Arc<Mutex<Option<Result<Secret, String>>>>);

impl fmt::Debug for PasswordCommandInspection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Password command inspection")
    }
}

impl VaultWindow {
    #[cfg(test)]
    pub(crate) fn with_unlocked_fixture(path: PathBuf, vault: Vault) -> Self {
        let mut window = Self::open(path).expect("fixture vault exists");
        window.vault = Some(vault);
        window
    }

    #[cfg(test)]
    pub(crate) fn qdrant_fixture(root: &Path, secret: &str) -> (Self, String) {
        let path = root.join("qdrant-fixture-vault.bin");
        let mut vault = Vault::create(&path, b"qdrant-fixture-master").unwrap();
        let id = vault
            .save(
                CredentialInput {
                    label: "Fixture Qdrant API key".into(),
                    kind: CredentialKind::QdrantApiKey,
                    ..CredentialInput::default()
                },
                Secret::new(secret.to_owned()),
            )
            .unwrap();
        (Self::with_unlocked_fixture(path, vault), id)
    }

    pub fn open(path: PathBuf) -> Result<Self, String> {
        let exists = Vault::exists(&path).map_err(|error| error.to_string())?;
        Ok(Self {
            path,
            exists,
            vault: None,
            sudo_revision: Arc::new(()),
            primary: Secret::default(),
            confirm: Secret::default(),
            next_master: Secret::default(),
            draft: None,
            operation: None,
            sudo_groups: Vec::new(),
            backup_path: String::new(),
            selected: None,
            status: None,
            webhook_test: None,
            webhook_test_status: None,
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

    pub(crate) fn recovery_output_locked(&self) -> bool {
        self.vault.is_none() && !self.busy
    }

    pub(crate) fn seal_recovery_output(
        &self,
        layout: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, VaultError> {
        self.vault
            .as_ref()
            .ok_or(VaultError::InvalidInput(
                "Unlock the vault to save session output.",
            ))?
            .seal_recovery_output(layout, plaintext)
    }

    pub(crate) fn open_recovery_output(
        &self,
        layout: &[u8],
        sealed: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        self.vault
            .as_ref()
            .ok_or(VaultError::InvalidInput(
                "Unlock the vault to read saved session output.",
            ))?
            .open_recovery_output(layout, sealed)
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
                        && endpoint.is_none_or(|endpoint| {
                            if kind == Kind::PasswordCommand {
                                password_command::matches_endpoint(entry, endpoint)
                            } else {
                                matches_endpoint(entry, endpoint)
                            }
                        })
                })
                .map(|entry| CredentialChoice {
                    id: entry.id.clone(),
                    label: entry.label.clone(),
                    username: entry.username.clone(),
                })
                .collect()
        })
    }

    pub(crate) fn password_command(
        &self,
        id: &str,
        endpoint: &LoginTarget,
    ) -> Result<Option<CommandAuthority>, String> {
        let Some(vault) = self.vault.as_ref() else {
            return Ok(None);
        };
        let Some(entry) = vault
            .get(id)
            .filter(|entry| entry.kind == CredentialKind::SshPasswordCommand)
        else {
            return Ok(None);
        };
        if !password_command::matches_endpoint(entry, endpoint) {
            return Err("The password command is outside the scope of this SSH login.".into());
        }
        let recipe = vault
            .secret(id)
            .ok_or("The password command is unavailable.")?;
        CommandAuthority::new(recipe, Arc::downgrade(&self.sudo_revision)).map(Some)
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

    pub(crate) fn huggingface_token(&self, id: &str) -> Result<Secret, &'static str> {
        self.vault
            .as_ref()
            .ok_or("Unlock the vault to use the selected Hugging Face token.")?
            .huggingface_token(id)
            .map(|token| Secret::new(token.to_owned()))
            .ok_or("The selected Hugging Face token is missing or has the wrong type.")
    }

    pub(crate) fn qdrant_api_key(&self, id: &str) -> Result<Secret, &'static str> {
        self.vault
            .as_ref()
            .ok_or("Unlock the vault to use the selected Qdrant API key.")?
            .qdrant_api_key(id)
            .map(|key| Secret::new(key.to_owned()))
            .ok_or("The selected Qdrant API key is missing or has the wrong type.")
    }

    pub(crate) fn qdrant_api_key_with_envelope(
        &self,
        id: &str,
    ) -> Result<(&str, &[u8]), &'static str> {
        self.vault
            .as_ref()
            .ok_or("Unlock the vault to use the selected Qdrant API key.")?
            .qdrant_api_key_with_envelope(id)
            .ok_or("The selected Qdrant API key is missing or has the wrong type.")
    }

    pub(crate) fn scheduler_sudo_password(
        &self,
        target: &LoginTarget,
        group: Option<&str>,
    ) -> Result<Secret, &'static str> {
        let vault = self
            .vault
            .as_ref()
            .ok_or("Unlock the vault before running a password-based sudo job.")?;
        resolve_scheduler_sudo(vault, target, group)
    }

    pub(crate) fn agent_sudo_credential(
        &self,
        target: &LoginTarget,
    ) -> Result<AgentSudoCredential, &'static str> {
        let vault = self.vault.as_ref().ok_or("The vault is locked or busy.")?;
        let entry = exact_sudo_entry(vault, target)?
            .ok_or("No vault sudo credential matches this exact SSH account.")?;
        Ok(AgentSudoCredential {
            id: entry.id.clone(),
            label: entry.label.clone(),
            revision: Arc::downgrade(&self.sudo_revision),
        })
    }

    pub(crate) fn credential_revision(&self) -> Option<Weak<()>> {
        self.vault
            .as_ref()
            .map(|_| Arc::downgrade(&self.sudo_revision))
    }

    pub(crate) fn check_agent_sudo_credential(
        &self,
        target: &LoginTarget,
        credential: &AgentSudoCredential,
    ) -> Result<(), &'static str> {
        let current = self.agent_sudo_credential(target)?;
        if current.id != credential.id || !Weak::ptr_eq(&current.revision, &credential.revision) {
            return Err("The vault sudo credential changed. Start a new review.");
        }
        Ok(())
    }

    pub(crate) fn agent_sudo_line(
        &self,
        target: &LoginTarget,
        credential: &AgentSudoCredential,
    ) -> Result<SensitiveLine, &'static str> {
        SensitiveLine::new(self.agent_sudo_secret(target, credential)?)
            .map_err(|_| "The reviewed vault sudo credential is unavailable.")
    }

    pub(crate) fn agent_sudo_line_with_overlay(
        &self,
        target: &LoginTarget,
        credential: &AgentSudoCredential,
        previous: Option<&RedactionSecrets>,
    ) -> Result<(SensitiveLine, RedactionSecrets), &'static str> {
        let secret = self.agent_sudo_secret(target, credential)?;
        let line = SensitiveLine::new(secret)
            .map_err(|_| "The reviewed vault sudo credential is unavailable.")?;
        let overlay = RedactionSecrets::with_secret(previous, secret)
            .map_err(|_| "The reviewed sudo password could not be safely masked.")?;
        Ok((line, overlay))
    }

    fn agent_sudo_secret(
        &self,
        target: &LoginTarget,
        credential: &AgentSudoCredential,
    ) -> Result<&str, &'static str> {
        self.check_agent_sudo_credential(target, credential)?;
        self.vault
            .as_ref()
            .and_then(|vault| vault.secret(&credential.id))
            .ok_or("The reviewed vault sudo credential is unavailable.")
    }

    pub(crate) fn job_webhook_url(&self, id: &str) -> Option<Secret> {
        self.vault
            .as_ref()?
            .job_webhook_url(id)
            .map(|url| Secret::new(url.to_owned()))
    }

    pub(crate) fn job_webhook_settings(&self, id: &str) -> Option<WebhookSettings> {
        self.vault.as_ref()?.get(id)?.webhook_settings()
    }

    pub(crate) fn test_webhook(
        &mut self,
        source: &WebhookTestSource,
        policy: &ClipboardPolicy,
    ) -> Task<Message> {
        if self.busy
            || self.confirming_close
            || self.unlock_prompt.is_some()
            || self.operation.is_some()
            || self.webhook_test_pending()
            || !match source {
                WebhookTestSource::Saved(id) => {
                    self.draft.is_none() && self.selected.as_deref() == Some(id)
                }
                WebhookTestSource::Draft(revision) => self.webhook_test_draft(revision).is_some(),
            }
        {
            return Task::none();
        }
        let installed = ClipboardPolicy::load();
        let (secret, settings) = match self.webhook_test_destination(source, [policy, &installed]) {
            Ok(destination) => destination,
            Err(outcome) => {
                self.webhook_test_status = Some(webhook_test_status(outcome));
                return Task::none();
            }
        };
        self.webhook_test_status = None;
        let gate = Arc::new(AtomicBool::new(true));
        let result = webhooks::spawn(secret, webhooks::sample_body(settings), Arc::clone(&gate));
        self.webhook_test = Some(WebhookTest {
            gate: Arc::clone(&gate),
            result: result.clone(),
        });
        Task::future(async move {
            let outcome = result.recv().await.unwrap_or(WebhookOutcome::Unavailable);
            Message::WebhookTestFinished(gate, outcome)
        })
    }

    fn webhook_test_destination(
        &self,
        source: &WebhookTestSource,
        policies: [&ClipboardPolicy; 2],
    ) -> Result<(Secret, WebhookSettings), WebhookOutcome> {
        if policies.iter().any(|policy| !policy.job_webhooks_allowed) {
            return Err(WebhookOutcome::PolicyDenied);
        }
        let (secret, settings) = match source {
            WebhookTestSource::Saved(id) => (
                self.job_webhook_url(id),
                self.job_webhook_settings(id)
                    .ok_or(WebhookOutcome::MissingCredential)?,
            ),
            WebhookTestSource::Draft(revision) => {
                let draft = self
                    .webhook_test_draft(revision)
                    .ok_or(WebhookOutcome::MissingCredential)?;
                let secret = match &draft.id {
                    Some(id) => self.job_webhook_url(id),
                    None => Some(Secret::new(draft.secret.as_str().to_owned())),
                };
                (secret, draft.webhook)
            }
        };
        let secret = secret.ok_or(WebhookOutcome::MissingCredential)?;
        let url = webhooks::destination(&secret).ok_or(WebhookOutcome::InvalidDestination)?;
        if policies
            .iter()
            .any(|policy| !policy.job_webhook_hosts.allows(&url))
        {
            return Err(WebhookOutcome::PolicyDenied);
        }
        Ok((secret, settings))
    }

    fn webhook_test_draft(&self, revision: &Arc<()>) -> Option<&EntryDraft> {
        self.vault.as_ref()?;
        self.draft.as_ref().filter(|draft| {
            draft.kind == Kind::JobWebhookUrl
                && Arc::ptr_eq(&draft.webhook_revision, revision)
                && draft
                    .id
                    .as_deref()
                    .is_none_or(|id| self.selected.as_deref() == Some(id))
        })
    }

    fn webhook_test_pending(&self) -> bool {
        self.webhook_test
            .as_ref()
            .is_some_and(|test| !test.result.is_closed())
    }

    fn revoke_webhook_test(&mut self) {
        self.webhook_test_status = None;
        if let Some(test) = &self.webhook_test {
            test.gate.store(false, Ordering::Release);
        }
    }

    pub(crate) fn job_webhook_choices(&self) -> Vec<crate::scheduler_window::webhooks::Choice> {
        let Some(vault) = &self.vault else {
            return Vec::new();
        };
        vault
            .entries()
            .filter_map(|entry| {
                let settings = entry.webhook_settings()?;
                Some(crate::scheduler_window::webhooks::Choice {
                    id: entry.id.clone(),
                    label: entry.label.clone(),
                    settings,
                })
            })
            .collect()
    }

    pub(crate) fn set_sudo_groups(&mut self, groups: impl Iterator<Item = impl Into<String>>) {
        self.sudo_groups = groups.map(Into::into).collect();
    }

    pub(crate) fn add_summary_secrets(&self, summary: &str, secrets: &Secrets) {
        for secret in self.matching_summary_secrets(summary) {
            secrets.add(secret);
        }
    }

    pub(crate) fn journal_question_secrets(&self) -> JournalQuestionSecrets {
        // The unlocked vault already bounds entry count and total plaintext size.
        // Keep a disposable snapshot so source text can be masked before clipping.
        let mut values = Vec::new();
        if let Some(vault) = &self.vault {
            for entry in vault.entries() {
                let Some(secret) = vault.secret(&entry.id).map(str::trim) else {
                    continue;
                };
                if secret.is_empty() {
                    continue;
                }
                let mut display = Zeroizing::new(secret.to_owned());
                display.retain(crate::session_journal::is_display_character);
                if !display.trim().is_empty() && display.as_str() != secret {
                    values.push(display);
                }
                values.push(Zeroizing::new(secret.to_owned()));
            }
        }
        JournalQuestionSecrets(Arc::from(values))
    }

    pub(crate) fn summary_contains_secret(&self, summary: &str) -> bool {
        self.matching_summary_secrets(summary).next().is_some()
    }

    fn matching_summary_secrets<'a>(&'a self, summary: &'a str) -> impl Iterator<Item = &'a str> {
        self.vault
            .iter()
            .flat_map(|vault| {
                vault
                    .entries()
                    .filter_map(move |entry| vault.secret(&entry.id))
            })
            .filter(move |secret| !secret.trim().is_empty() && summary.contains(secret.trim()))
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
        self.sudo_revision = Arc::new(());
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
        self.revoke_webhook_test();
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
                (Vault::unlock(&path, primary.as_bytes()), "Vault unlocked.")
            } else {
                let result = prepare_directory(&path).map_err(VaultError::from);
                if let Some(backup) = backup {
                    (
                        result
                            .and_then(|()| Vault::restore_from(&path, &backup, primary.as_bytes())),
                        "Vault restored.",
                    )
                } else {
                    (
                        result.and_then(|()| Vault::create(&path, primary.as_bytes())),
                        "Vault created.",
                    )
                }
            };
            let recovery_key_changed =
                !existing && matches!(result, Ok(_) | Err(VaultError::Durability(_)));
            let result = match result {
                Ok(vault) => Finished {
                    vault: Some(vault),
                    status: Ok(status),
                    selected: None,
                    recovery_key_changed,
                },
                Err(error) => Finished {
                    vault: None,
                    status: Err(error.to_string()),
                    selected: None,
                    recovery_key_changed,
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

    fn save_entry(&mut self, update_secret: Secret) -> Task<Message> {
        let Some(draft) = self.draft.as_ref() else {
            return Task::none();
        };
        let input = draft.input();
        let webhook = draft.webhook;
        self.mutate(false, move |vault| {
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
            let saved = if input.kind == CredentialKind::JobWebhookUrl {
                vault.save_webhook(input, secret, webhook)
            } else {
                vault.save(input, secret)
            };
            saved.map(|id| (Some(id), "Credential saved."))
        })
    }

    fn mutate(
        &mut self,
        changes_recovery_key: bool,
        operation: impl FnOnce(
            &mut Vault,
        ) -> Result<(Option<String>, &'static str), kortty_vault::Error>
        + Send
        + 'static,
    ) -> Task<Message> {
        let Some(mut vault) = self.vault.take() else {
            return Task::none();
        };
        self.revoke_webhook_test();
        self.sudo_revision = Arc::new(());
        let previous = self.selected.clone();
        self.busy = true;
        Task::future(async move {
            let result = operation(&mut vault);
            let recovery_key_changed =
                changes_recovery_key && matches!(result, Ok(_) | Err(VaultError::Durability(_)));
            let (selected, status) = match result {
                Ok((id, status)) => (id.or(previous), Ok(status)),
                Err(error) => (previous, Err(error.to_string())),
            };
            let result = Finished {
                vault: Some(vault),
                status,
                selected,
                recovery_key_changed,
            };
            Message::Completed(Completion(Arc::new(Mutex::new(Some(result)))))
        })
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        if let Message::WebhookTestFinished(gate, outcome) = message {
            if self
                .webhook_test
                .as_ref()
                .is_some_and(|test| Arc::ptr_eq(&test.gate, &gate))
            {
                if gate.load(Ordering::Acquire) {
                    self.webhook_test_status = Some(webhook_test_status(outcome));
                }
                self.webhook_test = None;
            }
            return Task::none();
        }
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
                self.sudo_revision = Arc::new(());
                self.clear_form();
                self.selected = None;
                self.status = Some("Vault locked.".into());
            }
            Message::NewEntry if self.operation.is_none() && self.draft.is_none() => {
                if self.vault.is_some() {
                    self.revoke_webhook_test();
                    self.draft = Some(EntryDraft::new());
                }
            }
            Message::Select(id) if self.draft.is_none() && self.operation.is_none() => {
                if self.selected.as_deref() != Some(&id) {
                    self.revoke_webhook_test();
                }
                self.selected = Some(id);
            }
            Message::EditSelected if self.draft.is_none() && self.operation.is_none() => {
                self.revoke_webhook_test();
                self.draft = self.selected_entry().map(EntryDraft::from_entry);
            }
            Message::Field(field) => {
                if let Some(draft) = &mut self.draft {
                    draft.update(field);
                    draft.webhook_revision = Arc::new(());
                    self.revoke_webhook_test();
                }
            }
            Message::SaveEntry => {
                let Some(draft) = self.draft.as_mut().filter(|draft| draft.valid()) else {
                    return Task::none();
                };
                if draft.kind == Kind::PasswordCommand && !draft.password_program.is_empty() {
                    let revision = Arc::new(());
                    draft.password_inspection = Some(Arc::clone(&revision));
                    let executable = draft.password_program.clone();
                    let arguments = draft.password_arguments.clone();
                    return password_command::inspect(executable, arguments, revision);
                }
                let update_secret = std::mem::take(&mut draft.secret);
                return self.save_entry(update_secret);
            }
            Message::PasswordCommandInspected(PasswordCommandInspection(revision, result)) => {
                let Some(draft) = self.draft.as_mut().filter(|draft| {
                    draft
                        .password_inspection
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &revision))
                }) else {
                    return Task::none();
                };
                draft.password_inspection = None;
                if let Some(result) = result
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                {
                    match result {
                        Ok(secret) => return self.save_entry(secret),
                        Err(error) => self.status = Some(error),
                    }
                }
            }
            Message::DeleteEntry if self.selected_entry().is_some() && self.draft.is_none() => {
                self.revoke_webhook_test();
                self.operation = Some(Operation::Delete);
            }
            Message::ConfirmDelete if self.operation == Some(Operation::Delete) => {
                self.operation = None;
                if let Some(id) = self.selected.clone() {
                    return self.mutate(false, move |vault| {
                        vault.delete(&id).map(|_| (None, "Credential deleted."))
                    });
                }
            }
            Message::ChangeMaster if self.draft.is_none() && self.operation.is_none() => {
                self.revoke_webhook_test();
                self.operation = Some(Operation::ChangeMaster);
            }
            Message::Backup if self.draft.is_none() && self.operation.is_none() => {
                self.revoke_webhook_test();
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
                return self.mutate(true, move |vault| {
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
                return self.mutate(false, move |vault| {
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
                self.sudo_revision = Arc::new(());
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

    pub(crate) fn view<'a>(
        &'a self,
        environments: &'a Catalog,
        webhook_test_allowed: bool,
    ) -> Element<'a, Message> {
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
            let right = self.unlocked_details(environments, webhook_test_allowed);
            return contents
                .push(
                    row![
                        scrollable(entries).width(Length::FillPortion(1)),
                        scrollable(container(right).width(Length::Fill))
                            .spacing(6)
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

    fn unlocked_details<'a>(
        &'a self,
        environments: &'a Catalog,
        webhook_test_allowed: bool,
    ) -> Element<'a, Message> {
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
            let webhook_test = (draft.kind == Kind::JobWebhookUrl).then(|| {
                self.webhook_test_control(
                    (draft.id.is_some() || !draft.secret.is_empty())
                        .then(|| WebhookTestSource::Draft(Arc::clone(&draft.webhook_revision))),
                    webhook_test_allowed,
                    "Sends sample data using these draft settings.",
                )
            });
            return draft.view(environments, &self.sudo_groups, webhook_test);
        }
        if let Some(entry) = self.selected_entry() {
            let kind = Kind::from_native(entry.kind);
            let scope_label = match kind {
                Kind::SudoGroupPassword => "Connection group",
                Kind::SudoPassword | Kind::PasswordCommand => "SSH endpoint",
                _ => "Host pattern",
            };
            let scope = if entry.server_pattern.is_empty() {
                "Any host"
            } else {
                &entry.server_pattern
            };
            return column![
                text(&entry.label).size(20),
                text(kind.to_string()),
                kind.has_login_scope()
                    .then(|| text(format!("Username  {}", entry.username))),
                kind.has_login_scope()
                    .then(|| text(format!("{scope_label}  {scope}"))),
                kind.has_environment().then(|| text(format!(
                    "Environment  {}",
                    environments.display_label(&entry.environment)
                ))),
                entry.webhook_settings().map(|settings| text(format!(
                    "{} · {} · {}",
                    if settings.enabled {
                        "Enabled"
                    } else {
                        "Disabled"
                    },
                    settings.format,
                    if settings.include_summary {
                        "Masked summary"
                    } else {
                        "Job metadata"
                    }
                ))),
                entry.webhook_settings().map(|_| {
                    self.webhook_test_control(
                        Some(WebhookTestSource::Saved(entry.id.clone())),
                        webhook_test_allowed,
                        "Sends sample data to this saved destination.",
                    )
                }),
                text(&entry.description),
            ]
            .spacing(8)
            .into();
        }
        text("Select a credential or create one.").into()
    }

    fn webhook_test_control(
        &self,
        source: Option<WebhookTestSource>,
        allowed: bool,
        description: &'static str,
    ) -> Element<'_, Message> {
        let pending = self.webhook_test_pending();
        column![
            row![
                button(if pending {
                    "Sending test…"
                } else {
                    "Send test"
                })
                .on_press_maybe(
                    source
                        .filter(|_| allowed && !pending)
                        .map(Message::TestWebhook),
                ),
                text(if allowed {
                    description
                } else {
                    "Managed policy blocks webhook tests."
                })
                .size(13),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .wrap(),
            self.webhook_test_status.map(|status| text(status).size(13)),
        ]
        .spacing(6)
        .into()
    }
}

fn webhook_test_status(outcome: WebhookOutcome) -> &'static str {
    match outcome {
        WebhookOutcome::Delivered => "Webhook test delivered.",
        WebhookOutcome::PolicyDenied => "Managed policy blocks this webhook test.",
        WebhookOutcome::MissingCredential => {
            "The saved webhook is unavailable. Unlock the vault and select it again."
        }
        WebhookOutcome::InvalidDestination => "The saved webhook URL is invalid.",
        WebhookOutcome::Rejected => "The receiver rejected the webhook test.",
        WebhookOutcome::Revoked | WebhookOutcome::TargetDisabled => {
            "The webhook test was cancelled."
        }
        WebhookOutcome::Unavailable | WebhookOutcome::QueueFull => {
            "The webhook test could not reach the receiver."
        }
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
    if !kind.native().is_ssh() || entry.kind != kind.native() || !matches_endpoint(entry, endpoint)
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

fn resolve_scheduler_sudo(
    vault: &Vault,
    target: &LoginTarget,
    group: Option<&str>,
) -> Result<Secret, &'static str> {
    let entry = if let Some(entry) = exact_sudo_entry(vault, target)? {
        entry
    } else {
        let mut choices = vault.entries().filter(|entry| {
            entry.kind == CredentialKind::SudoGroupPassword
                && entry.username == target.username
                && Some(entry.server_pattern.as_str()) == group
        });
        let entry = choices
            .next()
            .ok_or("No sudo password matches this SSH account or its saved connection group.")?;
        if choices.next().is_some() {
            return Err(
                "Multiple group sudo passwords match this SSH account. Keep one in the vault.",
            );
        }
        entry
    };
    vault
        .secret(&entry.id)
        .filter(|secret| valid_sudo_password(secret))
        .map(|secret| Secret::new(secret.to_owned()))
        .ok_or("The matching sudo password is invalid.")
}

fn exact_sudo_entry<'a>(
    vault: &'a Vault,
    target: &LoginTarget,
) -> Result<Option<&'a Credential>, &'static str> {
    let endpoint = Endpoint::new(&target.host, target.port)
        .map_err(|_| "The sudo target is not a valid SSH endpoint.")?;
    let mut choices = vault.entries().filter(|entry| {
        entry.kind == CredentialKind::SudoPassword
            && entry.username == target.username
            && sudo_scope(&entry.server_pattern)
                .and_then(|(host, port)| Endpoint::new(host, port).ok())
                .is_some_and(|scope| scope == endpoint && scope.wire_host() == endpoint.wire_host())
    });
    let entry = choices.next();
    if choices.next().is_some() {
        return Err("Multiple sudo passwords match this SSH account. Keep one in the vault.");
    }
    Ok(entry)
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
            webhook: WebhookSettings {
                format: WebhookFormat::GenericJson,
                ..WebhookSettings::default()
            },
            webhook_revision: Arc::new(()),
            secret: Secret::default(),
            password_program: Secret::default(),
            password_arguments: Secret::default(),
            password_inspection: None,
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
            webhook: entry.webhook_settings().unwrap_or_default(),
            ..Self::new()
        }
    }

    fn valid(&self) -> bool {
        let scope_valid = match self.kind {
            Kind::SudoPassword | Kind::PasswordCommand => {
                sudo_scope(self.host_pattern.trim()).is_some()
            }
            Kind::SudoGroupPassword => valid_sudo_group(self.host_pattern.trim()),
            _ => true,
        };
        let account_valid = !matches!(
            self.kind,
            Kind::SudoPassword | Kind::SudoGroupPassword | Kind::PasswordCommand
        ) || !self.username.trim().is_empty();
        !self.label.trim().is_empty()
            && scope_valid
            && account_valid
            && self.password_inspection.is_none()
            && (self.kind != Kind::PasswordCommand
                || !self.password_program.is_empty()
                || (self.id.is_some() && self.password_arguments.is_empty()))
            && (self.id.is_some() && self.secret.is_empty()
                || match self.kind {
                    Kind::ArchivePassword => valid_archive_password(&self.secret),
                    Kind::HuggingFaceToken => valid_huggingface_token(&self.secret),
                    Kind::QdrantApiKey => valid_qdrant_api_key(&self.secret),
                    Kind::SudoPassword | Kind::SudoGroupPassword => {
                        valid_sudo_password(&self.secret)
                    }
                    Kind::JobWebhookUrl => {
                        crate::scheduler_window::webhooks::destination(&self.secret).is_some()
                    }
                    Kind::Password | Kind::KeyPassphrase => !self.secret.is_empty(),
                    Kind::PasswordCommand => !self.password_program.is_empty(),
                })
    }

    fn update(&mut self, field: Field) {
        self.password_inspection = None;
        match field {
            Field::Label(value) => self.label = bound(value, 160),
            Field::Username(value) => self.username = bound(value, 255),
            Field::HostPattern(value) => {
                let limit = if self.kind == Kind::SudoGroupPassword {
                    MAX_SUDO_GROUP_BYTES
                } else {
                    MAX_SUDO_SCOPE_BYTES
                };
                self.host_pattern = bound(value, limit);
            }
            Field::Environment(value) => self.environment = bound(value, 128),
            Field::Description(value) => self.description = bound(value, 1024),
            Field::WebhookEnabled(enabled) => self.webhook.enabled = enabled,
            Field::WebhookFormat(format) => self.webhook.format = format,
            Field::WebhookSummary(include) => self.webhook.include_summary = include,
            Field::Kind(kind) => {
                if kind != self.kind && (!kind.native().is_ssh() || !self.kind.native().is_ssh()) {
                    if self.id.is_some() {
                        return;
                    }
                    self.secret = Secret::default();
                    self.password_program = Secret::default();
                    self.password_arguments = Secret::default();
                    if !kind.native().is_ssh() && self.environment == DEFAULT_ENVIRONMENT {
                        self.environment.clear();
                    } else if kind.native().is_ssh() && self.environment.is_empty() {
                        self.environment = DEFAULT_ENVIRONMENT.to_owned();
                    }
                }
                self.kind = kind;
            }
            Field::Secret(SecretEdit(value)) if self.kind == Kind::QdrantApiKey => {
                // Keep an over-limit paste invalid even when an empty edit would retain the saved key.
                let limit = MAX_QDRANT_API_KEY_BYTES + char::MAX.len_utf8();
                self.secret = if value.len() > limit {
                    Secret::new(value[..value.floor_char_boundary(limit)].to_owned())
                } else {
                    value
                };
            }
            Field::Secret(secret) => self.secret = bound_secret(secret.0, 4096),
            Field::PasswordProgram(value) if self.kind == Kind::PasswordCommand => {
                self.password_program = bound_secret(value.0, 4097);
            }
            Field::PasswordArguments(value) if self.kind == Kind::PasswordCommand => {
                self.password_arguments =
                    bound_secret(value.0, password_command::MAX_ARGUMENT_INPUT + 1);
            }
            Field::PasswordProgram(_) | Field::PasswordArguments(_) => return,
        }
        self.changed = true;
    }

    fn input(&self) -> CredentialInput {
        CredentialInput {
            id: self.id.clone(),
            label: self.label.trim().to_owned(),
            username: if !self.kind.has_login_scope() {
                String::new()
            } else {
                self.username.trim().to_owned()
            },
            kind: self.kind.native(),
            server_pattern: if !self.kind.has_login_scope() {
                String::new()
            } else {
                self.host_pattern.trim().to_owned()
            },
            environment: if self.kind.has_environment() {
                self.environment.trim().to_owned()
            } else {
                String::new()
            },
            description: self.description.trim().to_owned(),
        }
    }

    fn view<'a>(
        &'a self,
        environments: &'a Catalog,
        groups: &'a [String],
        webhook_test: Option<Element<'a, Message>>,
    ) -> Element<'a, Message> {
        let kinds: &[Kind] = if self.id.is_none() {
            &[
                Kind::Password,
                Kind::PasswordCommand,
                Kind::KeyPassphrase,
                Kind::ArchivePassword,
                Kind::HuggingFaceToken,
                Kind::QdrantApiKey,
                Kind::SudoPassword,
                Kind::SudoGroupPassword,
                Kind::JobWebhookUrl,
            ]
        } else if !self.kind.native().is_ssh() {
            std::slice::from_ref(&self.kind)
        } else {
            &[Kind::Password, Kind::KeyPassphrase]
        };
        let scope: Element<'_, Message> = if self.kind == Kind::ArchivePassword {
            text("Used by encrypted ZIP jobs while the vault is unlocked.")
                .size(13)
                .into()
        } else if self.kind == Kind::HuggingFaceToken {
            text("Used for reviewed Hugging Face requests while the vault is unlocked.")
                .size(13)
                .into()
        } else if self.kind == Kind::QdrantApiKey {
            text("Use an ASCII key without spaces, up to 4096 bytes. Reviewed Qdrant requests require the vault to be unlocked.")
                .size(13)
                .into()
        } else if self.kind == Kind::JobWebhookUrl {
            text("Used by assigned scheduler jobs while the vault is unlocked. Create a new credential to change the destination.")
                .size(13)
                .into()
        } else if matches!(self.kind, Kind::SudoPassword | Kind::PasswordCommand) {
            column![
                text_input("SSH username (required)", &self.username)
                    .on_input(|value| Message::Field(Field::Username(value))),
                text_input("Exact SSH host and port (server:22)", &self.host_pattern)
                    .on_input(|value| Message::Field(Field::HostPattern(value))),
                text(if self.kind == Kind::PasswordCommand {
                    "Review and run the program when signing in to this account. Its first output line is the password."
                } else {
                    "Available only to sudo jobs for this SSH account while the vault is unlocked."
                })
                .size(13)
            ]
            .spacing(6)
            .into()
        } else if self.kind == Kind::SudoGroupPassword {
            let group = groups
                .iter()
                .find(|group| *group == &self.host_pattern)
                .cloned();
            let missing = group.is_none() && !self.host_pattern.is_empty();
            column![
                text_input("SSH username (required)", &self.username)
                    .on_input(|value| Message::Field(Field::Username(value))),
                pick_list(groups, group, |value| Message::Field(Field::HostPattern(value)))
                    .placeholder("Choose the exact connection group")
                    .width(Length::Fill),
                missing.then(|| text(format!("Saved group is unavailable · {}", self.host_pattern)).size(12)),
                text("This fallback can be used by matching scheduler jobs in this exact group. An endpoint-specific password takes precedence.").size(13),
            ]
            .spacing(6)
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
            self.kind.has_environment().then(|| row![
                text("Environment").width(92),
                pick_list(choices, selection, |choice| {
                    Message::Field(Field::Environment(choice.id))
                })
            ]
            .spacing(6)
            .align_y(Alignment::Center)),
            text_input("Description (optional)", &self.description)
                .on_input(|value| Message::Field(Field::Description(value))),
            (self.kind == Kind::JobWebhookUrl).then(|| self.webhook_view()),
            (self.kind == Kind::PasswordCommand).then(|| column![
                text_input(
                    if self.id.is_some() {
                        "New absolute executable path (blank keeps saved command)"
                    } else {
                        "Absolute native executable path"
                    },
                    &self.password_program
                )
                .on_input(|value| Message::Field(Field::PasswordProgram(SecretEdit::new(value)))),
                text_input(
                    "Arguments as a JSON array of strings (optional)",
                    &self.password_arguments
                )
                .secure(true)
                .on_input(|value| Message::Field(Field::PasswordArguments(SecretEdit::new(value)))),
                self.password_inspection
                    .is_some()
                    .then(|| text("Checking the executable…").size(13)),
            ]
            .spacing(6)),
            (self.kind != Kind::PasswordCommand
                && (self.id.is_none() || self.kind != Kind::JobWebhookUrl))
                .then(|| {
                    let prompt = match (self.kind, self.id.is_some()) {
                        (Kind::JobWebhookUrl, _) => "HTTPS webhook URL",
                        (Kind::QdrantApiKey, false) => "Qdrant API key",
                        (_, true) => "New secret (blank keeps the stored value)",
                        _ => "Secret",
                    };
                    text_input(prompt, &self.secret)
                        .secure(true)
                        .on_input(|value| Message::Field(Field::Secret(SecretEdit::new(value))))
                }),
            webhook_test,
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

    fn webhook_view(&self) -> Element<'static, Message> {
        let formats = [
            WebhookFormat::JobDefault,
            WebhookFormat::GenericJson,
            WebhookFormat::Slack,
            WebhookFormat::Teams,
        ];
        column![
            checkbox(self.webhook.enabled)
                .label("Enable this destination for assigned jobs")
                .on_toggle(|enabled| Message::Field(Field::WebhookEnabled(enabled))),
            row![
                text("Message format"),
                pick_list(formats, Some(self.webhook.format), |format| {
                    Message::Field(Field::WebhookFormat(format))
                }),
            ].spacing(6).align_y(Alignment::Center),
            checkbox(self.webhook.include_summary)
                .label("Send the masked job summary")
                .on_toggle(|include| Message::Field(Field::WebhookSummary(include))),
            text("The summary can include target identifiers and reasons for the outcome. Command output and recordings stay in kortty.").size(13),
        ]
        .spacing(6)
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
    fn qdrant_key_draft_rejects_an_over_limit_paste_and_keeps_dedicated_metadata() {
        let mut draft = EntryDraft::new();
        draft.update(Field::Label("Fixture key".into()));
        draft.update(Field::Username("fixture".into()));
        draft.update(Field::HostPattern("fixture.test".into()));
        draft.update(Field::Environment("fixture".into()));
        draft.update(Field::Kind(Kind::QdrantApiKey));
        assert!(!draft.valid());
        draft.id = Some("a".repeat(32));
        assert!(draft.valid());
        let valid = "x".repeat(MAX_QDRANT_API_KEY_BYTES);
        draft.update(Field::Secret(SecretEdit::new(format!("{valid}😃extra"))));
        assert!(!draft.valid());
        draft.update(Field::Secret(SecretEdit::new(valid)));
        assert!(draft.valid());
        let input = draft.input();
        assert_eq!(input.kind, CredentialKind::QdrantApiKey);
        assert!(
            input.username.is_empty()
                && input.server_pattern.is_empty()
                && input.environment.is_empty()
        );
    }

    #[test]
    fn qdrant_api_key_lookup_is_typed_and_requires_an_unlocked_vault() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("vault.bin");
        let mut vault = Vault::create(&path, b"qdrant-fixture-master").unwrap();
        let id = vault
            .save(
                CredentialInput {
                    label: "Fixture Qdrant key".into(),
                    kind: CredentialKind::QdrantApiKey,
                    ..CredentialInput::default()
                },
                Secret::new("fixture-key".into()),
            )
            .unwrap();
        let ssh = vault
            .save(
                CredentialInput {
                    label: "Fixture SSH password".into(),
                    ..CredentialInput::default()
                },
                Secret::new("fixture-password".into()),
            )
            .unwrap();
        let mut window = VaultWindow::with_unlocked_fixture(path, vault);
        assert_eq!(window.choices(Kind::QdrantApiKey)[0].id, id);
        assert_eq!(window.qdrant_api_key(&id).unwrap().as_str(), "fixture-key");
        assert!(window.qdrant_api_key(&ssh).is_err());
        let revision = window.credential_revision().unwrap();
        let _ = window.update(Message::Lock);
        assert!(revision.upgrade().is_none());
        assert!(window.choices(Kind::QdrantApiKey).is_empty());
        assert!(window.qdrant_api_key(&id).is_err());
    }

    #[test]
    fn scheduler_webhook_url_stays_encrypted_and_cannot_be_retargeted() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("vault.bin");
        let master = b"webhook-fixture-master";
        let url = "https://hooks.example.test/path?private=token";
        let mut vault = Vault::create(&path, master).unwrap();
        let input = CredentialInput {
            label: "Fixture webhook".into(),
            kind: CredentialKind::JobWebhookUrl,
            ..CredentialInput::default()
        };
        let id = vault.save(input.clone(), Secret::new(url.into())).unwrap();
        let edit = CredentialInput {
            id: Some(id.clone()),
            ..input
        };
        assert!(
            vault
                .save(
                    edit.clone(),
                    Secret::new("https://other.example.test/".into()),
                )
                .is_err()
        );
        vault
            .save(
                CredentialInput {
                    label: "Renamed webhook".into(),
                    ..edit
                },
                Secret::new(url.into()),
            )
            .unwrap();
        assert!(
            !fs::read(&path)
                .unwrap()
                .windows(url.len())
                .any(|bytes| bytes == url.as_bytes())
        );
        drop(vault);

        let mut window = VaultWindow::open(path.clone()).unwrap();
        let saved = WebhookTestSource::Saved(id.clone());
        assert!(window.job_webhook_url(&id).is_none());
        let unrestricted = ClipboardPolicy::default();
        let parse_policy = |rule| {
            crate::clipboard_policy::parse(&format!("[meta]\nschema-version=1\n[[rule]]\n{rule}"))
                .unwrap()
        };
        let feature_denied = parse_policy("[rule.features]\njob-webhooks='deny'");
        assert!(matches!(
            window.webhook_test_destination(&saved, [&unrestricted, &feature_denied]),
            Err(WebhookOutcome::PolicyDenied)
        ));
        assert!(matches!(
            window.webhook_test_destination(&saved, [&unrestricted, &unrestricted]),
            Err(WebhookOutcome::MissingCredential)
        ));
        window.vault = Some(Vault::unlock(&path, master).unwrap());
        assert_eq!(window.job_webhook_url(&id).unwrap().as_str(), url);
        assert_eq!(
            window.choices(Kind::JobWebhookUrl)[0].label,
            "Renamed webhook"
        );
        assert!(window.choices(Kind::Password).is_empty());
        assert!(window.archive_password(&id).is_err());
        assert_eq!(
            window.job_webhook_settings(&id),
            Some(WebhookSettings::default())
        );
        let settings = WebhookSettings {
            enabled: false,
            format: WebhookFormat::Teams,
            include_summary: true,
        };
        let vault = window.vault.as_mut().unwrap();
        let edit = CredentialInput {
            id: Some(id.clone()),
            label: "Renamed webhook".into(),
            kind: CredentialKind::JobWebhookUrl,
            ..CredentialInput::default()
        };
        vault
            .save_webhook(edit.clone(), Secret::new(url.into()), settings)
            .unwrap();
        // Metadata-only callers preserve the authenticated delivery options.
        vault.save(edit, Secret::new(url.into())).unwrap();
        let host_denied =
            parse_policy("[rule.job-scheduler]\nwebhook-host-allowlist=['other.example.test']");
        assert!(matches!(
            window.webhook_test_destination(&saved, [&host_denied, &unrestricted]),
            Err(WebhookOutcome::PolicyDenied)
        ));
        let (test_url, test_settings) = window
            .webhook_test_destination(&saved, [&unrestricted, &unrestricted])
            .unwrap();
        assert_eq!(test_url.as_str(), url);
        assert_eq!(test_settings, settings);
        assert_eq!(window.test_webhook(&saved, &unrestricted).units(), 0);
        let _ = window.update(Message::Select(id.clone()));
        let _ = window.update(Message::EditSelected);
        assert_eq!(window.test_webhook(&saved, &unrestricted).units(), 0);
        let draft_source = |window: &VaultWindow| {
            WebhookTestSource::Draft(Arc::clone(&window.draft.as_ref().unwrap().webhook_revision))
        };
        let previous = draft_source(&window);
        let (sender, result) = async_channel::bounded(1);
        let gate = Arc::new(AtomicBool::new(true));
        window.webhook_test = Some(WebhookTest {
            gate: Arc::clone(&gate),
            result,
        });
        let _ = window.update(Message::Field(Field::WebhookFormat(WebhookFormat::Slack)));
        assert!(!gate.load(Ordering::Acquire));
        drop(sender);
        assert_eq!(window.test_webhook(&previous, &unrestricted).units(), 0);
        let current = draft_source(&window);
        let (draft_url, draft_settings) = window
            .webhook_test_destination(&current, [&unrestricted, &unrestricted])
            .unwrap();
        assert_eq!(draft_url.as_str(), url);
        assert_eq!(draft_settings.format, WebhookFormat::Slack);
        let _ = window.update(Message::CancelEdit);
        assert_eq!(window.test_webhook(&current, &unrestricted).units(), 0);

        let _ = window.update(Message::NewEntry);
        let _ = window.update(Message::Field(Field::Kind(Kind::JobWebhookUrl)));
        let new_url = "https://other.example.test/new?private=new-token";
        let _ = window.update(Message::Field(Field::Secret(SecretEdit::new(
            new_url.into(),
        ))));
        let source = draft_source(&window);
        let (draft_url, draft_settings) = window
            .webhook_test_destination(&source, [&unrestricted, &unrestricted])
            .unwrap();
        assert_eq!(draft_url.as_str(), new_url);
        assert_eq!(draft_settings.format, WebhookFormat::GenericJson);
        drop(window);
        let vault = Vault::unlock(&path, master).unwrap();
        assert_eq!(vault.get(&id).unwrap().webhook_settings(), Some(settings));
        assert_eq!(vault.job_webhook_url(&id), Some(url));
    }

    #[test]
    fn closing_vault_revokes_webhook_test_and_ignores_its_late_completion() {
        let profile = crate::snippets::tests::Profile::new();
        let mut window = VaultWindow::open(profile.0.join("vault.bin")).unwrap();
        let (_sender, result) = async_channel::bounded(1);
        let gate = Arc::new(AtomicBool::new(true));
        window.webhook_test = Some(WebhookTest {
            gate: Arc::clone(&gate),
            result,
        });
        assert!(window.webhook_test_pending());
        window.close_form();
        assert!(!gate.load(Ordering::Acquire));
        assert!(window.webhook_test_pending());
        let _ = window.update(Message::WebhookTestFinished(
            gate,
            WebhookOutcome::Delivered,
        ));
        assert!(!window.webhook_test_pending());
        assert!(window.webhook_test_status.is_none());
    }

    #[test]
    fn webhook_completion_is_retained_during_close_confirmation() {
        let profile = crate::snippets::tests::Profile::new();
        let mut window = VaultWindow::open(profile.0.join("vault.bin")).unwrap();
        let (_sender, result) = async_channel::bounded(1);
        let gate = Arc::new(AtomicBool::new(true));
        window.webhook_test = Some(WebhookTest {
            gate: Arc::clone(&gate),
            result,
        });
        window.confirming_close = true;
        let _ = window.update(Message::WebhookTestFinished(
            gate,
            WebhookOutcome::Delivered,
        ));
        assert_eq!(window.webhook_test_status, Some("Webhook test delivered."));
        assert!(!window.webhook_test_pending());
    }

    #[test]
    fn scheduler_sudo_password_round_trips_with_an_exact_account_scope() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("vault.bin");
        let master = b"sudo-fixture-master";
        let mut vault = Vault::create(&path, master).unwrap();
        let input = CredentialInput {
            label: "Fixture elevation".into(),
            username: "operator".into(),
            server_pattern: "[2001:db8::1]:2222".into(),
            kind: CredentialKind::SudoPassword,
            ..CredentialInput::default()
        };
        let id = vault
            .save(
                input.clone(),
                Secret::new("fixture-elevation-secret".into()),
            )
            .unwrap();
        assert!(
            vault
                .save(
                    CredentialInput {
                        server_pattern: "*.example.test:22".into(),
                        ..input.clone()
                    },
                    Secret::new("fixture".into()),
                )
                .is_err()
        );
        assert!(
            !fs::read(&path)
                .unwrap()
                .windows(b"fixture-elevation-secret".len())
                .any(|bytes| bytes == b"fixture-elevation-secret")
        );
        drop(vault);

        let target = LoginTarget {
            host: "2001:0db8::1".into(),
            port: 2222,
            username: "operator".into(),
        };
        let mut window = VaultWindow::open(path.clone()).unwrap();
        assert!(
            window
                .scheduler_sudo_password(&target, Some("Ops/Production"))
                .is_err()
        );
        window.vault = Some(Vault::unlock(&path, master).unwrap());
        let group_password = CredentialInput {
            label: "Production fallback".into(),
            username: "operator".into(),
            server_pattern: "Ops/Production".into(),
            kind: CredentialKind::SudoGroupPassword,
            ..CredentialInput::default()
        };
        window
            .vault
            .as_mut()
            .unwrap()
            .save(
                group_password.clone(),
                Secret::new("fixture-group-secret".into()),
            )
            .unwrap();
        assert_eq!(
            window
                .scheduler_sudo_password(&target, Some("Ops/Production"))
                .unwrap()
                .as_str(),
            "fixture-elevation-secret"
        );
        assert!(
            window
                .scheduler_sudo_password(
                    &LoginTarget {
                        port: 22,
                        ..target.clone()
                    },
                    None
                )
                .is_err()
        );
        assert!(
            window
                .scheduler_sudo_password(
                    &LoginTarget {
                        username: "other".into(),
                        ..target.clone()
                    },
                    Some("Ops/Production")
                )
                .is_err()
        );
        let another_endpoint = LoginTarget {
            port: 22,
            ..target.clone()
        };
        assert_eq!(
            window
                .scheduler_sudo_password(&another_endpoint, Some("Ops/Production"))
                .unwrap()
                .as_str(),
            "fixture-group-secret"
        );
        assert!(
            window
                .scheduler_sudo_password(&another_endpoint, Some("Ops/Production/Child"))
                .is_err()
        );
        window
            .vault
            .as_mut()
            .unwrap()
            .save(
                group_password,
                Secret::new("fixture-duplicate-group-secret".into()),
            )
            .unwrap();
        assert!(
            window
                .scheduler_sudo_password(&another_endpoint, Some("Ops/Production"))
                .unwrap_err()
                .contains("Multiple group")
        );
        assert!(window.resolve(&id, Kind::Password, &target).is_err());
        window
            .vault
            .as_mut()
            .unwrap()
            .save(input, Secret::new("ambiguous".into()))
            .unwrap();
        assert!(
            window
                .scheduler_sudo_password(&target, Some("Ops/Production"))
                .unwrap_err()
                .contains("Multiple")
        );
    }

    #[test]
    fn agent_sudo_credential_requires_exact_account_and_current_unlocked_revision() {
        use std::time::{Duration, Instant};

        use crate::terminal_logging::{Redactor, Secrets};

        let secret = r#"synthetic-"only\value"#;
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("vault.bin");
        let master = b"agent-sudo-fixture-master";
        let mut vault = Vault::create(&path, master).unwrap();
        let target = LoginTarget {
            host: "fixture.example.test".into(),
            port: 2222,
            username: "operator".into(),
        };
        let input = CredentialInput {
            label: "Reviewed sudo account".into(),
            username: target.username.clone(),
            server_pattern: "fixture.example.test:2222".into(),
            kind: CredentialKind::SudoPassword,
            ..CredentialInput::default()
        };
        vault.save(input, Secret::new(secret.into())).unwrap();
        vault
            .save(
                CredentialInput {
                    label: "Group credential".into(),
                    username: target.username.clone(),
                    server_pattern: "Production".into(),
                    kind: CredentialKind::SudoGroupPassword,
                    ..CredentialInput::default()
                },
                Secret::new("another-fixture".into()),
            )
            .unwrap();
        let mut window = VaultWindow::with_unlocked_fixture(path.clone(), vault);
        let scope = window.agent_sudo_credential(&target).unwrap();
        assert!(scope.active());
        assert_eq!(scope.label, "Reviewed sudo account");
        assert!(window.agent_sudo_line(&target, &scope).is_ok());
        let base = Secrets::default();
        base.add("synthetic");
        let previous = RedactionSecrets::with_secret(None, "prior-fixture-password").unwrap();
        let (_, learned) = window
            .agent_sudo_line_with_overlay(&target, &scope, Some(&previous))
            .unwrap();
        let redactor = Redactor::for_journal(base.seal().unwrap(), Default::default())
            .with_overlay(&learned)
            .unwrap();
        let masked = redactor
            .journal_text(
                &format!("{secret} synthetic prior-fixture-password another-fixture"),
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(masked.as_str(), "*** *** *** another-fixture");
        assert!(
            window
                .agent_sudo_credential(&LoginTarget {
                    port: 22,
                    ..target.clone()
                })
                .is_err()
        );
        assert!(
            window
                .agent_sudo_credential(&LoginTarget {
                    username: "other".into(),
                    ..target.clone()
                })
                .is_err()
        );
        let _ = window.update(Message::Lock);
        assert!(!scope.active());
        assert!(window.agent_sudo_line(&target, &scope).is_err());
        assert!(
            window
                .agent_sudo_line_with_overlay(&target, &scope, None)
                .is_err()
        );
        let vault = Vault::unlock(&path, master).unwrap();
        let reopened = VaultWindow::with_unlocked_fixture(path, vault);
        let next = reopened.agent_sudo_credential(&target).unwrap();
        assert_eq!(next.id, scope.id);
        assert!(reopened.agent_sudo_line(&target, &scope).is_err());
        assert!(reopened.agent_sudo_line(&target, &next).is_ok());
    }

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
