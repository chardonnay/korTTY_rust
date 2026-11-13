//! Trust and compatibility rules for the signed llama.cpp stable runtime index.

use std::collections::HashSet;
use std::fmt;
use std::sync::LazyLock;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use ed25519_dalek::{Signature, VerifyingKey};
use jiff::Timestamp;
use pgp::crypto::hash::HashAlgorithm;
use regex::Regex;
use serde::Deserialize;
use url::Url;

pub(crate) const MAX_INDEX_BYTES: usize = 5 * 1024 * 1024;
pub(crate) const MAX_SIGNATURE_BYTES: usize = 4096;
pub(crate) const API_CONTRACT_VERSION: i32 = 1;

// Raw Ed25519 key from config/trust/llama-runtime-ed25519-public.pem at 1ebbe724ee84.
pub(super) const RELEASE_KEY: [u8; 32] = [
    0x7e, 0x81, 0xfb, 0xbf, 0x0a, 0x66, 0x4a, 0xe1, 0x58, 0xe3, 0x33, 0x36, 0x71, 0xf8, 0xd0, 0xb3,
    0x42, 0x4c, 0xa0, 0xf1, 0x60, 0xfb, 0xbb, 0xaf, 0x35, 0xa7, 0x6c, 0xf3, 0x1e, 0x65, 0xd7, 0x97,
];

static RUNTIME_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\Allama-b[0-9]+-kortty[1-9][0-9]*\z").unwrap());
static APP_VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\Av?([0-9]+)(?:\.([0-9]+))?(?:\.([0-9]+))?(?:-([0-9A-Za-z.-]+))?(?:\+.*)?\z")
        .unwrap()
});

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Host {
    platform: &'static str,
    architecture: &'static str,
}

impl Host {
    pub(crate) fn current() -> Result<Self, String> {
        Self::new(std::env::consts::OS, std::env::consts::ARCH)
    }

    pub(crate) fn new(platform: &str, architecture: &str) -> Result<Self, String> {
        let platform = match platform.trim().to_ascii_lowercase().as_str() {
            "linux" => "linux",
            "macos" => "macos",
            "windows" => "windows",
            _ => return Err("Unsupported llama.cpp runtime platform".into()),
        };
        let architecture = match architecture.trim().to_ascii_lowercase().as_str() {
            "amd64" | "x64" | "x86-64" | "x86_64" => "x86_64",
            "arm64" | "aarch64" => "aarch64",
            _ => return Err("Unsupported llama.cpp runtime architecture".into()),
        };
        Ok(Self {
            platform,
            architecture,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackageBackend {
    Cpu,
    Metal,
    Vulkan,
}

impl PackageBackend {
    fn parse(value: &str, host: Host) -> Result<Self, String> {
        match value {
            "cpu" => Ok(Self::Cpu),
            "metal" if host.platform == "macos" => Ok(Self::Metal),
            "vulkan" => Ok(Self::Vulkan),
            _ => Err("Invalid llama.cpp package backend for this platform".into()),
        }
    }
}

impl fmt::Display for PackageBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Cpu => "CPU",
            Self::Metal => "Metal",
            Self::Vulkan => "Vulkan",
        })
    }
}

#[derive(Clone)]
pub(crate) struct Package {
    runtime_id: String,
    installation_id: String,
    host: Host,
    backend: PackageBackend,
    size: u64,
    sha256: String,
    download_url: String,
    entrypoint: String,
    api_contract_version: i32,
    minimum_version: Option<Version>,
}

impl fmt::Debug for Package {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Signed runtime package")
            .field("installation_id", &self.installation_id)
            .field("backend", &self.backend)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl Package {
    pub(crate) fn runtime_id(&self) -> &str {
        &self.runtime_id
    }

    pub(crate) fn installation_id(&self) -> &str {
        &self.installation_id
    }

    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    pub(super) fn download_url(&self) -> &str {
        &self.download_url
    }

    pub(crate) fn entrypoint(&self) -> &str {
        &self.entrypoint
    }

    pub(crate) fn backend(&self) -> PackageBackend {
        self.backend
    }

    fn parse(wire: PackageJson) -> Result<Self, String> {
        let runtime_id = wire.runtime_id.trim();
        if !RUNTIME_ID.is_match(runtime_id) {
            return Err("Invalid immutable llama.cpp runtime ID".into());
        }
        let llama_tag = wire.llama_tag.trim();
        if !llama_tag.strip_prefix('b').is_some_and(|build| {
            !build.is_empty() && build.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            return Err("Invalid llama.cpp release tag".into());
        }
        let host = Host::new(&wire.platform, &wire.architecture)?;
        let backend = wire.backend.trim().to_ascii_lowercase();
        let package_backend = PackageBackend::parse(&backend, host)?;
        if !valid_hex(wire.commit.trim(), 40) {
            return Err("Invalid llama.cpp runtime commit".into());
        }
        let sha256 = wire.sha256.trim();
        if !valid_hex(sha256, 64) {
            return Err("Invalid llama.cpp package SHA-256".into());
        }
        let minimum_version = wire.minimum_kortty_version.trim();
        if wire.size <= 0 || wire.api_contract_version <= 0 || minimum_version.is_empty() {
            return Err("Invalid llama.cpp package size or compatibility requirements".into());
        }
        validate_package_url(wire.download_url.trim())?;
        let entrypoint = wire.entrypoint.trim().replace('\\', "/");
        validate_package_entrypoint(&entrypoint)?;

        Ok(Self {
            runtime_id: runtime_id.into(),
            installation_id: format!(
                "{runtime_id}-{}-{}-{backend}",
                host.platform, host.architecture
            ),
            host,
            backend: package_backend,
            size: wire.size as u64,
            sha256: sha256.to_ascii_lowercase(),
            download_url: wire.download_url.trim().into(),
            entrypoint,
            api_contract_version: wire.api_contract_version,
            minimum_version: Version::parse(minimum_version),
        })
    }
}

#[derive(Debug)]
pub(crate) struct VerifiedIndex {
    generated_at: Timestamp,
    sha256: String,
    packages: Vec<Package>,
    revoked_ids: HashSet<String>,
}

impl VerifiedIndex {
    pub(crate) fn verify(index_bytes: &[u8], detached_signature: &str) -> Result<Self, String> {
        let key = VerifyingKey::from_bytes(&RELEASE_KEY)
            .map_err(|_| "Invalid embedded llama.cpp runtime verification key")?;
        Self::verify_signature(index_bytes, detached_signature, &key)
    }

    #[cfg(test)]
    pub(crate) fn verify_with_key(
        index_bytes: &[u8],
        detached_signature: &str,
        key: &VerifyingKey,
    ) -> Result<Self, String> {
        Self::verify_signature(index_bytes, detached_signature, key)
    }

    fn verify_signature(
        index_bytes: &[u8],
        detached_signature: &str,
        key: &VerifyingKey,
    ) -> Result<Self, String> {
        verify_detached(index_bytes, detached_signature, key, "llama.cpp")?;
        Self::parse(index_bytes)
    }

    fn parse(index_bytes: &[u8]) -> Result<Self, String> {
        let wire: IndexJson = serde_json::from_slice(index_bytes)
            .map_err(|error| format!("Invalid llama.cpp runtime index JSON ({error})"))?;
        if wire.schema_version != 1 {
            return Err("Unsupported llama.cpp runtime index schema".into());
        }
        let generated_at = wire
            .generated_at
            .trim()
            .parse()
            .map_err(|_| "Invalid llama.cpp runtime index generation timestamp")?;
        let mut revoked_ids = HashSet::new();
        for id in wire.revoked_runtime_ids.unwrap_or_default() {
            if id.trim().is_empty() {
                return Err("Invalid llama.cpp runtime revocation ID".into());
            }
            revoked_ids.insert(id.trim().into());
        }
        let mut packages = Vec::with_capacity(wire.packages.len());
        let mut installations = HashSet::new();
        for wire_package in wire.packages {
            let revoked = wire_package.revoked;
            let package = Package::parse(wire_package)?;
            if !installations.insert(package.installation_id.clone()) {
                return Err(format!(
                    "Duplicate llama.cpp runtime package {}",
                    package.installation_id
                ));
            }
            if revoked {
                revoked_ids.insert(package.runtime_id.clone());
                revoked_ids.insert(package.installation_id.clone());
            }
            packages.push(package);
        }
        let sha256 = HashAlgorithm::Sha256
            .digest(index_bytes)
            .map_err(|_| "Could not hash llama.cpp runtime index")?
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self {
            generated_at,
            sha256,
            packages,
            revoked_ids,
        })
    }

    pub(crate) fn generated_at(&self) -> Timestamp {
        self.generated_at
    }

    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    pub(crate) fn revoked_ids(&self) -> &HashSet<String> {
        &self.revoked_ids
    }

    pub(crate) fn packages(&self) -> &[Package] {
        &self.packages
    }

    pub(crate) fn is_revoked(&self, package: &Package) -> bool {
        self.revoked_ids.contains(package.runtime_id())
            || self.revoked_ids.contains(package.installation_id())
    }

    pub(crate) fn is_compatible(
        &self,
        package: &Package,
        host: Host,
        supported_contract: i32,
        kortty_version: &str,
    ) -> Result<bool, String> {
        let version =
            Version::parse(kortty_version).ok_or("Invalid current korTTY compatibility version")?;
        Ok(package.host == host
            && package.api_contract_version == supported_contract
            && package
                .minimum_version
                .as_ref()
                .is_some_and(|minimum| &version >= minimum)
            && !self.is_revoked(package))
    }
}

pub(super) fn verify_detached(
    index: &[u8],
    detached_signature: &str,
    key: &VerifyingKey,
    runtime: &str,
) -> Result<(), String> {
    if index.is_empty() || index.len() > MAX_INDEX_BYTES {
        return Err(format!("Invalid {runtime} runtime index size"));
    }
    if detached_signature.len() > MAX_SIGNATURE_BYTES {
        return Err(format!("Invalid {runtime} runtime signature size"));
    }
    let encoded = detached_signature.trim();
    let encoded = encoded.strip_prefix("ed25519:").unwrap_or(encoded).trim();
    let decoded = if encoded.ends_with('=') {
        STANDARD.decode(encoded)
    } else {
        STANDARD_NO_PAD.decode(encoded)
    }
    .map_err(|_| format!("Invalid Base64 {runtime} runtime signature"))?;
    let signature = Signature::from_slice(&decoded)
        .map_err(|_| format!("Invalid {runtime} runtime signature length"))?;
    key.verify_strict(index, &signature)
        .map_err(|_| format!("{runtime} runtime index signature verification failed"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexJson {
    schema_version: i32,
    generated_at: String,
    packages: Vec<PackageJson>,
    revoked_runtime_ids: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageJson {
    runtime_id: String,
    llama_tag: String,
    commit: String,
    api_contract_version: i32,
    minimum_kortty_version: String,
    platform: String,
    architecture: String,
    backend: String,
    size: i64,
    sha256: String,
    download_url: String,
    entrypoint: String,
    #[serde(default)]
    revoked: bool,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Version {
    numeric: [i32; 3],
    release: (bool, String),
}

impl Version {
    fn parse(text: &str) -> Option<Self> {
        let version = APP_VERSION.captures(text.trim())?;
        let number = |index| {
            version
                .get(index)
                .map_or(Some(0), |n| n.as_str().parse().ok())
        };
        Some(Self {
            numeric: [number(1)?, number(2)?, number(3)?],
            // A final release sorts after all prereleases of the same version.
            release: version.get(4).map_or_else(
                || (true, String::new()),
                |release| (false, release.as_str().into()),
            ),
        })
    }
}

fn valid_hex(value: &str, digits: usize) -> bool {
    value.len() == digits && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_package_url(text: &str) -> Result<(), String> {
    const ERROR: &str = "Invalid HTTPS llama.cpp package URL";
    let (scheme, authority) = text.split_once("://").ok_or(ERROR)?;
    let authority = authority.split(['/', '?', '#']).next().unwrap_or_default();
    let url = Url::parse(text).map_err(|_| ERROR)?;
    let https = scheme.eq_ignore_ascii_case("https");
    let loopback = scheme.eq_ignore_ascii_case("http")
        && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(https || loopback)
        || text.chars().any(|ch| ch.is_control() || ch == '\\')
        || authority.is_empty()
        || authority.contains('@')
        || !url.has_host()
        || url.fragment().is_some()
    {
        return Err(ERROR.into());
    }
    Ok(())
}

fn validate_package_entrypoint(text: &str) -> Result<(), String> {
    if text.split(['/', '\\']).any(|part| {
        part.is_empty()
            || part.ends_with(['.', ' '])
            || part
                .chars()
                .any(|ch| ch.is_control() || matches!(ch, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
    }) {
        return Err("llama.cpp package entrypoint must be a safe relative path".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
