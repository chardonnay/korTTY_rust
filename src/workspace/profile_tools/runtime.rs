use super::super::{PaneSession, Workspace};
use super::RuntimePreferences;
use crate::session_security::IsolationLevel;

impl Workspace {
    pub(in crate::workspace) fn native_runtime<'a>(
        &'a self,
        pane: &'a PaneSession,
    ) -> Option<&'a RuntimePreferences> {
        pane.saved_local
            .as_ref()
            .and_then(|profile| self.saved_terminal_preferences.local(profile))
            .or_else(|| {
                pane.saved_serial
                    .as_ref()
                    .and_then(|profile| self.saved_terminal_preferences.serial(profile))
            })
            .or(pane.saved_runtime.as_deref())
    }

    pub(in crate::workspace) fn validate_native_privacy(
        &self,
        runtime: &RuntimePreferences,
    ) -> std::io::Result<()> {
        if runtime.settings.incognito && !self.clipboard.policy.incognito_allowed {
            return Err(std::io::Error::other(
                "Managed policy blocks this incognito connection.",
            ));
        }
        Ok(())
    }

    pub(in crate::workspace) fn serial_runtime_isolation(
        &self,
        runtime: &RuntimePreferences,
    ) -> IsolationLevel {
        let requested = if runtime.settings.inherit_isolation {
            runtime
                .inherited_isolation
                .or(self.window_settings.default_session_isolation)
                .unwrap_or_default()
        } else {
            runtime.settings.isolation
        };
        requested.max(self.clipboard.policy.minimum_isolation)
    }
}

impl Workspace {
    pub(in crate::workspace) fn native_logging(
        &self,
        runtime: Option<&RuntimePreferences>,
        incognito: bool,
        name: &str,
        host: &str,
    ) -> Option<Box<crate::terminal_logging::Prepared>> {
        let config = &runtime?.settings.logging;
        if incognito || !config.enabled() {
            return None;
        }
        Some(Box::new(crate::terminal_logging::Prepared::new(
            config.clone(),
            self.clipboard.policy.terminal_logging.clone(),
            name.to_owned(),
            host.to_owned(),
            std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .unwrap_or_default(),
        )))
    }
}

impl Workspace {
    fn native_connection_window(
        &mut self,
        runtime: &RuntimePreferences,
    ) -> (iced::window::Id, iced::Task<super::Message>) {
        if let Some(geometry) = runtime.settings.window_geometry {
            let (id, opened) = self.open_window_at_placement(geometry.placement());
            self.windows
                .get_mut(&id)
                .expect("new connection window")
                .geometry
                .mark_connection_specific();
            let opened = if geometry.maximized {
                opened.chain(iced::window::maximize(id, true))
            } else {
                opened
            };
            (id, opened)
        } else {
            self.open_window(false)
        }
    }

    pub(super) fn open_saved_local_in_new_window(
        &mut self,
        owner: iced::window::Id,
        profile: crate::connection_store::LocalProfile,
    ) -> iced::Task<super::Message> {
        if self.exiting || self.scheduler_draining {
            return iced::Task::none();
        }
        let runtime = self
            .saved_terminal_preferences
            .resolve_local(&profile, None)
            .and_then(|runtime| self.validate_native_privacy(&runtime).map(|()| runtime));
        let runtime = match runtime {
            Ok(runtime) => runtime,
            Err(error) => {
                self.notice(owner, format!("Could not start the local shell ({error})"));
                return iced::Task::none();
            }
        };
        let (id, opened) = self.native_connection_window(&runtime);
        let launch = self.open_saved_local(id, profile);
        opened.chain(launch)
    }

    pub(super) fn open_saved_serial_in_new_window(
        &mut self,
        owner: iced::window::Id,
        profile: crate::connection_store::SerialProfile,
    ) -> iced::Task<super::Message> {
        if self.exiting
            || self.scheduler_draining
            || !self.clipboard.policy.serial_allowed
            || !self.allow_in_process_transport(owner, "Serial connections")
        {
            return iced::Task::none();
        }
        let runtime = self
            .saved_terminal_preferences
            .resolve_serial(&profile)
            .and_then(|runtime| self.validate_native_privacy(&runtime).map(|()| runtime));
        let runtime = match runtime {
            Ok(runtime) => runtime,
            Err(error) => {
                self.notice(
                    owner,
                    format!("Could not open the saved serial connection ({error})."),
                );
                return iced::Task::none();
            }
        };
        let isolation = self.serial_runtime_isolation(&runtime);
        if isolation != IsolationLevel::None {
            self.notice(owner, format!("This connection requires {isolation}. Serial connections cannot provide it in this build."));
            return iced::Task::none();
        }
        if let Err(error) = profile.connection.validate() {
            self.notice(
                owner,
                format!("Could not open the saved serial connection ({error})."),
            );
            return iced::Task::none();
        }
        let (id, opened) = self.native_connection_window(&runtime);
        let launch = self.open_saved_serial(id, profile);
        opened.chain(launch)
    }
}
