//! Recheck installed policy before dispatching an approved agent operation.

use crate::clipboard_policy::{AiAgentExecution, ClipboardPolicy};
use crate::local_ai::Profile;
use crate::terminal_logging::Policy as RedactionPolicy;

use super::swarm::headless::Source;
use super::{IsolationLevel, Mode, Run, Workspace};
use crate::workspace::SshTarget;

#[derive(Clone, Copy, Default)]
pub(super) struct Approvals {
    pub(super) batch: bool,
    pub(super) remaining: bool,
}

impl From<bool> for Approvals {
    fn from(batch: bool) -> Self {
        Self {
            batch,
            remaining: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ApprovalKind {
    ReadOnly,
    Ordinary,
    Root,
}

#[derive(Clone)]
pub(super) struct RunApproval {
    kind: ApprovalKind,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    configuration: std::sync::Arc<crate::ai_manager::Configuration>,
    reference: crate::ai_manager::ProfileRef,
    revision: [u8; 32],
}

impl RunApproval {
    pub(super) fn root_active(&self) -> bool {
        self.kind == ApprovalKind::Root && self.active.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(super) fn revoke(&self) {
        self.active
            .store(false, std::sync::atomic::Ordering::Release);
    }

    fn check(
        &self,
        current: &crate::ai_manager::Configuration,
        policy: &ClipboardPolicy,
    ) -> Result<(), &'static str> {
        if !self.active.load(std::sync::atomic::Ordering::Acquire)
            || !current.agent_execution_enabled()
            || (self.kind != ApprovalKind::ReadOnly
                && current.agent_requires_command_confirmation())
            || policy.ai_agent_execution == crate::clipboard_policy::AiAgentExecution::ReadOnly
            || (self.kind != ApprovalKind::ReadOnly
                && policy.ai_agent_execution != crate::clipboard_policy::AiAgentExecution::Allow)
            || current.agent_profile_reference_revision(&self.reference) != Some(self.revision)
        {
            return Err("The run-wide command approval changed. Review the command again.");
        }
        Ok(())
    }

    fn check_saved(&self) -> Result<(), String> {
        let current = self
            .configuration
            .reload_saved()
            .map_err(|_| "The saved command approval settings are unavailable.")?;
        self.check(&current, &ClipboardPolicy::load())
            .map_err(str::to_owned)
    }
}

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
    run_approval: Option<RunApproval>,
    configuration: Option<std::sync::Arc<crate::ai_manager::Configuration>>,
    mode: Option<Mode>,
    swarm: bool,
    route: Route,
    redactions: Option<RedactionPolicy>,
    managed_https: Option<Box<Profile>>,
}

impl Dispatch {
    pub(super) fn check(&self) -> Result<(), String> {
        self.check_with(&ClipboardPolicy::load())
            .map_err(str::to_owned)?;
        if self.mode == Some(Mode::Run)
            && !self
                .configuration
                .as_ref()
                .is_some_and(|config| config.current_agent_execution_enabled())
        {
            return Err(
                "Agent command execution is disabled or its saved settings are unavailable.".into(),
            );
        }
        if let Some(transport) = &self.managed_https {
            transport.require_current_policy_authority()?;
        }
        if let Some(approval) = &self.run_approval {
            approval.check_saved()?;
        }
        Ok(())
    }

    pub(super) fn check_with(&self, policy: &ClipboardPolicy) -> Result<(), &'static str> {
        if self.mode == Some(Mode::Run)
            && !self
                .configuration
                .as_ref()
                .is_some_and(|config| config.agent_execution_enabled())
        {
            return Err("Agent command execution is disabled in AI Manager.");
        }
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
    pub(super) fn approve_remaining_agent_commands(
        &self,
        run: &Run,
    ) -> Result<RunApproval, String> {
        if !run.ordinary_command_review() || run.paused || run.confirm_every_command {
            return Err("Review an ordinary Agent command first.".into());
        }
        let configuration = self
            .ai_configuration()
            .filter(|config| config.saved_root().is_some())
            .ok_or("Save the AI settings before approving a run.")?;
        let reference = run
            .profile()
            .ok_or("The Agent profile is unavailable.")?
            .reference
            .clone();
        let revision = configuration
            .agent_profile_reference_revision(&reference)
            .ok_or("The Agent profile changed.")?;
        let approval = RunApproval {
            kind: ApprovalKind::Ordinary,
            active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            configuration,
            reference,
            revision,
        };
        approval.check(&approval.configuration, &self.clipboard.policy)?;
        approval.check_saved()?;
        Ok(approval)
    }

    pub(super) fn automatic_agent_command(&self, run: &mut Run) -> bool {
        if run.paused {
            run.auto_approve_root = false;
            run.revoke_command_approval();
            return false;
        }
        let root_review = run.mode == Mode::Run
            && run.phase == super::Phase::Approval
            && run.profile().is_some()
            && !run.commands.is_empty();
        if run.auto_approve_root
            && run.probe_reviewed
            && root_review
            && run.run_approval.is_none()
            && let Some(configuration) = self
                .ai_configuration()
                .filter(|config| config.saved_root().is_some())
            && let Some(reference) = run.profile().map(|profile| profile.reference.clone())
            && let Some(revision) = configuration.agent_profile_reference_revision(&reference)
        {
            let approval = RunApproval {
                kind: ApprovalKind::Root,
                active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
                configuration,
                reference,
                revision,
            };
            if approval
                .check(&approval.configuration, &self.clipboard.policy)
                .is_ok()
                && approval.check_saved().is_ok()
            {
                run.run_approval = Some(approval);
            }
        }
        // Java's explicitly requested root bypass precedes the per-command ask option.
        let root = run.auto_approve_root
            && run
                .run_approval
                .as_ref()
                .is_some_and(|approval| approval.kind == ApprovalKind::Root);
        if run.confirm_every_command && !root {
            run.revoke_command_approval();
            return false;
        }
        let read_only = run.probe_reviewed
            && run.ordinary_command_review()
            && run.commands.iter().all(|command| {
                command.sudo.is_none()
                    && super::swarm::batch::ordinary(&command.proposal)
                    && command.proposal.risk == crate::terminal_agent::Risk::ReadOnly
                    && !crate::terminal_agent::requires_confirmation_by_shape(
                        &command.proposal.command,
                    )
            });
        if !read_only
            && run
                .run_approval
                .as_ref()
                .is_some_and(|approval| approval.kind == ApprovalKind::ReadOnly)
        {
            run.revoke_command_approval();
        }
        if run.run_approval.is_none()
            && read_only
            && let Some(configuration) = self
                .ai_configuration()
                .filter(|config| config.saved_root().is_some())
            && let Some(reference) = run.profile().map(|profile| profile.reference.clone())
            && let Some(revision) = configuration.agent_profile_reference_revision(&reference)
        {
            let approval = RunApproval {
                kind: ApprovalKind::ReadOnly,
                active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
                configuration,
                reference,
                revision,
            };
            if approval
                .check(&approval.configuration, &self.clipboard.policy)
                .is_ok()
                && approval.check_saved().is_ok()
            {
                run.run_approval = Some(approval);
            }
        }
        let valid = run.run_approval.as_ref().is_some_and(|approval| {
            self.ai_configuration()
                .is_some_and(|config| approval.check(&config, &self.clipboard.policy).is_ok())
                && approval.check_saved().is_ok()
        });
        if !valid {
            if run
                .run_approval
                .as_ref()
                .is_some_and(|approval| approval.kind == ApprovalKind::Root)
            {
                run.auto_approve_root = false;
            }
            run.revoke_command_approval();
        }
        if run.phase != super::Phase::Approval {
            return false;
        }
        if !run.ordinary_command_review() && !(root && root_review) {
            run.revoke_command_approval();
            return false;
        }
        valid
    }
    pub(in crate::workspace) fn require_agent_execution_enabled(&self) -> Result<(), &'static str> {
        if self
            .ai_configuration()
            .is_some_and(|config| config.agent_execution_enabled())
        {
            Ok(())
        } else {
            Err("Agent command execution is disabled in AI Manager.")
        }
    }

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
            run_approval: run.run_approval.clone(),
            configuration: self.ai_configuration(),
            mode: run.profile().map(|_| run.mode),
            swarm: run.swarm,
            route,
            redactions: run
                .profile()
                .map(|_| run.redactor.redaction_policy().clone()),
            managed_https: run
                .transport
                .as_ref()
                .map(|(transport, _)| transport)
                .filter(|transport| {
                    transport.is_remote() && transport.policy_profile_id().is_some()
                })
                .map(|transport| Box::new(transport.clone())),
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
    fn agent_dispatch_rechecks_saved_execution_preference_after_review() {
        use crate::ai_manager::Configuration;
        let scratch = crate::snippets::tests::Profile::new();
        let root = scratch.0.canonicalize().unwrap();
        Configuration::default()
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let config = Configuration::load_fixture_from(&root).unwrap();
        let dispatch = |mode| Dispatch {
            run_approval: None,
            configuration: Some(std::sync::Arc::new(config.clone())),
            mode: Some(mode),
            swarm: false,
            route: Route::Local,
            redactions: None,
            managed_https: None,
        };
        assert!(dispatch(Mode::Run).check().is_ok());
        let disabled: Configuration = serde_json::from_value(
            serde_json::json!({"general":{"agent_execution_enabled":false}}),
        )
        .unwrap();
        disabled.save_fixture_at(&root, &config).unwrap();
        assert!(dispatch(Mode::Run).check().is_err());
        assert!(dispatch(Mode::Ask).check().is_ok());
        assert!(dispatch(Mode::Plan).check().is_ok());
        let mut snippet = dispatch(Mode::Run);
        snippet.mode = None;
        assert!(snippet.check().is_ok());
    }

    #[test]
    fn dispatch_distinguishes_planning_agent_and_swarm_snippets() {
        let reviewed = |mode, swarm| Dispatch {
            run_approval: None,
            configuration: Some(std::sync::Arc::new(
                crate::ai_manager::Configuration::default(),
            )),
            mode,
            swarm,
            route: Route::Local,
            redactions: None,
            managed_https: None,
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
