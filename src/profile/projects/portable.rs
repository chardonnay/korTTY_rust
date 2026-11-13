//! User-selected project files, separate from the private named layout store.

use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::private_files::{Directory, EntryIdentity, FileStamp, overwrite};
use crate::profile::{PaneLayout, WorkspaceLayout, invalid};
use crate::ssh_dialog::unsafe_text;

use super::{MAX_BYTES, java};

const MAX_PATH: usize = 4096;
const MAX_NAME: usize = 256;
const MAX_DESCRIPTION: usize = 4096;
const EXTENSION: &str = "kortty";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectFile {
    path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    catalogue: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub(crate) struct FileProject {
    pub(crate) source: ProjectFile,
    pub(crate) layout: WorkspaceLayout,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) auto_reconnect: bool,
    pub(crate) java_notes: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ProjectFolder {
    path: PathBuf,
    identity: EntryIdentity,
}

#[derive(Debug)]
pub(crate) struct PreparedProject {
    pub(crate) source: ProjectFile,
    folder: ProjectFolder,
    contents: Vec<u8>,
    previous: Option<FileStamp>,
}

#[derive(Serialize)]
struct Document {
    #[serde(flatten)]
    layout: WorkspaceLayout,
    #[serde(flatten)]
    metadata: DocumentMetadata,
}

#[derive(Deserialize, Serialize)]
struct DocumentMetadata {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    description: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    auto_reconnect: bool,
}

impl ProjectFile {
    pub(crate) fn new(path: PathBuf, catalogue: Option<PathBuf>) -> io::Result<Self> {
        if !valid_path(&path)
            || !Self::has_extension(&path)
            || path.file_name().and_then(|name| name.to_str()).is_none()
            || catalogue.as_deref().is_some_and(|path| !valid_path(path))
        {
            return Err(invalid(
                "Choose an absolute .kortty file and, if needed, an absolute Java connections.xml path",
            ));
        }
        Ok(Self { path, catalogue })
    }

    pub(crate) fn valid(&self) -> bool {
        valid_path(&self.path)
            && Self::has_extension(&self.path)
            && self.catalogue.as_deref().is_none_or(valid_path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn has_extension(path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case(EXTENSION))
    }

    pub(crate) fn is_present(&self) -> bool {
        self.valid() && fs::symlink_metadata(&self.path).is_ok_and(|entry| entry.is_file())
    }

    pub(crate) fn display_name(&self) -> &str {
        self.path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Project")
    }

    pub(crate) fn read(mut self) -> io::Result<FileProject> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| invalid("Project has no parent folder"))?;
        let folder = ProjectFolder::inspect(parent)?;
        self.path = folder.path.join(file_name(&self.path)?);
        self.read_from(&folder)
    }

    pub(crate) fn read_from(self, folder: &ProjectFolder) -> io::Result<FileProject> {
        if self.path.parent() != Some(folder.path()) {
            return Err(invalid("Choose the project again in the selected folder"));
        }
        let filename = file_name(&self.path)?;
        let directory = folder.open()?;
        let mut file = directory.open_file(filename)?;
        let stamp = FileStamp::read(&file)?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(java::PROJECT_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > java::PROJECT_BYTES {
            return Err(invalid("Project exceeds its file size limit"));
        }
        stamp.require_unchanged(&file)?;
        let text = std::str::from_utf8(&bytes).map_err(invalid)?;
        let text = text.trim_start_matches('\u{feff}').trim_start();
        let path = folder.path.join(filename);
        let (document, catalogue, java_notes) = if text.starts_with('<') {
            let catalogue = self
                .catalogue
                .as_deref()
                .map(fs::canonicalize)
                .transpose()?;
            let imported = java::read_project_text(text, catalogue.as_deref())?;
            let notes = imported
                .notes
                .into_iter()
                .map(|(note, count)| format!("{} ({count})", note.description()))
                .collect();
            (
                Document {
                    layout: imported.layout,
                    metadata: DocumentMetadata {
                        name: imported.name,
                        description: imported.description,
                        auto_reconnect: imported.auto_reconnect,
                    },
                },
                catalogue,
                notes,
            )
        } else {
            if bytes.len() as u64 > MAX_BYTES {
                return Err(invalid("Project exceeds the layout size limit"));
            }
            (Document::read_json(text)?, None, Vec::new())
        };
        document.validate()?;
        let source = Self::new(path, catalogue)?;
        let name = if document.metadata.name.is_empty() {
            source.display_name().to_owned()
        } else {
            document.metadata.name
        };
        Ok(FileProject {
            source,
            layout: document.layout,
            name,
            description: document.metadata.description,
            auto_reconnect: document.metadata.auto_reconnect,
            java_notes,
        })
    }
}

impl ProjectFolder {
    pub(crate) fn inspect(path: &Path) -> io::Result<Self> {
        if !valid_path(path) {
            return Err(invalid("Choose an absolute project folder"));
        }
        let path = fs::canonicalize(path)?;
        if !valid_path(&path) {
            return Err(invalid("The project folder path is unsupported"));
        }
        let identity = Directory::open(&path, false)?.identity()?;
        Ok(Self { path, identity })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn require_current(&self) -> io::Result<()> {
        self.open().map(drop)
    }

    fn open(&self) -> io::Result<Directory> {
        let directory = Directory::open(&self.path, false)?;
        if directory.identity()? != self.identity {
            return Err(invalid("The project folder changed. Choose it again."));
        }
        Ok(directory)
    }

    pub(crate) fn prepare(
        &self,
        filename: &str,
        name: String,
        description: String,
        layout: WorkspaceLayout,
        auto_reconnect: bool,
    ) -> io::Result<PreparedProject> {
        if filename.is_empty()
            || filename.len() > 240
            || filename.ends_with([' ', '.'])
            || unsafe_text(filename)
            || filename.contains(['/', '\\', ':', '"', '<', '>', '|', '?', '*'])
        {
            return Err(invalid("Enter a valid project filename"));
        }
        let filename = if ProjectFile::has_extension(Path::new(filename)) {
            filename.to_owned()
        } else {
            format!("{filename}.kortty")
        };
        let source = ProjectFile::new(self.path.join(&filename), None)?;
        let directory = self.open()?;
        let previous = overwrite::review(&directory, &filename, |_| Ok(()))?;
        let document = Document {
            layout,
            metadata: DocumentMetadata {
                name: name.trim().into(),
                description: description.trim().into(),
                auto_reconnect,
            },
        };
        document.validate()?;
        if document.metadata.name.is_empty() {
            return Err(invalid("Enter a project name"));
        }
        let contents = serde_json::to_vec_pretty(&document).map_err(invalid)?;
        if contents.len() as u64 > MAX_BYTES {
            return Err(invalid("Project exceeds the layout size limit"));
        }
        Ok(PreparedProject {
            source,
            folder: self.clone(),
            contents,
            previous,
        })
    }
}

impl PreparedProject {
    pub(crate) fn exists(&self) -> bool {
        self.previous.is_some()
    }

    pub(crate) fn publish(&self) -> io::Result<Option<String>> {
        if self.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Confirm replacing the existing project first.",
            ));
        }
        let directory = self.folder.open()?;
        directory.publish_checked(
            file_name(self.source.path())?,
            |file| file.write_all(&self.contents),
            || self.folder.require_current(),
        )?;
        Ok(directory
            .sync()
            .err()
            .map(|_| "The destination folder could not be synchronized.".into()))
    }

    pub(crate) fn replace(&self) -> io::Result<Option<String>> {
        let previous = self
            .previous
            .as_ref()
            .ok_or_else(|| invalid("Review the existing project before replacing it"))?;
        let directory = self.folder.open()?;
        overwrite::Reviewed {
            parent: &directory,
            path: self.source.path(),
            previous,
            stage_prefix: "project",
        }
        .publish(
            |file| file.write_all(&self.contents),
            || self.folder.require_current(),
            |_| Ok(()),
        )
    }
}

impl Document {
    fn read_json(text: &str) -> io::Result<Self> {
        // Read the layout directly so serde_json decodes object keys as tab indexes.
        // Flattened deserialization buffers them as generic strings.
        Ok(Self {
            layout: serde_json::from_str(text).map_err(invalid)?,
            metadata: serde_json::from_str(text).map_err(invalid)?,
        })
    }

    fn validate(&self) -> io::Result<()> {
        self.layout.validate()?;
        let metadata = &self.metadata;
        if metadata.name.len() > MAX_NAME
            || unsafe_text(&metadata.name)
            || metadata.description.len() > MAX_DESCRIPTION
            || metadata
                .description
                .split(['\n', '\r', '\t'])
                .any(unsafe_text)
        {
            return Err(invalid("Invalid project name or description"));
        }
        let tabs = self.layout.windows.iter().flat_map(|window| &window.tabs);
        if tabs.clone().next().is_none() {
            return Err(invalid("Project has no tabs"));
        }
        if tabs.clone().any(has_private_local_settings) {
            return Err(invalid(
                "Projects with a local sandbox folder or saved local shell must use the private project store",
            ));
        }
        Ok(())
    }
}

fn has_private_local_settings(pane: &PaneLayout) -> bool {
    match pane {
        PaneLayout::Terminal {
            security: Some(settings),
            ..
        } => !settings.directory.is_empty() || settings.saved_profile.is_some(),
        PaneLayout::Split { first, second, .. } => {
            has_private_local_settings(first) || has_private_local_settings(second)
        }
        _ => false,
    }
}

fn valid_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .to_str()
            .is_some_and(|path| path.len() <= MAX_PATH && !unsafe_text(path))
}

fn file_name(path: &Path) -> io::Result<&str> {
    path.file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("Choose a valid project filename"))
}

#[cfg(test)]
mod tests {
    use super::super::tests::Fixture;
    use super::*;

    fn java_fixture(folder: &ProjectFolder) -> io::Result<ProjectFile> {
        let java = folder.path().join("Interop.KORTTY");
        let catalogue = folder.path().join("connections.xml");
        fs::write(&java, include_bytes!("java/fixtures/interop.kortty"))?;
        fs::write(&catalogue, include_bytes!("java/fixtures/connections.xml"))?;
        ProjectFile::new(java, Some(catalogue))
    }

    #[test]
    fn project_file_reads_java_then_round_trips_the_native_split_layout() -> io::Result<()> {
        let scratch = Fixture::new();
        fs::create_dir(&scratch.0)?;
        let folder = ProjectFolder::inspect(&scratch.0)?;
        let opened = java_fixture(&folder)?.read()?;
        assert_eq!(opened.name, "JAXB interoperability fixture");
        assert_eq!(opened.layout.windows.len(), 2);
        assert!(opened.auto_reconnect);
        assert!(!opened.java_notes.is_empty());

        let prepared = folder.prepare(
            "Release notes",
            "Deployment layout".into(),
            "First line\nSecond line".into(),
            opened.layout.clone(),
            true,
        )?;
        assert_eq!(prepared.source.path().extension().unwrap(), "kortty");
        assert!(!prepared.source.is_present());
        assert!(prepared.publish()?.is_none());
        let native = prepared.source.clone().read()?;
        assert_eq!(native.layout, opened.layout);
        assert_eq!(native.name, "Deployment layout");
        assert_eq!(native.description, "First line\nSecond line");
        assert!(native.auto_reconnect);
        assert!(native.java_notes.is_empty());
        assert_eq!(
            prepared.publish().unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(prepared.source.read()?.layout, opened.layout);
        Ok(())
    }

    #[test]
    fn project_file_save_and_read_reject_a_replaced_selected_folder() -> io::Result<()> {
        let scratch = Fixture::new();
        let selected = scratch.0.join("selected");
        fs::create_dir_all(&selected)?;
        let folder = ProjectFolder::inspect(&selected)?;
        let source = java_fixture(&folder)?;
        let opened = source.clone().read_from(&folder)?;
        let prepared = folder.prepare(
            "Review",
            "Review".into(),
            String::new(),
            opened.layout,
            false,
        )?;

        fs::rename(&selected, scratch.0.join("previous"))?;
        fs::create_dir(&selected)?;
        assert!(source.read_from(&folder).is_err());
        assert!(prepared.publish().is_err());
        assert_eq!(fs::read_dir(&selected)?.count(), 0);
        assert!(!scratch.0.join("previous/Review.kortty").exists());
        Ok(())
    }

    #[test]
    fn project_file_replacement_checks_the_reviewed_file_and_requires_confirmation()
    -> io::Result<()> {
        let scratch = Fixture::new();
        fs::create_dir(&scratch.0)?;
        let folder = ProjectFolder::inspect(&scratch.0)?;
        let opened = java_fixture(&folder)?.read()?;
        let path = folder.path().join("Release.kortty");
        fs::write(&path, b"first occupant")?;
        let prepare = || {
            folder.prepare(
                "Release",
                "Replacement".into(),
                "New notes".into(),
                opened.layout.clone(),
                false,
            )
        };
        let first = prepare()?;
        assert!(first.exists());
        assert_eq!(
            first.publish().unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&path)?, b"first occupant");

        fs::rename(&path, folder.path().join("moved"))?;
        fs::write(&path, b"second occupant")?;
        assert!(first.replace().is_err());
        assert_eq!(fs::read(&path)?, b"second occupant");
        assert_eq!(fs::read(folder.path().join("moved"))?, b"first occupant");

        let second = prepare()?;
        assert!(second.replace()?.is_none());
        let native = second.source.read()?;
        assert_eq!(native.name, "Replacement");
        assert_eq!(native.description, "New notes");
        assert_eq!(native.layout, opened.layout);
        assert!(fs::read_dir(folder.path())?.all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".kortty-project-")
        }));
        Ok(())
    }
}
