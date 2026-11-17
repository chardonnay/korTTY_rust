//! Fixed control frames for replacing a protected Linux regular file.

use serde::{Deserialize, Serialize};

pub use crate::rsync_tree::ExistingDecision as Decision;
use crate::rsync_tree::valid_stage;

pub const ARGUMENT: &str = "--privileged-file-replace-v1";
pub const READY: &[u8] = b"kortty-privileged-file-replace-v1\n";
pub const MAX_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub path: String,
    pub review_only: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recurring: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Payload {
    pub bytes: u64,
    pub sha256: [u8; 32],
}

impl Payload {
    pub fn valid(self) -> bool {
        self.bytes <= MAX_BYTES
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Reviewed {
        fingerprint: [u8; 32],
        bytes: u64,
    },
    Receive,
    Verified {
        payload: Payload,
    },
    Quarantined {
        recovery: String,
    },
    Published {
        payload: Payload,
        recovery: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        successor: Option<[u8; 32]>,
    },
    Aborted,
    Failed {
        outcome_unknown: bool,
        recovery: Option<String>,
    },
}

pub fn valid_recovery(destination: &str, recovery: &str) -> bool {
    destination.rsplit_once('/').is_some_and(|(parent, _)| {
        recovery
            .strip_prefix(parent)
            .and_then(|path| path.strip_prefix('/'))
            .and_then(|path| path.strip_suffix("/original"))
            .is_some_and(valid_stage)
    })
}
