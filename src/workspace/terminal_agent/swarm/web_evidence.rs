//! Web search consent and model evidence for an interactive Swarm round.

use std::sync::{Arc, Weak};

use iced::{Task, window};

use crate::terminal_logging::{Redactor, Secrets};
use crate::workspace::terminal_ai::web;

use super::super::knowledge_review;
use super::{ClipboardPolicy, Desktop, Group, Message, Mode, RunMessage, SwarmMode, Workspace};

const SETTINGS_CHANGED: &str = "The web search settings changed. Review the search again.";

#[derive(Clone)]
pub(in crate::workspace::terminal_agent) struct Evidence {
    scope: Arc<web::Scope>,
    excerpts: Arc<web::Evidence>,
    vault: Weak<()>,
}

impl Evidence {
    pub(in crate::workspace::terminal_agent) fn check(&self) -> Result<(), String> {
        self.scope.require_current()?;
        check_vault(&self.vault)
    }

    pub(in crate::workspace::terminal_agent) fn require_current(
        &self,
        workspace: &Workspace,
    ) -> Result<(), String> {
        if !workspace.clipboard.policy.ai_web_search_allowed
            || !Weak::ptr_eq(&self.vault, &workspace.vault_change_revision())
        {
            return Err(SETTINGS_CHANGED.into());
        }
        self.check()
    }

    pub(in crate::workspace::terminal_agent) fn prompt_context(
        &self,
        masking: &knowledge_review::Mask,
    ) -> Result<String, String> {
        self.prompt_context_with(masking, None)
    }

    pub(in crate::workspace::terminal_agent) fn prompt_context_with(
        &self,
        masking: &knowledge_review::Mask,
        additional: Option<&web::Evidence>,
    ) -> Result<String, String> {
        let combined = additional
            .map(|additional| self.excerpts.merged(additional))
            .transpose()?;
        let excerpts = combined.as_ref().unwrap_or(self.excerpts.as_ref());
        excerpts.require_mask(|value| masking.apply(value))?;
        let mut context = String::new();
        excerpts.append_to(&mut context);
        Ok(context)
    }
}

fn check_vault(vault: &Weak<()>) -> Result<(), String> {
    if vault.strong_count() == 0 {
        Err(SETTINGS_CHANGED.into())
    } else {
        Ok(())
    }
}

impl Group {
    pub(super) fn clear_web_search(&mut self) {
        self.web_search = web::State::default();
        self.web_vault = None;
    }
}

impl Workspace {
    pub(in crate::workspace::terminal_agent) fn swarm_web_available(&self, id: window::Id) -> bool {
        let Some(group) = self.terminal_agents.swarms.get(&id) else {
            return false;
        };
        self.swarm_web_scope(group).is_some()
    }

    fn swarm_web_scope(&self, group: &Group) -> Option<web::Scope> {
        let policy = &self.clipboard.policy;
        let private = group
            .owner
            .and_then(|owner| self.windows.get(&owner))
            .and_then(|window| window.tab())
            .is_some_and(|tab| tab.incognito);
        let allowed = !group.closing
            && !private
            && group.mode != SwarmMode::Snippet
            && policy.ai_web_search_allowed
            && super::require_swarm_policy(policy, group.mode).is_ok();
        web::Scope::new(
            self.ai_configuration().as_deref(),
            group.profile.as_ref()?.id,
            allowed,
        )
    }

    fn swarm_web_mask(&self, group: &Group) -> Result<(Redactor, knowledge_review::Mask), String> {
        let secrets = Secrets::scheduled_targets(group.selected.len());
        let config = self.ai_configuration().ok_or(SETTINGS_CHANGED)?;
        let profile = group.profile.as_ref().ok_or(SETTINGS_CHANGED)?;
        let transport = profile.transport(&config)?.0;
        let credential = transport.completion_secret();
        if let Some(credential) = credential.as_deref() {
            secrets.add(credential);
        }
        let mode = if group.mode == SwarmMode::Agent {
            Mode::Run
        } else {
            Mode::Ask
        };
        for choice in group
            .choices
            .iter()
            .filter(|choice| group.selected.contains(&choice.target.key))
        {
            if let Some(source) = &choice.saved {
                let connection = self.saved_swarm_connection(source, mode, true)?;
                let (_, redactor) = self.saved_swarm_credentials(source, &connection)?;
                redactor.add_known_secrets(&secrets);
            } else if let Some((session, stream)) = choice.target.key.pane() {
                let (_, pane, _) = self
                    .agent_pane(session)
                    .filter(|(_, pane, tab)| pane.stream_generation == stream && !tab.incognito)
                    .ok_or("A selected Swarm terminal changed. Review the search again.")?;
                self.agent_source_redactor(pane)?
                    .add_known_secrets(&secrets);
            }
        }
        let redactor = Redactor::for_journal(
            secrets.seal().map_err(|error| error.to_string())?,
            self.clipboard.policy.terminal_logging.clone(),
        );
        let masking = knowledge_review::Mask::new(redactor.clone(), None, self.vault.as_ref())
            .for_targets(group.selected.len());
        Ok((redactor, masking))
    }

    pub(super) fn update_swarm_web(
        &self,
        id: window::Id,
        group: &mut Group,
        message: web::Message,
    ) -> Result<Task<Desktop>, String> {
        if matches!(message, web::Message::Close) {
            group.clear_web_search();
            return Ok(Task::none());
        }
        let current = self
            .swarm_web_scope(group)
            .ok_or(SETTINGS_CHANGED)
            .map_err(str::to_owned)
            .and_then(|scope| {
                let (redactor, masking) = self.swarm_web_mask(group)?;
                Ok((scope.with_redactor(&redactor), masking))
            });
        let (scope, masking) = match current {
            Ok(current) => current,
            Err(error) => {
                group.clear_web_search();
                return Err(error);
            }
        };
        let current_vault = self.vault_change_revision();
        if group.web_search.invalidate(Some(&scope))
            || group
                .web_vault
                .as_ref()
                .is_some_and(|vault| !Weak::ptr_eq(vault, &current_vault))
        {
            group.clear_web_search();
            return Err(SETTINGS_CHANGED.into());
        }
        if matches!(message, web::Message::Open) {
            group.web_vault = Some(current_vault);
        }
        let Some(vault) = group.web_vault.clone() else {
            return Ok(Task::none());
        };
        let ticket = matches!(message, web::Message::Send).then(|| {
            group.web_ticket += 1;
            group.web_ticket
        });
        let mode = group.mode;
        group.web_search.update(
            message,
            scope,
            move |value| masking.apply(value),
            ticket,
            move || {
                super::require_swarm_policy(&ClipboardPolicy::load(), mode)?;
                check_vault(&vault)
            },
            move |message| Desktop::TerminalAgent(id, RunMessage::Swarm(Message::Web(message))),
        )
    }

    pub(super) fn reviewed_swarm_web(&self, group: &Group) -> Result<Option<Evidence>, String> {
        let Some((scope, results)) = group.web_search.included() else {
            return Ok(None);
        };
        let current = self.swarm_web_scope(group).ok_or(SETTINGS_CHANGED)?;
        let (redactor, masking) = self.swarm_web_mask(group)?;
        if &current.with_redactor(&redactor) != scope {
            return Err(SETTINGS_CHANGED.into());
        }
        let evidence = Evidence {
            scope: Arc::new(scope.clone()),
            excerpts: Arc::new(web::Evidence::new(scope, results)?),
            vault: group.web_vault.clone().ok_or(SETTINGS_CHANGED)?,
        };
        evidence.require_current(self)?;
        evidence
            .excerpts
            .require_mask(|value| masking.apply(value))?;
        Ok(Some(evidence))
    }
}
