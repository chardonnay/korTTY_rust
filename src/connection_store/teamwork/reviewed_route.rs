//! Bind a reviewed Teamwork route to its exact shared entry.

use serde::{Deserialize, Serialize};

use crate::connection_store::{AuthMethod, SavedLogin};
use crate::ssh_dialog::LoginTarget;

use super::{Entry, Identity};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewedRoute {
    identity: Identity,
    group: String,
    target: Login,
    jump: Option<Login>,
    pub(crate) default_auth_eligible: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Login {
    endpoint: LoginTarget,
    method: AuthMethod,
}

impl From<&SavedLogin> for Login {
    fn from(login: &SavedLogin) -> Self {
        Self {
            endpoint: login.endpoint.clone(),
            method: login.auth.method(),
        }
    }
}

impl From<&Entry> for ReviewedRoute {
    fn from(entry: &Entry) -> Self {
        Self {
            identity: entry.identity().clone(),
            group: entry.group.clone(),
            target: (&entry.connection.login).into(),
            jump: entry.connection.jump.as_ref().map(Login::from),
            default_auth_eligible: entry.default_auth_eligible,
        }
    }
}

impl ReviewedRoute {
    pub(crate) fn identity(&self) -> &Identity {
        &self.identity
    }

    pub(crate) fn group(&self) -> &str {
        &self.group
    }

    pub(crate) fn endpoint(&self) -> &LoginTarget {
        &self.target.endpoint
    }

    pub(crate) fn jump(&self) -> Option<&LoginTarget> {
        self.jump.as_ref().map(|login| &login.endpoint)
    }

    pub(crate) fn matches(&self, entry: &Entry) -> bool {
        *self == Self::from(entry)
    }
}
