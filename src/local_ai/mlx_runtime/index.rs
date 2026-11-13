//! The signed MLX release receipt at the pinned Java runtime contract.

use std::collections::BTreeSet;

use ed25519_dalek::VerifyingKey;
use jiff::Timestamp;
use pgp::crypto::hash::HashAlgorithm;
use serde::Deserialize;

use crate::local_ai::runtime_index::{RELEASE_KEY, verify_detached};
use crate::local_ai::runtime_package::MAX_ARCHIVE_BYTES;

pub(super) const PYTHON: &str = "python/bin/python3";
pub(super) const LAUNCHER: &str = "kortty_mlx_server.py";
pub(super) const MAX_REVOKED: usize = 10_000;

pub(super) fn release_key() -> VerifyingKey {
    VerifyingKey::from_bytes(&RELEASE_KEY).expect("valid embedded runtime release key")
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct OsVersion([u16; 3]);

impl OsVersion {
    pub(super) const MINIMUM: Self = Self([14, 0, 0]);

    pub(super) fn parse(value: &str) -> Option<Self> {
        let mut version = [0; 3];
        for (position, part) in value.split('.').enumerate() {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            *version.get_mut(position)? = part.parse().ok()?;
        }
        (version[0] != 0).then_some(Self(version))
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Package {
    schema_version: u8,
    pub(super) runtime_id: String,
    pub(super) installation_id: String,
    platform: String,
    architecture: String,
    backend: String,
    minimum_os_version: String,
    pub(super) mlx_lm_version: String,
    python_version: String,
    source_commit: String,
    executable_path: String,
    launcher_path: String,
    pub(super) size_bytes: u64,
    pub(super) sha256: String,
    requirements_lock_sha256: String,
    pub(super) download_url: String,
}

impl Package {
    fn validate(&mut self) -> Result<(), String> {
        let safe_version = |value: &str| {
            (1..=64).contains(&value.len())
                && value.as_bytes()[0].is_ascii_alphanumeric()
                && value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-')
                })
        };
        if self.schema_version != 1
            || !identifier(&self.runtime_id)
            || !identifier(&self.installation_id)
            || !self.platform.eq_ignore_ascii_case("macos")
            || !(self.architecture.eq_ignore_ascii_case("aarch64")
                || self.architecture.eq_ignore_ascii_case("arm64"))
            || !self.backend.eq_ignore_ascii_case("mlx")
            || OsVersion::parse(&self.minimum_os_version).is_none()
            || !safe_version(&self.mlx_lm_version)
            || !safe_version(&self.python_version)
            || !(7..=64).contains(&self.source_commit.len())
            || !self
                .source_commit
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || self.executable_path != PYTHON
            || self.launcher_path != LAUNCHER
            || !(1..=MAX_ARCHIVE_BYTES).contains(&self.size_bytes)
            || !digest(&self.sha256)
            || !digest(&self.requirements_lock_sha256)
        {
            return Err("Invalid signed MLX runtime package descriptor".into());
        }
        let url = url::Url::parse(&self.download_url)
            .map_err(|_| "Invalid signed MLX runtime package URL")?;
        if self.download_url.len() > 4096
            || self
                .download_url
                .chars()
                .any(|ch| ch.is_control() || ch == '\\')
            || url.scheme() != "https"
            || !url.has_host()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err("Invalid signed MLX runtime package URL".into());
        }
        self.sha256.make_ascii_lowercase();
        Ok(())
    }

    pub(super) fn compatible(&self, host: OsVersion) -> bool {
        host >= OsVersion::MINIMUM
            && OsVersion::parse(&self.minimum_os_version).is_some_and(|minimum| host >= minimum)
    }

    pub(super) fn version_key(&self) -> Option<[u32; 3]> {
        let mut version = [0; 3];
        let mut numeric = false;
        for (part, value) in self.mlx_lm_version.split(['.', '+', '-']).zip(&mut version) {
            if (1..=6).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_digit()) {
                *value = part.parse().ok()?;
                numeric = true;
            }
        }
        numeric.then_some(version)
    }
}

#[derive(Clone)]
pub(super) struct Index {
    pub(super) generated_at: Timestamp,
    pub(super) sha256: String,
    pub(super) packages: Vec<Package>,
    pub(super) revoked: BTreeSet<String>,
}

impl Index {
    pub(super) fn verify(
        bytes: &[u8],
        signature: &[u8],
        key: &VerifyingKey,
    ) -> Result<Self, String> {
        let signature = std::str::from_utf8(signature).map_err(|_| "Invalid MLX signature text")?;
        verify_detached(bytes, signature, key, "MLX")?;
        let wire: WireIndex = serde_json::from_slice(bytes)
            .map_err(|error| format!("Invalid signed MLX runtime index ({error})"))?;
        let mut packages = wire.packages;
        let revoked = wire.revoked_runtime_ids.unwrap_or_default();
        if wire.schema_version != 1
            || packages.len() > 1_000
            || revoked.len() > MAX_REVOKED
            || revoked.iter().any(|id| !identifier(id))
        {
            return Err("Invalid signed MLX runtime index schema or limits".into());
        }
        let generated_at = wire
            .generated_at
            .parse()
            .map_err(|_| "Invalid signed MLX runtime index timestamp")?;
        let mut unique = BTreeSet::new();
        for package in &mut packages {
            package.validate()?;
            if !unique.insert(package.installation_id.clone()) {
                return Err("Duplicate signed MLX runtime installation ID".into());
            }
        }
        let sha256 = HashAlgorithm::Sha256
            .digest(bytes)
            .map_err(|_| "Could not hash signed MLX runtime index")?
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self {
            generated_at,
            sha256,
            packages,
            revoked: revoked.into_iter().collect(),
        })
    }

    pub(super) fn select(
        &self,
        size: u64,
        sha256: &str,
        host: OsVersion,
    ) -> Result<&Package, String> {
        let mut matching = self
            .packages
            .iter()
            .filter(|package| package.size_bytes == size && package.sha256 == sha256);
        let package = matching
            .next()
            .ok_or("The selected ZIP does not match this signed MLX catalog")?;
        if matching.next().is_some() {
            return Err("The signed MLX catalog has ambiguous entries for this ZIP".into());
        }
        if self.revoked.contains(&package.runtime_id)
            || self.revoked.contains(&package.installation_id)
        {
            return Err("This signed MLX runtime has been withdrawn".into());
        }
        if !package.compatible(host) {
            return Err("This MLX runtime requires a newer macOS version".into());
        }
        Ok(package)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireIndex {
    schema_version: u8,
    generated_at: String,
    packages: Vec<Package>,
    revoked_runtime_ids: Option<Vec<String>>,
}

pub(super) fn identifier(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && !value.ends_with('.')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub(super) fn digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
