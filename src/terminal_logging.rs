use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

use iced::Element;
use iced::widget::{checkbox, column, pick_list, row, text, text_input};
use serde::{Deserialize, Serialize};

mod capture;
mod policy;
mod writer;

pub(crate) use capture::{Capture, Prepared, RedactionSecrets, Redactor, Secrets, Warning};
pub(crate) use policy::Policy;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Format {
    #[default]
    Text,
    Xml,
    Json,
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Text => "Plain Text",
            Self::Xml => "XML",
            Self::Json => "JSON",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DirectoryMode {
    Full,
    Relative,
    #[default]
    None,
}

impl fmt::Display for DirectoryMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::None => "Do not include",
            Self::Full => "Full path",
            Self::Relative => "Relative to first reported directory",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Config {
    enabled: bool,
    folder: String,
    format: Format,
    max_mib: u16,
    rotate_daily: bool,
    compress: bool,
    retention_days: u16,
    include_username: bool,
    directory_mode: DirectoryMode,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            folder: String::new(),
            format: Format::Text,
            max_mib: 10,
            rotate_daily: true,
            compress: true,
            retention_days: 30,
            include_username: false,
            directory_mode: DirectoryMode::None,
        }
    }
}

impl Config {
    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        if self.folder.len() > 4096
            || self.folder.chars().any(char::is_control)
            || (!self.folder.is_empty() && !valid_folder(Path::new(&self.folder)))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Use an absolute log folder without parent segments.",
            ));
        }
        if !(1..=256).contains(&self.max_mib) || self.retention_days > 3650 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Use a log size of 1–256 MiB and retention of 0–3650 days.",
            ));
        }
        Ok(())
    }

    fn directory(&self, profile: &Path) -> PathBuf {
        if self.folder.is_empty() {
            profile.join("terminal-logs")
        } else {
            PathBuf::from(&self.folder)
        }
    }
}

fn valid_folder(path: &Path) -> bool {
    path.is_absolute() && !path.components().any(|part| part == Component::ParentDir)
}

#[derive(Clone, Debug)]
pub(crate) enum Edit {
    Enabled(bool),
    Folder(String),
    Format(Format),
    Size(String),
    Daily(bool),
    Compress(bool),
    Retention(String),
    Username(bool),
    Directory(DirectoryMode),
}

pub(crate) struct Form {
    config: Config,
    size: String,
    retention: String,
}

impl Form {
    pub(crate) fn new(config: Config) -> Self {
        Self {
            size: config.max_mib.to_string(),
            retention: config.retention_days.to_string(),
            config,
        }
    }

    pub(crate) fn update(&mut self, edit: Edit) {
        match edit {
            Edit::Enabled(value) => self.config.enabled = value,
            Edit::Folder(value) if value.len() <= 4096 && !value.chars().any(char::is_control) => {
                self.config.folder = value
            }
            Edit::Format(value) => self.config.format = value,
            Edit::Size(value)
                if value.len() <= 3 && value.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                self.size = value
            }
            Edit::Daily(value) => self.config.rotate_daily = value,
            Edit::Compress(value) => self.config.compress = value,
            Edit::Retention(value)
                if value.len() <= 4 && value.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                self.retention = value
            }
            Edit::Username(value) => self.config.include_username = value,
            Edit::Directory(value) => self.config.directory_mode = value,
            _ => {}
        }
    }

    pub(crate) fn config(&self) -> io::Result<Config> {
        let mut config = self.config.clone();
        if config.enabled {
            config.max_mib = self.size.parse().unwrap_or_default();
            config.retention_days = self.retention.parse().unwrap_or(u16::MAX);
            config.validate()?;
        } else if config.validate().is_err() {
            config = Config::default();
        }
        Ok(config)
    }

    pub(crate) fn view(&self, eligible: bool, advanced: bool) -> Element<'_, Edit> {
        let mut fields = column![
            text("Terminal Logging").size(17),
            checkbox(self.config.enabled && eligible)
                .label("Log terminal output")
                .on_toggle_maybe(eligible.then_some(Edit::Enabled))
        ]
        .spacing(6);
        if !eligible {
            return fields
                .push(text("Terminal logging is unavailable for this session.").size(12))
                .into();
        }
        if !self.config.enabled {
            return fields.into();
        }
        fields = fields.push(
            text_input(
                "Default logfile folder or an absolute path",
                &self.config.folder,
            )
            .on_input(Edit::Folder),
        );
        fields = fields.push(
            row![
                pick_list(
                    [Format::Text, Format::Xml, Format::Json],
                    Some(self.config.format),
                    Edit::Format
                ),
                checkbox(self.config.compress)
                    .label("Compress closed logs (gzip)")
                    .on_toggle(Edit::Compress),
            ]
            .spacing(8),
        );
        if advanced {
            fields = fields.push(row![
                text_input("Max size (MiB)", &self.size).width(110).on_input(Edit::Size),
                text_input("Retention (days)", &self.retention).width(125).on_input(Edit::Retention),
                checkbox(self.config.rotate_daily).label("Rotate daily").on_toggle(Edit::Daily),
            ].spacing(8)).push(text("Automatic retention applies to logs created in the private default folder. Zero keeps them.").size(12))
            .push(row![
                checkbox(self.config.include_username).label("Include SSH username").on_toggle(Edit::Username),
                pick_list([DirectoryMode::None, DirectoryMode::Full, DirectoryMode::Relative], Some(self.config.directory_mode), Edit::Directory),
            ].spacing(8));
        }
        if let Err(error) = self.config() {
            fields = fields.push(text(error.to_string()).size(12));
        }
        fields.push(text("Known SSH credentials and managed patterns are masked. Treat other terminal output as sensitive.").size(12)).into()
    }
}

#[cfg(test)]
mod tests;
