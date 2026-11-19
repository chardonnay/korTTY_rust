//! Session approval for repeated requests from the snippet completion list.

use super::*;

pub(in crate::workspace::snippet_access) struct AutomaticCompletion {
    config: Arc<Configuration>,
    choice: Choice,
    transport: Profile,
    cli_revision: Option<[u8; 32]>,
    connection_profile: Option<crate::ai_manager::ProfileRef>,
    instructions: String,
    skills: Vec<u64>,
    policy: Policy,
    vault: Weak<()>,
    credentials: Option<Weak<()>>,
    lease: Arc<()>,
}

impl AutomaticCompletion {
    pub(super) fn from_review(panel: &Panel, context: &Context<'_>) -> Option<Self> {
        let Phase::Review(review) = &panel.phase else {
            return None;
        };
        panel.recipe.completion.as_ref()?;
        Some(Self {
            config: review.config.clone(),
            choice: review.choice.clone(),
            transport: review.transport.clone(),
            cli_revision: cli_revision(review),
            connection_profile: panel.source.connection_profile.clone(),
            instructions: panel.recipe.instructions.clone(),
            skills: panel.recipe.skills.clone(),
            policy: context.policy.terminal_logging.clone(),
            vault: context.vault_revision.clone(),
            credentials: context.vault.and_then(VaultWindow::credential_revision),
            lease: Arc::new(()),
        })
    }

    pub(super) fn matches(&self, config: &Arc<Configuration>, context: &Context<'_>) -> bool {
        Arc::ptr_eq(&self.config, config)
            && context.policy.ai_allowed
            && context.policy.problem.is_none()
            && (context.additional_instructions || self.instructions.is_empty())
            && self
                .policy
                .equivalent_redactions(&context.policy.terminal_logging)
            && context.vault_revision.ptr_eq(&self.vault)
            && match (
                &self.credentials,
                context.vault.and_then(VaultWindow::credential_revision),
            ) {
                (Some(reviewed), Some(current)) => reviewed.ptr_eq(&current),
                (None, None) => true,
                _ => false,
            }
    }

    pub(super) fn panel(&self, editor: &dyn Editor, inline: bool) -> Result<Option<Panel>, String> {
        let Ok(source) = editor.assist_snapshot() else {
            return Ok(None);
        };
        let Some(target) = editor.completion_target(&source, inline) else {
            return Ok(None);
        };
        if source.connection_profile != self.connection_profile {
            return Err("The snippet connection profile changed. Review a new request.".into());
        }
        Ok(Some(Panel {
            source,
            language_mix: None,
            staged_migration: None,
            recipe: Recipe {
                action: Action::Complete,
                completion: Some(target),
                profile: Some(self.choice.id),
                instructions: self.instructions.clone(),
                skills: self.skills.clone(),
                approved_transport: Some(self.transport.clone()),
                approved_cli_revision: self.cli_revision,
                ..Recipe::default()
            },
            explicit_profile: true,
            profiles: Vec::new(),
            skills: Vec::new(),
            phase: Phase::Configure,
            status: None,
        }))
    }

    pub(super) fn check_review(&self, panel: &Panel) -> Result<(), String> {
        let Phase::Review(review) = &panel.phase else {
            return Err("The automatic completion request could not be prepared.".into());
        };
        if review.choice != self.choice
            || review.transport != self.transport
            || cli_revision(review) != self.cli_revision
            || panel.recipe.skills != self.skills
        {
            return Err(
                "The approved completion profile or skills changed. Review a new request.".into(),
            );
        }
        Ok(())
    }

    pub(super) fn guard(&self) -> impl Fn() -> Result<(), String> + Send + use<> {
        let lease = Arc::downgrade(&self.lease);
        let policy = self.policy.clone();
        let vault = self.vault.clone();
        let credentials = self.credentials.clone();
        let config = self.config.clone();
        move || {
            let installed = ClipboardPolicy::load();
            if lease.strong_count() == 0
                || vault.strong_count() == 0
                || credentials
                    .as_ref()
                    .is_some_and(|value| value.strong_count() == 0)
                || !installed.ai_allowed
                || installed.problem.is_some()
                || !policy.equivalent_redactions(&installed.terminal_logging)
                || (config.saved_root().is_some()
                    && !config
                        .reload_saved()
                        .is_ok_and(|current| current == *config))
            {
                return Err(
                    "The session completion approval changed. Review a new request.".into(),
                );
            }
            Ok(())
        }
    }
}

fn cli_revision(review: &Review) -> Option<[u8; 32]> {
    match &review.outbound {
        Some(Outbound::Cli(cli)) => Some(cli.revision()),
        _ => None,
    }
}
