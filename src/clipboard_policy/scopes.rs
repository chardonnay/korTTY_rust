use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::io;

use toml_edit::{ArrayOfTables, Item, Table};

use super::{invalid, trim_java};

type Names = BTreeSet<String>;
type Groups = BTreeMap<String, Names>;

pub(super) struct Scopes<'a> {
    groups: Groups,
    rules: Vec<ScopedRule<'a>>,
}

struct ScopedRule<'a> {
    table: &'a Table,
    users: Names,
    groups: Names,
}

#[derive(Clone, Copy)]
enum Tier {
    User,
    Group,
    All,
}

impl<'a> Scopes<'a> {
    pub(super) fn parse(root: &'a Table) -> io::Result<Self> {
        let mut groups = Groups::new();
        if let Some(item) = root.get("groups") {
            let table = item
                .as_table()
                .ok_or_else(|| invalid("groups must be a table"))?;
            for (group, item) in table.iter() {
                let group = parse_name(group)
                    .ok_or_else(|| invalid("groups must use non-empty group names"))?;
                let members = names(Some(item), "group members")?;
                if groups.insert(group, members).is_some() {
                    return Err(invalid("groups contains duplicate normalized group names"));
                }
            }
        }

        let mut rules = Vec::new();
        if let Some(item) = root.get("rule") {
            let tables = item
                .as_array_of_tables()
                .ok_or_else(|| invalid("rule must be an array of tables"))?;
            for (index, rule) in tables.iter().enumerate() {
                let read = |key| names(rule.get(key), &format!("rule #{} {key}", index + 1));
                rules.push(ScopedRule {
                    table: rule,
                    users: read("users")?,
                    groups: read("groups")?,
                });
            }
        }
        Ok(Self { groups, rules })
    }

    pub(super) fn rules(&self) -> impl Iterator<Item = &'a Table> + '_ {
        self.rules.iter().map(|rule| rule.table)
    }

    pub(super) fn needs_user(&self) -> bool {
        self.rules
            .iter()
            .any(|rule| !rule.users.is_empty() || !rule.groups.is_empty())
    }

    pub(super) fn needs_os_groups(&self, username: &str) -> bool {
        let user = normalize(username);
        self.rules.iter().any(|rule| {
            !rule.groups.is_empty()
                && !rule.users.contains(&user)
                && !rule.matches_policy_group(&user, &self.groups)
        })
    }

    /// Filter validated rule payloads while retaining the reducers' same-tier input order.
    pub(super) fn effective_rules(
        self,
        username: &str,
        os_groups: &[String],
    ) -> io::Result<Vec<Table>> {
        let user = normalize(username);
        let os_groups: Names = os_groups.iter().map(|group| normalize(group)).collect();
        let mut tiers = [Vec::new(), Vec::new(), Vec::new()];
        for rule in self.rules {
            if let Some(tier) = rule.tier(&user, &self.groups, &os_groups) {
                tiers[tier as usize].push(rule.table);
            }
        }

        let mut selected = Vec::new();
        let mut winners = BTreeMap::new();
        let mut seen_replacements = BTreeSet::new();
        for (tier, rules) in tiers.into_iter().enumerate() {
            for rule in rules {
                let mut filtered = Table::new();
                for (section, item) in rule.iter() {
                    match section {
                        "users" | "groups" | "name" => {}
                        "servers" => {
                            if *winners.entry((section, Cow::Borrowed(""))).or_insert(tier) == tier
                            {
                                filtered.insert(section, item.clone());
                            }
                        }
                        _ => {
                            let table = item.as_table().ok_or_else(|| {
                                invalid(format!("rule.{section} must be a table"))
                            })?;
                            let mut fields = Table::new();
                            for (key, value) in table.iter() {
                                if (section, key) == ("session-journal", "replace") {
                                    if let Some(replacements) =
                                        new_replacements(value, &mut seen_replacements)
                                    {
                                        fields.insert(key, replacements);
                                    }
                                    continue;
                                }
                                let setting = if section == "features" {
                                    Cow::Owned(normalize(key))
                                } else {
                                    Cow::Borrowed(key)
                                };
                                if *winners.entry((section, setting)).or_insert(tier) == tier {
                                    fields.insert(key, value.clone());
                                }
                            }
                            if !fields.is_empty() {
                                filtered.insert(section, Item::Table(fields));
                            }
                        }
                    }
                }
                if !filtered.is_empty() {
                    if let Some(name) = rule.get("name") {
                        filtered.insert("name", name.clone());
                    }
                    selected.push(filtered);
                }
            }
        }
        Ok(selected)
    }
}

impl ScopedRule<'_> {
    fn matches_policy_group(&self, user: &str, groups: &Groups) -> bool {
        self.groups.iter().any(|group| {
            groups
                .get(group)
                .is_some_and(|members| members.contains(user))
        })
    }

    fn tier(&self, user: &str, groups: &Groups, os_groups: &Names) -> Option<Tier> {
        if self.users.contains(user) {
            Some(Tier::User)
        } else if !self.groups.is_disjoint(os_groups) || self.matches_policy_group(user, groups) {
            Some(Tier::Group)
        } else if self.users.is_empty() && self.groups.is_empty() {
            Some(Tier::All)
        } else {
            None
        }
    }
}

fn names(item: Option<&Item>, field: &str) -> io::Result<Names> {
    let Some(item) = item else {
        return Ok(Names::new());
    };
    let invalid_names = || invalid(format!("{field} must be an array of non-empty names"));
    item.as_array()
        .ok_or_else(invalid_names)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .and_then(parse_name)
                .ok_or_else(invalid_names)
        })
        .collect()
}

fn parse_name(value: &str) -> Option<String> {
    let value = trim_java(value);
    (!value.trim().is_empty() && !value.chars().any(char::is_control)).then(|| value.to_lowercase())
}

fn normalize(value: &str) -> String {
    trim_java(value).to_lowercase()
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
struct Replacement<'a> {
    pattern: &'a str,
    replacement: &'a str,
    regex: bool,
    ignore_case: bool,
    label: Option<&'a str>,
}

fn new_replacements<'a>(item: &'a Item, seen: &mut BTreeSet<Replacement<'a>>) -> Option<Item> {
    let Some(entries) = item.as_array_of_tables() else {
        return Some(item.clone());
    };
    let mut selected = ArrayOfTables::new();
    for entry in entries.iter() {
        let text = |key| entry.get(key).and_then(Item::as_str);
        let flag = |key| entry.get(key).and_then(Item::as_bool).unwrap_or(false);
        // Use the Rust redactor's literal text and defaults when comparing definitions.
        if text("pattern").is_none_or(|pattern| {
            seen.insert(Replacement {
                pattern,
                replacement: text("replacement").unwrap_or("***"),
                regex: flag("regex"),
                ignore_case: flag("ignore-case"),
                label: text("label")
                    .map(trim_java)
                    .filter(|label| !label.is_empty()),
            })
        }) {
            selected.push(entry.clone());
        }
    }
    (!selected.is_empty()).then_some(Item::ArrayOfTables(selected))
}

#[cfg(test)]
mod tests {
    use toml_edit::DocumentMut;

    use super::*;

    fn document(source: &str) -> DocumentMut {
        source.parse().unwrap()
    }

    fn field<'a>(rule: &'a Table, section: &str, key: &str) -> Option<&'a Item> {
        rule.get(section)?.as_table()?.get(key)
    }

    #[test]
    fn keeps_winning_tier_per_setting_and_replaces_the_whole_server_table() {
        let document = document(
            r#"
            [groups]
            ' Operations ' = [' ALIce ']

            [[rule]]
            name = 'baseline'
            [rule.features]
            teamwork = 'deny'
            control-api = 'deny'
            [rule.servers]
            mode = 'deny'
            hosts = ['baseline.example']

            [[rule]]
            name = 'group allow'
            groups = ['operations']
            [rule.features]
            teamwork = 'deny'
            [rule.servers]
            mode = 'allow'
            hosts = ['*.example']

            [[rule]]
            name = 'user'
            users = [' ALICE ']
            groups = ['operations']
            [rule.features]
            ' TEAMWORK ' = 'allow'

            [[rule]]
            name = 'group deny'
            groups = [' operations ']
            [rule.servers]
            mode = 'deny'
            hosts = ['vault.example']

            [[rule]]
            name = 'unmatched'
            users = ['Bob']
            [rule.features]
            control-api = 'allow'
        "#,
        );
        let scopes = Scopes::parse(document.as_table()).unwrap();
        assert_eq!(scopes.rules().count(), 5);
        assert!(scopes.needs_user());
        assert!(!scopes.needs_os_groups(" Alice "));
        let selected = scopes.effective_rules(" Alice ", &[]).unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|rule| rule["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["user", "group allow", "group deny", "baseline"]
        );
        assert_eq!(
            field(&selected[0], "features", " TEAMWORK ").and_then(Item::as_str),
            Some("allow")
        );
        assert!(!selected[1].contains_key("features"));
        for (rule, mode) in selected[1..3].iter().zip(["allow", "deny"]) {
            assert_eq!(
                field(rule, "servers", "mode").and_then(Item::as_str),
                Some(mode)
            );
            assert!(field(rule, "servers", "hosts").is_some());
        }
        assert!(!selected[3].contains_key("servers"));
        assert_eq!(
            field(&selected[3], "features", "control-api").and_then(Item::as_str),
            Some("deny")
        );
        assert!(
            selected
                .iter()
                .all(|rule| !rule.contains_key("users") && !rule.contains_key("groups"))
        );
    }

    #[test]
    fn requests_os_groups_only_for_unresolved_group_selectors() {
        let document = document(
            r#"
            [groups]
            'Operators' = ['alice']

            [[rule]]
            name = 'selected'
            users = ['bob']
            groups = ['operators', 'machine-only']
            [rule.features]
            control-api = 'allow'
        "#,
        );
        let scopes = Scopes::parse(document.as_table()).unwrap();
        assert!(!scopes.needs_os_groups("ALICE"));
        assert!(!scopes.needs_os_groups(" Bob "));
        assert!(scopes.needs_os_groups("carol"));
        assert!(scopes.effective_rules("carol", &[]).unwrap().is_empty());
        let scopes = Scopes::parse(document.as_table()).unwrap();
        assert_eq!(
            scopes
                .effective_rules("carol", &[" MACHINE-ONLY ".into()])
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn journal_replacements_accumulate_in_tier_and_file_order() {
        let document = document(
            r#"
            [[rule]]
            [[rule.session-journal.replace]]
            pattern = 'baseline'

            [[rule]]
            groups = ['operators']
            [[rule.session-journal.replace]]
            pattern = 'group'
            [[rule.session-journal.replace]]
            pattern = 'duplicate'
            replacement = '***'

            [[rule]]
            users = ['alice']
            [[rule.session-journal.replace]]
            pattern = 'duplicate'
            [[rule.session-journal.replace]]
            pattern = 'user first'

            [[rule]]
            users = ['alice']
            [[rule.session-journal.replace]]
            pattern = 'user second'

            [[rule]]
            users = ['bob']
            [[rule.session-journal.replace]]
            pattern = 'unmatched'
        "#,
        );
        let selected = Scopes::parse(document.as_table())
            .unwrap()
            .effective_rules("alice", &["operators".into()])
            .unwrap();
        let patterns: Vec<_> = selected
            .iter()
            .filter_map(|rule| field(rule, "session-journal", "replace"))
            .flat_map(|item| item.as_array_of_tables().unwrap().iter())
            .map(|entry| entry["pattern"].as_str().unwrap())
            .collect();
        assert_eq!(
            patterns,
            [
                "duplicate",
                "user first",
                "user second",
                "group",
                "baseline"
            ]
        );
    }

    #[test]
    fn validates_scopes_for_unmatched_rules_and_without_rules() {
        for source in [
            "groups = ['operators']",
            "[groups]\noperators = false",
            "[groups]\noperators = ['alice', 42]",
            "[groups]\noperators = ['   ']",
            "[groups]\n' Operators ' = ['alice']\noperators = ['bob']",
            "[groups]\n'' = ['alice']",
            "[[rule]]\nusers = 'somebody else'",
            "[[rule]]\nusers = ['somebody else']\ngroups = ['']",
            "[[rule]]\nusers = ['somebody else']\ngroups = ['valid', true]",
        ] {
            assert!(
                Scopes::parse(document(source).as_table()).is_err(),
                "{source}"
            );
        }
        let document = document(
            r#"
            [groups]
            empty = []
            [[rule]]
            users = []
            groups = []
            [rule.features]
            control-api = 'deny'
        "#,
        );
        let scopes = Scopes::parse(document.as_table()).unwrap();
        assert!(!scopes.needs_user());
        assert!(!scopes.needs_os_groups(""));
        assert_eq!(scopes.effective_rules("", &[]).unwrap().len(), 1);
    }
}
