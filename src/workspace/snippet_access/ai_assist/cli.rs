use std::sync::{Arc, Weak};

use iced::Element;
use iced::widget::{column, text};

use crate::ai_manager::{Configuration, ProfileRef};
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::{self, Profile};
use crate::terminal_logging::Policy;
use crate::vault_window::VaultWindow;

use super::{ActionMessage, Choice, Context, Request};

const CHANGED: &str = "The snippet, saved AI settings, vault, or masking policy changed. Review the CLI request again.";

pub(super) fn allowed(policy: &ClipboardPolicy) -> bool {
    policy.problem.is_none() && local_ai::cli::allowed(policy, false)
}

pub(super) struct Prepared {
    pub(super) ticket: u64,
    pub(super) request: local_ai::cli::Request,
}

impl Prepared {
    pub(super) fn new(ticket: u64, profile: &Profile, request: &Request) -> Result<Self, String> {
        let request = profile
            .prepare_cli_chat(&request.system, &[], &request.input)
            .map_err(local_ai::Error::message)?;
        Ok(Self { ticket, request })
    }

    pub(super) fn view(&self) -> Element<'_, ActionMessage> {
        let (label, prompt) = self.request.primary();
        let mut details = column![
            text("The approved program can access files, accounts or the network."),
            text(self.request.command_review()),
            text(label).size(16),
            text(prompt).font(iced::Font::MONOSPACE),
        ]
        .spacing(8);
        if let Some(prompt) = self.request.secondary() {
            details = details
                .push(text("Exact UTF-8 user-prompt.txt").size(16))
                .push(text(prompt).font(iced::Font::MONOSPACE));
        }
        details.into()
    }
}

pub(super) struct Review {
    // The editor panel owns this lease. Workers only hold weak references.
    draft: Arc<()>,
    scope: Arc<Scope>,
}

struct Scope {
    profile: ProfileRef,
    revision: [u8; 32],
    policy: Policy,
    vault: Weak<()>,
    credentials: Option<Weak<()>>,
}

impl Review {
    pub(super) fn capture(
        config: &Configuration,
        choice: &Choice,
        context: &Context<'_>,
    ) -> Result<Self, String> {
        let profile = choice.cli_reference()?;
        let revision = config
            .snippet_analysis_cli_revision(&profile)
            .ok_or(CHANGED)?;
        let review = Self {
            draft: Arc::new(()),
            scope: Arc::new(Scope {
                profile,
                revision,
                policy: context.policy.terminal_logging.clone(),
                vault: context.vault_revision.clone(),
                credentials: context.vault.and_then(VaultWindow::credential_revision),
            }),
        };
        review.require_current(config, context)?;
        Ok(review)
    }

    pub(super) fn revision(&self) -> [u8; 32] {
        self.scope.revision
    }

    pub(super) fn matches(&self, context: &Context<'_>) -> bool {
        self.scope.policy_current(context.policy)
            && self.scope.vault.ptr_eq(&context.vault_revision)
            && match (
                &self.scope.credentials,
                context.vault.and_then(VaultWindow::credential_revision),
            ) {
                (Some(reviewed), Some(current)) => reviewed.ptr_eq(&current),
                (None, None) => true,
                _ => false,
            }
    }

    pub(super) fn require_current(
        &self,
        config: &Configuration,
        context: &Context<'_>,
    ) -> Result<(), String> {
        if !self.matches(context) {
            return Err(CHANGED.into());
        }
        self.scope.authorize(config)
    }

    pub(super) fn guard(
        &self,
        config: Arc<Configuration>,
    ) -> impl Fn() -> Result<(), String> + Send + 'static {
        let draft = Arc::downgrade(&self.draft);
        let scope = Arc::clone(&self.scope);
        move || {
            if draft.strong_count() == 0 {
                return Err(CHANGED.into());
            }
            scope.authorize(&config)?;
            if draft.strong_count() == 0 {
                return Err(CHANGED.into());
            }
            Ok(())
        }
    }
}

impl Scope {
    fn policy_current(&self, policy: &ClipboardPolicy) -> bool {
        allowed(policy)
            && self.policy.equivalent_redactions(&policy.terminal_logging)
            && self.vault.strong_count() > 0
            && self
                .credentials
                .as_ref()
                .is_none_or(|revision| revision.strong_count() > 0)
    }

    fn authorize(&self, config: &Configuration) -> Result<(), String> {
        if !self.policy_current(&ClipboardPolicy::load())
            || config
                .saved_root()
                .is_none_or(|root| !crate::profile::root().is_ok_and(|current| current == root))
        {
            return Err(CHANGED.into());
        }
        let saved = config.reload_saved().map_err(|_| CHANGED)?;
        if !saved.enabled()
            || &saved != config
            || saved.snippet_analysis_cli_revision(&self.profile) != Some(self.revision)
        {
            return Err(CHANGED.into());
        }
        Ok(())
    }
}
