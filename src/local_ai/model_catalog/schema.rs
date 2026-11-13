//! Schema version 1 of the korTTY model and prompt catalog.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::io;

use serde::Deserialize;

use super::super::prompt_preset::PromptPreset;
use super::invalid;

const MAX_RECOMMENDATIONS: usize = 256;
const MAX_FAMILIES: usize = 64;
const MAX_MEMORY: u64 = 1 << 50;
const MAX_ALIASES: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Catalog {
    schema_version: u8,
    sequence: u64,
    catalog_version: String,
    recommendations: Vec<Recommendation>,
    prompt_families: Vec<PromptFamily>,
}

impl Catalog {
    pub(super) fn parse(bytes: &[u8]) -> io::Result<Self> {
        let mut catalog: Self = serde_json::from_slice(bytes)
            .map_err(|_| invalid("The AI catalog schema is invalid."))?;
        if catalog.schema_version != 1 {
            return Err(invalid("The AI catalog schema version is unsupported."));
        }
        if !(1..=i64::MAX as u64).contains(&catalog.sequence)
            || !clean_text(&mut catalog.catalog_version, 128)
            || !(1..=MAX_RECOMMENDATIONS).contains(&catalog.recommendations.len())
            || !(1..=MAX_FAMILIES).contains(&catalog.prompt_families.len())
        {
            return Err(invalid(
                "The AI catalog has invalid version or collection bounds.",
            ));
        }
        for recommendation in &mut catalog.recommendations {
            recommendation.normalize()?;
        }
        let mut ids = HashSet::new();
        if !catalog
            .recommendations
            .iter()
            .all(|entry| ids.insert(&entry.id))
        {
            return Err(invalid("The AI catalog has duplicate recommendation IDs."));
        }
        if ![Role::Text, Role::Coding, Role::Embedding]
            .into_iter()
            .all(|role| {
                catalog.recommendations.iter().any(|entry| {
                    entry.minimum_system_memory_bytes == 0 && entry.roles.contains(&role)
                })
            })
        {
            return Err(invalid(
                "The AI catalog has no baseline for one or more model roles.",
            ));
        }
        for family in &mut catalog.prompt_families {
            family.normalize()?;
        }
        ids.clear();
        if !catalog
            .prompt_families
            .iter()
            .all(|entry| ids.insert(&entry.id))
        {
            return Err(invalid("The AI catalog has duplicate prompt-family IDs."));
        }
        catalog.prompt_families.sort_by(|left, right| {
            (Reverse(left.priority), &left.id).cmp(&(Reverse(right.priority), &right.id))
        });
        Ok(catalog)
    }

    pub(crate) fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(crate) fn version(&self) -> &str {
        &self.catalog_version
    }

    pub(crate) fn recommendations(&self) -> &[Recommendation] {
        &self.recommendations
    }

    pub(crate) fn preset_for(&self, model: &str) -> Option<PromptPreset> {
        let model = model.to_lowercase();
        self.prompt_families.iter().find_map(|entry| {
            entry
                .model_name_contains
                .iter()
                .any(|alias| model.contains(alias))
                .then(|| entry.preset.value())
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub(crate) enum Role {
    #[default]
    Text,
    Coding,
    Embedding,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Recommendation {
    pub(crate) id: String,
    pub(crate) model_id: String,
    pub(crate) revision: Option<String>,
    pub(crate) quantization: String,
    pub(crate) roles: Vec<Role>,
    pub(crate) minimum_system_memory_bytes: u64,
    pub(crate) preference: i32,
}

impl Recommendation {
    fn normalize(&mut self) -> io::Result<()> {
        if !identifier(&mut self.id, 128)
            || !clean_text(&mut self.model_id, 192)
            || !self
                .model_id
                .split_once('/')
                .is_some_and(|(namespace, repository)| {
                    repository_segment(namespace) && repository_segment(repository)
                })
            || !identifier(&mut self.quantization, 64)
            || self.roles.is_empty()
            || self.minimum_system_memory_bytes > MAX_MEMORY
            || !(-10_000..=10_000).contains(&self.preference)
        {
            return Err(invalid(
                "The AI catalog has an invalid model recommendation.",
            ));
        }
        self.roles.sort_unstable();
        self.roles.dedup();
        if let Some(revision) = self.revision.take() {
            let revision = revision.trim();
            if !revision.is_empty() {
                if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(invalid("The AI catalog has an invalid model revision."));
                }
                self.revision = Some(revision.to_ascii_lowercase());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PromptFamily {
    id: String,
    preset: ConcretePreset,
    model_name_contains: Vec<String>,
    priority: i32,
}

impl PromptFamily {
    fn normalize(&mut self) -> io::Result<()> {
        if !identifier(&mut self.id, 128)
            || !(-10_000..=10_000).contains(&self.priority)
            || self.model_name_contains.is_empty()
        {
            return Err(invalid(
                "The AI catalog has an invalid prompt-family mapping.",
            ));
        }
        for alias in &mut self.model_name_contains {
            if !clean_text(alias, 64) {
                return Err(invalid("The AI catalog has an invalid model-name alias."));
            }
            *alias = alias.to_lowercase();
        }
        let mut unique = HashSet::new();
        self.model_name_contains
            .retain(|alias| unique.insert(alias.clone()));
        if self.model_name_contains.len() > MAX_ALIASES {
            return Err(invalid(
                "The AI catalog has too many aliases for a prompt family.",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ConcretePreset {
    Llama,
    Qwen,
    Mistral,
    Gemma,
    #[serde(rename = "DEEPSEEK")]
    DeepSeek,
    Phi,
    GptOss,
}

impl ConcretePreset {
    fn value(self) -> PromptPreset {
        match self {
            Self::Llama => PromptPreset::Llama,
            Self::Qwen => PromptPreset::Qwen,
            Self::Mistral => PromptPreset::Mistral,
            Self::Gemma => PromptPreset::Gemma,
            Self::DeepSeek => PromptPreset::DeepSeek,
            Self::Phi => PromptPreset::Phi,
            Self::GptOss => PromptPreset::GptOss,
        }
    }
}

fn clean_text(value: &mut String, max_utf16: usize) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.encode_utf16().count() > max_utf16
        || trimmed.chars().any(|character| {
            character.is_control() || !crate::session_journal::is_display_character(character)
        })
    {
        return false;
    }
    if trimmed.len() != value.len() {
        *value = trimmed.into();
    }
    true
}

fn identifier(value: &mut String, limit: usize) -> bool {
    clean_text(value, limit)
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value.bytes().all(identifier_byte)
}

fn repository_segment(value: &str) -> bool {
    !matches!(value, "" | "." | "..") && value.bytes().all(identifier_byte)
}

fn identifier_byte(value: u8) -> bool {
    value.is_ascii_alphanumeric() || matches!(value, b'.' | b'_' | b'-')
}
