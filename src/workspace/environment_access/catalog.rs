use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{profile, ssh_dialog::unsafe_text};

pub(super) use crate::connection_store::TabColor as Rgb;

const VERSION: u32 = 1;
const MAX_BYTES: u64 = 64 * 1024;
const MAX_CUSTOM: usize = 128;
pub(super) const MAX_NAME: usize = 128;
pub(crate) const DEFAULT_ENVIRONMENT: &str = "PRODUCTION";

const BUILT_INS: [(&str, &str); 4] = [
    (DEFAULT_ENVIRONMENT, "Production"),
    ("DEVELOPMENT", "Development"),
    ("TEST", "Test"),
    ("STAGING", "Staging"),
];

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    id: String,
    label: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Catalog {
    version: u32,
    custom: Vec<Definition>,
    colors: BTreeMap<String, Rgb>,
}

#[derive(Clone, Copy)]
pub(super) struct Entry<'a> {
    pub(super) id: &'a str,
    pub(super) label: &'a str,
    pub(super) built_in: bool,
    pub(super) color: Option<Rgb>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Choice {
    pub(crate) id: String,
    label: String,
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            version: VERSION,
            custom: Vec::new(),
            colors: BTreeMap::new(),
        }
    }
}

impl Catalog {
    pub(super) fn read(path: &Path) -> io::Result<Self> {
        let file = match profile::open_preference(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        if !file.metadata()?.is_file() {
            return Err(invalid("environment catalog requires a regular file"));
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        Self::decode(&bytes)
    }

    pub(in crate::workspace) fn validate_backup(bytes: &[u8]) -> io::Result<()> {
        Self::decode(bytes).map(|_| ())
    }

    fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("environment catalog exceeds its size limit"));
        }
        let catalog: Self = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub(super) fn save(&self, path: &Path, previous: &Self) -> io::Result<()> {
        self.validate()?;
        if Self::read(path)? != *previous {
            return Err(io::Error::other(
                "the environment catalog changed on disk; reopen it",
            ));
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("environment catalog exceeds its size limit"));
        }
        profile::save_atomic(path, &bytes)
    }

    pub(super) fn entries(&self) -> impl Iterator<Item = Entry<'_>> {
        BUILT_INS
            .into_iter()
            .map(|(id, label)| self.entry(id, label, true))
            .chain(
                self.custom
                    .iter()
                    .map(|item| self.entry(&item.id, &item.label, false)),
            )
    }

    pub(super) fn get(&self, id: &str) -> Option<Entry<'_>> {
        self.entries()
            .find(|item| item.id == id || (item.built_in && item.id.eq_ignore_ascii_case(id)))
    }

    pub(crate) fn label(&self, id: &str) -> Option<&str> {
        self.get(id).map(|entry| entry.label)
    }

    pub(crate) fn display_label<'a>(&'a self, id: &'a str) -> &'a str {
        self.label(id).unwrap_or_else(|| {
            if unsafe_text(id) {
                "Unrecognized environment"
            } else {
                id
            }
        })
    }

    pub(crate) fn choices(&self, current: &str) -> Vec<Choice> {
        let mut choices = vec![Choice {
            id: String::new(),
            label: "None".to_owned(),
        }];
        choices.extend(self.entries().map(|entry| Choice {
            id: entry.id.to_owned(),
            label: entry.label.to_owned(),
        }));
        if !current.is_empty() && self.get(current).is_none() {
            choices.push(Choice {
                id: current.to_owned(),
                label: self.display_label(current).to_owned(),
            });
        }
        choices
    }

    pub(crate) fn selection(&self, choices: &[Choice], current: &str) -> Option<Choice> {
        let id = self.get(current).map_or(current, |entry| entry.id);
        choices.iter().find(|choice| choice.id == id).cloned()
    }

    pub(super) fn add(&mut self, label: &str) -> io::Result<String> {
        let label = checked_name(label)?;
        if self.custom.len() >= MAX_CUSTOM {
            return Err(invalid("the environment catalog is full"));
        }
        let mut entropy = [0; 16];
        getrandom::fill(&mut entropy).map_err(|error| io::Error::other(error.to_string()))?;
        let id = format!("custom-{:032x}", u128::from_be_bytes(entropy));
        if self.get(&id).is_some() {
            return Err(io::Error::other(
                "could not allocate a unique environment ID; retry",
            ));
        }
        self.custom.push(Definition {
            id: id.clone(),
            label: label.to_owned(),
        });
        Ok(id)
    }

    pub(super) fn rename(&mut self, id: &str, label: &str) -> io::Result<()> {
        let label = checked_name(label)?;
        let item = self
            .custom
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or_else(|| invalid("only a custom environment can be renamed"))?;
        item.label = label.to_owned();
        Ok(())
    }

    pub(super) fn remove(&mut self, id: &str) -> bool {
        let Some(index) = self.custom.iter().position(|item| item.id == id) else {
            return false;
        };
        self.custom.remove(index);
        self.colors.remove(id);
        true
    }

    pub(super) fn set_color(&mut self, id: &str, color: Option<Rgb>) {
        if let Some(entry) = self.get(id) {
            let id = entry.id.to_owned();
            if let Some(color) = color {
                self.colors.insert(id, color);
            } else {
                self.colors.remove(&id);
            }
        }
    }

    fn entry<'a>(&self, id: &'a str, label: &'a str, built_in: bool) -> Entry<'a> {
        Entry {
            id,
            label,
            built_in,
            color: self.colors.get(id).copied(),
        }
    }

    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION || self.custom.len() > MAX_CUSTOM {
            return Err(invalid("unsupported or oversized environment catalog"));
        }
        let mut ids = std::collections::BTreeSet::new();
        for item in &self.custom {
            let valid_id = item.id.strip_prefix("custom-").is_some_and(|suffix| {
                suffix.len() == 32
                    && suffix
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            });
            if !valid_id || !ids.insert(&item.id) || checked_name(&item.label)? != item.label {
                return Err(invalid("invalid environment definition"));
            }
        }
        if self
            .colors
            .keys()
            .any(|id| !self.entries().any(|entry| entry.id == id))
        {
            return Err(invalid(
                "environment color refers to an unknown environment",
            ));
        }
        Ok(())
    }
}

fn checked_name(value: &str) -> io::Result<&str> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_NAME || unsafe_text(value) {
        return Err(invalid(
            "enter an environment name without control characters",
        ));
    }
    Ok(value)
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}
