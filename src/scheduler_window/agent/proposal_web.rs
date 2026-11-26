//! Reviewed web evidence for one-use and standing model requests.

use std::sync::{Arc, Weak};

use iced::Task;

use crate::workspace::terminal_ai::web;

use super::{
    Message, Model, SchedulerView, SchedulerWindow, scope, scope_with_fresh_web,
    scope_with_web_lookup, web_lookup,
};

const ENVIRONMENT_REVIEW_REQUIRED: &str =
    "Research after an SSH probe requires a Swarm with a reviewed fixed probe.";
const ROUND_REVIEW_REQUIRED: &str =
    "Research after a command batch requires a guided iterative Swarm review.";
const ROUND_PAGE_READER_REQUIRED: &str =
    "Page requests after command batches require a configured webpage reader.";
const MODEL_TOOLS_REQUIRED: &str =
    "Reviewed model web tools require Agent or iterative Swarm and a direct Tavily profile.";

enum OptionKind {
    Followups,
    Environment,
    Rounds,
    RoundPages,
    ModelTools,
}

pub(in crate::scheduler_window) struct Review {
    pub(in crate::scheduler_window) state: web::State,
    scope: web::Scope,
    vault_revision: Option<Weak<()>>,
}

impl Model {
    pub(in crate::scheduler_window) fn web_active(&self) -> bool {
        self.web.as_ref().is_some_and(|web| web.state.active())
    }
}

impl super::Review {
    pub(super) fn require_fresh_web_scope(&self) -> Result<(), String> {
        if !self.supports_fresh_web() || self.approval.web.is_some() {
            return Err("This workflow cannot use a fresh web lookup.".into());
        }
        let model = self
            .model
            .as_ref()
            .ok_or("The model review is unavailable.")?;
        let scope = &model
            .web
            .as_ref()
            .ok_or(
                "Enable web search for this AI profile or clear the fresh web lookup requirement.",
            )?
            .scope;
        scope.require_current()?;
        if let Some(grant) = self.approval.web_lookup() {
            if grant.allows_environment_lookup() && !self.supports_unattended_web_environment() {
                return Err(ENVIRONMENT_REVIEW_REQUIRED.into());
            }
            if grant.allows_round_lookup() && !self.supports_unattended_web_rounds() {
                return Err(ROUND_REVIEW_REQUIRED.into());
            }
            if grant.allows_round_page() && scope.page_review().is_none() {
                return Err(ROUND_PAGE_READER_REQUIRED.into());
            }
            if grant.allows_model_tools() && !self.supports_unattended_web_model_tools() {
                return Err(MODEL_TOOLS_REQUIRED.into());
            }
            let prompt = model
                .prompts
                .first()
                .ok_or("The model review is unavailable.")?;
            if !scope.supports_unattended_search()
                || scope.profile_revision() != grant.profile_revision()
                || prompt.redaction_policy().redaction_revision()
                    != Some(grant.redaction_revision())
                || prompt.knowledge_mask(grant.public_topic())? != grant.public_topic()
            {
                return Err("The automatic web lookup moved to a different profile or mask. Review it again.".into());
            }
        }
        Ok(())
    }

    pub(in crate::scheduler_window) fn unattended_web_review(
        &self,
    ) -> Option<(&str, Option<String>)> {
        if !self.supports_fresh_web()
            || self.approval.web.is_some()
            || self.approval.fresh_web
            || self.model.as_ref()?.skills.is_none()
        {
            return None;
        }
        let ticket = self.firing_review_ticket()?;
        let destination = self
            .model
            .as_ref()
            .and_then(|model| model.web.as_ref())
            .filter(|review| review.scope.supports_unattended_search())
            .map(|review| review.scope.search_review());
        (destination.is_some() || self.approval.web_lookup().is_some())
            .then_some((ticket, destination))
    }

    pub(in crate::scheduler_window) fn unattended_web_page_destination(&self) -> Option<String> {
        self.model.as_ref()?.web.as_ref()?.scope.page_review()
    }

    pub(in crate::scheduler_window) fn supports_unattended_web_model_tools(&self) -> bool {
        let guided = self.action.agent.as_ref().is_some_and(|agent| {
            matches!(
                (agent.kind, agent.probe),
                (super::Kind::Agent, super::Probe::PerFiring)
                    | (
                        super::Kind::Swarm,
                        super::Probe::SwarmIterative | super::Probe::SwarmReadOnly
                    )
            )
        });
        guided
            && self.model.as_ref().is_some_and(|model| {
                model
                    .web
                    .as_ref()
                    .is_some_and(|review| review.scope.uses_direct_tools())
                    && model
                        .prompts
                        .first()
                        .is_some_and(|prompt| prompt.transport.supports_web_tool_turns())
            })
    }
}

impl SchedulerView {
    pub(crate) fn unattended_web_includes_page_mask(&self, ticket: &str) -> bool {
        self.unattended_web_topic(ticket).is_some()
            && self.agent_review.as_ref().is_some_and(|review| {
                review.unattended_web_followups
                    || review.unattended_web_round_pages
                    || review.unattended_web_model_tools
            })
    }

    pub(crate) fn unattended_web_topic(&self, ticket: &str) -> Option<&str> {
        let review = self.agent_review.as_ref()?;
        let (current, destination) = review.unattended_web_review()?;
        (current == ticket && destination.is_some()).then_some(review.unattended_web_topic.as_str())
    }

    pub(crate) fn proposal_web_scope(&self, preview: &str) -> Option<&web::Scope> {
        if !self.skill_preview_closed(preview) {
            return None;
        }
        let (review, model) = self.proposal_review()?;
        let web = model.web.as_ref()?;
        (review.knowledge_ready()
            && !review.unattended_web_draft_pending()
            && !review.approval.requires_fresh_web())
        .then_some(&web.scope)
    }

    pub(crate) fn proposal_web_state_mut(&mut self) -> Option<&mut web::State> {
        Some(&mut self.preview_model_mut()?.0.web.as_mut()?.state)
    }

    pub(crate) fn bind_proposal_web_vault(&mut self, revision: Option<Weak<()>>) {
        if let Some(web) = self
            .preview_model_mut()
            .and_then(|(model, _)| model.web.as_mut())
        {
            web.vault_revision = revision;
        }
    }

    pub(crate) fn active_proposal_web_scope(&self) -> Option<&web::Scope> {
        let (review, model) = self.proposal_review()?;
        let web = model.web.as_ref()?;
        (review.approval.requires_fresh_web()
            || web.state.active()
            || web.state.included().is_some())
        .then_some(&web.scope)
    }

    pub(crate) fn proposal_web_vault_revision(&self) -> Result<Option<Weak<()>>, String> {
        let revision = self
            .proposal_review()
            .and_then(|(_, model)| model.web.as_ref())
            .and_then(|web| web.vault_revision.clone());
        if revision
            .as_ref()
            .is_some_and(|revision| revision.strong_count() == 0)
        {
            return Err("The credential vault changed. Review the web search again.".into());
        }
        Ok(revision)
    }
}

impl SchedulerWindow {
    pub(in crate::scheduler_window) fn edit_unattended_web_topic(
        &self,
        view: &mut SchedulerView,
        ticket: &str,
        topic: String,
    ) -> Result<(), String> {
        if view.unattended_web_topic(ticket).is_none()
            || topic.len() > web_lookup::MAX_PUBLIC_TOPIC_BYTES
        {
            return Ok(());
        }
        let review = view.agent_review.as_ref().expect("current public topic");
        self.agent_review_current(view, review)?;
        view.agent_review
            .as_mut()
            .expect("current model review")
            .unattended_web_topic = topic;
        Ok(())
    }

    pub(in crate::scheduler_window) fn edit_unattended_web_followups(
        &self,
        view: &mut SchedulerView,
        ticket: &str,
        enabled: bool,
    ) -> Result<(), String> {
        self.edit_unattended_web_option(view, ticket, enabled, OptionKind::Followups)
    }

    pub(in crate::scheduler_window) fn edit_unattended_web_environment(
        &self,
        view: &mut SchedulerView,
        ticket: &str,
        enabled: bool,
    ) -> Result<(), String> {
        self.edit_unattended_web_option(view, ticket, enabled, OptionKind::Environment)
    }

    pub(in crate::scheduler_window) fn edit_unattended_web_rounds(
        &self,
        view: &mut SchedulerView,
        ticket: &str,
        enabled: bool,
    ) -> Result<(), String> {
        self.edit_unattended_web_option(view, ticket, enabled, OptionKind::Rounds)
    }

    pub(in crate::scheduler_window) fn edit_unattended_web_round_pages(
        &self,
        view: &mut SchedulerView,
        ticket: &str,
        enabled: bool,
    ) -> Result<(), String> {
        self.edit_unattended_web_option(view, ticket, enabled, OptionKind::RoundPages)
    }

    pub(in crate::scheduler_window) fn edit_unattended_web_model_tools(
        &self,
        view: &mut SchedulerView,
        ticket: &str,
        enabled: bool,
    ) -> Result<(), String> {
        self.edit_unattended_web_option(view, ticket, enabled, OptionKind::ModelTools)
    }

    fn edit_unattended_web_option(
        &self,
        view: &mut SchedulerView,
        ticket: &str,
        enabled: bool,
        option: OptionKind,
    ) -> Result<(), String> {
        if view.unattended_web_topic(ticket).is_none() {
            return Ok(());
        }
        let review = view
            .agent_review
            .as_ref()
            .expect("current web research review");
        self.agent_review_current(view, review)?;
        let current = match option {
            OptionKind::Followups => review.unattended_web_followups,
            OptionKind::Environment if review.supports_unattended_web_environment() => {
                review.unattended_web_environment
            }
            OptionKind::Rounds if review.supports_unattended_web_rounds() => {
                review.unattended_web_rounds
            }
            OptionKind::RoundPages
                if review.supports_unattended_web_rounds()
                    && review.unattended_web_rounds
                    && (!enabled || review.unattended_web_page_destination().is_some()) =>
            {
                review.unattended_web_round_pages
            }
            OptionKind::ModelTools if !enabled || review.supports_unattended_web_model_tools() => {
                review.unattended_web_model_tools
            }
            _ => return Ok(()),
        };
        if current == enabled {
            return Ok(());
        }
        let review = view
            .agent_review
            .as_mut()
            .expect("current web research review");
        review
            .model
            .as_mut()
            .and_then(|model| model.skills.as_mut())
            .expect("current model preview")
            .renew_preview()?;
        match option {
            OptionKind::Followups => review.unattended_web_followups = enabled,
            OptionKind::Environment => review.unattended_web_environment = enabled,
            OptionKind::Rounds => {
                review.unattended_web_rounds = enabled;
                if !enabled {
                    review.unattended_web_round_pages = false;
                }
            }
            OptionKind::RoundPages => review.unattended_web_round_pages = enabled,
            OptionKind::ModelTools => review.unattended_web_model_tools = enabled,
        }
        Ok(())
    }

    pub(in crate::scheduler_window) fn review_unattended_web(
        &self,
        view: &mut SchedulerView,
        ticket: &str,
        topic: &str,
    ) -> Result<(), String> {
        if view.unattended_web_topic(ticket) != Some(topic) {
            return Ok(());
        }
        let (review, model) = view.proposal_review().expect("current public topic");
        self.agent_review_current(view, review)?;
        let scope = &model.web.as_ref().expect("current web settings").scope;
        scope.require_current()?;
        let prompt = model
            .prompts
            .first()
            .ok_or("The model review is unavailable.")?;
        if prompt.knowledge_mask(topic)? != topic {
            return Err("Remove any private or masked text from the public research topic.".into());
        }
        let grant = web_lookup::Grant::new(
            topic.to_owned(),
            review.approval.profile.id.clone(),
            scope.profile_revision(),
            prompt
                .redaction_policy()
                .redaction_revision()
                .ok_or("The current redaction rules are unavailable.")?,
        )?;
        let grant = if review.unattended_web_followups {
            grant.with_followups()
        } else {
            grant
        };
        let grant = if review.unattended_web_environment {
            if !review.supports_unattended_web_environment() {
                return Err(ENVIRONMENT_REVIEW_REQUIRED.into());
            }
            grant.with_environment_lookup()
        } else {
            grant
        };
        let grant = if review.unattended_web_rounds {
            if !review.supports_unattended_web_rounds() {
                return Err(ROUND_REVIEW_REQUIRED.into());
            }
            grant.with_round_lookup()
        } else {
            grant
        };
        let grant = if review.unattended_web_round_pages {
            if !grant.allows_round_lookup() || scope.page_review().is_none() {
                return Err(ROUND_PAGE_READER_REQUIRED.into());
            }
            grant.with_round_page()
        } else {
            grant
        };
        let grant = if review.unattended_web_model_tools {
            if !review.supports_unattended_web_model_tools() {
                return Err(MODEL_TOOLS_REQUIRED.into());
            }
            grant.with_model_tools()
        } else {
            grant
        };
        self.replace_unattended_web(view, Some(grant))?;
        view.status = Some("Review the displayed automatic research and approve the scheduled model requests, then save the job.".into());
        Ok(())
    }

    pub(in crate::scheduler_window) fn remove_unattended_web(
        &self,
        view: &mut SchedulerView,
        ticket: &str,
    ) -> Result<(), String> {
        let Some(review) = view.agent_review.as_ref() else {
            return Ok(());
        };
        if !review
            .unattended_web_review()
            .is_some_and(|(current, _)| current == ticket)
        {
            return Ok(());
        }
        self.agent_review_current(view, review)?;
        self.replace_unattended_web(view, None)
    }

    pub(super) fn replace_unattended_web(
        &self,
        view: &mut SchedulerView,
        grant: Option<web_lookup::Grant>,
    ) -> Result<(), String> {
        let review = view.agent_review.as_ref().expect("current model review");
        let job = self.build_current_draft(view, review.id.clone())?;
        let updated = scope_with_web_lookup(scope(&job)?, false, grant.as_ref())?;
        let review = view.agent_review.as_mut().expect("current model review");
        review
            .model
            .as_mut()
            .and_then(|model| model.skills.as_mut())
            .ok_or("The model review is unavailable.")?
            .renew_preview()?;
        if grant.is_none() {
            review.unattended_web_topic.clear();
            review.unattended_web_followups = false;
            review.unattended_web_environment = false;
            review.unattended_web_rounds = false;
            review.unattended_web_round_pages = false;
            review.unattended_web_model_tools = false;
        }
        review.approval.scope = updated;
        review.approval.web_lookup = grant;
        Ok(())
    }

    pub(in crate::scheduler_window) fn set_fresh_web_requirement(
        &self,
        view: &mut SchedulerView,
        ticket: &str,
        enabled: bool,
    ) -> Result<(), String> {
        let Some(review) = view.agent_review.as_ref() else {
            return Ok(());
        };
        let Some((current_ticket, current)) = review.fresh_web_choice() else {
            return Ok(());
        };
        if ticket != current_ticket || current == enabled {
            return Ok(());
        }
        self.agent_review_current(view, review)?;
        if enabled {
            if review.unattended_web_draft_pending() {
                return Err(
                    "Remove the public research topic before selecting a manual web review.".into(),
                );
            }
            review.require_fresh_web_scope()?;
        }
        let job = self.build_current_draft(view, review.id.clone())?;
        let updated = scope_with_fresh_web(scope(&job)?, enabled)?;
        let review = view.agent_review.as_mut().expect("checked agent review");
        review
            .model
            .as_mut()
            .and_then(|model| model.skills.as_mut())
            .expect("checked model preview")
            .renew_preview()?;
        review.approval.scope = updated;
        review.approval.fresh_web = enabled;
        Ok(())
    }

    pub(crate) fn attach_proposal_web(
        &self,
        view: &mut SchedulerView,
        policy_allows: bool,
        vault: Option<&crate::vault_window::VaultWindow>,
    ) {
        let Ok(Some(authority)) = self.proposal_authority(view) else {
            return;
        };
        let Some(config) = authority.skills.map(|setup| setup.config.as_ref()) else {
            return;
        };
        let Some(profile) = config.resolve_profile_reference(&authority.profile.id) else {
            return;
        };
        let Some(scope) = web::Scope::new(Some(config), profile, policy_allows) else {
            return;
        };
        let scope = scope
            .with_vault(vault)
            .with_redactor(authority.prompt.web_redactor());
        if let Some((model, _)) = view.preview_model_mut() {
            model.web = Some(Review {
                state: web::State::default(),
                scope,
                vault_revision: None,
            });
        }
    }

    pub(crate) fn replace_proposal_web(
        &self,
        view: &mut SchedulerView,
        evidence: Option<Arc<web::Evidence>>,
    ) -> Result<Task<Message>, String> {
        let (review, _) = view
            .proposal_review()
            .ok_or("The model review is unavailable.")?;
        self.agent_review_current(view, review)?;
        if review.approval.requires_fresh_web() && evidence.is_some() {
            return Err("Fresh web lookups are reviewed separately for each run.".into());
        }
        let review = view.agent_review.as_mut().expect("checked review");
        review.replace_proposal_context(
            "Finish the skill review before changing web excerpts.",
            |prompt, standing| prompt.with_web(evidence.clone(), standing),
            |approval, standing| approval.web = standing.then(|| evidence.clone()).flatten(),
        )
    }
}
