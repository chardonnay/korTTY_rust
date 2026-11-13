//! Profile-local metadata keyed by the shared source and its external connection ID.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use serde::de::Error as _;
use serde::ser::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::super::import::valid_teamwork_id;
use super::super::valid_text;
use super::{Entry, GitUrl, Identity, Origin, SourceKind, invalid};
use crate::ssh_dialog::normalize_host;

const MAX_FAVORITES: usize = 512;
const MAX_HIDDEN: usize = 512;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(in crate::connection_store) struct Favorites(BTreeSet<Identity>);

impl Favorites {
    pub(in crate::connection_store) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(in crate::connection_store) fn contains(&self, id: &Identity) -> bool {
        self.0.contains(id)
    }

    pub(in crate::connection_store) fn set(
        &mut self,
        ids: &[Identity],
        favorite: bool,
    ) -> io::Result<()> {
        if ids.len() > MAX_FAVORITES {
            return Err(invalid("Select at most 512 Teamwork favorites at a time."));
        }
        if favorite {
            self.0.extend(ids.iter().cloned());
        } else {
            for id in ids {
                self.0.remove(id);
            }
        }
        if self.0.len() > MAX_FAVORITES {
            return Err(invalid("The Teamwork favorites limit is 512."));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for Favorites {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let ids = Vec::<Identity>::deserialize(deserializer)?;
        let count = ids.len();
        if count > MAX_FAVORITES {
            return Err(D::Error::custom("The Teamwork favorites limit is 512."));
        }
        let ids: BTreeSet<_> = ids.into_iter().collect();
        if ids.len() != count {
            return Err(D::Error::custom("Duplicate Teamwork favorite."));
        }
        Ok(Self(ids))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HiddenEntry {
    identity: Identity,
    name: String,
    host: String,
}

impl HiddenEntry {
    pub(crate) fn identity(&self) -> &Identity {
        &self.identity
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn host(&self) -> &str {
        &self.host
    }

    fn validate(&self) -> io::Result<()> {
        if !valid_text(&self.name, 160, false)
            || normalize_host(&self.host).as_deref() != Ok(self.host.as_str())
        {
            return Err(invalid("Invalid hidden Teamwork connection label."));
        }
        Ok(())
    }
}

impl From<&Entry> for HiddenEntry {
    fn from(entry: &Entry) -> Self {
        Self {
            identity: entry.identity.clone(),
            name: entry.connection.name.clone(),
            host: entry.connection.login.endpoint.host.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(in crate::connection_store) struct HiddenEntries(BTreeMap<Identity, HiddenEntry>);

impl HiddenEntries {
    pub(in crate::connection_store) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(in crate::connection_store) fn contains(&self, id: &Identity) -> bool {
        self.0.contains_key(id)
    }

    pub(in crate::connection_store) fn iter(&self) -> impl Iterator<Item = &HiddenEntry> {
        self.0.values()
    }

    pub(in crate::connection_store) fn hide(&mut self, entries: &[HiddenEntry]) -> io::Result<()> {
        if entries.is_empty() || entries.len() > MAX_HIDDEN {
            return Err(invalid(
                "Select between 1 and 512 Teamwork connections to hide.",
            ));
        }
        self.0.extend(checked_hidden(entries.iter().cloned())?);
        if self.0.len() > MAX_HIDDEN {
            return Err(invalid("The hidden Teamwork connection limit is 512."));
        }
        Ok(())
    }

    pub(in crate::connection_store) fn restore(&mut self, identity: &Identity) -> io::Result<()> {
        if self.0.remove(identity).is_none() {
            return Err(invalid(
                "The hidden Teamwork connection is no longer present.",
            ));
        }
        Ok(())
    }
}

fn checked_hidden(
    entries: impl IntoIterator<Item = HiddenEntry>,
) -> io::Result<BTreeMap<Identity, HiddenEntry>> {
    let mut checked = BTreeMap::new();
    for entry in entries {
        entry.validate()?;
        if checked.insert(entry.identity.clone(), entry).is_some() {
            return Err(invalid("Duplicate hidden Teamwork connection."));
        }
    }
    Ok(checked)
}

impl Serialize for HiddenEntries {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

impl<'de> Deserialize<'de> for HiddenEntries {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let entries = Vec::<HiddenEntry>::deserialize(deserializer)?;
        if entries.len() > MAX_HIDDEN {
            return Err(D::Error::custom(
                "The hidden Teamwork connection limit is 512.",
            ));
        }
        checked_hidden(entries).map(Self).map_err(D::Error::custom)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredIdentity<T> {
    kind: SourceKind,
    location: T,
    id: T,
}

impl Serialize for Identity {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let (kind, location) = match self.source() {
            Origin::File(path) => (
                SourceKind::File,
                path.to_str()
                    .ok_or_else(|| S::Error::custom("Invalid Teamwork source path."))?,
            ),
            Origin::Git(url) => (SourceKind::Git, url.as_str()),
        };
        StoredIdentity {
            kind,
            location,
            id: self.id.as_ref(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Identity {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let record = StoredIdentity::<String>::deserialize(deserializer)?;
        if !valid_teamwork_id(&record.id) {
            return Err(D::Error::custom("Invalid Teamwork connection ID."));
        }
        let source = match record.kind {
            SourceKind::File => Origin::from_file(PathBuf::from(record.location)),
            SourceKind::Git => GitUrl::parse(&record.location).map(Origin::Git),
        }
        .map_err(D::Error::custom)?;
        Ok(Self {
            source: Arc::new(source),
            id: Arc::from(record.id),
        })
    }
}
