//! Local routing and reviewed mutations for rules learned by an AI skill.

use std::collections::{BTreeMap, HashSet};
use std::fmt::{self, Write as _};

use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Serialize};

use super::{BUILTINS, Skill, Store};
use crate::ai_manager::memory::learning::checked_rule;
use crate::ai_manager::{RuleOrigin, RuleSource};

const KNOWN_TOOLS: &[&str] = &[
    "elixir",
    "erlang",
    "haskell",
    "scala",
    "clojure",
    "ocaml",
    "f#",
    "zig",
    "nim",
    "julia",
    "fortran",
    "cobol",
    "lisp",
    "racket",
    "crystal",
    "matlab",
    "octave",
    "pascal",
    "delphi",
    "prolog",
    "solidity",
    "objective-c",
    "docker",
    "kubernetes",
    "terraform",
    "helm",
    "git",
    "cmake",
    "gradle",
    "maven",
    "npm",
    "pip",
    "cargo",
    "nginx",
    "postgresql",
    "mysql",
    "sqlite",
    "redis",
    "mongodb",
    "vim",
    "emacs",
    "tmux",
    "awk",
    "sed",
    "graphql",
    "protobuf",
    "bazel",
    "podman",
];
const COMMON_TOPICS: &[&str] = &[
    "all",
    "alle",
    "alles",
    "allgemein",
    "andere",
    "answer",
    "answers",
    "antwort",
    "antworten",
    "ausgabe",
    "code",
    "comments",
    "datei",
    "dateien",
    "deutsch",
    "english",
    "englisch",
    "file",
    "files",
    "format",
    "formatierung",
    "formatting",
    "general",
    "german",
    "global",
    "keine",
    "kein",
    "kommentare",
    "language",
    "sprache",
    "misc",
    "name",
    "namen",
    "naming",
    "none",
    "other",
    "output",
    "regel",
    "regeln",
    "rule",
    "rules",
    "sonstiges",
    "style",
    "stil",
    "text",
    "texte",
    "tool",
    "tools",
];
const STOP_WORDS: &[&str] = &[
    "about", "after", "also", "and", "are", "ask", "but", "can", "code", "das", "der", "die",
    "ein", "eine", "for", "from", "how", "ich", "ist", "mit", "not", "oder", "please", "prompt",
    "soll", "the", "this", "und", "use", "was", "welche", "what", "when", "where", "wie", "with",
    "you", "zur",
];

#[derive(Clone, Eq, PartialEq)]
enum TargetKind {
    Existing(u64),
    New(String),
}

#[derive(Clone)]
pub(crate) struct SkillRuleTarget {
    kind: TargetKind,
    name: String,
    show_id: bool,
}

impl SkillRuleTarget {
    fn existing(id: u64, skill: &Skill) -> Self {
        Self {
            kind: TargetKind::Existing(id),
            name: skill.name.clone(),
            show_id: false,
        }
    }

    pub(crate) fn is_new(&self) -> bool {
        matches!(self.kind, TargetKind::New(_))
    }
}

impl PartialEq for SkillRuleTarget {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.name == other.name
    }
}

impl Eq for SkillRuleTarget {}

impl fmt::Display for SkillRuleTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_new() {
            return write!(formatter, "New skill: {}", self.name);
        }
        write!(formatter, "Skill: {}", self.name)?;
        if let TargetKind::Existing(id) = self.kind
            && self.show_id
        {
            write!(formatter, " [#{id}]")?;
        }
        Ok(())
    }
}

impl fmt::Debug for SkillRuleTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SkillRuleTarget")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Options {
    pub(crate) targets: Vec<SkillRuleTarget>,
    pub(crate) suggested: Option<SkillRuleTarget>,
}

#[derive(Clone)]
pub(crate) struct SkillRuleProposal {
    target: SkillRuleTarget,
    origin: RuleOrigin,
    before: Option<Skill>,
    after: Skill,
    rule: String,
    identity: RuleIdentity,
    current_text: String,
    proposed_text: String,
}

impl fmt::Debug for SkillRuleProposal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SkillRuleProposal")
            .finish_non_exhaustive()
    }
}

impl SkillRuleProposal {
    pub(crate) fn current_text(&self) -> &str {
        &self.current_text
    }

    pub(crate) fn proposed_text(&self) -> &str {
        &self.proposed_text
    }

    pub(crate) fn rule(&self) -> &str {
        &self.rule
    }

    pub(in crate::ai_manager) fn apply(
        &self,
        store: &mut Store,
        allow_new: bool,
    ) -> Result<SavedSkillRule, String> {
        if !store
            .rule_targets(&self.rule, &self.origin, allow_new)
            .targets
            .contains(&self.target)
        {
            return Err("The reviewed skill target is no longer available.".into());
        }
        let (id, created) = match self.target.kind {
            TargetKind::Existing(id) => {
                if store.skills.get(&id) != self.before.as_ref() {
                    return Err("The skill changed. Review the rule again before saving.".into());
                }
                store.replace(id, self.after.clone())?;
                (id, None)
            }
            TargetKind::New(_) => (store.insert(self.after.clone())?, Some(self.after.clone())),
        };
        Ok(SavedSkillRule {
            id,
            identity: self.identity.clone(),
            rule: self.rule.clone(),
            created,
        })
    }
}

#[derive(Clone)]
pub(crate) struct SavedSkillRule {
    id: u64,
    identity: RuleIdentity,
    rule: String,
    created: Option<Skill>,
}

impl fmt::Debug for SavedSkillRule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SavedSkillRule")
            .finish_non_exhaustive()
    }
}

impl SavedSkillRule {
    pub(in crate::ai_manager) fn undo(&self, store: &mut Store) -> Result<(), String> {
        let current = store
            .skills
            .get(&self.id)
            .ok_or("The saved skill was removed.")?;
        if self.created.as_ref() == Some(current) {
            store.skills.remove(&self.id);
            return Ok(());
        }
        let Some(record) = current.learned_rule_ids.iter().position(|identity| {
            identity.id == self.identity.id && identity.sha256 == self.identity.sha256
        }) else {
            return Err("The saved skill rule was edited or removed.".into());
        };
        let mut indexes = current
            .rules
            .iter()
            .enumerate()
            .filter(|(_, rule)| *rule == &self.rule);
        let Some((index, _)) = indexes.next().filter(|_| indexes.next().is_none()) else {
            return Err("The saved skill rule no longer matches.".into());
        };
        let mut changed = current.clone();
        changed.rules.remove(index);
        changed.learned_rule_ids.remove(record);
        store.replace(self.id, changed)
    }
}

impl Store {
    pub(in crate::ai_manager) fn rule_targets(
        &self,
        input: &str,
        origin: &RuleOrigin,
        allow_new: bool,
    ) -> Options {
        if input.len() > super::MAX_SKILL_BYTES {
            return Options::default();
        }
        let hint = origin
            .topic
            .as_deref()
            .filter(|_| origin.source == RuleSource::Chat)
            .map(normalize_topic);
        let topic = topic(input, hint.as_deref());
        let builtin_id = topic.as_deref().and_then(builtin_for_topic);
        let users: Vec<_> = self
            .skills
            .iter()
            .filter(|(_, skill)| eligible_user(skill))
            .collect();
        let builtin = builtin_id.and_then(|id| {
            self.skills.iter().find(|(_, skill)| {
                skill.builtin.as_deref() == Some(id) && skill.enabled && !skill.hidden
            })
        });
        let new = topic
            .as_deref()
            .filter(|topic| {
                allow_new
                    && builtin_id.is_none()
                    && (KNOWN_TOOLS.contains(topic)
                        || (hint.as_deref() == Some(*topic)
                            && tool_word(topic)
                            && !COMMON_TOPICS.contains(topic)))
            })
            .and_then(|topic| {
                let name = format!("{} (kortty)", topic_name(topic));
                (!self
                    .skills
                    .values()
                    .any(|skill| has_tag(skill, topic) || skill.name.eq_ignore_ascii_case(&name)))
                .then(|| SkillRuleTarget {
                    kind: TargetKind::New(topic.to_owned()),
                    name,
                    show_id: false,
                })
            });
        let matching = if let Some(topic) = topic.as_deref().filter(|_| builtin_id.is_some()) {
            users
                .iter()
                .copied()
                .find(|(_, skill)| has_tag(skill, topic))
                .or(builtin)
        } else {
            relevant_user(&users, topic.as_deref().unwrap_or(input))
        };
        let mut names = BTreeMap::new();
        for (_, skill) in users.iter().copied().chain(builtin) {
            *names.entry(skill.name.as_str()).or_insert(0) += 1;
        }
        let existing = |(&id, skill): (&u64, &Skill)| {
            let mut target = SkillRuleTarget::existing(id, skill);
            target.show_id = names[skill.name.as_str()] > 1;
            target
        };
        let suggested = matching.map(existing).or_else(|| new.clone());
        let targets = users
            .into_iter()
            .chain(builtin)
            .map(existing)
            .chain(new)
            .collect();
        Options { targets, suggested }
    }

    pub(in crate::ai_manager) fn review_rule(
        &self,
        target: &SkillRuleTarget,
        rule: &str,
        origin: &RuleOrigin,
        allow_new: bool,
    ) -> Result<SkillRuleProposal, String> {
        let rule = checked_rule(rule).map_err(|error| error.to_string())?;
        if rule.contains("kortty:learned-rules") {
            return Err("The rule contains a reserved skill Markdown marker.".into());
        }
        if !self
            .rule_targets(&rule, origin, allow_new)
            .targets
            .contains(target)
        {
            return Err("The selected skill is no longer available for this rule.".into());
        }
        let (before, mut after) = match &target.kind {
            TargetKind::Existing(id) => {
                let skill = self
                    .skills
                    .get(id)
                    .ok_or("The selected skill was removed.")?;
                (Some(skill.clone()), skill.clone())
            }
            TargetKind::New(topic) => (None, new_skill(topic, &target.name)),
        };
        let sha256 = fingerprint(&rule)?;
        if after.rules.iter().any(|existing| existing.trim() == rule) {
            return Err("This rule is already saved in the skill.".into());
        }
        let identity = RuleIdentity::new(sha256, origin.source)?;
        after.rules.push(rule.clone());
        after.learned_rule_ids.push(identity.clone());
        after.check()?;
        let current_text = before.as_ref().map(Skill::to_markdown).unwrap_or_default();
        let proposed_text = after.to_markdown();
        Ok(SkillRuleProposal {
            target: target.clone(),
            origin: origin.clone(),
            before,
            after,
            rule,
            identity,
            current_text,
            proposed_text,
        })
    }
}

fn eligible_user(skill: &Skill) -> bool {
    // Native Markdown imports are editable local skills.
    skill.builtin.is_none() && skill.enabled && !skill.hidden && !skill.content.trim().is_empty()
}

fn has_tag(skill: &Skill, topic: &str) -> bool {
    skill.tags.iter().any(|tag| tag.eq_ignore_ascii_case(topic))
}

fn builtin_for_topic(topic: &str) -> Option<&'static str> {
    BUILTINS
        .iter()
        .find(|skill| {
            skill
                .builtin_topics
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(topic))
        })
        .and_then(|skill| skill.builtin.as_deref())
}

fn word_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '#' | '+' | '.' | '_' | '-')
}

fn normalize_topic(topic: &str) -> String {
    topic.trim().trim_end_matches('.').to_lowercase()
}

fn tool_word(topic: &str) -> bool {
    (2..=30).contains(&topic.len())
        && topic.as_bytes()[0].is_ascii_lowercase()
        && topic
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+#._-".contains(&byte))
}

fn topic(input: &str, hint: Option<&str>) -> Option<String> {
    let known = |topic: &str| KNOWN_TOOLS.contains(&topic) || builtin_for_topic(topic).is_some();
    if let Some(hint) = hint.filter(|hint| known(hint)) {
        return Some(hint.into());
    }
    let leading = input
        .split_once(':')
        .map(|(prefix, _)| prefix.trim())
        .filter(|word| (2..=30).contains(&word.chars().count()) && word.chars().all(word_character))
        .map(normalize_topic)
        .filter(|word| !word.is_empty());
    if leading.as_deref().is_some_and(known) {
        return leading;
    }
    input
        .split(|character| !word_character(character))
        .map(normalize_topic)
        .find(|word| {
            let count = word.chars().count();
            (count >= 3 || count == 2 && word.contains(['#', '+', '.'])) && known(word)
        })
        .or_else(|| hint.filter(|hint| tool_word(hint)).map(str::to_owned))
        .or(leading)
}

fn relevant_user<'a>(
    users: &[(&'a u64, &'a Skill)],
    relevance: &str,
) -> Option<(&'a u64, &'a Skill)> {
    let context = normalize_score(relevance);
    let context_terms = terms(&context);
    let mut best = None;
    let mut best_score = 3;
    for &(id, skill) in users {
        let score = |value: &str, phrase_weight, term_weight| {
            let value = normalize_score(value);
            if value.is_empty() {
                return 0;
            }
            usize::from(context.contains(&value)) * phrase_weight
                + terms(&value).intersection(&context_terms).count() * term_weight
        };
        let headings = skill.content.lines().filter_map(|line| {
            let line = line.trim();
            line.starts_with('#')
                .then(|| line.trim_start_matches('#').trim())
        });
        let total = skill.tags.iter().map(|tag| score(tag, 6, 4)).sum::<usize>()
            + score(&skill.name, 6, 2)
            + score(&skill.description, 5, 1)
            + headings.map(|heading| score(heading, 4, 1)).sum::<usize>();
        if total > best_score {
            best = Some((id, skill));
            best_score = total;
        }
    }
    best
}

fn normalize_score(value: &str) -> String {
    value.trim().to_lowercase().replace('_', "-")
}

fn terms(text: &str) -> HashSet<&str> {
    text.split(|character| !word_character(character))
        .filter(|term| term.chars().count() >= 3 && !STOP_WORDS.contains(term))
        .collect()
}

fn topic_name(topic: &str) -> String {
    let mut characters = topic.chars();
    characters
        .next()
        .into_iter()
        .flat_map(char::to_uppercase)
        .chain(characters)
        .collect()
}

fn new_skill(topic: &str, name: &str) -> Skill {
    let display = topic_name(topic);
    Skill {
        name: name.into(),
        description: format!("Rules the user confirmed for {display}, saved by kortty."),
        tags: vec![topic.into()],
        content: format!(
            "# {name}\n\nThe rules listed below are the ones the user confirmed for {display}. Follow them whenever this skill applies."
        ),
        enabled: true,
        origin: Some("KORTTY".into()),
        created_at: Some(jiff::Zoned::now().date().to_string()),
        ..Skill::default()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuleIdentity {
    id: String,
    sha256: String,
    created_at: String,
    source: RuleSource,
}

impl RuleIdentity {
    fn new(sha256: String, source: RuleSource) -> Result<Self, String> {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(|_| "Could not allocate a learned rule ID.")?;
        random[6] = (random[6] & 0x0f) | 0x40;
        random[8] = (random[8] & 0x3f) | 0x80;
        let mut id = String::with_capacity(36);
        for (index, byte) in random.into_iter().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                id.push('-');
            }
            write!(id, "{byte:02x}").expect("writing into a String succeeds");
        }
        Ok(Self {
            id,
            sha256,
            created_at: jiff::Timestamp::now().to_string(),
            source,
        })
    }

    pub(super) fn bytes(&self) -> usize {
        self.id.len() + self.sha256.len() + self.created_at.len()
    }
}

pub(super) fn check_identities(skill: &Skill) -> Result<(), String> {
    if skill.learned_rule_ids.is_empty() {
        return Ok(());
    }
    if skill.learned_rule_ids.len() > skill.rules.len() {
        return Err("The learned rule identity metadata is invalid.".into());
    }
    let counts = fingerprint_counts(&skill.rules)?;
    let mut ids = HashSet::new();
    let mut digests = HashSet::new();
    if skill.learned_rule_ids.iter().any(|identity| {
        !valid_id(&identity.id)
            || !ids.insert(&identity.id)
            || !digests.insert(&identity.sha256)
            || counts.get(&identity.sha256) != Some(&1)
            || identity.created_at.len() > 40
            || !identity.created_at.ends_with('Z')
            || identity.created_at.parse::<jiff::Timestamp>().is_err()
    }) {
        return Err("The learned rule identity metadata is invalid.".into());
    }
    Ok(())
}

pub(super) fn prune_identities(skill: &mut Skill) -> Result<(), String> {
    if !skill.learned_rule_ids.is_empty()
        && skill.rules.len() <= super::MAX_RULES
        && skill.bytes() <= super::MAX_SKILL_BYTES
    {
        let counts = fingerprint_counts(&skill.rules)?;
        skill
            .learned_rule_ids
            .retain(|identity| counts.get(&identity.sha256) == Some(&1));
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            14 => byte == b'4',
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => matches!(byte, b'0'..=b'9' | b'a'..=b'f'),
        })
}

fn fingerprint_counts(rules: &[String]) -> Result<BTreeMap<String, usize>, String> {
    let mut counts = BTreeMap::new();
    for rule in rules {
        *counts.entry(fingerprint(rule)?).or_default() += 1;
    }
    Ok(counts)
}

fn fingerprint(rule: &str) -> Result<String, String> {
    let digest = HashAlgorithm::Sha256
        .digest(rule.as_bytes())
        .map_err(|_| "Could not calculate the learned rule fingerprint.")?;
    let mut hex = String::with_capacity(64);
    for byte in digest {
        write!(hex, "{byte:02x}").expect("writing into a String succeeds");
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_manager::skills::Target;

    fn user(name: &str, tag: &str) -> Skill {
        Skill {
            name: name.into(),
            tags: vec![tag.into()],
            content: "Use the documented conventions.".into(),
            enabled: true,
            ..Skill::default()
        }
    }

    fn builtin(store: &Store, builtin: &str) -> u64 {
        store
            .skills
            .iter()
            .find_map(|(&id, skill)| (skill.builtin.as_deref() == Some(builtin)).then_some(id))
            .unwrap()
    }

    #[test]
    fn learned_rule_targets_prefer_tagged_user_override_and_exclude_ineligible_skills() {
        let mut store = Store::default();
        let rust = builtin(&store, "builtin.lang.rust");
        let rust_target = SkillRuleTarget::existing(rust, &store.skills[&rust]);
        let mut disabled = user("Disabled", "rust");
        disabled.enabled = false;
        let disabled = store.insert(disabled).unwrap();
        let mut hidden = user("Hidden", "rust");
        hidden.hidden = true;
        let hidden = store.insert(hidden).unwrap();
        let mut empty = user("Empty", "rust");
        empty.content.clear();
        let empty = store.insert(empty).unwrap();
        let local = store.insert(user("Local Rust", "RuSt")).unwrap();
        let local_target = SkillRuleTarget::existing(local, &store.skills[&local]);
        let input = "Rust: Use checked arithmetic for user input.";
        let options = store.rule_targets(input, &RuleOrigin::MANUAL, false);
        assert_eq!(options.suggested, Some(local_target.clone()));
        assert_eq!(local_target.to_string(), "Skill: Local Rust");
        assert_eq!(options.targets, [local_target, rust_target.clone()]);
        for id in [disabled, hidden, empty] {
            let invalid = SkillRuleTarget::existing(id, &store.skills[&id]);
            assert!(
                store
                    .review_rule(&invalid, input, &RuleOrigin::MANUAL, true)
                    .is_err()
            );
        }

        store.skills.get_mut(&local).unwrap().enabled = false;
        assert_eq!(
            store
                .rule_targets(input, &RuleOrigin::MANUAL, false)
                .suggested,
            Some(rust_target)
        );
        store.skills.get_mut(&rust).unwrap().hidden = true;
        assert!(
            store
                .rule_targets(input, &RuleOrigin::MANUAL, false)
                .suggested
                .is_none()
        );
    }

    #[test]
    fn learned_rule_targets_only_create_known_free_topics_and_find_relevant_user_skills() {
        let mut store = Store::default();
        let local = store
            .insert(user("Release procedures", "releases"))
            .unwrap();
        assert_eq!(
            store
                .rule_targets("Releases: check the approval.", &RuleOrigin::MANUAL, true)
                .suggested,
            Some(SkillRuleTarget::existing(local, &store.skills[&local]))
        );
        assert!(
            store
                .rule_targets("Deploy: Check the release.", &RuleOrigin::MANUAL, true)
                .targets
                .iter()
                .all(|target| !target.is_new())
        );
        let input = "Elixir: Prefer explicit timeouts.";
        assert!(
            store
                .rule_targets(input, &RuleOrigin::MANUAL, false)
                .suggested
                .is_none()
        );
        let target = store
            .rule_targets(input, &RuleOrigin::MANUAL, true)
            .suggested
            .unwrap();
        assert!(target.is_new());
        assert_eq!(target.to_string(), "New skill: Elixir (kortty)");
        let reviewed = store
            .review_rule(&target, input, &RuleOrigin::MANUAL, true)
            .unwrap();
        assert!(reviewed.current_text().is_empty());
        assert!(reviewed.proposed_text().contains("kortty-origin: KORTTY"));
        assert!(reviewed.apply(&mut store, false).is_err());
        let mut duplicate = user("Retired project", "ELIXIR");
        duplicate.enabled = false;
        store.insert(duplicate).unwrap();
        assert!(
            store
                .rule_targets(input, &RuleOrigin::MANUAL, true)
                .suggested
                .is_none()
        );
        assert!(reviewed.apply(&mut store, true).is_err());
    }

    #[test]
    fn learned_rule_same_name_labels_distinguish_ids_without_changing_selection() {
        let mut store = Store::default();
        let first = store.insert(user("Local Rust", "rust")).unwrap();
        let second = store.insert(user("Local Rust", "rust")).unwrap();
        let input = "Rust: Use checked arithmetic.";
        let options = store.rule_targets(input, &RuleOrigin::MANUAL, false);
        let target = options.suggested.unwrap();
        assert_eq!(target.to_string(), format!("Skill: Local Rust [#{first}]"));
        assert_eq!(
            options.targets[1].to_string(),
            format!("Skill: Local Rust [#{second}]")
        );
        let reviewed = store
            .review_rule(&target, input, &RuleOrigin::MANUAL, false)
            .unwrap();
        store.skills.get_mut(&second).unwrap().enabled = false;
        let current = store
            .rule_targets(input, &RuleOrigin::MANUAL, false)
            .suggested
            .unwrap();
        assert_eq!(current, target);
        assert_eq!(current.to_string(), "Skill: Local Rust");
        reviewed.apply(&mut store, false).unwrap();
        assert_eq!(store.skills[&first].rules, [input]);
        assert!(store.skills[&second].rules.is_empty());
    }

    #[test]
    fn reviewed_chat_rule_uses_a_safe_tool_hint_and_persists_its_source() {
        let mut store = Store::default();
        let input = "Keep reactive declarations close to their inputs.";
        let origin = RuleOrigin {
            source: RuleSource::Chat,
            topic: Some("Svelte.".into()),
        };
        let target = store.rule_targets(input, &origin, true).suggested.unwrap();
        assert_eq!(target.to_string(), "New skill: Svelte (kortty)");
        assert!(store.rule_targets(input, &origin, false).targets.is_empty());
        assert!(
            store
                .rule_targets(
                    "Svelte: Keep declarations together.",
                    &RuleOrigin::MANUAL,
                    true
                )
                .targets
                .is_empty()
        );
        let rust = builtin(&store, "builtin.lang.rust");
        assert_eq!(
            store
                .rule_targets("Rust: Keep declarations together.", &origin, true)
                .suggested,
            Some(SkillRuleTarget::existing(rust, &store.skills[&rust]))
        );
        for topic in ["formatting", "svelte/path"] {
            let origin = RuleOrigin {
                source: RuleSource::Chat,
                topic: Some(topic.into()),
            };
            assert!(store.rule_targets(input, &origin, true).targets.is_empty());
        }
        let proposal = store.review_rule(&target, input, &origin, true).unwrap();
        let saved = proposal.apply(&mut store, true).unwrap();
        let encoded = serde_json::to_value(&store).unwrap();
        assert_eq!(
            encoded["skills"][saved.id.to_string()]["learned_rule_ids"][0]["source"],
            "chat"
        );
        let reopened: Store = serde_json::from_value(encoded).unwrap();
        reopened.validate().unwrap();
    }

    #[test]
    fn learned_rule_preview_checks_exact_skill_snapshot_and_undo_survives_reload() {
        let mut store = Store::default();
        let rust = builtin(&store, "builtin.lang.rust");
        let legacy = "Keep the legacy rule.";
        let mut original = store.skills[&rust].clone();
        original.rules.push(legacy.into());
        store.replace(rust, original.clone()).unwrap();
        let input = "Rust: Preserve the reviewed  output.\u{200b}";
        let target = store
            .rule_targets(input, &RuleOrigin::MANUAL, true)
            .suggested
            .unwrap();
        let first = store
            .review_rule(&target, input, &RuleOrigin::MANUAL, true)
            .unwrap();
        assert_eq!(first.rule(), "Rust: Preserve the reviewed output.");
        assert_eq!(first.current_text(), original.to_markdown());
        assert!(first.proposed_text().contains(first.rule()));
        assert!(!format!("{first:?}").contains(first.rule()));

        store
            .skills
            .get_mut(&rust)
            .unwrap()
            .description
            .push_str(" Later edit.");
        assert!(first.apply(&mut store, true).is_err());
        let next = store
            .review_rule(&target, input, &RuleOrigin::MANUAL, true)
            .unwrap();
        let saved = next.apply(&mut store, true).unwrap();
        assert!(next.apply(&mut store, true).is_err());
        assert!(
            store
                .review_rule(&target, input, &RuleOrigin::MANUAL, true)
                .is_err()
        );

        let encoded = serde_json::to_value(&store).unwrap();
        let persisted = &encoded["skills"][rust.to_string()];
        assert_eq!(persisted["rules"], serde_json::json!([legacy, next.rule()]));
        let identity = &persisted["learned_rule_ids"][0];
        assert_eq!(identity["source"], "manual");
        assert!(valid_id(identity["id"].as_str().unwrap()));
        assert_eq!(identity["sha256"], fingerprint(next.rule()).unwrap());
        assert!(identity["created_at"].as_str().unwrap().ends_with('Z'));

        let mut reopened: Store = serde_json::from_value(encoded).unwrap();
        reopened.validate().unwrap();
        let mut changed = reopened.skills[&rust].clone();
        changed.tags.push("review fixture".into());
        reopened.replace(rust, changed).unwrap();
        saved.undo(&mut reopened).unwrap();
        let current = &reopened.skills[&rust];
        assert_eq!(current.rules, [legacy]);
        assert!(current.tags.contains(&"review fixture".into()));
        assert!(current.learned_rule_ids.is_empty());
        assert!(
            serde_json::to_value(current)
                .unwrap()
                .get("learned_rule_ids")
                .is_none()
        );
        assert!(saved.undo(&mut reopened).is_err());
    }

    #[test]
    fn learned_rule_undo_deletes_only_unchanged_created_skill_and_keeps_edits() {
        let mut store = Store::default();
        let input = "Elixir: Prefer explicit timeouts.";
        let target = store
            .rule_targets(input, &RuleOrigin::MANUAL, true)
            .suggested
            .unwrap();
        let proposal = store
            .review_rule(&target, input, &RuleOrigin::MANUAL, true)
            .unwrap();
        let saved = proposal.apply(&mut store, true).unwrap();
        assert!(
            store
                .prompt_addendum("Elixir", Target::Chat)
                .contains(input)
        );
        saved.undo(&mut store).unwrap();
        assert!(!store.skills.contains_key(&saved.id));

        let proposal = store
            .review_rule(&target, input, &RuleOrigin::MANUAL, true)
            .unwrap();
        let saved = proposal.apply(&mut store, true).unwrap();
        assert_eq!(store.skills[&saved.id].origin.as_deref(), Some("KORTTY"));
        let mut changed = store.skills[&saved.id].clone();
        changed.content = "Keep this manual change.".into();
        store.replace(saved.id, changed).unwrap();
        saved.undo(&mut store).unwrap();
        assert_eq!(store.skills[&saved.id].content, "Keep this manual change.");
        assert!(store.skills[&saved.id].rules.is_empty());
    }

    #[test]
    fn learned_rule_undo_refuses_edited_text_and_validates_metadata() {
        let mut store = Store::default();
        let input = "Rust: Keep the reviewed rule.";
        let target = store
            .rule_targets(input, &RuleOrigin::MANUAL, true)
            .suggested
            .unwrap();
        let proposal = store
            .review_rule(&target, input, &RuleOrigin::MANUAL, true)
            .unwrap();
        let saved = proposal.apply(&mut store, true).unwrap();
        let mut changed = store.skills[&saved.id].clone();
        changed.rules[0] = "Keep a manual edit.".into();
        assert!(changed.check().is_err());
        store.replace(saved.id, changed).unwrap();
        assert!(saved.undo(&mut store).is_err());
        assert_eq!(store.skills[&saved.id].rules, ["Keep a manual edit."]);
        assert!(store.skills[&saved.id].learned_rule_ids.is_empty());
        assert!(
            store
                .review_rule(
                    &target,
                    "Rust: pass\u{200b}word: Winter2026!",
                    &RuleOrigin::MANUAL,
                    true
                )
                .is_err()
        );
        assert!(
            store
                .review_rule(
                    &target,
                    "Rust: Keep <!-- kortty:learned-rules -->.",
                    &RuleOrigin::MANUAL,
                    true
                )
                .is_err()
        );
    }
}
