//! Saved text-only authority for individual and automatic reviewed summary passes.

use std::io;
use std::path::Path;
use std::sync::{Arc, Weak};

use zeroize::Zeroizing;

use crate::ai_manager::Configuration;
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::Profile;
use crate::session_journal::summary_job::Mode;
use crate::session_journal::summary_queue::{ProfileChoice, Queue, Receipt};
use crate::session_journal::topic_ai_settings;
use crate::terminal_logging::{Redactor, Secrets};
use crate::vault_window::{JournalQuestionSecrets, VaultWindow};
use crate::workspace::tab_titles::WindowSettings;

use super::mask;

pub(super) const CHANGED: &str = "The HTTPS summary approval is unavailable or has changed. Review the journal pass or automatic summary settings again.";
pub(super) const CLI_CHANGED: &str = "The CLI summary approval is unavailable or has changed. Review the journal pass or automatic summary settings again.";

pub(super) fn resolve(
    configuration: &Configuration,
    choice: &ProfileChoice,
    root: &Path,
    policy: &ClipboardPolicy,
) -> Option<Profile> {
    let binding = choice.binding()?;
    if !topic_ai_settings::allowed(policy)
        || binding.root != root
        || Some(binding.redaction_revision) != policy.terminal_logging.redaction_revision()
    {
        return None;
    }
    let id = configuration.resolve_profile_reference(&choice.id)?;
    let (profile, revision) = if choice.cli.is_some() {
        if !crate::local_ai::cli::allowed(policy, false) {
            return None;
        }
        (
            configuration.journal_summary_cli_transport(id).ok()?.0,
            configuration.journal_summary_cli_revision(id).ok()?,
        )
    } else {
        (
            configuration.journal_question_https_transport(id).ok()?.0,
            configuration.journal_summary_https_revision(id).ok()?,
        )
    };
    if revision != binding.profile_revision {
        return None;
    }
    Some(profile)
}

pub(super) struct Authority {
    configuration: Arc<Configuration>,
    receipt: Receipt,
    profile: Profile,
    credential: Option<Zeroizing<String>>,
    vault: Option<Weak<()>>,
}

impl Authority {
    pub(super) fn new(
        configuration: Arc<Configuration>,
        receipt: Receipt,
        profile: Profile,
        vault: Option<&VaultWindow>,
    ) -> Self {
        let credential = profile.completion_secret();
        Self {
            configuration,
            receipt,
            profile,
            credential,
            vault: vault.and_then(VaultWindow::credential_revision),
        }
    }

    pub(super) fn credential(&self) -> Option<Zeroizing<String>> {
        self.credential.clone()
    }

    pub(super) fn authorize(&self) -> Result<(), String> {
        let changed = if self.receipt.cli() {
            CLI_CHANGED
        } else {
            CHANGED
        };
        let root = self.configuration.saved_root().ok_or(changed)?;
        let queue = Queue::load(root).map_err(|_| changed)?;
        if !queue.authorizes_reviewed(&self.receipt)
            || self.profile.completion_secret() != self.credential
            || self
                .vault
                .as_ref()
                .is_some_and(|vault| vault.strong_count() == 0)
        {
            return Err(changed.into());
        }
        let settings =
            WindowSettings::read(&root.join("window-settings.json")).map_err(|_| changed)?;
        let policy = ClipboardPolicy::load();
        if !settings.journal_ai.enabled
            || self.receipt.mode == Mode::Automation && !policy.automation_journal_allowed()
        {
            return Err(changed.into());
        }
        let directory = policy
            .terminal_logging
            .journal_directory()
            .map(Path::to_owned)
            .map_or_else(|| settings.session_journal.directory(root), Ok)
            .map_err(|_| changed)?;
        let configuration = self.configuration.reload_saved().map_err(|_| changed)?;
        let choice = self.receipt.profile.as_ref().ok_or(changed)?;
        if resolve(&configuration, choice, &directory, &policy).as_ref() != Some(&self.profile) {
            return Err(changed.into());
        }
        self.receipt
            .source
            .source()
            .require_summary_location()
            .map_err(|_| changed.into())
    }
}

pub(super) fn vault_matches(reviewed: Option<&Weak<()>>, vault: Option<&VaultWindow>) -> bool {
    match (reviewed, vault.and_then(VaultWindow::credential_revision)) {
        (Some(reviewed), Some(current)) => Weak::ptr_eq(reviewed, &current),
        (None, None) => true,
        _ => false,
    }
}

#[derive(Clone)]
pub(super) struct Mask {
    pub(super) vault: JournalQuestionSecrets,
    pub(super) credential: Option<Zeroizing<String>>,
    pub(super) policy: crate::terminal_logging::Policy,
}

impl Mask {
    pub(super) fn apply(&self, value: &str, capture: &Redactor) -> io::Result<String> {
        let secrets = Secrets::default();
        capture.add_known_secrets(&secrets);
        if let Some(credential) = &self.credential {
            secrets.add(credential);
        }
        self.vault.add_matches(value, &secrets);
        let redactor = Redactor::for_journal(secrets.seal()?, self.policy.clone());
        mask::prompt_counted(&redactor, value)
            .map(|(text, _)| text)
            .map_err(io::Error::other)
    }
}
