//! Saved routes and live streams occupy disjoint target identity spaces.

use crate::session::SessionId;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Key {
    Live { session: SessionId, stream: u64 },
    Saved(u64),
}

impl Key {
    pub(crate) fn pane(self) -> Option<(SessionId, u64)> {
        match self {
            Self::Live { session, stream } => Some((session, stream)),
            Self::Saved(_) => None,
        }
    }

    pub(crate) fn stable(self) -> (u8, u64) {
        match self {
            Self::Live { session, .. } => (0, session.0),
            Self::Saved(id) => (1, id),
        }
    }
}
