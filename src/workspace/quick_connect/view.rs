use std::fmt;

use iced::widget::{
    button, checkbox, column, container, pick_list, row, rule, scrollable, space, text, text_input,
};
use iced::{Alignment, Color, Element, Length, Theme, window};
#[cfg(windows)]
use kortty_pty::LocalShell;

use super::{
    Connection, Desktop, Dialog, IbmKind, Message, Protocol, Store, TerminalProfile, Workspace,
    profile, wrap,
};
use crate::ssh_dialog::{self, LoginMessage};
use crate::vault_window::Kind;

const RECENT_COUNT: usize = 10;
const LABEL_WIDTH: f32 = 200.0;

#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct ShellChoice(LocalShell);

#[cfg(windows)]
impl fmt::Display for ShellChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.0 {
            LocalShell::Default => "System default",
            LocalShell::PowerShell => "PowerShell",
            LocalShell::CommandPrompt => "cmd.exe (Command Prompt)",
        })
    }
}

impl Workspace {
    pub(in crate::workspace) fn quick_connect_view(
        &self,
        id: window::Id,
    ) -> Option<Element<'_, Desktop>> {
        let dialog = self
            .quick_connect
            .as_ref()
            .filter(|dialog| dialog.id == id)?;
        Some(
            self.quick_view(dialog)
                .map(move |message| wrap(id, message)),
        )
    }

    fn quick_view<'a>(&'a self, dialog: &'a Dialog) -> Element<'a, Message> {
        let recent: Vec<_> = dialog
            .store
            .iter()
            .flat_map(Store::recent)
            .take(RECENT_COUNT)
            .collect();
        let mut content = column![].spacing(10);
        if !recent.is_empty() {
            let choices = row(recent.into_iter().map(|connection| {
                button(text(&connection.name).wrapping(text::Wrapping::None))
                    .width(140)
                    .style(neutral_button)
                    .on_press_maybe((!dialog.saving).then_some(Message::Recent(connection.id)))
                    .into()
            }))
            .spacing(8)
            .wrap();
            let buttons = column![text("Recently used connections").size(15), choices].spacing(6);
            content = content.push(buttons).push(rule::horizontal(1));
        }
        let tabs = row![
            tab(
                "Individual Connection",
                !dialog.group_tab,
                Message::GroupTab(false)
            ),
            tab("Open Group", dialog.group_tab, Message::GroupTab(true)),
        ]
        .spacing(4);
        let split = dialog.split.is_some();
        let body = if dialog.group_tab {
            dialog.group_view()
        } else {
            self.quick_individual_view(dialog)
        };
        let enabled = dialog.can_connect()
            && self.clipboard.policy.problem.is_none()
            && (dialog.group_tab || self.quick_protocols(split).contains(&dialog.protocol));
        let actions = row![
            (!split).then(|| button("Load Project")
                .style(neutral_button)
                .on_press_maybe((!dialog.saving).then_some(Message::LoadProject))),
            space::horizontal(),
            button(if dialog.group_tab {
                "Open Group"
            } else {
                "Connect"
            })
            .on_press_maybe(enabled.then_some(Message::Connect)),
            button("Cancel")
                .style(neutral_button)
                .on_press_maybe((!dialog.saving).then_some(Message::Cancel)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let status = if dialog.saving {
            Some("Saving connection…")
        } else {
            dialog.status.as_deref()
        };
        if !split {
            content = content.push(tabs).push(rule::horizontal(1));
        }
        content = content.push(body);
        if let Some(status) = status {
            content = content.push(text(status).size(13));
        }
        content
            .push(actions)
            .padding(15)
            .height(Length::Fill)
            .into()
    }

    fn quick_individual_view<'a>(&'a self, dialog: &'a Dialog) -> Element<'a, Message> {
        let mut body = column![dialog.saved_view()].spacing(9);
        if dialog.protocol.is_ssh() {
            body = body.push(dialog.ssh.identity_view(LABEL_WIDTH).map(Message::Ssh));
        }
        body = body.push(labeled(
            "Protocol",
            pick_list(
                self.quick_protocols(dialog.split.is_some()),
                Some(dialog.protocol),
                Message::Protocol,
            ),
        ));
        if dialog.protocol.ibm_models().is_none() && dialog.protocol != Protocol::Sftp {
            body = body.push(labeled(
                "Terminal emulation",
                pick_list(
                    TerminalProfile::ALL,
                    Some(dialog.ssh.profile()),
                    Message::Profile,
                ),
            ));
        }
        match dialog.protocol {
            Protocol::Ssh | Protocol::Sftp => {
                body = body.push(
                    dialog
                        .ssh
                        .authentication_view(LABEL_WIDTH, &self.ssh_key_choices)
                        .map(Message::Ssh),
                );
                let credentials = dialog
                    .ssh
                    .target_endpoint()
                    .map_or_else(Vec::new, |endpoint| {
                        self.scoped_credential_choices(Kind::Password, &endpoint)
                    });
                let selected = credentials
                    .iter()
                    .find(|choice| Some(choice.id.as_str()) == dialog.ssh.vault_password())
                    .cloned();
                body = body.push(labeled(
                    "Saved credentials",
                    row![
                        pick_list(credentials, selected, |choice| {
                            Message::Ssh(ssh_dialog::Message::Target(LoginMessage::VaultPassword(
                                choice.id,
                            )))
                        })
                        .placeholder("Select a vault credential")
                        .width(Length::Fill),
                        button("Vault…")
                            .style(neutral_button)
                            .on_press(Message::Ssh(ssh_dialog::Message::OpenVault)),
                    ]
                    .spacing(7),
                ));
                if dialog.protocol == Protocol::Ssh {
                    body = body.push(labeled(
                        "",
                        checkbox(dialog.save)
                            .label("Save connection")
                            .on_toggle(Message::SaveConnection),
                    ));
                    if dialog.save {
                        body = body.push(labeled(
                            "Connection name",
                            text_input("Defaults to username@host", &dialog.name)
                                .on_input(Message::ConnectionName),
                        ));
                    }
                }
                body = body.push(
                    button(if dialog.options {
                        "▾ Connection settings"
                    } else {
                        "▸ Connection settings"
                    })
                    .style(neutral_button)
                    .on_press(Message::ShowOptions)
                    .width(Length::Fill),
                );
                if dialog.options {
                    body = body
                        .push(
                            dialog
                                .ssh
                                .options_view(LABEL_WIDTH, &self.ssh_key_choices)
                                .map(Message::Ssh),
                        )
                        .push(text("Host-key verification is required.").size(13));
                    if dialog.protocol == Protocol::Ssh {
                        body = body.push(
                            dialog
                                .ssh
                                .logging_view(
                                    self.clipboard.policy.problem.is_none()
                                        && !dialog
                                            .selected()
                                            .is_some_and(|connection| connection.incognito),
                                )
                                .map(Message::Ssh),
                        );
                    }
                }
                if let Some(notice) = dialog.ssh.validation_notice() {
                    body = body.push(text(notice).size(13));
                }
            }
            Protocol::Local => {
                let integration = self.quick_shell_integration(dialog.ssh.profile());
                #[cfg(windows)]
                let shell = pick_list(
                    [
                        LocalShell::Default,
                        LocalShell::PowerShell,
                        LocalShell::CommandPrompt,
                    ]
                    .map(ShellChoice),
                    Some(ShellChoice(dialog.local_shell)),
                    |choice| Message::LocalShell(choice.0),
                );
                #[cfg(not(windows))]
                let shell = text("System default");
                body = body.push(labeled("Shell", shell)).push(labeled(
                    "Shell integration",
                    checkbox(dialog.shell_integration && integration.is_ok())
                        .label("Add shell integration automatically")
                        .on_toggle_maybe(integration.is_ok().then_some(Message::ShellIntegration)),
                ));
                if let Err(reason) = integration {
                    body = body.push(text(reason).size(13));
                }
            }
            Protocol::Serial => body = body.push(dialog.serial.fields().map(Message::Serial)),
            Protocol::Tn3270 | Protocol::Tn5250 => {
                if let Some(models) = dialog.protocol.ibm_models() {
                    body = body.push(dialog.ibm_view(models));
                }
            }
        }
        if dialog.protocol != Protocol::Sftp {
            body = body.push(
                button(if dialog.appearance {
                    "▾ Terminal Appearance"
                } else {
                    "▸ Terminal Appearance"
                })
                .style(neutral_button)
                .on_press(Message::ShowAppearance)
                .width(Length::Fill),
            );
            if dialog.appearance {
                body = body.push(labeled(
                    "Font size",
                    pick_list(
                        (profile::MIN_TERMINAL_FONT..=profile::MAX_TERMINAL_FONT)
                            .collect::<Vec<_>>(),
                        Some(dialog.font.unwrap_or(self.window_settings.terminal_font)),
                        Message::Font,
                    ),
                ));
            }
            body = body.push(row![
                space::horizontal(),
                button("Reset to Global Defaults")
                    .style(neutral_button)
                    .on_press(Message::Reset)
            ]);
        }
        scrollable(
            container(body.padding(14))
                .width(Length::Fill)
                .style(|_| container::Style::default().background(Color::from_rgb8(31, 31, 31))),
        )
        .height(Length::Fill)
        .into()
    }
}

impl Dialog {
    fn saved_view(&self) -> Element<'_, Message> {
        let options: Vec<_> = self
            .store
            .iter()
            .flat_map(|store| store.search(&self.query))
            .map(SavedChoice::from)
            .collect();
        let selected = options
            .iter()
            .find(|choice| Some(choice.id) == self.selected)
            .cloned();
        let first = options.first().map(|choice| Message::Select(choice.id));
        let mut fields = column![
            text("Saved Connections").size(15),
            text_input("Search saved connections (* wildcard)", &self.query)
                .on_input(Message::Query)
                .on_submit_maybe(first),
            row![
                pick_list(options, selected, |choice| Message::Select(choice.id))
                    .placeholder("Select a saved connection")
                    .width(Length::Fill),
                button("Manage Connections…")
                    .style(neutral_button)
                    .on_press_maybe(self.split.is_none().then_some(Message::Manage)),
            ]
            .spacing(7),
        ]
        .spacing(6);
        if !self.query.trim().is_empty()
            && self
                .store
                .as_ref()
                .is_some_and(|store| store.search(&self.query).next().is_none())
        {
            fields = fields.push(text("No saved connections match.").size(13));
        }
        fields.push(rule::horizontal(1)).into()
    }

    fn group_view(&self) -> Element<'_, Message> {
        let groups = self
            .store
            .iter()
            .flat_map(Store::groups)
            .filter_map(|name| {
                let count = self.group_members(name);
                if count == 0 {
                    return None;
                }
                let selected = self.group.as_deref() == Some(name);
                let label = if count == 1 {
                    "connection"
                } else {
                    "connections"
                };
                Some(
                    button(text(format!("{name} ({count} {label})")))
                        .style(if selected {
                            button::primary
                        } else {
                            button::secondary
                        })
                        .on_press(Message::Group(name.to_owned()))
                        .width(Length::Fill)
                        .into(),
                )
            })
            .collect::<Vec<_>>();
        let body = if groups.is_empty() {
            column![text("No connection groups with saved connections.")]
        } else {
            column(groups).spacing(5)
        };
        column![
            text("Select a connection group").size(15),
            scrollable(container(body).width(Length::Fill)).height(Length::Fill),
        ]
        .spacing(8)
        .padding(10)
        .height(Length::Fill)
        .into()
    }

    fn ibm_view(&self, models: [IbmKind; 2]) -> Element<'_, Message> {
        let mut fields = column![
            labeled(
                "Terminal model",
                pick_list(models, Some(self.ibm.kind), Message::IbmDevice)
            ),
            labeled(
                "Host and port",
                text_input("Numeric IP:port", &self.ibm.address)
                    .on_input(Message::IbmAddress)
                    .on_submit_maybe(self.ibm.connection().map(|_| Message::Connect))
            ),
        ]
        .spacing(8);
        if self.ibm.kind.model_5250().is_some() {
            fields = fields.push(labeled(
                "Device name",
                text_input("Optional", &self.ibm.device_name).on_input(Message::IbmName),
            ));
            if let Err(error) = self.ibm.device_name() {
                fields = fields.push(text(error.to_string()).size(13));
            }
        }
        fields
            .push(
                checkbox(self.ibm.plaintext)
                    .label("Allow plaintext for this connection")
                    .on_toggle(Message::IbmPlaintext),
            )
            .into()
    }
}

#[derive(Clone, PartialEq, Eq)]
struct SavedChoice {
    id: u64,
    label: String,
}

impl From<&Connection> for SavedChoice {
    fn from(connection: &Connection) -> Self {
        let login = &connection.login.endpoint;
        let mut label = format!("{} ({}@{})", connection.name, login.username, login.host);
        if !connection.tag.is_empty() {
            label.push_str(" · ");
            label.push_str(&connection.tag);
        }
        Self {
            id: connection.id,
            label,
        }
    }
}

impl fmt::Display for SavedChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

fn tab(label: &str, selected: bool, message: Message) -> Element<'_, Message> {
    button(label)
        .style(move |theme, status| {
            let style = neutral_button(theme, status);
            if selected && status == button::Status::Active {
                style.with_background(Color::from_rgb8(31, 31, 31))
            } else {
                style
            }
        })
        .on_press(message)
        .into()
}

fn labeled<'a>(label: &'a str, field: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    row![text(label).width(LABEL_WIDTH), field.into()]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

fn neutral_button(theme: &Theme, status: button::Status) -> button::Style {
    let color = match status {
        button::Status::Active => Color::from_rgb8(59, 59, 59),
        button::Status::Hovered => Color::from_rgb8(73, 73, 73),
        button::Status::Pressed => Color::from_rgb8(49, 49, 49),
        button::Status::Disabled => Color::from_rgb8(43, 43, 43),
    };
    button::Style {
        text_color: if status == button::Status::Disabled {
            Color::from_rgb8(145, 145, 145)
        } else {
            Color::from_rgb8(229, 229, 229)
        },
        ..button::secondary(theme, status).with_background(color)
    }
}
