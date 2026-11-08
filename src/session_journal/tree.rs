//! Pure Java journal-manager grouping and metadata filters over the bounded catalog.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::str::FromStr;

use jiff::Timestamp;

use super::{Catalog, MAX_CATALOG_ROWS, Summary, invalid, metadata::java_whitespace};

/// OffsetDateTime orders equal instants by their original local date/time, hence by offset.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct DateOrder {
    instant: Timestamp,
    offset_seconds: i32,
}

impl DateOrder {
    pub(super) fn timestamp(self) -> Timestamp {
        self.instant
    }
    pub(super) fn offset_seconds(self) -> i32 {
        self.offset_seconds
    }

    fn from_parts(instant: Option<Timestamp>, offset_seconds: i32) -> Option<Self> {
        instant.map(|instant| Self {
            instant,
            offset_seconds,
        })
    }
}

impl FromStr for DateOrder {
    type Err = io::Error;

    fn from_str(value: &str) -> io::Result<Self> {
        let instant = value
            .parse()
            .map_err(|_| invalid("Invalid session journal date"))?;
        let pieces = jiff::fmt::temporal::Pieces::parse(value)
            .map_err(|_| invalid("Invalid session journal date"))?;
        let offset = pieces
            .to_numeric_offset()
            .ok_or_else(|| invalid("Session journal date requires an offset"))?;
        Ok(Self {
            instant,
            offset_seconds: offset.seconds(),
        })
    }
}

impl Summary {
    pub(super) fn started_order(&self) -> Option<DateOrder> {
        DateOrder::from_parts(self.started_at, self.started_offset_seconds)
    }

    fn run_order(&self) -> Option<DateOrder> {
        DateOrder::from_parts(self.run_started_at, self.run_started_offset_seconds)
            .or_else(|| self.started_order())
    }

    fn expiry_order(&self) -> Option<DateOrder> {
        DateOrder::from_parts(self.expires_at, self.expires_offset_seconds)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum SourceKind {
    #[default]
    Interactive,
    Job,
    Swarm,
}

impl SourceKind {
    pub(super) fn parse(value: &str) -> Self {
        match value {
            "JOB" => Self::Job,
            "SWARM" => Self::Swarm,
            _ => Self::Interactive,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum RunStatus {
    Success,
    Cancelled,
    Blocked,
    Failed,
}

impl RunStatus {
    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "SUCCESS" => Some(Self::Success),
            "CANCELLED" => Some(Self::Cancelled),
            "BLOCKED" => Some(Self::Blocked),
            "FAILED" => Some(Self::Failed),
            _ => None,
        }
    }

    pub(crate) fn is_failure(self) -> bool {
        self != Self::Success
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Filter {
    #[default]
    All,
    Interactive,
    Jobs,
    Swarm,
    Failed,
    Pinned,
}

impl Filter {
    pub(crate) const ALL: [Self; 6] = [
        Self::All,
        Self::Interactive,
        Self::Jobs,
        Self::Swarm,
        Self::Failed,
        Self::Pinned,
    ];

    pub(crate) fn matches(self, journal: &Summary) -> bool {
        match self {
            Self::All => true,
            Self::Interactive => journal.source_kind == SourceKind::Interactive,
            Self::Jobs => journal.source_kind == SourceKind::Job,
            Self::Swarm => journal.source_kind == SourceKind::Swarm,
            Self::Failed => journal.run_status.is_some_and(RunStatus::is_failure),
            Self::Pinned => journal.pinned,
        }
    }
}

impl fmt::Display for Filter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::All => "All journals",
            Self::Interactive => "Interactive",
            Self::Jobs => "JobScheduler",
            Self::Swarm => "AI Swarm",
            Self::Failed => "Failed runs",
            Self::Pinned => "Pinned",
        })
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct GroupKey {
    pub kind: SourceKind,
    pub source_id: Option<String>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum RunKey {
    Id(String),
    Directory(PathBuf),
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum NodeKey {
    Journal(PathBuf),
    Group(GroupKey),
    Run(GroupKey, RunKey),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Journal,
    Group,
    Run,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub call_count: u64,
    pub cost: f64,
    pub currency: Option<String>,
    pub local: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub key: NodeKey,
    pub children: Vec<Node>,
    /// Original catalog indices in the input order, including every descendant.
    pub journals: Vec<usize>,
    pub source_kind: SourceKind,
    pub source_name: Option<String>,
    pub automation_action: Option<String>,
    pub run_count: usize,
    pub started_at: Option<Timestamp>,
    started_order: Option<DateOrder>,
    pub next_expiry: Option<Timestamp>,
    expiry_order: Option<DateOrder>,
    pub worst_status: Option<RunStatus>,
    pub any_pinned: bool,
    pub all_pinned: bool,
    pub servers: Vec<String>,
    pub storage_bytes: u64,
    pub log_entry_count: u64,
    pub duplicate_run_count: u64,
    pub usage: Usage,
}

impl Node {
    fn new(key: NodeKey, source_kind: SourceKind) -> Self {
        Self {
            key,
            children: Vec::new(),
            journals: Vec::new(),
            source_kind,
            source_name: None,
            automation_action: None,
            run_count: 0,
            started_at: None,
            started_order: None,
            next_expiry: None,
            expiry_order: None,
            worst_status: None,
            any_pinned: false,
            all_pinned: false,
            servers: Vec::new(),
            storage_bytes: 0,
            log_entry_count: 0,
            duplicate_run_count: 0,
            usage: Usage::default(),
        }
    }

    pub(crate) fn kind(&self) -> Kind {
        match self.key {
            NodeKey::Journal(_) => Kind::Journal,
            NodeKey::Group(_) => Kind::Group,
            NodeKey::Run(_, _) => Kind::Run,
        }
    }

    pub(crate) fn journal_index(&self) -> Option<usize> {
        match self.key {
            NodeKey::Journal(_) => self.journals.first().copied(),
            _ => None,
        }
    }

    pub(crate) fn journal_count(&self) -> usize {
        self.journals.len()
    }

    pub(crate) fn is_scheduled_swarm(&self) -> bool {
        self.source_kind == SourceKind::Job && self.automation_action.as_deref() == Some("AI_SWARM")
    }

    fn aggregate(&mut self, catalog: &Catalog) {
        let mut named: Option<&Summary> = None;
        let mut newest: Option<&Summary> = None;
        self.all_pinned = !self.journals.is_empty();
        self.usage.local = !self.journals.is_empty();
        for &index in &self.journals {
            let journal = &catalog.journals[index];
            if newest.is_none_or(|current| journal.started_order() > current.started_order()) {
                newest = Some(journal);
            }
            if journal
                .source_name
                .as_ref()
                .is_some_and(|name| !name.chars().all(java_whitespace))
                && named.is_none_or(|current| journal.started_order() > current.started_order())
            {
                named = Some(journal);
            }
            self.worst_status = self.worst_status.max(journal.run_status);
            self.any_pinned |= journal.pinned;
            self.all_pinned &= journal.pinned;
            self.usage.local &= journal.usage.local;
            if !journal.pinned
                && let Some(expires) = journal.expiry_order()
            {
                self.expiry_order = Some(
                    self.expiry_order
                        .map_or(expires, |current| current.min(expires)),
                );
                self.next_expiry = self.expiry_order.map(DateOrder::timestamp);
            }
            if journal.host_present && !self.servers.contains(&journal.host) {
                self.servers.push(journal.host.clone());
            }
            self.storage_bytes = self.storage_bytes.saturating_add(journal.storage_bytes);
            self.log_entry_count = self.log_entry_count.saturating_add(journal.log_entry_count);
            self.duplicate_run_count = self
                .duplicate_run_count
                .saturating_add(journal.duplicate_run_count);
            self.usage.prompt_tokens = self
                .usage
                .prompt_tokens
                .saturating_add(journal.usage.prompt_tokens);
            self.usage.completion_tokens = self
                .usage
                .completion_tokens
                .saturating_add(journal.usage.completion_tokens);
            self.usage.total_tokens = self
                .usage
                .total_tokens
                .saturating_add(journal.usage.total_tokens);
            self.usage.call_count = self
                .usage
                .call_count
                .saturating_add(journal.usage.call_count);
            self.usage.cost = (self.usage.cost + journal.usage.cost).clamp(-f64::MAX, f64::MAX);
            if self.usage.currency.is_none() {
                self.usage.currency.clone_from(&journal.usage.currency);
            }
        }
        match self.kind() {
            Kind::Journal => {
                if let Some(journal) = newest {
                    self.started_order = journal.started_order();
                    self.source_name.clone_from(&journal.source_name);
                    self.automation_action
                        .clone_from(&journal.automation_action);
                }
            }
            Kind::Group => {
                self.started_order = newest.and_then(Summary::started_order);
                self.source_name = named.and_then(|journal| journal.source_name.clone());
                self.automation_action =
                    newest.and_then(|journal| journal.automation_action.clone());
            }
            Kind::Run => {
                self.started_order = self
                    .journals
                    .iter()
                    .filter_map(|&index| catalog.journals[index].run_order())
                    .min();
                self.source_name = named.and_then(|journal| journal.source_name.clone());
                self.automation_action = self
                    .journals
                    .first()
                    .and_then(|&index| catalog.journals[index].automation_action.clone());
                self.run_count = 1;
            }
        }
        self.started_at = self.started_order.map(DateOrder::timestamp);
    }
}

/// Uses metadata only. Filtered journals never contribute to aggregates or group names.
pub(crate) fn build(catalog: &Catalog, filter: Filter, query: &str) -> Vec<Node> {
    let query = prepare_query(query);
    build_matching(catalog, |_, journal| {
        matches_query(journal, filter, &query, false)
    })
}

pub(crate) fn build_matching(
    catalog: &Catalog,
    matches: impl Fn(usize, &Summary) -> bool,
) -> Vec<Node> {
    let mut roots = Vec::new();
    let mut groups: HashMap<GroupKey, usize> = HashMap::new();
    let mut runs: HashMap<GroupKey, HashMap<RunKey, usize>> = HashMap::new();
    for (index, journal) in catalog.journals.iter().take(MAX_CATALOG_ROWS).enumerate() {
        if !matches(index, journal) {
            continue;
        }
        let mut leaf = Node::new(
            NodeKey::Journal(journal.directory.clone()),
            journal.source_kind,
        );
        leaf.journals.push(index);
        leaf.aggregate(catalog);
        if journal.source_kind == SourceKind::Interactive {
            roots.push(leaf);
            continue;
        }
        let group_key = GroupKey {
            kind: journal.source_kind,
            source_id: journal.source_id.clone(),
        };
        let group_index = *groups.entry(group_key.clone()).or_insert_with(|| {
            roots.push(Node::new(
                NodeKey::Group(group_key.clone()),
                journal.source_kind,
            ));
            roots.len() - 1
        });
        let group = &mut roots[group_index];
        group.journals.push(index);
        let run_key = journal
            .run_id
            .clone()
            .map_or_else(|| RunKey::Directory(journal.directory.clone()), RunKey::Id);
        let run_index = *runs
            .entry(group_key.clone())
            .or_default()
            .entry(run_key.clone())
            .or_insert_with(|| {
                group.children.push(Node::new(
                    NodeKey::Run(group_key.clone(), run_key),
                    journal.source_kind,
                ));
                group.children.len() - 1
            });
        let run = &mut group.children[run_index];
        run.journals.push(index);
        run.children.push(leaf);
    }
    for root in &mut roots {
        if root.kind() != Kind::Group {
            continue;
        }
        root.run_count = root.children.len();
        root.aggregate(catalog);
        for run in &mut root.children {
            run.aggregate(catalog);
            run.children.sort_by(|left, right| {
                catalog.journals[left.journals[0]]
                    .host
                    .encode_utf16()
                    .cmp(catalog.journals[right.journals[0]].host.encode_utf16())
            });
        }
        root.children.sort_by_key(|run| Reverse(run.started_order));
        for run in &mut root.children {
            if run.journals.len() == 1 {
                *run = run.children.remove(0);
            }
        }
    }
    roots.sort_by_key(|node| Reverse(node.started_order));
    roots
}

pub(crate) fn matches_query(
    journal: &Summary,
    filter: Filter,
    normalized_query: &str,
    content_match: bool,
) -> bool {
    filter.matches(journal) && (content_match || matches_metadata(journal, normalized_query))
}

/// Normalizes the metadata query with Java's strip and locale-independent lowercase rules.
pub(crate) fn prepare_query(query: &str) -> String {
    query.trim_matches(java_whitespace).to_lowercase()
}

/// Matches a query already normalized by `prepare_query`.
pub(crate) fn matches_metadata(journal: &Summary, query: &str) -> bool {
    query.is_empty()
        || [
            &journal.title,
            &journal.connection_name,
            &journal.host,
            &journal.username,
            &journal.description,
        ]
        .into_iter()
        .chain(journal.source_name.iter())
        .chain(journal.keywords.iter())
        .any(|value| value.to_lowercase().contains(query))
}

/// Expands selected groups/runs and deduplicates journals by their loaded directory path.
pub(crate) fn dedup_journals(
    catalog: &Catalog,
    nodes: &[Node],
    selection: &HashSet<NodeKey>,
) -> Vec<usize> {
    fn collect(
        catalog: &Catalog,
        nodes: &[Node],
        selection: &HashSet<NodeKey>,
        seen: &mut HashSet<PathBuf>,
        output: &mut Vec<usize>,
    ) {
        for node in nodes {
            if selection.contains(&node.key) {
                for &index in &node.journals {
                    if let Some(journal) = catalog.journals.get(index)
                        && seen.insert(journal.directory.clone())
                    {
                        output.push(index);
                    }
                }
            }
            collect(catalog, &node.children, selection, seen, output);
        }
    }
    let mut output = Vec::new();
    collect(catalog, nodes, selection, &mut HashSet::new(), &mut output);
    output
}

#[cfg(test)]
mod tests;
