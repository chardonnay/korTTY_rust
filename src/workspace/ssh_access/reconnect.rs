use std::collections::VecDeque;

use iced::{Task, window};

use super::{
    FormKey, PendingSsh, SavedSshProfile, Stage, Workspace, WorkspaceMessage, form_message, wrap,
};
use crate::session::GridSize;
use crate::session_security::IsolationLevel;
use crate::ssh_worker;
use crate::workspace::project_access::WaitReason;
use crate::workspace::reconnect::{ReconnectTarget, SFTP_BUSY};

#[derive(Clone, Copy, Eq, PartialEq)]
enum LaunchMode {
    Manual,
    ProjectAutomatic,
    ProjectReview,
}

pub(super) struct ReconnectLaunch {
    pub(super) target: ReconnectTarget,
    pub(super) form_revision: u64,
    pub(super) saved: Option<Box<SavedSshProfile>>,
    pub(super) project: bool,
}

impl Workspace {
    pub(in crate::workspace) fn start_ssh_reconnect(
        &mut self,
        owner: window::Id,
        target: ReconnectTarget,
    ) -> Task<WorkspaceMessage> {
        self.begin_ssh_reconnect(owner, target, LaunchMode::Manual)
    }

    pub(in crate::workspace) fn start_project_ssh_reconnect(
        &mut self,
        owner: window::Id,
        target: ReconnectTarget,
    ) -> Task<WorkspaceMessage> {
        self.begin_ssh_reconnect(owner, target, LaunchMode::ProjectAutomatic)
    }

    pub(in crate::workspace) fn review_project_ssh_reconnect(
        &mut self,
        owner: window::Id,
        target: ReconnectTarget,
    ) -> Task<WorkspaceMessage> {
        self.begin_ssh_reconnect(owner, target, LaunchMode::ProjectReview)
    }

    fn begin_ssh_reconnect(
        &mut self,
        owner: window::Id,
        target: ReconnectTarget,
        mode: LaunchMode,
    ) -> Task<WorkspaceMessage> {
        let automatic = mode == LaunchMode::ProjectAutomatic;
        let project = mode != LaunchMode::Manual;
        if !self.allow_ssh_transport(owner, "SSH") {
            if automatic {
                self.defer_project_ssh(owner, target, WaitReason::Policy);
            }
            return Task::none();
        }
        if self.reconnect_sftp_busy(target.source) {
            if automatic {
                self.defer_project_ssh(owner, target, WaitReason::ConnectionFailed);
            }
            self.notice(owner, SFTP_BUSY.into());
            return Task::none();
        }
        let Some((tab, pane)) = self.reconnect_target(owner, target) else {
            return Task::none();
        };
        let Some(ssh) = pane.ssh.as_ref() else {
            return Task::none();
        };
        let incognito = tab.incognito;
        let saved_id = ssh.saved_id();
        let pane_profile = pane.frame.screen.profile;
        let mut floor = ssh.isolation.max(self.clipboard.policy.minimum_isolation);
        let saved = if project {
            SavedSshProfile::from_project(ssh)
        } else {
            saved_id
                .map(SavedSshProfile::from_profile)
                .transpose()
                .map(Option::flatten)
        };
        let saved = match saved {
            Ok(Some(saved)) => Some(Box::new(saved)),
            _ if mode == LaunchMode::ProjectReview => None,
            Ok(None) if !project && saved_id.is_none() => None,
            _ => {
                let notice = if project {
                    format!(
                        "{} is offline. No matching saved SSH profile is available for this project pane.",
                        ssh.label()
                    )
                } else {
                    "The saved SSH connection is unavailable.".into()
                };
                if automatic {
                    self.defer_project_ssh(owner, target, WaitReason::Profile);
                }
                self.notice(owner, notice);
                return Task::none();
            }
        };
        let form = if let Some(saved) = &saved {
            if saved.connection.incognito && !incognito {
                self.notice(
                    owner,
                    "This profile now requires an incognito tab. Open it as a new connection."
                        .into(),
                );
                return Task::none();
            }
            let profile = if project {
                Some(pane_profile)
            } else {
                saved.connection.profile()
            };
            let Some(profile) = profile else {
                self.notice(
                    owner,
                    "The saved SSH connection uses an unsupported terminal profile.".into(),
                );
                return Task::none();
            };
            floor = floor.max(saved.connection.isolation);
            saved.form(profile, floor)
        } else {
            ssh.form(pane_profile)
                .with_isolation(floor, floor)
                .with_fixed_route()
        };
        if floor == IsolationLevel::Sandbox && !ssh_worker::sandbox_available() {
            if automatic {
                self.defer_project_ssh(owner, target, WaitReason::Policy);
            }
            self.notice(owner, "SSH sandbox is unavailable on this system.".into());
            return Task::none();
        }

        let ready = saved.is_some() && form.can_connect();
        if automatic
            && (!ready
                || saved.as_ref().is_some_and(|saved| {
                    let connection = &saved.connection;
                    std::iter::once(&connection.login)
                        .chain(connection.jump.as_ref())
                        .any(|login| {
                            login.endpoint.validate().is_err() || login.auth.requires_interactive()
                        })
                }))
        {
            let notice = format!(
                "{} is offline. Sign in to reconnect this project pane.",
                ssh.label()
            );
            self.defer_project_ssh(owner, target, WaitReason::SignIn);
            self.notice(owner, notice);
            return Task::none();
        }
        let saved_id = saved.as_ref().map(|saved| saved.connection.id);
        let revision = self.allocate();
        let key = Some(FormKey::Reconnect(revision));
        let focus = form
            .focus()
            .map(move |message| wrap(form_message(owner, key, message)));
        let state = self
            .windows
            .get_mut(&owner)
            .expect("validated reconnect owner");
        state.cancel_paste();
        state.notice = None;
        state.pending_ssh = Some(PendingSsh {
            replace: None,
            split: None,
            reopen: None,
            reconnect: Some(ReconnectLaunch {
                target,
                form_revision: revision,
                saved,
                project: automatic,
            }),
            sftp: false,
            saved: saved_id,
            incognito,
            retry: None,
            key_snapshots: Default::default(),
            group: VecDeque::new(),
            font: None,
            duplicate_tab: None,
            stage: Stage::Form(Box::new(form)),
        });
        if ready && mode != LaunchMode::ProjectReview {
            let task = self.connect_ssh(owner);
            if automatic
                && self
                    .windows
                    .get(&owner)
                    .and_then(|window| window.pending_ssh.as_ref())
                    .is_some_and(|pending| matches!(pending.stage, Stage::Form(_)))
            {
                self.defer_project_ssh(owner, target, WaitReason::ConnectionFailed);
                self.windows
                    .get_mut(&owner)
                    .expect("reconnect owner")
                    .pending_ssh = None;
            }
            task
        } else {
            focus
        }
    }

    pub(super) fn defer_project_ssh_dial(&mut self, owner: window::Id, reason: WaitReason) -> bool {
        let Some(target) = self.project_ssh_target(owner) else {
            return false;
        };
        self.defer_project_ssh(owner, target, reason);
        self.windows
            .get_mut(&owner)
            .expect("project reconnect owner")
            .pending_ssh = None;
        true
    }

    pub(in crate::workspace) fn project_ssh_target(
        &self,
        owner: window::Id,
    ) -> Option<ReconnectTarget> {
        let launch = self
            .windows
            .get(&owner)?
            .pending_ssh
            .as_ref()?
            .reconnect
            .as_ref()?;
        launch.project.then_some(launch.target)
    }

    pub(in crate::workspace) fn project_ssh_dial_pending(
        &self,
        owner: window::Id,
        target: ReconnectTarget,
    ) -> bool {
        self.project_ssh_target(owner) == Some(target)
    }

    pub(super) fn guarded_ssh_form_key(&mut self, owner: window::Id) -> Option<FormKey> {
        self.pending_ssh_mut(owner)?.form_key()
    }

    pub(super) fn reconnect_ssh_grid(&self, owner: window::Id) -> Option<(GridSize, bool)> {
        let target = self
            .windows
            .get(&owner)?
            .pending_ssh
            .as_ref()?
            .reconnect
            .as_ref()?
            .target;
        let (_, pane) = self.reconnect_target(owner, target)?;
        Some((pane.grid, pane.strict_escapes))
    }

    pub(super) fn validate_ssh_reconnect(&mut self, owner: window::Id) -> bool {
        let Some(reconnect) = self
            .windows
            .get(&owner)
            .and_then(|window| window.pending_ssh.as_ref())
            .and_then(|pending| pending.reconnect.as_ref())
        else {
            return true;
        };
        let failure = if self.reconnect_target(owner, reconnect.target).is_none() {
            Some("The original SSH pane is no longer available for reconnecting.")
        } else if self.reconnect_sftp_busy(reconnect.target.source) {
            Some(SFTP_BUSY)
        } else if reconnect
            .saved
            .as_ref()
            .is_some_and(|saved| !saved.is_current())
        {
            Some("The saved SSH connection changed. Reconnect again to use the current profile.")
        } else {
            None
        };
        if let Some(failure) = failure {
            let target = reconnect.target;
            let reason = if self.clipboard.policy.problem.is_some() {
                WaitReason::Policy
            } else if reconnect
                .saved
                .as_ref()
                .is_some_and(|saved| !saved.is_current())
            {
                WaitReason::Profile
            } else {
                WaitReason::ConnectionFailed
            };
            if reconnect.project {
                self.defer_project_ssh(owner, target, reason);
            }
            if let Some(window) = self.windows.get_mut(&owner) {
                window.pending_ssh = None;
            }
            self.notice(owner, failure.into());
            return false;
        }
        true
    }

    pub(in crate::workspace) fn cancel_ssh_reconnect_for_tab(
        &mut self,
        owner: window::Id,
        tab: u64,
    ) {
        if let Some(window) = self.windows.get_mut(&owner) {
            window.pending_ssh.take_if(|pending| {
                pending
                    .reconnect
                    .as_ref()
                    .is_some_and(|reconnect| reconnect.target.tab == tab)
            });
        }
    }
}
