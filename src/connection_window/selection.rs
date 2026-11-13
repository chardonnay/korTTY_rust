use std::collections::{BTreeMap, BTreeSet};

use iced::keyboard::Modifiers;

use crate::connection_store::teamwork;
use crate::connection_store::{Connection, LocalProfile, RECENT_LIMIT, Search, SerialProfile};

use super::{ConnectionWindow, Scope};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RowKey {
    Connection(u64),
    Shared(teamwork::Identity),
    Serial(u64),
    Local(u64),
    Group(String),
    Empty,
}

#[derive(Default)]
pub(super) struct Selection {
    rows: BTreeSet<RowKey>,
    anchor: Option<RowKey>,
}

impl Selection {
    pub(super) fn contains(&self, row: &RowKey) -> bool {
        self.rows.contains(row)
    }

    pub(super) fn connections(&self) -> impl Iterator<Item = u64> + '_ {
        self.rows.iter().filter_map(|row| match row {
            RowKey::Connection(id) => Some(*id),
            _ => None,
        })
    }

    pub(super) fn shared(&self) -> impl Iterator<Item = &teamwork::Identity> {
        self.rows.iter().filter_map(|row| match row {
            RowKey::Shared(identity) => Some(identity),
            _ => None,
        })
    }

    pub(super) fn serials(&self) -> impl Iterator<Item = u64> + '_ {
        self.rows.iter().filter_map(|row| match row {
            RowKey::Serial(id) => Some(*id),
            _ => None,
        })
    }

    pub(super) fn single_connection(&self) -> Option<u64> {
        if self.rows.len() != 1 {
            return None;
        }
        let mut connections = self.connections();
        let id = connections.next()?;
        connections.next().is_none().then_some(id)
    }

    pub(super) fn single_group(&self) -> Option<&str> {
        if self.rows.len() == 1
            && let Some(RowKey::Group(path)) = self.rows.first()
        {
            return Some(path);
        }
        None
    }

    pub(super) fn single_shared(&self) -> Option<&teamwork::Identity> {
        if self.rows.len() == 1
            && let Some(RowKey::Shared(identity)) = self.rows.first()
        {
            return Some(identity);
        }
        None
    }

    pub(super) fn single_serial(&self) -> Option<u64> {
        if self.rows.len() == 1
            && let Some(RowKey::Serial(id)) = self.rows.first()
        {
            return Some(*id);
        }
        None
    }

    pub(super) fn single_local(&self) -> Option<u64> {
        if self.rows.len() == 1
            && let Some(RowKey::Local(id)) = self.rows.first()
        {
            return Some(*id);
        }
        None
    }

    pub(super) fn replace(&mut self, row: Option<RowKey>) {
        self.rows.clear();
        self.anchor = row.clone();
        self.rows.extend(row);
    }

    pub(super) fn press(&mut self, row: RowKey, visible: &[RowKey], modifiers: Modifiers) {
        if modifiers.shift()
            && let Some(anchor) = self.anchor.as_ref()
            && let Some(start) = visible.iter().position(|item| item == anchor)
            && let Some(end) = visible.iter().position(|item| item == &row)
        {
            if !modifiers.command() {
                self.rows.clear();
            }
            self.rows
                .extend(visible[start.min(end)..=start.max(end)].iter().cloned());
        } else if modifiers.command() {
            if !self.rows.remove(&row) {
                self.rows.insert(row.clone());
            }
            self.anchor = Some(row);
        } else {
            self.replace(Some(row));
        }
    }

    pub(super) fn retain(&mut self, mut visible: impl FnMut(&RowKey) -> bool) {
        self.rows.retain(&mut visible);
        if self.anchor.as_ref().is_some_and(|row| !visible(row)) {
            self.anchor = self.rows.first().cloned();
        }
    }
}

/// A single projection drives both rendering and range selection.
pub(super) enum VisualRow<'a> {
    Connection(&'a Connection, usize),
    Shared(&'a teamwork::Entry),
    Serial(&'a SerialProfile, usize),
    Local(&'a LocalProfile),
    Group {
        path: &'a str,
        indent: usize,
        count: usize,
        collapsed: bool,
    },
}

impl VisualRow<'_> {
    pub(super) fn key(&self) -> RowKey {
        match self {
            Self::Connection(connection, _) => RowKey::Connection(connection.id),
            Self::Shared(entry) => RowKey::Shared(entry.identity.clone()),
            Self::Serial(profile, _) => RowKey::Serial(profile.id),
            Self::Local(profile) => RowKey::Local(profile.id),
            Self::Group { path, .. } => RowKey::Group((*path).to_owned()),
        }
    }
}

impl ConnectionWindow {
    fn tree_rows<'a>(&'a self, entries: Vec<VisualRow<'a>>) -> Vec<VisualRow<'a>> {
        let root = match &self.scope {
            Scope::All => None,
            Scope::Group(path) => Some(path.as_str()),
            _ => return entries,
        };
        let mut tree = Tree::default();
        for entry in entries {
            let id = match entry {
                VisualRow::Connection(profile, _) => profile.id,
                VisualRow::Serial(profile, _) => profile.id,
                _ => unreachable!("the folder tree contains only SSH and serial profiles"),
            };
            let group = self.store.group_of(id);
            tree.entries.entry(group).or_default().push(entry);
            let mut parent = group;
            while let Some(path) = parent {
                *tree.counts.entry(path).or_default() += 1;
                parent = parent_group(path);
            }
        }
        let searching = !self.query.trim().is_empty();
        for path in self.store.groups() {
            if !searching || tree.counts.contains_key(path) {
                tree.groups
                    .entry(parent_group(path))
                    .or_default()
                    .push(path);
            }
        }
        let mut rows = Vec::new();
        tree.append_rows(root, 0, &mut rows, |path| {
            !searching && self.collapsed.contains(path)
        });
        rows
    }

    pub(super) fn visible_row_keys(&self) -> Vec<RowKey> {
        self.visible_rows(self.visible_entries())
            .iter()
            .map(VisualRow::key)
            .collect()
    }

    pub(super) fn recent_entries(&self) -> Vec<VisualRow<'_>> {
        let local = self.store.recent_ids().filter_map(|id| {
            let row = if let Some(connection) = self.store.get(id) {
                VisualRow::Connection(connection, 0)
            } else if let Some(profile) = self.store.serial(id) {
                VisualRow::Serial(profile, 0)
            } else {
                VisualRow::Local(self.store.local(id)?)
            };
            Some((self.store.last_used_at(id), row))
        });
        let shared = self
            .shared_entries()
            .filter_map(|entry| Some((entry.last_used_at?, VisualRow::Shared(entry))));
        let mut recent = Vec::with_capacity(RECENT_LIMIT);
        // Keep the bounded combined selection before applying search. Equal
        // timestamps retain the original order, with local rows before shared.
        for (time, row) in local.chain(shared) {
            let index = recent.partition_point(|(used_at, _)| *used_at >= time);
            if index < RECENT_LIMIT {
                if recent.len() == RECENT_LIMIT {
                    recent.pop();
                }
                recent.insert(index, (time, row));
            }
        }
        recent.into_iter().map(|(_, row)| row).collect()
    }

    pub(super) fn visible_entries(&self) -> Vec<VisualRow<'_>> {
        if self.scope == Scope::Recent {
            let search = Search::new(&self.query);
            return self
                .recent_entries()
                .into_iter()
                .filter(|row| match row {
                    VisualRow::Connection(connection, _) => search.manager(connection),
                    VisualRow::Serial(profile, _) => search.serial(profile),
                    VisualRow::Local(profile) => search.local(profile),
                    VisualRow::Shared(entry) => search.manager(&entry.connection),
                    VisualRow::Group { .. } => false,
                })
                .collect();
        }
        self.visible_connections()
            .into_iter()
            .map(|connection| VisualRow::Connection(connection, 0))
            .chain(self.visible_shared().into_iter().map(VisualRow::Shared))
            .chain(
                self.visible_serial()
                    .into_iter()
                    .map(|profile| VisualRow::Serial(profile, 0)),
            )
            .chain(self.visible_local().into_iter().map(VisualRow::Local))
            .collect()
    }

    pub(super) fn visible_rows<'a>(&'a self, entries: Vec<VisualRow<'a>>) -> Vec<VisualRow<'a>> {
        if !matches!(self.scope, Scope::All | Scope::Group(_)) {
            return entries;
        }
        let mut profiles = Vec::new();
        let mut direct = Vec::new();
        let mut shared = Vec::new();
        for row in entries {
            match row {
                VisualRow::Connection(..) | VisualRow::Serial(..) => profiles.push(row),
                VisualRow::Local(_) => direct.push(row),
                VisualRow::Shared(_) => shared.push(row),
                VisualRow::Group { .. } => unreachable!("visible entries contain connections only"),
            }
        }
        let mut rows = self.tree_rows(profiles);
        rows.extend(direct);
        rows.extend(shared);
        rows
    }
}

#[derive(Default)]
struct Tree<'a> {
    groups: BTreeMap<Option<&'a str>, Vec<&'a str>>,
    entries: BTreeMap<Option<&'a str>, Vec<VisualRow<'a>>>,
    counts: BTreeMap<&'a str, usize>,
}

impl<'a> Tree<'a> {
    fn append_rows(
        &self,
        parent: Option<&'a str>,
        indent: usize,
        rows: &mut Vec<VisualRow<'a>>,
        collapsed: impl Fn(&str) -> bool + Copy,
    ) {
        if let Some(entries) = self.entries.get(&parent) {
            rows.extend(entries.iter().map(|entry| match *entry {
                VisualRow::Connection(profile, _) => VisualRow::Connection(profile, indent),
                VisualRow::Serial(profile, _) => VisualRow::Serial(profile, indent),
                _ => unreachable!("the folder tree contains only SSH and serial profiles"),
            }));
        }
        if let Some(groups) = self.groups.get(&parent) {
            for &path in groups {
                let closed = collapsed(path);
                rows.push(VisualRow::Group {
                    path,
                    indent,
                    count: self.counts.get(path).copied().unwrap_or(0),
                    collapsed: closed,
                });
                if !closed {
                    self.append_rows(Some(path), indent + 1, rows, collapsed);
                }
            }
        }
    }
}

fn parent_group(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(parent, _)| parent)
}
