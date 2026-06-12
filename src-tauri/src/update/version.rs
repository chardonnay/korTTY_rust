//! Semantic version parsing and ordering, ported from Java `UpdateVersion`.
//!
//! Accepts `v?major[.minor[.patch]][-prerelease][+build]` (Java pattern
//! `^v?(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:-([0-9A-Za-z.-]+))?(?:\+.*)?$`).

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cmp::Ordering;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UpdateVersion {
    major: i32,
    minor: i32,
    patch: i32,
    prerelease: Option<String>,
}

impl UpdateVersion {
    /// Parses a version string; returns `None` when the text does not match
    /// the supported pattern (mirrors Java `UpdateVersion.parse`).
    pub fn parse(text: &str) -> Option<UpdateVersion> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        let rest = trimmed.strip_prefix('v').unwrap_or(trimmed);
        // Build metadata (`+...`) is ignored, exactly like the Java pattern.
        let rest = match rest.find('+') {
            Some(index) => &rest[..index],
            None => rest,
        };
        let (numbers, prerelease) = match rest.find('-') {
            Some(index) => (&rest[..index], Some(&rest[index + 1..])),
            None => (rest, None),
        };
        if let Some(pre) = prerelease {
            let valid = !pre.is_empty()
                && pre
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
            if !valid {
                return None;
            }
        }
        let mut parts = numbers.split('.');
        let major = parse_component(parts.next()?)?;
        let minor_part = parts.next();
        let patch_part = parts.next();
        if parts.next().is_some() {
            return None;
        }
        let minor = match minor_part {
            Some(part) => parse_component(part)?,
            None => 0,
        };
        let patch = match patch_part {
            Some(part) => parse_component(part)?,
            None => 0,
        };
        Some(UpdateVersion {
            major,
            minor,
            patch,
            prerelease: prerelease.map(str::to_string),
        })
    }

    pub fn major(&self) -> i32 {
        self.major
    }

    pub fn minor(&self) -> i32 {
        self.minor
    }

    pub fn patch(&self) -> i32 {
        self.patch
    }

    pub fn prerelease(&self) -> Option<&str> {
        self.prerelease.as_deref()
    }

    pub fn is_newer_than(&self, other: &UpdateVersion) -> bool {
        self > other
    }
}

fn parse_component(part: &str) -> Option<i32> {
    if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // Java uses Integer.parseInt; overflow rejects the version.
    part.parse::<i32>().ok()
}

impl Ord for UpdateVersion {
    /// Mirrors Java `UpdateVersion.compareTo`: major/minor/patch numerically,
    /// then a release (no prerelease) sorts above any prerelease, and two
    /// prereleases compare lexicographically.
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then_with(|| self.minor.cmp(&other.minor))
            .then_with(|| self.patch.cmp(&other.patch))
            .then_with(|| match (&self.prerelease, &other.prerelease) {
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (None, None) => Ordering::Equal,
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
}

impl PartialOrd for UpdateVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for UpdateVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(prerelease) = &self.prerelease {
            write!(f, "-{prerelease}")?;
        }
        Ok(())
    }
}

impl Serialize for UpdateVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for UpdateVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        UpdateVersion::parse(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid update version: {text}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_semantic_versions_numerically() {
        let older = UpdateVersion::parse("v2.2.0").unwrap();
        let newer = UpdateVersion::parse("v2.10.0").unwrap();

        assert!(newer > older);
        assert!(newer.is_newer_than(&older));
        assert!(!older.is_newer_than(&newer));
    }

    #[test]
    fn treats_release_as_newer_than_prerelease() {
        let prerelease = UpdateVersion::parse("v2.2.0-beta.1").unwrap();
        let release = UpdateVersion::parse("2.2.0").unwrap();

        assert!(release > prerelease);
    }

    #[test]
    fn compares_prereleases_lexicographically() {
        let alpha = UpdateVersion::parse("2.2.0-alpha").unwrap();
        let beta = UpdateVersion::parse("2.2.0-beta").unwrap();

        assert!(beta > alpha);
    }

    #[test]
    fn rejects_unparseable_versions() {
        assert!(UpdateVersion::parse("release-two").is_none());
        assert!(UpdateVersion::parse("").is_none());
        assert!(UpdateVersion::parse("   ").is_none());
        assert!(UpdateVersion::parse("1.2.3.4").is_none());
        assert!(UpdateVersion::parse("1..2").is_none());
        assert!(UpdateVersion::parse("1.2.3-").is_none());
        assert!(UpdateVersion::parse("99999999999").is_none());
    }

    #[test]
    fn parses_partial_versions_with_zero_defaults() {
        let version = UpdateVersion::parse("v3").unwrap();
        assert_eq!(
            (version.major(), version.minor(), version.patch()),
            (3, 0, 0)
        );
        assert_eq!(version.to_string(), "3.0.0");

        let version = UpdateVersion::parse("3.1").unwrap();
        assert_eq!(
            (version.major(), version.minor(), version.patch()),
            (3, 1, 0)
        );
    }

    #[test]
    fn ignores_build_metadata() {
        let plain = UpdateVersion::parse("2.3.0").unwrap();
        let with_build = UpdateVersion::parse("2.3.0+build.7").unwrap();
        assert_eq!(plain, with_build);

        let pre_with_build = UpdateVersion::parse("2.3.0-rc.1+abc").unwrap();
        assert_eq!(pre_with_build.prerelease(), Some("rc.1"));
        assert_eq!(pre_with_build.to_string(), "2.3.0-rc.1");
    }

    #[test]
    fn serializes_as_display_string() {
        let version = UpdateVersion::parse("v2.3.0-beta.1").unwrap();
        assert_eq!(serde_json::to_string(&version).unwrap(), "\"2.3.0-beta.1\"");
        let parsed: UpdateVersion = serde_json::from_str("\"2.3.0-beta.1\"").unwrap();
        assert_eq!(parsed, version);
    }
}
