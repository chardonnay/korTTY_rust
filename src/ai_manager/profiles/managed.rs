//! Installed profile projection and stable references for native consumers.

use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use std::sync::Arc;

use pgp::crypto::hash::HashAlgorithm;

use crate::clipboard_policy::{ClipboardPolicy, PolicyAiProfile, PolicyAiProvider};
use crate::local_ai::{UsageAccount, UsageBudget};

use super::{Catalog, ConnectionMode, Profile, ProfileId, ProfileRef, Vision};

const POLICY_REFERENCE_START: u64 = 1 << 63;

fn profile_hash(id: &str, domain: &[u8]) -> [u8; 32] {
    let mut hash = HashAlgorithm::Sha256
        .new_hasher()
        .expect("SHA-256 is supported");
    hash.update(domain);
    hash.update(id.as_bytes());
    hash.finalize()
        .as_ref()
        .try_into()
        .expect("SHA-256 has thirty-two bytes")
}

fn historical_alias(id: &str) -> u64 {
    let bytes = profile_hash(id, b"kortty native policy profile reference v1\0");
    let prefix = bytes[..8]
        .try_into()
        .expect("SHA-256 has at least eight bytes");
    u64::from_le_bytes(prefix) | POLICY_REFERENCE_START
}

fn runtime_id(id: &str) -> ProfileId {
    let bytes = profile_hash(id, b"kortty native policy runtime profile v1\0");
    let prefix = bytes[..16]
        .try_into()
        .expect("SHA-256 has at least sixteen bytes");
    ProfileId::from_le_bytes(prefix) | (1 << 127)
}

impl Catalog {
    pub(in crate::ai_manager) fn legacy_policy_alias(&self, id: ProfileId) -> Option<u64> {
        self.legacy_alias_for_profile(self.profile(id)?)
    }

    pub(super) fn legacy_alias_for_profile(&self, profile: &Profile) -> Option<u64> {
        if !self.has_legacy_policy_aliases() {
            return None;
        }
        profile
            .policy_definition()
            .map(|definition| historical_alias(&definition.id))
    }

    pub(in crate::ai_manager) fn usage_account(
        &self,
        profile: &Profile,
        root: &Path,
        budget: UsageBudget,
    ) -> Result<UsageAccount, String> {
        if let Some(definition) = profile.policy_definition() {
            let legacy = self.legacy_alias_for_profile(profile);
            UsageAccount::policy(root.to_path_buf(), &definition.id, legacy, budget)
        } else {
            let id = u64::try_from(profile.id)
                .map_err(|_| "The saved AI profile identity is unavailable.")?;
            UsageAccount::new(root.to_path_buf(), id, budget)
        }
    }

    pub(in crate::ai_manager) fn copy_policy_profiles(&mut self, source: &Self) {
        self.policy_profiles.clone_from(&source.policy_profiles);
    }

    pub(in crate::ai_manager) fn policy_profiles_changed(&self, other: &Self) -> bool {
        self.policy_profiles != other.policy_profiles
    }

    pub(in crate::ai_manager) fn require_current_profile_reference(
        &self,
        reference: &ProfileRef,
        policy: &ClipboardPolicy,
    ) -> io::Result<()> {
        if !self.is_policy_profile_reference(reference) {
            return Ok(());
        }
        let definition = self
            .profile_for_reference(reference)
            .and_then(Profile::policy_definition);
        self.require_current_definition(definition, policy)
    }

    pub(super) fn require_current_definition(
        &self,
        definition: Option<&PolicyAiProfile>,
        policy: &ClipboardPolicy,
    ) -> io::Result<()> {
        if policy.problem.is_none()
            && policy.ai_allowed
            && definition.is_some_and(|definition| policy.ai_profiles().contains(definition))
        {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The provisioned AI profile changed. Refresh the profile list and review the selection again.",
            ))
        }
    }

    pub(in crate::ai_manager) fn project_policy_profiles(
        &mut self,
        policy: &ClipboardPolicy,
    ) -> io::Result<()> {
        self.policy_profiles.clear();
        let mut ids = BTreeSet::new();
        let mut aliases = BTreeSet::new();
        let mut projected = Vec::new();
        for definition in policy.ai_profiles() {
            let id = runtime_id(&definition.id);
            if !ids.insert(id)
                || (self.has_legacy_policy_aliases()
                    && !aliases.insert(historical_alias(&definition.id)))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Provisioned AI profile references conflict.",
                ));
            }
            projected.push(Profile::from_policy(id, definition));
        }
        self.policy_profiles = projected;
        Ok(())
    }

    pub(in crate::ai_manager) fn is_policy_reference(&self, id: u64) -> bool {
        self.has_legacy_policy_aliases() && id >= POLICY_REFERENCE_START
    }

    fn has_legacy_policy_aliases(&self) -> bool {
        // The original allocator reserved this range only in low catalogs.
        self.last_id < POLICY_REFERENCE_START
    }

    pub(in crate::ai_manager) fn is_policy_profile_reference(
        &self,
        reference: &ProfileRef,
    ) -> bool {
        match reference {
            ProfileRef::Numeric(id) => self.is_policy_reference(*id),
            ProfileRef::Policy(_) => true,
        }
    }
}

impl Profile {
    fn from_policy(id: ProfileId, definition: &PolicyAiProfile) -> Self {
        let mut endpoint = definition.endpoint.clone().unwrap_or_default();
        let connection_mode = match definition.provider {
            PolicyAiProvider::Anthropic => {
                if endpoint.is_empty() {
                    endpoint = "https://api.anthropic.com/v1/messages".into();
                }
                ConnectionMode::Anthropic
            }
            PolicyAiProvider::OpenAiCompatible
                if endpoint
                    .get(..8)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://")) =>
            {
                ConnectionMode::RemoteOpenAi
            }
            PolicyAiProvider::OpenAiCompatible => ConnectionMode::External,
            PolicyAiProvider::LmStudio => ConnectionMode::LmStudio,
            PolicyAiProvider::EmbeddedLlama => ConnectionMode::Managed,
            PolicyAiProvider::EmbeddedMlx => ConnectionMode::ManagedMlx,
        };
        let policy_auto_model = crate::local_ai::Profile::supports_policy_auto_model(definition)
            .then(|| crate::ai_manager::policy_auto::cached(definition))
            .flatten();
        let model = definition
            .model
            .as_deref()
            .or_else(|| {
                policy_auto_model
                    .as_ref()
                    .and_then(|result| result.as_ref().ok())
                    .map(crate::local_ai::PolicyAutoModel::model)
            })
            .unwrap_or_default();
        Self {
            id,
            name: definition.name.clone(),
            connection_mode,
            endpoint,
            model: model.to_owned(),
            vision: if connection_mode.is_managed() {
                Vision::Disabled
            } else {
                Vision::Auto
            },
            policy: Some(Arc::new(definition.clone())),
            policy_auto_model,
            ..Self::default()
        }
    }
}
