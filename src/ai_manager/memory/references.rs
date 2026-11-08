use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::private_files::{Directory, FileStamp, require_single_link};

use super::{Scope, format, store};

const NAME: &str = "external.json";
const MAX_FILE_BYTES: usize = 1024 * 1024;
const MAX_REFERENCES: usize = 256;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Reference {
    pub(super) id: String,
    pub(super) path: String,
    root: String,
    tool: String,
    connection_id: Option<String>,
    pub(super) active: bool,
    approved_sha256: String,
    approved_at: String,
    status: String,
    reason: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Registry {
    version: u32,
    pub(super) entries: Vec<Reference>,
    #[serde(skip)]
    previous: store::Snapshot,
}

#[derive(Clone)]
pub(crate) struct Review {
    pub(super) path: PathBuf,
    pub(super) text: String,
    sha256: String,
}

impl Reference {
    pub(super) fn applies_to(&self, scope: &Scope) -> bool {
        self.connection_id == scope.id.map(|id| id.to_string())
    }
}

pub(super) fn registry(path: &Path) -> io::Result<Registry> {
    let previous = store::optional(Directory::open(path, false))?
        .map(|root| store::read(&root, NAME, MAX_FILE_BYTES))
        .transpose()?
        .unwrap_or_default();
    if previous.text.is_empty() {
        return Ok(Registry {
            version: 1,
            entries: Vec::new(),
            previous,
        });
    }
    let mut registry: Registry = serde_json::from_str(&previous.text).map_err(io::Error::other)?;
    if registry.version != 1 || registry.entries.len() > MAX_REFERENCES {
        return Err(io::Error::other("Unsupported external memory registry"));
    }
    registry.previous = previous;
    Ok(registry)
}

pub(super) fn review(path: PathBuf) -> io::Result<Review> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(io::Error::other("Choose an absolute reference file path"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Choose a regular reference file"))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("Choose a regular UTF-8 file name"))?;
    let root = Directory::open(parent, false)?;
    let mut file = root.open_file(name)?;
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    let mut text = String::new();
    Read::by_ref(&mut file)
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_string(&mut text)?;
    stamp.require_unchanged(&file)?;
    if text.len() > MAX_FILE_BYTES {
        return Err(io::Error::other("The reference exceeds 1 MiB"));
    }
    if text.contains('\0') {
        return Err(io::Error::other("Choose a UTF-8 text file"));
    }
    if let Some(line) = format::secret_line(&text, "") {
        return Err(io::Error::other(format!(
            "Possible credential on line {line}. The reference was refused."
        )));
    }
    let sha256 =
        format::hash(&text).ok_or_else(|| io::Error::other("Could not hash the reference file"))?;
    text.retain(|c| (!c.is_control() || matches!(c, '\r' | '\n' | '\t'))
        && !matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}'));
    Ok(Review { path, text, sha256 })
}

pub(super) fn approve(
    directory: &Path,
    scope: &Scope,
    mut registry: Registry,
    preview: Review,
) -> io::Result<Registry> {
    if review(preview.path.clone())?.sha256 != preview.sha256 {
        return Err(io::Error::other(
            "The reference changed. Review the new text before approving.",
        ));
    }
    let path = preview
        .path
        .to_str()
        .ok_or_else(|| io::Error::other("Choose a UTF-8 path"))?;
    let root = preview
        .path
        .parent()
        .and_then(Path::to_str)
        .ok_or_else(|| io::Error::other("Choose a UTF-8 directory path"))?;
    let approved_at = jiff::Timestamp::now().to_string();
    if let Some(entry) = registry
        .entries
        .iter_mut()
        .find(|entry| entry.path == path && entry.applies_to(scope))
    {
        entry.approved_sha256 = preview.sha256;
        entry.approved_at = approved_at;
        entry.active = true;
        entry.status = "ok".into();
        entry.reason.clear();
    } else {
        if registry.entries.len() >= MAX_REFERENCES {
            return Err(io::Error::other(
                "The external memory reference limit was reached",
            ));
        }
        registry.entries.push(Reference {
            id: crate::private_files::nonce()?,
            path: path.to_owned(),
            root: root.to_owned(),
            tool: "OTHER".into(),
            connection_id: scope.id.map(|id| id.to_string()),
            active: true,
            approved_sha256: preview.sha256,
            approved_at,
            status: "ok".into(),
            reason: String::new(),
        });
    }
    save(directory, &registry)
}

pub(super) fn change(
    directory: &Path,
    scope: &Scope,
    mut registry: Registry,
    id: &str,
    active: Option<bool>,
) -> io::Result<Registry> {
    let Some(index) = registry
        .entries
        .iter()
        .position(|entry| entry.id == id && entry.applies_to(scope))
    else {
        return Err(io::Error::other("The reference is missing"));
    };
    if let Some(active) = active {
        let entry = &mut registry.entries[index];
        if active && review(PathBuf::from(&entry.path))?.sha256 != entry.approved_sha256 {
            return Err(io::Error::other(
                "The reference changed. Review it again before enabling it.",
            ));
        }
        entry.active = active;
    } else {
        registry.entries.remove(index);
    }
    save(directory, &registry)
}

fn save(directory: &Path, registry: &Registry) -> io::Result<Registry> {
    let root = Directory::open(directory, true)?;
    let serialized = serde_json::to_string_pretty(registry).map_err(io::Error::other)?;
    if serialized.len() > MAX_FILE_BYTES {
        return Err(io::Error::other(
            "The reference registry exceeds its size limit",
        ));
    }
    store::publish(&root, NAME, &registry.previous, &serialized)?;
    self::registry(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_and_secret_references_cannot_be_approved() -> io::Result<()> {
        let tmp = std::env::temp_dir().canonicalize()?;
        let name = format!("kortty-memory-reference-{}", crate::private_files::nonce()?);
        let base = tmp.join(&name);
        let dir = Directory::open(&base, true)?;
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(base.clone());
        let name = "AGENTS.md";
        let path = base.join(name);
        let scope = Scope::connection(42, "test connection".into());
        dir.publish_copy(name, b"Review the plan before deployment.\n")?;
        let first = review(path.clone())?;
        dir.replace_file(name, b"Review the release notes before deployment.\n")?;
        assert!(approve(&base, &scope, registry(&base)?, first).is_err());

        let approved = approve(&base, &scope, registry(&base)?, review(path.clone())?)?;
        assert!(approved.entries[0].active);
        assert!(!approved.entries[0].applies_to(&Scope::global()));
        let id = approved.entries[0].id.clone();
        let disabled = change(&base, &scope, approved, &id, Some(false))?;
        dir.replace_file(name, b"password: Winter2026!\n")?;
        assert!(review(path).is_err());
        assert!(change(&base, &scope, disabled, &id, Some(true)).is_err());
        assert!(!registry(&base)?.entries[0].active);
        drop(dir);
        Ok(())
    }
}
