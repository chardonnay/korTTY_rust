use std::pin::pin;

use async_channel::Receiver;
use iced::futures::future::select;
use iced::widget::{button, column, text};
use iced::{Element, Task, window};

use crate::serial_dialog;
use crate::session::{InputError, SerialConnection, Session, SessionId, Status};

use super::ReconnectTarget;
use crate::workspace::{Message, PendingIbm, PendingSerial, TerminalProfile, Window, Workspace};

pub(in crate::workspace) struct IbmReconnect {
    pub(in crate::workspace) target: ReconnectTarget,
    fixed_route: bool,
}

impl PendingIbm {
    pub(in crate::workspace) fn can_edit_route(&self) -> bool {
        self.reconnect
            .as_ref()
            .is_none_or(|state| !state.fixed_route)
    }
}

pub(in crate::workspace) struct SerialReconnect {
    target: ReconnectTarget,
    token: u64,
    disconnect: bool,
    released: Option<Receiver<()>>,
}

impl SerialReconnect {
    pub(in crate::workspace) fn new(target: ReconnectTarget, token: u64, disconnect: bool) -> Self {
        Self {
            target,
            token,
            disconnect,
            released: None,
        }
    }
}

impl PendingSerial {
    pub(in crate::workspace) fn accepts_form_input(&self) -> bool {
        self.reconnect
            .as_ref()
            .is_none_or(|state| state.released.is_none())
    }

    pub(in crate::workspace) fn view(&self) -> Element<'_, serial_dialog::Message> {
        match &self.reconnect {
            Some(state) if state.released.is_some() => column![
                text("Waiting for the serial port to close…").size(22),
                text("Canceling leaves the original terminal disconnected."),
                button("Cancel reconnect").on_press(serial_dialog::Message::Cancel),
            ]
            .padding(16)
            .spacing(8)
            .into(),
            Some(state) => self.form.reconnect_view(state.token, state.disconnect),
            None => self.form.view(),
        }
    }
}

#[cfg(test)]
mod tests;

impl Window {
    fn cancel_transport_reconnect(&mut self, matches_target: impl Fn(ReconnectTarget) -> bool) {
        if self.pending_serial.as_ref().is_some_and(|pending| {
            pending
                .reconnect
                .as_ref()
                .is_some_and(|state| matches_target(state.target))
        }) {
            self.pending_serial = None;
        }
        if self
            .pending_ibm
            .as_ref()
            .and_then(|pending| pending.reconnect.as_ref())
            .map(|state| state.target)
            .is_some_and(matches_target)
        {
            self.pending_ibm = None;
        }
    }
}

impl Workspace {
    pub(in crate::workspace) fn show_serial_reconnect(
        &mut self,
        owner: window::Id,
        target: ReconnectTarget,
    ) -> Task<Message> {
        if !self.clipboard.policy.serial_allowed
            || !self.allow_in_process_transport(owner, "Serial connections")
        {
            return Task::none();
        }
        let Some((_, pane)) = self.reconnect_target(owner, target) else {
            return Task::none();
        };
        let frame = pane.session.frame();
        let Some(connection) = frame.serial.as_deref() else {
            return Task::none();
        };
        if frame.status == Status::Starting {
            return Task::none();
        }
        self.show_serial_form(
            owner,
            connection.clone(),
            frame.screen.profile,
            None,
            Some((target, frame.status == Status::Running)),
        )
    }

    pub(in crate::workspace) fn begin_serial_reconnect(
        &mut self,
        owner: window::Id,
        token: u64,
    ) -> Task<Message> {
        if !self.clipboard.policy.serial_allowed
            || !self.allow_in_process_transport(owner, "Serial connections")
        {
            return Task::none();
        }
        let Some(pending) = self
            .windows
            .get(&owner)
            .and_then(|window| window.pending_serial.as_ref())
        else {
            return Task::none();
        };
        let Some(state) = pending
            .reconnect
            .as_ref()
            .filter(|state| state.token == token && state.released.is_none())
        else {
            return Task::none();
        };
        let Ok(parameters) = pending.form.validated() else {
            return Task::none();
        };
        let target = state.target;
        let Some((_, source)) = self.reconnect_target(owner, target) else {
            return Task::none();
        };
        if source.session.frame().status != Status::Running {
            return self.start_serial_reconnect_candidate(owner, target, parameters);
        }
        if !state.disconnect {
            return Task::none();
        }
        let released = match source.session.disconnect_serial() {
            Ok(released) => released,
            Err(InputError::Busy) => {
                self.notice(
                    owner,
                    "The serial worker is busy. Try reconnecting again.".into(),
                );
                return Task::none();
            }
            Err(_) => {
                self.notice(owner, "The serial worker is no longer available.".into());
                return Task::none();
            }
        };
        let stopped = source.session.shutdown_handle();
        if let Some(window) = self.windows.get_mut(&owner)
            && let Some(state) = window
                .pending_serial
                .as_mut()
                .and_then(|pending| pending.reconnect.as_mut())
        {
            state.released = Some(released.clone());
            window.notice = None;
        }
        Task::perform(
            async move {
                let _ = select(pin!(released.recv()), pin!(stopped.recv())).await;
            },
            move |()| Message::SerialReconnectReleased(owner, token),
        )
    }

    pub(in crate::workspace) fn resume_serial_reconnect(
        &mut self,
        owner: window::Id,
        token: u64,
    ) -> Task<Message> {
        let Some(pending) = self
            .windows
            .get(&owner)
            .and_then(|window| window.pending_serial.as_ref())
        else {
            return Task::none();
        };
        let Some(state) = pending
            .reconnect
            .as_ref()
            .filter(|state| state.token == token)
        else {
            return Task::none();
        };
        let Some(released) = state.released.as_ref() else {
            return Task::none();
        };
        let target = state.target;
        if !released.is_closed()
            && self
                .reconnect_target(owner, target)
                .is_some_and(|(_, source)| !source.session.shutdown_handle().is_closed())
        {
            return Task::none();
        }
        let Ok(parameters) = pending.form.validated() else {
            return Task::none();
        };
        self.start_serial_reconnect_candidate(owner, target, parameters)
    }

    fn start_serial_reconnect_candidate(
        &mut self,
        owner: window::Id,
        target: ReconnectTarget,
        (connection, profile): (SerialConnection, TerminalProfile),
    ) -> Task<Message> {
        if let Some(window) = self.windows.get_mut(&owner) {
            window.pending_serial = None;
        }
        let Some((_, source)) = self.reconnect_target(owner, target) else {
            return Task::none();
        };
        if matches!(
            source.session.frame().status,
            Status::Starting | Status::Running
        ) {
            return Task::none();
        }
        let (grid, strict) = (source.grid, source.strict_escapes);
        match Session::start_serial_with_strict(
            connection,
            grid,
            profile,
            self.window_settings.prompt_navigation_enabled,
            self.window_settings.terminal_appearance(),
            strict,
        ) {
            Ok(session) => {
                let candidate =
                    self.store_connected_session(session, grid, Some(target.source), None);
                if let Some(pane) = self.sessions.get_mut(&candidate) {
                    pane.strict_escapes = strict;
                }
                self.start_reconnect_worker(owner, target, candidate, None, Default::default())
            }
            Err(error) => {
                self.notice(
                    owner,
                    format!("Could not restart the serial worker ({error})"),
                );
                Task::none()
            }
        }
    }

    pub(in crate::workspace) fn show_ibm_reconnect(
        &mut self,
        owner: window::Id,
        target: ReconnectTarget,
    ) -> Task<Message> {
        if !self.allow_in_process_transport(owner, "IBM") {
            return Task::none();
        }
        let Some((_, pane)) = self.reconnect_target(owner, target) else {
            return Task::none();
        };
        let frame = pane.session.frame();
        let Some(ibm) = frame.ibm.as_ref() else {
            return Task::none();
        };
        let address = pane.ibm_origin.as_ref().and_then(|origin| origin.address);
        let dialog = PendingIbm {
            kind: ibm.kind,
            replace: None,
            reconnect: Some(IbmReconnect {
                target,
                fixed_route: address.is_some(),
            }),
            font: None,
            address: address.map_or_else(String::new, |address| address.to_string()),
            device_name: pane
                .ibm_origin
                .as_ref()
                .map_or_else(String::new, |origin| origin.device_name.clone()),
            plaintext: false,
        };
        if let Some(window) = self.windows.get_mut(&owner) {
            window.cancel_paste();
            window.pending_ibm = Some(dialog);
            window.notice = None;
        }
        Task::none()
    }

    pub(in crate::workspace) fn cancel_transport_reconnect_for_tab(
        &mut self,
        owner: window::Id,
        tab: u64,
    ) {
        if let Some(window) = self.windows.get_mut(&owner) {
            window.cancel_transport_reconnect(|target| target.tab == tab);
        }
    }

    pub(in crate::workspace) fn cancel_transport_reconnect_for_source(
        &mut self,
        source: SessionId,
    ) {
        for window in self.windows.values_mut() {
            window.cancel_transport_reconnect(|target| target.source == source);
        }
    }
}
