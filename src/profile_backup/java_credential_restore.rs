use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use kortty_vault::{PreparedAdditions, Secret, Vault};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::java_migration::{CONNECTIONS, SshSource};
use super::java_secrets::{
    JavaSecrets, keys,
    passwords::{self, Hop},
};
use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_store::import::{
    JavaConnectionBindings, JavaImportedRoute, JavaManagedKeyBinding, JavaPasswordBinding,
};
use crate::ssh_key_catalog::{Catalog, ManagedAddition, PreparedManagedAdditions};

#[derive(Default)]
pub(super) struct Prepared {
    pub(super) vault: Option<PreparedAdditions>,
    pub(super) connections: Option<Connections>,
    pub(super) keys: Option<ManagedKeys>,
}

pub(super) struct ManagedKeys {
    pub(super) catalogue: PreparedManagedAdditions,
    pub(super) files: Vec<Zeroizing<Vec<u8>>>,
}

pub(super) struct Connections {
    pub(super) before: Option<[u8; 32]>,
    pub(super) contents: Zeroizing<Vec<u8>>,
    pub(super) routes: BTreeMap<String, JavaImportedRoute>,
}

pub(super) struct Review {
    pub(super) prepared: Option<Prepared>,
    pub(super) passwords: Option<(usize, usize)>,
    pub(super) keys: Option<(usize, usize)>,
    pub(super) omitted_key_routes: usize,
    pub(super) unused_key_files: usize,
}

pub(super) fn prepare(
    root: &Path,
    source: &SshSource,
    password: Option<&Secret>,
    vault: Option<&Vault>,
    scheduler: bool,
) -> io::Result<Review> {
    if password.is_some() && vault.is_none() {
        return Err(io::Error::other(
            "Unlock the destination vault to import protected Java credentials.",
        ));
    }
    let secrets = password
        .map(|password| JavaSecrets::open(source.master.as_deref().map(Vec::as_slice), password))
        .transpose()?;
    let source_connections = source.connections.as_deref().map(|bytes| bytes.as_slice());
    let password_import = secrets
        .as_ref()
        .filter(|_| source.has_passwords)
        .map(|secrets| {
            passwords::inspect(
                secrets,
                source.credentials.as_deref().map(Vec::as_slice),
                source_connections,
            )
        })
        .transpose()?;
    let key_import = source
        .key_catalog
        .as_ref()
        .map(|catalog| keys::inspect(secrets.as_ref(), catalog, &source.key_files))
        .transpose()?;
    if password.is_some()
        && source.master.is_none()
        && password_import
            .as_ref()
            .is_none_or(|import| import.additions.is_empty())
        && key_import.as_ref().is_some_and(|import| {
            import
                .omissions
                .contains_key(&keys::Omission::UnauthenticatedPassphrase)
                && import.keys.iter().all(|key| key.passphrase.is_none())
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The Java master password could not authenticate any SSH key passphrases.",
        ));
    }
    let omitted_passwords = password_import
        .as_ref()
        .map_or(0, |import| import.omitted_count());
    let omitted_keys = key_import
        .as_ref()
        .map_or(0, |import| import.omissions.values().sum());
    let mut keys = key_import.map_or_else(Vec::new, |import| import.keys);
    let unused_key_files = source.key_files.len().saturating_sub(
        keys.iter()
            .map(|key| key.archive_path.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
    );
    let policy = ClipboardPolicy::load();
    let current = super::restore::read_java_connections(root)?;
    let password_bindings: Vec<_> = password_import
        .iter()
        .flat_map(|import| &import.bindings)
        .map(|binding| {
            let for_jump = binding.hop == Hop::Jump;
            let endpoint = if for_jump {
                binding.route_jump.as_ref()
            } else {
                Some(&binding.route_target)
            };
            if endpoint != Some(&binding.endpoint) {
                return Err(io::Error::other("Inconsistent Java SSH password route."));
            }
            Ok(JavaPasswordBinding {
                source_id: &binding.source_connection_id,
                target: &binding.route_target,
                jump: binding.route_jump.as_ref(),
                for_jump,
                credential: binding.addition,
            })
        })
        .collect::<io::Result<_>>()?;
    let key_bindings: Vec<_> = keys
        .iter()
        .enumerate()
        .map(|(index, key)| JavaManagedKeyBinding {
            source_key_id: &key.source_id,
            key: index,
        })
        .collect();
    let routes = JavaConnectionBindings::review(
        source_connections,
        current.as_deref().map(Vec::as_slice),
        &password_bindings,
        &key_bindings,
        policy.server_access(),
    )?;
    let omitted_passwords = omitted_passwords + routes.omitted();
    let omitted_key_routes = routes.omitted_keys();
    let needed = &routes.needed();
    let reviewed_passwords = password_import.is_some();
    let (indexes, mut additions): (Vec<_>, Vec<_>) = password_import
        .into_iter()
        .flat_map(|import| {
            import
                .additions
                .into_iter()
                .enumerate()
                .filter(move |(index, _)| *index < import.standalone || needed.contains(index))
        })
        .unzip();
    let password_count = additions.len();
    let key_count = keys.len();
    let mut passphrase_indexes = Vec::with_capacity(key_count);
    for key in &mut keys {
        passphrase_indexes.push(key.passphrase.take().map(|passphrase| {
            let index = additions.len();
            additions.push(passphrase);
            index
        }));
    }
    let vault = if additions.is_empty() {
        None
    } else {
        Some(
            vault
                .ok_or_else(|| io::Error::other("The destination vault is locked."))?
                .prepare_additions(additions)
                .map_err(io::Error::other)?,
        )
    };
    let sealed_ids = vault.as_ref().map_or(&[][..], |vault| vault.ids.as_slice());
    let password_ids = indexes
        .into_iter()
        .zip(sealed_ids.iter().cloned())
        .collect();
    let keys = if keys.is_empty() {
        None
    } else {
        let additions = keys
            .iter_mut()
            .zip(passphrase_indexes)
            .map(|(key, index)| ManagedAddition {
                name: std::mem::take(&mut key.name),
                description: std::mem::take(&mut key.description),
                passphrase: index.map(|index| sealed_ids[index].clone()),
            })
            .collect();
        let catalogue = Catalog::open(root.to_path_buf())?.prepare_managed_additions(additions)?;
        let files = keys.into_iter().map(|key| key.key.clone()).collect();
        Some(ManagedKeys { catalogue, files })
    };
    let key_ids: BTreeMap<_, _> = keys
        .iter()
        .flat_map(|keys| &keys.catalogue.additions)
        .enumerate()
        .map(|(index, identity)| (index, identity.id))
        .collect();
    let (contents, routes) = if scheduler {
        let import = routes.bind_with_routes(&password_ids, &key_ids, policy.server_access())?;
        (import.contents, import.routes)
    } else {
        (routes.bind(&password_ids, &key_ids)?, BTreeMap::new())
    };
    let connections = contents.map(|bytes| Connections {
        before: current.as_deref().map(|bytes| digest(bytes)),
        contents: Zeroizing::new(bytes),
        routes,
    });
    if connections.as_ref().is_some_and(|connections| {
        connections.contents.len() > super::data::file_limit(CONNECTIONS)
    }) {
        return Err(io::Error::other(
            "The merged connection catalogue exceeds its size limit.",
        ));
    }
    Ok(Review {
        prepared: (vault.is_some() || keys.is_some()).then_some(Prepared {
            vault,
            connections,
            keys,
        }),
        passwords: reviewed_passwords.then_some((password_count, omitted_passwords)),
        keys: source
            .key_catalog
            .is_some()
            .then_some((key_count, omitted_keys)),
        omitted_key_routes,
        unused_key_files,
    })
}

pub(super) fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
