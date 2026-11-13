//! Authority for one reviewed remote journal answer.

use std::sync::{Arc, Weak};

use zeroize::Zeroizing;

use crate::ai_manager::{Configuration, ProfileId};
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::Profile;
use crate::session_journal::topic_ai_settings;
use crate::terminal_logging::Policy;
use crate::vault_window::VaultWindow;

const CHANGED: &str = "The saved HTTPS profile, journal policy, or credentials changed. Review the journal question again.";

#[derive(Clone)]
pub(crate) struct HttpsAuthority {
    configuration: Arc<Configuration>,
    profile: Profile,
    policy: Policy,
    credential: Option<Zeroizing<String>>,
    vault: Option<Weak<()>>,
}

impl HttpsAuthority {
    pub(crate) fn capture(
        configuration: Arc<Configuration>,
        id: ProfileId,
        profile: &Profile,
        policy: &ClipboardPolicy,
        vault: Option<&VaultWindow>,
    ) -> Result<Self, String> {
        if !Self::allowed(policy) || configuration.saved_root().is_none() {
            return Err(CHANGED.into());
        }
        let (configured, _) = configuration.journal_question_https_transport(id)?;
        if &configured != profile || !profile.is_remote() {
            return Err(CHANGED.into());
        }
        profile.require_current_policy_authority()?;
        Ok(Self {
            configuration,
            profile: configured,
            policy: policy.terminal_logging.clone(),
            credential: profile.completion_secret(),
            vault: vault.and_then(VaultWindow::credential_revision),
        })
    }

    pub(crate) fn completion_secret(&self) -> Option<&str> {
        self.credential.as_ref().map(|secret| secret.as_str())
    }

    pub(crate) fn matches(&self, policy: &ClipboardPolicy, vault: Option<&VaultWindow>) -> bool {
        self.policy_matches(policy)
            && self.credential_matches()
            && match (
                &self.vault,
                vault.and_then(VaultWindow::credential_revision),
            ) {
                (Some(reviewed), Some(current)) => Weak::ptr_eq(reviewed, &current),
                (None, None) => true,
                _ => false,
            }
    }

    pub(super) fn profile(&self) -> &Profile {
        &self.profile
    }

    pub(super) fn authorize(&self) -> Result<(), String> {
        let saved = self.configuration.reload_saved().map_err(|_| CHANGED)?;
        if saved == *self.configuration
            && self.profile.require_current_policy_authority().is_ok()
            && self.policy_matches(&ClipboardPolicy::load())
            && self.credential_matches()
            && self
                .vault
                .as_ref()
                .is_none_or(|vault| vault.strong_count() > 0)
        {
            Ok(())
        } else {
            Err(CHANGED.into())
        }
    }

    fn allowed(policy: &ClipboardPolicy) -> bool {
        topic_ai_settings::allowed(policy) && policy.terminal_logging.journal_ai_ask_allowed()
    }

    fn policy_matches(&self, policy: &ClipboardPolicy) -> bool {
        Self::allowed(policy)
            && self.policy.journal_directory() == policy.terminal_logging.journal_directory()
            && self.policy.equivalent_redactions(&policy.terminal_logging)
    }

    fn credential_matches(&self) -> bool {
        self.profile.completion_secret() == self.credential
    }
}
