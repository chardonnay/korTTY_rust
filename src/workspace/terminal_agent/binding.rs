//! Stable identities for live pane streams and explicitly reviewed saved routes.

use std::sync::Arc;

use crate::session::SessionId;
use crate::terminal_agent::swarm::headless::{Execution, Review};
use crate::terminal_agent::{Cancel, Outcome, PasswordlessSudo, Target};

use super::swarm::headless::Source;

#[derive(Clone, Copy)]
pub(super) struct Live {
    pub(super) session: SessionId,
    pub(super) stream: u64,
    pub(super) private: bool,
}

pub(super) enum Binding {
    Live(Live),
    Saved(Box<Source>),
}

impl Binding {
    pub(super) fn live(&self) -> Result<Live, &'static str> {
        match self {
            Self::Live(live) => Ok(*live),
            Self::Saved(_) => Err("The operation requires its original terminal pane."),
        }
    }

    pub(super) fn private(&self) -> bool {
        self.live().is_ok_and(|live| live.private)
    }

    pub(super) fn session(&self) -> Option<SessionId> {
        self.live().ok().map(|live| live.session)
    }

    pub(super) fn saved(&self) -> Option<&Source> {
        match self {
            Self::Live(_) => None,
            Self::Saved(source) => Some(source),
        }
    }
}

#[derive(Clone)]
pub(super) enum RunTarget {
    Attached(Target),
    Saved(Arc<Review>),
}

impl RunTarget {
    pub(super) fn probe_command(&self) -> &'static str {
        match self {
            Self::Attached(target) => target.probe_command(),
            Self::Saved(_) => crate::terminal_agent::POSIX_PROBE,
        }
    }

    pub(super) fn directory(&self) -> &str {
        match self {
            Self::Attached(target) => target.directory(),
            Self::Saved(review) => review.directory(),
        }
    }

    pub(super) fn shell(&self) -> &str {
        match self {
            Self::Attached(target) => target.shell(),
            Self::Saved(_) => "Remote /bin/sh",
        }
    }

    pub(super) fn supports_script(
        &self,
        script: &crate::terminal_agent::swarm::script::Prepared,
    ) -> bool {
        match self {
            Self::Attached(target) => script.supports_target(target),
            Self::Saved(_) => script.supports_ssh(),
        }
    }
}

pub(super) enum ApprovedExecution {
    Attached(Target),
    Saved(Box<Execution>),
}

pub(super) enum CommandToRun {
    Plain(String),
    PasswordlessSudo(PasswordlessSudo),
}

impl CommandToRun {
    pub(super) fn text(&self) -> &str {
        match self {
            Self::Plain(command) => command,
            Self::PasswordlessSudo(sudo) => sudo.command(),
        }
    }
}

impl ApprovedExecution {
    pub(super) fn run(
        self,
        command: CommandToRun,
        cancel: &Cancel,
        before_dispatch: impl Fn() -> Result<(), String>,
    ) -> Result<Outcome, String> {
        match (self, command) {
            (Self::Attached(target), CommandToRun::Plain(command)) => {
                before_dispatch()?;
                target.execute(&command, cancel)
            }
            (Self::Attached(target), CommandToRun::PasswordlessSudo(reviewed)) => {
                before_dispatch()?;
                target.execute_passwordless_sudo(reviewed, cancel)
            }
            (Self::Saved(execution), CommandToRun::Plain(command)) => {
                execution.run_guarded(&command, cancel, before_dispatch)
            }
            (Self::Saved(_), CommandToRun::PasswordlessSudo(_)) => {
                Err("Passwordless sudo requires an attached SSH agent run.".into())
            }
        }
    }
}
