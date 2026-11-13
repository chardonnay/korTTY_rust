//! A local directory selected or observed for one reviewed agent run.

use std::path::{Path, PathBuf};

use crate::private_files::{Directory, EntryIdentity};

#[derive(Clone, Eq, PartialEq)]
pub struct LocalDirectory {
    configured: PathBuf,
    canonical: PathBuf,
    identity: EntryIdentity,
}

impl LocalDirectory {
    pub(crate) fn capture(configured: PathBuf) -> Result<Self, String> {
        if !valid_path(&configured) {
            return Err("Choose an absolute local directory without control characters.".into());
        }
        let canonical = configured.canonicalize().map_err(|_| unavailable())?;
        if !valid_path(&canonical) {
            return Err(unavailable());
        }
        let identity = Directory::open(&canonical, false)
            .and_then(|directory| directory.identity())
            .map_err(|_| unavailable())?;
        Ok(Self {
            configured,
            canonical,
            identity,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.canonical
    }

    pub(crate) fn require_current(&self) -> Result<(), String> {
        if Self::capture(self.configured.clone()).as_ref() == Ok(self) {
            return Ok(());
        }
        Err("The reviewed directory changed. Start a new agent run.".into())
    }
}

fn valid_path(path: &Path) -> bool {
    let valid = path.is_absolute() && path.to_str().is_some_and(valid_label);
    #[cfg(windows)]
    let valid = {
        use std::path::{Component, Prefix};
        valid
            && matches!(path.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
    };
    valid
}

pub(crate) fn valid_label(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 4096
        && !text.chars().any(|character| {
            character.is_control()
                || matches!(character,
                    '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
                    | '\u{2060}'..='\u{206f}' | '\u{feff}'
                )
        })
}

fn unavailable() -> String {
    "Could not verify the selected local directory.".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippets::tests::Profile;

    #[test]
    fn agent_local_directory_rejects_replacement_at_same_path() {
        let profile = Profile::new();
        let path = profile.0.join("approved");
        std::fs::create_dir(&path).unwrap();
        let directory = LocalDirectory::capture(path.clone()).unwrap();
        assert!(directory.require_current().is_ok());
        std::fs::rename(&path, profile.0.join("old")).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(directory.require_current().is_err());
    }
}
