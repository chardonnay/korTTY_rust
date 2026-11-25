use std::fmt;

use iced::widget::{
    button, checkbox, column, container, pick_list, row, rule, scrollable, space, text, text_input,
};
use iced::{Alignment, Color, Element, Length, Theme, window};

use super::{
    Connection, Desktop, Dialog, IbmKind, Message, Protocol, Section, Store, TerminalProfile,
    Workspace, wrap,
};
use crate::ssh_dialog::{self, LoginMessage};
use crate::terminal_logging::EditorAction;
use crate::vault_window::Kind;
use crate::workspace::dialog_placement::escape::on_escape;

const RECENT_COUNT: usize = 10;
const LABEL_WIDTH: f32 = 220.0;

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
            crate::workspace::dialog_placement::escape::on_key_press(
                on_escape(
                    container(self.quick_view(dialog))
                        .id(dialog.focus_scope.clone())
                        .into(),
                    Message::Cancel,
                ),
                |key, modifiers| {
                    (key == &iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab)
                        && (modifiers - iced::keyboard::Modifiers::SHIFT).is_empty())
                    .then_some(Message::Navigate(modifiers.shift()))
                },
            )
            .map(move |message| wrap(id, message)),
        )
    }

    fn quick_view<'a>(&'a self, dialog: &'a Dialog) -> Element<'a, Message> {
        let recent: Vec<_> = dialog
            .store
            .iter()
            .flat_map(|store| {
                store.recent_ids().filter_map(|id| {
                    let label = if let Some(connection) = store.get(id) {
                        connection.name.clone()
                    } else if let Some(profile) = store.serial(id) {
                        profile.display_name()
                    } else {
                        store.local(id)?.display_name()
                    };
                    Some((id, label))
                })
            })
            .take(RECENT_COUNT)
            .collect();
        let mut content = column![].spacing(10);
        if !recent.is_empty() {
            let choices = row(recent.into_iter().map(|(id, label)| {
                let message = (!dialog.saving).then_some(Message::Recent(id));
                crate::workspace::dialog_design::keyboard_table::action(
                    button(text(label).size(13).wrapping(text::Wrapping::None))
                        .width(140)
                        .clip(true)
                        .style(neutral_button)
                        .on_press_maybe(message.clone())
                        .into(),
                    message,
                    true,
                )
            }))
            .spacing(8)
            .wrap();
            let buttons = column![
                text("Recently used connections").size(13).font(
                    crate::workspace::dialog_design::ui_font(iced::font::Weight::Bold)
                ),
                choices
            ]
            .spacing(6);
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
        let connect = if dialog.group_tab {
            "Open Group"
        } else {
            "Connect"
        };
        let actions = row![
            space::horizontal(),
            (!split).then(|| footer_action(
                "Load Project",
                (!dialog.saving).then_some(Message::LoadProject),
                false
            )),
            (!split).then(space::horizontal),
            footer_action("Cancel", (!dialog.saving).then_some(Message::Cancel), false),
            footer_action(connect, enabled.then_some(Message::Connect), true),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        let logging_error = dialog
            .section_visible(Section::Logging)
            .then(|| dialog.logging_form().config().err())
            .flatten()
            .map(|e| e.to_string());
        let status = if dialog.saving {
            Some("Saving connection…")
        } else if dialog.section_visible(Section::Appearance) && !dialog.appearance.valid() {
            Some("Enter colors as #RRGGBB or leave them empty.")
        } else {
            logging_error.as_deref().or(dialog.status.as_deref())
        };
        if !split {
            content = content.push(tabs).push(rule::horizontal(1));
        }
        content = content.push(body);
        if let Some(status) = status {
            content = content.push(text(status.to_owned()).size(13));
        }
        content
            .push(actions)
            .padding(15)
            .height(Length::Fill)
            .into()
    }

    fn quick_individual_view<'a>(&'a self, dialog: &'a Dialog) -> Element<'a, Message> {
        let section =
            |kind, title: &str| {
                let expanded = self
                    .window_settings
                    .quick_connect_expanded_sections
                    .as_ref()
                    .is_some_and(|sections| sections.contains(&kind));
                let label = format!("{} {title}", if expanded { "▾" } else { "▸" });
                let message = Message::ToggleSection(kind);
                let header =
                    crate::workspace::dialog_design::keyboard_table::action(
                        button(text(label).size(13).font(
                            crate::workspace::dialog_design::ui_font(iced::font::Weight::Bold),
                        ))
                        .style(neutral_button)
                        .on_press(message.clone())
                        .width(Length::Fill)
                        .into(),
                        Some(message),
                        true,
                    );
                (header, expanded)
            };
        let active = dialog.protocol.is_ssh();
        let mut body = column![dialog.saved_view()].spacing(10);
        if dialog.protocol.ibm_models().is_none() {
            body = body.push(
                dialog
                    .ssh
                    .quick_identity_view(LABEL_WIDTH, active)
                    .map(Message::Ssh),
            );
        }
        body = body.push(labeled(
            "Protocol",
            pick_list(
                self.quick_protocols(dialog.split.is_some()),
                Some(dialog.protocol),
                Message::Protocol,
            )
            .width(180)
            .text_size(13)
            .keyboard_navigation(dialog.protocol_picker.clone())
            .on_open(Message::FocusWidget(dialog.protocol_picker.clone())),
        ));
        if dialog.protocol.ibm_models().is_none() {
            if dialog.protocol != Protocol::Sftp {
                body = body.push(labeled(
                    "Terminal emulation",
                    pick_list(
                        TerminalProfile::ALL,
                        Some(match dialog.protocol {
                            Protocol::Serial => dialog.serial.profile(),
                            Protocol::Local => dialog.local_emulation,
                            _ => dialog.ssh.profile(),
                        }),
                        Message::Profile,
                    )
                    .text_size(13)
                    .width(Length::Fill)
                    .keyboard_navigation(dialog.emulation_picker.clone())
                    .on_open(Message::FocusWidget(dialog.emulation_picker.clone())),
                ));
            }
            body = body.push(
                dialog
                    .ssh
                    .quick_authentication_view(LABEL_WIDTH, active)
                    .map(Message::Ssh),
            );
            let credentials = dialog
                .ssh
                .target_endpoint()
                .map_or_else(Vec::new, |endpoint| {
                    let mut values = self.scoped_credential_choices(Kind::Password, &endpoint);
                    values.extend(self.scoped_credential_choices(Kind::PasswordCommand, &endpoint));
                    values
                });
            let selected = credentials
                .iter()
                .find(|choice| Some(choice.id.as_str()) == dialog.ssh.vault_password())
                .cloned();
            let credentials: Element<'a, Message> = if dialog.ssh.quick_password_enabled(active) {
                pick_list(credentials, selected, |choice| {
                    Message::Ssh(ssh_dialog::Message::Target(LoginMessage::VaultPassword(
                        choice.id,
                    )))
                })
                .placeholder("Select a vault credential")
                .width(300)
                .text_size(13)
                .keyboard_navigation(dialog.credential_picker.clone())
                .on_open(Message::FocusWidget(dialog.credential_picker.clone()))
                .into()
            } else {
                button(
                    text(
                        selected
                            .map_or_else(|| "Select a vault credential".into(), |v| v.to_string()),
                    )
                    .size(13),
                )
                .width(300)
                .style(neutral_button)
                .into()
            };
            body = body.push(labeled(
                "Stored credentials",
                row![
                    credentials,
                    action(
                        "Vault…",
                        active.then_some(Message::Ssh(ssh_dialog::Message::OpenVault)),
                        false
                    )
                ]
                .spacing(7),
            ));
            body = body.push(
                dialog
                    .ssh
                    .quick_key_view(
                        LABEL_WIDTH,
                        active,
                        &self.ssh_key_choices,
                        dialog.key_picker.clone(),
                    )
                    .map(Message::Ssh),
            );
            body = body.push(self.quick_local_view(dialog));
            body = body.push(
                dialog
                    .serial
                    .protocol_fields_at(dialog.protocol == Protocol::Serial, LABEL_WIDTH as u16)
                    .map(Message::Serial),
            );
            let native_private = matches!(dialog.protocol, Protocol::Local | Protocol::Serial)
                && (dialog.selected_incognito()
                    || dialog.split.is_some_and(|target| {
                        self.split_connection_privacy(dialog.owner, target, false) != Ok(false)
                    }));
            let save_allowed = dialog.protocol != Protocol::Sftp
                && !dialog.ssh.temporary_key_selected()
                && !native_private;
            body = body
                .push(labeled(
                    "",
                    crate::workspace::dialog_design::keyboard_table::action(
                        checkbox(dialog.save)
                            .label("Save Connection")
                            .text_size(13)
                            .on_toggle_maybe(save_allowed.then_some(Message::SaveConnection))
                            .into(),
                        save_allowed.then_some(Message::SaveConnection(!dialog.save)),
                        false,
                    ),
                ))
                .push(labeled(
                    "Name",
                    text_input("Connection name (optional)", &dialog.name)
                        .size(13)
                        .on_input_maybe(
                            (save_allowed && dialog.save).then_some(Message::ConnectionName),
                        ),
                ));
            let (header, expanded) = section(Section::ConnectionSettings, "Connection Timeout");
            body = body.push(header);
            if expanded {
                body = body.push(
                    dialog
                        .ssh
                        .quick_options_view(LABEL_WIDTH, active, &self.ssh_key_choices)
                        .map(Message::Ssh),
                );
                if active {
                    body = body.push(text("Host-key verification is required.").size(12));
                }
            }
            if active && let Some(notice) = dialog.ssh.validation_notice() {
                body = body.push(text(notice).size(13));
            }
        } else if let Some(models) = dialog.protocol.ibm_models() {
            body = body.push(dialog.ibm_view(models));
        }
        if dialog.section_visible(Section::Appearance) {
            let (header, expanded) = section(Section::Appearance, "Terminal Appearance");
            body = body.push(header);
            if expanded {
                body = body.push(self.quick_appearance_view(dialog));
            }
        }
        if self.terminal_effects_enabled() && dialog.section_visible(Section::Effect) {
            let (header, expanded) = section(Section::Effect, "Terminal Effect");
            body = body.push(header);
            if expanded {
                let available: Vec<_> = crate::terminal_effects::Effect::ALL
                    .into_iter()
                    .filter(|effect| self.allowed_terminal_effect(*effect))
                    .collect();
                body = body.push(dialog.effect.view(
                    &available,
                    Message::Effect,
                    Message::EffectSpeed,
                ));
            }
        }
        if dialog.section_visible(Section::Logging) {
            let (header, expanded) = section(Section::Logging, "Terminal log");
            body = body.push(header);
            if expanded {
                body = body.push(
                    dialog
                        .logging_form()
                        .compact_editor_view(self.quick_logging_available(dialog), LABEL_WIDTH)
                        .map(|action| match action {
                            EditorAction::Edit(edit) => {
                                Message::Ssh(ssh_dialog::Message::Logging(edit))
                            }
                            EditorAction::Browse => Message::BrowseTerminalLogFolder,
                        }),
                );
            }
        }
        if dialog.section_visible(Section::Ai)
            && let Some(config) = self.ai_configuration()
            && let Some(ai) = dialog.ai_view(&config)
        {
            let (header, expanded) = section(Section::Ai, "AI");
            body = body.push(header);
            if expanded {
                body = body.push(ai);
            }
        }
        if dialog.section_visible(Section::Appearance) {
            body = body.push(action(
                "Reset to Global Defaults",
                Some(Message::Reset),
                false,
            ));
        }
        scrollable(
            container(body.padding(15))
                .width(Length::Fill)
                .style(|_| container::Style::default().background(Color::from_rgb8(31, 31, 31))),
        )
        .id(dialog.form_scroll.clone())
        .height(Length::Fill)
        .into()
    }
}

impl Dialog {
    fn saved_view(&self) -> Element<'_, Message> {
        let serial_choice = |profile: &crate::connection_store::SerialProfile| SavedChoice {
            id: profile.id,
            label: format!(
                "{} ({} · {})",
                profile.display_name(),
                if profile.pinned {
                    "Pinned serial"
                } else {
                    "Serial"
                },
                profile.line()
            ),
        };
        let options: Vec<_> = self
            .store
            .iter()
            .flat_map(|store| {
                let (pinned, serials): (Vec<_>, Vec<_>) = store
                    .search_serial(&self.query)
                    .partition(|profile| profile.pinned);
                pinned
                    .into_iter()
                    .map(serial_choice)
                    .chain(store.search(&self.query).map(SavedChoice::from))
                    .chain(serials.into_iter().map(serial_choice))
                    .chain(store.search_local(&self.query).map(|profile| SavedChoice {
                        id: profile.id,
                        label: format!("{} (Local Shell)", profile.display_name()),
                    }))
            })
            .collect();
        let empty = options.is_empty();
        let selected = options
            .iter()
            .find(|choice| Some(choice.id) == self.selected_profile_id())
            .cloned();
        let first = options.first().map(|choice| Message::Select(choice.id));
        let mut fields = column![
            text("Saved Connections:").size(13),
            text_input(
                "Search saved connections by name, host or tag (* wildcard)",
                &self.query
            )
            .size(13)
            .on_input(Message::Query)
            .on_submit_maybe(first),
            row![
                pick_list(options, selected, |choice| Message::Select(choice.id))
                    .placeholder("Select a saved connection")
                    .width(400)
                    .text_size(13)
                    .keyboard_navigation(self.saved_picker.clone())
                    .on_open(Message::FocusWidget(self.saved_picker.clone())),
                button(text("Manage Connections…").size(13))
                    .style(neutral_button)
                    .on_press_maybe(self.split.is_none().then_some(Message::Manage)),
            ]
            .spacing(7),
        ]
        .spacing(6);
        if !self.query.trim().is_empty() && self.store.is_some() && empty {
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
            scrollable(container(body).width(Length::Fill))
                .id(self.form_scroll.clone())
                .height(Length::Fill),
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
    let control = button(text(label).size(13))
        .style(move |theme, status| {
            let style = neutral_button(theme, status);
            if selected && status == button::Status::Active {
                style.with_background(Color::from_rgb8(31, 31, 31))
            } else {
                style
            }
        })
        .on_press(message.clone());
    crate::workspace::dialog_design::keyboard_table::action(control.into(), Some(message), true)
}

pub(super) fn labeled<'a>(
    label: &'a str,
    field: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    row![text(label).size(13).width(LABEL_WIDTH), field.into()]
        .spacing(10)
        .align_y(Alignment::Center)
        .into()
}

fn action<'a>(label: &'a str, message: Option<Message>, primary: bool) -> Element<'a, Message> {
    action_width(label, message, primary, Length::Shrink)
}

fn footer_action<'a>(
    label: &'a str,
    message: Option<Message>,
    primary: bool,
) -> Element<'a, Message> {
    action_width(label, message, primary, Length::Fixed(100.0))
}

fn action_width<'a>(
    label: &'a str,
    message: Option<Message>,
    primary: bool,
    width: Length,
) -> Element<'a, Message> {
    let button = button(text(label).size(13).width(Length::Fill).center())
        .width(width)
        .on_press_maybe(message.clone());
    let button = if primary {
        button
    } else {
        button.style(neutral_button)
    };
    crate::workspace::dialog_design::keyboard_table::action(button.into(), message, true)
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
