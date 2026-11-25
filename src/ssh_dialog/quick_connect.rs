//! Quick Connect's persistent protocol fields; inactive values stay visible.
use super::*;
use iced::widget::{column, radio};

#[derive(Clone, PartialEq, Eq)]
struct KeySource(Option<KeyChoice>);

impl fmt::Display for KeySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(key) => key.fmt(f),
            None => f.write_str("Use a private key file"),
        }
    }
}

fn line<'a>(
    label: &'a str,
    field: impl Into<Element<'a, Message>>,
    width: f32,
) -> Element<'a, Message> {
    row![text(label).size(13).width(width), field.into()]
        .spacing(10)
        .align_y(Alignment::Center)
        .into()
}

fn inactive(value: String, width: u16) -> Element<'static, Message> {
    button(row![
        text(value).size(13).width(Length::Fill),
        text("▾").size(13)
    ])
    .style(crate::workspace::dialog_design::secondary_button)
    .width(u32::from(width))
    .into()
}

impl SshDialog {
    pub(crate) fn quick_identity_view(
        &self,
        label_width: f32,
        active: bool,
    ) -> Element<'_, Message> {
        let form = &self.target;
        let enabled = active && !form.identity_locked;
        let submit = (active && self.can_connect()).then_some(Message::Connect);
        column![
            line(
                "Host",
                row![
                    text_input("hostname or IP", &form.host)
                        .style(crate::workspace::dialog_design::form_input)
                        .id(form.host_field.clone())
                        .size(13)
                        .width(250)
                        .on_input_maybe(
                            enabled.then_some(|v| Message::Target(LoginMessage::Host(v)))
                        )
                        .on_submit_maybe(submit.clone()),
                    text("Port").size(13),
                    crate::workspace::dialog_design::number_input(
                        text_input("22", &form.port)
                            .style(crate::workspace::dialog_design::form_input)
                            .on_input_maybe(
                                enabled.then_some(|v| Message::Target(LoginMessage::Port(v)))
                            ),
                        &form.port,
                        1..=65535,
                        1,
                        enabled.then_some(|v| Message::Target(LoginMessage::Port(v))),
                        54.0
                    ),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
                label_width
            ),
            line(
                "Username",
                text_input("SSH username", &form.username)
                    .style(crate::workspace::dialog_design::form_input)
                    .size(13)
                    .on_input_maybe(
                        enabled.then_some(|v| Message::Target(LoginMessage::Username(v)))
                    )
                    .on_submit_maybe(submit),
                label_width
            ),
        ]
        .spacing(10)
        .into()
    }

    pub(crate) fn quick_password_enabled(&self, active: bool) -> bool {
        active && matches!(self.target.method, Method::Password | Method::VaultPassword)
    }

    pub(crate) fn quick_authentication_view(
        &self,
        label_width: f32,
        active: bool,
    ) -> Element<'_, Message> {
        let form = &self.target;
        let selected = match form.method {
            Method::StoredKey => Method::PrivateKey,
            Method::VaultPassword => Method::Password,
            other => other,
        };
        let mut methods = vec![
            (Method::Password, "Password"),
            (Method::PrivateKey, "Private Key"),
        ];
        if form.temporary_key.is_some() {
            methods.push((Method::TemporaryKey, "Temporary SSH key"));
        }
        methods.push((Method::KeyboardInteractive, "Keyboard-interactive"));
        let choices = column(methods.into_iter().map(|(method, label)| {
            if active {
                let next = if method == Method::PrivateKey && form.stored_key.is_some() {
                    Method::StoredKey
                } else {
                    method
                };
                crate::workspace::dialog_design::keyboard_table::action(
                    radio(label, method, Some(selected), move |_| {
                        Message::Target(LoginMessage::Authentication(next))
                    })
                    .size(14)
                    .text_size(13)
                    .spacing(7)
                    .into(),
                    Some(Message::Target(LoginMessage::Authentication(next))),
                    true,
                )
            } else {
                row![
                    text(if selected == method { "◉" } else { "○" })
                        .size(14)
                        .style(text::secondary),
                    text(label).size(13).style(text::secondary)
                ]
                .spacing(7)
                .into()
            }
        }))
        .spacing(5);
        let follow_up = active && form.method != Method::KeyboardInteractive;
        let choices =
            choices.push(crate::workspace::dialog_design::keyboard_table::action(
                checkbox(form.keyboard_interactive)
                    .label("Also allow keyboard-interactive")
                    .text_size(13)
                    .on_toggle_maybe(follow_up.then_some(|enabled| {
                        Message::Target(LoginMessage::KeyboardFollowUp(enabled))
                    }))
                    .into(),
                follow_up.then_some(Message::Target(LoginMessage::KeyboardFollowUp(
                    !form.keyboard_interactive,
                ))),
                false,
            ));
        let password = self.quick_password_enabled(active) && form.method == Method::Password;
        column![
            row![text("Authentication").size(13).width(label_width), choices]
                .spacing(10)
                .align_y(Alignment::Start),
            line(
                "Password",
                text_input(
                    if form.method == Method::VaultPassword {
                        "Uses the selected vault credential"
                    } else {
                        "Password"
                    },
                    form.password.as_str()
                )
                .style(crate::workspace::dialog_design::form_input)
                .secure(true)
                .size(13)
                .id(form.credential_field.clone())
                .on_input_maybe(
                    password
                        .then_some(|v| Message::Target(LoginMessage::Password(Password::new(v))))
                )
                .on_submit_maybe((password && self.can_connect()).then_some(Message::Connect)),
                label_width
            ),
        ]
        .spacing(10)
        .into()
    }

    pub(crate) fn quick_key_view(
        &self,
        label_width: f32,
        active: bool,
        keys: &[KeyChoice],
        picker: widget::Id,
    ) -> Element<'_, Message> {
        let form = &self.target;
        let enabled = active && matches!(form.method, Method::PrivateKey | Method::StoredKey);
        let selected = KeySource(KeyChoice::selected(keys, form.stored_key));
        let key: Element<'_, Message> = if enabled {
            let mut choices = keys.to_vec();
            if let Some(selected) = &selected.0
                && !choices.iter().any(|key| key.id == selected.id)
            {
                choices.push(selected.clone());
            }
            pick_list(
                std::iter::once(KeySource(None))
                    .chain(choices.into_iter().map(|key| KeySource(Some(key))))
                    .collect::<Vec<_>>(),
                Some(selected),
                |choice| {
                    Message::Target(match choice.0 {
                        Some(key) => LoginMessage::StoredKey(key),
                        None => LoginMessage::Authentication(Method::PrivateKey),
                    })
                },
            )
            .width(300)
            .text_size(13)
            .keyboard_navigation(picker.clone())
            .on_open(Message::Focus(picker))
            .into()
        } else {
            inactive(selected.to_string(), 300)
        };
        let mut fields = column![line("SSH Key", key, label_width)].spacing(10);
        if form.method == Method::PrivateKey || !form.key_file.is_empty() {
            let file_enabled = active && form.method == Method::PrivateKey;
            fields = fields
                .push(line(
                    "Key file",
                    text_input("Absolute key path", &form.key_file)
                        .style(crate::workspace::dialog_design::form_input)
                        .size(13)
                        .on_input_maybe(
                            file_enabled.then_some(|v| Message::Target(LoginMessage::KeyFile(v))),
                        ),
                    label_width,
                ))
                .push(line(
                    "Passphrase",
                    text_input(
                        "Private key passphrase (optional)",
                        form.passphrase.as_str(),
                    )
                    .style(crate::workspace::dialog_design::form_input)
                    .secure(true)
                    .size(13)
                    .on_input_maybe(
                        file_enabled.then_some(|v| {
                            Message::Target(LoginMessage::Passphrase(Password::new(v)))
                        }),
                    ),
                    label_width,
                ));
        }
        if let Some(temporary) = &form.temporary_key {
            let enabled = active && form.method == Method::TemporaryKey;
            fields = fields.push(line(
                "Temporary SSH key",
                column![
                    row![
                        text_input("1–1440", &temporary.minutes)
                            .style(crate::workspace::dialog_design::form_input)
                            .size(13)
                            .width(72)
                            .on_input_maybe(enabled.then_some(|v| Message::Target(
                                LoginMessage::TemporaryLifetime(v)
                            ))),
                        text("minutes").size(13),
                        button(text("Paste private key").size(13))
                            .on_press_maybe(enabled.then_some(Message::PasteTemporaryKey))
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                    text(temporary.status()).size(13),
                ]
                .spacing(5),
                label_width,
            ));
        }
        fields.into()
    }

    pub(crate) fn quick_options_view(
        &self,
        label_width: f32,
        active: bool,
        keys: &[KeyChoice],
    ) -> Element<'_, Message> {
        if active {
            return self.options_view(label_width, keys);
        }
        column![
            self.startup
                .editor_view(label_width, false)
                .map(|(field, value)| Message::Startup(field, value)),
            line(
                "Host key verification",
                text("Required for SSH").size(13).style(text::secondary),
                label_width
            )
        ]
        .spacing(8)
        .into()
    }
}
