use std::path::PathBuf;

use kortty_server_archive::protocol::ArchiveMemberLayout;
use serde::{Deserialize, Serialize};

use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_store::Store;

use super::{Archive, ArchiveFormat, ConnectionRef, RemotePath};

pub(crate) const REVIEW_REQUIRED: &str =
    "Review and adopt the imported archive before running or enabling this job.";

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    pub(crate) sources: Vec<RemotePath>,
    pub(crate) target: RemotePath,
    pub(crate) format: ArchiveFormat,
    pub(crate) level: u8,
    pub(crate) java_excludes: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) java_sudo: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) local_hint: Option<String>,
}

impl Settings {
    pub(crate) fn member_layout(&self) -> ArchiveMemberLayout {
        if self.format.supports_server_member_paths() {
            ArchiveMemberLayout::ServerPath
        } else {
            ArchiveMemberLayout::SourceBasename
        }
    }

    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        use super::remote::{MAX_ARCHIVE_EXCLUDES, MAX_ARCHIVE_PATTERN_BYTES};

        let display_path = |path: &RemotePath| {
            path.root() == "/"
                && path.as_str().trim() == path.as_str()
                && display_text(path.as_str(), 4096)
        };
        if !self.format.supports_server_member_paths()
            || !display_path(&self.target)
            || self
                .sources
                .iter()
                .any(|path| !display_path(path) || path.contains(&self.target))
            || self.java_excludes.len() > MAX_ARCHIVE_EXCLUDES
            || self
                .java_excludes
                .iter()
                .any(|pattern| !display_text(pattern, MAX_ARCHIVE_PATTERN_BYTES))
            || self
                .local_hint
                .as_deref()
                .is_some_and(|path| !display_text(path, 4096))
        {
            return Err("Invalid imported Scheduler archive settings");
        }
        self.archive(Vec::new(), None, None).validate_layout()
    }

    fn archive(
        &self,
        excludes: Vec<String>,
        download: Option<PathBuf>,
        password_credential: Option<String>,
    ) -> Archive {
        Archive {
            sources: self.sources.clone(),
            target: self.target.clone(),
            format: self.format,
            level: self.level,
            excludes,
            download,
            password_credential,
            server_output: None,
            reuse_output: false,
            member_layout: self.member_layout(),
        }
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Draft {
    pub(crate) route: ConnectionRef,
    pub(crate) settings: Settings,
}

impl Draft {
    pub(super) fn require_current_route(
        &self,
        store: &Store,
        trust: &kortty_ssh::TrustStore,
        policy: &ClipboardPolicy,
    ) -> Result<(), &'static str> {
        let saved = store
            .get(self.route.id)
            .ok_or("The imported SSH route is no longer saved.")?;
        if ConnectionRef::from(saved) != self.route {
            return Err("The saved SSH route no longer matches the imported archive.");
        }
        if saved.incognito || !saved.isolation.is_none() {
            return Err("This saved connection requires privacy or isolation unavailable to jobs.");
        }
        let pins = super::sync::pins(&self.route, trust)
            .map_err(|_| "Verify the saved SSH route host keys before adopting the archive.")?;
        if self.settings.java_sudo && !policy.scheduler_sudo_allowed {
            return Err("Application policy forbids Scheduler sudo for this archive.");
        }
        let access = policy.server_access();
        if policy.problem.is_some()
            || !policy.minimum_isolation.is_none()
            || (self.settings.local_hint.is_some() && !policy.file_transfer_allowed)
            || access
                .first_blocked(
                    self.route
                        .jump
                        .iter()
                        .chain(std::iter::once(&self.route.target))
                        .map(|login| (login.host.as_str(), login.port)),
                )
                .is_some()
            || pins
                .iter()
                .any(|(endpoint, _)| !access.allows(endpoint.wire_host().as_ref(), endpoint.port()))
        {
            return Err("Application policy forbids this archive route or local download.");
        }
        Ok(())
    }

    pub(super) fn adopt(
        &self,
        excludes: Vec<String>,
        download: Option<PathBuf>,
        password_credential: Option<String>,
    ) -> Result<Archive, &'static str> {
        if self.settings.local_hint.is_some() && download.is_none() {
            return Err("Choose a new local archive destination on this device.");
        }
        let archive = self
            .settings
            .archive(excludes, download, password_credential);
        archive.server_spec()?;
        Ok(archive)
    }
}

fn display_text(text: &str, max_bytes: usize) -> bool {
    !text.trim().is_empty()
        && text.len() <= max_bytes
        && text
            .chars()
            .all(|ch| !ch.is_control() && crate::session_journal::is_display_character(ch))
}
