//! Private profile receipts for automatic finishing and user-reviewed journal jobs.

use std::collections::VecDeque;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ai_manager::ProfileRef;

use super::summary_job::Mode;
use super::viewer::SummarySource;
use super::{check_path, invalid};

const FILE: &str = "journal-summary-jobs.json";
const MAX_BYTES: u64 = 128 * 1024;
pub(crate) const MAX_PENDING: usize = 64;
pub(crate) const DEFAULT_INTERVAL: u16 = 5;
const VERSION: u8 = 1;
const MAX_CANCELLED_AUTOMATION: usize = 2_048;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Preferences {
    pub(crate) enabled: bool,
    pub(crate) interval_minutes: u16,
    pub(crate) automatic_title: bool,
    pub(crate) full_output: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) https: Option<AutomaticApproval>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cli: Option<AutomaticApproval>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_minutes: DEFAULT_INTERVAL,
            automatic_title: false,
            full_output: false,
            https: None,
            cli: None,
        }
    }
}

impl Preferences {
    pub(crate) fn reviewed_grant(&self) -> Option<&AutomaticApproval> {
        self.https.as_ref().or(self.cli.as_ref())
    }
}

/// Saved manager profile identity. Endpoint and model are display-safe, credential-free values.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProfileChoice {
    pub(crate) id: ProfileRef,
    pub(crate) model: String,
    pub(crate) destination: String,
    // Keep the receipt key used by approvals bound only to a policy model.
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

    fn valid(&self) -> bool {
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
    pub(crate) automatic_title: bool,
    #[serde(default)]
    pub(crate) full_output: bool,
    pub(crate) screenshots: bool,
}

impl Receipt {
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
            || (self.automatic() || self.reviewed()) && self.screenshots
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

#[derive(Clone, Deserialize, Serialize)]
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
            || self.preferences.https.as_ref().is_some_and(|grant| {
                !self.preferences.enabled || grant.profile.https.is_none() || !grant.profile.valid()
            })
            || self.preferences.cli.as_ref().is_some_and(|grant| {
                !self.preferences.enabled
                    || self.preferences.https.is_some()
                    || grant.profile.cli.is_none()
                    || !grant.profile.valid()
            })
        {
            return Err(invalid(
                "Journal summary settings exceed their saved limits",
            ));
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
                if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_BYTES {
                    return Err(invalid("Invalid journal summary preferences file"));
                }
                let mut bytes = Vec::new();
                file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_BYTES {
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

    pub(crate) fn pending(&self) -> &VecDeque<Receipt> {
        &self.disk.pending
    }

    pub(crate) fn cancelled_automation(&self) -> &[String] {
        &self.disk.cancelled_automation
    }

    pub(crate) fn set_preferences(&mut self, preferences: Preferences) -> io::Result<()> {
        let mut next = self.disk.clone();
        if next.preferences.reviewed_grant() != preferences.reviewed_grant()
            || next.preferences.full_output != preferences.full_output
        {
            next.pending
                .retain(|receipt| !(receipt.automatic() && receipt.reviewed()));
        }
        next.preferences = preferences;
        if !next.preferences.enabled {
            next.pending.retain(|receipt| receipt.mode != Mode::Closing);
        }
        self.commit(next)
    }

    pub(crate) fn authorizes_reviewed(&self, receipt: &Receipt) -> bool {
        receipt.reviewed()
            && self.disk.pending.contains(receipt)
            && (!receipt.automatic()
                || self.disk.preferences.enabled
                    && self.disk.preferences.full_output == receipt.full_output
                    && self
                        .disk
                        .preferences
                        .reviewed_grant()
                        .is_some_and(|grant| Some(&grant.profile) == receipt.profile.as_ref()))
    }

    pub(crate) fn enqueue(
        &mut self,
        receipts: impl IntoIterator<Item = Receipt>,
    ) -> io::Result<()> {
        let mut next = self.disk.clone();
        next.pending.extend(receipts);
        self.commit(next)
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
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid(
                "Journal summary preferences exceed their size limit",
            ));
        }
        crate::profile::save_atomic(path, &bytes)?;
        self.disk = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::ProfileChoice;
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
}
