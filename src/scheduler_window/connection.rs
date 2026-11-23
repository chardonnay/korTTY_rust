use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use kortty_ssh::{Endpoint, TrustStore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::connection_store::teamwork::{
    self, DefaultAuth, Entry, Identity, Origin, ReviewedRoute, SourceKind,
};
use crate::connection_store::{Connection, SavedLogin, Store};
use crate::ssh_dialog::LoginTarget;

pub(crate) const MAX_TARGETS: usize = 8;
pub(super) const GROUP_TARGETS_CHANGED: &str =
    "Saved connection group targets changed. Refresh the targets and review the job again.";
pub(crate) const MAX_SELECTED_GROUPS: usize = 32;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TargetGroups {
    pub(crate) groups: Vec<ConnectionGroup>,
    pub(crate) explicit: Vec<ConnectionRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) group_order: Vec<u64>,
}

impl TargetGroups {
    pub(super) fn has_teamwork_groups(&self) -> bool {
        self.groups
            .iter()
            .any(|group| matches!(group, ConnectionGroup::Teamwork(_)))
    }

    pub(super) fn validate(&self, swarm: bool) -> Result<(), &'static str> {
        if self.groups.is_empty()
            || self.groups.len() > MAX_SELECTED_GROUPS
            || self.groups.iter().enumerate().any(|(index, group)| {
                !crate::connection_store::valid_group_path(group.path())
                    || matches!(group, ConnectionGroup::Teamwork(group) if group.origin().is_err())
                    || self.groups[..index].contains(group)
            })
        {
            return Err("Choose distinct saved connection groups.");
        }
        if self.explicit.len() > target_limit(swarm) || self.group_order.len() > target_limit(swarm)
        {
            return Err(target_limit_notice(swarm));
        }
        if self
            .group_order
            .iter()
            .enumerate()
            .any(|(index, id)| *id == 0 || self.group_order[..index].contains(id))
        {
            return Err("Choose each ordered group target only once.");
        }
        let mut ids = BTreeSet::new();
        for reference in &self.explicit {
            reference.validate()?;
            if !ids.insert(reference.id) {
                return Err("Choose each saved SSH connection only once.");
            }
        }
        Ok(())
    }

    pub(crate) fn resolve(
        &self,
        store: &Store,
        shared: &SharedTargets,
        swarm: bool,
    ) -> Result<Vec<ConnectionRef>, &'static str> {
        self.validate(swarm)?;
        let limit = target_limit(swarm);
        let mut targets = Vec::with_capacity(limit);
        for reference in &self.explicit {
            reference.require_current_selection(store, shared)?;
            targets.push(reference.clone());
        }
        for group in &self.groups {
            let mut members: Box<dyn Iterator<Item = ConnectionRef>> = match group {
                ConnectionGroup::Local(path) => {
                    Box::new(store.in_group(path).map(ConnectionRef::from))
                }
                ConnectionGroup::Teamwork(group) => {
                    let source = group.origin()?;
                    Box::new(
                        shared
                            .entries
                            .iter()
                            .filter(move |(identity, target)| {
                                identity.source() == &source
                                    && crate::connection_store::group_suffix(
                                        &target.group,
                                        &group.path,
                                    )
                                    .is_some()
                            })
                            .map(|(_, target)| target.reference.clone()),
                    )
                }
            };
            let Some(first) = members.next() else {
                return Err("A selected connection group is missing or has no saved SSH targets.");
            };
            for reference in std::iter::once(first).chain(members) {
                if let Some(previous) = targets.iter().find(|previous| previous.id == reference.id)
                {
                    if *previous != reference {
                        return Err("Selected SSH connection IDs are ambiguous.");
                    }
                    continue;
                }
                if targets.len() == limit {
                    return Err(target_limit_notice(swarm));
                }
                targets.push(reference);
            }
        }
        if !self.group_order.is_empty() {
            targets[self.explicit.len()..].sort_by_key(|reference| {
                self.group_order
                    .iter()
                    .position(|id| *id == reference.id)
                    .unwrap_or(usize::MAX)
            });
        }
        Ok(targets)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ConnectionGroup {
    Local(String),
    Teamwork(SharedGroup),
}

impl From<String> for ConnectionGroup {
    fn from(value: String) -> Self {
        Self::Local(value)
    }
}

impl From<&str> for ConnectionGroup {
    fn from(value: &str) -> Self {
        Self::Local(value.to_owned())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SharedGroup {
    kind: SourceKind,
    source: String,
    path: String,
}

impl SharedGroup {
    fn origin(&self) -> Result<Origin, &'static str> {
        self.kind
            .parse_origin(&self.source)
            .ok()
            .filter(|origin| origin.to_string() == self.source)
            .ok_or("Choose a valid Teamwork group source.")
    }
}

impl ConnectionGroup {
    pub(crate) fn from_shared(source: &Origin, path: &str) -> Self {
        let kind = match source {
            Origin::File(_) => SourceKind::File,
            Origin::Git(_) => SourceKind::Git,
        };
        Self::Teamwork(SharedGroup {
            kind,
            source: source.to_string(),
            path: path.to_owned(),
        })
    }

    fn path(&self) -> &str {
        match self {
            Self::Local(path) => path,
            Self::Teamwork(group) => &group.path,
        }
    }
}

impl fmt::Display for ConnectionGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(path) => formatter.write_str(path),
            Self::Teamwork(group) => write!(
                formatter,
                "{} [{}]",
                teamwork::group_title(&group.path),
                group.source
            ),
        }
    }
}

#[derive(Default)]
pub(crate) struct SharedTargets {
    entries: BTreeMap<Identity, SharedTarget>,
}

struct SharedTarget {
    reference: ConnectionRef,
    name: String,
    group: String,
}

impl SharedTargets {
    pub(super) fn group_path(&self, reference: &ConnectionRef) -> Option<&str> {
        Some(&self.entries.get(reference.shared_identity()?)?.group)
    }

    pub(crate) fn new(
        catalog: &teamwork::Catalog,
        store: &Store,
        trust: &TrustStore,
        mut binding: impl FnMut(&Entry) -> Option<(LoginTarget, SharedAuth)>,
    ) -> Self {
        let pins: BTreeMap<_, _> = trust
            .list()
            .unwrap_or_default()
            .into_iter()
            .map(|host| (host.endpoint, host.key.fingerprint()))
            .collect();
        let find_pin = |login: &LoginTarget| {
            Endpoint::new(&login.host, login.port)
                .ok()
                .and_then(|endpoint| pins.get(&endpoint).cloned())
        };
        let mut targets = Self::default();
        for entry in catalog
            .entries()
            .filter(|entry| !store.is_teamwork_hidden(entry.identity()))
        {
            let endpoint = &entry.connection.login.endpoint;
            let pin = find_pin(endpoint);
            let jump = entry.connection.jump.as_ref().and_then(|login| {
                let saved = local_jump_profile(store, login)?;
                Some(SharedJump {
                    profile: saved.id,
                    login_digest: login_digest(&saved.login)?,
                    pin: find_pin(&login.endpoint),
                })
            });
            let auth = if entry.connection.incognito || store.teamwork_requires_isolation(entry) {
                None
            } else {
                binding(entry)
            };
            targets.entries.insert(
                entry.identity().clone(),
                SharedTarget {
                    reference: ConnectionRef::shared(entry, auth, pin, jump),
                    name: entry.connection.name.clone(),
                    group: entry.group.clone(),
                },
            );
        }
        targets
    }

    pub(crate) fn load_reviewed(
        store: &Store,
        reviewed: impl IntoIterator<Item = ConnectionRef>,
    ) -> Result<Self, &'static str> {
        let root = crate::profile::root().map_err(|_| "The Teamwork profile is unavailable.")?;
        let policy = crate::clipboard_policy::ClipboardPolicy::load();
        let reviewed = reviewed.into_iter().collect::<Vec<_>>();
        for reference in &reviewed {
            reference.require_shared_policy(&policy)?;
        }
        let catalog = teamwork::Catalog::load_for_scheduler(&root, &policy)
            .map_err(|_| "The current Teamwork sources are unavailable.")?;
        let auth = DefaultAuth::load(&root)
            .map_err(|_| "The current Teamwork authentication settings are unavailable.")?;
        let trust = crate::ssh_connection::trust_store()
            .map_err(|_| "The SSH host-key store is unavailable.")?;
        Ok(Self::recheck(&catalog, store, &auth, &trust, reviewed))
    }

    pub(crate) fn recheck(
        catalog: &teamwork::Catalog,
        store: &Store,
        auth: &DefaultAuth,
        trust: &TrustStore,
        reviewed: impl IntoIterator<Item = ConnectionRef>,
    ) -> Self {
        let reviewed: BTreeMap<_, _> = reviewed
            .into_iter()
            .filter_map(|reference| {
                let identity = reference.teamwork.as_ref()?.route.identity().clone();
                Some((identity, reference))
            })
            .collect();
        Self::new(catalog, store, trust, |entry| {
            let reference = reviewed.get(entry.identity())?;
            reference.require_shared_entry(entry, auth).ok()?;
            Some((reference.target.clone(), reference.shared_auth()?.clone()))
        })
    }

    pub(crate) fn java_target(
        &self,
        source: &Origin,
        id: &str,
        group: &str,
        target: &LoginTarget,
        jump: Option<&LoginTarget>,
    ) -> Option<&ConnectionRef> {
        let (_, entry) = self
            .entries
            .iter()
            .find(|(identity, _)| identity.source() == source && identity.external_id() == id)?;
        let route = &entry.reference.teamwork.as_ref()?.route;
        (entry.group == group
            && route.endpoint() == target
            && route.jump() == jump
            && entry.reference.validate().is_ok())
        .then_some(&entry.reference)
    }

    pub(crate) fn require_saved(
        &self,
        store: &Store,
        reference: &ConnectionRef,
    ) -> Result<(), &'static str> {
        if reference.shared_identity().is_some() {
            return self.require(reference);
        }
        if store.get(reference.id).map(ConnectionRef::from).as_ref() != Some(reference) {
            return Err("The saved SSH route changed or is unavailable.");
        }
        Ok(())
    }

    pub(super) fn require_for_adoption(
        &self,
        store: &Store,
        reference: &ConnectionRef,
        policy: &crate::clipboard_policy::ClipboardPolicy,
        changed: &'static str,
    ) -> Result<(), &'static str> {
        reference.require_shared_policy(policy)?;
        self.require_saved(store, reference).map_err(|_| changed)?;
        if reference.teamwork.is_none() {
            let saved = store
                .get(reference.id)
                .ok_or("The saved SSH route is unavailable.")?;
            if saved.incognito || store.connection_requires_isolation(saved) {
                return Err(
                    "This saved connection requires privacy or isolation unavailable to jobs.",
                );
            }
        }
        Ok(())
    }

    pub(crate) fn require(&self, reference: &ConnectionRef) -> Result<(), &'static str> {
        let Some(binding) = &reference.teamwork else {
            return Ok(());
        };
        if self
            .entries
            .get(binding.route.identity())
            .map(|target| &target.reference)
            != Some(reference)
        {
            return Err(
                "A Teamwork SSH route or its local authentication changed or is unavailable. Select it again.",
            );
        }
        reference.validate()
    }

    pub(super) fn choices(&self) -> impl Iterator<Item = ConnectionChoice> + '_ {
        self.entries
            .iter()
            .map(|(identity, target)| ConnectionChoice::Saved {
                reference: target.reference.clone(),
                name: format!("{} [Teamwork: {}]", target.name, identity.source()),
            })
    }

    pub(super) fn groups(&self) -> impl Iterator<Item = ConnectionGroup> {
        let mut groups = BTreeSet::new();
        for (identity, target) in &self.entries {
            let kind = match identity.source() {
                Origin::File(_) => SourceKind::File,
                Origin::Git(_) => SourceKind::Git,
            };
            let mut path = target.group.as_str();
            while !path.is_empty() {
                groups.insert(SharedGroup {
                    kind,
                    source: identity.source().to_string(),
                    path: path.to_owned(),
                });
                path = path.rsplit_once('/').map_or("", |(parent, _)| parent);
            }
        }
        groups.into_iter().map(ConnectionGroup::Teamwork)
    }
}

pub(super) fn target_limit(swarm: bool) -> usize {
    if swarm {
        kortty_jobs::MAX_SWARM_TARGETS
    } else {
        MAX_TARGETS
    }
}

pub(super) fn target_limit_notice(swarm: bool) -> &'static str {
    if swarm {
        "An AI Swarm job can use up to 32 saved SSH connections"
    } else {
        "A job can use up to eight saved SSH connections"
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionRef {
    pub id: u64,
    pub target: LoginTarget,
    pub jump: Option<LoginTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) teamwork: Option<Box<SharedBinding>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SharedBinding {
    route: ReviewedRoute,
    auth: Option<SharedAuth>,
    pin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    jump: Option<SharedJump>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SharedJump {
    profile: u64,
    login_digest: [u8; 32],
    pin: Option<String>,
}

fn local_jump_profile<'a>(store: &'a Store, shared: &SavedLogin) -> Option<&'a Connection> {
    if shared.keyboard_interactive || shared.endpoint.validate().is_err() {
        return None;
    }
    let mut matches = store.connections().iter().filter(|profile| {
        profile.jump.is_none()
            && !profile.incognito
            && !store.connection_requires_isolation(profile)
            && profile.login.endpoint == shared.endpoint
            && profile.login.auth.method() == shared.auth.method()
            && !profile.login.keyboard_interactive
            && !profile.login.auth.requires_interactive()
    });
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn login_digest(login: &SavedLogin) -> Option<[u8; 32]> {
    let mut hash = Sha256::new();
    hash.update(b"kortty scheduler teamwork jump login\0");
    hash.update(serde_json::to_vec(login).ok()?);
    Some(hash.finalize().into())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum SharedAuth {
    Password {
        credential: String,
    },
    Key {
        id: u64,
        password_preference: Option<String>,
    },
}

fn shared_id(identity: &Identity) -> u64 {
    let mut hash = Sha256::new();
    hash.update(b"kortty scheduler teamwork target\0");
    hash.update(serde_json::to_vec(identity).expect("validated Teamwork identity"));
    let bytes = hash.finalize();
    u64::from_be_bytes(bytes[..8].try_into().expect("eight hash bytes")) | (1 << 63)
}

impl From<&Connection> for ConnectionRef {
    fn from(profile: &Connection) -> Self {
        Self {
            id: profile.id,
            target: profile.login.endpoint.clone(),
            jump: profile.jump.as_ref().map(|login| login.endpoint.clone()),
            teamwork: None,
        }
    }
}

impl ConnectionRef {
    pub(super) fn require_current_selection(
        &self,
        store: &Store,
        shared: &SharedTargets,
    ) -> Result<(), &'static str> {
        let current = if let Some(identity) = self.shared_identity() {
            shared
                .entries
                .get(identity)
                .map(|target| target.reference.clone())
        } else {
            store.get(self.id).map(Self::from)
        };
        match current {
            None => Err("An explicitly selected SSH connection is unavailable."),
            Some(current) if current != *self => {
                Err("An explicitly selected SSH connection changed. Select it again.")
            }
            Some(_) => Ok(()),
        }
    }

    fn shared(
        entry: &Entry,
        binding: Option<(LoginTarget, SharedAuth)>,
        pin: Option<String>,
        jump: Option<SharedJump>,
    ) -> Self {
        let route = ReviewedRoute::from(entry);
        let (target, auth) = match binding {
            Some((target, auth)) => (target, Some(auth)),
            None => (route.endpoint().clone(), None),
        };
        Self {
            id: shared_id(entry.identity()),
            target,
            jump: route.jump().cloned(),
            teamwork: Some(Box::new(SharedBinding {
                route,
                auth,
                pin,
                jump,
            })),
        }
    }

    pub(crate) fn require_shared_policy(
        &self,
        policy: &crate::clipboard_policy::ClipboardPolicy,
    ) -> Result<(), &'static str> {
        if self.teamwork.is_none() {
            return Ok(());
        }
        if policy.problem.is_some() || !policy.teamwork_allowed {
            return Err("Managed policy forbids this Teamwork job.");
        }
        if !policy.minimum_isolation.is_none() {
            return Err("Current policy requires isolation for this Teamwork job.");
        }
        if policy
            .server_access()
            .first_blocked(self.target.servers(self.jump.as_ref()))
            .is_some()
        {
            return Err("Managed policy blocks a reviewed Teamwork SSH server.");
        }
        Ok(())
    }

    pub(crate) fn strict_trust(&self, trust: TrustStore) -> Result<TrustStore, &'static str> {
        let Some(binding) = &self.teamwork else {
            return Ok(trust);
        };
        let changed = "The trusted SSH host key for this Teamwork route changed or is unavailable.";
        let mut expected = vec![(&self.target, binding.pin.as_deref())];
        match (&self.jump, &binding.jump) {
            (Some(login), Some(bastion)) => expected.push((login, bastion.pin.as_deref())),
            (None, None) => {}
            _ => return Err(changed),
        }
        let endpoints = expected
            .iter()
            .map(|(login, _)| Endpoint::new(&login.host, login.port))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| changed)?;
        let pins = trust.selected_pins(&endpoints).map_err(|_| changed)?;
        for (endpoint, (_, fingerprint)) in endpoints.iter().zip(expected) {
            if !pins.iter().any(|(host, key)| {
                host == endpoint && Some(key.fingerprint().as_str()) == fingerprint
            }) {
                return Err(changed);
            }
        }
        TrustStore::from_pins(pins).map_err(|_| changed)
    }

    pub(crate) fn require_installed_shared_route(&self) -> Result<(), &'static str> {
        if self.teamwork.is_none() {
            return Ok(());
        }
        let path = crate::profile::connections_path()
            .map_err(|_| "The saved SSH connections are unavailable.")?;
        self.require_saved_at(&path)
    }

    pub(crate) fn require_saved_at(&self, path: &std::path::Path) -> Result<(), &'static str> {
        let store = Store::open(path.to_owned())
            .map_err(|_| "The saved SSH connections are unavailable.")?;
        let shared = if self.teamwork.is_some() {
            SharedTargets::load_reviewed(&store, [self.clone()])?
        } else {
            SharedTargets::default()
        };
        shared.require_saved(&store, self)
    }

    pub(crate) fn shared_identity(&self) -> Option<&Identity> {
        Some(self.teamwork.as_ref()?.route.identity())
    }

    pub(crate) fn shared_jump_login<'a>(
        &self,
        entry: &Entry,
        store: &'a Store,
    ) -> Result<Option<&'a SavedLogin>, &'static str> {
        let Some(jump) = &entry.connection.jump else {
            return Ok(None);
        };
        let changed = "The local Teamwork jump login changed or is unavailable.";
        let saved = local_jump_profile(store, jump).ok_or(changed)?;
        let binding = self
            .teamwork
            .as_ref()
            .and_then(|binding| binding.jump.as_ref())
            .ok_or(changed)?;
        if binding.profile != saved.id || Some(binding.login_digest) != login_digest(&saved.login) {
            return Err(changed);
        }
        Ok(Some(&saved.login))
    }

    pub(crate) fn shared_auth(&self) -> Option<&SharedAuth> {
        self.teamwork.as_ref()?.auth.as_ref()
    }

    pub(super) fn same_identity(&self, other: &Self) -> bool {
        match (self.shared_identity(), other.shared_identity()) {
            (None, None) => self.id == other.id,
            (Some(left), Some(right)) => left == right,
            _ => false,
        }
    }

    pub(crate) fn require_shared_route(&self, entry: &Entry) -> Result<(), &'static str> {
        self.validate()?;
        if entry.connection.incognito || !entry.connection.isolation.is_none() {
            return Err("This saved connection requires privacy or isolation unavailable to jobs.");
        }
        if !self
            .teamwork
            .as_ref()
            .is_some_and(|binding| binding.route.matches(entry))
        {
            return Err("The reviewed Teamwork SSH route changed.");
        }
        Ok(())
    }

    fn require_shared_entry(
        &self,
        entry: &Entry,
        current: &DefaultAuth,
    ) -> Result<(), &'static str> {
        let binding = self
            .teamwork
            .as_ref()
            .ok_or("Teamwork route is unavailable.")?;
        let changed = "The reviewed Teamwork SSH route or its local authentication changed.";
        self.require_shared_route(entry)?;
        match self.shared_auth() {
            Some(SharedAuth::Password { credential })
                if current.credential() == Some(credential.as_str()) =>
            {
                Ok(())
            }
            Some(SharedAuth::Key {
                id,
                password_preference,
            }) if current.key() == Some(*id)
                && current.credential() == password_preference.as_deref() =>
            {
                let ordered = current.ordered();
                let username = ordered.username.trim();
                let expected = if username.is_empty() {
                    &binding.route.endpoint().username
                } else {
                    username
                };
                if self.target.username == expected {
                    Ok(())
                } else {
                    Err(changed)
                }
            }
            _ => Err(changed),
        }
    }

    pub(super) fn validate(&self) -> Result<(), &'static str> {
        if let Some(binding) = &self.teamwork {
            let endpoint = binding.route.endpoint();
            if self.id != shared_id(binding.route.identity())
                || endpoint.host != self.target.host
                || endpoint.port != self.target.port
                || binding.route.jump() != self.jump.as_ref()
                || !binding.route.default_auth_eligible
            {
                return Err("Choose a valid reviewed Teamwork SSH connection.");
            }
            if binding.auth.is_none() || self.jump.is_some() != binding.jump.is_some() {
                return Err(
                    "Teamwork jobs require local unattended SSH authentication for the complete route.",
                );
            }
            if binding.pin.is_none() || binding.jump.as_ref().is_some_and(|hop| hop.pin.is_none()) {
                return Err(
                    "Verify every SSH host key before scheduling this Teamwork connection.",
                );
            }
        }
        if self.id == 0
            || self.target.validate().is_err()
            || self
                .jump
                .as_ref()
                .is_some_and(|jump| jump.validate().is_err())
        {
            return Err("Choose a valid saved SSH connection");
        }
        Ok(())
    }
}

#[derive(Clone, Eq, PartialEq)]
pub enum ConnectionChoice {
    Manual,
    Saved {
        reference: ConnectionRef,
        name: String,
    },
}

impl ConnectionChoice {
    pub(super) fn reference(&self) -> Option<&ConnectionRef> {
        match self {
            Self::Manual => None,
            Self::Saved { reference, .. } => Some(reference),
        }
    }
}

impl fmt::Display for ConnectionChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manual => formatter.write_str("Manual SSH endpoint and key file"),
            Self::Saved { name, reference } => {
                write!(formatter, "{name} ({})", reference.target.label())?;
                if let Some(binding) = &reference.teamwork {
                    if let Some(jump) = &reference.jump {
                        write!(formatter, " via {}", jump.label())?;
                    }
                    if binding.auth.is_none() {
                        formatter.write_str(" · local authentication unavailable")?;
                    } else if reference.jump.is_some() && binding.jump.is_none() {
                        formatter.write_str(" · matching local jump login unavailable")?;
                    } else if binding.pin.is_none()
                        || binding.jump.as_ref().is_some_and(|hop| hop.pin.is_none())
                    {
                        formatter.write_str(" · verify SSH host keys first")?;
                    }
                }
                Ok(())
            }
        }
    }
}
