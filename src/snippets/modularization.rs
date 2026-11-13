//! Atomic snippet-library publication for a reviewed modularization plan.

use crate::snippet_analysis::model::content_hash;
use crate::snippet_analysis::model::modularization::{
    MAX_MODULE_BYTES, MAX_MODULES, Module, path_key, valid_path,
};

use super::*;

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Publication {
    root: String,
    files: Vec<PreparedFile>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PreparedFile {
    path: String,
    original_sha256: Option<String>,
    result: Snippet,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum PublicationState {
    Pending,
    Applied,
    Conflict,
}

impl Publication {
    pub(crate) fn ids(&self) -> impl Iterator<Item = SnippetId> + '_ {
        self.files.iter().map(|file| file.result.id)
    }

    pub(crate) fn count(&self) -> usize {
        self.files.len()
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        if !(1..=MAX_MODULES).contains(&self.files.len())
            || (!self.root.is_empty() && !valid_path(&self.root))
        {
            return Err(invalid("Select between one and twelve proposed files"));
        }
        let mut ids = HashSet::new();
        let mut paths = HashSet::new();
        for file in &self.files {
            file.result.validate()?;
            let filename = file
                .result
                .metadata
                .file_name
                .as_deref()
                .unwrap_or_default();
            if !valid_path(&file.path)
                || file.path != join(&file.result.metadata.folder_path, filename)
                || self.relative_path(&file.path).is_none()
                || !ids.insert(file.result.id)
                || !paths.insert(path_key(&file.path))
                || file.result.content.len() > MAX_MODULE_BYTES
                || (file.result.content.trim().is_empty()
                    && (filename != "__init__.py"
                        || file.result.metadata.executable != Some(false)))
                || file.original_sha256.as_deref().is_some_and(|hash| {
                    hash.len() != 64
                        || !hash
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                })
            {
                return Err(invalid("The reviewed file destinations are invalid"));
            }
        }
        Ok(())
    }

    pub(crate) fn matches_outputs(&self, outputs: &std::collections::HashMap<&str, &str>) -> bool {
        self.files.iter().all(|file| {
            self.relative_path(&file.path)
                .and_then(|path| outputs.get(path))
                .is_some_and(|content| *content == file.result.content())
        })
    }

    pub(crate) fn matches_targets(
        &self,
        root: &str,
        mut original: impl FnMut(&str) -> Option<SnippetId>,
    ) -> bool {
        self.root == root
            && self.files.iter().all(|file| {
                self.relative_path(&file.path)
                    .is_some_and(|path| match original(path) {
                        Some(id) => file.original_sha256.is_some() && file.result.id == id,
                        None => file.original_sha256.is_none(),
                    })
            })
    }

    fn relative_path<'a>(&self, path: &'a str) -> Option<&'a str> {
        if self.root.is_empty() {
            Some(path)
        } else {
            path.strip_prefix(&self.root)?.strip_prefix('/')
        }
    }

    pub(crate) fn state(&self, library: &Library) -> io::Result<PublicationState> {
        self.validate()?;
        if self
            .files
            .iter()
            .all(|file| library.get(file.result.id) == Some(&file.result))
        {
            return Ok(PublicationState::Applied);
        }
        for file in &self.files {
            let current = library.get(file.result.id).map(fingerprint).transpose()?;
            if current != file.original_sha256 {
                return Ok(PublicationState::Conflict);
            }
        }
        Ok(PublicationState::Pending)
    }
}

impl Library {
    pub(crate) fn prepare_modularization<'a>(
        &self,
        root: &str,
        language: &str,
        category: &str,
        selected: impl IntoIterator<Item = (&'a Module, &'a str, Option<SnippetId>)>,
    ) -> io::Result<Publication> {
        let mut names: HashSet<_> = self
            .entries()
            .iter()
            .map(|snippet| path_key(snippet.name()))
            .collect();
        let mut files = Vec::new();
        for (module, content, original) in selected {
            let path = join(root, &module.path);
            if !valid_path(&path) {
                return Err(invalid(
                    "A planned destination is not a portable relative path",
                ));
            }
            let (folder, filename) = path.rsplit_once('/').unwrap_or(("", &path));
            let original = original
                .map(|id| {
                    self.get(id)
                        .ok_or_else(|| invalid("An original snippet is missing"))
                })
                .transpose()?;
            let id = original.map_or_else(SnippetId::new, |snippet| Ok(snippet.id))?;
            let (name, language, category, mut metadata) = match original {
                Some(snippet) => (
                    snippet.name.clone(),
                    snippet.language.clone(),
                    snippet.category.clone(),
                    snippet.metadata.clone(),
                ),
                None => (
                    new_name(filename, &mut names)?,
                    module_language(filename, language).into(),
                    category.into(),
                    Metadata {
                        description: module.purpose.clone(),
                        ..Metadata::default()
                    },
                ),
            };
            metadata.folder_path = folder.into();
            metadata.file_name = Some(filename.into());
            metadata.executable = Some(module.executable);
            if let Some(snippet) = original {
                diagrams::rebase(&mut metadata.diagrams, snippet.content(), content);
            }
            files.push(PreparedFile {
                path,
                original_sha256: original.map(fingerprint).transpose()?,
                result: Snippet::from_input(
                    id,
                    Input {
                        name,
                        language,
                        category,
                        content: content.into(),
                    },
                    metadata,
                ),
            });
        }
        let publication = Publication {
            root: root.into(),
            files,
        };
        publication.validate()?;
        let _ = self.modularization_document(&publication)?;
        Ok(publication)
    }

    pub(crate) fn publish_modularization(&mut self, publication: &Publication) -> io::Result<()> {
        match publication.state(self)? {
            PublicationState::Applied => Ok(()),
            PublicationState::Pending => {
                let candidate = self.modularization_document(publication)?;
                self.commit(candidate)
            }
            PublicationState::Conflict => {
                Err(invalid("A reviewed snippet changed before publication"))
            }
        }
    }

    fn modularization_document(&self, publication: &Publication) -> io::Result<Document> {
        let mut candidate = self.document.as_ref().clone();
        let replaced: HashSet<_> = publication.ids().collect();
        for file in &publication.files {
            if let Some(index) = candidate
                .snippets
                .iter()
                .position(|snippet| snippet.id == file.result.id)
            {
                let original = &candidate.snippets[index];
                if original.metadata.folder_path != file.result.metadata.folder_path {
                    let path = original.metadata.folder_path.clone();
                    candidate.preserve_folder(&path);
                }
                candidate.snippets[index] = file.result.clone();
            } else {
                candidate.snippets.push(file.result.clone());
            }
        }
        candidate.validate()?;
        candidate.materialize_folders()?;
        let proposed = Library {
            document: Arc::new(candidate.clone()),
            ..self.clone()
        };
        let current_paths = export_paths(self)?;
        let proposed_paths = export_paths(&proposed)?;
        if publication
            .files
            .iter()
            .any(|file| proposed_paths.get(&file.result.id) != Some(&file.path))
            || current_paths
                .iter()
                .any(|(id, path)| !replaced.contains(id) && proposed_paths.get(id) != Some(path))
        {
            return Err(invalid(
                "The proposed paths conflict with existing snippet or folder paths",
            ));
        }
        let folder_paths: HashSet<_> = proposed
            .folders()
            .iter()
            .map(|folder| path_key(&folder.path))
            .collect();
        if publication
            .files
            .iter()
            .any(|file| folder_paths.contains(&path_key(&file.path)))
        {
            return Err(invalid("A proposed file path is already used by a folder"));
        }
        Ok(candidate)
    }
}

fn export_paths(library: &Library) -> io::Result<std::collections::HashMap<SnippetId, String>> {
    let ids: Vec<_> = library.entries().iter().map(Snippet::id).collect();
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    Ok(archive::analysis_files(library, &ids, None)?
        .into_iter()
        .map(|file| (file.snippet.id(), file.path))
        .collect())
}

fn fingerprint(snippet: &Snippet) -> io::Result<String> {
    content_hash(&serde_json::to_string(snippet).map_err(io::Error::other)?)
}

fn join(folder: &str, file: &str) -> String {
    if folder.is_empty() {
        file.into()
    } else {
        format!("{folder}/{file}")
    }
}

fn new_name(filename: &str, names: &mut HashSet<String>) -> io::Result<String> {
    let base = &filename[..filename.floor_char_boundary(MAX_NAME_BYTES.min(filename.len()))];
    if names.insert(path_key(base)) {
        return Ok(base.into());
    }
    for number in 2..=MAX_SNIPPETS + 1 {
        let suffix = format!(" ({number})");
        let prefix = base.floor_char_boundary((MAX_NAME_BYTES - suffix.len()).min(base.len()));
        let name = format!("{}{suffix}", &base[..prefix]);
        if names.insert(path_key(&name)) {
            return Ok(name);
        }
    }
    Err(invalid(
        "Could not assign unique names to the proposed snippets",
    ))
}

fn module_language<'a>(filename: &str, fallback: &'a str) -> &'a str {
    match filename.rsplit_once('.').map(|(_, extension)| extension) {
        Some("sh" | "bash" | "zsh" | "ksh") => "bash",
        Some("py") => "python",
        Some("pl" | "pm") => "perl",
        Some("rb") => "ruby",
        Some("js" | "mjs" | "cjs") => "javascript",
        Some("ts") => "typescript",
        Some("ps1" | "psm1") => "powershell",
        Some("groovy") => "groovy",
        Some("yaml" | "yml") => "yaml",
        Some("json") => "json",
        _ => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippets::tests::Profile;

    fn input(name: &str, content: &str) -> Input {
        Input {
            name: name.into(),
            language: "python".into(),
            category: String::new(),
            content: content.into(),
        }
    }

    fn module(path: &str, entry_point: bool) -> Module {
        Module {
            path: path.into(),
            purpose: "One reviewed file".into(),
            symbols: vec![],
            executable: entry_point,
            entry_point,
        }
    }

    #[test]
    fn stale_source_cannot_publish_a_partial_split() {
        let profile = Profile::new();
        let mut library = Library::open(profile.path()).unwrap();
        let original = library
            .save(None, input("original", "print('original')\n"))
            .unwrap();
        let entry = module("entry.py", true);
        let helper = module("helper.py", false);
        let publication = library
            .prepare_modularization(
                "split",
                "python",
                "",
                [
                    (&entry, "import helper\n", Some(original)),
                    (&helper, "print('reviewed')\n", None),
                ],
            )
            .unwrap();
        let new = publication.ids().find(|id| *id != original).unwrap();
        assert!(publication.state(&library).unwrap() == PublicationState::Pending);
        library
            .save(
                Some(original),
                input("original", "print('changed elsewhere')\n"),
            )
            .unwrap();
        assert!(library.publish_modularization(&publication).is_err());
        let reopened = Library::open(profile.path()).unwrap();
        assert_eq!(
            reopened.get(original).unwrap().content(),
            "print('changed elsewhere')\n"
        );
        assert!(reopened.get(new).is_none());
    }

    #[test]
    fn new_files_cannot_displace_an_existing_export_path() {
        let profile = Profile::new();
        let mut library = Library::open(profile.path()).unwrap();
        library
            .save_with_metadata(
                None,
                input("Existing", "x = 1\n"),
                Some(Metadata {
                    folder_path: "split".into(),
                    file_name: Some("helper.py".into()),
                    ..Metadata::default()
                }),
            )
            .unwrap();
        let helper = module("helper.py", false);
        assert!(
            library
                .prepare_modularization("split", "python", "", [(&helper, "x = 2\n", None)])
                .is_err()
        );
        assert_eq!(library.entries().len(), 1);
    }
}
