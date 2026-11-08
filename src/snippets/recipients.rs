//! Public OpenPGP certificates enrolled with an independently verified fingerprint.

use std::collections::HashSet;
use std::fmt;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::archive::{MAX_CERTIFICATE_BYTES, enroll_certificate, normalize_fingerprint};
use super::{MAX_NAME_BYTES, StoreLock, invalid, profile};

pub(crate) use super::archive::{CertificateInfo, inspect_certificate};

pub(crate) const FILE_NAME: &str = "openpgp-recipients.json";
const VERSION: u8 = 1;
const MAX_RECIPIENTS: usize = 64;
const MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    name: String,
    fingerprint: String,
    revision: [u8; 16],
    revoked: bool,
}

impl Choice {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub(crate) fn key_id(&self) -> &str {
        key_id(&self.fingerprint)
    }

    pub(crate) fn revoked(&self) -> bool {
        self.revoked
    }
}

pub(super) fn key_id(fingerprint: &str) -> &str {
    // RFC 9580 §5.5.4 defines the v4 ID from the low bits and v6 from the high bits.
    match fingerprint.len() {
        40 => fingerprint.get(24..),
        64 => fingerprint.get(..16),
        _ => None,
    }
    .unwrap_or_default()
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.name, self.fingerprint)?;
        if self.revoked {
            formatter.write_str(" — revoked")?;
        }
        Ok(())
    }
}

#[derive(PartialEq, Eq)]
pub(crate) enum Action {
    List(PathBuf),
    Save(PathBuf, Change),
    SaveReviewed(PathBuf, Change, CertificateInfo),
    Delete(PathBuf, Choice),
}

#[derive(PartialEq, Eq)]
pub(crate) struct Change {
    pub(crate) previous: Option<Choice>,
    pub(crate) name: String,
    pub(crate) fingerprint: String,
    pub(crate) certificate: Option<PathBuf>,
}

impl Action {
    pub(crate) fn run(self) -> io::Result<Vec<Choice>> {
        match self {
            Self::List(path) => Ok(Document::read(&path)?.choices()),
            Self::Save(path, change) => save(&path, change, None),
            Self::SaveReviewed(path, change, reviewed) => save(&path, change, Some(&reviewed)),
            Self::Delete(path, choice) => mutate(&path, |document| {
                let index = document.index(&choice)?;
                document.entries.remove(index);
                Ok(())
            }),
        }
    }
}

pub(super) fn certificate(path: &Path, choice: &Choice) -> io::Result<String> {
    let mut document = Document::read(path)?;
    let index = document.index(choice)?;
    if choice.revoked {
        return Err(invalid("The saved OpenPGP recipient is revoked"));
    }
    Ok(document.entries.swap_remove(index).certificate)
}

pub(super) fn certificate_by_fingerprint(path: &Path, fingerprint: &str) -> io::Result<String> {
    let fingerprint = normalize_fingerprint(fingerprint)?;
    Document::read(path)?
        .entries
        .into_iter()
        .find(|entry| entry.choice.fingerprint == fingerprint && !entry.choice.revoked)
        .map(|entry| entry.certificate)
        .ok_or_else(|| invalid("The verified OpenPGP recipient is unavailable or revoked"))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    choice: Choice,
    certificate: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u8,
    entries: Vec<Entry>,
}

impl Document {
    fn read(path: &Path) -> io::Result<Self> {
        let file = match profile::open_preference(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    version: VERSION,
                    entries: Vec::new(),
                });
            }
            Err(error) => return Err(error),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES {
            return Err(invalid(
                "The recipient catalog must be a regular file of at most 8 MiB",
            ));
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        Self::decode(&bytes)
    }

    fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("The recipient catalog exceeds 8 MiB"));
        }
        let document: Self = serde_json::from_slice(bytes)
            .map_err(|_| invalid("Invalid OpenPGP recipient catalog JSON"))?;
        document.validate()?;
        Ok(document)
    }

    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION || self.entries.len() > MAX_RECIPIENTS {
            return Err(invalid("Unsupported OpenPGP recipient catalog"));
        }
        let mut names = HashSet::new();
        let mut fingerprints = HashSet::new();
        for entry in &self.entries {
            let choice = &entry.choice;
            if name(&choice.name)? != choice.name
                || normalize_fingerprint(&choice.fingerprint)? != choice.fingerprint
                || !names.insert(choice.name.to_lowercase())
                || !fingerprints.insert(&choice.fingerprint)
                || entry.certificate.is_empty()
                || entry.certificate.len() as u64 > MAX_CERTIFICATE_BYTES
            {
                return Err(invalid("Invalid or duplicate OpenPGP recipient"));
            }
        }
        Ok(())
    }

    fn choices(self) -> Vec<Choice> {
        let mut choices: Vec<_> = self.entries.into_iter().map(|entry| entry.choice).collect();
        choices.sort_by_cached_key(|choice| choice.name.to_lowercase());
        choices
    }

    fn index(&self, previous: &Choice) -> io::Result<usize> {
        self.entries
            .iter()
            .position(|entry| entry.choice == *previous)
            .ok_or_else(|| {
                invalid("The OpenPGP recipient changed or was removed. Reload and select it again")
            })
    }
}

pub(crate) fn validate_backup(bytes: &[u8]) -> io::Result<()> {
    for entry in Document::decode(bytes)?.entries {
        super::archive::validate_backup_certificate(
            entry.certificate.as_bytes(),
            &entry.choice.fingerprint,
        )?;
    }
    Ok(())
}

fn name(value: &str) -> io::Result<&str> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_NAME_BYTES || value.chars().any(char::is_control) {
        return Err(invalid(
            "Enter an OpenPGP recipient name of 1 to 160 UTF-8 bytes",
        ));
    }
    Ok(value)
}

fn save(
    path: &Path,
    change: Change,
    reviewed: Option<&CertificateInfo>,
) -> io::Result<Vec<Choice>> {
    let name = name(&change.name)?.to_owned();
    let fingerprint = normalize_fingerprint(&change.fingerprint)?;
    if change
        .previous
        .as_ref()
        .is_some_and(|previous| previous.fingerprint != fingerprint)
    {
        return Err(invalid(
            "Enroll a new recipient to use a different OpenPGP fingerprint",
        ));
    }
    let candidate = change
        .certificate
        .as_ref()
        .map(|path| enroll_certificate(path, &fingerprint))
        .transpose()?;
    if let Some(reviewed) = reviewed
        && !change
            .certificate
            .as_deref()
            .zip(candidate.as_ref())
            .is_some_and(|(source, (certificate, _))| reviewed.matches_source(source, certificate))
    {
        return Err(invalid(
            "The OpenPGP certificate changed. Preview it again before saving",
        ));
    }
    let mut revision = [0; 16];
    getrandom::fill(&mut revision).map_err(io::Error::other)?;
    let revoked = candidate.as_ref().is_some_and(|(_, revoked)| *revoked)
        || change.previous.as_ref().is_some_and(Choice::revoked);
    let choice = Choice {
        name,
        fingerprint,
        revision,
        revoked,
    };
    mutate(path, |document| {
        if let Some(previous) = change.previous {
            let index = document.index(&previous)?;
            let entry = &mut document.entries[index];
            if previous.revoked && candidate.is_some() {
                return Err(invalid("A revoked OpenPGP certificate cannot be replaced"));
            }
            entry.choice = choice;
            if let Some((certificate, _)) = candidate {
                entry.certificate = certificate;
            }
        } else {
            let (certificate, revoked) =
                candidate.ok_or_else(|| invalid("Choose an OpenPGP public certificate"))?;
            if revoked {
                return Err(invalid("The OpenPGP certificate is revoked"));
            }
            document.entries.push(Entry {
                choice,
                certificate,
            });
        }
        Ok(())
    })
}

fn mutate(
    path: &Path,
    change: impl FnOnce(&mut Document) -> io::Result<()>,
) -> io::Result<Vec<Choice>> {
    let _lock = StoreLock::acquire_for(
        path,
        "The recipient catalog is being saved by another instance",
    )?;
    let mut document = Document::read(path)?;
    change(&mut document)?;
    document.validate()?;
    let bytes = serde_json::to_vec(&document).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("The recipient catalog exceeds 8 MiB"));
    }
    profile::save_atomic(path, &bytes)?;
    Ok(document.choices())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use pgp::composed::SignedPublicKey;
    use pgp::packet::{SignatureConfig, SignatureType};
    use pgp::ser::Serialize as _;
    use pgp::types::{KeyDetails, KeyVersion, Password};
    use rand::{SeedableRng, rngs::StdRng};

    use super::*;
    use crate::snippets::archive::test_openpgp_key;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let base = std::env::current_dir().unwrap().join("target/.scratch");
            fs::create_dir_all(&base).unwrap();
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).unwrap();
            let path = base.join(format!("pgp-preview-{:032x}", u128::from_le_bytes(nonce)));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reviewed_certificate_checks_current_file_and_preserves_verified_revocation() {
        let scratch = Scratch::new();
        for (version, seed) in [(KeyVersion::V4, 31), (KeyVersion::V6, 32)] {
            let secret = test_openpgp_key(version, seed);
            let mut public = SignedPublicKey::from(secret.clone());
            let source = scratch.0.join(format!("recipient-{seed}.pgp"));
            let catalog = scratch.0.join(format!("catalog-{seed}.json"));
            let write_public = |public: &SignedPublicKey| {
                fs::write(&source, public.to_bytes().unwrap()).unwrap();
            };
            write_public(&public);

            let reviewed = inspect_certificate(&source).unwrap();
            assert_eq!(reviewed.fingerprint, format!("{:X}", public.fingerprint()));
            assert_eq!(
                reviewed.key_id,
                public.legacy_key_id().to_string().to_ascii_uppercase()
            );
            assert_eq!(
                reviewed.user_id.as_deref(),
                Some("Local test recipient <test@example.invalid>")
            );
            assert!(!reviewed.revoked);
            let save = |previous, fingerprint, reviewed| {
                Action::SaveReviewed(
                    catalog.clone(),
                    Change {
                        previous,
                        name: "Verified fixture".into(),
                        fingerprint,
                        certificate: Some(source.clone()),
                    },
                    reviewed,
                )
                .run()
            };
            assert!(
                save(
                    None,
                    "0".repeat(reviewed.fingerprint.len()),
                    reviewed.clone()
                )
                .is_err()
            );
            let choice = save(None, reviewed.fingerprint.clone(), reviewed.clone())
                .unwrap()
                .remove(0);
            assert_eq!(choice.key_id(), reviewed.key_id);

            let revocation = SignatureConfig::from_key(
                StdRng::from_seed([33; 32]),
                &secret.primary_key,
                SignatureType::KeyRevocation,
            )
            .unwrap()
            .sign_key(
                &secret.primary_key,
                &Password::default(),
                &public.primary_key,
            )
            .unwrap();
            public.details.revocation_signatures.push(revocation);
            write_public(&public);
            // A file edit keeps the primary fingerprint and changes the exact certificate.
            assert!(
                save(
                    Some(choice.clone()),
                    reviewed.fingerprint.clone(),
                    reviewed.clone()
                )
                .is_err()
            );
            assert!(!Action::List(catalog.clone()).run().unwrap()[0].revoked());

            let reviewed = inspect_certificate(&source).unwrap();
            assert!(reviewed.revoked);
            let updated = save(Some(choice), reviewed.fingerprint.clone(), reviewed)
                .unwrap()
                .remove(0);
            assert!(updated.revoked());
            assert!(certificate_by_fingerprint(&catalog, updated.fingerprint()).is_err());

            fs::write(&source, secret.to_bytes().unwrap()).unwrap();
            assert!(inspect_certificate(&source).is_err());
        }
    }
}
