use std::io::{Read, Write};

use kortty_jobs::Cancellation;
use kortty_ssh::sftp::{Attributes, Error, FileKind, Sftp};

use crate::scheduler_window::RemotePath;

use super::{Failure, before_change, verify_entry};

const MAX_ENTRIES: usize = 4096;
const MAX_PATH_BYTES: usize = 4 * 1024 * 1024;
const MAX_LISTING_BYTES: usize = 2 * 1024 * 1024;
const LIMIT: &str = "The SFTP deletion tree exceeds its entry or path budget";

struct Entry {
    path: RemotePath,
    expected: Attributes,
    children: Vec<Entry>,
}

struct Budget {
    entries: usize,
    path_bytes: usize,
}

pub(super) fn delete(
    client: &mut Sftp<impl Read, impl Write>,
    path: &RemotePath,
    recursive: bool,
    cancellation: &Cancellation,
    confirmed_deletions: &mut usize,
) -> Result<String, Failure> {
    let mut budget = Budget {
        entries: 1,
        path_bytes: path.as_str().len(),
    };
    let plan = Entry::inspect(client, path.clone(), recursive, cancellation, &mut budget)?;
    if let Err(error) = plan.remove(client, cancellation, confirmed_deletions) {
        before_change(cancellation)?;
        return Err(if *confirmed_deletions == 0 {
            error
        } else {
            Failure::failed("SFTP deletion stopped after removing part of the remote tree")
        });
    }
    Ok(if recursive {
        format!("Deleted {confirmed_deletions} remote entries via SFTP")
    } else {
        "Deleted the remote entry via SFTP".into()
    })
}

fn child_paths(
    client: &mut Sftp<impl Read, impl Write>,
    parent: &RemotePath,
    budget: &mut Budget,
) -> Result<Vec<RemotePath>, Failure> {
    let available = MAX_ENTRIES - budget.entries;
    // SFTP listings may also include `.` and `..`, which the codec discards.
    let mut entries = client
        .read_dir_limited(parent.as_str(), available + 2, MAX_LISTING_BYTES)
        .map_err(|error| match error {
            Error::Limit(_) => Failure::blocked(LIMIT),
            _ => Failure::failed("Could not list the checked remote deletion directory"),
        })?;
    if entries.len() > available {
        return Err(Failure::blocked(LIMIT));
    }
    entries.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    if entries.windows(2).any(|pair| pair[0].name == pair[1].name) {
        return Err(Failure::failed("The SFTP server listed duplicate names"));
    }
    budget.entries += entries.len();
    entries
        .into_iter()
        .map(|entry| {
            let path = parent.child(&entry.name).map_err(Failure::blocked)?;
            budget.path_bytes += path.as_str().len();
            if budget.path_bytes > MAX_PATH_BYTES {
                return Err(Failure::blocked(LIMIT));
            }
            Ok(path)
        })
        .collect()
}

impl Entry {
    fn inspect(
        client: &mut Sftp<impl Read, impl Write>,
        path: RemotePath,
        recursive: bool,
        cancellation: &Cancellation,
        budget: &mut Budget,
    ) -> Result<Self, Failure> {
        before_change(cancellation)?;
        let expected = verify_entry(client, path.as_str())?;
        if expected.uid.is_none()
            || expected.gid.is_none()
            || expected.modified.is_none()
            || expected.size.is_none()
        {
            return Err(Failure::failed(
                "The SFTP server did not provide enough metadata for checked deletion",
            ));
        }
        let children = if recursive && expected.kind() == FileKind::Directory {
            // RemotePath bounds components, including when joining listed names.
            child_paths(client, &path, budget)?
                .into_iter()
                .map(|child| Self::inspect(client, child, true, cancellation, budget))
                .collect::<Result<_, _>>()?
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            expected,
            children,
        })
    }

    fn verify(
        &self,
        client: &mut Sftp<impl Read, impl Write>,
        removed_children: bool,
    ) -> Result<(), Failure> {
        let current = verify_entry(client, self.path.as_str())?;
        let stable_metadata = |mut attrs: Attributes| {
            attrs.accessed = None;
            if removed_children {
                // Earlier planned child removals can change these values.
                attrs.modified = None;
                attrs.size = None;
            }
            attrs
        };
        if stable_metadata(current) != stable_metadata(self.expected) {
            return Err(Failure::failed("The remote deletion target changed"));
        }
        Ok(())
    }

    fn remove(
        &self,
        client: &mut Sftp<impl Read, impl Write>,
        cancellation: &Cancellation,
        removed: &mut usize,
    ) -> Result<(), Failure> {
        before_change(cancellation)?;
        self.verify(client, false)?;
        for child in &self.children {
            child.remove(client, cancellation, removed)?;
        }
        if !self.children.is_empty() {
            self.verify(client, true)?;
        }
        before_change(cancellation)?;
        let result = if self.expected.kind() == FileKind::Directory {
            client.remove_dir(self.path.as_str())
        } else {
            client.remove_file(self.path.as_str())
        };
        result.map_err(|error| {
            if error.connection_lost() {
                Failure::failed("SFTP lost the deletion acknowledgement. The entry may be gone.")
            } else {
                Failure::failed("The SFTP server did not remove the remote entry")
            }
        })?;
        *removed += 1;
        Ok(())
    }
}
