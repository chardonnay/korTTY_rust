use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use kortty_recording::CaptureOptions;
use serde::{Deserialize, Serialize};

use crate::profile;

pub(super) const DEFAULT_IDLE: u16 = 20;
pub(super) const MAX_IDLE: u16 = 3600;
const MAX_BYTES: u64 = 32 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Preferences {
    pub(super) directory: Option<PathBuf>,
    pub(crate) include_color: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) enabled_after_restart: bool,
    pub(super) auto_pause: bool,
    pub(super) idle_seconds: u16,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            directory: None,
            include_color: false,
            enabled_after_restart: false,
            auto_pause: true,
            idle_seconds: DEFAULT_IDLE,
        }
    }
}

impl Preferences {
    pub(crate) fn load() -> io::Result<Self> {
        Self::load_from(&path()?)
    }

    pub(crate) fn load_from(path: &Path) -> io::Result<Self> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("Recording settings exceed the profile size limit"));
        }
        let preferences: Self = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        preferences.validate()?;
        Ok(preferences)
    }

    pub(crate) fn save(&self) -> io::Result<()> {
        self.save_to(&path()?)
    }

    pub(crate) fn save_to(&self, path: &Path) -> io::Result<()> {
        self.validate()?;
        if let Some(directory) = &self.directory {
            match fs::symlink_metadata(directory) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => return Err(invalid("Recording storage must be a directory")),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("Recording settings exceed the profile size limit"));
        }
        profile::save_atomic(path, &bytes)
    }

    pub(crate) fn directory(&self) -> io::Result<PathBuf> {
        self.directory.clone().map_or_else(default_directory, Ok)
    }

    pub(crate) fn capture_options(&self) -> CaptureOptions {
        CaptureOptions {
            include_style: self.include_color,
            idle_after: self
                .auto_pause
                .then(|| Duration::from_secs(self.idle_seconds.into())),
        }
    }

    pub(super) fn validate(&self) -> io::Result<()> {
        if !(1..=MAX_IDLE).contains(&self.idle_seconds) {
            return Err(invalid(
                "Enter an idle threshold between 1 and 3600 seconds",
            ));
        }
        if let Some(directory) = &self.directory
            && (!directory.is_absolute()
                || directory
                    .components()
                    .any(|component| component == Component::ParentDir)
                || directory.to_str().is_none_or(|name| {
                    name.len() > super::MAX_PATH || name.chars().any(char::is_control)
                }))
        {
            return Err(invalid(
                "Enter an absolute recording folder without parent segments",
            ));
        }
        Ok(())
    }
}

pub(super) fn default_directory() -> io::Result<PathBuf> {
    Ok(profile::root()?.join("recordings"))
}

fn path() -> io::Result<PathBuf> {
    Ok(profile::root()?.join("video-settings.json"))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording_window::tests::TestDirectory;

    #[test]
    fn capture_preferences_round_trip_and_a_failed_save_preserves_previous_settings() {
        let scratch = TestDirectory::new();
        let config = scratch.0.join("video-settings.json");
        let defaults = Preferences::load_from(&config).unwrap();
        assert!(!defaults.enabled_after_restart);
        assert!(!defaults.capture_options().include_style);
        assert_eq!(
            defaults.capture_options().idle_after,
            Some(Duration::from_secs(20))
        );

        let configured = Preferences {
            directory: Some(scratch.0.join("recordings")),
            include_color: true,
            enabled_after_restart: true,
            auto_pause: false,
            idle_seconds: 71,
        };
        configured.save_to(&config).unwrap();
        let loaded = Preferences::load_from(&config).unwrap();
        assert_eq!(loaded, configured);
        assert!(loaded.capture_options().include_style);
        assert!(loaded.capture_options().idle_after.is_none());

        let invalid = Preferences {
            directory: Some(config.clone()),
            ..loaded
        };
        assert_eq!(
            invalid.save_to(&config).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(Preferences::load_from(&config).unwrap(), configured);
        let stored: serde_json::Value =
            serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
        assert!(stored.get("enabled").is_none());
        assert_eq!(stored["enabled_after_restart"], true);
    }
}
