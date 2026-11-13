use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use crate::ai_manager::{Configuration, SkillGuidance};
use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_store::Store;
use crate::job_executor;
use crate::job_executor::agent::advisory::{
    automatic_guidance as automatic_firing_guidance, target_guidance as render_firing_guidance,
};
use crate::scheduler_window::agent::{
    ClassifiedPrompts, Kind, ProposalAuthority, ProposalSkillSetup, Target, proposal::Prompt,
};
use crate::terminal_logging::{Policy, Redactor};
use crate::workspace::{
    skill_classifier::{Request, Scope},
    terminal_ai::mask,
};

pub(super) fn prepare(
    config: Arc<Configuration>,
    store: &Store,
    targets: &[Target],
    goal: &str,
    redactor: &Redactor,
    prompt: Prompt,
) -> Result<(Prompt, ProposalSkillSetup, bool), String> {
    let pins = target_pins(store, targets)?;
    let context = mask::prompt(redactor, goal)?;
    let assigned = combined_pins(&pins);
    let selection = config.agent_skill_selection(&context, &assigned);
    let guidance = render_guidance(&config, &pins, selection.reviewed_automatic_guidance(&[]));
    let (prompt, guidance, omitted) = match prompt.with_skill_guidance(&guidance.text) {
        Ok(prompt) => (prompt, guidance, false),
        Err(_) => (
            prompt.with_skill_guidance("")?,
            SkillGuidance::default(),
            true,
        ),
    };
    let mut setup = setup(config, pins, context, redactor, &prompt);
    setup.one_use_guidance = guidance;
    Ok((prompt, setup, omitted))
}

pub(super) fn prepare_standing(
    config: Arc<Configuration>,
    store: &Store,
    targets: &[Target],
    redactor: &Redactor,
    prompts: Vec<Prompt>,
) -> Result<(Vec<Prompt>, ProposalSkillSetup), String> {
    require_saved_config(&config)?;
    let pins = target_pins(store, targets)?;
    let first = prompts
        .first()
        .ok_or("The reviewed model request is unavailable.")?;
    let context = first
        .masked_question()
        .ok_or("The reviewed model question is unavailable.")?;
    let setup = setup(config, pins, context, redactor, first);
    let automatic = automatic_firing_guidance(&setup.config, &setup.pins, &setup.context, &[])?;
    let prompts = bind_standing_prompts(&setup, targets, prompts, &automatic, &[])?;
    Ok((prompts, setup))
}

fn setup(
    config: Arc<Configuration>,
    pins: Vec<(u64, Vec<u64>)>,
    context: String,
    redactor: &Redactor,
    prompt: &Prompt,
) -> ProposalSkillSetup {
    let classifier = if prompt.transport.is_remote() || prompt.transport.is_cli() {
        None
    } else {
        classifier_request(&config, &pins, &context, redactor, prompt.context_limit)
    };
    ProposalSkillSetup {
        config,
        pins,
        context,
        classifier,
        one_use_guidance: SkillGuidance::default(),
    }
}

fn classifier_request(
    config: &Configuration,
    pins: &[(u64, Vec<u64>)],
    context: &str,
    redactor: &Redactor,
    limit: usize,
) -> Option<Request> {
    let assigned = combined_pins(pins);
    let candidates = config.agent_skill_selection(context, &assigned);
    Request::prepare(
        Scope::Agent,
        redactor,
        context,
        None,
        &candidates.classifier_candidates()?,
    )
    .filter(|request| request.fits_context(limit))
}

pub(super) fn classified_prompts(
    authority: &ProposalAuthority<'_>,
    ids: &[u64],
) -> Option<ClassifiedPrompts> {
    let setup = authority.skills?;
    let assigned = combined_pins(&setup.pins);
    let selection = setup
        .config
        .agent_skill_selection(&setup.context, &assigned);
    let ids = selection.classified_ids(ids);
    if ids.is_empty() {
        return None;
    }
    let automatic = selection.reviewed_automatic_guidance(&ids);
    if authority.standing {
        let prompts = authority.prompts.iter().map(|prompt| (**prompt).clone());
        Some(ClassifiedPrompts {
            prompts: bind_standing_prompts(setup, authority.targets, prompts, &automatic, &ids)
                .ok()?,
            one_use_guidance: SkillGuidance::default(),
        })
    } else {
        let guidance = render_guidance(&setup.config, &setup.pins, automatic);
        Some(ClassifiedPrompts {
            prompts: vec![authority.prompt.with_skill_guidance(&guidance.text).ok()?],
            one_use_guidance: guidance,
        })
    }
}

fn bind_standing_prompts(
    setup: &ProposalSkillSetup,
    targets: &[Target],
    prompts: impl IntoIterator<Item = Prompt, IntoIter: ExactSizeIterator>,
    automatic: &SkillGuidance,
    ids: &[u64],
) -> Result<Vec<Prompt>, String> {
    let prompts = prompts.into_iter();
    if prompts.len() != targets.len() {
        return Err("Review one model request for each saved SSH target.".into());
    }
    targets
        .iter()
        .zip(prompts)
        .map(|(target, prompt)| {
            let guidance =
                render_firing_guidance(&setup.config, &setup.pins, target.reference.id, automatic)?;
            let prompt = prompt.with_firing_skill_guidance(&guidance.text)?;
            if ids.is_empty() {
                Ok(prompt)
            } else {
                prompt.with_firing_skill_classification(ids)
            }
        })
        .collect()
}

fn firing_guidance(
    config: &Configuration,
    store: &Store,
    targets: &[Target],
    target: &Target,
    prompt: &Prompt,
) -> Result<SkillGuidance, &'static str> {
    let question = prompt
        .masked_question()
        .ok_or("The reviewed model question is unavailable.")?;
    let pins = target_pins(store, targets)?;
    let automatic = automatic_firing_guidance(config, &pins, &question, prompt.firing_skill_ids())?;
    render_firing_guidance(config, &pins, target.reference.id, &automatic)
}

pub(super) fn require_firing_guidance(
    config: &Configuration,
    store: &Store,
    targets: &[Target],
    target: &Target,
    prompt: &Prompt,
) -> Result<SkillGuidance, &'static str> {
    if prompt.firing_skill_guidance().is_some() {
        let guidance = firing_guidance(config, store, targets, target, prompt)?;
        if !prompt.matches_firing_skill_guidance(&guidance.text) {
            return Err("The scheduled AI skill guidance changed. Review this job again.");
        }
        return Ok(guidance);
    }
    Ok(SkillGuidance::default())
}

fn target_pins(store: &Store, targets: &[Target]) -> Result<Vec<(u64, Vec<u64>)>, &'static str> {
    targets
        .iter()
        .map(|target| {
            let id = target.reference.id;
            store
                .get(id)
                .map(|connection| (id, connection.terminal.ai_skills.clone()))
                .ok_or("The saved SSH connection is unavailable.")
        })
        .collect()
}

fn combined_pins(pins: &[(u64, Vec<u64>)]) -> Vec<u64> {
    pins.iter()
        .flat_map(|(_, ids)| ids.iter().copied())
        .collect()
}

fn render_guidance(
    config: &Configuration,
    pins: &[(u64, Vec<u64>)],
    automatic: SkillGuidance,
) -> SkillGuidance {
    let mut guidance = SkillGuidance::default();
    for (target, ids) in pins {
        let pinned = config.reviewed_agent_pinned_skill_guidance(ids);
        if !pinned.text.is_empty() {
            let _ = write!(
                guidance.text,
                "\n\nSkill instructions for SSH target {target}. Apply these instructions only to that target."
            );
            guidance.append(pinned);
        }
    }
    if !guidance.text.is_empty() && !automatic.text.is_empty() {
        guidance
            .text
            .push_str("\n\nGeneral proposal skills apply to any selected SSH target.");
    }
    guidance.append(automatic);
    guidance
}

fn require_saved_config(config: &Configuration) -> Result<(), String> {
    if !config
        .reload_saved()
        .is_ok_and(|current| current == *config)
    {
        return Err("The saved AI skill or profile settings changed. Review again.".into());
    }
    Ok(())
}

pub(super) fn require_current(
    setup: &ProposalSkillSetup,
    current: Option<&Arc<Configuration>>,
    store: &Store,
    targets: &[Target],
    redactor: &Redactor,
    limit: usize,
) -> Result<(), String> {
    if !current.is_some_and(|current| Arc::ptr_eq(current, &setup.config)) {
        return Err("The AI skill or profile settings changed. Review again.".into());
    }
    require_saved_config(&setup.config)?;
    if target_pins(store, targets)? != setup.pins {
        return Err("The SSH target skill assignments changed. Review again.".into());
    }
    if let Some(reviewed) = &setup.classifier
        && classifier_request(&setup.config, &setup.pins, &setup.context, redactor, limit).as_ref()
            != Some(reviewed)
    {
        return Err("The masked skill classification changed. Review again.".into());
    }
    Ok(())
}

struct DispatchGuard {
    config: Arc<Configuration>,
    kind: Kind,
    targets: Vec<Target>,
    pins: Vec<(u64, Vec<u64>)>,
    redactions: Policy,
    profile: crate::scheduler_window::agent::ProfileChoice,
    transport: crate::local_ai::Profile,
    context_limit: usize,
    connection_path: PathBuf,
    trust: kortty_ssh::TrustStore,
    vault_revision: Option<Weak<()>>,
    knowledge: Option<crate::scheduler_window::agent::knowledge::Grant>,
}

impl DispatchGuard {
    fn verify(&self) -> Result<(), String> {
        require_vault_revision(self.vault_revision.as_ref())?;
        let policy = ClipboardPolicy::load();
        job_executor::agent::require_policy(&policy, self.kind)?;
        if !self
            .redactions
            .equivalent_redactions(&policy.terminal_logging)
        {
            return Err("The managed AI redaction rules changed. Review again.".into());
        }
        require_saved_config(&self.config)?;
        if !self
            .profile
            .transport(&self.config)
            .is_ok_and(|(transport, limit)| {
                transport == self.transport && limit == self.context_limit
            })
        {
            return Err("The reviewed model profile settings changed. Review again.".into());
        }
        let store = Store::open(self.connection_path.clone())
            .map_err(|_| "The saved SSH connections are unavailable.")?;
        if target_pins(&store, &self.targets)? != self.pins {
            return Err("The SSH target skill assignments changed. Review again.".into());
        }
        for target in &self.targets {
            require_saved_target(&store, target, &self.trust, &policy)?;
        }
        if let Some(knowledge) = &self.knowledge {
            let root = self
                .config
                .saved_root()
                .ok_or("The AI settings are unavailable.")?;
            knowledge.require_current(&self.config, &self.profile.id, root, || {
                require_vault_revision(self.vault_revision.as_ref()).map_err(str::to_owned)
            })?;
        }
        Ok(())
    }
}

pub(super) struct FiringDispatchGuard {
    pub(super) targets: Vec<Target>,
    pub(super) target_index: usize,
    pub(super) profile: crate::scheduler_window::agent::ProfileChoice,
    pub(super) connection_path: PathBuf,
    pub(super) trust: kortty_ssh::TrustStore,
    pub(super) vault_revision: Option<Weak<()>>,
    pub(super) cancellation: kortty_jobs::Cancellation,
}

impl FiringDispatchGuard {
    pub(super) fn verify(&self, prompt: &Prompt, policy: &ClipboardPolicy) -> Result<(), String> {
        require_vault_revision(self.vault_revision.as_ref())?;
        let config = Configuration::load().map_err(|_| "The saved AI settings are unavailable.")?;
        if !config.enabled()
            || self.profile.revision(&config, &policy.terminal_logging) != prompt.profile_revision()
            || !self
                .profile
                .transport(&config)
                .is_ok_and(|(transport, limit)| {
                    transport == prompt.transport && limit == prompt.context_limit
                })
        {
            return Err("The reviewed model profile settings changed. Review again.".into());
        }
        let store = Store::open(self.connection_path.clone())
            .map_err(|_| "The saved SSH connections are unavailable.")?;
        let target = self
            .targets
            .get(self.target_index)
            .ok_or("The reviewed SSH target is unavailable.")?;
        require_saved_target(&store, target, &self.trust, policy)?;
        require_firing_guidance(&config, &store, &self.targets, target, prompt)?;
        if let Some(knowledge) = prompt.knowledge() {
            let root = config
                .saved_root()
                .ok_or("The AI settings are unavailable.")?;
            knowledge.require_current(&config, &self.profile.id, root, || {
                if self.cancellation.is_requested() {
                    return Err("The scheduled run was cancelled.".into());
                }
                require_vault_revision(self.vault_revision.as_ref()).map_err(str::to_owned)
            })?;
        }
        Ok(())
    }
}

fn require_vault_revision(revision: Option<&Weak<()>>) -> Result<(), &'static str> {
    if revision.is_some_and(|revision| revision.strong_count() == 0) {
        return Err("The SSH credential vault changed or was locked.");
    }
    Ok(())
}

fn require_saved_target(
    store: &Store,
    target: &Target,
    trust: &kortty_ssh::TrustStore,
    policy: &ClipboardPolicy,
) -> Result<(), &'static str> {
    let reference = &target.reference;
    if policy
        .server_access()
        .first_blocked(reference.target.servers(reference.jump.as_ref()))
        .is_some()
    {
        return Err("Managed policy blocks this SSH route.");
    }
    let current = store
        .get(reference.id)
        .and_then(|connection| Target::capture(reference, connection, trust).ok());
    if current.as_ref() != Some(target) {
        return Err("The reviewed SSH route or host keys changed. Review again.");
    }
    Ok(())
}

pub(super) fn dispatch_guard(
    authority: &ProposalAuthority<'_>,
    vault_revision: Option<Weak<()>>,
) -> Result<impl Fn() -> Result<(), String> + Send + 'static + use<>, String> {
    let action = authority
        .action
        .target_groups
        .is_some()
        .then(|| authority.action.clone());
    let guard = authority
        .skills
        .map(|setup| -> Result<DispatchGuard, String> {
            Ok(DispatchGuard {
                config: Arc::clone(&setup.config),
                kind: authority.kind,
                targets: authority.targets.to_vec(),
                pins: setup.pins.clone(),
                redactions: authority.prompt.redaction_policy().clone(),
                profile: authority.profile.clone(),
                transport: authority.transport.clone(),
                context_limit: authority.context_limit,
                connection_path: crate::profile::connections_path()
                    .map_err(|_| "The saved SSH connections are unavailable.")?,
                trust: crate::ssh_connection::trust_store()
                    .map_err(|_| "The SSH host-key store is unavailable.")?,
                vault_revision,
                knowledge: authority.prompt.knowledge().cloned(),
            })
        })
        .transpose()?;
    Ok(move || {
        if let Some(action) = &action {
            action.require_installed_target_groups()?;
        }
        guard.as_ref().map_or(Ok(()), DispatchGuard::verify)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn firing_guidance_keeps_other_targets_pins_out_of_automatic_selection() {
        let config: Configuration = serde_json::from_value(json!({"skills": {
            "last_id": 53,
            "skills": {
                "51": {"name": "Primary target", "target": "CONNECTION", "enabled": true,
                    "content": "Use the first connection's inventory."},
                "52": {"name": "Secondary target", "target": "AGENT", "enabled": true,
                    "tags": ["inventory"], "content": "Use the second connection's inventory."},
                "53": {"name": "General inventory", "target": "BOTH", "enabled": true,
                    "tags": ["inventory"], "content": "Check the inventory age."}
            }
        }}))
        .unwrap();
        let pins = [(1, vec![51]), (2, vec![52])];
        let automatic =
            automatic_firing_guidance(&config, &pins, "Inspect inventory", &[]).unwrap();
        let first = render_firing_guidance(&config, &pins, 1, &automatic)
            .unwrap()
            .text;
        let second = render_firing_guidance(&config, &pins, 2, &automatic)
            .unwrap()
            .text;
        assert!(first.contains("first connection") && !first.contains("second connection"));
        assert!(second.contains("second connection") && !second.contains("first connection"));
        assert!(first.contains("Check the inventory age."));
        assert!(second.contains("Check the inventory age."));
    }
}
