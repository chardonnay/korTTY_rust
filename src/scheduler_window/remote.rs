use std::fmt;
use std::path::{Component, Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::sftp_window::privileged_file::ProtectedOperation;
use crate::sftp_window::validate_transfer_name;

use super::{
    privileged::{
        PrivilegedArchive, PrivilegedDirection, PrivilegedFileAction, PrivilegedTransfer,
    },
    rsync, sync,
};

const MAX_PATH_BYTES: usize = 4096;
const MAX_COMPONENTS: usize = 32;
pub(crate) const MAX_ARCHIVE_SOURCES: usize = 16;
pub(super) const MAX_ARCHIVE_SOURCE_TEXT: usize = MAX_ARCHIVE_SOURCES * (MAX_PATH_BYTES + 1);
const MAX_ARCHIVE_EXCLUDES: usize = 32;
const MAX_ARCHIVE_PATTERN_BYTES: usize = 256;
pub(crate) const MAX_ARCHIVE_EXCLUDE_TEXT: usize =
    MAX_ARCHIVE_EXCLUDES * (MAX_ARCHIVE_PATTERN_BYTES + 1);

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum RemoteAction {
    Mkdir {
        path: RemotePath,
    },
    Rename {
        from: RemotePath,
        to: RemotePath,
    },
    Chmod {
        path: RemotePath,
        mode: u16,
    },
    Chown {
        path: RemotePath,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uid: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gid: Option<u32>,
    },
    CopyRemote {
        from: RemotePath,
        to: RemotePath,
    },
    #[serde(alias = "archive_zip")]
    Archive(Archive),
    ServerArchive(Archive),
    PrivilegedArchive(PrivilegedArchive),
    Delete {
        path: RemotePath,
        confirmed_path: String,
        #[serde(default)]
        recursive: bool,
    },
    Upload {
        local: PathBuf,
        remote: RemotePath,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        folder: bool,
    },
    Download {
        local: PathBuf,
        remote: RemotePath,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        folder: bool,
    },
    Privileged(PrivilegedTransfer),
    PrivilegedFileAction(PrivilegedFileAction),
    Sync(sync::Copy),
    PrivilegedTreeUpload(sync::Copy),
    PrivilegedTreeDownload(sync::Copy),
    RsyncDownload(rsync::Transfer),
    RsyncUpload(rsync::Transfer),
}

impl RemoteAction {
    /// The Java action key stored in exported Session Journal metadata.
    pub(crate) fn automation_action(&self) -> &'static str {
        match self {
            Self::Mkdir { .. } => "SFTP_MKDIR",
            Self::Rename { .. } => "SFTP_RENAME",
            Self::Chmod { .. } => "SFTP_CHMOD",
            Self::Chown { .. } => "SFTP_CHOWN",
            Self::PrivilegedFileAction(metadata) => match metadata.operation {
                ProtectedOperation::Mkdir => "SFTP_MKDIR",
                ProtectedOperation::Chmod { .. } => "SFTP_CHMOD",
                ProtectedOperation::Chown { .. } => "SFTP_CHOWN",
                ProtectedOperation::Copy { .. } | ProtectedOperation::CopyTree { .. } => {
                    "SFTP_COPY_REMOTE"
                }
                ProtectedOperation::Rename { .. } => "SFTP_RENAME",
                ProtectedOperation::Delete { .. } => "SFTP_DELETE",
            },
            Self::CopyRemote { .. } => "SFTP_COPY_REMOTE",
            Self::Archive(_) | Self::ServerArchive(_) | Self::PrivilegedArchive(_) => {
                "SFTP_ARCHIVE"
            }
            Self::Delete { .. } => "SFTP_DELETE",
            Self::Upload { .. }
            | Self::PrivilegedTreeUpload(_)
            | Self::Privileged(PrivilegedTransfer {
                direction: PrivilegedDirection::Upload,
                ..
            }) => "SFTP_UPLOAD",
            Self::Download { .. }
            | Self::PrivilegedTreeDownload(_)
            | Self::Privileged(PrivilegedTransfer {
                direction: PrivilegedDirection::Download,
                ..
            }) => "SFTP_DOWNLOAD",
            Self::Sync(_) => "SFTP_SYNC",
            Self::RsyncDownload(_) | Self::RsyncUpload(_) => "RSYNC_SYNC",
        }
    }

    pub(super) fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Rename { from, to } if from == to => {
                Err("Choose a different remote rename destination")
            }
            Self::CopyRemote { from, to } if from.contains(to) => {
                Err("Choose a remote copy destination outside the source path")
            }
            Self::Archive(archive)
                if matches!(
                    archive.format,
                    ArchiveFormat::SevenZip | ArchiveFormat::SevenZipPassword
                ) =>
            {
                Err("Choose 7z from the file browser")
            }
            Self::Archive(archive) => archive.validate(),
            Self::ServerArchive(archive) => archive.server_spec().map(|_| ()),
            Self::PrivilegedArchive(archive) => archive.validate(),
            Self::Delete {
                path,
                confirmed_path,
                ..
            } if confirmed_path != path.as_str() => {
                Err("Repeat the exact remote path to authorize deletion")
            }
            Self::Chmod { mode, .. } if *mode > 0o7777 => {
                Err("Enter three or four octal permission digits")
            }
            Self::Chown { uid, gid, .. }
                if (uid.is_none() && gid.is_none())
                    || *uid == Some(u32::MAX)
                    || *gid == Some(u32::MAX) =>
            {
                Err("Enter a numeric UID or GID below 4294967295")
            }
            Self::Upload { local, .. } | Self::Download { local, .. } => validate_local_path(local),
            Self::Privileged(transfer) => transfer.validate(),
            Self::PrivilegedFileAction(action) => action.validate(),
            Self::Sync(copy)
            | Self::PrivilegedTreeUpload(copy)
            | Self::PrivilegedTreeDownload(copy) => {
                validate_local_path(&copy.local)?;
                let privileged_direction = match self {
                    Self::PrivilegedTreeUpload(_) => Some(sync::Direction::Upload),
                    Self::PrivilegedTreeDownload(_) => Some(sync::Direction::Download),
                    _ => None,
                };
                copy.validate_replacement(privileged_direction.is_some())?;
                if let Some(settings) = &copy.standing
                    && (privileged_direction.is_some() || settings.validate().is_err())
                {
                    return Err("Choose valid recurring SFTP limits for an ordinary folder copy.");
                }
                if let Some(direction) = privileged_direction {
                    if copy.direction != direction
                        || !crate::sftp_window::privileged_file::valid_path(copy.remote.as_str())
                    {
                        return Err(
                            "Choose an absolute Linux path and a matching privileged folder direction.",
                        );
                    }
                    if direction == sync::Direction::Download && !copy.excludes.is_empty() {
                        return Err("Privileged folder downloads review the complete source tree.");
                    }
                }
                crate::sftp_window::scheduled::validate_excludes(&copy.excludes)
                    .map_err(|_| "Enter up to 32 valid relative SFTP exclude globs.")?;
                if !copy.valid_approval() {
                    return Err("The scheduled SFTP review identifier is invalid.");
                }
                Ok(())
            }
            Self::RsyncDownload(download) => download.validate(false),
            Self::RsyncUpload(upload) => upload.validate(true),
            _ => Ok(()),
        }?;
        // Win32 OpenSSH reports a lexical REALPATH, cannot identify every
        // reparse point via LSTAT, and lacks an atomic no-replace directory
        // rename. Limit drive paths to operations without remote replacement,
        // deletion or recursive reads until those guarantees are available.
        let unsupported = match self {
            Self::Mkdir { .. } | Self::Download { folder: false, .. } => false,
            Self::Rename { from, to } | Self::CopyRemote { from, to } => {
                from.has_drive() || to.has_drive()
            }
            Self::Chmod { path, .. } | Self::Chown { path, .. } | Self::Delete { path, .. } => {
                path.has_drive()
            }
            Self::Upload { remote, .. } | Self::Download { remote, .. } => remote.has_drive(),
            Self::Privileged(_) | Self::PrivilegedFileAction(_) | Self::PrivilegedArchive(_) => {
                false
            }
            Self::Sync(copy)
            | Self::PrivilegedTreeUpload(copy)
            | Self::PrivilegedTreeDownload(copy) => copy.remote.has_drive(),
            Self::RsyncDownload(transfer) | Self::RsyncUpload(transfer) => {
                transfer.remote.has_drive()
            }
            Self::Archive(archive) | Self::ServerArchive(archive) => {
                archive.target.has_drive() || archive.sources.iter().any(RemotePath::has_drive)
            }
        };
        if unsupported {
            Err("Windows drive paths support creating directories and downloading regular files")
        } else {
            Ok(())
        }
    }

    pub(crate) fn transfers_files(&self) -> bool {
        matches!(
            self,
            Self::Upload { .. }
                | Self::Download { .. }
                | Self::Privileged(_)
                | Self::Archive(_)
                | Self::Sync(_)
                | Self::PrivilegedTreeUpload(_)
                | Self::PrivilegedTreeDownload(_)
                | Self::RsyncDownload(_)
                | Self::RsyncUpload(_)
        )
    }

    pub(crate) fn writes_local_file(&self) -> bool {
        matches!(
            self,
            Self::Download { .. }
                | Self::Privileged(PrivilegedTransfer {
                    direction: PrivilegedDirection::Download,
                    ..
                })
                | Self::RsyncDownload(_)
                | Self::PrivilegedTreeDownload(_)
                | Self::Sync(sync::Copy {
                    direction: sync::Direction::Download,
                    ..
                })
                | Self::Archive(Archive {
                    download: Some(_),
                    ..
                })
        )
    }

    pub(crate) fn uses_extended_timeout(&self) -> bool {
        self.transfers_files()
            || matches!(
                self,
                Self::CopyRemote { .. }
                    | Self::ServerArchive(_)
                    | Self::PrivilegedArchive(_)
                    | Self::Delete {
                        recursive: true,
                        ..
                    }
            )
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Archive {
    pub sources: Vec<RemotePath>,
    pub target: RemotePath,
    #[serde(default)]
    pub format: ArchiveFormat,
    pub level: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_credential: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excludes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<PathBuf>,
}

impl Archive {
    pub(crate) fn server_spec(
        &self,
    ) -> Result<kortty_server_archive::protocol::CreateSpec, &'static str> {
        use kortty_server_archive::protocol::{CreateFormat, CreateSpec};

        self.validate()?;
        if self.download.is_some() || self.password_credential.is_some() {
            return Err(
                "Server-only archives use a new server destination and no local download or password.",
            );
        }
        let format = match self.format {
            ArchiveFormat::Zip => CreateFormat::Zip,
            ArchiveFormat::Tar => CreateFormat::Tar,
            ArchiveFormat::TarBz2 => CreateFormat::TarBzip2,
            ArchiveFormat::SevenZip => CreateFormat::SevenZip,
            _ => return Err("Choose ZIP, TAR, TAR.BZ2 or 7z for server-only creation."),
        };
        Ok(CreateSpec {
            sources: self
                .sources
                .iter()
                .map(|source| source.as_str().into())
                .collect(),
            destination: self.target.as_str().into(),
            format,
            level: self.level,
            excludes: self.excludes.clone(),
        })
    }

    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self.format == ArchiveFormat::SevenZipPassword {
            return Err("Choose encrypted 7z for a local archive");
        }
        if self.sources.is_empty() || self.sources.len() > MAX_ARCHIVE_SOURCES {
            return Err("Choose between one and 16 absolute archive source paths");
        }
        if !self.target.as_str().ends_with(self.format.extension()) {
            return Err("Choose an archive target with the extension for the selected format");
        }
        if !self.format.valid_level(self.level) {
            return Err(match self.format {
                ArchiveFormat::SevenZip => "Choose 7z compression 0–9",
                _ => "Choose ZIP compression 0–9 or bzip2 compression 1–9",
            });
        }
        match (self.format, self.password_credential.as_deref()) {
            (ArchiveFormat::ZipPassword, Some(id))
                if id.len() == 32
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) => {}
            (ArchiveFormat::ZipPassword, _) => {
                return Err("Choose a ZIP archive password from the vault");
            }
            (_, Some(_)) => return Err("Only encrypted ZIP can use an archive password"),
            (_, None) => {}
        }
        for (index, source) in self.sources.iter().enumerate() {
            if source == &self.target
                || self.sources[..index]
                    .iter()
                    .any(|other| source.basename() == other.basename())
            {
                return Err("Archive sources must have distinct names and differ from the target");
            }
        }
        if let Some(download) = &self.download {
            validate_local_path(download)?;
        }
        self.exclusions()?;
        Ok(())
    }

    pub(crate) fn exclusions(&self) -> Result<ArchiveExclusions, &'static str> {
        ArchiveExclusions::compile(&self.excludes, !self.format.is_zip())
    }
}

pub(crate) struct ArchiveExclusions {
    absolute: GlobSet,
    relative: GlobSet,
}

impl ArchiveExclusions {
    pub(crate) fn compile_local(excludes: &[String]) -> Result<Self, &'static str> {
        Self::compile(excludes, true)
    }

    fn compile(excludes: &[String], recursive_patterns: bool) -> Result<Self, &'static str> {
        if excludes.len() > MAX_ARCHIVE_EXCLUDES {
            return Err("Enter at most 32 archive exclude patterns");
        }
        let mut absolute = GlobSetBuilder::new();
        let mut relative = GlobSetBuilder::new();
        let glob = |pattern: &str| {
            GlobBuilder::new(pattern)
                .backslash_escape(true)
                .literal_separator(false)
                .build()
                .map_err(|_| "Invalid archive exclude pattern")
        };
        for pattern in excludes {
            if pattern.len() > MAX_ARCHIVE_PATTERN_BYTES || pattern.chars().any(char::is_control) {
                return Err(
                    "An archive exclude pattern must be at most 256 bytes without control characters",
                );
            }
            let pattern = pattern.trim();
            if pattern.is_empty() {
                continue;
            }
            if pattern.starts_with('/') {
                absolute.add(glob(pattern)?);
            } else {
                let pattern = pattern.strip_prefix("./").unwrap_or(pattern);
                relative.add(glob(pattern)?);
                if recursive_patterns {
                    relative.add(glob(&format!("**/{pattern}"))?);
                }
            }
        }
        Ok(Self {
            absolute: absolute
                .build()
                .map_err(|_| "Archive exclude patterns are too complex")?,
            relative: relative
                .build()
                .map_err(|_| "Archive exclude patterns are too complex")?,
        })
    }
    pub(crate) fn matches(&self, path: &RemotePath, name: &str) -> bool {
        self.matches_paths(path.as_str(), name)
    }

    pub(crate) fn matches_paths(&self, absolute: &str, relative: &str) -> bool {
        self.absolute.is_match(absolute) || self.relative.is_match(relative)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormat {
    #[default]
    Zip,
    ZipPassword,
    Tar,
    TarBz2,
    SevenZip,
    SevenZipPassword,
}

impl ArchiveFormat {
    pub(super) const ALL: [Self; 4] = [Self::Zip, Self::ZipPassword, Self::Tar, Self::TarBz2];
    pub(super) const SERVER_ONLY: [Self; 4] = [Self::Zip, Self::Tar, Self::TarBz2, Self::SevenZip];

    pub(crate) fn is_zip(self) -> bool {
        matches!(self, Self::Zip | Self::ZipPassword)
    }

    pub(crate) fn is_password_protected(self) -> bool {
        matches!(self, Self::ZipPassword | Self::SevenZipPassword)
    }

    pub(crate) fn valid_level(self, level: u8) -> bool {
        level <= 9 && (level != 0 || self != Self::TarBz2)
    }

    pub(crate) fn level_hint(self) -> &'static str {
        match self {
            Self::Zip | Self::ZipPassword => "ZIP compression level (0–9)",
            Self::Tar => "Compression level (0–9)",
            Self::TarBz2 => "bzip2 compression level (1–9)",
            Self::SevenZip | Self::SevenZipPassword => "7z compression level (0–9)",
        }
    }

    pub(crate) fn stores_uncompressed(self, level: u8) -> bool {
        self == Self::Tar || (level == 0 && self != Self::TarBz2)
    }

    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Zip | Self::ZipPassword => ".zip",
            Self::Tar => ".tar",
            Self::TarBz2 => ".tar.bz2",
            Self::SevenZip | Self::SevenZipPassword => ".7z",
        }
    }
}

impl fmt::Display for ArchiveFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Zip => "ZIP",
            Self::ZipPassword => "ZIP (AES-256)",
            Self::Tar => "TAR",
            Self::TarBz2 => "TAR.BZ2",
            Self::SevenZip => "7z",
            Self::SevenZipPassword => "7z (AES-256)",
        })
    }
}

pub(super) fn validate_local_path(path: &Path) -> Result<(), &'static str> {
    if !path.is_absolute()
        || path.file_name().is_none()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
        || path
            .to_str()
            .is_none_or(|path| path.len() > MAX_PATH_BYTES || path.chars().any(char::is_control))
    {
        Err("Enter an absolute local path of at most 4 KiB without traversal")
    } else {
        Ok(())
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) struct RemotePath(String);

pub(crate) fn windows_drive_root(path: &str) -> Option<&str> {
    match path.as_bytes() {
        [b'/', letter, b':', b'/', ..] if letter.is_ascii_alphabetic() => Some(&path[..4]),
        _ => None,
    }
}

pub(crate) fn windows_drive_ancestors(path: &str) -> impl Iterator<Item = &str> {
    let drive = windows_drive_root(path);
    drive
        .filter(|root| path.len() > root.len())
        .into_iter()
        .chain(
            path.match_indices('/')
                .filter(move |(index, _)| drive.is_some_and(|root| *index >= root.len()))
                .map(|(index, _)| &path[..index]),
        )
}

impl RemotePath {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn parent(&self) -> &str {
        &self.0[..self.0.rfind('/').unwrap_or(0).max(self.root().len())]
    }

    pub(crate) fn root(&self) -> &str {
        windows_drive_root(&self.0).unwrap_or("/")
    }

    fn has_drive(&self) -> bool {
        windows_drive_root(&self.0).is_some()
    }

    pub(crate) fn basename(&self) -> &str {
        self.0.rsplit_once('/').map_or(&self.0, |(_, name)| name)
    }

    pub(crate) fn contains(&self, path: &Self) -> bool {
        self == path
            || path
                .as_str()
                .strip_prefix(self.as_str())
                .is_some_and(|suffix| suffix.starts_with('/'))
    }

    pub(crate) fn child(&self, name: &[u8]) -> Result<Self, &'static str> {
        validate_transfer_name(name).map_err(|_| "The tree contains an unsafe remote name")?;
        let name = std::str::from_utf8(name).map_err(|_| "The tree contains a non-UTF-8 name")?;
        Self::try_from(format!("{}/{name}", self.0))
    }

    pub(crate) fn prefixes(&self) -> impl Iterator<Item = &str> {
        let root_len = self.root().len();
        self.0
            .match_indices('/')
            .filter(move |(index, _)| *index >= root_len)
            .map(|(index, _)| &self.0[..index])
            .chain(std::iter::once(self.as_str()))
    }
}

impl TryFrom<String> for RemotePath {
    type Error = &'static str;

    fn try_from(path: String) -> Result<Self, Self::Error> {
        let drive = windows_drive_root(&path);
        let components = path
            .strip_prefix(drive.unwrap_or("/"))
            .ok_or("Enter an absolute SFTP path beginning with /")?;
        if path.len() > MAX_PATH_BYTES || path.chars().any(char::is_control) {
            return Err("Enter an absolute SFTP path of at most 4 KiB without control characters");
        }
        for (index, component) in components.split('/').enumerate() {
            if index == MAX_COMPONENTS {
                return Err("An SFTP action path supports up to 32 components");
            }
            validate_transfer_name(component.as_bytes())
                .map_err(|_| "Enter an absolute SFTP path with ordinary components")?;
            if drive.is_some() && component.contains(['<', '>', '"', '|', '?', '*']) {
                return Err("Enter a Windows drive path with ordinary components");
            }
        }
        Ok(Self(path))
    }
}

impl From<RemotePath> for String {
    fn from(path: RemotePath) -> Self {
        path.0
    }
}

pub(super) fn mode_from_text(value: &str) -> Result<u16, &'static str> {
    let value = value.trim();
    if (3..=4).contains(&value.len()) && value.bytes().all(|byte| matches!(byte, b'0'..=b'7')) {
        u16::from_str_radix(value, 8).map_err(|_| "Enter three or four octal permission digits")
    } else {
        Err("Enter three or four octal permission digits")
    }
}

pub(super) fn owner_id_from_text(value: &str) -> Result<Option<u32>, &'static str> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() <= 10
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && let Ok(id) = value.parse::<u32>()
        && id != u32::MAX
    {
        Ok(Some(id))
    } else {
        Err("Enter a numeric UID or GID below 4294967295")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_only_archive_persistence_preserves_its_transfer_boundary() {
        let action: RemoteAction = serde_json::from_value(serde_json::json!({
            "kind": "server_archive",
            "sources": ["/private/source"],
            "target": "/private/reviewed.tar.bz2",
            "format": "tar_bz2",
            "level": 6
        }))
        .unwrap();
        assert!(action.validate().is_ok());
        assert!(!action.transfers_files());
        assert!(!action.writes_local_file());
        assert!(matches!(
            serde_json::from_slice::<RemoteAction>(&serde_json::to_vec(&action).unwrap()).unwrap(),
            RemoteAction::ServerArchive(archive) if archive.server_spec().unwrap().destination == "/private/reviewed.tar.bz2"
        ));
        let RemoteAction::ServerArchive(mut archive) = action else {
            unreachable!()
        };
        archive.download = Some(std::env::temp_dir().join("reviewed.tar.bz2"));
        assert!(RemoteAction::ServerArchive(archive).validate().is_err());
    }

    #[test]
    fn scheduled_file_actions_use_java_automation_journal_keys() {
        use serde_json::json;

        for (action, key) in [
            (json!({"kind": "mkdir", "path": "/work/dir"}), "SFTP_MKDIR"),
            (
                json!({"kind": "rename", "from": "/work/old", "to": "/work/new"}),
                "SFTP_RENAME",
            ),
            (
                json!({"kind": "chmod", "path": "/work/file", "mode": 0o640}),
                "SFTP_CHMOD",
            ),
            (
                json!({"kind": "chown", "path": "/work/file", "gid": 1001}),
                "SFTP_CHOWN",
            ),
            (
                json!({"kind": "copy_remote", "from": "/work/file", "to": "/work/copy"}),
                "SFTP_COPY_REMOTE",
            ),
            (
                json!({"kind": "archive", "sources": ["/work/file"], "target": "/work/test.zip", "level": 6}),
                "SFTP_ARCHIVE",
            ),
            (
                json!({"kind": "delete", "path": "/work/file", "confirmed_path": "/work/file"}),
                "SFTP_DELETE",
            ),
            (
                json!({"kind": "upload", "local": "/tmp/file", "remote": "/work/file"}),
                "SFTP_UPLOAD",
            ),
            (
                json!({"kind": "download", "local": "/tmp/tree", "remote": "/work/tree", "folder": true}),
                "SFTP_DOWNLOAD",
            ),
            (
                json!({"kind": "privileged", "local": "/tmp/file", "remote": "/work/file", "direction": "upload"}),
                "SFTP_UPLOAD",
            ),
            (
                json!({"kind": "privileged", "local": "/tmp/file", "remote": "/work/file", "direction": "download"}),
                "SFTP_DOWNLOAD",
            ),
            (
                json!({"kind": "sync", "local": "/tmp/tree", "remote": "/work/tree", "direction": "download"}),
                "SFTP_SYNC",
            ),
            (
                json!({"kind": "privileged_tree_upload", "local": "/tmp/tree", "remote": "/work/tree", "direction": "upload"}),
                "SFTP_UPLOAD",
            ),
            (
                json!({"kind": "privileged_tree_download", "local": "/tmp/tree", "remote": "/work/tree", "direction": "download"}),
                "SFTP_DOWNLOAD",
            ),
            (
                json!({"kind": "rsync_download", "program": "/usr/bin/rsync", "local": "/tmp/file", "remote": "/work/file"}),
                "RSYNC_SYNC",
            ),
        ] {
            let action: RemoteAction = serde_json::from_value(action).unwrap();
            assert_eq!(action.automation_action(), key);
        }
    }

    #[test]
    fn seven_zip_is_exclusive_to_browser_archives() {
        for (format, key, server_validation) in [
            (ArchiveFormat::SevenZip, "seven_zip", Ok(())),
            (
                ArchiveFormat::SevenZipPassword,
                "seven_zip_password",
                Err("Choose encrypted 7z for a local archive"),
            ),
        ] {
            let action: RemoteAction = serde_json::from_value(serde_json::json!({
                "kind": "archive",
                "sources": ["/work/project"],
                "target": "/work/project.7z",
                "format": key,
                "level": 0,
            }))
            .unwrap();
            let RemoteAction::Archive(request) = &action else {
                panic!("expected an archive request");
            };
            assert_eq!(request.format, format);
            assert_eq!(request.validate(), server_validation);
            assert_eq!(action.validate(), Err("Choose 7z from the file browser"));
        }
    }

    #[test]
    fn windows_drive_paths_have_an_ordinary_leaf_beneath_the_drive() {
        let path = RemotePath::try_from("/c:/project data/subdir/file".to_owned()).unwrap();
        assert_eq!(path.root(), "/c:/");
        assert_eq!(path.parent(), "/c:/project data/subdir");
        assert_eq!(path.basename(), "file");
        assert_eq!(
            path.prefixes().collect::<Vec<_>>(),
            ["/c:/project data", "/c:/project data/subdir", path.as_str()]
        );
        assert_eq!(
            windows_drive_ancestors(path.as_str()).collect::<Vec<_>>(),
            ["/c:/", "/c:/project data", "/c:/project data/subdir"]
        );
        assert_eq!(
            RemotePath::try_from("/C:/top".to_owned()).unwrap().parent(),
            "/C:/"
        );
        assert!(windows_drive_ancestors("/C:/").next().is_none());
        let posix = RemotePath::try_from("/project/subdir".to_owned()).unwrap();
        assert_eq!(posix.root(), "/");
        assert_eq!(posix.parent(), "/project");
        assert_eq!(
            posix.prefixes().collect::<Vec<_>>(),
            ["/project", "/project/subdir"]
        );
        assert!(windows_drive_ancestors(posix.as_str()).next().is_none());

        for path in [
            "/",
            "/C:",
            "/C:/",
            "C:/relative",
            "/1:/file",
            "/C://file",
            "/C:/../file",
            "/C:/subdir/./file",
            "/C:/file:stream",
            "/dir/C:/file",
            "/C:/con.txt",
            "/C:/LPT¹",
            "/C:/file.",
            "/C:/file ",
            "/C:/f?le",
            "/C:/back\\slash",
            "//server/share/file",
        ] {
            assert!(RemotePath::try_from(path.to_owned()).is_err(), "{path}");
        }
    }

    #[test]
    fn windows_drive_paths_keep_mutation_guards() {
        let path = |text: &str| RemotePath::try_from(text.to_owned()).unwrap();
        assert!(
            RemoteAction::Mkdir {
                path: path("/C:/project/new")
            }
            .validate()
            .is_ok()
        );
        for action in [
            RemoteAction::Rename {
                from: path("/C:/project/old"),
                to: path("/C:/project/new"),
            },
            RemoteAction::Delete {
                path: path("/C:/project/old"),
                confirmed_path: "/C:/project/old".into(),
                recursive: false,
            },
        ] {
            assert_eq!(
                action.validate(),
                Err(
                    "Windows drive paths support creating directories and downloading regular files"
                )
            );
        }
        assert!(
            RemoteAction::Delete {
                path: path("/project/old"),
                confirmed_path: "/project/old".into(),
                recursive: true,
            }
            .validate()
            .is_ok()
        );
    }
}
