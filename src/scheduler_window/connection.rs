use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::connection_store::{Connection, Store};
use crate::ssh_dialog::LoginTarget;

pub(crate) const MAX_TARGETS: usize = 8;
pub(super) const GROUP_TARGETS_CHANGED: &str =
    "Saved connection group targets changed. Refresh the targets and review the job again.";
const MAX_SELECTED_GROUPS: usize = 32;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TargetGroups {
    pub(crate) groups: Vec<String>,
    pub(crate) explicit: Vec<ConnectionRef>,
}

impl TargetGroups {
    pub(super) fn validate(&self, swarm: bool) -> Result<(), &'static str> {
        if self.groups.is_empty()
            || self.groups.len() > MAX_SELECTED_GROUPS
            || self.groups.iter().enumerate().any(|(index, group)| {
                !crate::connection_store::valid_group_path(group)
                    || self.groups[..index].contains(group)
            })
        {
            return Err("Choose distinct saved connection groups.");
        }
        if self.explicit.len() > target_limit(swarm) {
            return Err(target_limit_notice(swarm));
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

    pub(super) fn resolve(
        &self,
        store: &Store,
        swarm: bool,
    ) -> Result<Vec<ConnectionRef>, &'static str> {
        self.validate(swarm)?;
        let limit = target_limit(swarm);
        let mut targets = Vec::with_capacity(limit);
        let mut seen = BTreeSet::new();
        for reference in &self.explicit {
            let connection = store
                .get(reference.id)
                .ok_or("An explicitly selected SSH connection is unavailable.")?;
            if ConnectionRef::from(connection) != *reference {
                return Err("An explicitly selected SSH connection changed. Select it again.");
            }
            seen.insert(reference.id);
            targets.push(reference.clone());
        }
        for group in &self.groups {
            let mut members = store.in_group(group).peekable();
            if !store.has_group(group) || members.peek().is_none() {
                return Err("A selected connection group is missing or has no saved SSH targets.");
            }
            for connection in members {
                if seen.insert(connection.id) {
                    if targets.len() == limit {
                        return Err(target_limit_notice(swarm));
                    }
                    targets.push(connection.into());
                }
            }
        }
        Ok(targets)
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
}

impl From<&Connection> for ConnectionRef {
    fn from(profile: &Connection) -> Self {
        Self {
            id: profile.id,
            target: profile.login.endpoint.clone(),
            jump: profile.jump.as_ref().map(|login| login.endpoint.clone()),
        }
    }
}

impl ConnectionRef {
    pub(super) fn validate(&self) -> Result<(), &'static str> {
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
                write!(formatter, "{name} ({})", reference.target.label())
            }
        }
    }
}
