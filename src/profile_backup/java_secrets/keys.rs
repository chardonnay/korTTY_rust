//! Maps Java-managed SSH key catalog records to exact archive members.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use kortty_vault::{CredentialAddition, CredentialInput, CredentialKind};
use zeroize::Zeroizing;

use crate::profile_backup::archive::{
    invalid_component,
    java::{Member, MemberKind},
};
use crate::ssh_dialog::unsafe_text;

use super::{JavaSecrets, children, parse_xml, raw_scalar, scalar};

const MAX_CATALOG_BYTES: usize = 1024 * 1024;
const MAX_KEY_BYTES: usize = 1024 * 1024;
const MAX_KEY_RECORDS: usize = 512;
const MAX_PATH_BYTES: usize = 4096;
const MAX_PASSPHRASE_BYTES: usize = 1024;
const MAX_DERIVATIONS: usize = 256;
const KEY_DIRECTORY: &str = "ssh-keys";

pub(in crate::profile_backup) struct ManagedKey<'a> {
    pub(in crate::profile_backup) source_id: String,
    pub(in crate::profile_backup) name: String,
    pub(in crate::profile_backup) description: String,
    pub(in crate::profile_backup) archive_path: String,
    pub(in crate::profile_backup) key: &'a Zeroizing<Vec<u8>>,
    pub(in crate::profile_backup) passphrase: Option<CredentialAddition>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::profile_backup) enum Omission {
    InvalidRecord,
    ExternalKey,
    InvalidCopyPath,
    MissingManagedFile,
    InvalidManagedFile,
    UnsupportedNativeKey,
    MissingKeyPassphrase,
    InvalidNativePassphrase,
    MissingSourceMasterPassword,
    InvalidPassphrase,
    UnauthenticatedPassphrase,
    DerivationLimit,
}

#[derive(Default)]
pub(in crate::profile_backup) struct KeyImport<'a> {
    pub(in crate::profile_backup) keys: Vec<ManagedKey<'a>>,
    pub(in crate::profile_backup) omissions: BTreeMap<Omission, usize>,
}

/// Inspects archive-managed copies using the native SSH backend without connecting.
/// The caller assigns native identifiers before creating any profile data.
pub(in crate::profile_backup) fn inspect<'a>(
    secrets: Option<&JavaSecrets<'_>>,
    catalog: &[u8],
    members: &'a BTreeMap<String, Member>,
) -> io::Result<KeyImport<'a>> {
    let document = parse_xml(catalog, MAX_CATALOG_BYTES, "sshKeys")?;
    let mut keys = KeyImport::default();
    let mut seen = BTreeSet::new();
    let mut remaining = MAX_DERIVATIONS;
    for (index, node) in children(document.root_element(), "sshKey").enumerate() {
        if index >= MAX_KEY_RECORDS {
            return Err(invalid("Java ssh-keys.xml exceeds its record limit."));
        }
        let Some(id) = raw_scalar(node, "id")
            .ok()
            .flatten()
            .filter(|id| valid_key_id(id))
        else {
            keys.omit(Omission::InvalidRecord);
            continue;
        };
        if !seen.insert(id.to_ascii_lowercase()) {
            return Err(invalid("Java ssh-keys.xml contains duplicate key IDs."));
        }
        match read_record(node, id, members, secrets, &mut remaining) {
            Ok(key) => keys.keys.push(key),
            Err(reason) => keys.omit(reason),
        }
    }

    Ok(keys)
}

impl KeyImport<'_> {
    fn omit(&mut self, reason: Omission) {
        *self.omissions.entry(reason).or_default() += 1;
    }
}

fn read_record<'a>(
    node: roxmltree::Node<'_, '_>,
    id: &str,
    members: &'a BTreeMap<String, Member>,
    secrets: Option<&JavaSecrets<'_>>,
    remaining: &mut usize,
) -> Result<ManagedKey<'a>, Omission> {
    let invalid = |_| Omission::InvalidRecord;
    let name = raw_scalar(node, "name")
        .map_err(invalid)?
        .filter(|name| !name.trim().is_empty() && name.len() <= 256 && !unsafe_text(name))
        .ok_or(Omission::InvalidRecord)?;
    let description = raw_scalar(node, "description")
        .map_err(invalid)?
        .unwrap_or_default();
    if description.len() > 4096
        || description
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\r' | '\n' | '\t'))
    {
        return Err(Omission::InvalidRecord);
    }
    match scalar(node, "copiedToUserDir").map_err(invalid)? {
        Some("true" | "1") => {}
        None | Some("false" | "0") => return Err(Omission::ExternalKey),
        _ => return Err(Omission::InvalidRecord),
    }
    let copied = raw_scalar(node, "userDirPath").map_err(invalid)?;
    let archive_path = managed_path(id, copied).ok_or(Omission::InvalidCopyPath)?;
    let member = members
        .get(&archive_path)
        .filter(|member| member.kind == MemberKind::SshKeyFile)
        .ok_or(Omission::MissingManagedFile)?;
    if !(1..=MAX_KEY_BYTES).contains(&member.bytes.len()) {
        return Err(Omission::InvalidManagedFile);
    }
    let passphrase = match kortty_ssh::validate_private_key(&member.bytes, None) {
        Ok(()) => None,
        Err(kortty_ssh::ConnectError::Io(error))
            if error.kind() == io::ErrorKind::PermissionDenied =>
        {
            let encoded = scalar(node, "encryptedPassphrase")
                .map_err(invalid)?
                .filter(|value| !value.is_empty())
                .ok_or(Omission::MissingKeyPassphrase)?;
            let addition = key_passphrase(name, encoded, secrets, remaining)?;
            kortty_ssh::validate_private_key(&member.bytes, Some(addition.secret.as_str()))
                .map_err(|error| match error {
                    kortty_ssh::ConnectError::Io(error)
                        if error.kind() == io::ErrorKind::PermissionDenied =>
                    {
                        Omission::InvalidNativePassphrase
                    }
                    _ => Omission::UnsupportedNativeKey,
                })?;
            Some(addition)
        }
        Err(_) => return Err(Omission::UnsupportedNativeKey),
    };
    Ok(ManagedKey {
        source_id: id.into(),
        name: name.into(),
        description: description.into(),
        archive_path,
        key: &member.bytes,
        passphrase,
    })
}

fn key_passphrase(
    name: &str,
    encoded: &str,
    secrets: Option<&JavaSecrets<'_>>,
    remaining: &mut usize,
) -> Result<CredentialAddition, Omission> {
    let secrets = secrets.ok_or(Omission::MissingSourceMasterPassword)?;
    if *remaining == 0 {
        return Err(Omission::DerivationLimit);
    }
    *remaining -= 1;
    let secret = secrets
        .decrypt_field_with_limit(encoded, MAX_PASSPHRASE_BYTES)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::PermissionDenied {
                Omission::UnauthenticatedPassphrase
            } else {
                Omission::InvalidPassphrase
            }
        })?;
    Ok(CredentialAddition {
        input: CredentialInput {
            label: name.into(),
            kind: CredentialKind::KeyPassphrase,
            ..CredentialInput::default()
        },
        secret,
        webhook: None,
    })
}

fn valid_key_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn managed_path(id: &str, copied: Option<&str>) -> Option<String> {
    // Java keeps using this copy when the original keyPath is edited.
    let copied = copied.filter(|path| path.len() <= MAX_PATH_BYTES && !unsafe_text(path))?;
    let mut components = copied.rsplit(['/', '\\']);
    let copied_name = components.next()?;
    let source_name = copied_name.strip_prefix(id)?.strip_prefix('_')?;
    if components.next()? != KEY_DIRECTORY
        || invalid_component(source_name)
        || invalid_component(copied_name)
    {
        return None;
    }
    Some(format!("{KEY_DIRECTORY}/{copied_name}"))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kortty_vault::Secret;

    const KEY_ID: &str = "f99ec24c-9aec-4978-a57f-ec37c449ca35";
    const KEY_PATH: &str = "ssh-keys/f99ec24c-9aec-4978-a57f-ec37c449ca35_fixture-ed25519.pem";
    const ENCRYPTED_ID: &str = "fe33d0a9-ab84-4351-8d5c-ece77f40432f";
    const ENCRYPTED_PATH: &str = "ssh-keys/fe33d0a9-ab84-4351-8d5c-ece77f40432f_fixture-encrypted";
    const CATALOG: &[u8] = include_bytes!("fixtures/java-managed-ssh-keys.xml");
    const KEY: &[u8] = include_bytes!("fixtures/java-managed-ed25519.pem");
    const ENCRYPTED: &[u8] = include_bytes!("fixtures/java-managed-encrypted.key");
    const MASTER: &[u8] = include_bytes!("fixtures/master.properties");

    fn member(bytes: &[u8]) -> Member {
        Member {
            kind: MemberKind::SshKeyFile,
            bytes: Zeroizing::new(bytes.to_vec()),
        }
    }

    #[test]
    fn reads_java_metadata_and_validates_both_key_formats_offline() {
        let members = BTreeMap::from([
            (KEY_PATH.into(), member(KEY)),
            (ENCRYPTED_PATH.into(), member(ENCRYPTED)),
        ]);
        let without_master = inspect(None, CATALOG, &members).unwrap();
        assert_eq!(without_master.keys.len(), 1);
        assert!(without_master.keys[0].passphrase.is_none());
        assert_eq!(
            without_master
                .omissions
                .get(&Omission::MissingSourceMasterPassword),
            Some(&1)
        );

        let source_password = Secret::new("päss🔑例".into());
        let secrets = JavaSecrets::open(Some(MASTER), &source_password).unwrap();
        let import = inspect(Some(&secrets), CATALOG, &members).unwrap();
        assert!(import.omissions.is_empty());
        let [key, encrypted] = import.keys.as_slice() else {
            panic!("expected both synthetic managed SSH keys");
        };
        assert_eq!(key.source_id, KEY_ID);
        assert_eq!(key.name, "Managed café");
        assert_eq!(key.description, "Synthetic Java-managed identity");
        assert_eq!(key.archive_path, KEY_PATH);
        assert_eq!(key.key.as_slice(), KEY);
        assert!(key.passphrase.is_none());
        assert_eq!(encrypted.source_id, ENCRYPTED_ID);
        assert_eq!(encrypted.archive_path, ENCRYPTED_PATH);
        let passphrase = encrypted.passphrase.as_ref().unwrap();
        assert_eq!(passphrase.input.kind, CredentialKind::KeyPassphrase);
        assert_eq!(passphrase.secret.as_str(), "fixture-only passphrase");
    }

    #[test]
    fn requires_exact_archive_association_and_omits_unusable_key_material() {
        let members = BTreeMap::from([
            (
                KEY_PATH.replace("fixture-ed25519", "elsewhere"),
                member(KEY),
            ),
            (ENCRYPTED_PATH.into(), member(b"invalid key")),
        ]);
        let import = inspect(None, CATALOG, &members).unwrap();
        assert!(import.keys.is_empty());
        assert_eq!(
            import.omissions.get(&Omission::MissingManagedFile),
            Some(&1)
        );
        assert_eq!(
            import.omissions.get(&Omission::UnsupportedNativeKey),
            Some(&1)
        );

        let members = BTreeMap::from([(KEY_PATH.into(), member(KEY))]);
        let text = std::str::from_utf8(CATALOG).unwrap();
        let foreign = text.replace("copiedToUserDir>true", "copiedToUserDir>false");
        let import = inspect(None, foreign.as_bytes(), &members).unwrap();
        assert!(import.keys.is_empty());
        assert_eq!(import.omissions.get(&Omission::ExternalKey), Some(&2));
        let mismatched = text.replace("ssh-keys\\", "keys\\");
        let import = inspect(None, mismatched.as_bytes(), &members).unwrap();
        assert!(import.keys.is_empty());
        assert_eq!(import.omissions.get(&Omission::InvalidCopyPath), Some(&1));
        assert_eq!(
            import.omissions.get(&Omission::MissingManagedFile),
            Some(&1)
        );
    }

    #[test]
    fn reports_a_wrong_source_password_and_keeps_an_independent_plain_key() {
        let members = BTreeMap::from([
            (KEY_PATH.into(), member(KEY)),
            (ENCRYPTED_PATH.into(), member(ENCRYPTED)),
        ]);
        let wrong_password = Secret::new("incorrect Java fixture password".into());
        let secrets = JavaSecrets::open(None, &wrong_password).unwrap();
        let import = inspect(Some(&secrets), CATALOG, &members).unwrap();
        assert_eq!(import.keys.len(), 1);
        assert_eq!(import.keys[0].source_id, KEY_ID);
        assert_eq!(
            import.omissions.get(&Omission::UnauthenticatedPassphrase),
            Some(&1)
        );
    }

    #[test]
    fn keeps_the_active_copy_after_java_edits_the_original_key_path() {
        let text = std::str::from_utf8(CATALOG).unwrap();
        let edited = text.replace(
            ".ssh\\fixture-ed25519.pem</keyPath>",
            ".ssh\\renamed-identity.pem</keyPath>",
        );
        assert_ne!(edited, text);
        let members = BTreeMap::from([(KEY_PATH.into(), member(KEY))]);
        let imported = inspect(None, edited.as_bytes(), &members).unwrap();
        assert_eq!(imported.keys.len(), 1);
        assert_eq!(imported.keys[0].archive_path, KEY_PATH);

        let foreign_uuid = "3cd2651e-2dfd-4d72-b16f-f45ba1fdcd13";
        let foreign = edited.replace(
            &format!("{KEY_ID}_fixture-ed25519.pem"),
            &format!("{foreign_uuid}_fixture-ed25519.pem"),
        );
        let foreign_member = KEY_PATH.replace(KEY_ID, foreign_uuid);
        let members = BTreeMap::from([(foreign_member, member(KEY))]);
        let imported = inspect(None, foreign.as_bytes(), &members).unwrap();
        assert!(imported.keys.is_empty());
        assert_eq!(imported.omissions.get(&Omission::InvalidCopyPath), Some(&1));

        let foreign_parent = edited.replace("ssh-keys\\", "other-keys\\");
        let members = BTreeMap::from([(KEY_PATH.into(), member(KEY))]);
        let imported = inspect(None, foreign_parent.as_bytes(), &members).unwrap();
        assert!(imported.keys.is_empty());
        assert_eq!(imported.omissions.get(&Omission::InvalidCopyPath), Some(&1));
    }

    #[test]
    fn rejects_duplicate_java_key_ids_across_uuid_case() {
        let catalog = format!(
            "<sshKeys><sshKey><id>{KEY_ID}</id></sshKey>\
             <sshKey><id>{}</id></sshKey></sshKeys>",
            KEY_ID.to_ascii_uppercase()
        );
        let files = BTreeMap::new();
        let result = inspect(None, catalog.as_bytes(), &files);
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::InvalidData));
    }
}
