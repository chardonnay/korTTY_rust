//! The saved route, skill assignments and credential authority for a model review.

use std::sync::{Arc, Weak};

use kortty_ssh::{Endpoint, HostKey, TrustStore};

use crate::ai_manager::ProfileId;
use crate::connection_store::ActiveAuth;
use crate::scheduler_window::ConnectionRef;
use crate::ssh_key_catalog::{Catalog, KeySource};
use crate::terminal_agent::swarm::headless::route_endpoints;

use super::super::super::RunTarget;
use super::{Connection, Mode, Purpose, Redactor, Review, Run, Source, Workspace};

const TARGET_CHANGED: &str = "The reviewed saved Swarm target changed. Start a new review.";
const PINS_CHANGED: &str =
    "The saved Swarm target's skill assignments changed. Start a new review.";
const TRUST_CHANGED: &str = "A reviewed Swarm host key changed. Start a new review.";
const MASK_CHANGED: &str = "The reviewed Swarm credential mask changed. Start a new review.";
const VAULT_CHANGED: &str = "The reviewed Swarm credential vault changed or was locked.";
const KEYS_CHANGED: &str = "A reviewed Swarm SSH identity changed. Start a new review.";

pub(in crate::workspace::terminal_agent) struct ModelReview {
    source: Source,
    mode: Mode,
    profile: ProfileId,
    target: ModelTarget,
    trust: TrustStore,
    pins: Vec<u64>,
    redactor: Redactor,
    vault_revision: Option<Weak<()>>,
    keys: Vec<StoredKey>,
}

enum ModelTarget {
    Advisory {
        endpoints: Vec<Endpoint>,
        host_keys: Vec<(Endpoint, HostKey)>,
    },
    Reviewed(Arc<Review>),
}

#[derive(PartialEq, Eq)]
struct StoredKey {
    id: u64,
    source: KeySource,
    passphrase_id: Option<String>,
}

impl ModelReview {
    pub(in crate::workspace::terminal_agent) fn capture(
        workspace: &Workspace,
        run: &Run,
    ) -> Result<Self, String> {
        let trust = crate::ssh_connection::trust_store()
            .map_err(|_| "The SSH host-key store is unavailable.")?;
        Self::capture_with_trust(workspace, run, trust)
    }

    pub(super) fn capture_with_trust(
        workspace: &Workspace,
        run: &Run,
        trust: TrustStore,
    ) -> Result<Self, String> {
        let source = run.binding.saved().ok_or(TARGET_CHANGED)?;
        let Purpose::Agent(profile) = &run.purpose else {
            return Err(TARGET_CHANGED.into());
        };
        if !run.swarm
            || source
                .follow_up
                .as_ref()
                .is_some_and(|follow_up| follow_up.profile != *profile)
        {
            return Err(TARGET_CHANGED.into());
        }
        let target = ModelTarget::capture(source, run, &trust)?;
        let connection = workspace.saved_swarm_connection(source, run.mode, true)?;
        let keys = stored_keys(&connection)?;
        let (_, redactor, vault_revision) =
            workspace.saved_swarm_credentials_with_revision(source, &connection)?;
        let redactor = source
            .follow_up
            .as_ref()
            .map_or(redactor, |follow_up| follow_up.redactor.clone());
        require_same_mask(&redactor, &run.redactor)?;
        let review = Self {
            source: source.clone(),
            mode: run.mode,
            profile: profile.id,
            target,
            trust,
            pins: connection.terminal.ai_skills,
            redactor,
            vault_revision,
            keys,
        };
        review.check_worker()?;
        Ok(review)
    }

    pub(in crate::workspace::terminal_agent) fn pins(&self) -> &[u64] {
        &self.pins
    }

    pub(in crate::workspace::terminal_agent) fn require_current(
        &self,
        workspace: &Workspace,
        run: &Run,
    ) -> Result<(), String> {
        let source = run.binding.saved().ok_or(TARGET_CHANGED)?;
        if !run.swarm
            || run.mode != self.mode
            || run.profile().map(|profile| profile.id) != Some(self.profile)
            || !same_source(&self.source, source)
            || !self.target.matches_run(run)
        {
            return Err(TARGET_CHANGED.into());
        }
        workspace.saved_swarm_policy(source, run.mode, true)?;
        let connection = self.checked_connection()?;
        let (_, redactor, vault_revision) =
            workspace.saved_swarm_credentials_with_revision(source, &connection)?;
        if !same_vault_revision(self.vault_revision.as_ref(), vault_revision.as_ref()) {
            return Err(VAULT_CHANGED.into());
        }
        let current = source
            .follow_up
            .as_ref()
            .map_or(&redactor, |follow_up| &follow_up.redactor);
        require_same_mask(&self.redactor, current)?;
        require_same_mask(&self.redactor, &run.redactor)
    }

    pub(in crate::workspace::terminal_agent) fn check_worker(&self) -> Result<(), String> {
        self.checked_connection().map(|_| ())
    }

    fn checked_connection(&self) -> Result<Connection, String> {
        let connection = self.source.current_connection()?;
        if connection.terminal.ai_skills != self.pins {
            return Err(PINS_CHANGED.into());
        }
        if stored_keys(&connection)? != self.keys {
            return Err(KEYS_CHANGED.into());
        }
        self.target
            .require_trust(&self.source.reference, &self.trust)?;
        if self
            .vault_revision
            .as_ref()
            .is_some_and(vault_revision_expired)
            || self
                .source
                .follow_up
                .as_ref()
                .and_then(|scope| scope.vault_revision.as_ref())
                .is_some_and(vault_revision_expired)
        {
            return Err(VAULT_CHANGED.into());
        }
        Ok(connection)
    }
}

impl ModelTarget {
    fn capture(source: &Source, run: &Run, trust: &TrustStore) -> Result<Self, String> {
        match (run.mode, &run.target, &source.follow_up) {
            (Mode::Ask, None, None) => {
                let endpoints = route_endpoints(&source.reference).map_err(|_| TARGET_CHANGED)?;
                let host_keys = trust
                    .selected_pins(&endpoints)
                    .map_err(|_| "The saved Swarm host keys could not be reviewed.")?;
                Ok(Self::Advisory {
                    endpoints,
                    host_keys,
                })
            }
            (Mode::Ask | Mode::Run, Some(RunTarget::Saved(review)), follow_up)
                if (run.mode == Mode::Run || follow_up.is_some())
                    && review.reference() == &source.reference
                    && review.directory() == source.directory
                    && follow_up
                        .as_ref()
                        .is_none_or(|scope| Arc::ptr_eq(&scope.review, review)) =>
            {
                Ok(Self::Reviewed(Arc::clone(review)))
            }
            _ => Err(TARGET_CHANGED.into()),
        }
    }

    fn matches_run(&self, run: &Run) -> bool {
        match (self, &run.target) {
            (Self::Advisory { .. }, None) => true,
            (Self::Reviewed(review), Some(RunTarget::Saved(current))) => {
                Arc::ptr_eq(review, current)
            }
            _ => false,
        }
    }

    fn require_trust(&self, reference: &ConnectionRef, trust: &TrustStore) -> Result<(), String> {
        match self {
            Self::Advisory {
                endpoints,
                host_keys,
            } => {
                if !trust
                    .selected_pins(endpoints)
                    .is_ok_and(|current| current == *host_keys)
                {
                    return Err(TRUST_CHANGED.into());
                }
            }
            Self::Reviewed(review) => {
                review
                    .trust_snapshot(reference, trust)
                    .map_err(|_| TRUST_CHANGED)?;
            }
        }
        Ok(())
    }
}

fn stored_keys(connection: &Connection) -> Result<Vec<StoredKey>, String> {
    let ids: Vec<_> = std::iter::once(&connection.login)
        .chain(connection.jump.iter())
        .filter_map(|login| match login.auth.active() {
            ActiveAuth::StoredKey(id) => Some(id),
            _ => None,
        })
        .collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let catalog =
        Catalog::open_profile().map_err(|_| "The registered SSH identities are unavailable.")?;
    ids.into_iter()
        .map(|id| {
            let key = catalog.get(id).ok_or(KEYS_CHANGED)?;
            Ok(StoredKey {
                id,
                source: key.source.clone(),
                passphrase_id: key.passphrase.clone(),
            })
        })
        .collect()
}

fn same_source(reviewed: &Source, current: &Source) -> bool {
    reviewed.reference == current.reference
        && reviewed.directory == current.directory
        && reviewed.security == current.security
        && reviewed.store_path == current.store_path
        && match (&reviewed.follow_up, &current.follow_up) {
            (Some(reviewed), Some(current)) => {
                Arc::ptr_eq(&reviewed.review, &current.review)
                    && reviewed.profile == current.profile
                    && reviewed.profile_revision == current.profile_revision
                    && same_vault_revision(
                        reviewed.vault_revision.as_ref(),
                        current.vault_revision.as_ref(),
                    )
            }
            (None, None) => true,
            _ => false,
        }
}

fn same_vault_revision(reviewed: Option<&Weak<()>>, current: Option<&Weak<()>>) -> bool {
    match (reviewed, current) {
        (Some(reviewed), Some(current)) => Weak::ptr_eq(reviewed, current),
        (None, None) => true,
        _ => false,
    }
}

fn vault_revision_expired(revision: &Weak<()>) -> bool {
    revision.strong_count() == 0
}

fn require_same_mask(reviewed: &Redactor, current: &Redactor) -> Result<(), String> {
    if !reviewed.matches_secrets(current)
        || !reviewed
            .redaction_policy()
            .equivalent_redactions(current.redaction_policy())
    {
        return Err(MASK_CHANGED.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_review_detects_changes_to_an_intermediate_host_key() {
        let reference: ConnectionRef = serde_json::from_value(serde_json::json!({
            "id": 1,
            "target": {"host": "target.example.test", "port": 22, "username": "operator"},
            "jump": {"host": "jump.example.test", "port": 2222, "username": "operator"},
        }))
        .unwrap();
        let key = HostKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
        )
        .unwrap();
        let target = Endpoint::new("target.example.test", 22).unwrap();
        let hop = Endpoint::new("jump.example.test", 2222).unwrap();
        let trusted =
            TrustStore::from_pins(vec![(target.clone(), key.clone()), (hop, key.clone())]).unwrap();
        let missing_hop = TrustStore::from_pins(vec![(target, key)]).unwrap();
        let endpoints = route_endpoints(&reference).unwrap();
        let advisory = ModelTarget::Advisory {
            host_keys: trusted.selected_pins(&endpoints).unwrap(),
            endpoints,
        };
        let agent = ModelTarget::Reviewed(Arc::new(
            Review::capture(reference.clone(), "/srv".into(), &trusted).unwrap(),
        ));
        for review in [advisory, agent] {
            assert!(review.require_trust(&reference, &trusted).is_ok());
            assert_eq!(
                review.require_trust(&reference, &missing_hop).unwrap_err(),
                TRUST_CHANGED
            );
        }
    }
}
