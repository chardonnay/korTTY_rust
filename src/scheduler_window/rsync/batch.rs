//! Maps a list of literal sources into one reviewed target directory.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use iced::widget::{column, text};
use kortty_server_archive::rsync_tree::Inventory;
use serde::{Deserialize, Serialize};

#[cfg(any(unix, windows, test))]
use super::{Approval, Elevation};
#[cfg(any(unix, windows))]
use super::{DirectoryReview, DirectoryUploadReview, UploadReview};
use super::{MAX_FILE_BYTES, MAX_TREE_ENTRIES, Message, RemotePath, Transfer, valid_remote};

pub(crate) const MAX_SOURCES: usize = 16;
pub(in crate::scheduler_window) const MAX_SOURCE_TEXT: usize = MAX_SOURCES * 4096;
const BAD_BATCH: &str =
    "An rsync batch requires distinct source names, valid literal paths and up to 16 sources.";
const TOO_LARGE: &str = "The rsync batch exceeds 256 MiB or 4,096 entries.";

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Paths {
    local: PathBuf,
    remote: RemotePath,
}

pub(crate) struct Plan {
    upload: bool,
    directory: bool,
    paths: Vec<Paths>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Review {
    upload: bool,
    directory: bool,
    members: Vec<Member>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Member {
    paths: Paths,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    contents: Option<Contents>,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Contents {
    File { bytes: u64, digest: [u8; 32] },
    Directory { inventory: Inventory },
}

impl Transfer {
    pub(crate) fn batch_plan(&self, upload: bool) -> Result<Option<Plan>, &'static str> {
        if self.additional_sources.is_empty() {
            return Ok(None);
        }
        if self.uses_existing_destination() {
            return Err("Transfers into existing rsync folders require one source directory.");
        }
        if self.additional_sources.len() >= MAX_SOURCES {
            return Err(BAD_BATCH);
        }
        let first = if upload {
            self.local.to_str().ok_or(BAD_BATCH)?
        } else {
            self.remote.as_str()
        };
        let mut names = HashSet::with_capacity(self.additional_sources.len() + 1);
        let mut paths = Vec::with_capacity(self.additional_sources.len() + 1);
        for source in
            std::iter::once(first).chain(self.additional_sources.iter().map(String::as_str))
        {
            let (name, local, remote) = if upload {
                let local = PathBuf::from(source);
                let name = local
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or(BAD_BATCH)?;
                let remote = self.remote.child(name.as_bytes()).map_err(|_| BAD_BATCH)?;
                (name.to_ascii_lowercase(), local, remote)
            } else {
                let remote = RemotePath::try_from(source.to_owned()).map_err(|_| BAD_BATCH)?;
                let name = remote.basename();
                (name.to_ascii_lowercase(), self.local.join(name), remote)
            };
            super::super::remote::validate_local_path(&local)?;
            let local_text = local.to_str().ok_or(BAD_BATCH)?;
            // Windows canonicalization prefixes a literal path with \\?\.
            #[cfg(windows)]
            let local_text = local_text.strip_prefix(r"\\?\").unwrap_or(local_text);
            if local == self.program
                || local_text.contains(['*', '?', '[', ']'])
                || !valid_remote(remote.as_str())
                || !names.insert(name)
            {
                return Err(BAD_BATCH);
            }
            paths.push(Paths { local, remote });
        }
        Ok(Some(Plan {
            upload,
            directory: self.directory,
            paths,
        }))
    }
}

impl Plan {
    pub(crate) fn paths(&self) -> impl Iterator<Item = (&Path, &RemotePath)> {
        self.paths
            .iter()
            .map(|paths| (paths.local.as_path(), &paths.remote))
    }

    pub(super) fn review(self) -> Result<Review, String> {
        let mut members = Vec::with_capacity(self.paths.len());
        let mut total = (0, 0);
        for paths in self.paths {
            let contents = if !self.upload {
                None
            } else if self.directory {
                let inventory = crate::job_executor::rsync::upload_tree_inventory(&paths.local)
                    .map_err(|error| error.to_string())?;
                Some(Contents::Directory { inventory })
            } else {
                let (bytes, digest) = crate::job_executor::rsync::upload_digest(&paths.local)
                    .map_err(|error| error.to_string())?;
                Some(Contents::File { bytes, digest })
            };
            if let Some(contents) = &contents {
                contents.add_to(&mut total)?;
            }
            members.push(Member { paths, contents });
        }
        Ok(Review {
            upload: self.upload,
            directory: self.directory,
            members,
        })
    }
}

impl Contents {
    fn add_to(&self, (bytes, entries): &mut (u64, usize)) -> Result<(), &'static str> {
        let (size, count) = match self {
            Self::File { bytes, .. } => (*bytes, 1),
            Self::Directory { inventory } if inventory.valid() => {
                (inventory.bytes, inventory.entries as usize)
            }
            _ => return Err(TOO_LARGE),
        };
        *bytes = bytes.checked_add(size).ok_or(TOO_LARGE)?;
        *entries = entries.checked_add(count).ok_or(TOO_LARGE)?;
        if *bytes > MAX_FILE_BYTES || *entries > MAX_TREE_ENTRIES {
            return Err(TOO_LARGE);
        }
        Ok(())
    }
}

impl Review {
    pub(super) fn matches(&self, plan: &Plan) -> bool {
        self.upload == plan.upload
            && self.directory == plan.directory
            && self
                .members
                .iter()
                .map(|member| &member.paths)
                .eq(&plan.paths)
    }

    pub(super) fn validate(&self) -> Result<(), &'static str> {
        if !(2..=MAX_SOURCES).contains(&self.members.len()) {
            return Err(BAD_BATCH);
        }
        let mut total = (0, 0);
        for member in &self.members {
            match (self.upload, self.directory, &member.contents) {
                (false, _, None) => {}
                (true, false, Some(contents @ Contents::File { .. }))
                | (true, true, Some(contents @ Contents::Directory { .. })) => {
                    contents.add_to(&mut total)?;
                }
                _ => return Err("The reviewed rsync batch does not match the operation."),
            }
        }
        Ok(())
    }

    #[cfg(any(unix, windows))]
    pub(crate) fn transfers<'a>(
        &'a self,
        approval: &'a Approval,
    ) -> impl Iterator<Item = Transfer> + 'a {
        self.members.iter().map(move |member| {
            let Paths { local, remote } = member.paths.clone();
            let (upload, directory_upload) = match member.contents {
                Some(Contents::File { bytes, digest }) => (
                    Some(UploadReview {
                        source: local.clone(),
                        destination: remote.clone(),
                        bytes,
                        digest,
                    }),
                    None,
                ),
                Some(Contents::Directory { inventory }) => (
                    None,
                    Some(Box::new(DirectoryUploadReview {
                        source: local.clone(),
                        destination: remote.clone(),
                        inventory,
                    })),
                ),
                None => (None, None),
            };
            let directory = (!self.upload && self.directory).then(|| {
                Box::new(DirectoryReview {
                    source: remote.clone(),
                    destination: local.clone(),
                })
            });
            Transfer {
                program: approval.program.clone(),
                local,
                remote,
                directory: self.directory,
                existing: None,
                existing_download: None,
                mixed: None,
                additional_sources: Vec::new(),
                routes: Vec::new(),
                standing: None,
                elevation: Elevation::Ordinary,
                approval: Some(Approval {
                    program: approval.program.clone(),
                    digest: approval.digest,
                    reference: approval.reference.clone(),
                    pins: approval.pins.clone(),
                    upload,
                    directory,
                    directory_upload,
                    existing: None,
                    existing_download: None,
                    existing_file_download: None,
                    mixed: None,
                    batch: None,
                    download: None,
                    privileged: None,
                }),
            }
        })
    }

    pub(super) fn view(&self) -> iced::widget::Column<'_, Message> {
        self.members.iter().fold(
            column![text(format!("{} reviewed transfers", self.members.len())).size(14)].spacing(4),
            |items, member| {
                let local = member.paths.local.display();
                let remote = member.paths.remote.as_str();
                let transfer = if self.upload {
                    format!("{local} → {remote}")
                } else {
                    format!("{remote} → {local}")
                };
                let details = match &member.contents {
                    Some(Contents::File { bytes, digest }) => {
                        Some(format!("{bytes} bytes · SHA-256 {}", fingerprint(digest)))
                    }
                    Some(Contents::Directory { inventory }) => Some(format!(
                        "{} entries · {} bytes · inventory SHA-256 {}",
                        inventory.entries,
                        inventory.bytes,
                        fingerprint(&inventory.digest),
                    )),
                    None => None,
                };
                items.push(column![
                    text(transfer),
                    details.map(|value| text(value).size(12))
                ])
            },
        )
    }
}

fn fingerprint(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(in crate::scheduler_window) fn sources(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_review_binds_each_literal_source_and_derived_vacant_destination() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let mut transfer = Transfer {
            program: root.join("rsync"),
            local: root.join("downloads"),
            remote: RemotePath::try_from("/incoming/data".to_owned()).unwrap(),
            directory: false,
            existing: None,
            existing_download: None,
            mixed: None,
            additional_sources: vec!["/logs/notes".to_owned()],
            routes: Vec::new(),
            standing: None,
            elevation: Elevation::Ordinary,
            approval: None,
        };
        let plan = transfer.batch_plan(false).unwrap().unwrap();
        assert_eq!(plan.paths[0].local, root.join("downloads/data"));
        assert_eq!(plan.paths[1].local, root.join("downloads/notes"));
        let review = plan.review().unwrap();
        let reference = serde_json::from_value(serde_json::json!({
            "id": 1,
            "target": {"host": "server.example", "port": 22, "username": "user"}
        }))
        .unwrap();
        transfer.approval = Some(Approval {
            program: transfer.program.clone(),
            digest: [1; 32],
            reference,
            pins: vec!["server.example SHA256:fixture".into()],
            upload: None,
            directory: None,
            directory_upload: None,
            existing: None,
            existing_download: None,
            existing_file_download: None,
            mixed: None,
            batch: Some(Box::new(review)),
            download: None,
            privileged: None,
        });
        assert!(transfer.validate(false).is_ok());
        assert!(transfer.validate(true).is_err());
        let encoded = serde_json::to_string(&transfer).unwrap();
        let mut restored: Transfer = serde_json::from_str(&encoded).unwrap();
        assert!(restored.validate(false).is_ok());
        restored.local = root.join("other");
        assert!(restored.validate(false).is_err());
        transfer.additional_sources[0] = "/logs/DATA".into();
        assert!(transfer.validate(false).is_err());
        transfer.additional_sources[0] = "/logs/*.txt".into();
        assert!(transfer.validate(false).is_err());
        transfer.additional_sources.clear();
        assert!(transfer.validate(false).is_err());
    }
}
