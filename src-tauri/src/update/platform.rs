//! Platform detection, ported from Java `PlatformProfile`/`OperatingSystem`.

use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatingSystem {
    Windows,
    Macos,
    Linux,
    Other,
}

#[derive(Debug, Clone)]
pub struct PlatformProfile {
    pub operating_system: OperatingSystem,
    /// Canonical architecture: `x86_64`, `aarch64`, or the normalized raw value.
    pub architecture: String,
    /// Normalized `ID=` value from `/etc/os-release` (Linux only).
    pub linux_id: Option<String>,
    /// Normalized `ID_LIKE=` tokens from `/etc/os-release` (Linux only).
    pub linux_id_like: BTreeSet<String>,
}

impl PlatformProfile {
    /// Mirrors the Java compact constructor: normalizes architecture and tokens.
    pub fn new(
        operating_system: OperatingSystem,
        architecture: &str,
        linux_id: Option<&str>,
        linux_id_like: &[&str],
    ) -> PlatformProfile {
        PlatformProfile {
            operating_system,
            architecture: canonical_architecture(architecture),
            linux_id: linux_id
                .map(normalize_token)
                .filter(|token| !token.is_empty()),
            linux_id_like: linux_id_like
                .iter()
                .map(|token| normalize_token(token))
                .filter(|token| !token.is_empty())
                .collect(),
        }
    }

    /// Detects the running platform (mirrors Java `PlatformProfile.current`).
    pub fn current() -> PlatformProfile {
        let operating_system = if cfg!(target_os = "windows") {
            OperatingSystem::Windows
        } else if cfg!(target_os = "macos") {
            OperatingSystem::Macos
        } else if cfg!(target_os = "linux") {
            OperatingSystem::Linux
        } else {
            OperatingSystem::Other
        };
        let (linux_id, linux_id_like) = if operating_system == OperatingSystem::Linux {
            read_os_release(Path::new("/etc/os-release"))
        } else {
            (None, BTreeSet::new())
        };
        PlatformProfile {
            operating_system,
            architecture: canonical_architecture(std::env::consts::ARCH),
            linux_id,
            linux_id_like,
        }
    }

    /// True when any token matches the distro `ID` or one of the `ID_LIKE` values.
    pub fn linux_matches(&self, tokens: &[&str]) -> bool {
        tokens.iter().any(|token| {
            let normalized = normalize_token(token);
            self.linux_id.as_deref() == Some(normalized.as_str())
                || self.linux_id_like.contains(&normalized)
        })
    }

    /// Architecture aliases used to match asset file names.
    pub fn architecture_tokens(&self) -> Vec<&str> {
        match self.architecture.as_str() {
            "x86_64" => vec!["x86_64", "amd64", "x64"],
            "aarch64" => vec!["aarch64", "arm64"],
            other => vec![other],
        }
    }
}

pub(crate) fn canonical_architecture(architecture: &str) -> String {
    let normalized = architecture.trim().to_lowercase();
    match normalized.as_str() {
        "x86_64" | "amd64" | "x64" => "x86_64".to_string(),
        "aarch64" | "arm64" => "aarch64".to_string(),
        "" => "unknown".to_string(),
        _ => normalized,
    }
}

pub(crate) fn read_os_release(path: &Path) -> (Option<String>, BTreeSet<String>) {
    match std::fs::read_to_string(path) {
        Ok(content) => parse_os_release(&content),
        Err(_) => (None, BTreeSet::new()),
    }
}

/// Parses `/etc/os-release` content into `(ID, ID_LIKE)` (mirrors Java
/// `PlatformProfile.readOsRelease`, including quote stripping).
pub(crate) fn parse_os_release(content: &str) -> (Option<String>, BTreeSet<String>) {
    let mut id: Option<String> = None;
    let mut id_like: BTreeSet<String> = BTreeSet::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("ID=") {
            let normalized = normalize_os_release_value(value);
            id = if normalized.is_empty() {
                None
            } else {
                Some(normalized)
            };
        } else if let Some(value) = trimmed.strip_prefix("ID_LIKE=") {
            let normalized = normalize_os_release_value(value);
            for token in normalized.split_whitespace() {
                let token = normalize_token(token);
                if !token.is_empty() {
                    id_like.insert(token);
                }
            }
        }
    }
    (id, id_like)
}

fn normalize_os_release_value(value: &str) -> String {
    let mut trimmed = value.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        trimmed = &trimmed[1..trimmed.len() - 1];
    }
    normalize_token(trimmed)
}

pub(crate) fn normalize_token(token: &str) -> String {
    token.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_os_release_with_quotes_and_id_like() {
        let content = "NAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=\"debian other\"\nVERSION_ID='24.04'\n";
        let (id, id_like) = parse_os_release(content);

        assert_eq!(id.as_deref(), Some("ubuntu"));
        assert!(id_like.contains("debian"));
        assert!(id_like.contains("other"));
        assert_eq!(id_like.len(), 2);
    }

    #[test]
    fn parses_os_release_with_single_quotes_and_blank_id() {
        let content = "ID='  '\nID_LIKE='RHEL Fedora'\n";
        let (id, id_like) = parse_os_release(content);

        assert_eq!(id, None);
        assert!(id_like.contains("rhel"));
        assert!(id_like.contains("fedora"));
    }

    #[test]
    fn canonicalizes_architecture_aliases() {
        assert_eq!(canonical_architecture("amd64"), "x86_64");
        assert_eq!(canonical_architecture("X64"), "x86_64");
        assert_eq!(canonical_architecture("x86_64"), "x86_64");
        assert_eq!(canonical_architecture("arm64"), "aarch64");
        assert_eq!(canonical_architecture("AARCH64 "), "aarch64");
        assert_eq!(canonical_architecture(""), "unknown");
        assert_eq!(canonical_architecture("riscv64"), "riscv64");
    }

    #[test]
    fn linux_matches_id_and_id_like() {
        let profile = PlatformProfile::new(
            OperatingSystem::Linux,
            "x86_64",
            Some("Ubuntu"),
            &["debian"],
        );

        assert!(profile.linux_matches(&["ubuntu"]));
        assert!(profile.linux_matches(&["DEBIAN"]));
        assert!(!profile.linux_matches(&["fedora"]));
    }

    #[test]
    fn architecture_tokens_expand_aliases() {
        let profile = PlatformProfile::new(OperatingSystem::Windows, "amd64", None, &[]);
        assert_eq!(
            profile.architecture_tokens(),
            vec!["x86_64", "amd64", "x64"]
        );

        let profile = PlatformProfile::new(OperatingSystem::Macos, "arm64", None, &[]);
        assert_eq!(profile.architecture_tokens(), vec!["aarch64", "arm64"]);

        let profile = PlatformProfile::new(OperatingSystem::Linux, "riscv64", None, &[]);
        assert_eq!(profile.architecture_tokens(), vec!["riscv64"]);
    }
}
