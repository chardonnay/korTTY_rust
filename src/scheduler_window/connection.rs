use std::fmt;

use serde::{Deserialize, Serialize};

use crate::connection_store::Connection;
use crate::ssh_dialog::LoginTarget;

pub(crate) const MAX_TARGETS: usize = 8;

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
