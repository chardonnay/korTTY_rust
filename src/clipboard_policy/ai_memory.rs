use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use toml_edit::Table;

use super::{allowed, invalid, optional_table, string, trim_java};

mod organization;
pub(crate) use organization::OrganizationMemoryBlock;
pub(super) use organization::parse as parse_organization;

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum UserMemoryMode {
    #[default]
    Editable,
    ReadOnly,
    Disabled,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum AiMemoryLearningMode {
    Propose,
    Off,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AiMemoryPolicy {
    pub(crate) feature_allowed: bool,
    pub(crate) user_memory: UserMemoryMode,
    pub(crate) learning: Option<AiMemoryLearningMode>,
    pub(crate) skill_creation: Option<bool>,
    pub(crate) statistics: Option<bool>,
    pub(crate) external_files_allowed: bool,
    pub(crate) max_tokens: Option<u16>,
    external_roots: Vec<Vec<PathBuf>>,
    organization: Arc<[OrganizationMemoryBlock]>,
}

impl Default for AiMemoryPolicy {
    fn default() -> Self {
        Self {
            feature_allowed: true,
            user_memory: UserMemoryMode::Editable,
            learning: None,
            skill_creation: None,
            statistics: None,
            external_files_allowed: true,
            max_tokens: None,
            external_roots: Vec::new(),
            organization: Arc::default(),
        }
    }
}

impl AiMemoryPolicy {
    pub(super) fn lockdown() -> Self {
        Self {
            feature_allowed: false,
            ..Self::default()
        }
    }

    pub(crate) fn user_files_allowed(&self) -> bool {
        self.feature_allowed && self.user_memory != UserMemoryMode::Disabled
    }

    pub(crate) fn external_files_allowed(&self) -> bool {
        self.user_files_allowed() && self.external_files_allowed
    }

    pub(crate) fn organization(&self) -> &[OrganizationMemoryBlock] {
        &self.organization
    }

    pub(super) fn install_organization(&mut self, parsed: organization::Parsed) {
        self.organization = parsed.into_blocks().into();
    }

    /// Resolve each nonempty list once on the worker. The candidate must be canonical.
    pub(crate) fn external_file_filter(&self) -> impl Fn(&Path) -> bool {
        let allowed = self.external_files_allowed();
        let roots: Vec<Vec<_>> = self
            .external_roots
            .iter()
            .map(|list| {
                list.iter()
                    .filter_map(|root| canonical_root(root))
                    .collect()
            })
            .collect();
        move |candidate| {
            allowed
                && roots
                    .iter()
                    .all(|list| list.iter().any(|root| candidate.starts_with(root)))
        }
    }

    pub(super) fn read_rule(&mut self, rule: &Table, home: Option<&Path>) -> io::Result<()> {
        const TABLE: &str = "rule.ai-memory";
        let Some(memory) = optional_table(rule, "ai-memory")? else {
            return Ok(());
        };
        allowed(
            memory,
            TABLE,
            &[
                "user-memory",
                "learning",
                "skill-creation",
                "statistics",
                "external-files",
                "external-file-roots",
                "max-tokens",
            ],
        )?;
        if let Some(value) = string(memory, TABLE, "user-memory")? {
            let mode = match trim_java(value).to_ascii_lowercase().as_str() {
                "editable" => UserMemoryMode::Editable,
                "read-only" => UserMemoryMode::ReadOnly,
                "disabled" => UserMemoryMode::Disabled,
                _ => {
                    return Err(invalid(
                        "rule.ai-memory.user-memory must be editable, read-only or disabled",
                    ));
                }
            };
            self.user_memory = self.user_memory.max(mode);
        }
        if let Some(value) = string(memory, TABLE, "learning")? {
            let mode = match trim_java(value).to_ascii_lowercase().as_str() {
                "propose" => AiMemoryLearningMode::Propose,
                "off" => AiMemoryLearningMode::Off,
                _ => return Err(invalid("rule.ai-memory.learning must be propose or off")),
            };
            self.learning = self.learning.max(Some(mode));
        }
        if let Some(item) = memory.get("skill-creation") {
            let value = item
                .as_bool()
                .ok_or_else(|| invalid("rule.ai-memory.skill-creation must be a boolean"))?;
            self.skill_creation = Some(self.skill_creation.unwrap_or(true) && value);
        }
        if let Some(item) = memory.get("statistics") {
            let value = item
                .as_bool()
                .ok_or_else(|| invalid("rule.ai-memory.statistics must be a boolean"))?;
            self.statistics = Some(self.statistics.unwrap_or(true) && value);
        }
        if let Some(value) = string(memory, TABLE, "external-files")? {
            match trim_java(value).to_ascii_lowercase().as_str() {
                "allow" => {}
                "deny" => self.external_files_allowed = false,
                _ => {
                    return Err(invalid(
                        "rule.ai-memory.external-files must be allow or deny",
                    ));
                }
            }
        }
        if let Some(item) = memory.get("external-file-roots") {
            let error = || {
                invalid(
                    "rule.ai-memory.external-file-roots must list absolute or ~/-relative paths",
                )
            };
            let roots = item.as_array().ok_or_else(error)?;
            let paths = roots
                .iter()
                .map(|value| root_path(value.as_str().ok_or_else(error)?, home).ok_or_else(error))
                .collect::<io::Result<Vec<_>>>()?;
            if !paths.is_empty() {
                self.external_roots.push(paths);
            }
        }
        if let Some(item) = memory.get("max-tokens") {
            let cap = item
                .as_integer()
                .filter(|cap| (300..=4000).contains(cap))
                .ok_or_else(|| {
                    invalid("rule.ai-memory.max-tokens must be an integer between 300 and 4000")
                })? as u16;
            self.max_tokens = Some(self.max_tokens.map_or(cap, |previous| previous.min(cap)));
        }
        Ok(())
    }
}

fn root_path(value: &str, home: Option<&Path>) -> Option<PathBuf> {
    let value = trim_java(value);
    if value.is_empty() || value.chars().any(char::is_control) {
        return None;
    }
    let path = if let Some(relative) = value
        .strip_prefix("~/")
        .or_else(|| (value == "~").then_some(""))
    {
        let suffix = Path::new(relative.trim_start_matches('/'));
        if suffix
            .components()
            .any(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
        {
            return None;
        }
        home?.join(suffix)
    } else {
        PathBuf::from(value)
    };
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component),
        }
    }
    Some(normalized)
}

fn canonical_root(mut prefix: &Path) -> Option<PathBuf> {
    let mut missing = Vec::new();
    loop {
        match prefix.canonicalize() {
            Ok(mut root) => {
                root.extend(missing.into_iter().rev());
                return Some(root);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(prefix.file_name()?);
                prefix = prefix.parent()?;
            }
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_reject_relative_and_invalid_home_paths() {
        let home = if cfg!(windows) {
            Path::new(r"C:\Users\operator")
        } else {
            Path::new("/home/operator")
        };
        for invalid in [
            "",
            "relative/notes",
            "~another/notes",
            "https://example.test/notes",
            "~/nested\0name",
        ] {
            assert!(root_path(invalid, Some(home)).is_none(), "{invalid:?}");
        }
        assert!(root_path("~/notes", None).is_none());
        assert!(root_path("~/notes", Some(Path::new("relative"))).is_none());
        assert_eq!(root_path("~", Some(home)).as_deref(), Some(home));
        assert_eq!(
            root_path(" ~/notes/./approved ", Some(home)),
            Some(home.join("notes/approved"))
        );
        #[cfg(windows)]
        for invalid in [r"~/C:\outside", r"~/\outside"] {
            assert!(root_path(invalid, Some(home)).is_none(), "{invalid:?}");
        }
    }

    #[test]
    fn root_canonicalization_keeps_missing_suffix_and_resolves_existing_aliases() -> io::Result<()>
    {
        let profile = crate::snippets::tests::Profile::new();
        let real = profile.0.canonicalize()?;
        let suffix = Path::new("future/nested");
        assert_eq!(
            canonical_root(&profile.0.join(suffix)),
            Some(real.join(suffix))
        );
        #[cfg(unix)]
        {
            let alias = profile.0.join("alias");
            std::os::unix::fs::symlink(&real, &alias)?;
            assert_eq!(canonical_root(&alias.join(suffix)), Some(real.join(suffix)));
        }
        Ok(())
    }
}
