use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use kortty_pty::{
    LaunchOptions, LocalShell, Sandbox, validate_local_command, validate_shell_integration,
};
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

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Process => "process",
            Self::Sandbox => "sandbox",
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct IsolationChoice(pub(crate) Option<IsolationLevel>);

impl IsolationChoice {
    pub(crate) const ALL: [Self; 4] = [
        Self(None),
        Self(Some(IsolationLevel::None)),
        Self(Some(IsolationLevel::Process)),
        Self(Some(IsolationLevel::Sandbox)),
    ];
}

impl fmt::Display for IsolationChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(level) => level.fmt(formatter),
            None => formatter.write_str("Inherit"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LocalIsolation {
    #[default]
    Global,
    Inherit,
    None,
    Process,
    Sandbox,
}

impl LocalIsolation {
    pub(crate) const ALL: [Self; 5] = [
        Self::Inherit,
        Self::None,
        Self::Process,
        Self::Sandbox,
        Self::Global,
    ];

    pub(crate) fn is_global(&self) -> bool {
        *self == Self::Global
    }

    pub(crate) fn from_override(level: Option<IsolationLevel>) -> Self {
        match level {
            None => Self::Inherit,
            Some(IsolationLevel::None) => Self::None,
            Some(IsolationLevel::Process) => Self::Process,
            Some(IsolationLevel::Sandbox) => Self::Sandbox,
        }
    }

    pub(crate) fn level(self) -> Option<IsolationLevel> {
        match self {
            Self::Global | Self::Inherit => None,
            Self::None => Some(IsolationLevel::None),
            Self::Process => Some(IsolationLevel::Process),
            Self::Sandbox => Some(IsolationLevel::Sandbox),
        }
    }

    pub(crate) fn resolve(
        self,
        default: IsolationLevel,
        folder: Option<(&str, IsolationLevel)>,
        minimum: IsolationLevel,
    ) -> (IsolationLevel, IsolationSource<'_>) {
        let folder = if self == Self::Inherit { folder } else { None };
        resolve_isolation(default, folder, self.level(), None, false, minimum)
    }
}

impl fmt::Display for LocalIsolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_global() {
            return formatter.write_str("Global Settings");
        }
        IsolationChoice(self.level()).fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IsolationSource<'a> {
    Settings,
    Folder(&'a str),
    Connection,
    Request,
    Managed,
}

impl fmt::Display for IsolationSource<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Settings => formatter.write_str("Global Settings"),
            Self::Folder(path) => write!(formatter, "Folder {path}"),
            Self::Connection => formatter.write_str("Connection"),
            Self::Request => formatter.write_str("Session request"),
            Self::Managed => formatter.write_str("Managed minimum"),
        }
    }
}

pub(crate) fn resolve_isolation<'a>(
    default: IsolationLevel,
    folder: Option<(&'a str, IsolationLevel)>,
    connection: Option<IsolationLevel>,
    request: Option<IsolationLevel>,
    teamwork: bool,
    minimum: IsolationLevel,
) -> (IsolationLevel, IsolationSource<'a>) {
    let mut resolved = (default, IsolationSource::Settings);
    if let Some((path, level)) = folder
        && (!teamwork || level > resolved.0)
    {
        resolved = (level, IsolationSource::Folder(path));
    }
    if let Some(level) = request.or(connection)
        && (!teamwork || level > resolved.0)
    {
        resolved = (
            level,
            if request.is_some() {
                IsolationSource::Request
            } else {
                IsolationSource::Connection
            },
        );
    }
    if minimum > resolved.0 {
        resolved = (minimum, IsolationSource::Managed);
    }
    resolved
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LocalSettings {
    pub(crate) isolation: IsolationLevel,
    pub(crate) directory: String,
    #[serde(skip_serializing_if = "LocalShell::is_default")]
    pub(crate) shell: LocalShell,
    /// Saved launch data comes from the local connection store, not a project.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) saved_profile: Option<u64>,
    #[serde(skip)]
    pub(crate) command: Option<Vec<String>>,
    #[serde(skip)]
    pub(crate) working_directory: Option<PathBuf>,
    /// A reviewed opt-in for the native startup wrapper, retained on project restore.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
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
        if let Some(command) = &self.command {
            validate_local_command(command)?;
            if !self.shell.is_default() {
                return Err(invalid("a custom command cannot use a shell preset"));
            }
        }
        if self.shell_integration {
            validate_shell_integration(self.shell, self.command.as_deref())?;
        }
        if let Some(directory) = &self.working_directory {
            if !directory.is_absolute()
                || directory.as_os_str().len() > 4096
                || directory.to_string_lossy().chars().any(char::is_control)
            {
                return Err(invalid("working directory must be an absolute directory"));
            }
            if self.isolation == IsolationLevel::Sandbox && directory != Path::new(&self.directory)
            {
                return Err(invalid(
                    "the working directory must match the configured sandbox workspace",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn launch_options(&self) -> io::Result<LaunchOptions> {
        self.validate()?;
        let mut options = LaunchOptions {
            shell: self.shell,
            command: self.command.clone(),
            working_directory: self.working_directory.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_isolation_distinguishes_legacy_defaults_folder_inheritance_and_explicit_none() {
        let folder = Some(("Development", IsolationLevel::Sandbox));
        let default = IsolationLevel::Process;
        assert_eq!(
            LocalIsolation::default().resolve(default, folder, IsolationLevel::None),
            (default, IsolationSource::Settings)
        );
        assert_eq!(
            LocalIsolation::Inherit.resolve(default, folder, IsolationLevel::None),
            (
                IsolationLevel::Sandbox,
                IsolationSource::Folder("Development")
            )
        );
        assert_eq!(
            LocalIsolation::None.resolve(default, folder, IsolationLevel::None),
            (IsolationLevel::None, IsolationSource::Connection)
        );
        assert_eq!(
            LocalIsolation::None.resolve(default, folder, IsolationLevel::Process),
            (IsolationLevel::Process, IsolationSource::Managed)
        );
    }

    #[test]
    fn teamwork_isolation_resolves_request_in_place_of_the_shared_connection_choice() {
        let inherited = Some(("Production", IsolationLevel::Process));
        assert_eq!(
            resolve_isolation(
                IsolationLevel::None,
                inherited,
                Some(IsolationLevel::Sandbox),
                None,
                true,
                IsolationLevel::None
            ),
            (IsolationLevel::Sandbox, IsolationSource::Connection)
        );
        assert_eq!(
            resolve_isolation(
                IsolationLevel::None,
                inherited,
                Some(IsolationLevel::Sandbox),
                Some(IsolationLevel::None),
                true,
                IsolationLevel::None
            ),
            (
                IsolationLevel::Process,
                IsolationSource::Folder("Production")
            )
        );
        assert_eq!(
            resolve_isolation(
                IsolationLevel::Sandbox,
                inherited,
                Some(IsolationLevel::None),
                Some(IsolationLevel::None),
                true,
                IsolationLevel::None
            ),
            (IsolationLevel::Sandbox, IsolationSource::Settings)
        );
    }

    #[test]
    fn restored_local_integration_keeps_only_the_explicit_wrapper_choice() {
        let settings = LocalSettings {
            shell_integration: true,
            command: Some(vec!["bash".into(), "-i".into()]),
            working_directory: Some(PathBuf::from("private launch directory")),
            ..LocalSettings::default()
        };
        let saved = serde_json::to_string(&settings).unwrap();
        let restored: LocalSettings = serde_json::from_str(&saved).unwrap();
        assert!(restored.shell_integration);
        assert!(restored.command.is_none());
        assert!(restored.working_directory.is_none());
        let legacy: LocalSettings = serde_json::from_str("{}").unwrap();
        assert!(!legacy.shell_integration);
        assert!(
            !serde_json::to_string(&legacy)
                .unwrap()
                .contains("shell_integration")
        );
    }

    #[test]
    fn automatic_integration_does_not_wrap_arbitrary_custom_commands() {
        let settings = LocalSettings {
            shell_integration: true,
            command: Some(vec![
                "python".into(),
                "-c".into(),
                "print('example')".into(),
            ]),
            ..LocalSettings::default()
        };
        assert!(settings.validate().is_err());
        assert!(
            LocalSettings {
                shell_integration: false,
                ..settings
            }
            .validate()
            .is_ok()
        );
    }
}
