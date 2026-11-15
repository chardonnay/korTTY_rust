//! Private profile receipts for automatic finishing and user-reviewed journal jobs.

use std::collections::VecDeque;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use pbkdf2::hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::ai_manager::ProfileRef;

use super::summary_job::Mode;
use super::summary_plan::EvidenceLimits;
use super::viewer::SummarySource;
use super::{check_path, invalid};

const FILE: &str = "journal-summary-jobs.json";
pub(crate) const MAX_PENDING: usize = 64;
pub(crate) const DEFAULT_INTERVAL: u16 = 5;
const VERSION: u8 = 1;
const MAX_CANCELLED_AUTOMATION: usize = 2_048;
const MAX_SOURCE_APPROVALS: usize = kortty_jobs::MAX_JOBS + 1;
const MAX_QUEUE_BYTES: u64 = 128 * 1024;
const MAX_FILE_BYTES: u64 = MAX_QUEUE_BYTES + 2 * 1024 * 1024;
const LOCK: &str = ".journal-summary-jobs.lock";

fn valid_evidence_limits(limits: Option<EvidenceLimits>) -> bool {
    limits.is_none_or(|limits| limits.valid())
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AutomationSource {
    Job(kortty_jobs::JobId),
    Swarm,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceApproval {
    pub(crate) grant: AutomaticApproval,
    pub(crate) full_output: bool,
    key: [u8; 32],
}

impl std::fmt::Debug for SourceApproval {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceApproval")
            .field("grant", &self.grant)
            .field("full_output", &self.full_output)
            .finish_non_exhaustive()
    }
}

impl SourceApproval {
    fn new(grant: AutomaticApproval, full_output: bool) -> io::Result<Self> {
        let mut key = [0; 32];
        getrandom::fill(&mut key).map_err(io::Error::other)?;
        Ok(Self {
            grant,
            full_output,
            key,
        })
    }

    pub(crate) fn receipt(
        &self,
        source: &AutomationSource,
        run: &str,
        started: jiff::Timestamp,
        journal: &str,
    ) -> io::Result<SourceReceipt> {
        Ok(SourceReceipt {
            source: source.clone(),
            run: run.into(),
            started,
            evidence_limits: self.grant.evidence_limits,
            proof: self
                .mac(source, run, started, journal)?
                .finalize()
                .into_bytes()
                .into(),
        })
    }

    pub(crate) fn verifies_receipt(&self, receipt: &SourceReceipt, journal: &str) -> bool {
        receipt.started >= self.grant.since
            && receipt.evidence_limits == self.grant.evidence_limits
            && self
                .mac(&receipt.source, &receipt.run, receipt.started, journal)
                .is_ok_and(|mac| mac.verify_slice(&receipt.proof).is_ok())
    }

    pub(crate) fn root_matches(&self, root: &Path) -> bool {
        self.grant
            .profile
            .binding()
            .is_some_and(|binding| binding.root == root)
    }

    fn mac(
        &self,
        source: &AutomationSource,
        run: &str,
        started: jiff::Timestamp,
        journal: &str,
    ) -> io::Result<Hmac<Sha256>> {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC accepts a 32-byte key");
        let scope = (
            source,
            run,
            started,
            journal,
            &self.grant.profile,
            self.full_output,
        );
        if let Some(limits) = self.grant.evidence_limits {
            mac.update(b"kortty automation journal source approval v2\0");
            mac.update(&serde_json::to_vec(&(scope, limits))?);
        } else {
            mac.update(b"kortty automation journal source approval v1\0");
            mac.update(&serde_json::to_vec(&scope)?);
        }
        Ok(mac)
    }

    fn valid(&self) -> bool {
        self.grant.profile.binding().is_some() && self.grant.profile.valid()
    }
}

/// A receipt carries the journal writer's proof, never the private signing key.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceReceipt {
    pub(crate) source: AutomationSource,
    pub(crate) run: String,
    pub(crate) started: jiff::Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) evidence_limits: Option<EvidenceLimits>,
    pub(crate) proof: [u8; 32],
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Preferences {
    pub(crate) enabled: bool,
    pub(crate) interval_minutes: u16,
    pub(crate) automatic_title: bool,
    pub(crate) full_output: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) evidence_limits: Option<EvidenceLimits>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) https: Option<AutomaticApproval>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cli: Option<AutomaticApproval>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) recording_profile: Option<ProfileChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) automation_profile: Option<AutomationProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) automation_inherited_since: Option<jiff::Timestamp>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) source_approvals: Vec<(AutomationSource, SourceApproval)>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_minutes: DEFAULT_INTERVAL,
            automatic_title: false,
            full_output: false,
            evidence_limits: None,
            https: None,
            cli: None,
            recording_profile: None,
            automation_profile: None,
            automation_inherited_since: None,
            source_approvals: Vec::new(),
        }
    }
}

impl Preferences {
    pub(crate) fn source_approval(&self, source: &AutomationSource) -> Option<&SourceApproval> {
        self.source_approvals
            .iter()
            .find_map(|(key, grant)| (key == source).then_some(grant))
    }

    pub(crate) fn source_grant_at(
        &self,
        source: &AutomationSource,
        started: jiff::Timestamp,
    ) -> Option<&SourceApproval> {
        self.source_approval(source)
            .filter(|approval| started >= approval.grant.since)
    }

    pub(crate) fn set_source_approval(
        &mut self,
        source: AutomationSource,
        approval: Option<(AutomaticApproval, bool)>,
    ) -> io::Result<()> {
        let approval = match approval {
            Some((mut grant, full_output)) if grant.evidence_limits == self.evidence_limits => {
                Some(match self.source_approval(&source) {
                    Some(current)
                        if current.full_output == full_output
                            && current.grant.same_summary_evidence(&grant)
                            && current.grant.title_since.is_some()
                                != grant.title_since.is_some() =>
                    {
                        grant.since = current.grant.since;
                        grant.title_since = grant.title_since.map(|title| title.max(grant.since));
                        SourceApproval {
                            grant,
                            ..current.clone()
                        }
                    }
                    _ => SourceApproval::new(grant, full_output)?,
                })
            }
            Some(_) => return Err(invalid("The reviewed journal evidence limits changed")),
            None => None,
        };
        self.source_approvals.retain(|(key, _)| *key != source);
        self.source_approvals
            .extend(approval.map(|approval| (source, approval)));
        Ok(())
    }

    pub(crate) fn reviewed_grant(&self) -> Option<&AutomaticApproval> {
        self.https.as_ref().or(self.cli.as_ref())
    }

    pub(crate) fn automation_grant(&self) -> Option<&AutomaticApproval> {
        match &self.automation_profile {
            Some(AutomationProfile::Reviewed(grant)) => Some(grant),
            Some(AutomationProfile::Local | AutomationProfile::Selected(_)) => None,
            None => self.reviewed_grant(),
        }
    }

    pub(crate) fn automation_local_profile(&self) -> Option<&ProfileChoice> {
        match &self.automation_profile {
            Some(AutomationProfile::Selected(profile)) => Some(profile),
            None => self.recording_profile.as_ref(),
            _ => None,
        }
    }

    pub(crate) fn automation_grant_at(
        &self,
        started: jiff::Timestamp,
    ) -> Option<&AutomaticApproval> {
        self.automation_grant()
            .filter(|grant| started >= grant.since)
            .filter(|_| {
                self.automation_inherited_since
                    .is_none_or(|since| started >= since)
            })
    }

    fn grant_for(&self, mode: Mode) -> Option<&AutomaticApproval> {
        match mode {
            Mode::Automation => self.automation_grant(),
            Mode::Closing => self.reviewed_grant(),
            Mode::Reevaluate | Mode::CatchUp => None,
        }
    }
}

/// A closing-summary route selected independently of the terminal recording route.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum AutomationProfile {
    Local,
    Selected(ProfileChoice),
    Reviewed(AutomaticApproval),
}

impl AutomationProfile {
    fn valid(&self) -> bool {
        match self {
            Self::Local => true,
            Self::Selected(profile) => profile.binding().is_none() && profile.valid(),
            Self::Reviewed(grant) => grant.profile.binding().is_some() && grant.profile.valid(),
        }
    }

    fn same_summary_scope(current: Option<&Self>, next: Option<&Self>) -> bool {
        match (current, next) {
            (Some(Self::Reviewed(current)), Some(Self::Reviewed(next))) => {
                current.same_summary_scope(next)
            }
            _ => current == next,
        }
    }
}

/// Saved manager profile identity. Endpoint and model are display-safe, credential-free values.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProfileChoice {
    pub(crate) id: ProfileRef,
    pub(crate) model: String,
    pub(crate) destination: String,
    // Keep the original receipt key for policy and automatically resolved model authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) policy_model_fingerprint: Option<[u8; 32]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) https: Option<ProfileBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cli: Option<ProfileBinding>,
}

impl ProfileChoice {
    pub(crate) fn binding(&self) -> Option<&ProfileBinding> {
        self.https.as_ref().or(self.cli.as_ref())
    }

    pub(crate) fn policy_model_matches(&self, current: Option<[u8; 32]>) -> bool {
        self.policy_model_fingerprint == current
    }

    pub(crate) fn pins_lm_studio_auto(&self) -> bool {
        matches!(self.id, ProfileRef::Numeric(id) if id != 0)
            && self.policy_model_fingerprint.is_some()
            && self.binding().is_none()
            && crate::local_ai::LmStudioEndpoint::from_destination(&self.destination)
                .is_some_and(|endpoint| !endpoint.is_https())
    }

    pub(crate) fn valid(&self) -> bool {
        !self.model.trim().is_empty()
            && self.model.len() <= 256
            && !self.model.chars().any(char::is_control)
            && !self.destination.trim().is_empty()
            && self.destination.len() <= if self.https.is_some() { 2_080 } else { 512 }
            && !self.destination.chars().any(char::is_control)
            && !(self.https.is_some() && self.cli.is_some())
            && (self.cli.is_none() || matches!(self.id, ProfileRef::Numeric(id) if id != 0))
            && match (&self.id, self.binding()) {
                (ProfileRef::Numeric(0), None) => {
                    self.policy_model_fingerprint.is_none()
                        && crate::local_ai::Profile::new(&self.destination, &self.model).is_ok()
                }
                (ProfileRef::Numeric(0), Some(_)) => false,
                (ProfileRef::Policy(_), None) => self.policy_model_fingerprint.is_some(),
                (_, Some(binding)) => {
                    self.policy_model_fingerprint.is_none()
                        && binding.root.is_absolute()
                        && check_path(&binding.root).is_ok()
                }
                (ProfileRef::Numeric(_), None) => true,
            }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProfileBinding {
    pub(crate) root: PathBuf,
    pub(crate) profile_revision: [u8; 32],
    pub(crate) redaction_revision: [u8; 32],
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AutomaticApproval {
    pub(crate) profile: ProfileChoice,
    pub(crate) since: jiff::Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) evidence_limits: Option<EvidenceLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title_since: Option<jiff::Timestamp>,
}

impl AutomaticApproval {
    fn same_summary_evidence(&self, other: &Self) -> bool {
        self.profile == other.profile && self.evidence_limits == other.evidence_limits
    }

    fn same_summary_scope(&self, other: &Self) -> bool {
        self.since == other.since && self.same_summary_evidence(other)
    }

    fn same_grant(current: Option<&Self>, next: Option<&Self>) -> bool {
        match (current, next) {
            (Some(current), Some(next)) => current.same_summary_scope(next),
            _ => current == next,
        }
    }

    pub(crate) fn title_approval_at(&self, started: jiff::Timestamp) -> Option<jiff::Timestamp> {
        self.title_since.filter(|approved| started >= *approved)
    }

    pub(crate) fn with_title_scope(mut self, enabled: bool, now: jiff::Timestamp) -> Self {
        self.title_since = enabled.then(|| self.title_since.unwrap_or_else(|| now.max(self.since)));
        self
    }

    fn valid_title_scope(&self) -> bool {
        self.title_since.is_none_or(|since| since >= self.since)
    }
}

/// Local vision authority fixed when a scheduled virtual terminal starts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AutomationVision {
    /// `None` denotes the active Journal role, including its legacy loopback form.
    pub(crate) job_profile: Option<ProfileRef>,
    pub(crate) revision: [u8; 32],
    pub(crate) redaction_revision: [u8; 32],
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    pub(crate) generation: String,
    pub(crate) batch: String,
    pub(crate) source: SummarySource,
    pub(crate) title: String,
    pub(crate) mode: Mode,
    pub(crate) profile: Option<ProfileChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_approval: Option<SourceReceipt>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) raw_only: bool,
    pub(crate) automatic_title: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title_approval: Option<jiff::Timestamp>,
    #[serde(default)]
    pub(crate) full_output: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) evidence_limits: Option<EvidenceLimits>,
    pub(crate) screenshots: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) automation_vision: Option<AutomationVision>,
}

impl Receipt {
    fn matches_source_approval(&self, preferences: &Preferences) -> bool {
        let Some(receipt) = &self.source_approval else {
            return false;
        };
        preferences
            .source_approval(&receipt.source)
            .is_some_and(|approval| {
                Some(&approval.grant.profile) == self.profile.as_ref()
                    && self.full_output == approval.full_output
                    && self.evidence_limits == approval.grant.evidence_limits
                    && approval.verifies_receipt(receipt, self.source.id())
            })
    }

    pub(crate) fn automatic(&self) -> bool {
        matches!(self.mode, Mode::Closing | Mode::Automation)
    }

    pub(crate) fn reviewed(&self) -> bool {
        self.profile
            .as_ref()
            .is_some_and(|profile| profile.binding().is_some())
    }

    pub(crate) fn cli(&self) -> bool {
        self.profile
            .as_ref()
            .is_some_and(|profile| profile.cli.is_some())
    }

    fn validate(&self) -> io::Result<()> {
        check_path(self.source.directory())?;
        if self.generation.len() != 32
            || !self.generation.bytes().all(|byte| byte.is_ascii_hexdigit())
            || self.batch.len() != 32
            || !self.batch.bytes().all(|byte| byte.is_ascii_hexdigit())
            || self.title.len() > 1_024
            || self.title.chars().any(char::is_control)
            || self.source.directory().file_name().is_none()
            || !valid_evidence_limits(self.evidence_limits)
            || self.title_approval.is_some() && (!self.automatic_title || !self.reviewed())
            || self.raw_only && (self.mode != Mode::Automation || self.profile.is_some())
            || self.source_approval.as_ref().is_some_and(|approval| {
                self.mode != Mode::Automation
                    || !self.reviewed()
                    || !kortty_jobs::valid_session_journal_id(&approval.run)
                    || approval.evidence_limits != self.evidence_limits
            })
            || (self.automatic() || self.reviewed()) && self.screenshots
            || self.automation_vision.is_some()
                && (self.mode != Mode::Automation || self.raw_only || self.screenshots)
            || matches!(self.mode, Mode::Reevaluate | Mode::CatchUp) && self.profile.is_none()
            || self
                .profile
                .as_ref()
                .is_some_and(|profile| !profile.valid())
            || self
                .profile
                .as_ref()
                .and_then(ProfileChoice::binding)
                .is_some_and(|binding| {
                    self.source.directory().parent() != Some(binding.root.as_path())
                })
        {
            return Err(invalid("Invalid saved journal summary job"));
        }
        Ok(())
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Disk {
    version: u8,
    preferences: Preferences,
    pending: VecDeque<Receipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cancelled_automation: Vec<String>,
}

impl Default for Disk {
    fn default() -> Self {
        Self {
            version: VERSION,
            preferences: Preferences::default(),
            pending: VecDeque::new(),
            cancelled_automation: Vec::new(),
        }
    }
}

impl Disk {
    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION
            || self.pending.len() > MAX_PENDING
            || self.cancelled_automation.len() > MAX_CANCELLED_AUTOMATION
            || self
                .cancelled_automation
                .iter()
                .any(|id| id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()))
            || !(1..=super::MAX_SUMMARY_MINUTES).contains(&self.preferences.interval_minutes)
            || !valid_evidence_limits(self.preferences.evidence_limits)
            || self.preferences.https.as_ref().is_some_and(|grant| {
                !self.preferences.enabled
                    || grant.profile.https.is_none()
                    || !grant.profile.valid()
                    || !grant.valid_title_scope()
                    || grant.evidence_limits != self.preferences.evidence_limits
            })
            || self.preferences.cli.as_ref().is_some_and(|grant| {
                !self.preferences.enabled
                    || self.preferences.https.is_some()
                    || grant.profile.cli.is_none()
                    || !grant.profile.valid()
                    || !grant.valid_title_scope()
                    || grant.evidence_limits != self.preferences.evidence_limits
            })
            || self
                .preferences
                .recording_profile
                .as_ref()
                .is_some_and(|profile| {
                    !profile.valid()
                        || profile.binding().is_some()
                        || self.preferences.reviewed_grant().is_some()
                })
            || self
                .preferences
                .automation_profile
                .as_ref()
                .is_some_and(|profile| {
                    !profile.valid()
                        || matches!(profile, AutomationProfile::Reviewed(grant)
                            if grant.evidence_limits != self.preferences.evidence_limits
                                || !grant.valid_title_scope())
                })
            || self.preferences.automation_profile.is_some()
                && self.preferences.automation_inherited_since.is_some()
            || self.preferences.source_approvals.len() > MAX_SOURCE_APPROVALS
            || self.preferences.source_approvals.iter().enumerate().any(
                |(index, (source, approval))| {
                    !approval.valid()
                        || approval.grant.evidence_limits != self.preferences.evidence_limits
                        || !approval.grant.valid_title_scope()
                        || self.preferences.source_approvals[..index]
                            .iter()
                            .any(|(key, _)| key == source)
                },
            )
        {
            return Err(invalid(
                "Journal summary settings exceed their saved limits",
            ));
        }
        if serde_json::to_vec(&self.pending)?.len() as u64
            + serde_json::to_vec(&self.cancelled_automation)?.len() as u64
            > MAX_QUEUE_BYTES
        {
            return Err(invalid("Journal summary queue exceeds its saved limit"));
        }
        for (index, receipt) in self.pending.iter().enumerate() {
            receipt.validate()?;
            if self.pending.iter().take(index).any(|other| {
                other.generation == receipt.generation || other.source == receipt.source
            }) {
                return Err(invalid("Duplicate saved journal summary job"));
            }
        }
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct Queue {
    path: Option<PathBuf>,
    disk: Disk,
}

impl Queue {
    pub(crate) fn load(root: &Path) -> io::Result<Self> {
        check_path(root)?;
        let path = root.join(FILE);
        let disk = match crate::profile::open_preference(&path) {
            Ok(file) => {
                if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_FILE_BYTES {
                    return Err(invalid("Invalid journal summary preferences file"));
                }
                let mut bytes = Vec::new();
                file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_FILE_BYTES {
                    return Err(invalid(
                        "Journal summary preferences exceed their size limit",
                    ));
                }
                serde_json::from_slice::<Disk>(&bytes)?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Disk::default(),
            Err(error) => return Err(error),
        };
        disk.validate()?;
        Ok(Self {
            path: Some(path),
            disk,
        })
    }

    pub(crate) fn preferences(&self) -> &Preferences {
        &self.disk.preferences
    }

    pub(crate) fn profile_root(&self) -> Option<&Path> {
        self.path.as_deref().and_then(Path::parent)
    }

    pub(crate) fn pending(&self) -> &VecDeque<Receipt> {
        &self.disk.pending
    }

    pub(crate) fn cancelled_automation(&self) -> &[String] {
        &self.disk.cancelled_automation
    }

    pub(crate) fn set_preferences(&mut self, mut preferences: Preferences) -> io::Result<()> {
        let evidence_limits = preferences.evidence_limits;
        preferences
            .source_approvals
            .retain(|(_, approval)| approval.grant.evidence_limits == evidence_limits);
        let mut next = self.disk.clone();
        next.pending.retain(|receipt| match receipt.mode {
            Mode::Closing | Mode::Automation if receipt.evidence_limits != evidence_limits => false,
            Mode::Automation if receipt.source_approval.is_some() => {
                receipt.matches_source_approval(&preferences)
            }
            Mode::Automation if receipt.reviewed() => {
                AutomationProfile::same_summary_scope(
                    next.preferences.automation_profile.as_ref(),
                    preferences.automation_profile.as_ref(),
                ) && next.preferences.automation_inherited_since
                    == preferences.automation_inherited_since
                    && AutomaticApproval::same_grant(
                        next.preferences.automation_grant(),
                        preferences.automation_grant(),
                    )
                    && next.preferences.full_output == preferences.full_output
            }
            Mode::Closing if receipt.reviewed() => {
                AutomaticApproval::same_grant(
                    next.preferences.reviewed_grant(),
                    preferences.reviewed_grant(),
                ) && next.preferences.full_output == preferences.full_output
            }
            _ => true,
        });
        next.preferences = preferences;
        if !next.preferences.enabled {
            next.pending.retain(|receipt| receipt.mode != Mode::Closing);
        }
        self.commit(next)
    }

    pub(crate) fn revoke_source(&mut self, source: &AutomationSource) -> io::Result<()> {
        let Some(root) = self.path.as_deref().and_then(Path::parent) else {
            return Err(invalid("Journal summary preferences are unavailable"));
        };
        let mut current = Self::load(root)?;
        if current.preferences().source_approval(source).is_some() {
            let mut preferences = current.preferences().clone();
            preferences.set_source_approval(source.clone(), None)?;
            current.set_preferences(preferences)?;
        }
        *self = current;
        Ok(())
    }

    pub(crate) fn authorizes_reviewed(&self, receipt: &Receipt) -> bool {
        receipt.reviewed()
            && self.disk.pending.contains(receipt)
            && (if receipt.source_approval.is_some() {
                receipt.matches_source_approval(&self.disk.preferences)
            } else {
                !receipt.automatic()
                    || self.disk.preferences.full_output == receipt.full_output
                        && self.disk.preferences.evidence_limits == receipt.evidence_limits
                        && self
                            .disk
                            .preferences
                            .grant_for(receipt.mode)
                            .is_some_and(|grant| {
                                grant.evidence_limits == receipt.evidence_limits
                                    && Some(&grant.profile) == receipt.profile.as_ref()
                            })
            })
    }

    pub(crate) fn authorizes_title(&self, receipt: &Receipt) -> bool {
        let preferences = &self.disk.preferences;
        if !receipt.automatic_title
            || !self.disk.pending.contains(receipt)
            || receipt.automatic() && !preferences.automatic_title
        {
            return false;
        }
        if !receipt.reviewed() {
            return true;
        }
        let Some(title_approval) = receipt.title_approval else {
            return false;
        };
        if !self.authorizes_reviewed(receipt) {
            return false;
        }
        if !receipt.automatic() {
            return true;
        }
        let grant = match &receipt.source_approval {
            Some(source) => preferences
                .source_approval(&source.source)
                .map(|approval| &approval.grant),
            None => preferences.grant_for(receipt.mode),
        };
        grant.and_then(|grant| grant.title_since) == Some(title_approval)
            && receipt
                .source_approval
                .as_ref()
                .is_none_or(|source| source.started >= title_approval)
    }

    pub(crate) fn enqueue(
        &mut self,
        receipts: impl IntoIterator<Item = Receipt>,
    ) -> io::Result<()> {
        let mut next = self.disk.clone();
        next.pending.extend(receipts);
        self.commit(next)
    }

    pub(crate) fn bind_inherited_profile(
        &mut self,
        unbound: &Receipt,
        profile: ProfileChoice,
    ) -> io::Result<Receipt> {
        if !unbound.automatic()
            || unbound.raw_only
            || unbound.profile.is_some()
            || unbound.source_approval.is_some()
            || !profile.pins_lm_studio_auto()
        {
            return Err(invalid("Invalid inherited journal model binding"));
        }
        let mut next = self.disk.clone();
        let receipt = next
            .pending
            .iter_mut()
            .find(|receipt| *receipt == unbound)
            .ok_or_else(|| invalid("The inherited journal summary changed"))?;
        receipt.profile = Some(profile);
        let bound = receipt.clone();
        self.commit(next)?;
        Ok(bound)
    }

    pub(crate) fn finish(&mut self, generation: &str) -> io::Result<()> {
        let mut next = self.disk.clone();
        next.pending
            .retain(|receipt| receipt.generation != generation);
        self.commit(next)
    }

    pub(crate) fn cancel_batch(&mut self, batch: &str) -> io::Result<()> {
        let mut next = self.disk.clone();
        for receipt in next
            .pending
            .iter()
            .filter(|receipt| receipt.batch == batch && receipt.mode == Mode::Automation)
        {
            if !next.cancelled_automation.contains(&receipt.generation) {
                next.cancelled_automation.push(receipt.generation.clone());
            }
        }
        next.pending.retain(|receipt| receipt.batch != batch);
        self.commit(next)
    }

    fn commit(&mut self, next: Disk) -> io::Result<()> {
        next.validate()?;
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| invalid("Journal summary preferences are unavailable"))?;
        let bytes = serde_json::to_vec(&next)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(invalid(
                "Journal summary preferences exceed their size limit",
            ));
        }
        let root = path
            .parent()
            .ok_or_else(|| invalid("Missing journal summary profile directory"))?;
        let directory = crate::private_files::Directory::open(root, true)?;
        let lock = directory.open_lock(LOCK)?;
        crate::private_files::require_single_link(&lock)?;
        lock.try_lock().map_err(io::Error::other)?;
        let _lock = crate::private_files::FileLock::new(lock);
        let current = Self::load(root)?;
        if current.disk != self.disk {
            self.disk = current.disk;
            return Err(io::Error::other(
                "Journal summary settings changed in another instance. Review them again.",
            ));
        }
        crate::profile::save_atomic(path, &bytes)?;
        self.disk = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{AutomaticApproval, Preferences, ProfileChoice};
    use crate::ai_manager::ProfileRef;

    #[test]
    fn journal_summary_policy_model_receipt_requires_the_reviewed_source() {
        let legacy = serde_json::json!({
            "id": 1,
            "model": "policy-team-model",
            "destination": "Managed llama.cpp on this computer"
        });
        let mut choice: ProfileChoice = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(&choice).unwrap(), legacy);
        assert!(choice.policy_model_matches(None));
        assert!(!choice.policy_model_matches(Some([1; 32])));
        choice.policy_model_fingerprint = Some([1; 32]);
        let reopened: ProfileChoice =
            serde_json::from_slice(&serde_json::to_vec(&choice).unwrap()).unwrap();
        assert!(reopened.policy_model_matches(Some([1; 32])));
        assert!(!reopened.policy_model_matches(Some([2; 32])));
        assert!(!reopened.policy_model_matches(None));
    }

    #[test]
    fn journal_summary_legacy_endpoint_sentinel_round_trips() {
        let endpoint = serde_json::json!({
            "id": 0,
            "model": "fixture-model",
            "destination": "127.0.0.1:1234"
        });
        let choice: ProfileChoice = serde_json::from_value(endpoint.clone()).unwrap();
        assert_eq!(choice.id, ProfileRef::Numeric(0));
        assert_eq!(serde_json::to_value(&choice).unwrap(), endpoint);
    }

    #[test]
    fn newly_inherited_automation_grant_excludes_runs_from_the_previous_scope() {
        let terminal: AutomaticApproval = serde_json::from_value(serde_json::json!({
            "profile": {"id": 1, "model": "fixture", "destination": "https://example.invalid/v1"},
            "since": "2026-01-01T00:00:00Z"
        }))
        .unwrap();
        let before = "2026-01-02T00:00:00Z".parse().unwrap();
        let after = "2026-01-03T00:00:00Z".parse().unwrap();
        let mut preferences = Preferences {
            enabled: true,
            https: Some(terminal.clone()),
            ..Preferences::default()
        };
        assert_eq!(preferences.automation_grant_at(before), Some(&terminal));
        preferences.automation_inherited_since = Some(after);
        assert!(preferences.automation_grant_at(before).is_none());
        assert_eq!(preferences.automation_grant_at(after), Some(&terminal));
    }
}
