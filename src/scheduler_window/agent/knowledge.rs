//! Exact masked knowledge retained by a scheduled model approval.

use std::path::Path;
use std::sync::{Arc, Weak};

use iced::Task;
use serde::{Deserialize, Serialize};

use crate::ai_manager::{Configuration, KnowledgeAuthority, ProfileRef, RetrievedKnowledge};
use crate::workspace::terminal_agent::knowledge_review::Review;

use super::{Classification, Message, Model, SchedulerView, SchedulerWindow, generation_grant};

const CHANGED: &str =
    "The scheduled knowledge store or its sources changed. Review this job again.";

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Grant {
    authority: KnowledgeAuthority,
    context: String,
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ScheduledKnowledge([redacted])")
    }
}

impl Grant {
    pub(crate) fn capture(retrieved: &RetrievedKnowledge) -> Result<Self, String> {
        let grant = Self {
            authority: retrieved.authority()?,
            context: retrieved.context().to_owned(),
        };
        if !grant.valid() {
            return Err("No usable knowledge excerpts were retrieved.".into());
        }
        Ok(grant)
    }

    pub(super) fn valid(&self) -> bool {
        !self.context.trim().is_empty()
            && self.context.len() <= 4096
            && self
                .context
                .chars()
                .all(crate::session_journal::is_display_character)
    }

    pub(crate) fn context(&self) -> &str {
        &self.context
    }

    #[cfg(test)]
    pub(super) fn fixture(snapshot: [u8; 32], context: &str) -> Arc<Self> {
        Arc::new(Self {
            authority: KnowledgeAuthority::fixture(snapshot),
            context: context.into(),
        })
    }

    pub(crate) fn require_current(
        &self,
        config: &Configuration,
        profile: &ProfileRef,
        root: &Path,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        let profile = config.resolve_profile_reference(profile).ok_or(CHANGED)?;
        if config.enabled()
            && config
                .autonomous_knowledge(profile, root)
                .iter()
                .any(|plan| plan.require_authority(&self.authority, &check).is_ok())
        {
            return check();
        }
        Err(CHANGED.into())
    }
}

impl SchedulerView {
    pub(crate) fn proposal_knowledge_current(&self, preview: &str) -> bool {
        self.skill_preview_closed(preview)
            && self
                .proposal_review()
                .is_some_and(|(_, model)| model.knowledge.is_some())
    }

    pub(crate) fn proposal_knowledge_mut(&mut self) -> Option<&mut Review> {
        self.preview_model_mut()?.0.knowledge.as_mut()
    }

    pub(crate) fn proposal_knowledge_vault_revision(&self) -> Result<Option<Weak<()>>, String> {
        let revision = self
            .proposal_review()
            .and_then(|(_, model)| model.knowledge_vault_revision.clone());
        if revision
            .as_ref()
            .is_some_and(|revision| revision.strong_count() == 0)
        {
            return Err(
                "The credential vault changed. Retrieve and review the knowledge again.".into(),
            );
        }
        Ok(revision)
    }

    pub(crate) fn bind_proposal_knowledge_vault(&mut self, revision: Option<Weak<()>>) {
        if let Some((model, _)) = self.preview_model_mut() {
            model.knowledge_vault_revision = revision;
        }
    }
}

impl SchedulerWindow {
    pub(crate) fn attach_proposal_knowledge(&self, view: &mut SchedulerView) {
        let Some((review, _)) = view.proposal_review() else {
            return;
        };
        let reference = review.approval.profile.id.clone();
        let Some((model, _)) = view.preview_model_mut() else {
            return;
        };
        let Some(setup) = model
            .skills
            .as_ref()
            .and_then(|skills| skills.setup.as_ref())
        else {
            return;
        };
        let Some(prompt) = model.prompts.first() else {
            return;
        };
        let Some(root) = setup.config.saved_root() else {
            return;
        };
        let Some(question) = prompt.masked_question() else {
            return;
        };
        let Some(profile) = setup.config.resolve_profile_reference(&reference) else {
            return;
        };
        let plans = setup
            .config
            .autonomous_knowledge(profile, root)
            .into_iter()
            .filter(|plan| plan.supports_standing_review())
            .map(|plan| plan.for_standing_review())
            .collect();
        model.knowledge = Review::from_plans(plans, &question, prompt.context_limit);
    }

    pub(crate) fn replace_proposal_knowledge(
        &self,
        view: &mut SchedulerView,
        knowledge: Option<Arc<Grant>>,
    ) -> Result<Task<Message>, String> {
        let (review, _) = view
            .proposal_review()
            .ok_or("The model review is unavailable.")?;
        self.agent_review_current(view, review)?;
        let review = view.agent_review.as_mut().expect("checked review");
        let standing = review.standing();
        let firing = review.per_firing();
        let Model {
            prompts, skills, ..
        } = review.model.as_mut().expect("checked model");
        let skills = skills
            .as_mut()
            .filter(|skills| matches!(skills.classification, Classification::Closed))
            .ok_or("Finish the skill review before changing knowledge excerpts.")?;
        let updated = prompts
            .iter()
            .map(|prompt| {
                (**prompt)
                    .clone()
                    .with_knowledge(knowledge.clone(), standing)
            })
            .collect::<Result<Vec<_>, _>>()?;
        for prompt in &updated {
            prompt.cli_review_input(standing)?;
        }
        let generation = firing
            .then(|| generation_grant(&updated, &review.approval.targets))
            .transpose()?;
        let advisory = review
            .approval
            .advisory
            .as_ref()
            .map(|grant| {
                let pins = grant
                    .reviewed_pins()
                    .ok_or("The reviewed read-only skill assignments are unavailable.")?;
                grant.clone().with_reviewed_prompts(
                    &updated,
                    &review.approval.targets,
                    pins.to_vec(),
                )
            })
            .transpose()?;
        skills.renew_preview()?;
        *prompts = updated.into_iter().map(Arc::new).collect();
        review.approval.generation = generation;
        review.approval.advisory = advisory;
        review.approval.knowledge = if standing { knowledge } else { None };
        review.approval.comparison = None;
        skills.proposal_quota(prompts, standing)
    }
}
