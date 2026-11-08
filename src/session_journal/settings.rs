use std::path::{Component, Path, PathBuf};
use std::{fmt, io};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub(crate) enum LogFormat {
    Xml,
    #[default]
    Json,
    Yaml,
}

impl LogFormat {
    pub(crate) const ALL: [Self; 3] = [Self::Xml, Self::Json, Self::Yaml];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Xml => "XML",
            Self::Json => "JSON",
            Self::Yaml => "YAML",
        }
    }

    pub(crate) const fn extension(self) -> &'static str {
        match self {
            Self::Xml => "xml",
            Self::Json => "json",
            Self::Yaml => "yaml",
        }
    }

    pub(crate) fn parse_policy(value: &str) -> Option<Self> {
        let value = value.trim_matches(|character| character <= ' ');
        Self::ALL
            .into_iter()
            .find(|format| format.name().eq_ignore_ascii_case(value))
    }

    pub(super) fn part_name(self, number: u32) -> String {
        let extension = self.extension();
        if number == 1 {
            format!("session-log.{extension}")
        } else {
            format!("session-log-{number}.{extension}")
        }
    }
}

impl fmt::Display for LogFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    pub(crate) directory: String,
    pub(crate) clickable_links: bool,
    pub(crate) log_format: LogFormat,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            directory: String::new(),
            clickable_links: true,
            log_format: LogFormat::default(),
        }
    }
}

impl Settings {
    pub(crate) fn validate(&self) -> io::Result<()> {
        if self.directory.len() > 4096 || self.directory.chars().any(char::is_control) {
            return Err(invalid(
                "Journal directory is invalid or longer than 4096 UTF-8 bytes",
            ));
        }
        Ok(())
    }

    pub(crate) fn directory(&self, profile: &Path) -> io::Result<PathBuf> {
        self.validate()?;
        let configured = self.directory.trim_matches(|character| character <= ' ');
        let requested = if configured.is_empty() {
            default_directory(profile)
        } else if let Some(suffix) = configured
            .strip_prefix("~/")
            .or_else(|| (configured == "~").then_some(""))
        {
            let home = std::env::home_dir()
                .filter(|home| home.is_absolute())
                .ok_or_else(|| invalid("The home directory is unavailable"))?;
            let suffix = Path::new(suffix.trim_start_matches('/'));
            if suffix.has_root()
                || suffix
                    .components()
                    .any(|part| matches!(part, Component::Prefix(_)))
            {
                return Err(invalid("Invalid home-relative journal directory"));
            }
            home.join(suffix)
        } else {
            std::path::absolute(configured)?
        };
        let mut normalized = PathBuf::new();
        for component in requested.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    normalized.pop();
                }
                component => normalized.push(component),
            }
        }
        if !normalized.is_absolute() {
            return Err(invalid(
                "Journal directory must resolve to an absolute path",
            ));
        }
        Ok(normalized)
    }
}

pub(crate) fn default_directory(profile: &Path) -> PathBuf {
    profile.join("journals")
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_settings_default_and_path_resolution_preserve_legacy_profiles() -> io::Result<()> {
        let profile = std::env::temp_dir().join("kortty-journal-profile");
        let defaults: Settings = serde_json::from_str("{}")?;
        assert_eq!(defaults, Settings::default());
        assert!(defaults.clickable_links);
        assert_eq!(defaults.directory(&profile)?, profile.join("journals"));
        let blank: Settings = serde_json::from_str(r#"{"directory":"   "}"#)?;
        assert!(blank.clickable_links);
        assert_eq!(blank.directory(&profile)?, profile.join("journals"));
        let absolute = profile.join("old").join("..").join("selected");
        let settings = Settings {
            directory: absolute.to_str().unwrap().to_owned(),
            ..Settings::default()
        };
        assert_eq!(settings.directory(&profile)?, profile.join("selected"));
        let relative = Settings {
            directory: "journal-previous/../journal-selected".into(),
            ..Settings::default()
        };
        assert_eq!(
            relative.directory(&profile)?,
            std::env::current_dir()?.join("journal-selected")
        );
        if let Some(home) = std::env::home_dir().filter(|home| home.is_absolute()) {
            let settings = Settings {
                directory: "~/kortty-journals".into(),
                ..Settings::default()
            };
            assert_eq!(settings.directory(&profile)?, home.join("kortty-journals"));
        }
        assert!(serde_json::from_str::<Settings>(r#"{"unknown": true}"#).is_err());
        Ok(())
    }

    #[test]
    fn journal_settings_reject_unbounded_or_control_paths() {
        for directory in [
            "x".repeat(4097),
            "journal\nother".into(),
            "journal\0other".into(),
        ] {
            assert!(
                Settings {
                    directory,
                    ..Settings::default()
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            Settings {
                directory: "x".repeat(4096),
                ..Settings::default()
            }
            .validate()
            .is_ok()
        );
    }
}
