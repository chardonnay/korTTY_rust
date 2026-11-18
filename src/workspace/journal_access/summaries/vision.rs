//! Local image authority for a native Scheduler journal's closing pass.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::ai_manager::{Configuration, ProfileRef};
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::Profile;
use crate::session_journal::summary_queue::{AutomationVision, ProfileChoice, Queue, Receipt};
use crate::session_journal::topic_ai_settings::Preferences;
use crate::workspace::tab_titles::WindowSettings;

use super::Workspace;

const CHANGED: &str = "The scheduled screenshot analysis approval is unavailable or changed.";

impl Workspace {
    pub(in crate::workspace) fn automation_screenshot_vision(
        &self,
        job_profile: Option<&ProfileChoice>,
    ) -> Option<AutomationVision> {
        if self.exiting || self.scheduler_draining || !self.automation_journal_capture_allowed() {
            return None;
        }
        let configuration = self.ai_configuration()?;
        configuration.saved_root()?;
        let settings = &self.window_settings.journal_ai;
        let policy = &self.clipboard.policy;
        let job_profile = job_profile
            .filter(|choice| choice.binding().is_none())
            .filter(|choice| self.journal_summary_profile(Some(choice), true).is_some())
            .map(|choice| &choice.id);
        let (authority, _) = resolve(&configuration, settings, policy, job_profile)
            .or_else(|| resolve(&configuration, settings, policy, None))?;
        Some(authority)
    }

    pub(super) fn automation_screenshot_profile(
        &self,
        approved: Option<&AutomationVision>,
    ) -> Option<Profile> {
        if self.exiting || self.scheduler_draining || !self.automation_journal_capture_allowed() {
            return None;
        }
        let approved = approved?;
        let configuration = self.ai_configuration()?;
        let (authority, profile) = resolve(
            &configuration,
            &self.window_settings.journal_ai,
            &self.clipboard.policy,
            approved.job_profile.as_ref(),
        )?;
        (authority == *approved).then_some(profile)
    }
}

pub(super) struct Authority {
    root: PathBuf,
    receipt: Receipt,
    profile: Profile,
}

pub(super) enum Client {
    Serial(crate::local_ai::Client),
    OwnSession(crate::local_ai::Session),
}

impl Client {
    pub(super) fn get(&self) -> &crate::local_ai::Client {
        match self {
            Self::Serial(client) => client,
            Self::OwnSession(session) => session.client(),
        }
    }
}

impl Authority {
    pub(super) fn new(
        configuration: &Configuration,
        receipt: &Receipt,
        profile: Profile,
    ) -> Option<Self> {
        Some(Self {
            root: configuration.saved_root()?.to_owned(),
            receipt: receipt.clone(),
            profile,
        })
    }

    pub(super) fn authorize(&self) -> Result<(), String> {
        let queue = Queue::load(&self.root).map_err(|_| CHANGED)?;
        if !queue.pending().contains(&self.receipt) {
            return Err(CHANGED.into());
        }
        let approved = self.receipt.automation_vision.as_ref().ok_or(CHANGED)?;
        let settings =
            WindowSettings::read(&self.root.join("window-settings.json")).map_err(|_| CHANGED)?;
        let policy = ClipboardPolicy::load();
        let directory = policy
            .terminal_logging
            .journal_directory()
            .map(Path::to_owned)
            .map_or_else(|| settings.session_journal.directory(&self.root), Ok)
            .map_err(|_| CHANGED)?;
        if self.receipt.source.directory().parent() != Some(directory.as_path()) {
            return Err(CHANGED.into());
        }
        let configuration = Configuration::load_from(&self.root).map_err(|_| CHANGED)?;
        let (authority, profile) = resolve(
            &configuration,
            &settings.journal_ai,
            &policy,
            approved.job_profile.as_ref(),
        )
        .ok_or(CHANGED)?;
        if authority != *approved || profile != self.profile {
            return Err(CHANGED.into());
        }
        self.receipt
            .source
            .source()
            .require_automation_vision(approved)
            .map_err(|_| CHANGED.into())
    }
}

fn resolve(
    configuration: &Configuration,
    settings: &Preferences,
    policy: &ClipboardPolicy,
    job_profile: Option<&ProfileRef>,
) -> Option<(AutomationVision, Profile)> {
    if !configuration.enabled()
        || !settings.enabled
        || !settings.vision_enabled
        || !settings.allows_screenshot_analysis(policy)
        || !policy.automation_journal_allowed()
        || !policy.recording_allowed
        || !policy
            .terminal_logging
            .journal_ai_screenshot_analysis()
            .unwrap_or(settings.auto_screenshot_analysis)
    {
        return None;
    }
    let use_legacy_role = (!settings.endpoint.is_empty() || !settings.model.is_empty())
        && (!configuration.journal_profile_is_explicit()
            || configuration.journal_profile(false).is_none());
    let (revision, profile) = match job_profile {
        Some(ProfileRef::Numeric(0)) => legacy(settings)?,
        Some(reference) => {
            let id = configuration.resolve_profile_reference(reference)?;
            (
                configuration.agent_profile_reference_revision(reference)?,
                configuration.journal_summary_transport(id, true).ok()?,
            )
        }
        None if use_legacy_role => legacy(settings)?,
        None => {
            let id = configuration.journal_summary_default_id()?;
            let reference = configuration.profile_reference(id)?;
            (
                configuration.agent_profile_reference_revision(&reference)?,
                configuration.journal_profile(true)?.ok()?,
            )
        }
    };
    if profile.is_remote() || profile.is_cli() {
        return None;
    }
    profile.require_current_policy_authority().ok()?;
    Some((
        AutomationVision {
            job_profile: job_profile.cloned(),
            revision,
            redaction_revision: policy.terminal_logging.redaction_revision()?,
        },
        profile,
    ))
}

fn legacy(settings: &Preferences) -> Option<([u8; 32], Profile)> {
    let profile = settings.vision_profile().ok().flatten()?;
    let bytes = serde_json::to_vec(&(
        "scheduled journal legacy vision",
        &settings.endpoint,
        &settings.model,
    ))
    .ok()?;
    Some((Sha256::digest(bytes).into(), profile))
}
