//! Signed model recommendations and prompt-family mappings.

pub(crate) mod online;
mod schema;

use std::io;
use std::path::Path;
use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::private_files::{Directory, FileLock, FileStamp, require_single_link};

pub(crate) use schema::{Catalog, Recommendation, Role};

pub(crate) const CATALOG_URL: &str = "https://github.com/chardonnay/kortty-ai-catalog/releases/latest/download/model-prompt-catalog-v1.json";
pub(crate) const SIGNATURE_URL: &str = "https://github.com/chardonnay/kortty-ai-catalog/releases/latest/download/model-prompt-catalog-v1.sig";
pub(crate) const MAX_CATALOG_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_SIGNATURE_BYTES: usize = 4096;

const MAX_CACHE_BYTES: u64 = 4 * 1024 * 1024;
const CACHE_DIR: &str = "ai-catalog";
const CACHE_FILE: &str = "last-valid-catalog-v1.json";
const CACHE_LOCK: &str = "operation.lock";
// SubjectPublicKeyInfo for id-Ed25519 with absent parameters and a 32-byte bit string.
const ED25519_SPKI: &[u8] = b"\x30\x2a\x30\x05\x06\x03\x2b\x65\x70\x03\x21\x00";

// Exact canonical file from korTTY Java v2.26.0, blob 30ea79b1b965d0e2eaf79f77e15755f8b913ed66.
const BOOTSTRAP: &[u8] = include_bytes!("model_catalog/bootstrap-v2.json");

pub(crate) fn bootstrap() -> &'static Catalog {
    static CATALOG: LazyLock<Catalog> =
        LazyLock::new(|| Catalog::parse(BOOTSTRAP).expect("valid bundled AI model catalog"));
    &CATALOG
}

pub(crate) fn load_cached(profile: &Path) -> io::Result<Option<Catalog>> {
    match Trust::configured()? {
        Some(trust) => Repository::new(profile, trust).load_cached(),
        None => Ok(None),
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Trust(VerifyingKey);

impl Trust {
    pub(crate) fn configured() -> io::Result<Option<Self>> {
        option_env!("KORTTY_AI_CATALOG_PUBLIC_KEY")
            .filter(|key| !key.trim().is_empty())
            .map(Self::decode)
            .transpose()
    }

    fn decode(encoded: &str) -> io::Result<Self> {
        let invalid_key =
            || invalid("The build does not have a valid independent Ed25519 AI catalog key.");
        if encoded.len() > MAX_SIGNATURE_BYTES {
            return Err(invalid_key());
        }
        let encoded = encoded.trim();
        let encoded = if let Some(body) = encoded.strip_prefix("-----BEGIN PUBLIC KEY-----") {
            body.strip_suffix("-----END PUBLIC KEY-----")
                .ok_or_else(invalid_key)?
        } else {
            encoded
        };
        let encoded: String = encoded
            .chars()
            .filter(|character| !character.is_ascii_whitespace())
            .collect();
        let der = decode_base64(&encoded).map_err(|_| invalid_key())?;
        let bytes: &[u8; 32] = der
            .strip_prefix(ED25519_SPKI)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(invalid_key)?;
        let key = VerifyingKey::from_bytes(bytes).map_err(|_| invalid_key())?;
        if key.is_weak() || *bytes == super::runtime_index::RELEASE_KEY {
            return Err(invalid_key());
        }
        Ok(Self(key))
    }

    fn verify(&self, bytes: &[u8], signature: &str) -> io::Result<Catalog> {
        if bytes.is_empty() || bytes.len() > MAX_CATALOG_BYTES {
            return Err(invalid("The AI catalog has an invalid size."));
        }
        if signature.is_empty() || signature.len() > MAX_SIGNATURE_BYTES || !signature.is_ascii() {
            return Err(invalid("The AI catalog signature is invalid."));
        }
        let encoded = signature.trim();
        let encoded = encoded.strip_prefix("ed25519:").unwrap_or(encoded).trim();
        let signature = decode_base64(encoded)
            .ok()
            .and_then(|bytes| Signature::from_slice(&bytes).ok())
            .ok_or_else(|| invalid("The AI catalog signature is invalid."))?;
        self.0
            .verify_strict(bytes, &signature)
            .map_err(|_| invalid("The AI catalog signature could not be verified."))?;
        Catalog::parse(bytes)
    }
}

pub(crate) struct Repository<'a> {
    profile: &'a Path,
    trust: Trust,
}

impl<'a> Repository<'a> {
    pub(crate) fn new(profile: &'a Path, trust: Trust) -> Self {
        Self { profile, trust }
    }

    pub(crate) fn load_cached(&self) -> io::Result<Option<Catalog>> {
        let root = match self.directory() {
            Ok(root) => root,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let Some(cached) = self.read_cache(&root)? else {
            return Ok(None);
        };
        let built_in = bootstrap();
        if cached.catalog.sequence() < built_in.sequence() {
            return Ok(None);
        }
        require_current(&cached.catalog, built_in)?;
        Ok(Some(cached.catalog))
    }

    pub(crate) fn accept(
        &self,
        bytes: &[u8],
        signature: &[u8],
        authorize: impl Fn() -> io::Result<()>,
    ) -> io::Result<Catalog> {
        authorize()?;
        let signature = std::str::from_utf8(signature)
            .map_err(|_| invalid("The AI catalog signature is invalid."))?;
        let catalog = self.trust.verify(bytes, signature)?;
        require_current(&catalog, bootstrap())?;

        let profile = Directory::open(self.profile, false)?;
        let root = profile.private_child(CACHE_DIR, true)?;
        let lock = root.open_lock(CACHE_LOCK)?;
        require_single_link(&lock)?;
        lock.try_lock().map_err(io::Error::from)?;
        let _lock = FileLock::new(lock);
        let previous = self.read_cache(&root)?;
        let check = || {
            authorize()?;
            if !root.same_as(&self.directory()?)? {
                return Err(io::Error::other("The AI catalog cache directory changed."));
            }
            if let Some(previous) = &previous {
                let file = root.open_private_file(CACHE_FILE)?;
                require_single_link(&file)?;
                previous.stamp.require_unchanged(&file)?;
            }
            Ok(())
        };
        if let Some(previous) = &previous {
            require_current(&catalog, &previous.catalog)?;
            if catalog.sequence() == previous.catalog.sequence() {
                if bytes != previous.bytes {
                    return Err(invalid(
                        "The AI catalog sequence collides with a different signed payload.",
                    ));
                }
                check()?;
                root.sync()?;
                profile.sync()?;
                return Ok(catalog);
            }
        }

        let envelope = Envelope {
            format_version: 1,
            catalog_base64: STANDARD.encode(bytes),
            detached_signature: signature.trim().into(),
        };
        let write = |file: &mut std::fs::File| {
            serde_json::to_writer(file, &envelope).map_err(io::Error::from)
        };
        if previous.is_some() {
            root.replace_with(CACHE_FILE, write, check)?;
        } else {
            root.publish_checked(CACHE_FILE, write, check)?;
        }
        root.sync()?;
        profile.sync()?;
        Ok(catalog)
    }

    fn directory(&self) -> io::Result<Directory> {
        Directory::open(self.profile, false)?.private_child(CACHE_DIR, false)
    }

    fn read_cache(&self, root: &Directory) -> io::Result<Option<Cached>> {
        let file = match root.open_private_file(CACHE_FILE) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        require_single_link(&file)?;
        let stamp = FileStamp::read(&file)?;
        let envelope = super::runtime_package::read_bounded(file, MAX_CACHE_BYTES)?;
        let envelope: Envelope = serde_json::from_slice(&envelope)
            .map_err(|_| invalid("The AI catalog cache envelope is invalid."))?;
        if envelope.format_version != 1 {
            return Err(invalid("The AI catalog cache format is unsupported."));
        }
        let bytes = STANDARD
            .decode(envelope.catalog_base64)
            .map_err(|_| invalid("The cached AI catalog payload is invalid."))?;
        let catalog = self.trust.verify(&bytes, &envelope.detached_signature)?;
        Ok(Some(Cached {
            catalog,
            bytes,
            stamp,
        }))
    }
}

struct Cached {
    catalog: Catalog,
    bytes: Vec<u8>,
    stamp: FileStamp,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Envelope {
    format_version: u8,
    catalog_base64: String,
    detached_signature: String,
}

fn require_current(candidate: &Catalog, previous: &Catalog) -> io::Result<()> {
    if candidate.sequence() < previous.sequence() {
        return Err(invalid(
            "The signed AI catalog is older than the last accepted sequence.",
        ));
    }
    if candidate.sequence() == previous.sequence() && candidate != previous {
        return Err(invalid(
            "The AI catalog sequence collides with a different catalog.",
        ));
    }
    Ok(())
}

fn decode_base64(encoded: &str) -> Result<Vec<u8>, base64::DecodeError> {
    if encoded.ends_with('=') {
        STANDARD.decode(encoded)
    } else {
        STANDARD_NO_PAD.decode(encoded)
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) fn catalog_with_fixture_mapping() -> Catalog {
    let mut value: serde_json::Value = serde_json::from_slice(BOOTSTRAP).unwrap();
    value["sequence"] = 3.into();
    value["catalogVersion"] = "catalog-fixture".into();
    value["recommendations"][0]["id"] = "fixture-model".into();
    value["promptFamilies"][1]["modelNameContains"] = serde_json::json!(["qwen"]);
    value["promptFamilies"][1]["priority"] = 101.into();
    Catalog::parse(&serde_json::to_vec(&value).unwrap()).unwrap()
}
