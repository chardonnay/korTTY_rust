use super::*;
use iced::widget::{column, container};

impl Draft {
    pub(super) fn settings_page<'a>(
        &'a self,
        choices: &ViewChoices<'_>,
        inherited: Option<(&str, IsolationLevel)>,
    ) -> Element<'a, Message> {
        let (level, source) = if self.protocol() == ProtocolKind::Local {
            self.protocols.local.protocol_isolation().resolve(
                choices.local_isolation_default,
                inherited,
                choices.isolation.1,
            )
        } else {
            resolve_isolation(
                choices.isolation.0,
                inherited,
                self.isolation.0,
                None,
                false,
                choices.isolation.1,
            )
        };
        let isolation: Element<'_, Message> = if self.protocol() == ProtocolKind::Local {
            pick_list(
                crate::session_security::LocalIsolation::ALL,
                Some(self.protocols.local.protocol_isolation()),
                |value| Message::Local(super::super::local::Edit::Isolation(value)),
            )
            .width(280)
            .text_size(13)
            .keyboard_navigation(self.ids.behavior_isolation.clone())
            .into()
        } else {
            pick_list(IsolationChoice::ALL, Some(self.isolation), |value| {
                Message::Edit(Edit::Isolation(value))
            })
            .width(280)
            .text_size(13)
            .keyboard_navigation(self.ids.behavior_isolation.clone())
            .into()
        };
        let paste_choice = |mode| PasteChoice {
            mode,
            default: choices.default_paste_warning,
        };
        let paste_choices: Vec<_> = std::iter::once(None)
            .chain(PasteWarningMode::ALL.map(Some))
            .map(paste_choice)
            .collect();
        let own = self.own_terminal_settings;
        let defaults = &choices.terminal_style;
        let terminal_edit = |edit| Message::Edit(Edit::Terminal(edit));
        let additional = Message::Edit(Edit::ShowAdditionalTerminalSettings(
            !self.show_additional_terminal_settings,
        ));
        let incognito = choices
            .incognito_allowed
            .then(|| Message::Edit(Edit::Incognito(!self.incognito)));
        let style = container(
            column![
                self.terminal
                    .ssh_style_view(&self.color_picker, choices.themes, defaults, own)
                    .map(Message::Edit),
                rule::horizontal(1),
                self.terminal
                    .ssh_close_confirmation_view(defaults, own)
                    .map(terminal_edit),
                terminal_overrides::ssh_style::ssh_checkbox(
                    "Show command timestamps",
                    self.command_timestamps,
                    defaults.command_timestamps.unwrap_or(false),
                    own,
                    |value| Message::Edit(Edit::CommandTimestamps(value)),
                ),
            ]
            .spacing(10),
        )
        .padding(10)
        .max_width(330);
        let behavior = container(column![
            behavior_row("Tab color", row![
                tooltip(
                    keyboard_table::action(
                        checkbox(self.tab_color.enabled())
                            .label("Mark this connection's tabs with a color")
                            .text_size(13)
                            .on_toggle(|enabled| Message::Edit(Edit::UseTabColor(enabled))).into(),
                        Some(Message::Edit(Edit::UseTabColor(!self.tab_color.enabled()))), false,
                    ), text("Hover over the tab to see its color."), tooltip::Position::Bottom,
                ),
                self.color_picker.view(ColorTarget::Tab, Some(self.tab_color.picker_color()), self.tab_color.enabled())
                    .map(|edit| Message::Edit(Edit::ColorPicker(edit))),
            ].spacing(10).align_y(Alignment::Center).width(Length::Fill).wrap()),
            behavior_row("Keyword highlighting",
                container(self.terminal.ssh_highlight_picker(&choices.highlighting, self.ids.behavior_highlight.clone()).map(terminal_edit)).width(280)),
            behavior_row("Warn about multi-line pastes",
                tooltip(pick_list(paste_choices, Some(paste_choice(self.paste_warning)), |choice| {
                    Message::Edit(Edit::PasteWarning(choice.mode))
                }).width(280).text_size(13).keyboard_navigation(self.ids.behavior_paste.clone()),
                text("Use the default follows Settings → Terminal → Paste Protection. Off also turns off control-character warnings."), tooltip::Position::Bottom)),
            (choices.minimum_paste_warning != PasteWarningMode::Off).then(|| {
                text(format!("Organization minimum · {}", choices.minimum_paste_warning)).size(12)
            }),
            self.terminal.ssh_paste_delay_view(choices.default_paste_line_delay_ms).map(terminal_edit),
            heading("Session isolation"),
            behavior_row("Isolation", isolation),
            row![iced::widget::space::horizontal().width(160), text(format!("Effective isolation · {level} · {source}")).size(12)].spacing(8),
            self.terminal.strict_mode_view().map(terminal_edit),
            row![iced::widget::space::horizontal().width(160),
                keyboard_table::action(checkbox(self.incognito)
                    .label(if choices.incognito_allowed { "Incognito session" } else { "Incognito session · managed policy" })
                    .text_size(13)
                    .on_toggle_maybe(choices.incognito_allowed.then_some(|value| Message::Edit(Edit::Incognito(value)))).into(), incognito, false),
            ].spacing(8),
        ].spacing(10)).padding(10);
        let mut editor = column![
            self.terminal_settings_source(defaults),
            text("These settings override the global settings only for this connection.").size(13),
            style,
            rule::horizontal(1),
            heading("Terminal behavior"),
            behavior,
        ]
        .spacing(15);
        if let Some(effects) = choices.terminal_effects {
            editor = editor.push(
                column![
                    rule::horizontal(1),
                    heading("Terminal Effect"),
                    container(self.terminal_effect.connection_view(
                        effects,
                        self.ids.behavior_effect.clone(),
                        |effect| Message::Edit(Edit::TerminalEffect(effect)),
                        |speed| Message::Edit(Edit::TerminalEffectSpeed(speed)),
                    ))
                    .padding(10),
                ]
                .spacing(6),
            );
        }
        editor = editor
            .push(rule::horizontal(1))
            .push(keyboard_table::action(
                button(if self.show_additional_terminal_settings {
                    "Hide additional terminal settings"
                } else {
                    "Show additional terminal settings"
                })
                .style(dialog_design::secondary_button)
                .on_press(additional.clone())
                .into(),
                Some(additional),
                true,
            ))
            .push(self.show_additional_terminal_settings.then(|| {
                container(
                    column![
                        self.terminal.ssh_launch_size_view().map(terminal_edit),
                        self.terminal
                            .ssh_additional_style_view(defaults, own)
                            .map(terminal_edit),
                    ]
                    .spacing(10),
                )
                .padding(10)
            }));
        editor.into()
    }

    fn terminal_settings_source(&self, defaults: &TerminalStyle) -> Element<'_, Message> {
        use iced::keyboard::{Key, key::Named};
        use keyboard_table::TableKey;

        let own = self.own_terminal_settings;
        let global = Message::Edit(Edit::UseOwnTerminalSettings(None));
        let custom = Message::Edit(Edit::UseOwnTerminalSettings(Some(Box::new(
            defaults.clone(),
        ))));
        let global_radio = radio("Use the global terminal settings", false, Some(own), {
            let global = global.clone();
            move |_| global.clone()
        })
        .text_size(13)
        .size(16);
        let custom_radio = radio("Own settings for this connection", true, Some(own), {
            let custom = custom.clone();
            move |_| custom.clone()
        })
        .text_size(13)
        .size(16);
        keyboard_table::view(
            column![global_radio, custom_radio].spacing(15).into(),
            move |key, modifiers, _| {
                if !modifiers.is_empty() {
                    return TableKey::Ignored;
                }
                match key {
                    Key::Named(Named::ArrowDown | Named::ArrowRight) => {
                        TableKey::Handled(Some(custom.clone()))
                    }
                    Key::Named(Named::ArrowUp | Named::ArrowLeft) => {
                        TableKey::Handled(Some(global.clone()))
                    }
                    Key::Named(Named::Space) => TableKey::Handled(None),
                    _ => TableKey::Ignored,
                }
            },
        )
    }
}

fn heading(label: &'static str) -> Element<'static, Message> {
    text(label)
        .size(13)
        .font(dialog_design::ui_font(iced::font::Weight::Bold))
        .into()
}

fn behavior_row<'a>(
    label: &'static str,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    row![text(label).size(13).width(160), control.into()]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}
