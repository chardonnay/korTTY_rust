use std::collections::{BTreeMap, BTreeSet};
use std::io;

use serde::{Deserialize, Serialize};

use super::{Connection, TabColor, invalid, valid_text};

pub(super) const RECENT_LIMIT: usize = 15;
const MAX_GROUPS: usize = 512;
const MAX_GROUP_DEPTH: usize = 8;
pub(crate) const MAX_GROUP_PATH_BYTES: usize = 512;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Library {
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub groups: BTreeSet<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub group_colors: BTreeMap<String, TabColor>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub membership: BTreeMap<u64, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub os_badges: BTreeMap<u64, String>,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub favorites: BTreeSet<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recent: Vec<u64>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub used_at: BTreeMap<u64, u64>,
}

impl Library {
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
            && self.group_colors.is_empty()
            && self.membership.is_empty()
            && self.os_badges.is_empty()
            && self.favorites.is_empty()
            && self.recent.is_empty()
            && self.used_at.is_empty()
    }

    pub fn validate(&self, connections: &BTreeSet<u64>) -> io::Result<()> {
        if self.groups.len() > MAX_GROUPS
            || self.groups.iter().any(|path| {
                !valid_path(path)
                    || path
                        .rsplit_once('/')
                        .is_some_and(|(parent, _)| !self.groups.contains(parent))
            })
            || self
                .group_colors
                .keys()
                .any(|path| !self.groups.contains(path))
            || self
                .membership
                .iter()
                .any(|(id, path)| !connections.contains(id) || !self.groups.contains(path))
            || self.os_badges.iter().any(|(id, badge)| {
                !connections.contains(id)
                    || badge.is_empty()
                    || badge.len() > 96
                    || !badge.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || matches!(byte, b'-' | b':')
                    })
            })
            || !self.favorites.is_subset(connections)
            || self.recent.len() > RECENT_LIMIT
            || !self.recent.iter().all(|id| connections.contains(id))
            || self.recent.iter().collect::<BTreeSet<_>>().len() != self.recent.len()
            || self
                .used_at
                .iter()
                .any(|(id, time)| *time == 0 || !self.recent.contains(id))
        {
            return Err(invalid("invalid saved connection library"));
        }
        Ok(())
    }

    pub fn require_group(&self, group: Option<&str>) -> io::Result<()> {
        if group.is_some_and(|group| !self.groups.contains(group)) {
            return Err(invalid("saved connection folder no longer exists"));
        }
        Ok(())
    }

    pub fn create_group(&mut self, parent: Option<&str>, name: &str) -> io::Result<String> {
        self.require_group(parent)?;
        let path = new_path(parent, name)?;
        if !self.groups.insert(path.clone()) {
            return Err(invalid("a folder with that name already exists here"));
        }
        Ok(path)
    }

    pub fn rename_group(&mut self, path: &str, name: &str) -> io::Result<String> {
        self.require_group(Some(path))?;
        let parent = path.rsplit_once('/').map(|(parent, _)| parent);
        let new_path = new_path(parent, name)?;
        if new_path == path {
            return Ok(new_path);
        }
        if self.groups.contains(&new_path) {
            return Err(invalid("a folder with that name already exists here"));
        }
        self.groups = self
            .groups
            .iter()
            .map(|group| match group_suffix(group, path) {
                Some(suffix) => format!("{new_path}{suffix}"),
                None => group.clone(),
            })
            .collect();
        for group in self.membership.values_mut() {
            if let Some(suffix) = group_suffix(group, path) {
                *group = format!("{new_path}{suffix}");
            }
        }
        self.group_colors = std::mem::take(&mut self.group_colors)
            .into_iter()
            .map(|(group, color)| {
                let group = match group_suffix(&group, path) {
                    Some(suffix) => format!("{new_path}{suffix}"),
                    None => group,
                };
                (group, color)
            })
            .collect();
        Ok(new_path)
    }

    pub fn remove_group(&mut self, path: &str) -> io::Result<BTreeSet<u64>> {
        self.require_group(Some(path))?;
        let ids: BTreeSet<_> = self
            .membership
            .iter()
            .filter(|(_, group)| group_suffix(group, path).is_some())
            .map(|(id, _)| *id)
            .collect();
        self.groups
            .retain(|group| group_suffix(group, path).is_none());
        self.group_colors
            .retain(|group, _| group_suffix(group, path).is_none());
        self.remove_connections(&ids);
        Ok(ids)
    }

    pub fn remove_connections(&mut self, ids: &BTreeSet<u64>) {
        self.membership.retain(|id, _| !ids.contains(id));
        self.os_badges.retain(|id, _| !ids.contains(id));
        self.favorites.retain(|id| !ids.contains(id));
        self.recent.retain(|id| !ids.contains(id));
        self.used_at.retain(|id, _| !ids.contains(id));
    }

    pub(super) fn set_os_badge(&mut self, id: u64, badge: Option<&str>) {
        if let Some(badge) = badge {
            self.os_badges.insert(id, badge.to_owned());
        } else {
            self.os_badges.remove(&id);
        }
    }

    pub(super) fn record_use(&mut self, id: u64, time: u64) -> bool {
        if self.used_at.get(&id).is_some_and(|&current| current > time) {
            return false;
        }
        self.recent.retain(|&entry| entry != id);
        let position = self
            .recent
            .iter()
            .position(|entry| self.used_at.get(entry).copied().unwrap_or(0) <= time)
            .unwrap_or(self.recent.len());
        self.recent.insert(position, id);
        self.recent.truncate(RECENT_LIMIT);
        self.used_at.insert(id, time);
        self.used_at.retain(|id, _| self.recent.contains(id));
        true
    }
}

pub(crate) fn group_suffix<'a>(path: &'a str, group: &str) -> Option<&'a str> {
    path.strip_prefix(group)
        .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))
}

fn new_path(parent: Option<&str>, name: &str) -> io::Result<String> {
    let name = name.trim();
    if !valid_segment(name) {
        return Err(invalid("enter an ordinary folder name of up to 96 bytes"));
    }
    let path = parent.map_or_else(|| name.to_owned(), |parent| format!("{parent}/{name}"));
    if !valid_path(&path) {
        return Err(invalid(
            "a connection folder may be at most eight levels deep",
        ));
    }
    Ok(path)
}

pub(super) fn valid_path(path: &str) -> bool {
    path.len() <= MAX_GROUP_PATH_BYTES
        && path.split('/').count() <= MAX_GROUP_DEPTH
        && path.split('/').all(valid_segment)
}

fn valid_segment(segment: &str) -> bool {
    valid_text(segment, 96, false)
        && segment == segment.trim()
        && !segment.contains('/')
        && !matches!(segment, "." | "..")
}

pub(crate) enum Search {
    Substring(String),
    Glob(String),
}

impl Search {
    pub(crate) fn new(query: &str) -> Self {
        let query = query.trim();
        if query.contains('*') {
            // Java's CASE_INSENSITIVE without UNICODE_CASE is ASCII-only.
            Self::Glob(query.to_ascii_lowercase())
        } else {
            Self::Substring(query.to_lowercase())
        }
    }

    pub(crate) fn manager(&self, connection: &Connection) -> bool {
        [
            &connection.name,
            &connection.login.endpoint.host,
            &connection.tag,
        ]
        .into_iter()
        .any(|value| self.matches(value))
    }

    pub(super) fn quick(&self, connection: &Connection) -> bool {
        self.manager(connection)
            || self.matches(&connection.login.endpoint.username)
            || self.matches(&format!(
                "{}@{}",
                connection.login.endpoint.username, connection.login.endpoint.host,
            ))
    }

    fn matches(&self, value: &str) -> bool {
        match self {
            Self::Substring(query) => query.is_empty() || value.to_lowercase().contains(query),
            Self::Glob(query) => {
                let mut parts = query.split('*');
                let prefix = parts.next().expect("glob has a prefix");
                let suffix = parts.next_back().expect("glob has a suffix");
                let value = value.to_ascii_lowercase();
                let Some(middle) = value
                    .strip_prefix(prefix)
                    .and_then(|tail| tail.strip_suffix(suffix))
                else {
                    return false;
                };
                parts
                    .try_fold(middle, |tail, part| {
                        tail.find(part).map(|index| &tail[index + part.len()..])
                    })
                    .is_some()
            }
        }
    }
}
