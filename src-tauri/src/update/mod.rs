//! Update-checker backend, ported from the Java `de.kortty.update` package.
//!
//! Submodules:
//! - [`version`]: semantic version parsing/ordering (`UpdateVersion`)
//! - [`platform`]: OS/architecture/Linux-distro detection (`PlatformProfile`)
//! - [`selector`]: picks the best release asset for the current platform
//! - [`github`]: GitHub "latest release" REST client
//! - [`downloader`]: SHA-256 verified asset download
//! - [`download_dir`]: resolves the user's Downloads directory
//! - [`service`]: background check service + suppression/snooze logic

pub mod download_dir;
pub mod downloader;
pub mod github;
pub mod platform;
pub mod selector;
pub mod service;
pub mod version;

use serde::{Deserialize, Serialize};
use version::UpdateVersion;

/// A single downloadable release asset (mirrors Java `UpdateAsset`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAsset {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub download_uri: String,
    /// Asset size in bytes; `-1` when unknown (mirrors Java).
    #[serde(default = "default_asset_size")]
    pub size: i64,
    #[serde(default)]
    pub digest: Option<String>,
}

fn default_asset_size() -> i64 {
    -1
}

/// A GitHub release (mirrors Java `UpdateRelease`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRelease {
    #[serde(default)]
    pub tag_name: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub html_uri: Option<String>,
    /// ISO-8601 timestamp of the release publication.
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<UpdateAsset>,
}

impl UpdateRelease {
    pub fn is_stable_latest_release(&self) -> bool {
        !self.draft && !self.prerelease
    }
}

/// An update that is newer than the running version (mirrors Java `AvailableUpdate`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableUpdate {
    pub release: UpdateRelease,
    pub asset: UpdateAsset,
    pub latest_version: UpdateVersion,
    pub current_version: UpdateVersion,
}

impl AvailableUpdate {
    pub fn version_label(&self) -> &str {
        &self.release.tag_name
    }
}

/// Mirrors Java `UpdateCheckResult.Status`; serialized as the variant name
/// (e.g. `"UpdateAvailable"`) to match `src/types/update.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateCheckStatus {
    UpdateAvailable,
    NoUpdate,
    NoCompatibleAsset,
    Failed,
}

/// Mirrors Java `UpdateCheckRunType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateCheckRunType {
    Manual,
    AutomaticStartup,
    AutomaticPeriodic,
}

/// Mirrors Java `UpdateCheckResult`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckResult {
    pub status: UpdateCheckStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update: Option<AvailableUpdate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl UpdateCheckResult {
    pub fn update_available(update: AvailableUpdate) -> UpdateCheckResult {
        UpdateCheckResult {
            status: UpdateCheckStatus::UpdateAvailable,
            update: Some(update),
            message: None,
        }
    }

    pub fn no_update(message: impl Into<String>) -> UpdateCheckResult {
        UpdateCheckResult {
            status: UpdateCheckStatus::NoUpdate,
            update: None,
            message: Some(message.into()),
        }
    }

    pub fn no_compatible_asset(message: impl Into<String>) -> UpdateCheckResult {
        UpdateCheckResult {
            status: UpdateCheckStatus::NoCompatibleAsset,
            update: None,
            message: Some(message.into()),
        }
    }

    pub fn failed(message: impl Into<String>) -> UpdateCheckResult {
        UpdateCheckResult {
            status: UpdateCheckStatus::Failed,
            update: None,
            message: Some(message.into()),
        }
    }
}
