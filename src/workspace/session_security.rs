use std::io::{self, Read};
use std::path::Path;

use iced::widget::{button, column, pick_list, row, text, text_input};
use iced::{Element, Task, window};
use kortty_pty::LocalShell;

use crate::profile;
use crate::session::Status;
use crate::session_security::{IsolationLevel, LocalSettings};

use super::{Message as Desktop, PaneSession, Tab, Workspace, ssh_access};

const MAX_SETTINGS: u64 = 8192;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub enum Message {
    Open(window::Id),
    Close(window::Id),
    Isolation(window::Id, IsolationLevel),
    Directory(window::Id, String),
    Save(window::Id),
    IncognitoLocal(window::Id),
    IncognitoSsh(window::Id),
}

#[derive(Default)]
pub(super) struct Security {
    local: LocalSettings,
    problem: Option<String>,
    draft: Option<(window::Id, LocalSettings)>,
}

impl Security {
    pub(super) fn load() -> Self {
        match profile::session_security_path().and_then(|path| Self::read(&path)) {
            Ok(local) => Self {
                local,
                ..Self::default()
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(error) => Self {
                problem: Some(error.to_string()),
                ..Self::default()
            },
        }
    }

    fn read(path: &Path) -> io::Result<LocalSettings> {
        let file = profile::open_preference(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::other(
                "session security settings must be a regular file",
            ));
        }
        let mut bytes = Vec::new();
        file.take(MAX_SETTINGS + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_SETTINGS {
            return Err(io::Error::other(
                "session security settings exceed the size limit",
            ));
        }
        let settings: LocalSettings = serde_json::from_slice(&bytes)?;
        settings.validate()?;
        Ok(settings)
    }

    pub(super) fn editing(&self, window: window::Id) -> bool {
        self.draft
            .as_ref()
            .is_some_and(|(owner, _)| *owner == window)
    }

    pub(super) fn close(&mut self, window: window::Id) {
        if self.editing(window) {
            self.draft = None;
        }
    }

    pub(super) fn local_settings(
        &self,
        stored: Option<LocalSettings>,
        minimum: IsolationLevel,
    ) -> io::Result<LocalSettings> {
        if let Some(error) = &self.problem {
            return Err(io::Error::other(format!(
                "session security settings could not be applied ({error})"
            )));
        }
        let mut settings = stored.unwrap_or_else(|| self.local.clone());
        if settings.directory.is_empty() {
            settings.directory.clone_from(&self.local.directory);
        }
        Ok(settings.with_minimum(minimum))
    }
}

impl Workspace {
    pub(super) fn local_settings_for_shell(&self, shell: LocalShell) -> io::Result<LocalSettings> {
        let mut settings = self
            .security
            .local_settings(None, self.clipboard.policy.minimum_isolation)?;
        settings.shell = shell;
        Ok(settings)
    }

    pub(super) fn isolation_required(&self) -> bool {
        self.clipboard.policy.minimum_isolation != IsolationLevel::None
    }

    pub(super) fn allow_in_process_transport(&mut self, id: window::Id, transport: &str) -> bool {
        if self.clipboard.policy.problem.is_some() {
            self.notice(
                id,
                "Managed policy could not be applied. Sessions are locked.".into(),
            );
            return false;
        }
        let minimum = self.clipboard.policy.minimum_isolation;
        if minimum != IsolationLevel::None {
            self.notice(id, format!("Managed policy requires {minimum}. {transport} cannot provide it in this build."));
            return false;
        }
        true
    }

    pub(super) fn update_session_security(&mut self, message: Message) -> Task<Desktop> {
        match message {
            Message::Open(id) if self.windows.contains_key(&id) => {
                self.windows
                    .get_mut(&id)
                    .expect("open terminal window")
                    .cancel_paste();
                let draft = self
                    .security
                    .local
                    .clone()
                    .with_minimum(self.clipboard.policy.minimum_isolation);
                self.security.draft = Some((id, draft));
            }
            Message::Close(id) => self.security.close(id),
            Message::Isolation(id, level) if self.security.editing(id) => {
                if level >= self.clipboard.policy.minimum_isolation {
                    self.security
                        .draft
                        .as_mut()
                        .expect("editing security settings")
                        .1
                        .isolation = level;
                }
            }
            Message::Directory(id, directory) if self.security.editing(id) => {
                self.security
                    .draft
                    .as_mut()
                    .expect("editing security settings")
                    .1
                    .directory = directory;
            }
            Message::Save(id) if self.security.editing(id) => {
                let draft = &self
                    .security
                    .draft
                    .as_ref()
                    .expect("editing security settings")
                    .1;
                let saved = draft.validate().and_then(|()| {
                    profile::save_atomic(
                        &profile::session_security_path()?,
                        &serde_json::to_vec(draft)?,
                    )
                });
                match saved {
                    Ok(()) => {
                        self.security.local = draft.clone();
                        self.security.problem = None;
                        self.security.close(id);
                        self.notice(id, "Security settings apply to new local sessions.".into());
                    }
                    Err(error) => {
                        self.notice(id, format!("Could not save session security ({error})"))
                    }
                }
            }
            Message::IncognitoLocal(id) => {
                if self.clipboard.policy.incognito_allowed
                    && let Some(window) = self.windows.get(&id)
                {
                    let profile = window.local_profile;
                    self.security.close(id);
                    if let Some(session) = self.start_session(id, profile) {
                        self.add_tab_with_privacy(id, session, true);
                    }
                }
            }
            Message::IncognitoSsh(id) if self.clipboard.policy.incognito_allowed => {
                self.security.close(id);
                return self.update_ssh(ssh_access::Message::ShowIncognito(id));
            }
            _ => {}
        }
        Task::none()
    }

    pub(super) fn session_security_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        let (owner, draft) = self.security.draft.as_ref()?;
        if *owner != id {
            return None;
        }
        let minimum = self.clipboard.policy.minimum_isolation;
        let options = IsolationLevel::ALL
            .into_iter()
            .filter(|level| *level >= minimum)
            .collect::<Vec<_>>();
        let mut panel = column![
            text("Session security").size(19),
            row![
                text("Default local shell isolation"),
                pick_list(options, Some(draft.isolation), move |level| {
                    Desktop::SessionSecurity(Message::Isolation(id, level))
                }),
            ]
            .spacing(8),
        ]
        .spacing(10)
        .padding(16);
        if minimum != IsolationLevel::None {
            panel = panel.push(text(format!("Managed minimum · {minimum}")).size(13));
        }
        if draft.isolation == IsolationLevel::Sandbox {
            panel = panel.push(
                text_input("Absolute sandbox workspace", &draft.directory).on_input(
                    move |directory| Desktop::SessionSecurity(Message::Directory(id, directory)),
                ),
            );
            panel = panel.push(text("The selected directory must be a separate workspace. A shell starts only after the operating system verifies containment.").size(13));
        }
        if let Some(problem) = &self.security.problem {
            panel = panel
                .push(text(format!("Session security settings need repair ({problem})")).size(13));
        }
        if let Some(notice) = self
            .windows
            .get(&id)
            .and_then(|window| window.notice.as_ref())
        {
            panel = panel.push(text(notice).size(13));
        }
        let private =
            self.clipboard.policy.incognito_allowed && self.clipboard.policy.problem.is_none();
        if private {
            panel = panel.push(text(
                "Incognito tabs are excluded from recordings and layouts. The shell or server can still keep its own history.",
            ).size(13));
        }
        Some(
            panel
                .push(
                    row![
                        button("Save").on_press(Desktop::SessionSecurity(Message::Save(id))),
                        button("Cancel").on_press(Desktop::SessionSecurity(Message::Close(id))),
                        button("Incognito shell").on_press_maybe(
                            private
                                .then_some(Desktop::SessionSecurity(Message::IncognitoLocal(id)),)
                        ),
                        button("Incognito SSH…").on_press_maybe(
                            (private
                                && (self.clipboard.policy.minimum_isolation
                                    <= IsolationLevel::Process
                                    || crate::ssh_worker::sandbox_available()))
                            .then_some(Desktop::SessionSecurity(Message::IncognitoSsh(id)),)
                        ),
                    ]
                    .spacing(6),
                )
                .into(),
        )
    }

    pub(super) fn tab_security(&self, tab: &Tab) -> (String, Option<String>) {
        let panes = tab
            .grid
            .iter()
            .filter_map(|(_, id)| self.sessions.get(id))
            .map(|pane| (pane, &pane.frame.status));
        Self::isolation_markers(tab.incognito, panes)
    }

    fn isolation_markers<'a>(
        incognito: bool,
        panes: impl Iterator<Item = (&'a PaneSession, &'a Status)>,
    ) -> (String, Option<String>) {
        let mut process = 0;
        let mut sandbox = 0;
        let mut backend = None;
        let mut failed = 0;
        let mut pending = 0;
        let mut offline = 0;
        let mut unrestricted = 0;
        for (pane, status) in panes {
            match (
                pane.local_security
                    .as_ref()
                    .map(|settings| settings.isolation)
                    .or_else(|| pane.ssh.as_ref().map(|target| target.isolation)),
                status,
            ) {
                (Some(IsolationLevel::Sandbox), Status::Running) => {
                    if let Some(name) = pane.session.sandbox_backend() {
                        sandbox += 1;
                        backend = Some(name);
                    } else {
                        failed += 1;
                    }
                }
                (Some(IsolationLevel::Process), Status::Running) => process += 1,
                // SSH launch failures stay in the login dialog. A process-isolated SSH
                // pane can report a stream error after the worker has already started.
                (Some(IsolationLevel::Process), Status::Error(_)) if pane.ssh.is_none() => {
                    failed += 1;
                }
                (Some(IsolationLevel::Sandbox), Status::Error(_))
                    if pane.session.sandbox_backend().is_none() =>
                {
                    failed += 1
                }
                (Some(IsolationLevel::Process | IsolationLevel::Sandbox), Status::Starting) => {
                    pending += 1;
                }
                (
                    Some(IsolationLevel::Process | IsolationLevel::Sandbox),
                    Status::Disconnected | Status::Exited(_) | Status::Error(_),
                ) => offline += 1,
                _ => unrestricted += 1,
            }
        }
        let mut markers = Vec::new();
        if incognito {
            markers.push("Incognito");
        }
        if failed > 0 {
            markers.push("Isolation unavailable");
        } else if pending > 0 {
            markers.push("Isolation pending");
        } else if offline > 0 {
            markers.push("Isolation offline");
        } else if unrestricted == 0 {
            if process > 0 {
                markers.push("Own process");
            } else if sandbox > 0 {
                markers.push("Sandbox");
            }
        }
        let detail = (sandbox + process + pending + offline + failed > 0).then(|| {
            let backend = backend.map_or_else(String::new, |name| format!(" ({name})"));
            format!(
                "{sandbox} sandboxed{backend} · {process} own process · {pending} starting · {offline} offline · {failed} unavailable · {unrestricted} unisolated",
            )
        });
        (markers.join(" · "), detail)
    }
}

impl PaneSession {
    pub(super) fn isolation_name(&self, status: &Status) -> &'static str {
        if *status == Status::Running {
            if self.session.sandbox_backend().is_some() {
                return "sandboxed";
            }
            if self
                .ssh
                .as_ref()
                .is_some_and(|target| target.isolation == IsolationLevel::Process)
                || self
                    .local_security
                    .as_ref()
                    .is_some_and(|settings| settings.isolation == IsolationLevel::Process)
            {
                return "process";
            }
        }
        "none"
    }
}
