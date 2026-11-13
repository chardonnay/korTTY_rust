//! Native AI skill library and bounded prompt selection.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

mod builtin;
mod editor;
mod external;
mod learning;
mod markdown;
mod relevance;
mod secret;
mod shorten;
pub(super) mod statistics;
pub(super) mod usage;

pub(super) use editor::Editor;
pub(crate) use editor::Message;
pub(crate) use learning::{Options, SavedSkillRule, SkillRuleProposal, SkillRuleTarget};
pub(crate) use usage::SkillUsageReceipt;

const MAX_SKILLS: usize = 256;
const MAX_SKILL_BYTES: usize = 64 * 1024;
const MAX_RULES: usize = 128;
const MAX_LIBRARY_BYTES: usize = 1024 * 1024;
const MAX_PROMPT_BYTES: usize = 16 * 1024;

static BUILTINS: LazyLock<Vec<Skill>> = LazyLock::new(builtin::all);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub(crate) enum Target {
    Chat,
    Agent,
    #[default]
    Both,
    Connection,
}

impl Target {
    const ALL: [Self; 4] = [Self::Chat, Self::Agent, Self::Both, Self::Connection];

    const fn value(self) -> &'static str {
        match self {
            Self::Chat => "CHAT",
            Self::Agent => "AGENT",
            Self::Both => "BOTH",
            Self::Connection => "CONNECTION",
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Chat => "Chat",
            Self::Agent => "Agent",
            Self::Both => "Chat and Agent",
            Self::Connection => "Assigned Connections",
        })
    }
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Skill {
    name: String,
    description: String,
    tags: Vec<String>,
    content: String,
    rules: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    learned_rule_ids: Vec<learning::RuleIdentity>,
    enabled: bool,
    target: Target,
    builtin: Option<String>,
    builtin_topics: Vec<String>,
    hidden: bool,
    origin: Option<String>,
    created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    external: Option<external::Source>,
}

impl fmt::Debug for Skill {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Skill([redacted])")
    }
}

impl Skill {
    fn available(&self) -> bool {
        self.enabled && !self.hidden && (!self.content.trim().is_empty() || !self.rules.is_empty())
    }

    fn bytes(&self) -> usize {
        self.name.len()
            + self.description.len()
            + self.content.len()
            + self.tags.iter().map(String::len).sum::<usize>()
            + self.rules.iter().map(String::len).sum::<usize>()
    }

    fn storage_bytes(&self) -> usize {
        self.bytes()
            + self
                .learned_rule_ids
                .iter()
                .map(learning::RuleIdentity::bytes)
                .sum::<usize>()
            + self.external.as_ref().map_or(0, external::Source::bytes)
    }

    fn check(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Enter a skill name.".into());
        }
        if self.storage_bytes() > MAX_SKILL_BYTES
            || self.name.len() > 256
            || self.description.len() > 4096
            || self.tags.len() > 64
            || self.rules.len() > MAX_RULES
            || self.tags.iter().any(|tag| tag.len() > 128)
        {
            return Err("The skill exceeds its size limit.".into());
        }
        let baseline = self.baseline();
        if self.builtin.is_some() && baseline.is_none()
            || self.builtin_topics.len() > 64
            || self.builtin_topics.iter().any(|topic| topic.len() > 128)
            || self
                .origin
                .as_deref()
                .is_some_and(|origin| origin != "KORTTY")
            || self
                .created_at
                .as_deref()
                .is_some_and(|date| !markdown::valid_date(date))
        {
            return Err("The skill metadata is invalid.".into());
        }
        let fields = [
            (
                "name",
                self.name.as_str(),
                baseline.map(|skill| skill.name.as_str()),
            ),
            (
                "description",
                self.description.as_str(),
                baseline.map(|skill| skill.description.as_str()),
            ),
            (
                "Markdown",
                self.content.as_str(),
                baseline.map(|skill| skill.content.as_str()),
            ),
        ]
        .into_iter()
        .chain(self.tags.iter().map(|tag| ("tags", tag.as_str(), None)))
        .chain(
            self.rules
                .iter()
                .map(|rule| ("learned rules", rule.as_str(), None)),
        );
        for (field, value, bundled) in fields {
            if value.chars().any(|ch| !storable(ch)) {
                return Err(format!(
                    "The skill {field} contains an unsupported control character."
                ));
            }
            if bundled != Some(value)
                && let Some(line) = secret::first_line(value)
            {
                return Err(format!(
                    "A possible secret was found in the skill {field}, line {line}."
                ));
            }
        }
        learning::check_identities(self)?;
        if let Some(source) = &self.external {
            if self.builtin.is_some() || self.origin.is_some() {
                return Err("An external skill cannot have bundled skill metadata.".into());
            }
            source.validate()?;
        }
        Ok(())
    }

    fn baseline(&self) -> Option<&'static Skill> {
        let id = self.builtin.as_ref()?;
        BUILTINS
            .iter()
            .find(|skill| skill.builtin.as_ref() == Some(id))
    }

    fn matches(&self, query: &str) -> bool {
        query.is_empty()
            || self.name.to_lowercase().contains(query)
            || self.description.to_lowercase().contains(query)
            || self
                .tags
                .iter()
                .any(|tag| tag.to_lowercase().contains(query))
    }
}

fn storable(ch: char) -> bool {
    matches!(ch, '\t' | '\n' | '\r') || (!ch.is_control() && !matches!(ch, '\u{fffe}' | '\u{ffff}'))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Store {
    enabled: bool,
    auto_detect: bool,
    learned_rules_enabled: bool,
    #[serde(default)]
    last_id: u64,
    // The compiled baseline remains available even after a user edits a bundled skill.
    skills: BTreeMap<u64, Skill>,
    #[serde(skip_serializing_if = "external::is_default_profiles")]
    external_providers: Vec<external::ProviderProfile>,
    #[serde(skip_serializing_if = "external::is_initial_provider_id")]
    external_provider_last_id: u64,
    #[serde(skip_serializing_if = "statistics_enabled_by_default")]
    usage_statistics: bool,
}

fn statistics_enabled_by_default(value: &bool) -> bool {
    *value
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct SkillCandidate<'a> {
    pub(crate) id: u64,
    pub(crate) name: &'a str,
    pub(crate) description: &'a str,
    pub(crate) tags: &'a [String],
    pub(crate) target: &'static str,
}

impl fmt::Debug for SkillCandidate<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SkillCandidate")
            .field("id", &self.id)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

pub(crate) struct SkillSelection<'a> {
    pinned: Vec<(u64, &'a Skill)>,
    automatic: AutomaticSelection<'a>,
}

enum AutomaticSelection<'a> {
    All(Vec<(u64, &'a Skill)>),
    Ranked(Vec<relevance::ScoredSkill<'a>>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct IncludedSkill {
    id: u64,
    pinned: bool,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct SkillGuidance {
    pub(crate) text: String,
    included: Vec<IncludedSkill>,
}

impl fmt::Debug for SkillGuidance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SkillGuidance")
            .field("skill_count", &self.included.len())
            .finish_non_exhaustive()
    }
}

impl SkillGuidance {
    pub(crate) fn append(&mut self, additional: Self) {
        self.text.push_str(&additional.text);
        self.included.extend(additional.included);
    }
}

impl<'skill> SkillSelection<'skill> {
    pub(crate) fn classifier_candidates(&self) -> Option<Vec<SkillCandidate<'_>>> {
        let AutomaticSelection::Ranked(ranked) = &self.automatic else {
            return None;
        };
        if !relevance::should_classify(ranked) {
            return None;
        }
        let mut candidates: Vec<_> = ranked
            .iter()
            .map(|candidate| SkillCandidate {
                id: candidate.id,
                name: &candidate.skill.name,
                description: &candidate.skill.description,
                tags: &candidate.skill.tags,
                target: candidate.skill.target.value(),
            })
            .collect();
        candidates.sort_unstable_by_key(|candidate| candidate.id);
        Some(candidates)
    }

    #[cfg(test)]
    pub(crate) fn guidance(&self, classified: &[u64]) -> String {
        self.reviewed_guidance(classified).text
    }

    #[cfg(test)]
    pub(crate) fn automatic_guidance(&self, classified: &[u64]) -> String {
        self.reviewed_automatic_guidance(classified).text
    }

    pub(crate) fn reviewed_guidance(&self, classified: &[u64]) -> SkillGuidance {
        self.guidance_with_pins(self.pinned.iter().copied(), classified)
    }

    pub(crate) fn reviewed_automatic_guidance(&self, classified: &[u64]) -> SkillGuidance {
        self.guidance_with_pins(std::iter::empty(), classified)
    }

    fn guidance_with_pins(
        &self,
        pinned: impl Iterator<Item = (u64, &'skill Skill)>,
        classified: &[u64],
    ) -> SkillGuidance {
        let pinned = pinned.map(|(id, skill)| (id, skill, true));
        let ranked = match &self.automatic {
            AutomaticSelection::All(skills) => {
                return prompt(pinned.chain(skills.iter().map(|&(id, skill)| (id, skill, false))));
            }
            AutomaticSelection::Ranked(ranked) => ranked,
        };
        let mut selected = self.classified_matches(classified).peekable();
        if selected.peek().is_some() {
            return prompt(
                pinned.chain(selected.map(|candidate| (candidate.id, candidate.skill, false))),
            );
        }
        prompt(
            pinned.chain(
                relevance::strong_matches(ranked)
                    .iter()
                    .take(relevance::MAX_AUTOMATIC_SKILLS)
                    .map(|candidate| (candidate.id, candidate.skill, false)),
            ),
        )
    }

    pub(crate) fn classified_ids(&self, classified: &[u64]) -> Vec<u64> {
        self.classified_matches(classified)
            .map(|candidate| candidate.id)
            .collect()
    }

    fn classified_matches<'a>(
        &'a self,
        classified: &'a [u64],
    ) -> impl Iterator<Item = &'a relevance::ScoredSkill<'skill>> {
        let ranked = match &self.automatic {
            AutomaticSelection::Ranked(ranked)
                if !classified.is_empty() && relevance::should_classify(ranked) =>
            {
                ranked.as_slice()
            }
            _ => &[],
        };
        ranked
            .iter()
            .filter(move |candidate| classified.contains(&candidate.id))
            .take(relevance::MAX_AUTOMATIC_SKILLS)
    }
}

impl Default for Store {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_detect: true,
            learned_rules_enabled: true,
            last_id: BUILTINS.len() as u64,
            skills: BUILTINS
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, skill)| (index as u64 + 1, skill))
                .collect(),
            external_providers: external::default_profiles(),
            external_provider_last_id: external::INITIAL_PROVIDER_ID,
            usage_statistics: true,
        }
    }
}

impl Store {
    pub(super) fn same_definitions(&self, other: &Self) -> bool {
        self.skills == other.skills
    }

    pub(super) fn statistics_enabled(&self, policy: Option<bool>) -> bool {
        policy.unwrap_or(self.usage_statistics)
    }

    pub(super) fn migrate_ids(&mut self) {
        if let Some((&largest, _)) = self.skills.last_key_value() {
            self.last_id = self.last_id.max(largest);
        }
        self.external_provider_last_id = self.external_provider_last_id.max(
            self.external_providers
                .iter()
                .map(|provider| provider.id)
                .chain(
                    self.skills
                        .values()
                        .filter_map(|skill| skill.external.as_ref()?.provider_id()),
                )
                .max()
                .unwrap_or_default(),
        );
    }

    pub(super) fn learned_rules_enabled(&self) -> bool {
        self.learned_rules_enabled
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        external::validate_profiles(&self.external_providers)?;
        if self.skills.contains_key(&0) {
            return Err("The AI skill library contains an invalid ID.".into());
        }
        if self.skills.len() > MAX_SKILLS
            || self
                .skills
                .values()
                .map(Skill::storage_bytes)
                .sum::<usize>()
                > MAX_LIBRARY_BYTES
        {
            return Err("The AI skill library exceeds its size limit.".into());
        }
        for skill in self.skills.values() {
            if skill.baseline() != Some(skill) {
                skill.check()?;
            }
        }
        Ok(())
    }

    fn insert(&mut self, skill: Skill) -> Result<u64, String> {
        let largest = self.skills.last_key_value().map_or(0, |(&id, _)| id);
        let id = self
            .last_id
            .max(largest)
            .checked_add(1)
            .ok_or("The skill library has no free IDs.")?;
        self.replace(id, skill)?;
        Ok(id)
    }

    fn replace(&mut self, id: u64, mut skill: Skill) -> Result<(), String> {
        if id == 0 {
            return Err("The AI skill ID is invalid.".into());
        }
        learning::prune_identities(&mut skill)?;
        if skill.baseline() != Some(&skill) {
            skill.check()?;
        }
        let total = self
            .skills
            .values()
            .map(Skill::storage_bytes)
            .sum::<usize>()
            - self.skills.get(&id).map_or(0, Skill::storage_bytes);
        if (!self.skills.contains_key(&id) && self.skills.len() >= MAX_SKILLS)
            || total + skill.storage_bytes() > MAX_LIBRARY_BYTES
        {
            return Err("The AI skill library exceeds its size limit.".into());
        }
        self.skills.insert(id, skill);
        self.last_id = self.last_id.max(id);
        Ok(())
    }

    /// Supplies skill instructions for an explicitly requested AI operation.
    #[cfg(test)]
    pub(super) fn prompt_addendum(&self, context: &str, target: Target) -> String {
        self.prompt_addendum_selected(context, target, &[])
    }

    pub(super) fn choices(&self) -> Vec<(u64, String)> {
        self.skills
            .iter()
            .filter(|(_, skill)| skill.enabled && !skill.hidden && skill.target != Target::Agent)
            .map(|(id, skill)| (*id, skill.name.clone()))
            .collect()
    }

    #[cfg(test)]
    pub(super) fn prompt_addendum_selected(
        &self,
        context: &str,
        target: Target,
        assigned: &[u64],
    ) -> String {
        self.selection(context, target, assigned).guidance(&[])
    }

    pub(super) fn selection(
        &self,
        context: &str,
        target: Target,
        assigned: &[u64],
    ) -> SkillSelection<'_> {
        if !self.enabled {
            return SkillSelection {
                pinned: Vec::new(),
                automatic: AutomaticSelection::All(Vec::new()),
            };
        }
        let mut seen = HashSet::new();
        let pinned = self.pinned_skills(target, assigned, &mut seen);
        let candidates = self.skills.iter().filter_map(|(&id, skill)| {
            (skill.available()
                && skill.target != Target::Connection
                && (skill.target == Target::Both || skill.target == target)
                && !seen.contains(&id))
            .then_some((id, skill))
        });
        let automatic = if self.auto_detect {
            AutomaticSelection::Ranked(relevance::scored(context, candidates))
        } else {
            AutomaticSelection::All(candidates.collect())
        };
        SkillSelection { pinned, automatic }
    }

    pub(super) fn reviewed_pinned_guidance(
        &self,
        target: Target,
        assigned: &[u64],
    ) -> SkillGuidance {
        if !self.enabled {
            return SkillGuidance::default();
        }
        prompt(
            self.pinned_skills(target, assigned, &mut HashSet::new())
                .into_iter()
                .map(|(id, skill)| (id, skill, true)),
        )
    }

    fn pinned_skills(
        &self,
        target: Target,
        assigned: &[u64],
        seen: &mut HashSet<u64>,
    ) -> Vec<(u64, &Skill)> {
        assigned
            .iter()
            .filter_map(|id| {
                let skill = self.skills.get(id)?;
                // Java Agent requests honor explicit pins independently of the automatic scope.
                (skill.available()
                    && (target == Target::Agent
                        || skill.target == target
                        || matches!(skill.target, Target::Both | Target::Connection))
                    && seen.insert(*id))
                .then_some((*id, skill))
            })
            .collect()
    }

    pub(super) fn snippet_choices(&self) -> Vec<(u64, String)> {
        if !self.enabled {
            return Vec::new();
        }
        self.skills
            .iter()
            .filter(|(_, skill)| skill.available())
            .map(|(&id, skill)| (id, skill.name.clone()))
            .collect()
    }

    #[cfg(test)]
    pub(super) fn snippet_prompt(&self, selected: &[u64]) -> String {
        self.reviewed_snippet_prompt(selected).text
    }

    pub(super) fn reviewed_snippet_prompt(&self, selected: &[u64]) -> SkillGuidance {
        if !self.enabled {
            return SkillGuidance::default();
        }
        let mut seen = HashSet::new();
        prompt(selected.iter().filter_map(|id| {
            let skill = self.skills.get(id)?;
            (skill.available() && seen.insert(*id)).then_some((*id, skill, false))
        }))
    }

    pub(super) fn suggest_snippet_skills(&self, language: &str, context: &str) -> Vec<u64> {
        if !self.enabled || context.trim().is_empty() {
            return Vec::new();
        }
        let candidates = self.skills.iter().filter(|(_, skill)| skill.available());
        let exact: Vec<_> = candidates
            .clone()
            .filter_map(|(&id, skill)| {
                skill
                    .tags
                    .iter()
                    .chain(&skill.builtin_topics)
                    .any(|tag| tag.eq_ignore_ascii_case(language))
                    .then_some(id)
            })
            .take(relevance::MAX_AUTOMATIC_SKILLS)
            .collect();
        if !exact.is_empty() {
            return exact;
        }
        relevance::ranked(
            context,
            candidates
                .filter(|(_, skill)| matches!(skill.target, Target::Chat | Target::Both))
                .map(|(&id, skill)| (id, skill)),
        )
        .into_iter()
        .map(|(id, _)| id)
        .collect()
    }
}

fn prompt<'a>(skills: impl Iterator<Item = (u64, &'a Skill, bool)>) -> SkillGuidance {
    let mut output = SkillGuidance::default();
    for (id, skill, pinned) in skills {
        let rules = skill.rules.join("\n- ");
        let addition = if rules.is_empty() {
            format!("\n\n## {}\n{}", skill.name, skill.content)
        } else {
            format!(
                "\n\n## {}\n{}\n\nLearned rules\n- {rules}",
                skill.name, skill.content
            )
        };
        if output.text.len() + addition.len() <= MAX_PROMPT_BYTES {
            output.text.push_str(&addition);
            output.included.push(IncludedSkill { id, pinned });
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_skills_use_java_topics_and_require_whole_topic_matches() {
        let store = Store::default();
        assert_eq!(store.skills.len(), 39);
        store.validate().unwrap();
        assert!(
            store
                .prompt_addendum("Review this Rust code.", Target::Chat)
                .contains("# Rust Best Practices")
        );
        assert!(
            !store
                .prompt_addendum("Use a trustworthy method.", Target::Chat)
                .contains("# Rust Best Practices")
        );
    }

    #[test]
    fn assignment_only_and_disabled_skills_never_enter_an_unassigned_prompt() {
        let mut store = Store {
            enabled: true,
            auto_detect: false,
            skills: BTreeMap::new(),
            ..Store::default()
        };
        let mut skill = Skill {
            name: "My instructions".into(),
            content: "Prefer short names.".into(),
            enabled: true,
            target: Target::Connection,
            ..Skill::default()
        };
        let id = store.insert(skill.clone()).unwrap();
        assert!(store.prompt_addendum("connection", Target::Chat).is_empty());
        assert!(
            store
                .prompt_addendum_selected("unrelated topic", Target::Chat, &[id])
                .contains("Prefer short names.")
        );
        assert!(
            store
                .prompt_addendum_selected("unrelated topic", Target::Chat, &[])
                .is_empty()
        );
        skill.target = Target::Both;
        store.replace(id, skill).unwrap();
        assert!(
            store
                .prompt_addendum("connection", Target::Chat)
                .contains("Prefer short names.")
        );
        store.enabled = false;
        assert!(store.prompt_addendum("connection", Target::Chat).is_empty());
    }

    #[test]
    fn ranked_relevance_applies_two_automatic_skills_after_scoped_pins() {
        let skill = |name: &str, content: &str, tags: &[&str], target| Skill {
            name: name.into(),
            content: content.into(),
            tags: tags.iter().map(|tag| (*tag).into()).collect(),
            enabled: true,
            target,
            ..Skill::default()
        };
        let store = Store {
            skills: BTreeMap::from([
                (
                    1,
                    skill(
                        "Weak match",
                        "Weak guidance",
                        &["cache retention"],
                        Target::Chat,
                    ),
                ),
                (
                    2,
                    skill("Cache", "First strong guidance", &["cache"], Target::Chat),
                ),
                (
                    3,
                    skill("Cache", "Second strong guidance", &["cache"], Target::Both),
                ),
                (
                    4,
                    skill("Cache", "Agent-only guidance", &["cache"], Target::Agent),
                ),
                (
                    5,
                    skill("Connection", "Pinned guidance", &[], Target::Connection),
                ),
                (
                    6,
                    skill(
                        "Cache",
                        "Unassigned connection guidance",
                        &["cache"],
                        Target::Connection,
                    ),
                ),
            ]),
            ..Store::default()
        };
        let result = store.prompt_addendum_selected("Review the cache", Target::Chat, &[5, 4, 5]);
        let first = result.find("First strong guidance").unwrap();
        let second = result.find("Second strong guidance").unwrap();
        assert!(result.find("Pinned guidance").unwrap() < first && first < second);
        assert_eq!(result.matches("Pinned guidance").count(), 1);
        for excluded in [
            "Weak guidance",
            "Agent-only guidance",
            "Unassigned connection guidance",
        ] {
            assert!(!result.contains(excluded));
        }
        assert!(result.len() <= MAX_PROMPT_BYTES);
    }

    #[test]
    fn classifier_metadata_and_guidance_share_the_scoped_automatic_candidates() {
        let skill = |tag: &str, content: &str, target| Skill {
            name: "Guidance".into(),
            description: "General description".into(),
            tags: vec![tag.into()],
            content: content.into(),
            target,
            enabled: true,
            ..Skill::default()
        };
        let mut hidden = skill("alpha", "Hidden body", Target::Both);
        hidden.hidden = true;
        let mut disabled = skill("alpha", "Disabled body", Target::Chat);
        disabled.enabled = false;
        let mut store = Store {
            skills: BTreeMap::from([
                (1, skill("alpha", "First body", Target::Chat)),
                (2, skill("beta", "Second body", Target::Both)),
                (3, skill("gamma", "Weak body", Target::Chat)),
                (4, skill("alpha", "Chat pin body", Target::Chat)),
                (5, skill("alpha", "Connection pin body", Target::Connection)),
                (6, skill("alpha", "Unassigned body", Target::Connection)),
                (7, skill("alpha", "Agent body", Target::Agent)),
                (8, hidden),
                (9, disabled),
            ]),
            ..Store::default()
        };
        let pins = [5, 4, 5, 7, 8, 9];
        let selection = store.selection("alpha beta", Target::Chat, &pins);
        let candidates = selection.classifier_candidates().unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.id)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(candidates[0].name, "Guidance");
        assert_eq!(candidates[0].description, "General description");
        assert_eq!(candidates[0].tags, ["alpha"]);
        assert_eq!(candidates[0].target, "CHAT");
        assert_eq!(candidates[1].target, "BOTH");

        let local = selection.guidance(&[]);
        assert!(
            selection
                .classified_ids(&[4, 5, 6, 7, 8, 9, 999])
                .is_empty()
        );
        assert_eq!(selection.guidance(&[4, 5, 6, 7, 8, 9, 999]), local);
        assert_eq!(selection.classified_ids(&[3, 2, 1, 2, 999]), [1, 2]);
        assert_eq!(selection.guidance(&[3, 2, 1, 2, 999]), local);
        assert_eq!(selection.classified_ids(&[3, 2, 2, 4, 5, 7, 999]), [2, 3]);
        let classified = selection.guidance(&[3, 2, 2, 4, 5, 7, 999]);
        let connection = classified.find("Connection pin body").unwrap();
        let chat = classified.find("Chat pin body").unwrap();
        let second = classified.find("Second body").unwrap();
        let weak = classified.find("Weak body").unwrap();
        assert!(connection < chat && chat < second && second < weak);
        assert_eq!(classified.matches("Second body").count(), 1);
        assert_eq!(classified.matches("Connection pin body").count(), 1);
        for excluded in [
            "First body",
            "Unassigned body",
            "Agent body",
            "Hidden body",
            "Disabled body",
        ] {
            assert!(!classified.contains(excluded));
        }

        store.auto_detect = false;
        let all = store.selection("alpha beta", Target::Chat, &pins);
        assert!(all.classifier_candidates().is_none());
        assert!(all.classified_ids(&[2]).is_empty());
        assert_eq!(all.guidance(&[2]), all.guidance(&[]));
        assert!(all.guidance(&[]).contains("Weak body"));

        store.enabled = false;
        let disabled = store.selection("alpha beta", Target::Chat, &pins);
        assert!(disabled.classifier_candidates().is_none());
        assert!(disabled.guidance(&[2]).is_empty());
    }

    #[test]
    fn agent_automatic_scope_and_connection_pins_render_separately() {
        let skill = |tag: &str, content: &str, target| Skill {
            name: format!("{tag} guidance"),
            tags: vec![tag.into()],
            content: content.into(),
            target,
            enabled: true,
            ..Skill::default()
        };
        let mut hidden = skill("redis", "Hidden body", Target::Agent);
        hidden.hidden = true;
        let mut disabled = skill("redis", "Disabled body", Target::Both);
        disabled.enabled = false;
        let store = Store {
            skills: BTreeMap::from([
                (1, skill("redis", "Agent body", Target::Agent)),
                (2, skill("database", "Shared body", Target::Both)),
                (3, skill("redis", "Chat pin body", Target::Chat)),
                (4, skill("redis", "Connection pin body", Target::Connection)),
                (5, skill("redis", "Unassigned body", Target::Connection)),
                (6, hidden),
                (7, disabled),
                (8, skill("redis", "Chat body", Target::Chat)),
                (9, skill("redis", "Agent pin body", Target::Agent)),
            ]),
            ..Store::default()
        };
        let assigned = [4, 3, 9, 4, 6, 7];
        let selection = store.selection("Check redis", Target::Agent, &assigned);
        let candidates = selection.classifier_candidates().unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|item| (item.id, item.target))
                .collect::<Vec<_>>(),
            [(1, "AGENT"), (2, "BOTH")]
        );
        assert_eq!(selection.classified_ids(&[2, 2, 4, 8, 9, 123]), [2]);
        let automatic = selection.automatic_guidance(&[2, 2, 4, 8, 9, 123]);
        assert!(automatic.contains("Shared body"));
        assert!(!automatic.contains("Agent body"));
        assert!(!automatic.contains("pin body"));
        assert!(
            selection
                .automatic_guidance(&[4, 8, 9])
                .contains("Agent body")
        );
        let pinned = store
            .reviewed_pinned_guidance(Target::Agent, &assigned)
            .text;
        let connection = pinned.find("Connection pin body").unwrap();
        let chat = pinned.find("Chat pin body").unwrap();
        let agent = pinned.find("Agent pin body").unwrap();
        assert!(connection < chat && chat < agent);
        assert_eq!(pinned.matches("Connection pin body").count(), 1);
        for excluded in ["Hidden body", "Disabled body", "Unassigned body"] {
            assert!(!pinned.contains(excluded));
        }
        assert!(
            !store
                .reviewed_pinned_guidance(Target::Chat, &assigned)
                .text
                .contains("Agent pin body")
        );
    }

    #[test]
    fn snippet_choices_are_an_explicit_allowlist_with_language_based_defaults() {
        let mut store = Store {
            auto_detect: false,
            ..Store::default()
        };
        let suggested = store.suggest_snippet_skills("rust", "#!/bin/bash\nset -eu");
        assert!(!suggested.is_empty());
        let rust = store.snippet_prompt(&suggested);
        assert!(rust.contains("Rust Best Practices"));
        assert!(!rust.contains("Bash Best Practices"));
        assert!(store.snippet_prompt(&[]).is_empty());

        let agent = store
            .insert(Skill {
                name: "Agent-scoped skill".into(),
                content: "Explicit snippet instructions.".into(),
                enabled: true,
                target: Target::Agent,
                ..Skill::default()
            })
            .unwrap();
        assert!(store.snippet_choices().iter().any(|(id, _)| *id == agent));
        assert!(!store.choices().iter().any(|(id, _)| *id == agent));
        assert!(
            store
                .snippet_prompt(&[agent])
                .contains("Explicit snippet instructions.")
        );
        assert!(
            !store
                .prompt_addendum_selected("", Target::Chat, &[agent])
                .contains("Explicit snippet instructions.")
        );
        store.skills.get_mut(&agent).unwrap().hidden = true;
        assert!(store.snippet_prompt(&[agent]).is_empty());
    }

    #[test]
    fn migrated_skill_ids_are_not_reused_after_deletion_or_restart() {
        let mut store: Store = serde_json::from_value(serde_json::json!({
            "skills": { "73": { "name": "Old assignment", "enabled": true, "target": "CONNECTION" } }
        })).unwrap();
        store.migrate_ids();
        assert_eq!(store.last_id, 73);
        store.skills.remove(&73).unwrap();
        let mut reloaded: Store =
            serde_json::from_slice(&serde_json::to_vec(&store).unwrap()).unwrap();
        let replacement = Skill {
            name: "New assignment".into(),
            content: "Use unique instructions.".into(),
            enabled: true,
            target: Target::Connection,
            ..Skill::default()
        };
        assert_eq!(reloaded.insert(replacement.clone()).unwrap(), 74);
        assert!(
            reloaded
                .prompt_addendum_selected("context", Target::Chat, &[73])
                .is_empty()
        );
        assert!(
            reloaded
                .prompt_addendum_selected("context", Target::Chat, &[74])
                .contains("Use unique instructions.")
        );
        reloaded.last_id = u64::MAX;
        let before = reloaded.clone();
        assert!(reloaded.insert(replacement).is_err());
        assert_eq!(reloaded, before);
    }
}
