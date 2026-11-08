use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use kortty_pty::{LaunchOptions, LocalShell, Sandbox};
use serde::{Deserialize, Serialize};

use crate::profile;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum IsolationLevel {
    #[default]
    None,
    Process,
    Sandbox,
}

impl IsolationLevel {
    pub(crate) const ALL: [Self; 3] = [Self::None, Self::Process, Self::Sandbox];

    pub(crate) fn is_none(&self) -> bool {
        *self == Self::None
    }

    pub(crate) fn from_id(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "process" => Some(Self::Process),
            "sandbox" => Some(Self::Sandbox),
            _ => None,
        }
    }
}

impl fmt::Display for IsolationLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::None => "None",
            Self::Process => "Own process",
            Self::Sandbox => "Own process + sandbox",
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LocalSettings {
    pub(crate) isolation: IsolationLevel,
    pub(crate) directory: String,
    #[serde(skip_serializing_if = "LocalShell::is_default")]
    pub(crate) shell: LocalShell,
    /// The Quick Connect choice remains with this live local tab and its splits.
    #[serde(skip)]
    pub(crate) shell_integration: bool,
}

impl LocalSettings {
    pub(crate) fn with_minimum(mut self, minimum: IsolationLevel) -> Self {
        self.isolation = self.isolation.max(minimum);
        self
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        if self.directory.len() > 4096 || self.directory.chars().any(char::is_control) {
            return Err(invalid("invalid sandbox directory"));
        }
        if !self.directory.is_empty() && !Path::new(&self.directory).is_absolute() {
            return Err(invalid("sandbox directory must be absolute"));
        }
        if self.isolation == IsolationLevel::Sandbox && self.directory.is_empty() {
            return Err(invalid("choose an absolute sandbox workspace first"));
        }
        Ok(())
    }

    pub(crate) fn launch_options(&self) -> io::Result<LaunchOptions> {
        self.validate()?;
        let mut options = LaunchOptions {
            shell: self.shell,
            ..LaunchOptions::default()
        };
        if self.isolation == IsolationLevel::Sandbox {
            options.working_directory = Some(PathBuf::from(&self.directory));
            options.sandbox = Some(Sandbox {
                profile_root: profile::root()?,
            });
        }
        Ok(options)
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
