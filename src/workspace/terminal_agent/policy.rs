//! Recheck installed policy before dispatching an approved agent operation.

use crate::clipboard_policy::{AiAgentExecution, ClipboardPolicy};
use crate::terminal_logging::Policy as RedactionPolicy;

use super::swarm::headless::Source;
use super::{IsolationLevel, Mode, Run, Workspace};
use crate::workspace::SshTarget;

pub(super) fn agent_mode(policy: &ClipboardPolicy, mode: Mode) -> Result<(), &'static str> {
    match mode {
        Mode::Plan if !policy.ai_planning_allowed() => Err("Managed policy blocks AI Planning."),
        Mode::Run | Mode::Ask if !policy.ai_agent_allowed() => {
            Err("Managed policy blocks AI Agent.")
        }
        Mode::Run if policy.ai_agent_execution == AiAgentExecution::ReadOnly => {
            Err("Agent command execution is disabled by managed policy.")
        }
        _ => Ok(()),
    }
}

enum Route {
    Local,
    Ssh(Box<SshTarget>),
    Saved(Box<Source>),
}

pub(super) struct Dispatch {
    mode: Option<Mode>,
    swarm: bool,
    route: Route,
    redactions: Option<RedactionPolicy>,
}

impl Dispatch {
    pub(super) fn check(&self) -> Result<(), String> {
        self.check_with(&ClipboardPolicy::load())
            .map_err(str::to_owned)
    }

    pub(super) fn check_with(&self, policy: &ClipboardPolicy) -> Result<(), &'static str> {
        if policy.problem.is_some() || (self.swarm && !policy.multi_exec_allowed) {
            return Err("Managed policy blocks the reviewed terminal operation.");
        }
        if let Some(mode) = self.mode {
            agent_mode(policy, mode)?;
            if self.swarm && !policy.ai_swarm_allowed() {
                return Err("Managed policy blocks AI Swarm.");
            }
            if self
                .redactions
                .as_ref()
                .is_some_and(|reviewed| !reviewed.equivalent_redactions(&policy.terminal_logging))
            {
                return Err("The managed AI redaction rules changed. Start a new run.");
            }
        }
        let route_allowed = match &self.route {
            Route::Local => {
                matches!(self.mode, Some(Mode::Ask | Mode::Plan))
                    || policy.minimum_isolation == IsolationLevel::None
            }
            Route::Ssh(route) => route.workflow_route_allowed(policy),
            Route::Saved(source) => source.route_allowed(policy) && source.revision_current(),
        };
        if !route_allowed {
            return Err("Managed policy blocks the reviewed terminal route.");
        }
        Ok(())
    }
}

impl Workspace {
    pub(super) fn agent_dispatch_policy(&self, run: &Run) -> Result<Dispatch, &'static str> {
        let route = if let Some(source) = run.binding.saved() {
            Route::Saved(Box::new(source.clone()))
        } else {
            let source = run.binding.live()?;
            let (_, pane, _) = self
                .agent_pane(source.session)
                .filter(|(_, pane, tab)| {
                    pane.stream_generation == source.stream && tab.incognito == source.private
                })
                .ok_or("The source terminal changed. Start a new review.")?;
            pane.ssh
                .as_ref()
                .map_or(Route::Local, |route| Route::Ssh(Box::new(route.clone())))
        };
        Ok(Dispatch {
            mode: run.profile().map(|_| run.mode),
            swarm: run.swarm,
            route,
            redactions: run
                .profile()
                .map(|_| run.redactor.redaction_policy().clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(features: &str) -> ClipboardPolicy {
        crate::clipboard_policy::parse(&format!(
            "[meta]\nschema-version = 1\n[[rule]]\n[rule.features]\n{features}"
        ))
        .unwrap()
    }

    #[test]
    fn dispatch_distinguishes_planning_agent_and_swarm_snippets() {
        let reviewed = |mode, swarm| Dispatch {
            mode,
            swarm,
            route: Route::Local,
            redactions: None,
        };
        let agent_denied = policy("ai-agent = 'deny'");
        assert!(
            reviewed(Some(Mode::Plan), false)
                .check_with(&agent_denied)
                .is_ok()
        );
        assert!(
            reviewed(Some(Mode::Ask), false)
                .check_with(&agent_denied)
                .is_err()
        );
        assert!(
            reviewed(Some(Mode::Run), false)
                .check_with(&agent_denied)
                .is_err()
        );

        let swarm_denied = policy("ai-swarm = 'deny'\nai-planning = 'deny'");
        assert!(
            reviewed(Some(Mode::Run), false)
                .check_with(&swarm_denied)
                .is_ok()
        );
        assert!(
            reviewed(Some(Mode::Plan), false)
                .check_with(&swarm_denied)
                .is_err()
        );
        assert!(
            reviewed(Some(Mode::Ask), true)
                .check_with(&swarm_denied)
                .is_err()
        );
        assert!(reviewed(None, true).check_with(&swarm_denied).is_ok());

        let read_only = policy("ai-agent-execution = 'read-only'");
        assert!(
            reviewed(Some(Mode::Run), false)
                .check_with(&read_only)
                .is_err()
        );
        assert!(
            reviewed(Some(Mode::Ask), false)
                .check_with(&read_only)
                .is_ok()
        );
        assert!(
            reviewed(Some(Mode::Plan), false)
                .check_with(&read_only)
                .is_ok()
        );
    }
}
