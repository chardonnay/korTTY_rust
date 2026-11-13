//! A snapshot of the palette's sources and the session's recent choices.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use crate::connection_store::teamwork;
use crate::snippets::SnippetId;
use crate::workspace::chrome::Action;

const DEFAULT_LIMIT: usize = 60;
const RECENT_ON_EMPTY_QUERY: usize = 8;
const RECENT_CAPACITY: usize = 20;
const RECENT_BOOST_STEP: i32 = 4;
const TITLE_WEIGHT: i32 = 3;
const MAX_ROW_CHARS: usize = 120;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Kind {
    Action,
    Tab,
    Connection,
    Snippet,
}

impl Kind {
    const fn prefix(self) -> &'static str {
        match self {
            Self::Action => ">",
            Self::Tab => "#",
            Self::Connection => "@",
            Self::Snippet => "$",
        }
    }

    fn from_prefix(first: u8) -> Option<Self> {
        match first {
            b'>' => Some(Self::Action),
            b'#' => Some(Self::Tab),
            b'@' => Some(Self::Connection),
            b'$' => Some(Self::Snippet),
            _ => None,
        }
    }
}

/// Stable identities only. Display text, query text, and payloads stay out of the MRU.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Key {
    Action(Action),
    ReconnectTab,
    Monitor(super::super::tab_watch::Kind),
    Tab(u64),
    Connection(u64),
    Teamwork(teamwork::Identity),
    Snippet(SnippetId),
}

impl Key {
    pub(super) const fn kind(&self) -> Kind {
        match self {
            Self::Action(_) | Self::ReconnectTab | Self::Monitor(_) => Kind::Action,
            Self::Tab(_) => Kind::Tab,
            Self::Connection(_) | Self::Teamwork(_) => Kind::Connection,
            Self::Snippet(_) => Kind::Snippet,
        }
    }
}

#[derive(Clone)]
pub(super) struct Entry {
    pub(super) key: Key,
    pub(super) title: String,
    pub(super) detail: String,
    pub(super) search_detail: Option<String>,
    pub(super) shortcut: Option<String>,
    pub(super) enabled: bool,
    pub(super) disabled_reason: Option<String>,
    pub(super) checked: bool,
    pub(super) alternate: bool,
}

impl Entry {
    pub(super) fn new(key: Key, title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            key,
            title: title.into(),
            detail: detail.into(),
            search_detail: None,
            shortcut: None,
            enabled: true,
            disabled_reason: None,
            checked: false,
            alternate: false,
        }
    }

    fn cleaned(mut self) -> Self {
        self.title = clean(&self.title);
        self.detail = clean(&self.detail);
        self.search_detail = self.search_detail.map(|text| clean(&text));
        let clean_optional = |text: String| {
            let text = clean(&text);
            (!text.is_empty()).then_some(text)
        };
        self.shortcut = self.shortcut.and_then(clean_optional);
        self.disabled_reason = self.disabled_reason.and_then(clean_optional);
        self
    }
}

pub(super) struct Source {
    pub(super) kind: Kind,
    pub(super) entries: Vec<Entry>,
}

#[derive(Default)]
pub(super) struct Mru {
    items: Vec<Key>,
}

impl Mru {
    /// Record a dispatched choice. Tabs already have their own source order.
    pub(super) fn choose(&mut self, key: Key) {
        if key.kind() != Kind::Tab {
            self.items.retain(|previous| previous != &key);
            self.items.insert(0, key);
            self.items.truncate(RECENT_CAPACITY);
        }
    }

    fn boost(&self, key: &Key) -> i32 {
        self.items
            .iter()
            .position(|item| item == key)
            .map_or(0, |index| {
                (RECENT_CAPACITY - index) as i32 * RECENT_BOOST_STEP
            })
    }
}

/// Immutable until the palette closes. Reopening supplies fresh rows and keeps the shared MRU.
pub(super) struct Snapshot {
    sources: [bool; 4],
    rows: Vec<Row>,
    keys: HashMap<Key, usize>,
    browse: [Vec<usize>; 4],
    longest_field: usize,
}

impl Snapshot {
    pub(super) fn new(sources: impl IntoIterator<Item = Source>) -> Self {
        let mut snapshot = Self {
            sources: [false; 4],
            rows: Vec::new(),
            keys: HashMap::new(),
            browse: std::array::from_fn(|_| Vec::new()),
            longest_field: 0,
        };
        for source in sources {
            snapshot.sources[source.kind as usize] = true;
            for entry in source.entries {
                if entry.key.kind() != source.kind || snapshot.keys.contains_key(&entry.key) {
                    continue;
                }
                let index = snapshot.rows.len();
                snapshot.keys.insert(entry.key.clone(), index);
                snapshot.browse[source.kind as usize].push(index);
                let row = Row::new(entry.cleaned());
                snapshot.longest_field = snapshot.longest_field.max(row.path_and_title.text.len());
                snapshot.rows.push(row);
            }
        }
        snapshot.group_actions();
        snapshot
    }

    /// Look up a captured row by identity. The caller still checks current state on dispatch.
    pub(super) fn entry(&self, key: &Key) -> Option<&Entry> {
        self.keys.get(key).map(|&index| &self.rows[index].entry)
    }

    pub(super) fn scoped_query(&self, scope: Option<Kind>) -> &'static str {
        scope
            .filter(|&kind| self.sources[kind as usize])
            .map_or("", Kind::prefix)
    }

    pub(super) fn query(&self, input: &str, recent: &Mru) -> Vec<&Entry> {
        self.query_limit(input, recent, DEFAULT_LIMIT)
    }

    fn query_limit(&self, input: &str, recent: &Mru, limit: usize) -> Vec<&Entry> {
        let limit = if limit == 0 { usize::MAX } else { limit };
        let (scope, terms) = self.parse(input);
        if terms.is_empty() {
            return self.browse(scope, recent, limit);
        }
        let Some(query) = normalized_query(terms, self.longest_field) else {
            return Vec::new();
        };
        let mut matches = Vec::new();
        for (index, row) in self.rows.iter().enumerate() {
            let kind = row.entry.key.kind();
            if scope.is_some_and(|scope| scope != kind) {
                continue;
            }
            if let Some(score) = row.score(&query) {
                matches.push((
                    !row.entry.enabled,
                    Reverse(score + recent.boost(&row.entry.key)),
                    kind,
                    // Equal commands sort by title. Other kinds keep their source order.
                    (kind != Kind::Action).then_some(index),
                    &row.title.text,
                    index,
                ));
            }
        }
        if matches.len() > limit {
            matches.select_nth_unstable(limit);
            matches.truncate(limit);
        }
        matches.sort_unstable();
        matches
            .into_iter()
            .map(|(_, _, _, _, _, index)| &self.rows[index].entry)
            .collect()
    }

    fn parse<'a>(&self, input: &'a str) -> (Option<Kind>, &'a str) {
        let input = input.trim_matches(java_whitespace);
        let scope = input
            .as_bytes()
            .first()
            .copied()
            .and_then(Kind::from_prefix)
            .filter(|&kind| self.sources[kind as usize]);
        (
            scope,
            scope.map_or(input, |_| input[1..].trim_matches(java_whitespace)),
        )
    }

    fn browse(&self, scope: Option<Kind>, recent: &Mru, limit: usize) -> Vec<&Entry> {
        let mut entries = Vec::new();
        let mut listed = HashSet::new();
        let recent_limit = if scope.is_some() {
            limit
        } else {
            limit.min(RECENT_ON_EMPTY_QUERY)
        };
        for key in &recent.items {
            if entries.len() >= recent_limit {
                break;
            }
            let kind = key.kind();
            if scope.map_or(kind != Kind::Snippet, |scope| scope == kind)
                && let Some(entry) = self.entry(key)
            {
                listed.insert(&entry.key);
                entries.push(entry);
            }
        }
        let kinds = if scope.is_some() {
            scope.as_slice()
        } else {
            &[Kind::Tab, Kind::Action]
        };
        for &kind in kinds {
            for &index in &self.browse[kind as usize] {
                if entries.len() >= limit {
                    return entries;
                }
                let entry = &self.rows[index].entry;
                if listed.insert(&entry.key) {
                    entries.push(entry);
                }
            }
        }
        entries
    }

    fn group_actions(&mut self) {
        let actions = &mut self.browse[Kind::Action as usize];
        let mut groups = HashMap::new();
        let mut next_group = 0;
        for &index in actions.iter() {
            let detail = self.rows[index].entry.detail.as_str();
            groups.entry(detail).or_insert_with(|| {
                let group = next_group;
                next_group += 1;
                group
            });
        }
        actions.sort_by_key(|&index| groups[self.rows[index].entry.detail.as_str()]);
    }
}

struct Row {
    entry: Entry,
    title: Field,
    detail: Field,
    path_and_title: Field,
}

impl Row {
    fn new(entry: Entry) -> Self {
        let detail = entry.search_detail.as_deref().unwrap_or(&entry.detail);
        let path_and_title = if detail.is_empty() {
            Field::new(&entry.title)
        } else {
            Field::new(&format!("{detail} {}", entry.title))
        };
        Self {
            title: Field::new(&entry.title),
            detail: Field::new(detail),
            path_and_title,
            entry,
        }
    }

    fn score(&self, query: &[u16]) -> Option<i32> {
        [
            score(query, &self.title).map(|score| score * TITLE_WEIGHT),
            score(query, &self.path_and_title),
            score(query, &self.detail),
        ]
        .into_iter()
        .flatten()
        .max()
    }
}

struct Field {
    text: Vec<u16>,
    word_start: Vec<bool>,
}

impl Field {
    fn new(text: &str) -> Self {
        let text = folded(text);
        let mut word_start = Vec::with_capacity(text.len());
        let mut next_word_start = true;
        for &unit in &text {
            word_start.push(next_word_start);
            next_word_start = !is_letter_or_digit(unit);
        }
        Self { text, word_start }
    }
}

fn is_letter_or_digit(unit: u16) -> bool {
    if unit <= 0x7f {
        return (unit as u8).is_ascii_alphanumeric();
    }
    // Java tests a UTF-16 code unit here. A surrogate is never a letter or digit.
    let Some(character) = char::from_u32(u32::from(unit)) else {
        return false;
    };
    static CATEGORY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\A[\p{L}\p{Nd}]\z").expect("fixed Unicode category pattern"));
    CATEGORY.is_match(character.encode_utf8(&mut [0; 4]))
}

fn folded(text: &str) -> Vec<u16> {
    // The pinned matcher scores UTF-16 positions, including for non-BMP titles.
    text.to_lowercase().encode_utf16().collect()
}

fn normalized_query(text: &str, longest_field: usize) -> Option<Vec<u16>> {
    let mut query = String::new();
    for (index, ch) in text
        .chars()
        .filter(|&ch| !matches!(ch, '\t'..='\r' | ' '))
        .enumerate()
    {
        if index == longest_field {
            return None;
        }
        query.push(ch);
    }
    Some(folded(&query))
}

fn score(query: &[u16], field: &Field) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let text = &field.text;
    if query.len() > text.len() {
        return None;
    }
    let mut score = 0;
    let mut from = 0;
    for &wanted in query {
        let found = locate(field, wanted, from)?;
        score += 1;
        if found == from && from != 0 {
            score += 5;
        }
        if found == 0 {
            score += 8;
        } else if field.word_start[found] {
            score += 4;
        }
        from = found + 1;
    }
    if text.starts_with(query) {
        score += 10;
    } else if text.windows(query.len()).any(|window| window == query) {
        score += 6;
    }
    Some(score + (20 - (text.len() - query.len()) as i32 / 4).max(0))
}

fn locate(field: &Field, wanted: u16, from: usize) -> Option<usize> {
    let text = &field.text;
    if from != 0 && text.get(from) == Some(&wanted) {
        return Some(from);
    }
    let mut occurrences = (from..text.len()).filter(|&index| text[index] == wanted);
    let first = occurrences.next()?;
    if field.word_start[first] {
        Some(first)
    } else {
        occurrences
            .find(|&index| field.word_start[index])
            .or(Some(first))
    }
}

// Character.isWhitespace differs from Unicode White_Space at the no-break spaces and C0 separators.
fn java_whitespace(ch: char) -> bool {
    matches!(ch, '\t'..='\r' | '\u{001c}'..='\u{001f}' | ' ' | '\u{1680}'
        | '\u{2000}'..='\u{2006}' | '\u{2008}'..='\u{200a}' | '\u{2028}' | '\u{2029}'
        | '\u{205f}' | '\u{3000}')
}

fn clean(text: &str) -> String {
    let mut output = String::new();
    let mut count = 0;
    for ch in text.chars() {
        let ch = match ch {
            '\t'..='\r' | '\u{0085}' | '\u{2028}' | '\u{2029}' if !output.ends_with(' ') => ' ',
            ch if ch.is_control()
                || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}') =>
            {
                continue;
            }
            ch => ch,
        };
        if output.is_empty() && java_whitespace(ch) {
            continue;
        }
        output.push(ch);
        count += 1;
        if count == MAX_ROW_CHARS {
            break;
        }
    }
    output.truncate(output.trim_end_matches(java_whitespace).len());
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(kind: Kind, entries: impl IntoIterator<Item = Entry>) -> Source {
        Source {
            kind,
            entries: entries.into_iter().collect(),
        }
    }

    fn action(action: Action, title: &str, detail: &str) -> Entry {
        Entry::new(Key::Action(action), title, detail)
    }

    fn snippet(id: u8) -> Key {
        Key::Snippet(SnippetId::try_from(format!("{id:032x}")).unwrap())
    }

    fn keys(entries: Vec<&Entry>) -> Vec<Key> {
        entries.into_iter().map(|entry| entry.key.clone()).collect()
    }

    #[test]
    fn scores_match_the_pinned_java_fuzzy_matcher() {
        // FuzzyMatcherTest.pinnedScores at 1ebbe724ee84.
        for (query, text, expected) in [
            ("dpl", "deploy.sh", 35),
            ("DEPLOY", "deploy.sh", 69),
            ("lpd", "deploy.sh", -1),
            ("x", "", -1),
            ("  ", "deploy.sh", 0),
            ("", "", 0),
            ("de ploy", "deploy.sh", 69),
            ("dep", "deploy.sh", 50),
            ("dep", "old-deploy-helper.sh", 39),
            ("bk", "backup-keys.sh", 31),
            ("bk", "bookmark.sh", 28),
            ("log", "log.sh", 51),
            ("log", "logrotate-configuration.sh", 46),
            ("docker", "docker-clean.sh", 67),
            ("docker", "docker", 69),
            ("jl", "View › Live Journal › Dock Left", 23),
            ("journalleft", "View › Live Journal › Dock Left", 79),
            ("aa", "banana", 21),
            ("ss", "ssh-tunnel", 43),
            ("Üb", "über-skript", 43),
            ("tun", "SSH Tunnel", 42),
            ("rs", "restart-service.sh", 30),
        ] {
            let query = normalized_query(query, usize::MAX).unwrap();
            assert_eq!(
                score(&query, &Field::new(text)).unwrap_or(-1),
                expected,
                "{text}"
            );
        }
    }

    #[test]
    fn word_starts_use_java_letter_and_decimal_digit_categories() {
        let query = folded("b");
        for text in ["¹b", "\u{0345}b", "😀b"] {
            assert_eq!(score(&query, &Field::new(text)), Some(31), "{text}");
        }
        for text in ["1b", "١b", "áb"] {
            assert_eq!(score(&query, &Field::new(text)), Some(27), "{text}");
        }
    }

    #[test]
    fn supported_prefixes_scope_rows_and_missing_prefixes_match_literally() {
        let action = action(Action::Editor, "C# Format", "Edit");
        let action_key = action.key.clone();
        let snapshot = Snapshot::new([source(Kind::Action, [action])]);
        assert_eq!(snapshot.scoped_query(Some(Kind::Action)), ">");
        assert_eq!(snapshot.scoped_query(Some(Kind::Snippet)), "");
        assert_eq!(snapshot.parse(" \t# "), (None, "#"));
        assert_eq!(
            keys(snapshot.query("#", &Mru::default())).as_slice(),
            std::slice::from_ref(&action_key)
        );

        let snapshot = Snapshot::new([
            source(
                Kind::Action,
                [Entry::new(action_key, "Deploy Settings", "Tools")],
            ),
            source(Kind::Tab, []),
            source(Kind::Connection, []),
            source(Kind::Snippet, [Entry::new(snippet(1), "deploy-a", "")]),
        ]);
        for (kind, prefix) in [
            (Kind::Action, ">"),
            (Kind::Tab, "#"),
            (Kind::Connection, "@"),
            (Kind::Snippet, "$"),
        ] {
            assert_eq!(snapshot.scoped_query(Some(kind)), prefix);
            assert_eq!(snapshot.parse(prefix), (Some(kind), ""));
        }
        assert_eq!(
            keys(snapshot.query("  $ deploy  ", &Mru::default())),
            [snippet(1)]
        );
        assert!(snapshot.query("#", &Mru::default()).is_empty());
    }

    #[test]
    fn rank_weights_title_searches_menu_paths_and_keeps_disabled_matches_last() {
        let mut disabled = action(Action::Scheduler, "Session Journals", "Tools");
        disabled.enabled = false;
        disabled.disabled_reason = Some("Managed by your organization".into());
        let snapshot = Snapshot::new([source(
            Kind::Action,
            [
                action(Action::CloseTab, "Close Tab", "Find"),
                action(Action::Find, "Find", "Edit"),
                action(Action::FilesLeft, "Dock Left", "View › Live Journal"),
                action(Action::FilesRight, "Dock Left", "View › File Browser"),
                disabled,
                action(
                    Action::ToggleActionToolbar,
                    "Toggle Session Journal",
                    "View",
                ),
            ],
        )]);
        let mru = Mru::default();
        assert_eq!(
            keys(snapshot.query("find", &mru)),
            [Key::Action(Action::Find), Key::Action(Action::CloseTab)]
        );
        assert_eq!(
            keys(snapshot.query("journal left", &mru)),
            [Key::Action(Action::FilesLeft)]
        );
        assert_eq!(
            keys(snapshot.query("session journal", &mru)),
            [
                Key::Action(Action::ToggleActionToolbar),
                Key::Action(Action::Scheduler)
            ]
        );
    }

    #[test]
    fn snippets_match_folder_category_and_tags_and_ignore_the_displayed_target() {
        let mut release = Entry::new(snippet(1), "release.sh", "Run in web-01 · pane 2");
        release.search_detail = Some("infra/deploy Operations helm chart".into());
        release.alternate = true;
        release.enabled = false;
        let snapshot = Snapshot::new([
            source(Kind::Tab, [Entry::new(Key::Tab(9), "web-01", "")]),
            source(Kind::Snippet, [release]),
        ]);
        let mru = Mru::default();
        assert_eq!(keys(snapshot.query("web", &mru)), [Key::Tab(9)]);
        for query in ["$", "$helm", "$operations", "$deploy release"] {
            let found = snapshot.query(query, &mru);
            assert_eq!(found.len(), 1, "{query}");
            assert_eq!(found[0].key, snippet(1));
            assert!(!found[0].enabled && found[0].alternate);
        }
    }

    #[test]
    fn empty_browse_groups_commands_and_excludes_snippets_and_unused_connections() {
        let snapshot = Snapshot::new([
            source(
                Kind::Action,
                [
                    action(Action::Find, "Find", "Edit"),
                    action(Action::Settings, "Settings", "View"),
                    action(Action::Paste, "Paste", "Edit"),
                ],
            ),
            source(
                Kind::Tab,
                [
                    Entry::new(Key::Tab(2), "Newer", ""),
                    Entry::new(Key::Tab(1), "Older", ""),
                ],
            ),
            source(
                Kind::Connection,
                [
                    Entry::new(Key::Connection(1), "db-01", ""),
                    Entry::new(Key::Connection(2), "db-02", ""),
                ],
            ),
            source(Kind::Snippet, [Entry::new(snippet(1), "Disk usage", "")]),
        ]);
        let mut mru = Mru::default();
        mru.choose(Key::Connection(2));
        mru.choose(snippet(1));
        mru.choose(Key::Tab(1));
        assert_eq!(mru.items, [snippet(1), Key::Connection(2)]);
        assert_eq!(
            keys(snapshot.query("", &mru)),
            [
                Key::Connection(2),
                Key::Tab(2),
                Key::Tab(1),
                Key::Action(Action::Find),
                Key::Action(Action::Paste),
                Key::Action(Action::Settings)
            ]
        );
        assert_eq!(
            keys(snapshot.query("@", &mru)),
            [Key::Connection(2), Key::Connection(1)]
        );
        assert_eq!(keys(snapshot.query("$", &mru)), [snippet(1)]);
    }

    #[test]
    fn equal_matches_preserve_source_order_and_recent_choice_boosts_ranking() {
        let snapshot = Snapshot::new([
            source(
                Kind::Tab,
                [
                    Entry::new(Key::Tab(2), "db-b", ""),
                    Entry::new(Key::Tab(1), "db-a", ""),
                ],
            ),
            source(
                Kind::Connection,
                [
                    Entry::new(Key::Connection(2), "db-b", ""),
                    Entry::new(Key::Connection(1), "db-a", ""),
                ],
            ),
            source(
                Kind::Snippet,
                [
                    Entry::new(snippet(2), "db-b", ""),
                    Entry::new(snippet(1), "db-a", ""),
                ],
            ),
            source(
                Kind::Action,
                [
                    action(Action::NewWindow, "db-b", "File"),
                    action(Action::OpenProject, "db-a", "File"),
                ],
            ),
        ]);
        let mut mru = Mru::default();
        assert_eq!(
            keys(snapshot.query("db", &mru)),
            [
                Key::Action(Action::OpenProject),
                Key::Action(Action::NewWindow),
                Key::Tab(2),
                Key::Tab(1),
                Key::Connection(2),
                Key::Connection(1),
                snippet(2),
                snippet(1),
            ]
        );
        mru.choose(Key::Action(Action::NewWindow));
        assert_eq!(
            keys(snapshot.query(">db", &mru)),
            [
                Key::Action(Action::NewWindow),
                Key::Action(Action::OpenProject)
            ]
        );
        mru.choose(Key::Connection(1));
        assert_eq!(
            keys(snapshot.query("@db", &mru)),
            [Key::Connection(1), Key::Connection(2)]
        );
    }

    #[test]
    fn teamwork_keys_keep_matching_external_ids_separate_by_origin() {
        let profile = crate::snippets::tests::Profile::new();
        let settings = teamwork::Settings {
            sources: ["alpha.xml", "beta.xml"]
                .into_iter()
                .map(|file| {
                    let path = profile.0.join(file);
                    std::fs::write(
                        &path,
                        r#"<connections><connection id="7"><name>Database</name>
                            <host>db.example</host><username>operator</username>
                            </connection></connections>"#,
                    )
                    .unwrap();
                    teamwork::Source {
                        location: path.to_str().unwrap().into(),
                        enabled: true,
                        kind: teamwork::SourceKind::File,
                        check_interval_minutes: teamwork::DEFAULT_CHECK_INTERVAL_MINUTES,
                    }
                })
                .collect(),
            ..teamwork::Settings::default()
        };
        let catalog =
            teamwork::Catalog::default().refreshed(&settings.active_sources(true, &[]).unwrap());
        let mut entries = catalog.entries();
        let first = Key::Teamwork(entries.next().unwrap().identity().clone());
        let second = Key::Teamwork(entries.next().unwrap().identity().clone());
        let local = Key::Connection(7);
        let snapshot = Snapshot::new([source(
            Kind::Connection,
            [&local, &first, &second]
                .into_iter()
                .map(|key| Entry::new(key.clone(), "Database", "")),
        )]);
        let mut recent = Mru::default();
        assert_eq!(
            keys(snapshot.query("@", &recent)),
            [local.clone(), first.clone(), second.clone()]
        );
        recent.choose(second.clone());
        assert_eq!(
            keys(snapshot.query("@database", &recent)),
            [second.clone(), local, first.clone()]
        );

        let reopened = Snapshot::new([source(
            Kind::Connection,
            [Entry::new(first.clone(), "Database", "")],
        )]);
        assert!(reopened.entry(&second).is_none());
        assert!(reopened.query("", &recent).is_empty());
        assert_eq!(keys(reopened.query("@", &recent)), [first]);
    }

    #[test]
    fn recent_and_result_limits_are_independent_and_old_snapshots_do_not_supply_rows() {
        let connections = (0..80).map(|id| Entry::new(Key::Connection(id), "db", ""));
        let snapshot = Snapshot::new([source(Kind::Connection, connections)]);
        let mut mru = Mru::default();
        for id in 0..25 {
            mru.choose(Key::Connection(id));
        }
        mru.choose(Key::Connection(12));
        assert_eq!(mru.items[0], Key::Connection(12));
        assert_eq!(mru.items.len(), RECENT_CAPACITY);
        assert!(!mru.items.contains(&Key::Connection(4)));
        assert_eq!(
            keys(snapshot.query("", &mru)),
            mru.items[..RECENT_ON_EMPTY_QUERY]
        );
        let scoped = snapshot.query("@db", &mru);
        assert_eq!(scoped.len(), DEFAULT_LIMIT);
        assert_eq!(keys(scoped)[..RECENT_CAPACITY], mru.items);
        assert_eq!(snapshot.query_limit("@db", &mru, 0).len(), 80);
        assert!(snapshot.entry(&Key::Connection(12)).is_some());
        let reopened = Snapshot::new([source(Kind::Connection, [])]);
        assert!(reopened.entry(&Key::Connection(12)).is_none());
        assert!(reopened.query("", &mru).is_empty());
    }

    #[test]
    fn text_matches_reference_sanitization_without_splitting_supplementary_characters() {
        let mut entry = Entry::new(
            Key::Connection(1),
            "prod\u{202e}10.0.0.1\u{202c}\x07",
            "root@\u{2066}web\u{2069}\r\nlog\u{2028}next",
        );
        entry.shortcut = Some("Ctrl+\0K".into());
        entry.disabled_reason = Some("Managed\x1b[31m".into());
        let snapshot = Snapshot::new([source(Kind::Connection, [entry])]);
        let entry = snapshot.entry(&Key::Connection(1)).unwrap();
        assert_eq!(entry.title, "prod10.0.0.1");
        assert_eq!(entry.detail, "root@web log next");
        assert_eq!(entry.shortcut.as_deref(), Some("Ctrl+K"));
        assert_eq!(entry.disabled_reason.as_deref(), Some("Managed[31m"));
        assert_eq!(
            clean(&format!(" \t{}\n", "😀".repeat(180))),
            "😀".repeat(120)
        );
        assert_eq!(clean(" \u{00a0}\x07 "), "\u{00a0}");
    }
}
