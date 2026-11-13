//! Retrieval approval for an exact masked Scheduler question.

use std::sync::Arc;

use iced::{Task, window};

use crate::scheduler_window::{self, agent::knowledge::Grant};
use crate::workspace::terminal_agent::knowledge_review::Message as KnowledgeMessage;

use super::{ConnectionStore, Message, Workspace, profile, proposal_skills};

impl Workspace {
    pub(super) fn update_scheduler_proposal_knowledge(
        &mut self,
        id: window::Id,
        ticket: String,
        message: KnowledgeMessage,
    ) -> Task<Message> {
        if !self
            .scheduler_views
            .get(&id)
            .is_some_and(|view| view.proposal_knowledge_current(&ticket))
        {
            return Task::none();
        }
        match self.apply_scheduler_proposal_knowledge(id, ticket, message) {
            Ok(task) => task,
            Err(error) => {
                if let Some(view) = self.scheduler_views.get_mut(&id) {
                    view.report_status(error);
                }
                Task::none()
            }
        }
    }

    fn apply_scheduler_proposal_knowledge(
        &mut self,
        id: window::Id,
        ticket: String,
        message: KnowledgeMessage,
    ) -> Result<Task<Message>, String> {
        match message {
            KnowledgeMessage::Retrieve => {
                if !self.scheduler_proposal_current(id, None)? {
                    return Ok(Task::none());
                }
                let token = self.allocate();
                let vault_revision = self.vault_change_revision();
                let scheduler = self
                    .scheduler
                    .as_ref()
                    .ok_or("The scheduler is unavailable.")?;
                let view = self
                    .scheduler_views
                    .get(&id)
                    .expect("checked Scheduler view");
                let authority = scheduler
                    .proposal_authority(view)?
                    .ok_or("The model request is unavailable.")?;
                let prompt = Arc::clone(&authority.prompts[0]);
                let guard =
                    proposal_skills::dispatch_guard(&authority, Some(vault_revision.clone()))?;
                let store = profile::connections_path()
                    .and_then(ConnectionStore::open)
                    .map_err(|_| "The saved SSH connections are unavailable.")?;
                let (secrets, _) = self.firing_secrets(authority.targets, &store)?;
                let redactor = crate::terminal_logging::Redactor::for_journal(
                    secrets,
                    prompt.redaction_policy().clone(),
                );
                let vault = self.vault.as_ref().map_or_else(
                    Default::default,
                    crate::vault_window::VaultWindow::journal_question_secrets,
                );
                let mask = move |text: &str| {
                    let secrets = crate::terminal_logging::Secrets::default();
                    vault.add_matches(text, &secrets);
                    let scoped = secrets
                        .seal()
                        .and_then(|secrets| redactor.with_overlay(&secrets))
                        .map_err(|error| error.to_string())?;
                    prompt.knowledge_mask_with(&scoped, text)
                };
                let view = self
                    .scheduler_views
                    .get_mut(&id)
                    .expect("checked Scheduler view");
                let review = view
                    .proposal_knowledge_mut()
                    .expect("checked knowledge review");
                if mask(review.query())? != review.query() {
                    return Err(
                        "The knowledge search question no longer matches the reviewed mask.".into(),
                    );
                }
                let task =
                    review.start(token, self.vault.as_ref(), mask, guard, move |message| {
                        Message::Scheduler(
                            id,
                            scheduler_window::Message::ProposalKnowledge(ticket, message),
                        )
                    })?;
                if review.busy() {
                    view.bind_proposal_knowledge_vault(Some(vault_revision));
                }
                Ok(task)
            }
            KnowledgeMessage::Select(index) => {
                let scheduler = self
                    .scheduler
                    .as_ref()
                    .ok_or("The scheduler is unavailable.")?;
                let view = self
                    .scheduler_views
                    .get_mut(&id)
                    .expect("checked Scheduler view");
                if !view
                    .proposal_knowledge_mut()
                    .expect("checked knowledge review")
                    .select(index)
                {
                    return Ok(Task::none());
                }
                view.bind_proposal_knowledge_vault(None);
                match scheduler.replace_proposal_knowledge(view, None) {
                    Ok(task) => Ok(task.map(move |message| Message::Scheduler(id, message))),
                    Err(error) => {
                        view.discard_proposal_review();
                        Err(error)
                    }
                }
            }
            KnowledgeMessage::Cancel => {
                let view = self
                    .scheduler_views
                    .get_mut(&id)
                    .expect("checked Scheduler view");
                if view
                    .proposal_knowledge_mut()
                    .expect("checked knowledge review")
                    .cancel()
                {
                    view.bind_proposal_knowledge_vault(None);
                }
                Ok(Task::none())
            }
            KnowledgeMessage::Finished(token, result) => {
                let current = self.scheduler_proposal_current(id, None);
                let scheduler = self
                    .scheduler
                    .as_ref()
                    .ok_or("The scheduler is unavailable.")?;
                let view = self
                    .scheduler_views
                    .get_mut(&id)
                    .expect("checked Scheduler view");
                let Some(retrieved) = view
                    .proposal_knowledge_mut()
                    .expect("checked knowledge review")
                    .finish(token, result)
                else {
                    return Ok(Task::none());
                };
                let applied = (|| {
                    if !current? {
                        return Err("The reviewed model request changed.".into());
                    }
                    let retrieved = retrieved?;
                    let grant = Arc::new(Grant::capture(&retrieved)?);
                    let task = scheduler.replace_proposal_knowledge(view, Some(grant))?;
                    view.proposal_knowledge_mut()
                        .expect("checked knowledge review")
                        .accept(retrieved);
                    Ok(task.map(move |message| Message::Scheduler(id, message)))
                })();
                if applied.is_err() {
                    view.bind_proposal_knowledge_vault(None);
                }
                applied
            }
        }
    }
}

pub(super) fn require_current_vault_mask(
    vault: Option<&crate::vault_window::VaultWindow>,
    knowledge: &Grant,
) -> Result<(), &'static str> {
    if let Some(vault) = vault {
        let secrets = crate::terminal_logging::Secrets::default();
        vault
            .journal_question_secrets()
            .add_matches(knowledge.context(), &secrets);
        let secrets = secrets
            .seal()
            .map_err(|_| "The knowledge mask could not be verified.")?;
        let redactor = crate::terminal_logging::Redactor::for_journal(secrets, Default::default());
        if redactor.contains_known_secret(knowledge.context()) {
            return Err("The credential vault changed the knowledge mask. Review this job again.");
        }
    }
    Ok(())
}
