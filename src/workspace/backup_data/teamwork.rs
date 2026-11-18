//! Inactive Teamwork proposals from Java global settings.

use std::collections::BTreeSet;
use std::io;

use roxmltree::Node;
use serde::{Deserialize, Serialize};

use crate::connection_store::teamwork::{
    GitUrl, MAX_CHECK_INTERVAL_MINUTES, MAX_LOCATION_BYTES, MAX_SOURCES, SourceKind,
};

use super::{java_flag, preference, preference_node, preference_text};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Proposals {
    pub(crate) sources: Vec<SourceProposal>,
    pub(crate) interval: Option<u16>,
    pub(crate) credential: Option<String>,
    pub(crate) key: Option<String>,
    pub(crate) username: Option<String>,
    pub(crate) temporary_key: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceProposal {
    pub(crate) id: Option<String>,
    pub(crate) kind: SourceKind,
    pub(crate) location: String,
    pub(crate) interval: u16,
    pub(crate) was_enabled: bool,
}

impl Proposals {
    pub(crate) fn count(&self) -> usize {
        self.sources.len()
            + [
                self.interval.is_some(),
                self.credential.is_some(),
                self.key.is_some(),
                self.username.is_some(),
                self.temporary_key.is_some(),
            ]
            .into_iter()
            .filter(|present| *present)
            .count()
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        if self.sources.len() > MAX_SOURCES
            || self.sources.iter().any(|source| !source.valid())
            || self.interval.is_some_and(|value| !valid_interval(value))
            || [&self.credential, &self.key]
                .into_iter()
                .flatten()
                .any(|id| !valid_id(id))
            || self
                .username
                .as_deref()
                .is_some_and(|value| !crate::ssh_dialog::valid_username(value))
        {
            return Err(io::Error::other("Invalid Java Teamwork proposals"));
        }
        Ok(())
    }
}

impl SourceProposal {
    fn valid(&self) -> bool {
        self.id.as_deref().is_none_or(valid_id)
            && valid_interval(self.interval)
            && !self.location.is_empty()
            && self.location.len() <= MAX_LOCATION_BYTES
            && !crate::ssh_dialog::unsafe_text(&self.location)
            && (self.kind != SourceKind::Git || GitUrl::parse(&self.location).is_ok())
    }

    fn read(node: Node<'_, '_>) -> Option<Self> {
        if node.tag_name().namespace().is_some()
            || !node.has_tag_name("source")
            || node.attributes().next().is_some()
        {
            return None;
        }
        let source = Self {
            id: preference_text(node, "id").ok()?.map(str::to_owned),
            kind: match preference_text(node, "type").ok()?.unwrap_or("GIT") {
                "GIT" => SourceKind::Git,
                "SHARED_FILE" => SourceKind::File,
                _ => return None,
            },
            location: preference_text(node, "location").ok()??.to_owned(),
            interval: match preference_text(node, "checkIntervalMinutes").ok()? {
                Some(value) => value.parse().ok()?,
                None => 15,
            },
            was_enabled: match preference_text(node, "enabled").ok()? {
                Some(value) => java_flag(value)?,
                None => true,
            },
        };
        source.valid().then_some(source)
    }
}

fn valid_interval(value: u16) -> bool {
    (1..=MAX_CHECK_INTERVAL_MINUTES).contains(&value)
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.chars().any(char::is_whitespace)
        && !crate::ssh_dialog::unsafe_text(value)
}

pub(super) fn read(root: Node<'_, '_>, omitted: &mut usize) -> Option<Proposals> {
    let mut sources = Vec::new();
    let mut identities = BTreeSet::new();
    let mut locations = BTreeSet::new();
    match preference_node(root, "teamworkSources") {
        Ok(Some(list)) => {
            for element in list.children().filter(Node::is_element) {
                let Some(source) = SourceProposal::read(element) else {
                    *omitted += 1;
                    continue;
                };
                let location = (source.kind as u8, source.location.clone());
                if sources.len() == MAX_SOURCES
                    || locations.contains(&location)
                    || source.id.as_ref().is_some_and(|id| identities.contains(id))
                {
                    *omitted += 1;
                    continue;
                }
                locations.insert(location);
                identities.extend(source.id.iter().cloned());
                sources.push(source);
            }
        }
        Ok(None) => {}
        Err(()) => *omitted += 1,
    }
    let reference = |value: &str| valid_id(value).then(|| value.to_owned());
    let proposals = Proposals {
        sources,
        interval: preference(
            root,
            "teamworkDefaultCheckIntervalMinutes",
            omitted,
            |value| value.parse().ok().filter(|value| valid_interval(*value)),
        ),
        credential: preference(root, "teamworkDefaultCredentialId", omitted, reference),
        key: preference(root, "teamworkDefaultSshKeyId", omitted, reference),
        username: preference(root, "teamworkDefaultUsername", omitted, |value| {
            crate::ssh_dialog::valid_username(value).then(|| value.to_owned())
        }),
        temporary_key: preference(root, "teamworkUseTemporaryKey", omitted, java_flag),
    };
    (proposals.count() != 0).then_some(proposals)
}
