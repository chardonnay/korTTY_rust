use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};

use iced::widget::{
    Id, button, checkbox, column, container, pick_list, row, rule, scrollable, space, text,
    text_input,
};
use iced::{Alignment, Element, Length, Task};

use crate::connection_store::{TunnelConfig, TunnelRule, TunnelTarget};
use crate::ssh_dialog::normalize_host;
use crate::workspace::dialog_design::{self, keyboard_table, number_input};

#[derive(Clone, Debug)]
pub(crate) enum Event {
    Focus,
    Traverse(bool),
    Change(Edit),
    Save,
    Cancel,
}

#[derive(Clone, Debug)]
pub(crate) enum Edit {
    Enabled(bool),
    Mode(Mode),
    LocalHost(String),
    LocalPort(String),
    RemoteHost(String),
    RemotePort(String),
    Description(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    #[default]
    Local,
    Remote,
    Dynamic,
}

impl fmt::Display for Mode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Local => "Local (-L)",
            Self::Remote => "Remote (-R)",
            Self::Dynamic => "Dynamic (-D)",
        })
    }
}

struct Endpoint {
    host: String,
    port: String,
}

impl Endpoint {
    fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port: port.to_string(),
        }
    }

    fn host(&self) -> &str {
        let host = self.host.trim();
        if host.is_empty() { "localhost" } else { host }
    }

    fn port(&self, error: &'static str) -> Result<u16, &'static str> {
        self.port
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|&port| port != 0)
            .ok_or(error)
    }

    fn bind(&self) -> Result<SocketAddr, &'static str> {
        let host = self.host();
        let address = if host.eq_ignore_ascii_case("localhost") {
            Ipv4Addr::LOCALHOST.into()
        } else {
            host.parse()
                .map_err(|_| "Use localhost or a numeric bind address.")?
        };
        Ok(SocketAddr::new(
            address,
            self.port("Enter a listen port from 1 to 65535.")?,
        ))
    }

    fn service(&self) -> Result<(String, u16), &'static str> {
        let host =
            normalize_host(self.host()).map_err(|_| "Enter a service hostname or IP address.")?;
        Ok((host, self.port("Enter a service port from 1 to 65535.")?))
    }
}

pub(crate) struct Editor {
    pub(super) source: Id,
    pub(super) original: Option<(usize, TunnelConfig)>,
    focus: Id,
    mode_picker: Id,
    enabled: bool,
    mode: Mode,
    local: Endpoint,
    remote: Endpoint,
    description: String,
}

impl Editor {
    pub(super) fn new(source: Id, original: Option<(usize, TunnelConfig)>) -> Self {
        let mut editor = Self {
            source,
            original,
            focus: Id::unique(),
            mode_picker: Id::unique(),
            enabled: true,
            mode: Mode::Local,
            local: Endpoint::new("localhost", 8080),
            remote: Endpoint::new("localhost", 80),
            description: String::new(),
        };
        if let Some((_, tunnel)) = &editor.original {
            editor.enabled = tunnel.enabled;
            editor.description = tunnel.description.clone();
            let address = Endpoint::new(tunnel.rule.bind.ip().to_string(), tunnel.rule.bind.port());
            match &tunnel.rule.target {
                TunnelTarget::Local { host, port } => {
                    editor.local = address;
                    editor.remote = Endpoint::new(host.clone(), *port);
                }
                TunnelTarget::Remote { host, port } => {
                    editor.mode = Mode::Remote;
                    editor.remote = address;
                    editor.local = Endpoint::new(host.clone(), *port);
                }
                TunnelTarget::Dynamic => {
                    editor.mode = Mode::Dynamic;
                    editor.local = address;
                }
            }
        }
        editor
    }

    pub(crate) fn title(&self) -> &'static str {
        if self.original.is_some() {
            "Edit SSH Tunnel"
        } else {
            "Add SSH Tunnel"
        }
    }

    pub(crate) fn update(&mut self, edit: Edit) {
        match edit {
            Edit::Enabled(value) => self.enabled = value,
            Edit::Mode(value) => self.mode = value,
            Edit::LocalHost(value) if value.len() <= 256 => self.local.host = value,
            Edit::LocalPort(value) if valid_port_input(&value) => self.local.port = value,
            Edit::RemoteHost(value) if self.mode != Mode::Dynamic && value.len() <= 256 => {
                self.remote.host = value;
            }
            Edit::RemotePort(value) if self.mode != Mode::Dynamic && valid_port_input(&value) => {
                self.remote.port = value;
            }
            Edit::Description(value) if value.len() <= 160 => self.description = value,
            _ => {}
        }
    }

    pub(crate) fn config(&self) -> Result<TunnelConfig, &'static str> {
        let (bind, target) = match self.mode {
            Mode::Local => {
                let (host, port) = self.remote.service()?;
                (self.local.bind()?, TunnelTarget::Local { host, port })
            }
            Mode::Remote => {
                let (host, port) = self.local.service()?;
                (self.remote.bind()?, TunnelTarget::Remote { host, port })
            }
            Mode::Dynamic => (self.local.bind()?, TunnelTarget::Dynamic),
        };
        let tunnel = TunnelConfig {
            enabled: self.enabled,
            description: self.description.trim().into(),
            rule: TunnelRule { bind, target },
        };
        tunnel
            .validate()
            .map_err(|_| "Enter a valid tunnel description and endpoints.")?;
        Ok(tunnel)
    }

    fn exposure_warning(&self) -> Option<&'static str> {
        let (endpoint, warning) = match self.mode {
            Mode::Local | Mode::Dynamic => {
                (&self.local, "Other devices may reach this local listener.")
            }
            Mode::Remote => (
                &self.remote,
                "Other devices may reach this listener if the SSH server's GatewayPorts setting permits it.",
            ),
        };
        let host = endpoint.host();
        if host.eq_ignore_ascii_case("localhost") {
            return None;
        }
        host.parse::<std::net::IpAddr>()
            .ok()
            .filter(|ip| !ip.is_loopback())
            .map(|_| warning)
    }

    pub(crate) fn traverse(&self, backwards: bool) -> Task<Event> {
        use iced::advanced::widget::operation::{focusable, scope};
        use iced_runtime::{Action, task};

        let root = self.focus.clone();
        let operation = if backwards {
            Action::widget(scope(root, focusable::focus_previous()))
        } else {
            Action::widget(scope(root, focusable::focus_next()))
        };
        task::effect(operation)
    }

    pub(crate) fn view(&self) -> Element<'_, Event> {
        let remote_first = self.mode == Mode::Remote;
        let dynamic = self.mode == Mode::Dynamic;
        let local = self.endpoint_fields(
            if remote_first {
                "Local host"
            } else {
                "Local bind address"
            },
            "Local port",
            &self.local,
            Some(|value| Event::Change(Edit::LocalHost(value))),
            Some(|value| Event::Change(Edit::LocalPort(value))),
        );
        let remote = self.endpoint_fields(
            if remote_first {
                "Remote bind address"
            } else {
                "Remote host"
            },
            "Remote port",
            &self.remote,
            (!dynamic).then_some(|value| Event::Change(Edit::RemoteHost(value))),
            (!dynamic).then_some(|value| Event::Change(Edit::RemotePort(value))),
        );
        let endpoints = if remote_first {
            column![remote, local]
        } else {
            column![local, remote]
        };
        let switch = checkbox(self.enabled)
            .label("Enable tunnel")
            .text_size(13)
            .on_toggle(|value| Event::Change(Edit::Enabled(value)));
        let validation = self.config();
        let save = validation.is_ok().then_some(Event::Save);
        let fields = column![
            keyboard_table::action(
                switch.into(),
                Some(Event::Change(Edit::Enabled(!self.enabled))),
                false
            ),
            field(
                "Type",
                pick_list(
                    [Mode::Local, Mode::Remote, Mode::Dynamic],
                    Some(self.mode),
                    |mode| Event::Change(Edit::Mode(mode)),
                )
                .width(200)
                .keyboard_navigation(self.mode_picker.clone())
            ),
            endpoints.spacing(10),
            self.exposure_warning()
                .map(|warning| text(warning).size(13)),
            field(
                "Description",
                text_input("Optional description", &self.description)
                    .width(200)
                    .style(dialog_design::form_input)
                    .on_input(|value| Event::Change(Edit::Description(value)))
            ),
            validation.err().map(|error| text(error).size(13)),
            text(super::HELP).size(12).style(text::secondary),
        ]
        .spacing(10);
        let header = container(
            text(if self.original.is_some() {
                "Edit SSH tunnel settings"
            } else {
                "Configure a new SSH tunnel"
            })
            .size(16),
        )
        .padding([12, 20])
        .width(Length::Fill)
        .style(dialog_design::header);
        let footer = row![
            space::horizontal(),
            keyboard_table::action(
                button("Save").on_press_maybe(save.clone()).into(),
                save,
                true
            ),
            keyboard_table::action(
                button("Cancel")
                    .style(dialog_design::secondary_button)
                    .on_press(Event::Cancel)
                    .into(),
                Some(Event::Cancel),
                true
            ),
        ]
        .spacing(8)
        .padding([10, 16]);
        let body = column![
            header,
            scrollable(container(fields).padding(20).width(Length::Fill)).height(Length::Fill),
            rule::horizontal(1),
            footer,
        ]
        .height(Length::Fill);
        container(body).id(self.focus.clone()).into()
    }

    fn endpoint_fields<'a>(
        &self,
        host_label: &'static str,
        port_label: &'static str,
        endpoint: &'a Endpoint,
        host_edit: Option<fn(String) -> Event>,
        port_edit: Option<fn(String) -> Event>,
    ) -> Element<'a, Event> {
        column![
            field(
                host_label,
                text_input("localhost", &endpoint.host)
                    .width(200)
                    .style(dialog_design::form_input)
                    .on_input_maybe(host_edit)
            ),
            field(
                port_label,
                number_input(
                    text_input("", &endpoint.port).on_input_maybe(port_edit),
                    &endpoint.port,
                    1..=65535,
                    1,
                    port_edit,
                    174.0,
                )
            ),
        ]
        .spacing(10)
        .into()
    }
}

fn valid_port_input(value: &str) -> bool {
    value.len() <= 5 && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn field<'a>(label: &'static str, input: impl Into<Element<'a, Event>>) -> Element<'a, Event> {
    row![text(label).size(13).width(116), input.into()]
        .align_y(Alignment::Center)
        .spacing(10)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_keep_both_endpoint_pairs_and_validate_the_listener() {
        let mut editor = Editor::new(Id::unique(), None);
        let local = editor.config().unwrap();
        assert_eq!(local.rule.bind.to_string(), "127.0.0.1:8080");
        assert!(matches!(
            local.rule.target,
            TunnelTarget::Local { port: 80, .. }
        ));
        editor.update(Edit::RemoteHost("192.0.2.8".into()));
        editor.update(Edit::Mode(Mode::Remote));
        let remote = editor.config().unwrap();
        assert_eq!(remote.rule.bind.to_string(), "192.0.2.8:80");
        assert!(matches!(
            remote.rule.target,
            TunnelTarget::Remote { ref host, port: 8080 } if host == "localhost"
        ));
        assert!(editor.exposure_warning().unwrap().contains("GatewayPorts"));
        editor.update(Edit::RemotePort("0".into()));
        assert!(editor.config().is_err());
        editor.update(Edit::Mode(Mode::Dynamic));
        editor.update(Edit::RemotePort("2222".into()));
        assert!(matches!(
            editor.config().unwrap().rule.target,
            TunnelTarget::Dynamic
        ));
        editor.update(Edit::Mode(Mode::Remote));
        assert!(editor.config().is_err());
        editor.update(Edit::RemotePort("65535".into()));
        assert_eq!(editor.config().unwrap().rule.bind.port(), 65535);
    }
}
