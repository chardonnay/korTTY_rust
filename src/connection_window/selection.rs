use std::collections::{BTreeMap, BTreeSet};

use iced::keyboard::Modifiers;

use crate::connection_store::Connection;

use super::{ConnectionWindow, Scope};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RowKey {
    Connection(u64),
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
            Self::Group { path, .. } => RowKey::Group((*path).to_owned()),
        }
    }
}

impl ConnectionWindow {
    pub(super) fn visual_rows<'a>(
        &'a self,
        connections: Vec<&'a Connection>,
    ) -> Vec<VisualRow<'a>> {
        let root = match &self.scope {
            Scope::All => None,
            Scope::Group(path) => Some(path.as_str()),
            _ => {
                return connections
                    .into_iter()
                    .map(|connection| VisualRow::Connection(connection, 0))
                    .collect();
            }
        };
        let mut tree = Tree::default();
        for connection in connections {
            let group = self.store.group_of(connection.id);
            tree.connections.entry(group).or_default().push(connection);
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
        self.visual_rows(self.visible_connections())
            .iter()
            .map(VisualRow::key)
            .collect()
    }
}

#[derive(Default)]
struct Tree<'a> {
    groups: BTreeMap<Option<&'a str>, Vec<&'a str>>,
    connections: BTreeMap<Option<&'a str>, Vec<&'a Connection>>,
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
        if let Some(connections) = self.connections.get(&parent) {
            rows.extend(
                connections
                    .iter()
                    .map(|&connection| VisualRow::Connection(connection, indent)),
            );
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
