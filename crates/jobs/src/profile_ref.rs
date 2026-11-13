use serde::{Deserialize, Serialize};

pub const MAX_POLICY_PROFILE_ID_BYTES: usize = 256;

/// A durable profile selection. Numeric references keep their original JSON shape.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ProfileRef {
    Numeric(u64),
    Policy(#[serde(deserialize_with = "deserialize_policy_reference")] String),
}

impl ProfileRef {
    /// Whether this reference is valid for a workflow that requires a profile.
    pub fn is_nonzero(&self) -> bool {
        match self {
            Self::Numeric(id) => *id != 0,
            Self::Policy(id) => valid_policy_ai_profile_id(id),
        }
    }
}

pub fn valid_policy_ai_profile_id(id: &str) -> bool {
    id.len() <= MAX_POLICY_PROFILE_ID_BYTES
        && id == id.trim_matches(|character| character <= ' ')
        && !id
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}' | '\u{feff}'))
        && id
            .strip_prefix("policy-")
            .is_some_and(|suffix| !suffix.trim().is_empty())
}

fn deserialize_policy_reference<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    let id = String::deserialize(deserializer)?;
    if valid_policy_ai_profile_id(&id) {
        Ok(id)
    } else {
        Err(serde::de::Error::custom(
            "Invalid provisioned AI profile identifier",
        ))
    }
}
