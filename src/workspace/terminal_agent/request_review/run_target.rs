//! The attached execution target described in a reviewed Run model request.

use std::sync::Weak;

use kortty_ssh::HostKey;

use crate::session::{ShellActivity, StreamLiveness};
use crate::ssh_connection::ExecOpener;
use crate::workspace::SshTarget;

use super::super::{
    LocalDirectory, Mode, PaneSession, Purpose, Run, RunTarget, Shell, Target, Workspace, engine,
    password,
};

const TARGET_CHANGED: &str = "The reviewed agent target changed. Start a new run.";
const SSH_CHANGED: &str = "The reviewed SSH connection changed. Start a new run.";
const DIRECTORY_CHANGED: &str = "The SSH directory changed after review. Start a new run.";
const SUDO_CHANGED: &str = "The reviewed sudo capabilities changed. Start a new run.";

pub(super) struct RunTargetReview {
    target: Attached,
    sudo: Option<SudoCapabilities>,
}

enum Attached {
    Local {
        shell: Shell,
        directory: LocalDirectory,
    },
    Ssh(Box<SshReview>),
}

struct SshReview {
    opener: ExecOpener,
    host_key: Option<HostKey>,
    directory: String,
    route: SshTarget,
    stream: StreamLiveness,
    directory_evidence: ShellActivity,
}

struct SudoCapabilities {
    passwordless: bool,
    password: bool,
    vault_revision: Option<Weak<()>>,
}

impl RunTargetReview {
    pub(super) fn capture(
        workspace: &Workspace,
        run: &Run,
        kind: engine::RunModel,
    ) -> Result<Self, String> {
        if run.mode != Mode::Run || !matches!(run.purpose, Purpose::Agent(_)) {
            return Err("A Run model review requires an attached agent.".into());
        }
        let (owner, pane) = workspace.current_run_pane(run)?;
        let target = match &run.target {
            Some(RunTarget::Attached(Target::Local { shell, directory })) => Attached::Local {
                shell: shell.clone(),
                directory: directory.clone(),
            },
            Some(RunTarget::Attached(Target::Ssh { opener, directory })) => {
                Attached::Ssh(Box::new(SshReview {
                    opener: opener.clone(),
                    host_key: opener.verified_host_key().cloned(),
                    directory: directory.clone(),
                    route: pane.ssh.as_ref().ok_or(SSH_CHANGED)?.clone(),
                    stream: pane.session.stream_liveness().ok_or(SSH_CHANGED)?,
                    directory_evidence: pane.session.frame().shell_activity().clone(),
                }))
            }
            _ => return Err(TARGET_CHANGED.into()),
        };
        let sudo = match kind {
            engine::RunModel::Decision => Some(SudoCapabilities::capture(workspace, run)?),
            engine::RunModel::FinalAdvice => None,
        };
        let review = Self { target, sudo };
        review.require_target(workspace, run, owner, pane)?;
        review.check_worker()?;
        Ok(review)
    }

    pub(super) fn password_sudo(&self) -> bool {
        self.sudo.as_ref().is_some_and(|sudo| sudo.password)
    }

    pub(super) fn require_current(&self, workspace: &Workspace, run: &Run) -> Result<(), String> {
        let (owner, pane) = workspace.current_run_pane(run)?;
        self.require_target(workspace, run, owner, pane)?;
        if let Some(sudo) = &self.sudo
            && (sudo.passwordless != run.passwordless_sudo_target().is_some()
                || sudo.password != workspace.password_sudo_available(run))
        {
            return Err(SUDO_CHANGED.into());
        }
        self.check_worker()
    }

    fn require_target(
        &self,
        workspace: &Workspace,
        run: &Run,
        owner: iced::window::Id,
        pane: &PaneSession,
    ) -> Result<(), String> {
        match (&self.target, &run.target) {
            (
                Attached::Local { shell, directory },
                Some(RunTarget::Attached(Target::Local {
                    shell: current_shell,
                    directory: current_directory,
                })),
            ) if pane.ssh.is_none()
                && pane.local_security.is_some()
                && shell == current_shell
                && directory == current_directory =>
            {
                Ok(())
            }
            (
                Attached::Ssh(reviewed),
                Some(RunTarget::Attached(Target::Ssh { opener, directory })),
            ) => {
                if pane.ssh.as_ref() != Some(&reviewed.route)
                    || directory != &reviewed.directory
                    || !reviewed.matches_opener(opener)
                    || pane
                        .session
                        .exec_opener()
                        .is_none_or(|current| !reviewed.matches_opener(&current))
                {
                    return Err(SSH_CHANGED.into());
                }
                if workspace.agent_remote_path(owner, run.binding.live()?.session)?
                    != reviewed.directory
                {
                    return Err(DIRECTORY_CHANGED.into());
                }
                Ok(())
            }
            _ => Err(TARGET_CHANGED.into()),
        }
    }

    pub(super) fn check_worker(&self) -> Result<(), String> {
        match &self.target {
            Attached::Local { directory, .. } => directory.require_current()?,
            Attached::Ssh(reviewed) => reviewed.check_worker()?,
        }
        if self
            .sudo
            .as_ref()
            .and_then(|sudo| sudo.vault_revision.as_ref())
            .is_some_and(|revision| revision.strong_count() == 0)
        {
            return Err(SUDO_CHANGED.into());
        }
        Ok(())
    }
}

impl SshReview {
    fn matches_opener(&self, opener: &ExecOpener) -> bool {
        self.opener.same_connection(opener)
            && !opener.is_closed()
            && opener.verified_host_key() == self.host_key.as_ref()
    }

    fn check_worker(&self) -> Result<(), String> {
        if !self.matches_opener(&self.opener) {
            return Err(SSH_CHANGED.into());
        }
        let activity = self.stream.shell_activity().ok_or(SSH_CHANGED)?;
        if !same_directory_evidence(&self.directory_evidence, &activity) {
            return Err(DIRECTORY_CHANGED.into());
        }
        Ok(())
    }
}

fn same_directory_evidence(reviewed: &ShellActivity, current: &ShellActivity) -> bool {
    let same_report = match (&reviewed.directory, &current.directory) {
        (Some(reviewed), Some(current)) => {
            reviewed.host == current.host && reviewed.path == current.path
        }
        (None, None) => true,
        _ => false,
    };
    // Repeated OSC 7 reports can name the same location. Clearing an old typed
    // cd marker alone also does not describe a directory change.
    same_report
        && current.typed_cd.as_ref().is_none_or(|current| {
            reviewed
                .typed_cd
                .as_ref()
                .is_some_and(|reviewed| reviewed.sequence == current.sequence)
        })
}

impl SudoCapabilities {
    fn capture(workspace: &Workspace, run: &Run) -> Result<Self, String> {
        let password = workspace.password_sudo_available(run);
        let vault_revision = if password && !password::MANUAL_AVAILABLE {
            Some(
                workspace
                    .vault
                    .as_ref()
                    .and_then(|vault| vault.credential_revision())
                    .ok_or(SUDO_CHANGED)?,
            )
        } else {
            None
        };
        Ok(Self {
            passwordless: run.passwordless_sudo_target().is_some(),
            password,
            vault_revision,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use crate::session::{ReportedDirectory, TypedCd};

    use super::*;

    #[test]
    fn run_model_directory_evidence_rejects_location_changes_and_new_confirmed_cd() {
        let reviewed = ShellActivity {
            directory: Some(ReportedDirectory {
                host: "reviewed.example".into(),
                path: "/srv/reviewed".into(),
                sequence: 1,
            }),
            ..Default::default()
        };
        let mut periodic = reviewed.clone();
        periodic.directory.as_mut().unwrap().sequence = 2;
        assert!(same_directory_evidence(&reviewed, &periodic));

        let mut other_path = periodic.clone();
        other_path.directory.as_mut().unwrap().path = "/srv/changed".into();
        assert!(!same_directory_evidence(&reviewed, &other_path));
        let mut other_host = periodic.clone();
        other_host.directory.as_mut().unwrap().host = "different.example".into();
        assert!(!same_directory_evidence(&reviewed, &other_host));

        let mut confirmed_cd = periodic.clone();
        confirmed_cd.typed_cd = Some(TypedCd {
            path: "/srv/changed".into(),
            sequence: 3,
            prompt_before: 2,
            output_before: 2,
            submitted_at: Instant::now(),
        });
        assert!(!same_directory_evidence(&reviewed, &confirmed_cd));
        assert!(same_directory_evidence(&confirmed_cd, &confirmed_cd));
        assert!(same_directory_evidence(&confirmed_cd, &periodic));
    }
}
