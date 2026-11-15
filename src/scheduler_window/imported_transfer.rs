use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub(crate) use super::sync::Direction;
use super::{RemoteAction, RemotePath, rsync, sync};

pub(crate) const MAX_RSYNC_SOURCES: usize = rsync::MAX_SOURCES;

pub(crate) const REVIEW_REQUIRED: &str = "Review the imported SFTP action and choose a path on this device before running or enabling this job.";
pub(crate) const RSYNC_REVIEW_REQUIRED: &str = "Review the imported rsync action, executable and every destination before running or enabling this job.";

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Upload,
    Download,
    SyncUpload,
    SyncDownload,
}

impl Kind {
    pub(super) fn direction(self) -> sync::Direction {
        match self {
            Self::Upload | Self::SyncUpload => sync::Direction::Upload,
            Self::Download | Self::SyncDownload => sync::Direction::Download,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Upload => "Java SFTP upload",
            Self::Download => "Java SFTP download",
            Self::SyncUpload => "Java SFTP sync upload",
            Self::SyncDownload => "Java SFTP sync download",
        }
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum Draft {
    Sftp(SftpDraft),
    Rsync(RsyncDraft),
}

impl Draft {
    pub(super) fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Sftp(draft) => draft.validate(),
            Self::Rsync(draft) => draft.validate(),
        }
    }

    pub(super) fn review_required(&self) -> &'static str {
        match self {
            Self::Sftp(_) => REVIEW_REQUIRED,
            Self::Rsync(_) => RSYNC_REVIEW_REQUIRED,
        }
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SftpDraft {
    pub(crate) kind: Kind,
    pub(crate) local_hint: String,
    pub(crate) remote: RemotePath,
}

impl SftpDraft {
    pub(super) fn validate(&self) -> Result<(), &'static str> {
        if !display_path(&self.local_hint) || !display_path(self.remote.as_str()) {
            return Err("Invalid imported SFTP paths");
        }
        Ok(())
    }

    pub(super) fn validate_adoption(&self, mode: Mode, local: PathBuf) -> Result<(), &'static str> {
        let remote = self.remote.clone();
        let direction = self.kind.direction();
        let action = match (mode, direction) {
            (Mode::Compare, _) => RemoteAction::Sync(sync::Copy {
                local,
                remote,
                direction,
                excludes: String::new(),
                replace_changed: false,
                standing: None,
                approval: None,
            }),
            (_, sync::Direction::Upload) => RemoteAction::Upload {
                local,
                remote,
                folder: mode == Mode::Folder,
            },
            (_, sync::Direction::Download) => RemoteAction::Download {
                local,
                remote,
                folder: mode == Mode::Folder,
            },
        };
        action.validate()
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RsyncDraft {
    pub(crate) direction: sync::Direction,
    pub(crate) java_sources: Vec<String>,
    pub(crate) java_target_root: String,
    pub(crate) java_delete: bool,
    pub(crate) java_sudo: bool,
}

impl RsyncDraft {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self.java_sources.is_empty()
            || self.java_sources.len() > MAX_RSYNC_SOURCES
            || !self.java_sources.iter().all(|path| display_path(path))
            || !display_path(&self.java_target_root)
        {
            return Err("Invalid imported rsync paths.");
        }
        Ok(())
    }

    pub(super) fn require_native_scope(&self) -> Result<(), &'static str> {
        self.validate()?;
        if self.java_sudo {
            return Err("Choose a reviewed passwordless sudo transfer for this Java draft.");
        }
        if self.java_delete {
            return Err("Choose a reviewed receiver folder for the Java delete option.");
        }
        if self.java_sources.iter().any(|path| path.ends_with('/')) {
            return Err(
                "A Java source ending in a slash copies a directory's contents. Native rsync currently publishes whole directories.",
            );
        }
        if self.direction == sync::Direction::Download
            && self.java_sources.iter().any(|source| {
                !rsync::valid_remote(source) || RemotePath::try_from(source.clone()).is_err()
            })
        {
            return Err(
                "Native rsync needs literal absolute server source paths with ordinary components.",
            );
        }
        if !cfg!(any(unix, windows)) {
            return Err(
                "Native rsync is unavailable on this platform. The draft remains available for backup and restore.",
            );
        }
        Ok(())
    }

    pub(super) fn existing_settings(&self) -> Result<rsync::ExistingDestination, &'static str> {
        self.validate()?;
        if self.java_sudo {
            return Err("Choose a reviewed passwordless sudo transfer for this Java draft.");
        }
        if self.java_sources.len() != 1 {
            return Err("An existing rsync transfer requires one source directory.");
        }
        let supported = match self.direction {
            Direction::Upload => cfg!(any(unix, windows)),
            Direction::Download => rsync::LOCAL_EXISTING_SUPPORTED,
        };
        if !supported {
            return Err("Scheduled rsync is unavailable on this platform.");
        }
        if self.direction == Direction::Download {
            let source = self.java_sources[0].trim_end_matches('/');
            if !rsync::valid_remote(source) || RemotePath::try_from(source.to_owned()).is_err() {
                return Err(
                    "Choose a literal absolute server source directory for the rsync download.",
                );
            }
        }
        Ok(rsync::ExistingDestination {
            delete: self.java_delete,
            include_source_name: !self.java_sources[0].ends_with('/'),
        })
    }

    pub(super) fn existing_local_file_source(&self) -> Result<RemotePath, &'static str> {
        self.validate()?;
        let [source] = self.java_sources.as_slice() else {
            return Err("Choose one Java server file for the existing local destination.");
        };
        if self.direction != Direction::Download || !rsync::LOCAL_EXISTING_SUPPORTED {
            return Err("Existing local file downloads are unavailable for this draft.");
        }
        if self.java_delete {
            return Err("The Java delete option requires an existing reviewed destination folder.");
        }
        if !rsync::valid_remote(source) {
            return Err("Choose a literal absolute server file without a trailing slash.");
        }
        RemotePath::try_from(source.clone())
    }

    pub(super) fn mixed_settings(
        &self,
        elevation: rsync::Elevation,
    ) -> Result<rsync::ExistingDestination, &'static str> {
        self.validate()?;
        if self.java_sudo != (elevation == rsync::Elevation::Passwordless) {
            return Err("Choose the elevation mode of the imported Java action.");
        }
        if !rsync::LOCAL_EXISTING_SUPPORTED {
            return Err("Mixed rsync is unavailable on this platform.");
        }
        if self.direction == Direction::Download
            && self.java_sources.iter().any(|raw| {
                let source = rsync::mixed::bare_source(raw, false);
                !rsync::valid_remote(source)
                    || RemotePath::try_from(source.to_owned()).is_err()
                    || self.java_sudo
                        && !kortty_server_archive::privileged_rsync::valid_path(source)
            })
        {
            return Err("Choose literal absolute server paths for the mixed rsync download.");
        }
        if self.java_sudo
            && self.direction == Direction::Upload
            && !kortty_server_archive::privileged_rsync::valid_path(
                self.java_target_root.trim_end_matches('/'),
            )
        {
            return Err(
                "Choose a literal absolute Linux destination for the imported passwordless rsync transfer.",
            );
        }
        Ok(rsync::ExistingDestination {
            delete: self.java_delete,
            include_source_name: false,
        })
    }
    pub(super) fn passwordless_settings(
        &self,
        directory: bool,
        existing: bool,
    ) -> Result<Option<rsync::ExistingDestination>, &'static str> {
        self.validate()?;
        if !self.java_sudo || !rsync::LOCAL_EXISTING_SUPPORTED {
            return Err("Passwordless rsync is unavailable for this draft.");
        }
        let [source] = self.java_sources.as_slice() else {
            return Err("Choose a mixed source transfer for this Java job.");
        };
        if !directory
            && (source.ends_with('/') || java_windows_path(source) && source.ends_with('\\'))
        {
            return Err("Choose a directory transfer for this Java source.");
        }
        if self.java_delete && !(existing && directory) {
            return Err("The Java delete option requires an existing reviewed destination folder.");
        }
        if existing && !directory {
            match self.direction {
                Direction::Upload => {
                    self.passwordless_existing_file_path()?;
                }
                Direction::Download => {
                    self.existing_local_file_source()?;
                }
            }
            return Ok(Some(rsync::ExistingDestination::FILE));
        }
        let remote = match self.direction {
            Direction::Upload => self.java_target_root.trim_end_matches('/'),
            Direction::Download => source.trim_end_matches('/'),
        };
        if !rsync::valid_remote(remote) || RemotePath::try_from(remote.to_owned()).is_err() {
            return Err(
                "Passwordless rsync needs a literal absolute Linux path for this Java route.",
            );
        }
        Ok(existing.then_some(rsync::ExistingDestination {
            delete: self.java_delete,
            include_source_name: !source.ends_with('/'),
        }))
    }

    pub(super) fn passwordless_existing_file_path(&self) -> Result<RemotePath, &'static str> {
        let [source] = self.java_sources.as_slice() else {
            return Err("Choose one Java source file for the protected destination.");
        };
        let root = self.java_target_root.trim_end_matches('/');
        let root = RemotePath::try_from(root.to_owned())?;
        // A trailing slash explicitly directs Java's rsync into a directory.
        let destination = if self.java_target_root.ends_with('/') {
            let name = source
                .rsplit(|character| {
                    character == '/' || character == '\\' && java_windows_path(source)
                })
                .next()
                .filter(|name| !name.is_empty())
                .ok_or("Choose a Java source with a literal file name.")?;
            root.child(name.as_bytes())?
        } else {
            root
        };
        if !kortty_server_archive::privileged_rsync::valid_path(destination.as_str()) {
            return Err("Choose a literal absolute Linux destination for the existing file.");
        }
        Ok(destination)
    }
}

fn java_windows_path(path: &str) -> bool {
    path.starts_with("\\\\")
        || matches!(path.as_bytes(), [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic())
}

fn display_path(path: &str) -> bool {
    !path.trim().is_empty()
        && path.len() <= 4096
        && path.chars().all(|character| {
            !character.is_control() && crate::session_journal::is_display_character(character)
        })
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Mode {
    File,
    Folder,
    Compare,
}
