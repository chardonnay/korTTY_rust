use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{Config, DirectoryMode, Format, MAX_LOG_MIB, MAX_RETENTION_DAYS, valid_folder};

/// Fields shared with the Java JAXB TerminalLogConfig.
pub(crate) struct JavaFields<'a> {
    pub(crate) enabled: bool,
    pub(crate) folder: &'a str,
    pub(crate) format: Format,
    pub(crate) max_mib: i32,
    pub(crate) rotate_daily: bool,
    pub(crate) compress: bool,
    pub(crate) retention_days: i32,
    pub(crate) include_username: bool,
    pub(crate) directory_mode: DirectoryMode,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct JavaReview {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) enable_requested: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) source_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source_max_mib: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source_retention_days: Option<i32>,
}

impl JavaReview {
    pub(super) fn usable_max_size(&self) -> Option<u16> {
        u16::try_from(self.source_max_mib?)
            .ok()
            .filter(|size| (1..=MAX_LOG_MIB).contains(size))
    }

    pub(super) fn validate(&self) -> io::Result<()> {
        if !valid_path_text(&self.source_path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid Java log path.",
            ));
        }
        Ok(())
    }

    pub(super) fn folder(&self) -> Option<&str> {
        let value = self.source_path.as_str();
        let path = Path::new(value);
        if !valid_folder(path) {
            return None;
        }
        // The Java field also held file paths. Use lexical inference here so
        // reviewing an imported profile never probes an unapproved location.
        let file_like = !value.ends_with(['/', '\\'])
            && path.file_stem().is_some_and(|stem| !stem.is_empty())
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    (1..=5).contains(&ext.chars().count()) && ext.chars().all(char::is_alphanumeric)
                });
        let directory = if file_like { path.parent()? } else { path };
        directory.to_str()
    }
}

pub(super) fn valid_path_text(value: &str) -> bool {
    value.len() <= 4096 && !crate::ssh_dialog::unsafe_text(value)
}

impl Config {
    pub(crate) fn java_fields(&self) -> JavaFields<'_> {
        JavaFields {
            enabled: self.enabled,
            folder: &self.folder,
            format: self.format,
            max_mib: i32::from(self.max_mib),
            rotate_daily: self.rotate_daily,
            compress: self.compress,
            retention_days: i32::from(self.retention_days),
            include_username: self.include_username,
            directory_mode: self.directory_mode,
        }
    }

    pub(crate) fn java_review(&self) -> Option<&JavaReview> {
        self.java_review.as_ref()
    }

    pub(crate) fn from_java(settings: JavaFields<'_>, mut review: JavaReview) -> io::Result<Self> {
        if !valid_path_text(settings.folder) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid Java log path.",
            ));
        }
        let max_mib = settings.max_mib.clamp(1, i32::from(MAX_LOG_MIB));
        let retention_days = settings
            .retention_days
            .clamp(0, i32::from(MAX_RETENTION_DAYS));
        review.enable_requested |= settings.enabled;
        if !settings.folder.is_empty() {
            review.source_path = settings.folder.to_owned();
        }
        if max_mib != settings.max_mib {
            review.source_max_mib = Some(settings.max_mib);
        }
        if retention_days != settings.retention_days {
            review.source_retention_days = Some(settings.retention_days);
        }
        let config = Self {
            enabled: false,
            folder: String::new(),
            format: settings.format,
            max_mib: max_mib as u16,
            rotate_daily: settings.rotate_daily,
            compress: settings.compress,
            retention_days: retention_days as u16,
            include_username: settings.include_username,
            directory_mode: settings.directory_mode,
            java_review: (review != JavaReview::default()).then_some(review),
        };
        config.validate()?;
        Ok(config)
    }

    pub(super) fn adopt_java_folder(&mut self) {
        let Some(review) = &mut self.java_review else {
            return;
        };
        let Some(folder) = review.folder() else {
            return;
        };
        self.folder = folder.to_owned();
        review.source_path.clear();
        if *review == JavaReview::default() {
            self.java_review = None;
        }
    }

    pub(super) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if enabled && let Some(review) = &mut self.java_review {
            review.enable_requested = false;
            if *review == JavaReview::default() {
                self.java_review = None;
            }
        }
    }

    pub(super) fn adopt_java_max_size(&mut self) -> Option<u16> {
        let review = self.java_review.as_mut()?;
        let size = review.usable_max_size()?;
        self.max_mib = size;
        review.source_max_mib = None;
        if *review == JavaReview::default() {
            self.java_review = None;
        }
        Some(size)
    }
}
