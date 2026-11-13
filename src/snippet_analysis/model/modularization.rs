//! Portable paths and publication checks for a reviewed snippet split.

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization as _;

use crate::ai_manager::{CodeTextLanguage, ProfileRef};
use crate::session_journal::export_output::valid_component;
use crate::snippet_analysis::request::modularization::{Progress, output_map};
use crate::snippets::{SnippetId, modularization::Publication};

pub(crate) const MAX_MODULES: usize = 12;
pub(crate) const MAX_MODULE_BYTES: usize = 256 * 1024;

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Job {
    pub(crate) id: SnippetId,
    pub(crate) revision: u32,
    pub(crate) source_sha256: String,
    pub(crate) rendered_sha256: String,
    pub(crate) sources: Vec<(String, SnippetId)>,
    pub(crate) plan: Plan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) outbound: Option<OutboundBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) generation: Option<Generation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) publication: Option<Publication>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) published: bool,
}

/// The saved destination used throughout a split. Each request still requires review.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct OutboundBinding {
    pub(crate) kind: OutboundKind,
    pub(crate) profile_id: ProfileRef,
    pub(crate) profile_revision: [u8; 32],
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OutboundKind {
    Https,
    Cli,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Generation {
    pub(crate) profile_id: ProfileRef,
    pub(crate) approved_preview_sha256: String,
    pub(crate) code_text_language: CodeTextLanguage,
    pub(crate) instructions: String,
    pub(crate) skills: String,
    pub(crate) progress: Progress,
}

impl fmt::Debug for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Modularization")
            .field("id", &self.id)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

impl Job {
    pub(crate) fn version(&self) -> (SnippetId, u32) {
        (self.id, self.revision)
    }

    pub(crate) fn validate(&self, analyzed_source: &str) -> Result<(), String> {
        let valid_hash = |hash: &str| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        self.plan.validate()?;
        let mut ids = HashSet::new();
        let mut paths = HashSet::new();
        if self.revision == 0
            || self.source_sha256 != analyzed_source
            || !valid_hash(&self.rendered_sha256)
            || !(1..=1024).contains(&self.sources.len())
            || self.sources.iter().any(|(path, id)| {
                path.is_empty()
                    || path.len() > 4096
                    || path.chars().any(char::is_control)
                    || !ids.insert(id)
                    || !paths.insert(path_key(path))
            })
        {
            return Err(
                "The stored modularization plan does not match the analyzed source.".into(),
            );
        }
        if self.outbound.as_ref().is_some_and(|binding| {
            !binding.profile_id.is_nonzero()
                || binding.profile_revision == [0; 32]
                || self
                    .generation
                    .as_ref()
                    .is_some_and(|generation| generation.profile_id != binding.profile_id)
        }) {
            return Err("The stored modularization destination is invalid.".into());
        }
        if let Some(generation) = &self.generation {
            if !self.plan.recommended
                || !generation.profile_id.is_nonzero()
                || !valid_hash(&generation.approved_preview_sha256)
                || generation.instructions.len() > 4096
                || generation.skills.len() > 16 * 1024
            {
                return Err("The stored modularization generation request is invalid.".into());
            }
            generation.progress.validate(&self.plan)?;
        }
        if let Some(publication) = &self.publication {
            publication.validate().map_err(|error| error.to_string())?;
            if self.generation.as_ref().is_none_or(|generation| {
                !generation.progress.ready()
                    || !publication.matches_outputs(&output_map(&generation.progress.outputs))
            }) {
                return Err(
                    "The stored publication does not match the reviewed file proposals.".into(),
                );
            }
        } else if self.published {
            return Err("The stored publication result is missing its file destinations.".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Plan {
    pub(crate) recommended: bool,
    pub(crate) rationale: String,
    pub(crate) files: Vec<Module>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Module {
    pub(crate) path: String,
    pub(crate) purpose: String,
    pub(crate) symbols: Vec<String>,
    pub(crate) executable: bool,
    pub(crate) entry_point: bool,
}

impl Module {
    pub(crate) fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or_default()
    }

    pub(crate) fn package_marker(&self) -> bool {
        self.file_name() == "__init__.py" && !self.entry_point
    }

    fn module_stem(&self) -> Option<&str> {
        let (stem, extension) = self.file_name().rsplit_once('.')?;
        matches!(
            extension,
            "sh" | "bash"
                | "zsh"
                | "ksh"
                | "py"
                | "pl"
                | "pm"
                | "rb"
                | "js"
                | "ts"
                | "ps1"
                | "psm1"
                | "groovy"
        )
        .then_some(stem)
    }

    pub(crate) fn referenced_in(&self, source: &str) -> bool {
        self.module_stem()
            .is_some_and(|stem| source.contains(self.file_name()) || contains_symbol(source, stem))
    }
}

impl Plan {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        let valid_text = |text: &str, max| {
            !text.trim().is_empty() && text.len() <= max && !text.chars().any(char::is_control)
        };
        if !valid_text(&self.rationale, 4096) {
            return Err("The modularization plan must have a bounded rationale.");
        }
        if !self.recommended && self.files.is_empty() {
            return Ok(());
        }
        if !self.recommended
            || !(2..=MAX_MODULES).contains(&self.files.len())
            || self.files.iter().filter(|file| file.entry_point).count() != 1
            || !self
                .files
                .iter()
                .any(|file| file.entry_point && file.executable)
        {
            return Err("A split needs 2 to 12 files and exactly one executable entry point.");
        }
        let mut files = HashSet::new();
        let mut folders = HashMap::new();
        for module in &self.files {
            if !valid_path(&module.path)
                || !valid_module_name(module.file_name())
                || !valid_text(&module.purpose, 1024)
                || module.symbols.len() > 64
                || (module.package_marker() && module.executable)
                || module.symbols.iter().any(|symbol| !valid_text(symbol, 256))
            {
                return Err("A planned file has an invalid relative path, purpose or symbols.");
            }
            if !files.insert(path_key(&module.path)) {
                return Err("Planned file paths collide, ignoring case and Unicode normalization.");
            }
            for (separator, _) in module.path.match_indices('/') {
                let folder = &module.path[..separator];
                let key = path_key(folder);
                if folders
                    .insert(key, folder)
                    .is_some_and(|previous| previous != folder)
                {
                    return Err(
                        "Planned folder paths differ only in case or Unicode normalization.",
                    );
                }
            }
        }
        if files.iter().any(|path| folders.contains_key(path)) {
            return Err("A planned file path is also used as a folder.");
        }
        Ok(())
    }

    pub(crate) fn generation_order(&self) -> impl Iterator<Item = &Module> {
        self.files
            .iter()
            .filter(|file| !file.entry_point)
            .chain(self.files.iter().filter(|file| file.entry_point))
    }

    pub(crate) fn problems(&self, contents: &HashMap<&str, &str>) -> Vec<String> {
        let mut problems = Vec::new();
        for module in &self.files {
            if contents.get(module.path.as_str()).is_none_or(|content| {
                content.len() > MAX_MODULE_BYTES
                    || content.contains('\0')
                    || (content.trim().is_empty() && !module.package_marker())
            }) {
                problems.push(format!(
                    "{} is missing, empty or exceeds the source limit.",
                    module.path
                ));
            }
            if !module.entry_point
                && !module.package_marker()
                && module.module_stem().is_some()
                && !contents
                    .iter()
                    .any(|(path, content)| *path != module.path && module.referenced_in(content))
            {
                problems.push(format!("No proposed file refers to {}.", module.path));
            }
        }
        problems
    }
}

pub(crate) fn valid_path(path: &str) -> bool {
    path.len() <= 768
        && path.split('/').count() <= 24
        && path
            .split('/')
            .all(|part| part == part.trim() && valid_component(part))
}

pub(crate) fn path_key(path: &str) -> String {
    path.chars().flat_map(char::to_lowercase).nfc().collect()
}

fn valid_module_name(name: &str) -> bool {
    // Snippet file exports preserve this subset of explicit filenames byte for byte.
    !name.starts_with(['.', '-'])
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn contains_symbol(source: &str, symbol: &str) -> bool {
    !symbol.is_empty()
        && source
            .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
            .any(|word| word == symbol)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(first: &str, second: &str) -> Plan {
        Plan {
            recommended: true,
            rationale: "Separate reusable logic from command-line handling.".into(),
            files: [first, second]
                .into_iter()
                .enumerate()
                .map(|(index, path)| Module {
                    path: path.into(),
                    purpose: "One part of the split.".into(),
                    symbols: vec![],
                    executable: index == 0,
                    entry_point: index == 0,
                })
                .collect(),
        }
    }

    #[test]
    fn plans_reject_ambiguous_or_nonportable_destinations() {
        for (first, second) in [
            ("../main.py", "helper.py"),
            ("/main.py", "helper.py"),
            ("main.py", "C:/helper.py"),
            ("main.py", "libs/NUL.py"),
            ("lib/entry.py", "Lib/helper.py"),
            ("lib.py", "LIB.PY/helper.py"),
            ("ma\u{ef}n/entry.py", "mai\u{308}n/helper.py"),
        ] {
            assert!(plan(first, second).validate().is_err(), "{first}, {second}");
        }
        let valid = plan("main.py", "lib/helper.py");
        assert!(valid.validate().is_ok());
        assert_eq!(
            valid.generation_order().next().unwrap().path,
            "lib/helper.py"
        );
    }

    #[test]
    fn generation_detects_a_missing_reference_and_allows_an_empty_package_marker() {
        let proposed = plan("main.py", "helper.py");
        assert!(
            !proposed
                .problems(&HashMap::from([
                    ("main.py", "print('ok')"),
                    ("helper.py", "x = 1")
                ]))
                .is_empty()
        );
        assert!(
            proposed
                .problems(&HashMap::from([
                    ("main.py", "import helper"),
                    ("helper.py", "x = 1")
                ]))
                .is_empty()
        );
        let marker = plan("main.py", "lib/__init__.py");
        assert!(
            marker
                .problems(&HashMap::from([
                    ("main.py", "print('ok')"),
                    ("lib/__init__.py", "")
                ]))
                .is_empty()
        );
    }
}
