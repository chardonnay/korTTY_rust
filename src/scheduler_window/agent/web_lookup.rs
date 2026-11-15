//! Standing consent for bounded public web research during a firing.

use serde::{Deserialize, Serialize};

use crate::ai_manager::ProfileRef;
use crate::local_ai::web_search::MAX_QUERY_BYTES;

pub(crate) const MAX_PUBLIC_TOPIC_BYTES: usize = MAX_QUERY_BYTES;
pub(crate) const MAX_FOLLOWUP_LOOKUPS: u8 = 2;

pub(crate) fn valid_input(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_PUBLIC_TOPIC_BYTES
        && text.trim() == text
        && text.chars().all(|ch| {
            crate::session_journal::is_display_character(ch)
                && !ch.is_control()
                && !matches!(ch, '\u{2028}' | '\u{2029}')
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Version {
    #[serde(rename = "scheduled-web-lookup-v1")]
    V1,
    #[serde(rename = "scheduled-web-lookup-v2")]
    V2,
    #[serde(rename = "scheduled-web-lookup-v3")]
    V3,
    #[serde(rename = "scheduled-web-lookup-v4")]
    V4,
    #[serde(rename = "scheduled-web-lookup-v5")]
    V5,
}

#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Grant {
    version: Version,
    public_topic: String,
    profile: ProfileRef,
    profile_revision: [u8; 32],
    redaction_revision: [u8; 32],
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    preflight_followups: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    environment_lookup: bool,
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Reviewed automatic web lookup")
    }
}

impl Grant {
    pub(crate) fn new(
        public_topic: String,
        profile: ProfileRef,
        profile_revision: [u8; 32],
        redaction_revision: [u8; 32],
    ) -> Result<Self, String> {
        let grant = Self {
            version: Version::V1,
            public_topic,
            profile,
            profile_revision,
            redaction_revision,
            preflight_followups: false,
            environment_lookup: false,
        };
        grant
            .valid()
            .then_some(grant)
            .ok_or_else(|| "Enter a one-line public research topic of up to 400 bytes.".into())
    }

    pub(crate) fn valid(&self) -> bool {
        self.profile.is_nonzero()
            && valid_input(&self.public_topic)
            && (!self.preflight_followups
                || matches!(self.version, Version::V3 | Version::V4 | Version::V5))
            && (!self.environment_lookup || self.allows_round_lookup())
    }

    pub(crate) fn with_followups(mut self) -> Self {
        match self.version {
            Version::V1 | Version::V2 => self.version = Version::V2,
            Version::V3 | Version::V4 | Version::V5 => self.preflight_followups = true,
        }
        self
    }

    pub(crate) fn allows_followups(&self) -> bool {
        self.version == Version::V2
            || (matches!(self.version, Version::V3 | Version::V4 | Version::V5)
                && self.preflight_followups)
    }

    pub(crate) fn with_environment_lookup(mut self) -> Self {
        if self.allows_round_lookup() {
            self.environment_lookup = true;
        } else {
            self.preflight_followups = self.allows_followups();
            self.version = Version::V3;
        }
        self
    }

    pub(crate) fn allows_environment_lookup(&self) -> bool {
        self.version == Version::V3 || (self.allows_round_lookup() && self.environment_lookup)
    }

    pub(crate) fn with_round_lookup(mut self) -> Self {
        if !self.allows_round_lookup() {
            self.preflight_followups = self.allows_followups();
            self.environment_lookup = self.allows_environment_lookup();
            self.version = Version::V4;
        }
        self
    }

    pub(crate) fn allows_round_lookup(&self) -> bool {
        matches!(self.version, Version::V4 | Version::V5)
    }

    pub(crate) fn with_round_page(self) -> Self {
        let mut grant = self.with_round_lookup();
        grant.version = Version::V5;
        grant
    }

    pub(crate) fn allows_round_page(&self) -> bool {
        self.version == Version::V5
    }

    pub(crate) fn public_topic(&self) -> &str {
        &self.public_topic
    }

    pub(crate) fn profile(&self) -> &ProfileRef {
        &self.profile
    }

    pub(crate) fn profile_revision(&self) -> [u8; 32] {
        self.profile_revision
    }

    pub(crate) fn redaction_revision(&self) -> [u8; 32] {
        self.redaction_revision
    }
}
