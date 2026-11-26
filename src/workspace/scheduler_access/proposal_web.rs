//! Explicit web searches and independently reviewed excerpts in Scheduler.

use std::sync::Arc;

use iced::{Task, window};

use crate::ai_manager::Configuration;
use crate::scheduler_window;
use crate::terminal_logging::{Redactor, Secrets};
use crate::vault_window::{JournalQuestionSecrets, VaultWindow};
use crate::workspace::terminal_ai::{mask, web};

use super::{ConnectionStore, Message, Workspace, profile, proposal_skills};

impl Workspace {
    pub(super) fn scheduler_web_allowed(&self) -> bool {
        self.ai_features_enabled() && self.clipboard.policy.ai_web_search_allowed
    }

    pub(super) fn require_public_scheduler_topic(
        &self,
        topic: &str,
        model: &crate::local_ai::Profile,
        scope: &web::Scope,
        include_page: bool,
    ) -> Result<(), String> {
        let vault = vault_secrets(self.vault.as_ref());
        let empty = Secrets::default()
            .seal()
            .map_err(|_| "The credential vault mask could not be verified.")?;
        let redactor = Redactor::for_journal(empty, self.clipboard.policy.terminal_logging.clone());
        let local = model.completion_secret();
        let search = scope
            .search_credential_env()
            .and_then(|name| scope.provider_credential(name));
        let page = scope
            .page_credential_env()
            .filter(|_| include_page)
            .and_then(|name| scope.provider_credential(name));
        if mask::prompt(&vault_overlay(&vault, &redactor, topic)?, topic)? != topic
            || local.iter().chain(&search).chain(&page).any(|secret| {
                crate::local_ai::credential_echo(topic, secret)
                    || percent_encoding::percent_decode_str(topic)
                        .decode_utf8()
                        .is_ok_and(|decoded| decoded.contains(secret.as_str()))
            })
        {
            return Err("Remove any private or masked text from the public research topic.".into());
        }
        Ok(())
    }

    pub(super) fn require_saved_public_scheduler_topic(
        &self,
        grant: &crate::scheduler_window::agent::web_lookup::Grant,
    ) -> Result<(), String> {
        let config = Configuration::load().map_err(|_| "The saved AI profile is unavailable.")?;
        let id = config
            .resolve_profile_reference(grant.profile())
            .ok_or("The saved AI profile is unavailable.")?;
        let scope = web::Scope::new(Some(&config), id, self.scheduler_web_allowed())
            .ok_or("The saved web search settings are unavailable.")?
            .with_vault(self.vault.as_ref());
        scope.require_unattended_profile(grant, &self.clipboard.policy.terminal_logging)?;
        let (model, _, _) = config
            .web_search_profile(id, false)
            .ok_or("The saved web search settings are unavailable.")?;
        self.require_public_scheduler_topic(
            grant.public_topic(),
            &model,
            &scope,
            grant.allows_followups() || grant.allows_round_page(),
        )
    }

    pub(super) fn revalidate_scheduler_web_reviews(&mut self) {
        let allowed = self.scheduler_web_allowed();
        for view in self.scheduler_views.values_mut() {
            let Some(scope) = view.active_proposal_web_scope() else {
                continue;
            };
            let current = if allowed {
                scope
                    .require_current()
                    .and(view.proposal_web_vault_revision().map(|_| ()))
            } else {
                Err("Managed policy blocks use of the reviewed web search.".into())
            };
            if let Err(error) = current {
                view.discard_proposal_review();
                view.report_status(error);
            }
        }
    }

    pub(super) fn update_scheduler_proposal_web(
        &mut self,
        id: window::Id,
        preview: String,
        message: web::Message,
    ) -> Task<Message> {
        let Some(scope) = self
            .scheduler_views
            .get(&id)
            .and_then(|view| view.proposal_web_scope(&preview))
            .cloned()
        else {
            return Task::none();
        };
        match self.apply_scheduler_proposal_web(id, preview, message, scope) {
            Ok(task) => task,
            Err(error) => {
                if let Some(view) = self.scheduler_views.get_mut(&id) {
                    view.report_status(error);
                }
                Task::none()
            }
        }
    }

    fn apply_scheduler_proposal_web(
        &mut self,
        id: window::Id,
        preview: String,
        message: web::Message,
        scope: web::Scope,
    ) -> Result<Task<Message>, String> {
        if matches!(message, web::Message::Close) {
            let scheduler = self
                .scheduler
                .as_ref()
                .ok_or("The scheduler is unavailable.")?;
            let view = self
                .scheduler_views
                .get_mut(&id)
                .expect("checked Scheduler review");
            let state = view.proposal_web_state_mut().expect("checked web review");
            let had_evidence = state.included().is_some();
            *state = web::State::default();
            view.bind_proposal_web_vault(None);
            if had_evidence {
                return scheduler
                    .replace_proposal_web(view, None)
                    .inspect_err(|_| view.discard_proposal_review())
                    .map(|task| task.map(move |message| Message::Scheduler(id, message)));
            }
            return Ok(Task::none());
        }
        if !self.scheduler_web_allowed() {
            return Ok(self.discard_changed_scheduler_proposal(
                id,
                "Managed policy blocks web search in Scheduler.".into(),
            ));
        }
        if !self.scheduler_proposal_current(id, None)? {
            return Ok(Task::none());
        }
        scope.require_current()?;
        let vault_revision = self.vault_change_revision();
        let scheduler = self
            .scheduler
            .as_ref()
            .ok_or("The scheduler is unavailable.")?;
        let view = self
            .scheduler_views
            .get(&id)
            .expect("checked Scheduler review");
        let authority = scheduler
            .proposal_authority(view)?
            .ok_or("The model request is unavailable.")?;
        let guard = proposal_skills::dispatch_guard(&authority, Some(vault_revision.clone()))?;
        let prompt = Arc::clone(&authority.prompts[0]);
        let store = profile::connections_path()
            .and_then(ConnectionStore::open)
            .map_err(|_| "The saved SSH connections are unavailable.")?;
        let (secrets, _) = self.firing_secrets(authority.targets, &store)?;
        let redactor = Redactor::for_journal(secrets, prompt.redaction_policy().clone());
        let vault = vault_secrets(self.vault.as_ref());
        let mask = move |text: &str| {
            let redactor = vault_overlay(&vault, &redactor, text)?;
            prompt.knowledge_mask_with(&redactor, text)
        };
        let send = matches!(message, web::Message::Send);
        let include = matches!(message, web::Message::Include);
        let token = send.then(|| self.allocate());
        let view = self
            .scheduler_views
            .get_mut(&id)
            .expect("checked Scheduler review");
        let state = view.proposal_web_state_mut().expect("checked web review");
        let task = state.update(message, scope, mask, token, guard, move |message| {
            Message::Scheduler(id, scheduler_window::Message::ProposalWeb(preview, message))
        })?;
        if include && let Some((scope, results)) = state.included() {
            let scheduler = self.scheduler.as_ref().expect("checked Scheduler model");
            let updated = web::Evidence::new(scope, results)
                .and_then(|evidence| scheduler.replace_proposal_web(view, Some(Arc::new(evidence))))
                .inspect_err(|_| view.discard_proposal_review())?;
            return Ok(Task::batch([
                task,
                updated.map(move |message| Message::Scheduler(id, message)),
            ]));
        }
        if send && state.active() {
            view.bind_proposal_web_vault(Some(vault_revision));
        }
        Ok(task)
    }
}

pub(super) fn require_current_vault_mask(
    vault: Option<&VaultWindow>,
    evidence: &web::Evidence,
) -> Result<(), &'static str> {
    let vault = vault_secrets(vault);
    let empty = Secrets::default()
        .seal()
        .map_err(|_| "The credential vault mask could not be verified.")?;
    let redactor = Redactor::for_journal(empty, Default::default());
    evidence
        .require_mask(|text| mask::prompt(&vault_overlay(&vault, &redactor, text)?, text))
        .map_err(|_| "The approved web excerpts no longer match the credential vault.")
}

pub(super) fn vault_secrets(vault: Option<&VaultWindow>) -> JournalQuestionSecrets {
    vault.map_or_else(Default::default, VaultWindow::journal_question_secrets)
}

pub(super) fn vault_overlay(
    vault: &JournalQuestionSecrets,
    redactor: &Redactor,
    text: &str,
) -> Result<Redactor, String> {
    let secrets = Secrets::default();
    vault.add_matches(text, &secrets);
    secrets
        .seal()
        .and_then(|secrets| redactor.with_overlay(&secrets))
        .map_err(|_| "The credential vault mask could not be verified.".into())
}
