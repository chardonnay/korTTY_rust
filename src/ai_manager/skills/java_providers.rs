//! Inactive external provider metadata retained from Java backups.

use std::{collections::BTreeMap, io};

use serde::{Deserialize, Serialize};

use super::external::{Provider, ProviderProfile};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(in crate::ai_manager) enum Authentication {
    #[default]
    None,
    Token,
    Basic,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ai_manager) struct Hint {
    pub(in crate::ai_manager) name: String,
    pub(in crate::ai_manager) provider: Provider,
    pub(in crate::ai_manager) base_url: String,
    pub(in crate::ai_manager) authentication: Authentication,
    pub(in crate::ai_manager) username: String,
}

impl Hint {
    fn valid(&self) -> bool {
        let draft = self.draft("");
        let Ok(base) = draft.base() else {
            return false;
        };
        !self.name.trim().is_empty()
            && self.name.len() <= 120
            && !self.name.chars().any(char::is_control)
            && self.username.len() <= 128
            && !self.username.chars().any(char::is_control)
            && !self.username.contains(':')
            && (self.authentication == Authentication::None || base.is_some())
            && if self.authentication == Authentication::Basic {
                self.provider == Provider::Http && !self.username.trim().is_empty()
            } else {
                self.username.is_empty()
            }
    }

    pub(super) fn draft(&self, id: &str) -> ProviderProfile {
        ProviderProfile {
            name: self.name.clone(),
            provider: self.provider,
            base_url: self.base_url.clone(),
            username: self.username.clone(),
            enabled: false,
            java_source: Some(id.to_owned()),
            ..ProviderProfile::default()
        }
    }
}

pub(super) fn valid_source_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(transparent)]
pub(in crate::ai_manager) struct Hints(BTreeMap<String, Hint>);

impl Hints {
    pub(in crate::ai_manager) fn len(&self) -> usize {
        self.0.len()
    }
    pub(in crate::ai_manager) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub(in crate::ai_manager) fn iter(&self) -> impl Iterator<Item = (&str, &Hint)> {
        self.0.iter().map(|(id, hint)| (id.as_str(), hint))
    }
    pub(in crate::ai_manager) fn get(&self, id: &str) -> Option<&Hint> {
        self.0.get(id)
    }
    pub(in crate::ai_manager) fn insert(&mut self, id: &str, hint: Hint) -> bool {
        if self.len() >= ProviderProfile::MAX_PROFILES
            || !valid_source_id(id)
            || !hint.valid()
            || self.0.contains_key(id)
        {
            return false;
        }
        self.0.insert(id.to_owned(), hint);
        true
    }
    pub(in crate::ai_manager) fn merge(&mut self, incoming: Self) -> bool {
        let before = self.len();
        for (id, hint) in incoming.0 {
            self.insert(&id, hint);
        }
        self.len() != before
    }
    pub(in crate::ai_manager) fn validate(&self) -> io::Result<()> {
        if self.len() > ProviderProfile::MAX_PROFILES
            || self
                .iter()
                .any(|(id, hint)| !valid_source_id(id) || !hint.valid())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid Java skill provider suggestions",
            ));
        }
        Ok(())
    }
}
