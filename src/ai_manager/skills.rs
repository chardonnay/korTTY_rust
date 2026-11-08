//! Native AI skill library and bounded prompt selection.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

mod builtin;
mod editor;
mod markdown;
mod secret;
mod shorten;

pub(super) use editor::Editor;
pub(crate) use editor::Message;

const MAX_SKILLS: usize = 256;
const MAX_SKILL_BYTES: usize = 64 * 1024;
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
    enabled: bool,
    target: Target,
    builtin: Option<String>,
    builtin_topics: Vec<String>,
    hidden: bool,
    origin: Option<String>,
    created_at: Option<String>,
}

impl fmt::Debug for Skill {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Skill([redacted])")
    }
}

impl Skill {
    fn bytes(&self) -> usize {
        self.name.len()
            + self.description.len()
            + self.content.len()
            + self.tags.iter().map(String::len).sum::<usize>()
            + self.rules.iter().map(String::len).sum::<usize>()
    }

    fn check(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Enter a skill name.".into());
        }
        if self.bytes() > MAX_SKILL_BYTES
            || self.name.len() > 256
            || self.description.len() > 4096
            || self.tags.len() > 64
            || self.rules.len() > 128
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
    // The compiled baseline remains available even after a user edits a bundled skill.
    skills: BTreeMap<u64, Skill>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_detect: true,
            skills: BUILTINS
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, skill)| (index as u64 + 1, skill))
                .collect(),
        }
    }
}

impl Store {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.skills.len() > MAX_SKILLS
            || self.skills.values().map(Skill::bytes).sum::<usize>() > MAX_LIBRARY_BYTES
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
        let id = (1..)
            .find(|id| !self.skills.contains_key(id))
            .ok_or("The skill library has no free IDs.")?;
        self.replace(id, skill)?;
        Ok(id)
    }

    fn replace(&mut self, id: u64, skill: Skill) -> Result<(), String> {
        if skill.baseline() != Some(&skill) {
            skill.check()?;
        }
        let total = self.skills.values().map(Skill::bytes).sum::<usize>()
            - self.skills.get(&id).map_or(0, Skill::bytes);
        if (!self.skills.contains_key(&id) && self.skills.len() >= MAX_SKILLS)
            || total + skill.bytes() > MAX_LIBRARY_BYTES
        {
            return Err("The AI skill library exceeds its size limit.".into());
        }
        self.skills.insert(id, skill);
        Ok(())
    }

    /// Supplies skill instructions for an explicitly requested AI operation.
    pub(super) fn prompt_addendum(&self, context: &str, target: Target) -> String {
        if !self.enabled {
            return String::new();
        }
        let context = context.to_lowercase();
        let mut output = String::new();
        for skill in self.skills.values() {
            if !skill.enabled
                || skill.hidden
                || skill.target == Target::Connection
                || (skill.target != Target::Both && skill.target != target)
                || (self.auto_detect
                    && !skill
                        .tags
                        .iter()
                        .chain(&skill.builtin_topics)
                        .any(|tag| contains_topic(&context, tag)))
            {
                continue;
            }
            let rules = skill.rules.join("\n- ");
            let addition = if rules.is_empty() {
                format!("\n\n## {}\n{}", skill.name, skill.content)
            } else {
                format!(
                    "\n\n## {}\n{}\n\nLearned rules\n- {rules}",
                    skill.name, skill.content
                )
            };
            if output.len() + addition.len() <= MAX_PROMPT_BYTES {
                output.push_str(&addition);
            }
        }
        output
    }
}

fn contains_topic(context: &str, tag: &str) -> bool {
    let tag = tag.to_lowercase();
    if tag.is_empty() {
        return false;
    }
    context.match_indices(&tag).any(|(start, _)| {
        let word = |ch: char| ch.is_alphanumeric() || ch == '_';
        !context[..start].chars().next_back().is_some_and(word)
            && !context[start + tag.len()..]
                .chars()
                .next()
                .is_some_and(word)
    })
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
}
