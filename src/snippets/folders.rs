//! Logical folder identities and transactional operations for the snippet library.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;

use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization as _;

use super::{Document, Library, MAX_SNIPPETS, Snippet, SnippetId, invalid, valid_field};

const MAX_DEPTH: usize = 32;
const MAX_PATH_BYTES: usize = 1024;
const MAX_FOLDERS: usize = MAX_SNIPPETS * MAX_DEPTH + 1024;

/// A folder's identity survives renames, moves and restarts.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct Id(SnippetId);

impl Id {
    fn from_legacy_path(key: &str, attempt: usize) -> io::Result<Self> {
        let input = format!("kortty/snippet-folder/v1\0{attempt}\0{key}");
        let digest = HashAlgorithm::Sha256
            .digest(input.as_bytes())
            .map_err(|_| invalid("Could not identify the snippet folder"))?;
        let bytes = digest
            .get(..16)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| invalid("Could not identify the snippet folder"))?;
        Ok(Self(SnippetId(bytes)))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Folder {
    pub(crate) id: Id,
    pub(crate) path: String,
}

#[derive(Default, Eq, PartialEq)]
pub(super) struct Catalog {
    entries: Vec<Folder>,
    by_id: HashMap<Id, usize>,
    by_key: HashMap<String, usize>,
    by_metadata: HashMap<String, Id>,
    displaced: Vec<Id>,
}

impl Catalog {
    fn build(folders: &[Folder], snippets: &[Snippet]) -> io::Result<Self> {
        if folders.len() > MAX_FOLDERS {
            return Err(invalid("Too many snippet folders"));
        }
        let mut catalog = Self::default();
        let reserved: HashSet<_> = folders.iter().map(|folder| folder.id).collect();
        if reserved.len() != folders.len() {
            return Err(invalid("Snippet folder IDs must be unique"));
        }
        let mut ordered: Vec<_> = folders.iter().collect();
        ordered.sort_by_key(|folder| folder.path.bytes().filter(|&byte| byte == b'/').count());
        for folder in ordered {
            if logical_path(&folder.path).as_deref() != Some(folder.path.as_str())
                || folder.path.is_empty()
            {
                return Err(invalid("Invalid stored snippet folder path"));
            }
            catalog.insert_path(&folder.path, Some(folder.id), &reserved)?;
        }
        let mut seen = HashSet::new();
        for snippet in snippets {
            let raw = snippet.metadata.folder_path.as_str();
            if raw.is_empty() || !seen.insert(raw) {
                continue;
            }
            // Legacy metadata can describe deeper paths than the tree supports.
            // Leave such metadata intact and show the snippet at the library root.
            if let Some(path) = logical_path(raw)
                && !path.is_empty()
            {
                let id = catalog.insert_path(&path, None, &reserved)?;
                catalog.by_metadata.insert(raw.into(), id);
            }
        }
        Ok(catalog)
    }

    fn insert_path(
        &mut self,
        path: &str,
        explicit_id: Option<Id>,
        reserved: &HashSet<Id>,
    ) -> io::Result<Id> {
        let mut key = String::new();
        let mut display = String::new();
        let mut segments = path.split('/').peekable();
        let mut last = None;
        while let Some(segment) = segments.next() {
            if !key.is_empty() {
                key.push('/');
            }
            key.push_str(&path_key(segment));
            let id = explicit_id.filter(|_| segments.peek().is_none());
            if let Some(&index) = self.by_key.get(&key) {
                if id.is_some() {
                    return Err(invalid(
                        "Snippet folder paths must be unique, ignoring case",
                    ));
                }
                let existing = &self.entries[index];
                display.clone_from(&existing.path);
                last = Some(existing.id);
                continue;
            }
            if self.entries.len() == MAX_FOLDERS {
                return Err(invalid("Too many snippet folders"));
            }
            let id = match id {
                Some(id) => id,
                None => self.legacy_id(&key, reserved)?,
            };
            let index = self.entries.len();
            if self.by_id.insert(id, index).is_some() {
                return Err(invalid("Snippet folder IDs must be unique"));
            }
            if !display.is_empty() {
                display.push('/');
            }
            display.push_str(segment);
            self.by_key.insert(key.clone(), index);
            self.entries.push(Folder {
                id,
                path: display.clone(),
            });
            last = Some(id);
        }
        last.ok_or_else(|| invalid("Enter a snippet folder name"))
    }

    fn legacy_id(&mut self, key: &str, reserved: &HashSet<Id>) -> io::Result<Id> {
        for attempt in 0..=MAX_FOLDERS {
            let id = Id::from_legacy_path(key, attempt)?;
            if !reserved.contains(&id) && !self.by_id.contains_key(&id) {
                if attempt != 0 {
                    self.displaced.push(id);
                }
                return Ok(id);
            }
        }
        Err(invalid("Could not assign a unique snippet folder ID"))
    }

    fn get(&self, id: Id) -> Option<&Folder> {
        self.by_id.get(&id).map(|&index| &self.entries[index])
    }

    fn at_path(&self, path: &str) -> Option<&Folder> {
        self.by_key
            .get(&path_key(path))
            .map(|&index| &self.entries[index])
    }
}

impl Document {
    pub(super) fn materialize_folders(&mut self) -> io::Result<()> {
        self.folder_catalog = Arc::new(Catalog::build(&self.folders, &self.snippets)?);
        Ok(())
    }

    pub(super) fn preserve_displaced_folders(&mut self) -> io::Result<()> {
        if !self.folder_catalog.displaced.is_empty() {
            // An old name can be reused while its original folder keeps the hash ID
            // at its new location. Keep the replacement ID after that folder is deleted.
            for id in self.folder_catalog.displaced.clone() {
                self.remember_folder(id);
            }
            self.materialize_folders()?;
        }
        Ok(())
    }

    pub(super) fn preserve_folder(&mut self, metadata_path: &str) {
        if let Some(id) = self.folder_catalog.by_metadata.get(metadata_path).copied() {
            self.remember_folder(id);
        }
    }

    fn remember_folder(&mut self, id: Id) {
        if !self.folders.iter().any(|folder| folder.id == id)
            && let Some(folder) = self.folder_catalog.get(id)
        {
            self.folders.push(folder.clone());
        }
    }

    fn preserve_parent(&mut self, path: &str) {
        if let Some(folder) = self.folder_catalog.at_path(parent_path(path)) {
            self.remember_folder(folder.id);
        }
    }

    fn rewrite_folders(&mut self, paths: &HashMap<Id, Option<String>>, promote: Option<&str>) {
        self.folders
            .retain(|folder| !paths.contains_key(&folder.id));
        self.folders
            .extend(self.folder_catalog.entries.iter().filter_map(|folder| {
                Some(Folder {
                    id: folder.id,
                    path: paths.get(&folder.id)?.as_ref()?.clone(),
                })
            }));
        self.snippets.retain_mut(|snippet| {
            let Some(change) = self
                .folder_catalog
                .by_metadata
                .get(&snippet.metadata.folder_path)
                .and_then(|id| paths.get(id))
            else {
                return true;
            };
            if let Some(path) = change.as_deref().or(promote) {
                snippet.metadata.folder_path = path.into();
                true
            } else {
                false
            }
        });
    }
}

impl Library {
    /// Includes legacy folders inferred from snippet paths and all explicit folders.
    pub(crate) fn folders(&self) -> &[Folder] {
        &self.document.folder_catalog.entries
    }

    pub(crate) fn folder(&self, id: Id) -> Option<&Folder> {
        self.document.folder_catalog.get(id)
    }

    pub(crate) fn folder_id(&self, snippet: &Snippet) -> Option<Id> {
        self.document
            .folder_catalog
            .by_metadata
            .get(&snippet.metadata.folder_path)
            .copied()
    }

    /// A missing folder means the library root. Recursive root queries include every snippet.
    pub(crate) fn snippets_in_folder(&self, folder: Option<Id>, recursive: bool) -> Vec<SnippetId> {
        let path = folder
            .and_then(|id| self.folder(id))
            .map(|folder| folder.path.as_str());
        self.entries()
            .iter()
            .filter(|snippet| {
                let actual = self.folder_id(snippet);
                if !recursive || folder.is_none() {
                    return (recursive && folder.is_none()) || actual == folder;
                }
                path.zip(actual.and_then(|id| self.folder(id)))
                    .is_some_and(|(path, folder)| subtree_suffix(&folder.path, path).is_some())
            })
            .map(Snippet::id)
            .collect()
    }

    pub(crate) fn create_folder(&mut self, parent: Option<Id>, name: &str) -> io::Result<Id> {
        let parent = self.folder_path(parent)?;
        let path = child_path(parent, &folder_name(name)?);
        self.check_destination(&path, None)?;
        let id = Id(SnippetId::new()?);
        let mut candidate = self.document.as_ref().clone();
        candidate.folders.push(Folder { id, path });
        self.commit(candidate)?;
        Ok(id)
    }

    pub(crate) fn rename_folder(&mut self, id: Id, name: &str) -> io::Result<()> {
        let origin = self.folder_path(Some(id))?;
        let path = child_path(parent_path(origin), &folder_name(name)?);
        self.relocate_folder(id, path)
    }

    pub(crate) fn move_folder(&mut self, id: Id, parent: Option<Id>) -> io::Result<()> {
        let origin = self.folder_path(Some(id))?;
        let parent = self.folder_path(parent)?;
        if subtree_suffix(parent, origin).is_some() {
            return Err(invalid(
                "A folder cannot be moved into itself or its descendants",
            ));
        }
        let path = child_path(parent, folder_basename(origin));
        self.relocate_folder(id, path)
    }

    /// Removes a subtree or promotes its children and snippets to the parent.
    pub(crate) fn remove_folder(
        &mut self,
        id: Id,
        delete_contents: bool,
    ) -> io::Result<Vec<SnippetId>> {
        let origin = self.folder_path(Some(id))?;
        let parent = parent_path(origin);
        let mut paths = HashMap::from([(id, None)]);
        let removed = if delete_contents {
            paths.extend(self.folders().iter().filter_map(|folder| {
                subtree_suffix(&folder.path, origin).map(|_| (folder.id, None))
            }));
            self.snippets_in_folder(Some(id), true)
        } else {
            let mut taken: HashSet<_> = self
                .folders()
                .iter()
                .filter(|folder| parent_path(&folder.path) == parent && folder.id != id)
                .map(|folder| path_key(folder_basename(&folder.path)))
                .collect();
            let mut children = HashMap::new();
            for child in self
                .folders()
                .iter()
                .filter(|folder| parent_path(&folder.path) == origin)
            {
                let name = unique_name(folder_basename(&child.path), &mut taken);
                children.insert(folder_basename(&child.path), child_path(parent, &name));
            }
            for folder in self.folders() {
                if let Some(rest) =
                    subtree_suffix(&folder.path, origin).and_then(|path| path.strip_prefix('/'))
                {
                    let (child, suffix) = rest.split_once('/').unwrap_or((rest, ""));
                    if let Some(destination) = children.get(child) {
                        paths.insert(folder.id, Some(child_path(destination, suffix)));
                    }
                }
            }
            Vec::new()
        };
        let mut candidate = self.document.as_ref().clone();
        candidate.preserve_parent(origin);
        candidate.rewrite_folders(&paths, (!delete_contents).then_some(parent));
        self.commit(candidate)?;
        Ok(removed)
    }

    /// Moves all supplied snippets in a single commit and preserves their identities.
    pub(crate) fn move_snippets_to_folder(
        &mut self,
        ids: &[SnippetId],
        folder: Option<Id>,
    ) -> io::Result<bool> {
        let destination = self.folder_path(folder)?;
        let ids: HashSet<_> = ids.iter().copied().collect();
        let mut candidate = self.document.as_ref().clone();
        let mut matched = 0;
        let mut previous = HashSet::new();
        let mut changed = false;
        for snippet in &mut candidate.snippets {
            if !ids.contains(&snippet.id) {
                continue;
            }
            matched += 1;
            let actual = self.folder_id(snippet);
            if actual != folder || (folder.is_none() && !snippet.metadata.folder_path.is_empty()) {
                if let Some(id) = actual {
                    previous.insert(id);
                }
                snippet.metadata.folder_path = destination.into();
                changed = true;
            }
        }
        if matched != ids.len() {
            return Err(invalid("A snippet no longer exists"));
        }
        if changed {
            // Preserve folder ordering when several source folders become empty.
            for folder in self
                .folders()
                .iter()
                .filter(|folder| previous.contains(&folder.id))
            {
                candidate.remember_folder(folder.id);
            }
            self.commit(candidate)?;
        }
        Ok(changed)
    }

    fn folder_path(&self, id: Option<Id>) -> io::Result<&str> {
        id.map_or(Ok(""), |id| {
            self.folder(id)
                .map(|folder| folder.path.as_str())
                .ok_or_else(|| invalid("The snippet folder no longer exists"))
        })
    }

    fn check_destination(&self, path: &str, origin: Option<Id>) -> io::Result<()> {
        if logical_path(path).as_deref() != Some(path) {
            return Err(invalid("The snippet folder path is too long or too deep"));
        }
        if self
            .document
            .folder_catalog
            .at_path(path)
            .is_some_and(|folder| Some(folder.id) != origin)
        {
            return Err(invalid(
                "A snippet folder with that name already exists here",
            ));
        }
        Ok(())
    }

    fn relocate_folder(&mut self, id: Id, path: String) -> io::Result<()> {
        let origin = self.folder_path(Some(id))?;
        if origin == path {
            return Ok(());
        }
        self.check_destination(&path, Some(id))?;
        let paths = self
            .folders()
            .iter()
            .filter_map(|folder| {
                subtree_suffix(&folder.path, origin)
                    .map(|suffix| (folder.id, Some(format!("{path}{suffix}"))))
            })
            .collect();
        let mut candidate = self.document.as_ref().clone();
        candidate.preserve_parent(origin);
        candidate.rewrite_folders(&paths, None);
        self.commit(candidate)
    }
}

fn sanitize_name(name: &str) -> String {
    let clean: String = name
        .trim()
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
            {
                '-'
            } else {
                character
            }
        })
        .collect();
    clean.trim_end_matches(['.', ' ']).trim().into()
}

fn folder_name(name: &str) -> io::Result<String> {
    let name = sanitize_name(name);
    if name.is_empty() || !valid_field(&name, MAX_PATH_BYTES) {
        return Err(invalid("Enter a valid snippet folder name"));
    }
    Ok(name)
}

fn logical_path(raw: &str) -> Option<String> {
    let mut path = String::new();
    for (depth, segment) in raw
        .split(['/', '\\'])
        .map(sanitize_name)
        .filter(|segment| !segment.is_empty())
        .enumerate()
    {
        if depth == MAX_DEPTH {
            return None;
        }
        if !path.is_empty() {
            path.push('/');
        }
        path.push_str(&segment);
    }
    (path.len() <= MAX_PATH_BYTES).then_some(path)
}

fn path_key(path: &str) -> String {
    path.chars().flat_map(char::to_lowercase).nfc().collect()
}

fn parent_path(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn folder_basename(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn child_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.into()
    } else if name.is_empty() {
        parent.into()
    } else {
        format!("{parent}/{name}")
    }
}

fn subtree_suffix<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    path.strip_prefix(root)
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
}

fn unique_name(name: &str, taken: &mut HashSet<String>) -> String {
    let mut candidate = name.to_owned();
    for suffix in 2.. {
        if taken.insert(path_key(&candidate)) {
            break;
        }
        candidate = format!("{name}-{suffix}");
    }
    candidate
}

#[cfg(test)]
mod tests;
