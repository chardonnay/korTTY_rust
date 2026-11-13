//! Saved authority for an explicitly reviewed journal CLI request.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use crate::ai_manager::{Configuration, ProfileId};
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::{self, Profile};
use crate::session_journal::topic_ai_settings;
use crate::terminal_logging::Policy;
use crate::vault_window::VaultWindow;
use crate::workspace::tab_titles::WindowSettings;

const CHANGED: &str = "The saved CLI profile, journal settings, policy, or vault changed. Prepare the question again.";

pub(super) struct Authority {
    configuration: Arc<Configuration>,
    profile_id: ProfileId,
    profile_revision: [u8; 32],
    profile: Profile,
    journal_root: PathBuf,
    preferences: topic_ai_settings::Preferences,
    policy: Policy,
    vault_change: Weak<()>,
    credentials: Option<Weak<()>>,
}

pub(super) struct Context<'a> {
    pub(super) journal_root: &'a Path,
    pub(super) preferences: &'a topic_ai_settings::Preferences,
    pub(super) policy: &'a ClipboardPolicy,
    pub(super) vault_change: Weak<()>,
    pub(super) vault: Option<&'a VaultWindow>,
}

impl Authority {
    pub(super) fn capture(
        configuration: Arc<Configuration>,
        profile_id: ProfileId,
        profile: Profile,
        context: Context<'_>,
    ) -> Result<Self, String> {
        if !allowed(context.policy) || !context.preferences.enabled || !profile.is_cli() {
            return Err(CHANGED.into());
        }
        let profile_revision = configuration.journal_question_cli_revision(profile_id)?;
        profile.require_current_policy_authority()?;
        Ok(Self {
            configuration,
            profile_id,
            profile_revision,
            profile,
            journal_root: context.journal_root.to_owned(),
            preferences: context.preferences.clone(),
            policy: context.policy.terminal_logging.clone(),
            vault_change: context.vault_change,
            credentials: context.vault.and_then(VaultWindow::credential_revision),
        })
    }

    pub(super) fn matches(
        &self,
        policy: &ClipboardPolicy,
        vault_change: &Weak<()>,
        vault: Option<&VaultWindow>,
    ) -> bool {
        self.policy_matches(policy)
            && self.vault_change.ptr_eq(vault_change)
            && match (
                &self.credentials,
                vault.and_then(VaultWindow::credential_revision),
            ) {
                (Some(reviewed), Some(current)) => reviewed.ptr_eq(&current),
                (None, None) => true,
                _ => false,
            }
    }

    pub(super) fn authorize(&self) -> Result<(), String> {
        let policy = ClipboardPolicy::load();
        if !self.policy_matches(&policy)
            || self.vault_change.strong_count() == 0
            || self
                .credentials
                .as_ref()
                .is_some_and(|token| token.strong_count() == 0)
        {
            return Err(CHANGED.into());
        }
        let root = self.configuration.saved_root().ok_or(CHANGED)?;
        if !crate::profile::root().is_ok_and(|current| current == root) {
            return Err(CHANGED.into());
        }
        let settings =
            WindowSettings::read(&root.join("window-settings.json")).map_err(|_| CHANGED)?;
        let directory = policy
            .terminal_logging
            .journal_directory()
            .map(Path::to_owned)
            .map_or_else(|| settings.session_journal.directory(root), Ok)
            .map_err(|_| CHANGED)?;
        let saved = self.configuration.reload_saved().map_err(|_| CHANGED)?;
        if directory != self.journal_root
            || settings.journal_ai != self.preferences
            || saved != *self.configuration
            || saved.journal_question_cli_revision(self.profile_id)? != self.profile_revision
            || saved.journal_question_cli_transport(self.profile_id)?.0 != self.profile
        {
            return Err(CHANGED.into());
        }
        self.profile.require_current_policy_authority()
    }

    fn policy_matches(&self, policy: &ClipboardPolicy) -> bool {
        allowed(policy)
            && self.policy.journal_directory() == policy.terminal_logging.journal_directory()
            && self.policy.equivalent_redactions(&policy.terminal_logging)
    }
}

pub(super) fn allowed(policy: &ClipboardPolicy) -> bool {
    topic_ai_settings::allowed(policy)
        && policy.terminal_logging.journal_ai_ask_allowed()
        && local_ai::cli::allowed(policy, false)
}
