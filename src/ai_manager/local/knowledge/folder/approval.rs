//! The directory and ignore rules approved for automatic source discovery.

use std::io;

use serde::{Deserialize, Serialize};

use crate::private_files::Directory;

use super::super::source::FileSizeLimit;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Approval {
    directory: Identity,
    gitignore: Option<[u8; 32]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_file_bytes: Option<FileSizeLimit>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    large_corpus: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Identity {
    Unix(u64, u64),
    Windows(u64, [u8; 16]),
}

impl Approval {
    pub(super) fn new(
        directory: &Directory,
        gitignore: Option<[u8; 32]>,
        max_file_bytes: Option<FileSizeLimit>,
        large_corpus: bool,
    ) -> io::Result<Self> {
        Ok(Self {
            directory: identity(directory)?,
            gitignore,
            max_file_bytes,
            large_corpus,
        })
    }

    pub(super) fn same_directory(&self, directory: &Directory) -> io::Result<bool> {
        Ok(self.directory == identity(directory)?)
    }

    pub(super) fn large_corpus(&self) -> bool {
        self.large_corpus
    }
}

fn identity(directory: &Directory) -> io::Result<Identity> {
    let (volume, id) = directory.identity()?;
    #[cfg(unix)]
    let directory = Identity::Unix(volume, id);
    #[cfg(windows)]
    let directory = Identity::Windows(volume, id);
    Ok(directory)
}
