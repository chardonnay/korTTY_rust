//! Inert Java Scheduler settings that require a native sudo adoption.

use kortty_ssh::TrustStore;
use serde::{Deserialize, Serialize};

use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_store::Store;
use crate::job_executor::virtual_terminal::Options as TerminalOptions;

use super::{ConnectionRef, MAX_COMMAND_BYTES, MAX_WORKING_DIRECTORY_BYTES, SnippetLink, sync};

pub(crate) const REVIEW_REQUIRED: &str =
    "Review the imported sudo action and choose a sudo mode before running or enabling this job.";

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Draft {
    pub(crate) routes: Vec<ConnectionRef>,
    pub(crate) settings: Settings,
}

impl Draft {
    pub(super) fn validate<'a>(
        &self,
        selected: impl Iterator<Item = &'a ConnectionRef>,
        has_groups: bool,
    ) -> Result<(), &'static str> {
        if self.routes.is_empty()
            || !self.routes.iter().eq(selected)
            || (self.settings.requires_single_route() && (self.routes.len() != 1 || has_groups))
        {
            return Err("Keep the exact saved SSH targets for the imported sudo action.");
        }
        self.settings.validate()
    }

    pub(super) fn require_current_routes(
        &self,
        store: &Store,
        shared: &super::SharedTargets,
        trust: &TrustStore,
        policy: &ClipboardPolicy,
    ) -> Result<(), &'static str> {
        if policy.problem.is_some()
            || !policy.scheduler_sudo_allowed
            || !policy.minimum_isolation.is_none()
        {
            return Err("Application policy forbids this Scheduler sudo action.");
        }
        let access = policy.server_access();
        for route in &self.routes {
            shared.require_for_adoption(
                store,
                route,
                policy,
                "A saved SSH target changed since the sudo action was imported.",
            )?;
            let reviewed_trust = route.strict_trust(trust.clone())?;
            let pins = sync::pins(route, &reviewed_trust)
                .map_err(|_| "Verify every saved SSH host key before adopting this sudo action.")?;
            if access
                .first_blocked(
                    route
                        .jump
                        .iter()
                        .chain(std::iter::once(&route.target))
                        .map(|login| (login.host.as_str(), login.port)),
                )
                .is_some()
                || pins.iter().any(|(endpoint, _)| {
                    !access.allows(endpoint.wire_host().as_ref(), endpoint.port())
                })
            {
                return Err("Application policy forbids an imported SSH target.");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Settings {
    Command {
        text: String,
        options: ShellOptions,
    },
    Snippet {
        link: SnippetLink,
        options: ShellOptions,
    },
    File {
        action: FileAction,
    },
}

#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShellOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) working_directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) virtual_terminal: Option<TerminalOptions>,
}

impl Settings {
    pub(crate) fn shell_options(&self) -> Option<&ShellOptions> {
        match self {
            Self::Command { options, .. } | Self::Snippet { options, .. } => Some(options),
            Self::File { .. } => None,
        }
    }

    pub(crate) fn requires_single_route(&self) -> bool {
        self.shell_options()
            .is_none_or(|options| options.virtual_terminal.is_some())
    }

    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if let Some(options) = self.shell_options() {
            if options
                .working_directory
                .as_deref()
                .is_some_and(|path| !display_text(path, MAX_WORKING_DIRECTORY_BYTES, false))
            {
                return Err("Invalid imported working directory.");
            }
            if let Some(terminal) = options.virtual_terminal {
                terminal
                    .validate()
                    .map_err(|_| "Invalid imported virtual terminal settings.")?;
            }
        }
        match self {
            Self::Command { text, .. } if !display_text(text, MAX_COMMAND_BYTES, true) => {
                Err("Invalid imported SSH command.")
            }
            Self::Snippet { link, .. } => {
                link.validate()?;
                if link
                    .arguments
                    .iter()
                    .any(|argument| !display_text(argument, 4096, false))
                {
                    return Err("Invalid imported snippet arguments.");
                }
                Ok(())
            }
            Self::File { action } => action.validate(),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum FileAction {
    Mkdir {
        path: String,
    },
    Rename {
        from: String,
        to: String,
    },
    Chmod {
        path: String,
        permissions: String,
    },
    Chown {
        path: String,
        owner: String,
        group: Option<String>,
    },
    Copy {
        from: String,
        to: String,
    },
    Delete {
        path: String,
    },
}

impl FileAction {
    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::Mkdir { .. } => "Create directory",
            Self::Rename { .. } => "Rename or move",
            Self::Chmod { .. } => "Change permissions",
            Self::Chown { .. } => "Change ownership",
            Self::Copy { .. } => "Copy on server",
            Self::Delete { .. } => "Delete recursively",
        }
    }

    pub(super) fn fields(&self) -> Vec<(&'static str, &str)> {
        match self {
            Self::Mkdir { path } | Self::Delete { path } => vec![("Server path", path)],
            Self::Rename { from, to } | Self::Copy { from, to } => {
                vec![("Source path", from), ("Destination path", to)]
            }
            Self::Chmod { path, permissions } => {
                vec![("Server path", path), ("Permissions", permissions)]
            }
            Self::Chown { path, owner, group } => {
                let mut fields = vec![("Server path", path.as_str()), ("Owner", owner.as_str())];
                if let Some(group) = group.as_deref() {
                    fields.push(("Group", group));
                }
                fields
            }
        }
    }

    fn validate(&self) -> Result<(), &'static str> {
        if self
            .fields()
            .iter()
            .any(|(_, value)| !display_text(value, 4096, false))
        {
            Err("Invalid imported privileged file action settings.")
        } else {
            Ok(())
        }
    }
}

fn display_text(value: &str, limit: usize, multiline: bool) -> bool {
    !value.trim().is_empty()
        && value.len() <= limit
        && value.chars().all(|ch| {
            crate::session_journal::is_display_character(ch) && (multiline || !ch.is_control())
        })
}
