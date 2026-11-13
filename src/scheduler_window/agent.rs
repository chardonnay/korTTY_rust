//! Reviewed scheduled diagnostics and SSH command proposals.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::sync::Arc;

use kortty_jobs::{Job, JobId, RunRequest};
use kortty_ssh::TrustStore;
use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Serialize};

use crate::ai_manager::{
    Configuration, ProfileRef, ReviewedLocalAiProfile, SkillGuidance, SkillUsageReceipt,
    SkillUsageSurface,
};
use crate::connection_store::Connection;
use crate::terminal_agent::swarm::comparison::ScheduledGrant;
use crate::workspace::skill_classifier;

use super::{
    ConnectionRef, Message, Page, SchedulerView, SchedulerWindow, SshCommand, new_id, sync,
};

mod comparison_review;
pub(crate) mod knowledge;
pub(crate) mod proposal;
pub(crate) mod provider_mask;
pub(super) mod skill_review;
#[cfg(test)]
mod tests;

use skill_review::{Classification, SkillReview};

pub(super) const MAX_GOAL_BYTES: usize = 4096;
const MAX_DIRECTORY_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Agent,
    Swarm,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Probe {
    #[default]
    System,
    Filesystem,
    Proposed,
    PerFiring,
    SwarmReadOnly,
    SwarmIterative,
}

impl Probe {
    pub(super) fn available(kind: Kind) -> &'static [Self] {
        match kind {
            Kind::Agent => &[
                Self::System,
                Self::Filesystem,
                Self::Proposed,
                Self::PerFiring,
            ],
            Kind::Swarm => &[
                Self::SwarmReadOnly,
                Self::SwarmIterative,
                Self::System,
                Self::Filesystem,
                Self::Proposed,
            ],
        }
    }

    pub(crate) fn uses_model(self) -> bool {
        self == Self::Proposed || self.is_firing()
    }

    pub(crate) fn is_firing(self) -> bool {
        matches!(self, Self::PerFiring | Self::SwarmIterative)
    }

    pub(crate) fn supports_comparison(self) -> bool {
        matches!(self, Self::SwarmReadOnly | Self::SwarmIterative)
    }

    pub(crate) fn supports_parallelism(self) -> bool {
        matches!(self, Self::SwarmReadOnly | Self::SwarmIterative)
    }

    pub(crate) fn reviews_advice(self, profile: &ProfileChoice) -> bool {
        self == Self::SwarmReadOnly
            || (profile.requires_masked_review() && matches!(self, Self::System | Self::Filesystem))
    }

    pub(crate) fn supports_target_journal(self, kind: Kind) -> bool {
        matches!(
            (kind, self),
            (
                Kind::Agent,
                Self::System | Self::Filesystem | Self::Proposed | Self::PerFiring
            ) | (Kind::Swarm, Self::SwarmReadOnly | Self::SwarmIterative)
        )
    }

    fn command(self) -> Option<&'static str> {
        match self {
            Self::System | Self::SwarmReadOnly => Some(
                "LC_ALL=C /usr/bin/uname -srm && LC_ALL=C /usr/bin/id -un && LC_ALL=C /usr/bin/id -u && pwd -P",
            ),
            Self::Filesystem => Some("LC_ALL=C /bin/df -P ."),
            Self::Proposed | Self::PerFiring | Self::SwarmIterative => None,
        }
    }

    pub(crate) fn checked_command(self, directory: &str) -> Option<String> {
        self.command()
            .map(|command| checked_command(command, directory))
    }
}

pub(crate) fn checked_command(command: &str, directory: &str) -> String {
    let mut checked = String::from("test \"$(pwd -P)\" = ");
    super::shell_quote(&mut checked, directory);
    checked.push_str(" && ");
    checked.push_str(command);
    checked
}

pub(crate) fn initial_swarm_probe(directory: &str) -> Option<String> {
    let command = Probe::System.checked_command(directory)?;
    crate::terminal_agent::wrap_ssh_command(directory, &command).ok()
}

pub(crate) fn checked_proposal(command: &str, directory: &str) -> String {
    let mut invocation = String::from("exec /bin/sh -c ");
    super::shell_quote(&mut invocation, command);
    invocation.push_str(" </dev/null");
    checked_command(&invocation, directory)
}

impl fmt::Display for Probe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::System => "System and SSH account",
            Self::Filesystem => "Filesystem capacity for the working directory",
            Self::Proposed => "Model-proposed SSH commands · one run",
            Self::PerFiring => "Model proposes commands at each start",
            Self::SwarmReadOnly => "Read-only advice per SSH target",
            Self::SwarmIterative => "Iterative command proposals per SSH target",
        })
    }
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileChoice {
    pub(crate) id: ProfileRef,
    pub(crate) name: String,
    pub(crate) model: String,
    pub(crate) destination: String,
    // Keep the original receipt key for the independently checked policy authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) policy_model_fingerprint: Option<[u8; 32]>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) reviewed_https: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reviewed_cli: Option<[u8; 32]>,
}

impl fmt::Display for ProfileChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} · {}", self.name, self.model)?;
        if self.reviewed_https {
            formatter.write_str(" · HTTPS")?;
        } else if self.reviewed_cli.is_some() {
            formatter.write_str(" · CLI")?;
        }
        Ok(())
    }
}

impl ProfileChoice {
    pub(crate) fn available(config: &Configuration) -> Vec<Self> {
        config
            .reviewed_local_ai_profiles()
            .filter_map(|entry| {
                let id = config.profile_reference(entry.id)?;
                Self::from_reviewed(id, entry)
            })
            .chain(config.reviewed_agent_https_profiles().filter_map(
                |(id, name, model, destination)| {
                    let id = config.profile_reference(id)?;
                    Self::from_https(id, name, model, destination)
                },
            ))
            .chain(config.reviewed_agent_cli_profiles().filter_map(
                |(id, name, model, destination)| {
                    Self::from_cli(
                        config,
                        config.profile_reference(id)?,
                        name,
                        model,
                        destination,
                    )
                },
            ))
            .collect()
    }

    pub(crate) fn for_reference(config: &Configuration, reference: &ProfileRef) -> Option<Self> {
        let id = config.resolve_profile_reference(reference)?;
        if let Some(entry) = config
            .reviewed_local_ai_profiles()
            .find(|entry| entry.id == id)
        {
            return Self::from_reviewed(reference.clone(), entry);
        }
        if let Some((_, name, model, destination)) = config
            .reviewed_agent_https_profiles()
            .find(|(profile, _, _, _)| *profile == id)
        {
            return Self::from_https(reference.clone(), name, model, destination);
        }
        let (_, name, model, destination) = config
            .reviewed_agent_cli_profiles()
            .find(|(profile, _, _, _)| *profile == id)?;
        Self::from_cli(config, reference.clone(), name, model, destination)
    }

    fn from_reviewed(id: ProfileRef, entry: ReviewedLocalAiProfile<'_>) -> Option<Self> {
        let profile = Self {
            id,
            name: entry.name.to_owned(),
            model: entry.model.to_owned(),
            destination: entry.destination,
            policy_model_fingerprint: entry.policy_model_fingerprint,
            reviewed_https: false,
            reviewed_cli: None,
        };
        profile.valid().then_some(profile)
    }

    fn from_https(id: ProfileRef, name: &str, model: &str, destination: String) -> Option<Self> {
        let profile = Self {
            id,
            name: name.to_owned(),
            model: model.to_owned(),
            destination,
            policy_model_fingerprint: None,
            reviewed_https: true,
            reviewed_cli: None,
        };
        profile.valid().then_some(profile)
    }

    fn from_cli(
        config: &Configuration,
        id: ProfileRef,
        name: &str,
        model: &str,
        destination: String,
    ) -> Option<Self> {
        let revision = config.reviewed_agent_cli_revision(&id)?;
        let profile = Self {
            id,
            name: name.to_owned(),
            model: model.to_owned(),
            destination,
            policy_model_fingerprint: None,
            reviewed_https: false,
            reviewed_cli: Some(revision),
        };
        profile.valid().then_some(profile)
    }

    pub(crate) fn requires_masked_review(&self) -> bool {
        self.reviewed_https || self.reviewed_cli.is_some()
    }

    pub(crate) fn transport(
        &self,
        config: &Configuration,
    ) -> Result<(crate::local_ai::Profile, usize), String> {
        if let Some(revision) = self.reviewed_cli {
            if config.reviewed_agent_cli_revision(&self.id) != Some(revision) {
                return Err("The reviewed CLI profile changed. Review the job again.".into());
            }
            config.reviewed_agent_cli_transport(&self.id, &self.destination)
        } else if self.reviewed_https {
            config.reviewed_agent_https_transport(&self.id, &self.destination)
        } else {
            config.agent_transport_for_reference(&self.id)
        }
    }

    pub(crate) fn revision(
        &self,
        config: &Configuration,
        policy: &crate::terminal_logging::Policy,
    ) -> Option<[u8; 32]> {
        if let Some(revision) = self.reviewed_cli {
            if config.reviewed_agent_cli_revision(&self.id) != Some(revision) {
                return None;
            }
            return digest(&(
                "kortty scheduled CLI profile v1",
                revision,
                policy.redaction_revision()?,
            ))
            .ok();
        }
        let revision = config.agent_profile_reference_revision(&self.id)?;
        if self.reviewed_https {
            digest(&(
                "kortty scheduled HTTPS profile v1",
                revision,
                policy.redaction_revision()?,
            ))
            .ok()
        } else {
            Some(revision)
        }
    }

    pub(crate) fn policy_model_matches(&self, current: Option<[u8; 32]>) -> bool {
        self.policy_model_fingerprint == current
    }

    fn valid(&self) -> bool {
        self.id.is_nonzero()
            && (self.reviewed_cli.is_none()
                || (!self.reviewed_https
                    && matches!(self.id, ProfileRef::Numeric(_))
                    && self.policy_model_fingerprint.is_none()))
            && (!self.reviewed_https
                || (matches!(self.id, ProfileRef::Numeric(_))
                    && self.destination.starts_with("https://")
                    && self.policy_model_fingerprint.is_none()))
            && (!matches!(self.id, ProfileRef::Policy(_))
                || self.policy_model_fingerprint.is_some())
            && valid_text(&self.name, 256)
            && valid_text(&self.model, 256)
            && valid_text(&self.destination, 512)
    }
}

#[derive(Default)]
pub(super) struct ProfileChoices {
    pub(super) entries: Vec<ProfileChoice>,
    legacy_aliases: BTreeMap<u64, usize>,
}

impl ProfileChoices {
    pub(super) fn new(config: &Configuration) -> Self {
        let entries = ProfileChoice::available(config);
        let legacy_aliases = entries
            .iter()
            .enumerate()
            .filter(|(_, choice)| matches!(choice.id, ProfileRef::Policy(_)))
            .filter_map(|(index, choice)| {
                config
                    .resolve_profile_reference(&choice.id)
                    .and_then(|id| config.legacy_policy_alias(id))
                    .map(|id| (id, index))
            })
            .collect();
        Self {
            entries,
            legacy_aliases,
        }
    }

    pub(super) fn selected(&self, reference: &ProfileRef) -> Option<&ProfileChoice> {
        self.entries
            .iter()
            .find(|choice| &choice.id == reference)
            .or_else(|| match reference {
                ProfileRef::Numeric(id) => self
                    .legacy_aliases
                    .get(id)
                    .and_then(|index| self.entries.get(*index)),
                ProfileRef::Policy(_) => None,
            })
    }
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Action {
    pub(crate) kind: Kind,
    pub(crate) probe: Probe,
    pub(crate) profile: ProfileRef,
    pub(crate) goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) swarm_parallelism: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) approval: Option<Box<Approval>>,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Approval {
    scope: [u8; 32],
    pub(crate) profile: ProfileChoice,
    pub(crate) targets: Vec<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proposal: Option<proposal::Grant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generation: Option<GenerationGrant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    initial_swarm_probe: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) advisory: Option<AdvisoryGrant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) comparison: Option<ScheduledGrant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) knowledge: Option<Arc<knowledge::Grant>>,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum GenerationGrant {
    // Preserve the original on-disk receipt for one target.
    Single([u8; 32]),
    Ordered(Vec<GenerationReceipt>),
    Guided(GuidedGenerationGrant),
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationReceipt {
    target: u64,
    fingerprint: [u8; 32],
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "version", deny_unknown_fields)]
enum GuidedGenerationGrant {
    #[serde(rename = "firing-skills-v1")]
    Local {
        receipts: Vec<GuidedGenerationReceipt>,
    },
    #[serde(rename = "firing-skills-v2")]
    Classified {
        receipts: Vec<GuidedGenerationReceipt>,
        classified_skill_ids: Vec<u64>,
    },
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GuidedGenerationReceipt {
    target: u64,
    fingerprint: [u8; 32],
    masked_guidance: String,
}

impl GenerationGrant {
    fn matches_targets(&self, targets: &[Target]) -> bool {
        match self {
            Self::Single(_) => targets.len() == 1,
            Self::Ordered(receipts) => ordered_generation_targets_match(
                targets,
                2,
                receipts.iter().map(|receipt| receipt.target),
            ),
            Self::Guided(grant) => grant.matches_targets(targets),
        }
    }
}

impl GuidedGenerationGrant {
    fn receipts(&self) -> &[GuidedGenerationReceipt] {
        match self {
            Self::Local { receipts } | Self::Classified { receipts, .. } => receipts,
        }
    }

    fn skill_ids(&self) -> &[u64] {
        match self {
            Self::Local { .. } => &[],
            Self::Classified {
                classified_skill_ids,
                ..
            } => classified_skill_ids,
        }
    }

    fn matches_targets(&self, targets: &[Target]) -> bool {
        let receipts = self.receipts();
        (!matches!(self, Self::Classified { .. })
            || proposal::valid_firing_skill_ids(self.skill_ids()))
            && ordered_generation_targets_match(
                targets,
                1,
                receipts.iter().map(|receipt| receipt.target),
            )
            && receipts
                .iter()
                .all(|receipt| proposal::valid_firing_skill_guidance(&receipt.masked_guidance))
    }
}

fn ordered_generation_targets_match(
    targets: &[Target],
    minimum: usize,
    receipt_targets: impl Iterator<Item = u64>,
) -> bool {
    (minimum..=kortty_jobs::MAX_SWARM_TARGETS).contains(&targets.len())
        && receipt_targets.eq(targets.iter().map(|target| target.reference.id))
        && targets.iter().enumerate().all(|(index, target)| {
            !targets[..index]
                .iter()
                .any(|previous| previous.reference.id == target.reference.id)
        })
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdvisoryGrant {
    profile_revision: [u8; 32],
    question: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    guided: Option<GuidedAdvisoryGrant>,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GuidedAdvisoryGrant {
    version: AdvisoryVersion,
    receipts: Vec<GuidedGenerationReceipt>,
    pins: Vec<(u64, Vec<u64>)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    classified_skill_ids: Vec<u64>,
}

#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
enum AdvisoryVersion {
    #[serde(rename = "advisory-skills-v1")]
    V1,
}

impl GuidedAdvisoryGrant {
    fn matches_targets(&self, targets: &[Target]) -> bool {
        (self.classified_skill_ids.is_empty()
            || proposal::valid_firing_skill_ids(&self.classified_skill_ids))
            && ordered_generation_targets_match(
                targets,
                1,
                self.receipts.iter().map(|receipt| receipt.target),
            )
            && self
                .pins
                .iter()
                .map(|(target, _)| *target)
                .eq(self.receipts.iter().map(|receipt| receipt.target))
            && self.pins.iter().all(|(_, ids)| {
                ids.len() <= 32
                    && !ids.contains(&0)
                    && ids
                        .iter()
                        .enumerate()
                        .all(|(index, id)| !ids[..index].contains(id))
            })
            && self
                .receipts
                .iter()
                .all(|receipt| proposal::valid_firing_skill_guidance(&receipt.masked_guidance))
    }
}

impl AdvisoryGrant {
    pub(crate) fn profile_revision(&self) -> [u8; 32] {
        self.profile_revision
    }

    pub(crate) fn matches_question(&self, question: &str) -> bool {
        digest(&question).is_ok_and(|current| current == self.question)
    }

    pub(crate) fn is_guided(&self) -> bool {
        self.guided.is_some()
    }

    pub(crate) fn skill_guidance_for(&self, index: usize) -> Option<&str> {
        Some(
            self.guided
                .as_ref()?
                .receipts
                .get(index)?
                .masked_guidance
                .as_str(),
        )
    }

    pub(crate) fn skill_ids(&self) -> &[u64] {
        self.guided
            .as_ref()
            .map_or(&[], |grant| grant.classified_skill_ids.as_slice())
    }

    pub(crate) fn reviewed_pins(&self) -> Option<&[(u64, Vec<u64>)]> {
        self.guided.as_ref().map(|grant| grant.pins.as_slice())
    }

    pub(crate) fn matches_prompt(&self, index: usize, prompt: &proposal::Prompt) -> bool {
        let Some(guided) = &self.guided else {
            return false;
        };
        let Some(receipt) = guided.receipts.get(index) else {
            return false;
        };
        prompt.target_ids() == [receipt.target]
            && prompt.profile_revision() == Some(self.profile_revision)
            && prompt
                .masked_question()
                .is_some_and(|question| self.matches_question(&question))
            && prompt.firing_skill_guidance() == Some(receipt.masked_guidance.as_str())
            && prompt.firing_skill_ids() == self.skill_ids()
            && prompt
                .advisory_fingerprint(&guided.pins)
                .is_ok_and(|current| current == receipt.fingerprint)
    }

    fn matches_targets(&self, targets: &[Target]) -> bool {
        self.guided
            .as_ref()
            .is_none_or(|grant| grant.matches_targets(targets))
    }

    fn with_reviewed_prompts(
        mut self,
        prompts: &[proposal::Prompt],
        targets: &[Target],
        pins: Vec<(u64, Vec<u64>)>,
    ) -> Result<Self, String> {
        let first = prompts
            .first()
            .ok_or("The reviewed read-only Swarm requests are unavailable.")?;
        if prompts.len() != targets.len() {
            return Err("Review one read-only model request for each saved SSH target.".into());
        }
        let receipts = prompts.iter().zip(targets).map(|(prompt, target)| {
            if prompt.target_ids() != [target.reference.id]
                || prompt.profile_revision() != Some(self.profile_revision)
                || prompt.transport != first.transport
                || prompt.context_limit != first.context_limit
                || prompt.firing_skill_ids() != first.firing_skill_ids()
                || prompt.knowledge() != first.knowledge()
                || !prompt.redaction_policy().equivalent_redactions(first.redaction_policy())
                || !prompt.masked_question().is_some_and(|question| self.matches_question(&question))
            {
                return Err("The reviewed read-only requests do not match their targets and AI profile.");
            }
            let guidance = prompt.firing_skill_guidance().ok_or("The reviewed read-only skill guidance is unavailable.")?;
            let fingerprint = prompt.advisory_fingerprint(&pins).map_err(|_| "Could not identify the reviewed read-only request.")?;
            Ok(GuidedGenerationReceipt {
                target: target.reference.id,
                fingerprint,
                masked_guidance: guidance.to_owned(),
            })
        }).collect::<Result<Vec<_>, _>>()?;
        self.guided = Some(GuidedAdvisoryGrant {
            version: AdvisoryVersion::V1,
            receipts,
            pins,
            classified_skill_ids: first.firing_skill_ids().to_vec(),
        });
        if !self.matches_targets(targets) {
            return Err(
                "The reviewed read-only requests have invalid targets or skill assignments.".into(),
            );
        }
        Ok(self)
    }
}

impl Approval {
    pub(crate) fn has_initial_swarm_probe(&self) -> bool {
        self.initial_swarm_probe == Some(true)
    }

    /// An empty snapshot still identifies a guided approval.
    pub(crate) fn firing_skill_guidance_for(&self, index: usize) -> Option<&str> {
        let GenerationGrant::Guided(grant) = self.generation.as_ref()? else {
            return None;
        };
        if !grant.matches_targets(&self.targets) {
            return None;
        }
        Some(grant.receipts().get(index)?.masked_guidance.as_str())
    }

    pub(crate) fn firing_skill_ids(&self) -> &[u64] {
        match &self.generation {
            Some(GenerationGrant::Guided(grant)) => grant.skill_ids(),
            _ => &[],
        }
    }

    pub(crate) fn generation_matches_for(&self, index: usize, prompt: &proposal::Prompt) -> bool {
        let Some((generation, target)) = self.generation.as_ref().zip(self.targets.get(index))
        else {
            return false;
        };
        if !generation.matches_targets(&self.targets)
            || prompt.target_ids() != [target.reference.id]
            || self.has_initial_swarm_probe() != prompt.swarm_probe_command().is_some()
        {
            return false;
        }
        let (fingerprint, guidance) = match generation {
            GenerationGrant::Single(fingerprint) => (*fingerprint, None),
            GenerationGrant::Ordered(receipts) => (receipts[index].fingerprint, None),
            GenerationGrant::Guided(grant) => {
                let receipt = &grant.receipts()[index];
                (receipt.fingerprint, Some(receipt.masked_guidance.as_str()))
            }
        };
        prompt.knowledge() == self.knowledge.as_deref()
            && prompt.firing_skill_guidance() == guidance
            && prompt.firing_skill_ids() == self.firing_skill_ids()
            && prompt
                .generation_fingerprint()
                .is_ok_and(|current| current == fingerprint)
    }
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Target {
    pub(crate) reference: ConnectionRef,
    pub(crate) pins: Vec<String>,
    revision: [u8; 32],
}

impl Target {
    pub(crate) fn capture(
        reference: &ConnectionRef,
        connection: &Connection,
        trust: &TrustStore,
    ) -> Result<Self, &'static str> {
        if &ConnectionRef::from(connection) != reference {
            return Err("The saved SSH route changed.");
        }
        if connection.incognito
            || !connection.isolation.is_none()
            || connection.terminal.ai_agent_disabled
            || std::iter::once(&connection.login)
                .chain(connection.jump.iter())
                .any(|login| login.keyboard_interactive || login.auth.requires_interactive())
        {
            return Err(
                "The saved connection requires privacy, isolation or interactive login, or has agent execution disabled.",
            );
        }
        let pins = sync::pins(reference, trust)
            .map_err(|_| "Verify host keys for the saved SSH route first.")?;
        let revision = digest(&(
            &connection.login,
            &connection.jump,
            connection.startup.seconds(),
            connection.startup.attempts(),
        ))
        .map_err(|_| "Could not identify the reviewed SSH connection settings.")?;
        Ok(Self {
            reference: reference.clone(),
            pins: sync::fingerprints(&pins),
            revision,
        })
    }
}

impl Action {
    pub(crate) fn swarm_parallelism(&self) -> usize {
        usize::from(self.swarm_parallelism.unwrap_or(1))
    }

    pub(crate) fn validate(&self, action: &SshCommand) -> Result<(), &'static str> {
        if self.probe == Probe::PerFiring && self.kind != Kind::Agent {
            return Err("Per-start command proposals require a scheduled AI Agent action.");
        }
        if self.probe == Probe::SwarmReadOnly && self.kind != Kind::Swarm {
            return Err("Read-only advice per target requires a scheduled AI Swarm action.");
        }
        if self.probe == Probe::SwarmIterative && self.kind != Kind::Swarm {
            return Err("Iterative command proposals require a scheduled AI Swarm action.");
        }
        if let Some(parallelism) = self.swarm_parallelism {
            if !self.probe.supports_parallelism() {
                return Err("Target parallelism requires read-only or iterative AI Swarm.");
            }
            if !(1..=kortty_jobs::MAX_SWARM_PARALLELISM).contains(&usize::from(parallelism)) {
                return Err("Choose a Swarm target parallelism from 1 to 16.");
            }
        }
        let target_count = action.targets().count();
        let valid_count = match (self.kind, self.probe) {
            (Kind::Agent, Probe::PerFiring) => (1..=super::MAX_TARGETS).contains(&target_count),
            (Kind::Agent, _) => target_count == 1,
            (Kind::Swarm, _) => (1..=kortty_jobs::MAX_SWARM_TARGETS).contains(&target_count),
        };
        if action.connection.is_none() || !valid_count {
            return Err(match (self.kind, self.probe) {
                (Kind::Agent, Probe::PerFiring) => {
                    "Choose one to eight saved SSH targets for per-start proposals."
                }
                (Kind::Agent, _) => "Choose one saved SSH connection for this AI Agent workflow.",
                (Kind::Swarm, _) => "Choose 1 to 32 saved SSH connections for AI Swarm.",
            });
        }
        if !self.profile.is_nonzero() || !valid_text(&self.goal, MAX_GOAL_BYTES) {
            return Err("Choose an AI profile and enter a short diagnostic question.");
        }
        let directory = action
            .working_directory()
            .map_err(|_| "Choose an absolute remote working directory.")?
            .ok_or("Choose an absolute remote working directory.")?;
        if !directory.starts_with('/')
            || directory.len() > MAX_DIRECTORY_BYTES
            || !crate::terminal_agent::valid_target_label(directory)
            || (directory != "/"
                && directory[1..]
                    .split('/')
                    .any(|part| matches!(part, "" | "." | "..")))
        {
            return Err("Choose a canonical absolute remote working directory of at most 1 KiB.");
        }
        if let Some(approval) = &self.approval
            && (!approval.profile.valid()
                || (approval.profile.requires_masked_review()
                    && self.probe.reviews_advice(&approval.profile)
                    && !approval
                        .advisory
                        .as_ref()
                        .is_some_and(AdvisoryGrant::is_guided))
                || approval
                    .proposal
                    .as_ref()
                    .is_some_and(|grant| !grant.valid())
                || approval
                    .generation
                    .as_ref()
                    .is_some_and(|grant| !grant.matches_targets(&approval.targets))
                || approval
                    .advisory
                    .as_ref()
                    .is_some_and(|grant| !grant.matches_targets(&approval.targets))
                || (approval.comparison.is_some()
                    && (self.kind != Kind::Swarm || !self.probe.supports_comparison()))
                || approval.knowledge.as_ref().is_some_and(|grant| {
                    !grant.valid()
                        || (approval.generation.is_none()
                            && !approval
                                .advisory
                                .as_ref()
                                .is_some_and(AdvisoryGrant::is_guided))
                })
                || (approval.has_initial_swarm_probe() && approval.generation.is_none())
                || !(1..=super::connection::target_limit(self.kind == Kind::Swarm))
                    .contains(&approval.targets.len())
                || approval.targets.iter().any(|target| {
                    target.reference.validate().is_err()
                        || !(1..=2).contains(&target.pins.len())
                        || target.pins.iter().any(|pin| !valid_text(pin, 512))
                }))
        {
            return Err("The saved diagnostics approval is invalid.");
        }
        Ok(())
    }
}

fn valid_text(value: &str, limit: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= limit
        && value
            .chars()
            .all(|ch| crate::session_journal::is_display_character(ch) && ch != '\n' && ch != '\t')
}

fn digest(value: &impl Serialize) -> io::Result<[u8; 32]> {
    let bytes = serde_json::to_vec(value)?;
    let mut hash = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(io::Error::other)?;
    hash.update(&bytes);
    Ok(hash
        .finalize()
        .as_ref()
        .try_into()
        .expect("SHA-256 produces 32 bytes"))
}

fn generation_grant(
    prompts: &[proposal::Prompt],
    targets: &[Target],
) -> Result<GenerationGrant, String> {
    let first = prompts
        .first()
        .ok_or("The reviewed model requests are unavailable.")?;
    if prompts.len() != targets.len() {
        return Err("Review one model request for each saved SSH target.".into());
    }
    let guided = first.firing_skill_guidance().is_some();
    let receipts = prompts
        .iter()
        .zip(targets)
        .map(|(prompt, target)| {
            let guidance = prompt.firing_skill_guidance();
            if prompt.target_ids() != [target.reference.id]
                || prompt.transport != first.transport
                || prompt.context_limit != first.context_limit
                || prompt.profile_revision() != first.profile_revision()
                || guidance.is_some() != guided
                || prompt.firing_skill_ids() != first.firing_skill_ids()
                || prompt.knowledge() != first.knowledge()
                || !prompt
                    .redaction_policy()
                    .equivalent_redactions(first.redaction_policy())
            {
                return Err(
                    "The model requests do not match the ordered targets and reviewed AI profile.",
                );
            }
            let fingerprint = prompt
                .generation_fingerprint()
                .map_err(|_| "Could not identify the reviewed model request.")?;
            Ok((
                GenerationReceipt {
                    target: target.reference.id,
                    fingerprint,
                },
                guidance,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let generation = match (guided, receipts.as_slice()) {
        (true, _) => {
            let receipts = receipts
                .into_iter()
                .map(|(receipt, guidance)| GuidedGenerationReceipt {
                    target: receipt.target,
                    fingerprint: receipt.fingerprint,
                    masked_guidance: guidance.expect("checked guided request").into(),
                })
                .collect();
            GenerationGrant::Guided(match first.firing_skill_ids() {
                [] => GuidedGenerationGrant::Local { receipts },
                ids => GuidedGenerationGrant::Classified {
                    receipts,
                    classified_skill_ids: ids.to_vec(),
                },
            })
        }
        (false, [(receipt, _)]) => GenerationGrant::Single(receipt.fingerprint),
        (false, _) => {
            GenerationGrant::Ordered(receipts.into_iter().map(|(receipt, _)| receipt).collect())
        }
    };
    if !generation.matches_targets(targets) {
        return Err("The reviewed model requests have invalid SSH targets.".into());
    }
    Ok(generation)
}

fn reviewed_initial_swarm_probe(
    prompts: &[proposal::Prompt],
    directory: &str,
    swarm_iterative: bool,
) -> Result<bool, String> {
    let reviewed = prompts
        .first()
        .and_then(proposal::Prompt::swarm_probe_command);
    if prompts
        .iter()
        .any(|prompt| prompt.swarm_probe_command() != reviewed)
        || reviewed.is_some_and(|command| {
            !swarm_iterative || initial_swarm_probe(directory).as_deref() != Some(command)
        })
    {
        return Err("The reviewed requests do not match the fixed initial Swarm probe.".into());
    }
    Ok(reviewed.is_some())
}

fn scope(job: &Job<SshCommand>) -> Result<[u8; 32], String> {
    let command = job.action();
    let action = command
        .agent
        .as_ref()
        .ok_or("Choose a scheduled AI diagnostics action.")?;
    action.validate(command)?;
    let proposed = action.probe == Probe::Proposed;
    if proposed {
        sync::validate_schedule(job.schedule()).map_err(
            |_| "Model-proposed commands require a manual run or one date and one fixed time.",
        )?;
        if !job.dependencies().is_empty() {
            return Err("Model-proposed commands cannot run as a job dependency.".into());
        }
    } else if action.probe.is_firing() && !job.dependencies().is_empty() {
        return Err(
            "Per-start proposals require Run Now or a calendar schedule without dependencies."
                .into(),
        );
    }
    let targets: Vec<_> = command.targets().collect();
    let review = (
        match action.probe {
            Probe::Proposed => "kortty-agent-proposal-review-v1",
            Probe::PerFiring => "kortty-agent-per-start-review-v1",
            Probe::SwarmReadOnly => "kortty-swarm-read-only-review-v1",
            Probe::SwarmIterative => "kortty-swarm-iterative-review-v1",
            _ => "kortty-agent-diagnostics-v1",
        },
        job.id(),
        action.kind,
        action.probe,
        action
            .probe
            .checked_command(
                command
                    .working_directory
                    .as_deref()
                    .expect("validated directory"),
            )
            .unwrap_or_default(),
        &action.profile,
        &action.goal,
        &command.working_directory,
        targets,
        job.schedule(),
        job.mode(),
        job.dependencies(),
    );
    let receipt = match action.swarm_parallelism {
        Some(limit) if action.probe == Probe::SwarmReadOnly => {
            digest(&("kortty-swarm-read-only-parallel-review-v1", review, limit))
        }
        Some(limit) => digest(&("kortty-swarm-iterative-parallel-review-v1", review, limit)),
        None => digest(&review),
    };
    let receipt = receipt.and_then(|receipt| match &command.target_groups {
        Some(groups) => digest(&("kortty-agent-target-groups-v1", receipt, groups)),
        None => Ok(receipt),
    });
    receipt.map_err(|_| "Could not identify the exact diagnostics review.".into())
}

pub(crate) struct ReviewRequest {
    pub(crate) job: Job<SshCommand>,
    scope: [u8; 32],
    graph_revision: u64,
}

pub(super) struct Review {
    id: JobId,
    kind: Kind,
    action: Arc<SshCommand>,
    comparison_supported: bool,
    generation_ticket: Option<String>,
    pub(super) advisory_question: Option<String>,
    pub(super) command: String,
    pub(super) directory: String,
    pub(super) swarm_parallelism: Option<usize>,
    pub(super) approval: Approval,
    pub(super) model: Option<Model>,
    pub(super) comparison_review: Option<comparison_review::Review>,
}

impl Review {
    pub(super) fn per_firing(&self) -> bool {
        self.approval.generation.is_some()
    }

    pub(super) fn advisory(&self) -> bool {
        self.approval.advisory.is_some()
    }

    pub(super) fn standing(&self) -> bool {
        self.per_firing() || self.advisory()
    }

    pub(super) fn swarm_iterative(&self) -> bool {
        self.per_firing() && self.kind == Kind::Swarm
    }

    pub(super) fn supports_comparison(&self) -> bool {
        self.kind == Kind::Swarm && self.comparison_supported && self.model.is_some()
    }

    pub(super) fn firing_review_ticket(&self) -> Option<&str> {
        if self.comparison_review.is_some() || !self.knowledge_ready() {
            return None;
        }
        let original = self.generation_ticket.as_deref()?;
        match self.model.as_ref().and_then(|model| model.skills.as_ref()) {
            Some(skills) => skills.ready().then_some(skills.preview.as_str()),
            None => Some(original),
        }
    }

    pub(super) fn knowledge_ready(&self) -> bool {
        self.model
            .as_ref()
            .and_then(|model| model.knowledge.as_ref())
            .is_none_or(|review| review.ready())
    }
}

pub(super) struct Model {
    graph_revision: u64,
    pub(super) prompts: Vec<Arc<proposal::Prompt>>,
    pub(super) state: ModelState,
    pub(super) skills: Option<SkillReview>,
    pub(super) knowledge: Option<crate::workspace::terminal_agent::knowledge_review::Review>,
    knowledge_vault_revision: Option<std::sync::Weak<()>>,
}

impl Model {
    pub(super) fn has_reviewed_firing_guidance(&self) -> bool {
        self.prompts
            .first()
            .is_some_and(|prompt| prompt.firing_skill_guidance().is_some())
    }
}

pub(super) enum ModelState {
    Preview,
    Pending {
        ticket: String,
        _cancel: crate::local_ai::Cancel,
        skill_usage: Option<SkillUsageReceipt>,
    },
    Ready {
        ticket: String,
        items: Arc<Vec<proposal::Item>>,
    },
}

pub(crate) struct ProposalAuthority<'a> {
    pub(crate) kind: Kind,
    pub(crate) action: &'a SshCommand,
    pub(crate) profile: &'a ProfileChoice,
    pub(crate) transport: &'a crate::local_ai::Profile,
    pub(crate) context_limit: usize,
    pub(crate) targets: &'a [Target],
    pub(crate) prompt: &'a proposal::Prompt,
    pub(crate) prompts: &'a [Arc<proposal::Prompt>],
    pub(crate) standing: bool,
    pub(crate) advisory: bool,
    pub(crate) skills: Option<&'a ProposalSkillSetup>,
    pub(crate) comparison: Option<&'a ScheduledGrant>,
    pub(crate) knowledge: Option<&'a Arc<knowledge::Grant>>,
}

struct AdvisoryDraft {
    question: String,
    profile_revision: [u8; 32],
    pins: Option<Vec<(u64, Vec<u64>)>>,
}

pub(crate) struct ProposalSkillSetup {
    pub(crate) config: Arc<Configuration>,
    pub(crate) pins: Vec<(u64, Vec<u64>)>,
    pub(crate) context: String,
    pub(crate) classifier: Option<skill_classifier::Request>,
    pub(crate) one_use_guidance: SkillGuidance,
}

pub(crate) struct ClassifiedPrompts {
    pub(crate) prompts: Vec<proposal::Prompt>,
    pub(crate) one_use_guidance: SkillGuidance,
}

pub(super) struct Approved {
    pub(super) id: JobId,
    approval: Approval,
    items: Option<std::sync::Arc<Vec<proposal::Item>>>,
    masked_goal: Option<String>,
}

impl SchedulerWindow {
    pub(crate) fn begin_agent_review(&self, view: &SchedulerView) -> Result<ReviewRequest, String> {
        if view.page != Page::Action
            || view.confirm.is_some()
            || view.hosted_close.is_some()
            || !self.can_edit()
            || !self.can_run()
            || !self.draft_matches(view)
        {
            return Err("The selected job cannot be reviewed right now.".into());
        }
        let id = view.draft.id.clone().map_or_else(new_id, Ok)?;
        let job = self.build_current_draft(view, id)?;
        let scope = scope(&job)?;
        Ok(ReviewRequest {
            job,
            scope,
            graph_revision: self.graph_revision,
        })
    }

    pub(crate) fn finish_agent_review(
        &self,
        view: &mut SchedulerView,
        request: ReviewRequest,
        profile: ProfileChoice,
        targets: Vec<Target>,
    ) -> Result<(), String> {
        self.prepare_agent_review(view, request, profile, targets, Vec::new(), None)
    }

    pub(crate) fn finish_agent_proposal_review(
        &self,
        view: &mut SchedulerView,
        request: ReviewRequest,
        profile: ProfileChoice,
        targets: Vec<Target>,
        prompt: proposal::Prompt,
    ) -> Result<(), String> {
        self.prepare_agent_review(view, request, profile, targets, vec![prompt], None)
    }

    pub(crate) fn finish_agent_firing_review(
        &self,
        view: &mut SchedulerView,
        request: ReviewRequest,
        profile: ProfileChoice,
        targets: Vec<Target>,
        prompts: Vec<proposal::Prompt>,
    ) -> Result<(), String> {
        if !request
            .job
            .action()
            .agent
            .as_ref()
            .is_some_and(|agent| agent.probe.is_firing())
        {
            return Err(
                "Choose a per-start proposal workflow before reviewing these requests.".into(),
            );
        }
        self.prepare_agent_review(view, request, profile, targets, prompts, None)
    }

    #[cfg(all(test, unix))]
    pub(crate) fn finish_swarm_read_only_review(
        &self,
        view: &mut SchedulerView,
        request: ReviewRequest,
        profile: ProfileChoice,
        targets: Vec<Target>,
        masked_question: String,
        profile_revision: [u8; 32],
    ) -> Result<(), String> {
        self.prepare_agent_review(
            view,
            request,
            profile,
            targets,
            Vec::new(),
            Some(AdvisoryDraft {
                question: masked_question,
                profile_revision,
                pins: None,
            }),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finish_guided_advisory_review(
        &self,
        view: &mut SchedulerView,
        request: ReviewRequest,
        profile: ProfileChoice,
        targets: Vec<Target>,
        prompts: Vec<proposal::Prompt>,
        masked_question: String,
        profile_revision: [u8; 32],
        pins: Vec<(u64, Vec<u64>)>,
    ) -> Result<(), String> {
        self.prepare_agent_review(
            view,
            request,
            profile,
            targets,
            prompts,
            Some(AdvisoryDraft {
                question: masked_question,
                profile_revision,
                pins: Some(pins),
            }),
        )
    }

    fn prepare_agent_review(
        &self,
        view: &mut SchedulerView,
        request: ReviewRequest,
        profile: ProfileChoice,
        targets: Vec<Target>,
        prompts: Vec<proposal::Prompt>,
        advisory: Option<AdvisoryDraft>,
    ) -> Result<(), String> {
        let agent = request
            .job
            .action()
            .agent
            .as_ref()
            .ok_or("Choose scheduled diagnostics.")?;
        let directory = request
            .job
            .action()
            .working_directory
            .as_deref()
            .ok_or("Choose a working directory.")?;
        let guided_advisory = advisory
            .as_ref()
            .is_some_and(|advisory| advisory.pins.is_some());
        if (agent.probe.uses_model() || guided_advisory) == prompts.is_empty() {
            return Err("The reviewed model request is unavailable.".into());
        }
        let swarm_iterative = agent.probe == Probe::SwarmIterative;
        if prompts.iter().any(|prompt| {
            prompt.is_swarm_iterative() != swarm_iterative
                || prompt.is_advisory() != guided_advisory
        }) {
            return Err("The masked model requests do not match the reviewed workflow.".into());
        }
        for prompt in &prompts {
            prompt.cli_review_input(agent.probe != Probe::Proposed)?;
        }
        let initial_swarm_probe =
            reviewed_initial_swarm_probe(&prompts, directory, swarm_iterative)?;
        if agent.probe.reviews_advice(&profile) != advisory.is_some() {
            return Err("The reviewed diagnostic question is unavailable.".into());
        }
        let (advisory_question, advisory_grant) = if let Some(advisory) = advisory {
            let grant = AdvisoryGrant {
                profile_revision: advisory.profile_revision,
                question: digest(&advisory.question)
                    .map_err(|_| "Could not identify the reviewed read-only Swarm question.")?,
                guided: None,
            };
            let grant = if let Some(pins) = advisory.pins {
                grant.with_reviewed_prompts(&prompts, &targets, pins)?
            } else {
                grant
            };
            (Some(advisory.question), Some(grant))
        } else {
            (None, None)
        };
        if profile.id != agent.profile
            || !profile.valid()
            || !request
                .job
                .action()
                .targets()
                .eq(targets.iter().map(|target| &target.reference))
        {
            return Err("The AI profile or ordered SSH targets changed. Review again.".into());
        }
        let generation = match agent.probe {
            probe if probe.is_firing() => Some(generation_grant(&prompts, &targets)?),
            Probe::Proposed => {
                if !matches!(prompts.as_slice(), [prompt] if prompt
                    .target_ids()
                    .iter()
                    .copied()
                    .eq(targets.iter().map(|target| target.reference.id)))
                {
                    return Err("The model request does not match the ordered SSH targets.".into());
                }
                None
            }
            _ => None,
        };
        let generation_ticket = (generation.is_some() || advisory_grant.is_some())
            .then(crate::private_files::nonce)
            .transpose()
            .map_err(|_| "Could not identify the model request review.")?;
        let command = agent.probe.checked_command(directory).map_or_else(
            || format!("Working directory · {directory}"),
            |command| invocation(directory, &command),
        );
        let skills = (agent.probe == Probe::Proposed)
            .then(SkillReview::new)
            .transpose()?;
        let model = (!prompts.is_empty()).then(|| Model {
            graph_revision: request.graph_revision,
            prompts: prompts.into_iter().map(Arc::new).collect(),
            state: ModelState::Preview,
            skills,
            knowledge: None,
            knowledge_vault_revision: None,
        });
        let review = Review {
            id: request.job.id().clone(),
            kind: agent.kind,
            action: Arc::new(request.job.action().clone()),
            comparison_supported: agent.probe.supports_comparison(),
            generation_ticket,
            advisory_question,
            command,
            directory: directory.to_owned(),
            swarm_parallelism: agent
                .probe
                .supports_parallelism()
                .then(|| agent.swarm_parallelism()),
            approval: Approval {
                scope: request.scope,
                profile,
                targets,
                proposal: None,
                generation,
                initial_swarm_probe: initial_swarm_probe.then_some(true),
                advisory: advisory_grant,
                comparison: None,
                knowledge: None,
            },
            model,
            comparison_review: None,
        };
        self.agent_review_current(view, &review)?;
        view.agent_review = Some(review);
        view.status = Some(
            match agent.probe {
                Probe::Proposed => {
                    "Review the masked request before asking the model for a proposal."
                }
                Probe::PerFiring => "Review each target's masked request for future starts.",
                Probe::SwarmIterative if initial_swarm_probe => {
                    "Review the fixed SSH probe and each target's masked starting request."
                }
                Probe::SwarmIterative => {
                    "Review each target's masked starting request and the follow-up limits."
                }
                Probe::SwarmReadOnly => {
                    "Review the fixed probe and the masked advisory request for each SSH target."
                }
                _ if guided_advisory => {
                    "Review the fixed probe and the masked advisory request for each SSH target."
                }
                _ => "Review the fixed probe, profile and pinned SSH routes.",
            }
            .into(),
        );
        Ok(())
    }

    fn agent_review_current(&self, view: &SchedulerView, review: &Review) -> Result<(), String> {
        if view.page != Page::Action
            || view.confirm.is_some()
            || view.hosted_close.is_some()
            || !self.can_edit()
            || !self.can_run()
            || !self.draft_matches(view)
            || review
                .model
                .as_ref()
                .is_some_and(|model| model.graph_revision != self.graph_revision)
            || scope(&self.build_current_draft(view, review.id.clone())?)? != review.approval.scope
        {
            return Err("The scheduled AI action changed. Review it again.".into());
        }
        Ok(())
    }

    pub(crate) fn proposal_authority<'a>(
        &self,
        view: &'a SchedulerView,
    ) -> Result<Option<ProposalAuthority<'a>>, String> {
        let Some((review, model)) = view.proposal_review() else {
            return Ok(None);
        };
        self.agent_review_current(view, review)?;
        let prompt = model
            .prompts
            .first()
            .ok_or("The reviewed model request is unavailable.")?;
        Ok(Some(ProposalAuthority {
            kind: review.kind,
            action: &review.action,
            profile: &review.approval.profile,
            transport: &prompt.transport,
            context_limit: prompt.context_limit,
            targets: &review.approval.targets,
            prompt,
            prompts: &model.prompts,
            standing: review.standing(),
            advisory: review.advisory(),
            skills: model
                .skills
                .as_ref()
                .and_then(|skills| skills.setup.as_ref()),
            comparison: review.approval.comparison.as_ref(),
            knowledge: review.approval.knowledge.as_ref(),
        }))
    }

    pub(crate) fn pending_proposal_authority<'a>(
        &self,
        view: &'a SchedulerView,
        ticket: &str,
    ) -> Result<Option<ProposalAuthority<'a>>, String> {
        if !view.has_pending_proposal(ticket) {
            return Ok(None);
        }
        self.proposal_authority(view)
    }

    pub(crate) fn attach_proposal_skills(
        &self,
        view: &mut SchedulerView,
        setup: ProposalSkillSetup,
    ) -> Result<iced::Task<Message>, String> {
        let (review, _) = view
            .proposal_review()
            .ok_or("The model request is unavailable.")?;
        self.agent_review_current(view, review)?;
        let (model, standing) = view
            .preview_model_mut()
            .ok_or("The model request is unavailable.")?;
        if model.skills.is_none() {
            model.skills = Some(SkillReview::new()?);
        }
        let skills = model
            .skills
            .as_mut()
            .filter(|skills| skills.setup.is_none())
            .ok_or("The model request already has a skill review.")?;
        skills.setup = Some(setup);
        skills.proposal_quota(&model.prompts, standing)
    }

    pub(crate) fn update_proposal_skill_review(
        &self,
        view: &mut SchedulerView,
        message: &Message,
    ) -> Option<iced::Task<Message>> {
        let result = match message {
            Message::ReviewProposalSkills(preview) => {
                self.open_proposal_skill_review(view, preview)
            }
            Message::CancelProposalSkills(preview) => {
                Self::cancel_proposal_skill_review(view, preview)
            }
            Message::ProposalQuotaFinished(preview, token, forecast) => {
                if let Some((skills, ..)) = view.skill_preview_mut()
                    && skills.preview == *preview
                {
                    skills.quota.complete(*token, forecast.clone());
                }
                Ok(iced::Task::none())
            }
            Message::ProposalSkillQuotaFinished(preview, token, forecast) => {
                if let Some((skills, ..)) = view.skill_preview_mut()
                    && skills.preview == *preview
                    && let Classification::Preview { quota, .. } = &mut skills.classification
                {
                    quota.complete(*token, forecast.clone());
                }
                Ok(iced::Task::none())
            }
            _ => return None,
        };
        Some(result.unwrap_or_else(|error| {
            view.discard_proposal_review();
            view.status = Some(error);
            iced::Task::none()
        }))
    }

    fn open_proposal_skill_review(
        &self,
        view: &mut SchedulerView,
        preview: &str,
    ) -> Result<iced::Task<Message>, String> {
        if !view.skill_preview_closed(preview) {
            return Ok(iced::Task::none());
        }
        let (review, _) = view.proposal_review().expect("matching proposal preview");
        if !review.knowledge_ready() {
            return Ok(iced::Task::none());
        }
        self.agent_review_current(view, review)?;
        let (skills, prompts, _) = view.skill_preview_mut().expect("matching skill review");
        let prompt = prompts.first().ok_or("The model request is unavailable.")?;
        skills.open(prompt)
    }

    fn cancel_proposal_skill_review(
        view: &mut SchedulerView,
        preview: &str,
    ) -> Result<iced::Task<Message>, String> {
        let Some((skills, prompts, standing)) = view.skill_preview_mut() else {
            return Ok(iced::Task::none());
        };
        if skills.preview != preview {
            return Ok(iced::Task::none());
        }
        let consume = match skills.classification {
            Classification::Closed => return Ok(iced::Task::none()),
            Classification::Preview { .. } => false,
            Classification::Pending { .. } => true,
        };
        skills.close(consume)?;
        let task = skills.proposal_quota(prompts, standing)?;
        view.status = Some(
            if consume {
                "Skill classification canceled. Review the model requests with their local skill selection."
            } else {
                "Review the masked model requests."
            }
            .into(),
        );
        Ok(task)
    }

    pub(crate) fn request_proposal_skills(
        &self,
        view: &mut SchedulerView,
        preview: &str,
        before_dispatch: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) -> Result<iced::Task<Message>, String> {
        if !view.matches_proposal_skill_preview(preview) {
            return Ok(iced::Task::none());
        }
        let (review, _) = view.proposal_review().expect("matching skill preview");
        self.agent_review_current(view, review)?;
        let (skills, prompts, _) = view.skill_preview_mut().expect("matching skill review");
        let prompt = prompts.first().ok_or("The model request is unavailable.")?;
        let task = skills.request(prompt, before_dispatch)?;
        if matches!(skills.classification, Classification::Pending { .. }) {
            view.status = Some("Waiting for the reviewed skill classification…".into());
        }
        Ok(task)
    }

    pub(crate) fn finish_proposal_skills(
        &self,
        view: &mut SchedulerView,
        ticket: &str,
        updated: Option<ClassifiedPrompts>,
    ) -> Result<iced::Task<Message>, String> {
        if !view.has_pending_proposal_skills(ticket) {
            return Ok(iced::Task::none());
        }
        let (review, _) = view
            .proposal_review()
            .expect("matching pending classification");
        self.agent_review_current(view, review)?;
        let review = view.agent_review.as_mut().expect("matching pending review");
        let firing = review.per_firing();
        let standing = review.standing();
        let model = review
            .model
            .as_mut()
            .expect("matching pending classification");
        let skills = model.skills.as_mut().expect("matching skill review");
        let (generation, advisory) = if let Some(ClassifiedPrompts {
            prompts: updated, ..
        }) = &updated
        {
            if updated.len() != model.prompts.len()
                || model.prompts.iter().zip(updated).any(|(current, updated)| {
                    current.transport != updated.transport
                        || current.context_limit != updated.context_limit
                        || current.target_ids() != updated.target_ids()
                        || current.input != updated.input
                        || current.profile_revision() != updated.profile_revision()
                        || current.is_advisory() != updated.is_advisory()
                        || current.is_swarm_iterative() != updated.is_swarm_iterative()
                        || current.swarm_probe_command() != updated.swarm_probe_command()
                        || current.firing_skill_guidance().is_some()
                            != updated.firing_skill_guidance().is_some()
                        || current.knowledge() != updated.knowledge()
                        || (standing && updated.firing_skill_ids().is_empty())
                        || !current
                            .redaction_policy()
                            .equivalent_redactions(updated.redaction_policy())
                })
            {
                return Err("The reviewed model requests changed. Review them again.".into());
            }
            let generation = firing
                .then(|| generation_grant(updated, &review.approval.targets))
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
                        updated,
                        &review.approval.targets,
                        pins.to_vec(),
                    )
                })
                .transpose()?;
            (generation, advisory)
        } else {
            (None, None)
        };
        skills.close(true)?;
        let status = if let Some(updated) = updated {
            model.prompts = updated.prompts.into_iter().map(Arc::new).collect();
            if let Some(setup) = &mut skills.setup {
                setup.one_use_guidance = updated.one_use_guidance;
            }
            if let Some(generation) = generation {
                review.approval.generation = Some(generation);
            }
            if let Some(advisory) = advisory {
                review.approval.advisory = Some(advisory);
            }
            "Review the model requests with their classified skills."
        } else {
            "Skill classification was unavailable. Review the model requests with their local skill selection."
        };
        let task = skills.proposal_quota(&model.prompts, standing)?;
        view.status = Some(status.into());
        Ok(task)
    }

    #[cfg(test)]
    pub(crate) fn request_agent_proposal(
        &self,
        view: &mut SchedulerView,
    ) -> Result<iced::Task<Message>, String> {
        self.request_agent_proposal_guarded(view, || Ok(()))
    }

    pub(crate) fn request_agent_proposal_guarded(
        &self,
        view: &mut SchedulerView,
        before_dispatch: impl Fn() -> Result<(), String> + Send + 'static,
    ) -> Result<iced::Task<Message>, String> {
        let Some(mut review) = view.agent_review.take() else {
            return Ok(iced::Task::none());
        };
        self.agent_review_current(view, &review)?;
        if review.standing() {
            view.agent_review = Some(review);
            return Ok(iced::Task::none());
        }
        let Some(mut model) = review.model.take() else {
            view.agent_review = Some(review);
            return Ok(iced::Task::none());
        };
        if !matches!(model.state, ModelState::Preview) {
            review.model = Some(model);
            view.agent_review = Some(review);
            return Ok(iced::Task::none());
        }
        let [prompt] = model.prompts.as_slice() else {
            return Err("The one-use model request is unavailable.".into());
        };
        if let Some(skills) = &mut model.skills {
            if !skills.ready() {
                review.model = Some(model);
                view.agent_review = Some(review);
                return Ok(iced::Task::none());
            }
            if skills.quota.stale() {
                skills.renew_preview()?;
                let task = skills.proposal_quota(&model.prompts, false)?;
                review.model = Some(model);
                view.agent_review = Some(review);
                view.status = Some("Review the refreshed proposal quota before sending.".into());
                return Ok(task);
            }
        }
        let ticket =
            crate::private_files::nonce().map_err(|_| "Could not identify the model request.")?;
        let skill_usage = model.skills.as_ref().and_then(|skills| {
            let setup = skills.setup.as_ref()?;
            setup.config.skill_usage(
                &setup.one_use_guidance,
                &prompt.transport,
                SkillUsageSurface::Agent,
                false,
            )
        });
        let (progress, updates) = async_channel::bounded(1);
        let pending = Arc::clone(prompt).start_guarded(progress, review.kind, before_dispatch)?;
        model.state = ModelState::Pending {
            ticket: ticket.clone(),
            _cancel: pending.cancel,
            skill_usage,
        };
        review.model = Some(model);
        view.agent_review = Some(review);
        view.status = Some("Waiting for the reviewed model proposal…".into());
        let progress_ticket = ticket.clone();
        let progress = iced::Task::run(updates, move |update| {
            Message::AgentProposalProgress(progress_ticket.clone(), update)
        });
        Ok(progress.chain(iced::Task::perform(
            async move {
                pending
                    .reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The model proposal stopped.".into()))
                    .map(Arc::new)
            },
            move |result| Message::AgentProposed(ticket.clone(), result),
        )))
    }

    pub(super) fn finish_agent_proposal(
        &self,
        view: &mut SchedulerView,
        ticket: &str,
        result: Result<std::sync::Arc<Vec<proposal::Item>>, String>,
    ) -> Result<(), String> {
        if !view.has_pending_proposal(ticket) {
            return Ok(());
        }
        let mut review = view.agent_review.take().expect("matching pending proposal");
        self.agent_review_current(view, &review)?;
        let items = result?;
        let previous = std::mem::replace(
            &mut review.model.as_mut().expect("pending model").state,
            ModelState::Ready {
                ticket: ticket.into(),
                items,
            },
        );
        if let ModelState::Pending {
            skill_usage: Some(usage),
            ..
        } = previous
        {
            usage.record();
        }
        view.agent_review = Some(review);
        view.status =
            Some("Review each exact command and pinned SSH route before approving one run.".into());
        Ok(())
    }

    pub(super) fn approve_agent_review(
        &self,
        view: &mut SchedulerView,
        ticket: Option<&str>,
    ) -> Result<(), String> {
        let current_ticket = view
            .firing_review_ticket()
            .or_else(|| view.proposal_command_ticket());
        if current_ticket != ticket
            || (ticket.is_none() && view.proposal_review().is_some())
            || view
                .skill_preview()
                .is_some_and(|skills| skills.quota.stale())
        {
            return Ok(());
        }
        let Some(mut review) = view.agent_review.take() else {
            return Ok(());
        };
        self.agent_review_current(view, &review)?;
        let standing = review.standing();
        let advisory = review.advisory();
        let masked_goal = if review.approval.profile.requires_masked_review() {
            let goal = review
                .model
                .as_ref()
                .and_then(|model| model.prompts.first())
                .and_then(|prompt| prompt.masked_question_for_save())
                .filter(|goal| valid_text(goal, MAX_GOAL_BYTES))
                .ok_or("The reviewed masked question cannot be saved. Edit the question and review again.")?;
            Some(goal)
        } else {
            None
        };
        let items = if standing {
            None
        } else if let Some(model) = review.model {
            let ModelState::Ready { items, .. } = model.state else {
                return Err("Review the returned commands before approving the run.".into());
            };
            review.approval.proposal = Some(
                proposal::Grant::create(&review.approval, &items)
                    .map_err(|_| "Could not create the one-use approval.")?,
            );
            Some(items)
        } else {
            None
        };
        view.status = Some(
            if review.approval.comparison.is_some() {
                "Scheduled Swarm requests and comparison approved. Save the job to keep the review."
            } else if review.approval.has_initial_swarm_probe() || advisory {
                "Initial SSH probe and model requests approved. Save the job to keep the review."
            } else if standing {
                "Model request approved. Save the job to keep the review."
            } else if items.is_some() {
                "One run approved. Save the job to keep the review."
            } else {
                "Diagnostics approved. Save the job to keep the review."
            }
            .into(),
        );
        view.agent_approval = Some(Approved {
            id: review.id,
            approval: review.approval,
            items,
            masked_goal,
        });
        view.dirty = true;
        Ok(())
    }

    pub(crate) fn refresh_firing_approval_quota(
        &self,
        view: &mut SchedulerView,
        ticket: &str,
    ) -> Result<Option<iced::Task<Message>>, String> {
        if view.firing_review_ticket() != Some(ticket) {
            return Ok(Some(iced::Task::none()));
        }
        if !view
            .skill_preview()
            .is_some_and(|skills| skills.quota.stale())
        {
            return Ok(None);
        }
        let (review, _) = view.proposal_review().expect("matching firing review");
        self.agent_review_current(view, review)?;
        let (skills, prompts, standing) = view.skill_preview_mut().expect("matching skill review");
        skills.renew_preview()?;
        let task = skills.proposal_quota(prompts, standing)?;
        view.status =
            Some("Review the refreshed model quota before approving these requests.".into());
        Ok(Some(task))
    }

    pub(super) fn agent_save(
        &self,
        view: &SchedulerView,
        job: Job<SshCommand>,
    ) -> Result<Job<SshCommand>, String> {
        let Some(agent) = job.action().agent.as_ref() else {
            return Ok(job);
        };
        let current = scope(&job)?;
        let mut action = job.action().clone();
        let approval = if let Some(Approved {
            approval,
            items,
            masked_goal,
            ..
        }) = &view.agent_approval
        {
            if approval.scope != current || approval.profile.id != agent.profile {
                return Err("The diagnostics settings changed. Review them again.".into());
            }
            let mut approval = approval.clone();
            if let Some(goal) = masked_goal.as_ref().filter(|goal| *goal != &agent.goal) {
                let action_agent = action.agent.as_mut().expect("diagnostics action");
                action_agent.goal.clone_from(goal);
                action_agent.approval = None;
                let masked_job = job
                    .definition()
                    .with_action(action.clone())
                    .map_err(|error| error.to_string())?;
                approval.scope = scope(&masked_job)?;
                if let Some(items) = items {
                    approval.proposal = Some(
                        approval
                            .proposal
                            .as_ref()
                            .ok_or("The reviewed one-use approval is unavailable.")?
                            .bind_saved_scope(&approval, items)
                            .map_err(|_| "Could not create the one-use approval.")?,
                    );
                }
            }
            if let Some(items) = items {
                self.proposal_store
                    .save(&approval, items)
                    .map_err(|_| "Could not save the private one-use proposal.")?;
            }
            Some(Box::new(approval))
        } else {
            agent
                .approval
                .as_ref()
                .filter(|approval| {
                    approval.scope == current && approval.profile.id == agent.profile
                })
                .cloned()
        };
        if approval.is_none()
            && self
                .agent_profiles
                .entries
                .iter()
                .any(|profile| profile.id == agent.profile && profile.requires_masked_review())
        {
            return Err("Review the masked provider request before saving this job.".into());
        }
        action.agent.as_mut().expect("diagnostics action").approval = approval;
        job.definition()
            .with_action(action)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn agent_run_approved(&self, request: &RunRequest<SshCommand>) -> bool {
        let Some(job) = self.scheduler.graph().job(&request.job) else {
            return false;
        };
        job.action() == request.action.as_ref()
            && request
                .action
                .agent
                .as_ref()
                .and_then(|agent| agent.approval.as_ref().map(|approval| (agent, approval)))
                .is_some_and(|(agent, approval)| {
                    let probe = agent.probe;
                    let firing = probe.is_firing();
                    let binds_order = firing
                        || approval
                            .advisory
                            .as_ref()
                            .is_some_and(AdvisoryGrant::is_guided);
                    agent.profile == approval.profile.id
                        && (probe == Probe::Proposed) == approval.proposal.is_some()
                        && firing == approval.generation.is_some()
                        && (!approval.has_initial_swarm_probe() || probe == Probe::SwarmIterative)
                        && (!binds_order
                            || request
                                .action
                                .targets()
                                .eq(approval.targets.iter().map(|target| &target.reference)))
                        && probe.reviews_advice(&approval.profile) == approval.advisory.is_some()
                        && scope(job).is_ok_and(|scope| scope == approval.scope)
                })
    }

    pub(crate) fn active_agent_run_approved(&self, request: &RunRequest<SshCommand>) -> bool {
        self.execution_allowed
            && !self.draining
            && !request.cancellation.is_requested()
            && self.active.get(&request.id).is_some_and(|active| {
                active.job == request.job
                    && active.started_at == request.started_at
                    && active.trigger == request.trigger
                    && !active.cancellation.is_requested()
            })
            && self.agent_run_approved(request)
    }

    pub(crate) fn active_swarm_comparison_approved(
        &self,
        request: &RunRequest<SshCommand>,
    ) -> bool {
        self.active_agent_run_approved(request)
            && self.active.get(&request.id).is_some_and(|active| {
                active
                    .cancellation
                    .shares_signal_with(&request.cancellation)
            })
            && request.action.agent.as_ref().is_some_and(|agent| {
                agent.kind == Kind::Swarm
                    && agent.probe.supports_comparison()
                    && agent
                        .approval
                        .as_ref()
                        .is_some_and(|approval| approval.comparison.is_some())
            })
    }

    pub(crate) fn agent_proposal_run(
        &self,
        request: &RunRequest<SshCommand>,
    ) -> Result<Option<proposal::Run>, &'static str> {
        let action = request
            .action
            .agent
            .as_ref()
            .ok_or("The scheduled AI action is unavailable.")?;
        if action.probe != Probe::Proposed {
            return Ok(None);
        }
        let approval = action
            .approval
            .as_ref()
            .ok_or("Review and approve a new proposal before running.")?;
        self.proposal_store.load(&request.job, approval).map(Some)
            .map_err(|_| "The one-use proposal is unavailable or consumed. Review and approve a new proposal.")
    }
}

impl SchedulerView {
    pub(crate) fn advisory_approval(&self) -> Option<&Approval> {
        let review = self.agent_review.as_ref()?;
        review.approval.advisory.as_ref()?;
        Some(&review.approval)
    }

    pub(crate) fn firing_review_ticket(&self) -> Option<&str> {
        self.agent_review.as_ref()?.firing_review_ticket()
    }

    pub(crate) fn discard_proposal_review(&mut self) {
        self.agent_review = None;
    }

    pub(crate) fn reviewed_agent_kind(&self) -> Option<Kind> {
        self.agent_review.as_ref().map(|review| review.kind)
    }

    fn proposal_review(&self) -> Option<(&Review, &Model)> {
        let review = self.agent_review.as_ref()?;
        let model = review.model.as_ref()?;
        Some((review, model))
    }

    fn skill_preview(&self) -> Option<&SkillReview> {
        let (_, model) = self.proposal_review()?;
        model
            .skills
            .as_ref()
            .filter(|_| matches!(model.state, ModelState::Preview))
    }

    fn preview_model_mut(&mut self) -> Option<(&mut Model, bool)> {
        let review = self.agent_review.as_mut()?;
        let standing = review.standing();
        let model = review.model.as_mut()?;
        matches!(model.state, ModelState::Preview).then_some((model, standing))
    }

    fn skill_preview_mut(&mut self) -> Option<(&mut SkillReview, &[Arc<proposal::Prompt>], bool)> {
        let (model, standing) = self.preview_model_mut()?;
        Some((model.skills.as_mut()?, &model.prompts, standing))
    }

    fn skill_preview_closed(&self, preview: &str) -> bool {
        self.agent_review
            .as_ref()
            .is_some_and(|review| review.comparison_review.is_none())
            && self.skill_preview().is_some_and(|skills| {
                skills.preview == preview && matches!(skills.classification, Classification::Closed)
            })
    }

    pub(crate) fn matches_proposal_preview(&self, preview: &str) -> bool {
        self.proposal_review()
            .is_some_and(|(review, _)| !review.standing() && review.knowledge_ready())
            && self.skill_preview_closed(preview)
    }

    pub(crate) fn matches_proposal_skill_preview(&self, preview: &str) -> bool {
        self.skill_preview().is_some_and(|skills| {
            skills.preview == preview
                && matches!(skills.classification, Classification::Preview { .. })
        })
    }

    pub(crate) fn has_pending_proposal_skills(&self, ticket: &str) -> bool {
        self.skill_preview().is_some_and(|skills| {
            matches!(&skills.classification, Classification::Pending { ticket: active, .. } if active == ticket)
        })
    }

    #[cfg(test)]
    pub(crate) fn proposal_preview_ticket(&self) -> Option<&str> {
        self.skill_preview().map(|skills| skills.preview.as_str())
    }

    #[cfg(test)]
    pub(crate) fn proposal_quota_label(&self) -> Option<&str> {
        self.skill_preview()?.quota.label()
    }

    #[cfg(test)]
    pub(crate) fn reviewed_proposal_items(&self) -> Option<&[proposal::Item]> {
        let (_, model) = self.proposal_review()?;
        match &model.state {
            ModelState::Ready { items, .. } => Some(items.as_slice()),
            _ => None,
        }
    }

    pub(crate) fn has_pending_proposal(&self, ticket: &str) -> bool {
        self.proposal_review().is_some_and(|(_, model)| {
            matches!(&model.state, ModelState::Pending { ticket: active, .. } if active == ticket)
        })
    }

    pub(crate) fn proposal_command_ticket(&self) -> Option<&str> {
        let (_, model) = self.proposal_review()?;
        match &model.state {
            ModelState::Ready { ticket, .. } => Some(ticket),
            _ => None,
        }
    }

    pub(crate) fn proposal_mask_current(
        &self,
        redactor: &crate::terminal_logging::Redactor,
        mask: proposal::Mask,
    ) -> bool {
        let Some((_, model)) = self.proposal_review() else {
            return true;
        };
        model
            .prompts
            .iter()
            .all(|prompt| prompt.mask_current(redactor))
            && match &model.state {
                ModelState::Ready { items, .. } => items.iter().all(|item| {
                    mask(redactor, &item.command).is_ok_and(|masked| masked == item.command)
                        && mask(redactor, &item.purpose).is_ok_and(|masked| masked == item.purpose)
                }),
                _ => true,
            }
    }
}

pub(super) fn invocation(directory: &str, command: &str) -> String {
    let mut line = String::from("cd ");
    super::shell_quote(&mut line, directory);
    line.push_str(" && exec /bin/sh -c ");
    super::shell_quote(&mut line, command);
    line
}

#[cfg(test)]
mod generation_tests {
    use serde_json::json;

    use super::*;
    use crate::terminal_logging::{Policy, Redactor, Secrets};

    #[test]
    fn scheduled_agent_policy_model_choice_preserves_legacy_proof_bytes() {
        let legacy = br#"{"id":7,"name":"Fixture","model":"policy-team-model","destination":"Managed llama.cpp on this computer"}"#;
        let mut profile: ProfileChoice = serde_json::from_slice(legacy).unwrap();
        assert_eq!(serde_json::to_vec(&profile).unwrap(), legacy);
        let legacy = profile.clone();
        assert!(!profile.policy_model_matches(Some([1; 32])));
        profile.policy_model_fingerprint = Some([1; 32]);
        assert!(profile != legacy);
        let reopened: ProfileChoice =
            serde_json::from_slice(&serde_json::to_vec(&profile).unwrap()).unwrap();
        assert!(reopened == profile && reopened.policy_model_matches(Some([1; 32])));
        assert!(!reopened.policy_model_matches(Some([2; 32])));
        assert!(!reopened.policy_model_matches(None));
    }

    #[test]
    fn scheduled_profile_picker_preserves_legacy_wire_identity_and_uses_exact_named_choices() {
        let policy = crate::clipboard_policy::parse(
            "[meta]\nschema-version = 1\n[[ai-profile]]\nid = 'policy-Fixture'\n\
             name = 'Fixture'\nprovider = 'lm-studio'\nendpoint = 'http://127.0.0.1:1235'\nmodel = 'model'\n",
        )
        .unwrap();
        let config = Configuration::default()
            .with_policy_profiles(&policy)
            .unwrap();
        let named = ProfileRef::Policy("policy-Fixture".into());
        let runtime = config.resolve_profile_reference(&named).unwrap();
        let alias = config.legacy_policy_alias(runtime).unwrap();
        let legacy = ProfileRef::Numeric(alias);
        let picker = ProfileChoices::new(&config);
        let selected = picker.selected(&named).unwrap();
        assert_eq!(selected.id, named);
        assert!(selected.policy_model_fingerprint.is_some());
        assert!(
            picker
                .selected(&legacy)
                .is_some_and(|choice| choice == selected)
        );
        let reopened = ProfileChoice::for_reference(&config, &legacy).unwrap();
        assert_eq!(reopened.id, legacy);
        assert_eq!(
            reopened.policy_model_fingerprint,
            selected.policy_model_fingerprint
        );

        let wrong_case = ProfileRef::Policy("policy-fixture".into());
        assert!(picker.selected(&wrong_case).is_none());
        assert!(ProfileChoice::for_reference(&config, &wrong_case).is_none());
        let missing = Configuration::default();
        assert!(ProfileChoices::new(&missing).selected(&named).is_none());
        assert!(ProfileChoice::for_reference(&missing, &named).is_none());

        for profiles in [
            json!([]),
            json!([{
                "id": alias, "name": "User profile", "model": "user-model",
                "endpoint": "http://127.0.0.1:1235"
            }]),
        ] {
            let has_user = !profiles.as_array().unwrap().is_empty();
            let config: Configuration = serde_json::from_value(json!({
                "profiles": {
                    "profiles": profiles, "last_id": u64::MAX,
                    "default": has_user.then_some(alias)
                }
            }))
            .unwrap();
            let config = config.with_policy_profiles(&policy).unwrap();
            let runtime = config.resolve_profile_reference(&named).unwrap();
            assert!(runtime > u128::from(u64::MAX));
            assert_eq!(config.legacy_policy_alias(runtime), None);
            let picker = ProfileChoices::new(&config);
            assert_eq!(picker.selected(&named).unwrap().id, named);
            assert_eq!(
                picker.selected(&legacy).map(|choice| &choice.id),
                has_user.then_some(&legacy)
            );
            assert_eq!(
                ProfileChoice::for_reference(&config, &legacy).map(|choice| choice.model),
                has_user.then(|| "user-model".to_owned())
            );
        }
    }

    fn target(id: u64) -> Target {
        Target {
            reference: serde_json::from_value(json!({
                "id": id,
                "target": {"host": format!("host-{id}.example"), "port": 22, "username": "operator"},
            }))
            .unwrap(),
            pins: vec!["SHA256:fixture".into()],
            revision: [5; 32],
        }
    }

    fn prompt(id: u64, goal: &str, revision: [u8; 32]) -> proposal::Prompt {
        proposal::Prompt::new(
            crate::local_ai::Profile::new("127.0.0.1:11434", "fixture").unwrap(),
            4096,
            goal,
            "/srv",
            vec![id],
            Redactor::for_journal(Secrets::default().seal().unwrap(), Policy::default()),
            |_, text| Ok(text.into()),
        )
        .unwrap()
        .bind_profile(revision)
    }

    fn approval(targets: Vec<Target>, generation: GenerationGrant) -> Approval {
        Approval {
            scope: [4; 32],
            profile: ProfileChoice {
                id: ProfileRef::Numeric(7),
                name: "Fixture".into(),
                model: "fixture".into(),
                destination: "127.0.0.1:11434".into(),
                policy_model_fingerprint: None,
                reviewed_https: false,
                reviewed_cli: None,
            },
            targets,
            proposal: None,
            generation: Some(generation),
            initial_swarm_probe: None,
            advisory: None,
            comparison: None,
            knowledge: None,
        }
    }

    #[test]
    fn per_start_prompt_receipts_bind_each_exact_target_in_order() {
        let targets = vec![target(9), target(3)];
        let prompts = [
            prompt(9, "Inspect the first host", [5; 32]),
            prompt(3, "Inspect the second host", [5; 32]),
        ];
        let reversed = [targets[1].clone(), targets[0].clone()];
        assert!(generation_grant(&prompts, &reversed).is_err());
        assert!(generation_grant(&prompts[..1], &targets).is_err());
        let mismatched_profile = [
            prompt(9, "Inspect the first host", [5; 32]),
            prompt(3, "Inspect the second host", [6; 32]),
        ];
        assert!(generation_grant(&mismatched_profile, &targets).is_err());
        let generation = generation_grant(&prompts, &targets).unwrap();
        let encoded = serde_json::to_value(approval(targets, generation)).unwrap();
        assert_eq!(
            encoded["generation"],
            json!([
                {"target": 9, "fingerprint": prompts[0].generation_fingerprint().unwrap()},
                {"target": 3, "fingerprint": prompts[1].generation_fingerprint().unwrap()},
            ])
        );
        let approval: Approval = serde_json::from_value(encoded).unwrap();
        assert_eq!(approval.firing_skill_guidance_for(0), None);
        assert!(approval.generation_matches_for(0, &prompts[0]));
        assert!(approval.generation_matches_for(1, &prompts[1]));
        assert!(!approval.generation_matches_for(0, &prompts[1]));
        assert!(!approval.generation_matches_for(1, &prompts[0]));
        assert!(!approval.generation_matches_for(2, &prompts[0]));
        assert!(!approval.generation_matches_for(1, &prompt(3, "Changed question", [5; 32])));
    }

    #[test]
    fn legacy_single_target_receipt_keeps_its_serialized_digest() {
        let targets = vec![target(9)];
        let prompts = [prompt(9, "Inspect the host", [5; 32])];
        let old_digest = prompts[0].generation_fingerprint().unwrap();
        let generation = generation_grant(&prompts, &targets).unwrap();
        let encoded = serde_json::to_value(approval(targets, generation)).unwrap();
        assert_eq!(encoded["generation"], json!(old_digest));
        let approval: Approval = serde_json::from_value(encoded).unwrap();
        assert!(approval.generation_matches_for(0, &prompts[0]));
        assert!(!approval.generation_matches_for(1, &prompts[0]));
    }

    #[test]
    fn advisory_receipt_binds_the_standing_request_and_preserves_the_legacy_shape() {
        let goal = "Inspect both hosts";
        let revision = [5; 32];
        let question = digest(&goal).unwrap();
        let legacy = AdvisoryGrant {
            profile_revision: revision,
            question,
            guided: None,
        };
        assert_eq!(
            serde_json::to_value(&legacy).unwrap(),
            json!({"profile_revision": revision, "question": question})
        );
        let legacy: AdvisoryGrant =
            serde_json::from_value(serde_json::to_value(&legacy).unwrap()).unwrap();
        assert!(!legacy.is_guided() && legacy.matches_question(goal));

        let target_prompt = |target, guidance| {
            proposal::Prompt::new_advisory(
                crate::local_ai::Profile::new("127.0.0.1:11434", "fixture").unwrap(),
                4096,
                goal,
                "/srv",
                target,
                Redactor::for_journal(Secrets::default().seal().unwrap(), Policy::default()),
                |_, text| Ok(text.into()),
            )
            .unwrap()
            .bind_profile(revision)
            .with_firing_skill_guidance(guidance)
            .unwrap()
            .with_firing_skill_classification(&[42])
            .unwrap()
        };
        let targets = [target(9), target(3)];
        let prompts = [
            target_prompt(9, "First target guidance"),
            target_prompt(3, ""),
        ];
        let pins = vec![(9, vec![51]), (3, vec![])];
        assert!(generation_grant(&prompts, &targets).is_err());
        let grant = legacy
            .with_reviewed_prompts(&prompts, &targets, pins.clone())
            .unwrap();
        assert!(grant.is_guided());
        assert_eq!(grant.reviewed_pins(), Some(pins.as_slice()));
        assert_eq!(grant.skill_ids(), &[42]);
        assert_eq!(grant.skill_guidance_for(1), Some(""));
        assert!(grant.matches_prompt(0, &prompts[0]));
        assert!(grant.matches_prompt(1, &prompts[1]));
        assert!(!grant.matches_prompt(1, &prompts[0]));

        let mut edited = serde_json::to_value(grant).unwrap();
        edited["guided"]["pins"][0][1][0] = json!(52);
        let edited: AdvisoryGrant = serde_json::from_value(edited).unwrap();
        assert!(edited.matches_targets(&targets));
        assert!(!edited.matches_prompt(0, &prompts[0]));
        assert!(!edited.matches_prompt(1, &prompts[1]));
    }

    #[test]
    fn guided_single_target_receipt_preserves_an_empty_review() {
        let targets = vec![target(9)];
        let plain = prompt(9, "Inspect the host", [5; 32]);
        let guided = plain.clone().with_firing_skill_guidance("").unwrap();
        let grant = generation_grant(std::slice::from_ref(&guided), &targets).unwrap();
        let legacy = generation_grant(std::slice::from_ref(&plain), &targets).unwrap();
        assert!(!approval(targets.clone(), legacy).generation_matches_for(0, &guided));
        let encoded = serde_json::to_value(approval(targets, grant)).unwrap();
        assert_eq!(
            encoded["generation"],
            json!({
                "version": "firing-skills-v1",
                "receipts": [{
                    "target": 9,
                    "fingerprint": guided.generation_fingerprint().unwrap(),
                    "masked_guidance": "",
                }],
            })
        );
        let approval: Approval = serde_json::from_value(encoded).unwrap();
        assert_eq!(approval.firing_skill_guidance_for(0), Some(""));
        assert_eq!(approval.firing_skill_guidance_for(1), None);
        assert!(approval.generation_matches_for(0, &guided));
        assert!(!approval.generation_matches_for(0, &plain));
    }

    #[test]
    fn guided_ordered_receipt_binds_the_text_and_fingerprint_to_each_target() {
        let targets = vec![target(9), target(3)];
        let goal = "Inspect both hosts";
        let wire = initial_swarm_probe("/srv").unwrap();
        let base = |target| {
            prompt(target, goal, [5; 32])
                .for_swarm()
                .with_swarm_probe(&wire)
        };
        let first = base(9)
            .with_firing_skill_guidance("First target\n")
            .unwrap();
        let second = base(3).with_firing_skill_guidance("").unwrap();
        let mixed = [first.clone(), base(3)];
        assert!(generation_grant(&mixed, &targets).is_err());
        let grant = generation_grant(&[first.clone(), second.clone()], &targets).unwrap();
        let mut original = approval(targets, grant);
        original.initial_swarm_probe = Some(true);
        let encoded = serde_json::to_value(&original).unwrap();
        let command: SshCommand = serde_json::from_value(json!({
            "connection": original.targets[0].reference,
            "additional_connections": [original.targets[1].reference],
            "working_directory": "/srv",
            "agent": {
                "kind": "swarm", "probe": "swarm_iterative", "profile": 7, "goal": goal,
                "approval": encoded,
            },
        }))
        .unwrap();
        assert!(command.validated().is_ok());
        let parsed: Approval = serde_json::from_value(encoded.clone()).unwrap();
        assert!(parsed.has_initial_swarm_probe());
        assert_eq!(parsed.firing_skill_guidance_for(0), Some("First target\n"));
        assert_eq!(parsed.firing_skill_guidance_for(1), Some(""));
        assert!(parsed.generation_matches_for(0, &first));
        assert!(parsed.generation_matches_for(1, &second));
        assert!(!parsed.generation_matches_for(0, &second));

        let mut changed = encoded.clone();
        changed["generation"]["receipts"][0]["masked_guidance"] = json!("Tampered target");
        let tampered: Approval = serde_json::from_value(changed).unwrap();
        let altered = base(9)
            .with_firing_skill_guidance("Tampered target")
            .unwrap();
        assert!(!tampered.generation_matches_for(0, &first));
        assert!(!tampered.generation_matches_for(0, &altered));
        assert!(!parsed.generation_matches_for(0, &altered));

        let mut reordered = encoded.clone();
        reordered["generation"]["receipts"]
            .as_array_mut()
            .unwrap()
            .swap(0, 1);
        let reordered: Approval = serde_json::from_value(reordered).unwrap();
        assert_eq!(reordered.firing_skill_guidance_for(0), None);
        assert!(!reordered.generation_matches_for(0, &first));

        let mut duplicate = encoded.clone();
        duplicate["targets"][1] = duplicate["targets"][0].clone();
        duplicate["generation"]["receipts"][1]["target"] = json!(9);
        let duplicate: Approval = serde_json::from_value(duplicate).unwrap();
        assert_eq!(duplicate.firing_skill_guidance_for(0), None);
        assert!(!duplicate.generation_matches_for(0, &first));

        for invalid in [
            "x".repeat(proposal::MAX_FIRING_SKILL_GUIDANCE_BYTES + 1),
            String::from("\0"),
        ] {
            let mut corrupt = encoded.clone();
            corrupt["generation"]["receipts"][0]["masked_guidance"] = json!(invalid);
            let corrupt: Approval = serde_json::from_value(corrupt).unwrap();
            assert_eq!(corrupt.firing_skill_guidance_for(0), None);
            assert!(!corrupt.generation_matches_for(0, &first));
        }

        let mut unsupported = encoded;
        unsupported["generation"]["version"] = json!("firing-skills-v3");
        assert!(serde_json::from_value::<Approval>(unsupported).is_err());
    }

    #[test]
    fn classified_generation_receipt_binds_the_same_ordered_skill_ids_for_every_target() {
        let targets = vec![target(9), target(3)];
        let base = |id| {
            prompt(id, "Inspect both hosts", [5; 32])
                .with_firing_skill_guidance("Prefer read-only checks.")
                .unwrap()
        };
        let classified =
            [9, 3].map(|id| base(id).with_firing_skill_classification(&[15, 8]).unwrap());
        let substituted = base(3).with_firing_skill_classification(&[15, 6]).unwrap();
        assert!(generation_grant(&[classified[0].clone(), substituted.clone()], &targets).is_err());
        assert!(generation_grant(&[classified[0].clone(), base(3)], &targets).is_err());

        let grant = generation_grant(&classified, &targets).unwrap();
        let encoded = serde_json::to_value(approval(targets, grant)).unwrap();
        assert_eq!(
            encoded["generation"],
            json!({
                "version": "firing-skills-v2",
                "classified_skill_ids": [15, 8],
                "receipts": [
                    {
                        "target": 9,
                        "fingerprint": classified[0].generation_fingerprint().unwrap(),
                        "masked_guidance": "Prefer read-only checks.",
                    },
                    {
                        "target": 3,
                        "fingerprint": classified[1].generation_fingerprint().unwrap(),
                        "masked_guidance": "Prefer read-only checks.",
                    },
                ],
            })
        );
        let parsed: Approval = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), encoded);
        assert_eq!(parsed.firing_skill_ids(), [15, 8]);
        assert!(parsed.generation_matches_for(0, &classified[0]));
        assert!(parsed.generation_matches_for(1, &classified[1]));

        let mut wrong_target = encoded.clone();
        wrong_target["generation"]["receipts"][1]["target"] = json!(9);
        let wrong_target: Approval = serde_json::from_value(wrong_target).unwrap();
        assert_eq!(wrong_target.firing_skill_guidance_for(0), None);
        assert!(!wrong_target.generation_matches_for(0, &classified[0]));

        let mut tampered = encoded;
        tampered["generation"]["classified_skill_ids"] = json!([15, 6]);
        let tampered: Approval = serde_json::from_value(tampered).unwrap();
        assert_eq!(tampered.firing_skill_ids(), [15, 6]);
        assert_eq!(substituted.firing_system(), classified[1].firing_system());
        assert!(!tampered.generation_matches_for(1, &classified[1]));
        assert!(!tampered.generation_matches_for(1, &substituted));
    }

    #[test]
    fn classified_generation_receipt_rejects_invalid_skill_ids_and_extra_v1_fields() {
        let targets = vec![target(9)];
        let prompt = prompt(9, "Inspect the host", [5; 32])
            .with_firing_skill_guidance("Prefer read-only checks.")
            .unwrap()
            .with_firing_skill_classification(&[8])
            .unwrap();
        let grant = generation_grant(std::slice::from_ref(&prompt), &targets).unwrap();
        let encoded = serde_json::to_value(approval(targets, grant)).unwrap();

        for invalid in [json!([]), json!([0]), json!([8, 8]), json!([1, 2, 3])] {
            let mut corrupt = encoded.clone();
            corrupt["generation"]["classified_skill_ids"] = invalid;
            let corrupt: Approval = serde_json::from_value(corrupt).unwrap();
            assert_eq!(corrupt.firing_skill_guidance_for(0), None);
            assert!(!corrupt.generation_matches_for(0, &prompt));
        }

        let mut v1_with_extra_ids = encoded.clone();
        v1_with_extra_ids["generation"]["version"] = json!("firing-skills-v1");
        assert!(serde_json::from_value::<Approval>(v1_with_extra_ids).is_err());

        let mut missing_ids = encoded;
        missing_ids["generation"]
            .as_object_mut()
            .unwrap()
            .remove("classified_skill_ids");
        assert!(serde_json::from_value::<Approval>(missing_ids).is_err());
    }

    #[test]
    fn only_per_start_agent_accepts_multiple_agent_targets() {
        let action: SshCommand = serde_json::from_value(json!({
            "connection": target(9).reference,
            "additional_connections": [target(3).reference],
            "working_directory": "/srv",
            "agent": {"kind":"agent","probe":"per_firing","profile":7,"goal":"Inspect both hosts"},
        }))
        .unwrap();
        assert!(action.clone().validated().is_ok());
        let mut single_target_agent = action.clone();
        single_target_agent.agent.as_mut().unwrap().probe = Probe::System;
        assert!(single_target_agent.validated().is_err());
        let mut swarm = action;
        swarm.agent.as_mut().unwrap().kind = Kind::Swarm;
        assert!(swarm.validated().is_err());
    }

    #[test]
    fn iterative_swarm_accepts_single_target_and_binds_ordered_prompt_receipts() {
        let action: SshCommand = serde_json::from_value(json!({
            "connection": target(9).reference,
            "additional_connections": [target(3).reference],
            "working_directory": "/srv",
            "agent": {"kind":"swarm","probe":"swarm_iterative","profile":7,"goal":"Inspect both hosts"},
        }))
        .unwrap();
        assert!(action.clone().validated().is_ok());
        let mut agent = action.clone();
        agent.agent.as_mut().unwrap().kind = Kind::Agent;
        assert!(agent.validated().is_err());
        let mut singleton = action.clone();
        singleton.additional_connections.clear();
        let wire = initial_swarm_probe("/srv").unwrap();
        let single_target = vec![target(9)];
        let single_prompt = prompt(9, "Inspect both hosts", [5; 32])
            .for_swarm()
            .with_swarm_probe(&wire);
        let grant = generation_grant(std::slice::from_ref(&single_prompt), &single_target).unwrap();
        let mut approved = approval(single_target, grant);
        approved.initial_swarm_probe = Some(true);
        singleton.agent.as_mut().unwrap().approval = Some(Box::new(approved.clone()));
        let singleton = singleton.validated().unwrap();
        let singleton: SshCommand =
            serde_json::from_value(serde_json::to_value(&singleton).unwrap()).unwrap();
        assert!(singleton.clone().validated().is_ok());
        let mut singleton = singleton;
        let saved_approval = singleton.agent.as_mut().unwrap().approval.as_mut().unwrap();
        assert!(saved_approval.generation_matches_for(0, &single_prompt));
        assert!(!saved_approval.generation_matches_for(1, &single_prompt));
        saved_approval.generation = None;
        assert!(singleton.validated().is_err());

        let job = Job::new(JobId::new("iterative").unwrap(), "Swarm fixture", action).unwrap();
        assert!(scope(&job).is_ok());
        let dependent = job.with_dependencies(
            kortty_jobs::DependencyMode::Gate,
            vec![JobId::new("prerequisite").unwrap()],
            None,
        );
        assert!(scope(&dependent).is_err());

        let targets = vec![target(9), target(3)];
        let swarm = [
            prompt(9, "Inspect both hosts", [5; 32]).for_swarm(),
            prompt(3, "Inspect both hosts", [5; 32]).for_swarm(),
        ];
        let generation = generation_grant(&swarm, &targets).unwrap();
        let approval = approval(targets, generation);
        assert!(approval.generation_matches_for(0, &swarm[0]));
        assert!(approval.generation_matches_for(1, &swarm[1]));
        assert!(!approval.generation_matches_for(0, &prompt(9, "Inspect both hosts", [5; 32])));
    }

    #[test]
    fn scheduled_read_only_swarm_accepts_one_saved_target() {
        let action: SshCommand = serde_json::from_value(json!({
            "connection": target(9).reference,
            "working_directory": "/srv",
            "agent": {"kind":"swarm","probe":"swarm_read_only","profile":7,"goal":"Inspect the host"},
        })).unwrap();
        assert!(action.validated().is_ok());
    }

    #[test]
    fn scheduler_target_groups_bind_the_exact_group_source_into_agent_review() {
        let action: SshCommand = serde_json::from_value(json!({
            "connection": target(9).reference,
            "working_directory": "/srv",
            "agent": {"kind":"swarm","probe":"swarm_read_only","profile":7,"goal":"Inspect the host"},
        })).unwrap();
        let id = JobId::new("group-review").unwrap();
        let job = |action| Job::new(id.clone(), "Swarm fixture", action).unwrap();
        let legacy = scope(&job(action.clone())).unwrap();
        let mut grouped = action;
        grouped.target_groups = Some(crate::scheduler_window::TargetGroups {
            groups: vec!["Fleet".into()],
            explicit: Vec::new(),
        });
        let reviewed = scope(&job(grouped.clone())).unwrap();
        assert_ne!(reviewed, legacy);
        grouped
            .target_groups
            .as_mut()
            .unwrap()
            .groups
            .push("Fleet/Production".into());
        assert_ne!(scope(&job(grouped)).unwrap(), reviewed);
    }

    #[test]
    fn swarm_parallelism_is_validated_and_bound_to_the_saved_review() {
        use crate::scheduler_window::form::{Draft, Field};

        let action: SshCommand = serde_json::from_value(json!({
            "connection": target(9).reference,
            "additional_connections": [target(3).reference],
            "working_directory": "/srv",
            "agent": {"kind":"swarm","probe":"swarm_iterative","profile":7,"goal":"Inspect both hosts"},
        }))
        .unwrap();
        let id = JobId::new("swarm-parallel").unwrap();
        let job = Job::new(id.clone(), "Swarm fixture", action).unwrap();
        let mut draft = Draft::from_job(&job);
        let legacy = draft.build(id.clone()).unwrap();
        let action = legacy.action();
        let agent = action.agent.as_ref().unwrap();
        assert_eq!(agent.swarm_parallelism(), 1);
        assert!(
            serde_json::to_value(agent)
                .unwrap()
                .get("swarm_parallelism")
                .is_none()
        );
        let legacy_scope = digest(&(
            "kortty-swarm-iterative-review-v1",
            &id,
            agent.kind,
            agent.probe,
            String::new(),
            7_u64,
            &agent.goal,
            &action.working_directory,
            action.targets().collect::<Vec<_>>(),
            legacy.schedule(),
            legacy.mode(),
            legacy.dependencies(),
        ))
        .unwrap();
        assert_eq!(scope(&legacy).unwrap(), legacy_scope);

        let named = ProfileRef::Policy("policy-Fixture".into());
        let mut named_draft = Draft::from_job(&legacy);
        named_draft.select_agent_profile(named.clone());
        let named_job = named_draft.build(id.clone()).unwrap();
        let reopened = Draft::from_job(&named_job).build(id.clone()).unwrap();
        assert_eq!(reopened.action().agent.as_ref().unwrap().profile, named);
        assert_eq!(scope(&reopened).unwrap(), scope(&named_job).unwrap());
        assert_ne!(scope(&reopened).unwrap(), legacy_scope);
        named_draft.select_agent_profile(ProfileRef::Policy("policy-Other".into()));
        assert_ne!(
            scope(&named_draft.build(id.clone()).unwrap()).unwrap(),
            scope(&named_job).unwrap()
        );

        draft.set(Field::SwarmParallelism, "16".into());
        let parallel = draft.build(id.clone()).unwrap();
        assert_eq!(
            parallel
                .action()
                .agent
                .as_ref()
                .unwrap()
                .swarm_parallelism(),
            16
        );
        assert_ne!(scope(&parallel).unwrap(), legacy_scope);
        let mut draft = Draft::from_job(&parallel);
        assert_eq!(
            scope(&draft.build(id.clone()).unwrap()).unwrap(),
            scope(&parallel).unwrap()
        );
        draft.set(Field::SwarmParallelism, "2".into());
        assert_ne!(
            scope(&draft.build(id.clone()).unwrap()).unwrap(),
            scope(&parallel).unwrap()
        );
        for limit in ["0", "17"] {
            draft.set(Field::SwarmParallelism, limit.into());
            assert!(draft.build(id.clone()).is_err());
        }

        let mut other = action.clone();
        let agent = other.agent.as_mut().unwrap();
        agent.probe = Probe::SwarmReadOnly;
        agent.swarm_parallelism = Some(16);
        let read_only = legacy
            .definition()
            .with_action(other.clone().validated().unwrap())
            .unwrap();
        assert_eq!(
            Draft::from_job(&read_only)
                .build(id.clone())
                .unwrap()
                .action()
                .agent
                .as_ref()
                .unwrap()
                .swarm_parallelism(),
            16
        );
        assert_ne!(scope(&read_only).unwrap(), scope(&parallel).unwrap());
        other.agent.as_mut().unwrap().swarm_parallelism = None;
        let legacy_read_only = legacy.definition().with_action(other.clone()).unwrap();
        let reopened = Draft::from_job(&legacy_read_only)
            .build(id.clone())
            .unwrap();
        assert_eq!(scope(&reopened).unwrap(), scope(&legacy_read_only).unwrap());
        assert_eq!(
            reopened.action().agent.as_ref().unwrap().swarm_parallelism,
            None
        );
        assert_ne!(
            scope(&read_only).unwrap(),
            scope(&legacy_read_only).unwrap()
        );

        let fixed = other.agent.as_mut().unwrap();
        fixed.probe = Probe::System;
        fixed.swarm_parallelism = Some(16);
        assert!(other.validated().is_err());
    }

    #[test]
    fn scheduled_swarm_reviews_up_to_32_routes_without_expanding_other_jobs() {
        use crate::scheduler_window::form::{Draft, Source};

        let references = (1..=33).map(|id| target(id).reference).collect::<Vec<_>>();
        let action: SshCommand = serde_json::from_value(json!({
            "connection": references[0],
            "additional_connections": [references[1]],
            "working_directory": "/srv",
            "agent": {"kind":"swarm","probe":"swarm_iterative","profile":7,"goal":"Inspect fleet"},
        }))
        .unwrap();
        let id = JobId::new("swarm-large").unwrap();
        let job = Job::new(id.clone(), "Swarm fixture", action).unwrap();
        let mut draft = Draft::from_job(&job);
        let choice = |index: usize| crate::scheduler_window::ConnectionChoice::Saved {
            reference: references[index].clone(),
            name: format!("Target {}", index + 1),
        };
        for index in 2..32 {
            assert_eq!(draft.add_connection(choice(index)), Ok(true));
        }
        assert!(draft.add_connection(choice(32)).is_err());
        let saved = draft.build(id.clone()).unwrap();
        assert_eq!(saved.action().targets().count(), 32);
        assert_eq!(
            scope(&Draft::from_job(&saved).build(id.clone()).unwrap()).unwrap(),
            scope(&saved).unwrap()
        );
        let mut overflow = saved.action().clone();
        overflow.additional_connections.push(references[32].clone());
        assert!(overflow.validated().is_err());

        let mut plain = saved.action().clone();
        plain.agent = None;
        plain.command = "uname -a".into();
        assert!(plain.validated().is_err());
        let mut draft = Draft::from_job(&saved);
        draft.select_source(Source::Command);
        assert!(draft.add_connection(choice(32)).is_err());
        let mut agent = saved.action().clone();
        let action = agent.agent.as_mut().unwrap();
        action.kind = Kind::Agent;
        action.probe = Probe::PerFiring;
        assert!(agent.validated().is_err());

        let targets = (1..=32).map(target).collect::<Vec<_>>();
        let prompts = (1..=32)
            .map(|id| prompt(id, "Inspect fleet", [5; 32]).for_swarm())
            .collect::<Vec<_>>();
        let grant = generation_grant(&prompts, &targets).unwrap();
        assert!(grant.matches_targets(&targets));
        let mut reordered = targets;
        reordered.swap(15, 16);
        assert!(!grant.matches_targets(&reordered));
    }

    #[test]
    fn initial_swarm_probe_requires_the_same_exact_wire_command_for_all_targets() {
        let targets = vec![target(9), target(3)];
        let legacy = || {
            [
                prompt(9, "Inspect both hosts", [5; 32]).for_swarm(),
                prompt(3, "Inspect both hosts", [5; 32]).for_swarm(),
            ]
        };
        let wire = initial_swarm_probe("/srv").unwrap();
        assert!(wire.starts_with("cd /srv") || wire.starts_with("cd '/srv'"));
        assert!(wire.contains("pwd -P") && wire.contains("/usr/bin/uname"));
        assert!(initial_swarm_probe("relative").is_none());

        let legacy_prompts = legacy();
        assert!(!reviewed_initial_swarm_probe(&legacy_prompts, "/srv", true).unwrap());
        let legacy_grant = generation_grant(&legacy_prompts, &targets).unwrap();
        let encoded = serde_json::to_value(approval(targets.clone(), legacy_grant)).unwrap();
        assert!(encoded.get("initial_swarm_probe").is_none());
        let legacy_approval: Approval = serde_json::from_value(encoded).unwrap();
        assert!(!legacy_approval.has_initial_swarm_probe());
        assert!(legacy_approval.generation_matches_for(0, &legacy_prompts[0]));

        let [first, second] = legacy();
        let mixed = [first.with_swarm_probe(&wire), second];
        assert!(reviewed_initial_swarm_probe(&mixed, "/srv", true).is_err());
        let wrong_wire = initial_swarm_probe("/var/other").unwrap();
        let mismatched = legacy().map(|prompt| prompt.with_swarm_probe(&wrong_wire));
        assert!(reviewed_initial_swarm_probe(&mismatched, "/srv", true).is_err());
        let reviewed = legacy().map(|prompt| prompt.with_swarm_probe(&wire));
        assert!(reviewed_initial_swarm_probe(&reviewed, "/srv", false).is_err());
        assert!(!legacy_approval.generation_matches_for(0, &reviewed[0]));

        let grant = generation_grant(&reviewed, &targets).unwrap();
        let mut approved = approval(targets, grant);
        assert!(!approved.generation_matches_for(0, &reviewed[0]));
        approved.initial_swarm_probe = reviewed_initial_swarm_probe(&reviewed, "/srv", true)
            .unwrap()
            .then_some(true);
        let encoded = serde_json::to_value(approved).unwrap();
        assert_eq!(encoded["initial_swarm_probe"], true);
        let approved: Approval = serde_json::from_value(encoded).unwrap();
        assert!(approved.has_initial_swarm_probe());
        assert!(approved.generation_matches_for(0, &reviewed[0]));
        assert!(approved.generation_matches_for(1, &reviewed[1]));
        assert!(!approved.generation_matches_for(0, &legacy_prompts[0]));
    }
}
