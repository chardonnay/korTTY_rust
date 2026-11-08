use std::fmt;
use std::path::{Component, Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::sftp_window::validate_transfer_name;

const MAX_PATH_BYTES: usize = 4096;
const MAX_COMPONENTS: usize = 32;
pub(super) const MAX_ARCHIVE_SOURCES: usize = 16;
pub(super) const MAX_ARCHIVE_SOURCE_TEXT: usize = MAX_ARCHIVE_SOURCES * (MAX_PATH_BYTES + 1);
const MAX_ARCHIVE_EXCLUDES: usize = 32;
const MAX_ARCHIVE_PATTERN_BYTES: usize = 256;
pub(super) const MAX_ARCHIVE_EXCLUDE_TEXT: usize =
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
}

impl RemoteAction {
    pub(super) fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Rename { from, to } if from == to => {
                Err("Choose a different remote rename destination")
            }
            Self::CopyRemote { from, to } if from.contains(to) => {
                Err("Choose a remote copy destination outside the source path")
            }
            Self::Archive(archive) => archive.validate(),
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
            _ => Ok(()),
        }
    }

    pub(crate) fn transfers_files(&self) -> bool {
        matches!(
            self,
            Self::Upload { .. } | Self::Download { .. } | Self::Archive(_)
        )
    }

    pub(crate) fn writes_local_file(&self) -> bool {
        matches!(
            self,
            Self::Download { .. }
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
    fn validate(&self) -> Result<(), &'static str> {
        if self.sources.is_empty() || self.sources.len() > MAX_ARCHIVE_SOURCES {
            return Err("Choose between one and 16 absolute archive source paths");
        }
        if !self.target.as_str().ends_with(self.format.extension()) {
            return Err("Choose an archive target with the extension for the selected format");
        }
        if self.level > 9 || (self.format == ArchiveFormat::TarBz2 && self.level == 0) {
            return Err("Choose ZIP compression 0–9 or bzip2 compression 1–9");
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
        if self.excludes.len() > MAX_ARCHIVE_EXCLUDES {
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
        for pattern in &self.excludes {
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
                if !self.format.is_zip() {
                    relative.add(glob(&format!("**/{pattern}"))?);
                }
            }
        }
        Ok(ArchiveExclusions {
            absolute: absolute
                .build()
                .map_err(|_| "Archive exclude patterns are too complex")?,
            relative: relative
                .build()
                .map_err(|_| "Archive exclude patterns are too complex")?,
        })
    }
}

pub(crate) struct ArchiveExclusions {
    absolute: GlobSet,
    relative: GlobSet,
}

impl ArchiveExclusions {
    pub(crate) fn matches(&self, path: &RemotePath, name: &str) -> bool {
        self.absolute.is_match(path.as_str()) || self.relative.is_match(name)
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
}

impl ArchiveFormat {
    pub(super) const ALL: [Self; 4] = [Self::Zip, Self::ZipPassword, Self::Tar, Self::TarBz2];

    pub(crate) fn is_zip(self) -> bool {
        matches!(self, Self::Zip | Self::ZipPassword)
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Zip | Self::ZipPassword => ".zip",
            Self::Tar => ".tar",
            Self::TarBz2 => ".tar.bz2",
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
        })
    }
}

fn validate_local_path(path: &Path) -> Result<(), &'static str> {
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

impl RemotePath {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn parent(&self) -> &str {
        &self.0[..self.0.rfind('/').unwrap_or(0).max(1)]
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
        self.0
            .match_indices('/')
            .skip(1)
            .map(|(index, _)| &self.0[..index])
            .chain(std::iter::once(self.as_str()))
    }
}

impl TryFrom<String> for RemotePath {
    type Error = &'static str;

    fn try_from(path: String) -> Result<Self, Self::Error> {
        let components = path
            .strip_prefix('/')
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
