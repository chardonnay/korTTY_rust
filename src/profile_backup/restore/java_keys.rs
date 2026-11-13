//! Preflight and replay of an additive managed SSH key catalogue update.

use std::fs::File;
use std::io::{self, Write as _};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::super::java_credential_restore::{ManagedKeys, digest};
use super::{Snapshot, data, invalid, missing, read_bounded};
use crate::private_files::Directory;
use crate::ssh_key_catalog::{self, CATALOG, MANAGED, PreparedManagedIdentity};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Revision {
    before: Option<[u8; 32]>,
    after: [u8; 32],
    identities: Vec<PreparedManagedIdentity>,
    digests: Vec<[u8; 32]>,
}

impl Revision {
    pub(super) fn ids(&self) -> Vec<u64> {
        self.identities.iter().map(|identity| identity.id).collect()
    }

    /// Returns whether the exact catalogue revision has already been installed.
    fn verify(&self, root: &Directory, contents: &[u8], allow_after: bool) -> io::Result<bool> {
        let current = ssh_key_catalog::read_catalog_for_restore(root)?;
        let actual = current.as_deref().map(|bytes| digest(bytes));
        if allow_after && actual == Some(self.after) {
            return Ok(true);
        }
        if actual == self.before {
            ssh_key_catalog::validate_prepared_managed_additions(
                current.as_deref().map(Vec::as_slice),
                contents,
                &self.identities,
            )?;
            return Ok(false);
        }
        Err(io::Error::other(
            "The SSH key catalogue changed after the Java credential review.",
        ))
    }
}

pub(super) fn stage(
    root: &Directory,
    snapshot: &mut Snapshot,
    keys: ManagedKeys,
    credential_ids: &[String],
) -> io::Result<Revision> {
    let contents = keys.catalogue.contents;
    let revision = Revision {
        before: keys.catalogue.before,
        after: digest(&contents),
        identities: keys.catalogue.additions,
        digests: keys.files.iter().map(|file| digest(file)).collect(),
    };
    let keys = Pending {
        revision,
        contents,
        files: keys.files,
    };
    keys.verify(root, credential_ids, false)?;
    for (identity, file) in keys.revision.identities.iter().zip(keys.files) {
        snapshot.insert(format!("{MANAGED}/{}", identity.token), file);
    }
    snapshot.insert(CATALOG.into(), keys.contents);
    Ok(keys.revision)
}

pub(super) struct Pending {
    revision: Revision,
    contents: Zeroizing<Vec<u8>>,
    files: Vec<Zeroizing<Vec<u8>>>,
}

impl Pending {
    pub(super) fn read(
        root: &Directory,
        snapshot: &mut Snapshot,
        revision: Revision,
        credential_ids: &[String],
        allow_after: bool,
    ) -> io::Result<Self> {
        let contents = snapshot
            .remove(CATALOG)
            .ok_or_else(|| invalid("The Java restore is missing its SSH key catalogue."))?;
        let files = revision
            .identities
            .iter()
            .map(|identity| {
                snapshot
                    .remove(&format!("{MANAGED}/{}", identity.token))
                    .ok_or_else(|| invalid("The Java restore is missing a reviewed SSH key."))
            })
            .collect::<io::Result<_>>()?;
        let keys = Self {
            revision,
            contents,
            files,
        };
        keys.verify(root, credential_ids, allow_after)?;
        Ok(keys)
    }

    pub(super) fn ids(&self) -> Vec<u64> {
        self.revision.ids()
    }

    fn verify(
        &self,
        root: &Directory,
        credential_ids: &[String],
        allow_after: bool,
    ) -> io::Result<()> {
        if self.contents.len() > data::file_limit(CATALOG)
            || digest(&self.contents) != self.revision.after
            || self.files.len() != self.revision.identities.len()
            || self.files.len() != self.revision.digests.len()
        {
            return Err(invalid("Invalid reviewed Java SSH key catalogue."));
        }
        ssh_key_catalog::validate_prepared_managed_paths(
            &self.contents,
            &self.revision.identities,
            credential_ids,
        )?;
        let catalog_installed = self.revision.verify(root, &self.contents, allow_after)?;
        let directory = missing(root.private_child(MANAGED, false))?;
        for ((identity, expected), file) in self.entries() {
            if file.is_empty()
                || file.len() > data::file_limit(MANAGED)
                || digest(file) != *expected
            {
                return Err(invalid("Invalid reviewed Java SSH key contents."));
            }
            let installed = directory
                .as_ref()
                .map(|directory| verify_copy(directory, &identity.token, expected, allow_after))
                .transpose()?
                .unwrap_or(false);
            if catalog_installed && !installed {
                return Err(invalid("An installed Java SSH key is missing."));
            }
        }
        Ok(())
    }

    pub(super) fn publish(self, root: &Directory) -> io::Result<()> {
        let directory = root.private_child(MANAGED, true)?;
        for ((identity, expected), file) in self.entries() {
            let name = &identity.token;
            if !verify_copy(&directory, name, expected, true)? {
                directory.publish_checked(
                    name,
                    |target| target.write_all(file),
                    || verify_copy(&directory, name, expected, false).map(|_| ()),
                )?;
            }
        }
        directory.sync()?;
        if !self.revision.verify(root, &self.contents, true)? {
            let write = |file: &mut File| file.write_all(&self.contents);
            let check = || {
                self.revision
                    .verify(root, &self.contents, false)
                    .map(|_| ())
            };
            if self.revision.before.is_none() {
                root.publish_checked(CATALOG, write, check)?;
            } else {
                root.replace_with(CATALOG, write, check)?;
            }
            root.sync()?;
        }
        Ok(())
    }

    fn entries(&self) -> impl Iterator<Item = ((&PreparedManagedIdentity, &[u8; 32]), &[u8])> {
        self.revision
            .identities
            .iter()
            .zip(&self.revision.digests)
            .zip(self.files.iter().map(|file| file.as_slice()))
    }
}

fn verify_copy(
    directory: &Directory,
    name: &str,
    expected: &[u8; 32],
    allow_after: bool,
) -> io::Result<bool> {
    if directory.has_entry(&format!("{name}.pub"))? {
        return Err(io::Error::other(
            "A Java SSH key destination already has a public companion.",
        ));
    }
    let Some(file) = missing(directory.open_private_file(name))? else {
        return Ok(false);
    };
    if allow_after && digest(&read_bounded(file, data::file_limit(MANAGED))?) == *expected {
        return Ok(true);
    }
    Err(io::Error::other(
        "A Java SSH key destination already exists or changed.",
    ))
}
