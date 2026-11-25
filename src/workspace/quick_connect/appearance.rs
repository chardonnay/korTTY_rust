use iced::Element;
use iced::widget::{checkbox, column, pick_list, text_input};

use crate::connection_store::TerminalAppearance;
use crate::connection_window::theme_choices;
use crate::profile::{MAX_TERMINAL_FONT, MIN_TERMINAL_FONT};
use crate::terminal_theme::{Rgb, installed_families};

use super::{Dialog, Message, Protocol, Workspace};

#[derive(Default)]
pub(super) struct Draft {
    pub(super) style: TerminalAppearance,
    pub(super) foreground: String,
    pub(super) background: String,
}

impl Draft {
    fn new(style: TerminalAppearance) -> Self {
        Self {
            foreground: style
                .foreground
                .map_or_else(String::new, |color| color.to_string()),
            background: style
                .background
                .map_or_else(String::new, |color| color.to_string()),
            style,
        }
    }

    pub(super) fn valid(&self) -> bool {
        [&self.foreground, &self.background]
            .into_iter()
            .all(|value| value.trim().is_empty() || Rgb::parse(value).is_some())
    }
}

impl Dialog {
    pub(super) fn reset_appearance_draft(&mut self) {
        let style = match self.protocol {
            Protocol::Ssh => self
                .selected()
                .map(|profile| TerminalAppearance::from_terminal(&profile.terminal)),
            Protocol::Local => self
                .selected_local()
                .map(|profile| TerminalAppearance::from_terminal(&profile.terminal)),
            Protocol::Serial => self
                .selected_serial()
                .map(|profile| profile.appearance.as_deref().cloned().unwrap_or_default()),
            _ => None,
        };
        self.appearance = Draft::new(style.unwrap_or_else(|| self.appearance_defaults.clone()));
        self.appearance_edited = false;
    }
}

impl Workspace {
    pub(super) fn quick_appearance_view<'a>(&'a self, dialog: &'a Dialog) -> Element<'a, Message> {
        use super::view::labeled;
        let draft = &dialog.appearance;
        let themes = &self.window_settings.themes;
        let (choices, selected) = theme_choices(themes, draft.style.theme.as_ref());
        let scheme = draft
            .style
            .theme
            .as_ref()
            .and_then(|theme| theme.scheme(themes));
        let families = std::iter::once("Inherit".to_owned())
            .chain(installed_families().iter().cloned())
            .collect::<Vec<_>>();
        let family = draft
            .style
            .font_family
            .clone()
            .unwrap_or_else(|| "Inherit".into());
        column![
            labeled(
                "Theme",
                pick_list(choices, Some(selected), |choice| Message::Theme(
                    choice.value
                ))
                .width(200)
                .text_size(13)
                .keyboard_navigation(dialog.appearance_pickers[0].clone())
                .on_open(Message::FocusWidget(dialog.appearance_pickers[0].clone()))
            ),
            labeled(
                "Font",
                iced::widget::row![
                    pick_list(families, Some(family), |value| Message::FontFamily(
                        (value != "Inherit").then_some(value)
                    ))
                    .width(200)
                    .text_size(13)
                    .keyboard_navigation(dialog.appearance_pickers[1].clone())
                    .on_open(Message::FocusWidget(dialog.appearance_pickers[1].clone())),
                    pick_list(
                        (MIN_TERMINAL_FONT..=MAX_TERMINAL_FONT).collect::<Vec<_>>(),
                        Some(
                            draft
                                .style
                                .font_size
                                .or_else(|| scheme.map(|s| s.font_size))
                                .unwrap_or(self.window_settings.terminal_font)
                        ),
                        Message::Font
                    )
                    .width(80)
                    .text_size(13)
                    .keyboard_navigation(dialog.appearance_pickers[2].clone())
                    .on_open(Message::FocusWidget(dialog.appearance_pickers[2].clone())),
                ]
                .spacing(10)
            ),
            labeled(
                "Foreground",
                text_input("Inherit or #RRGGBB", &draft.foreground)
                    .size(13)
                    .on_input(Message::Foreground)
            ),
            labeled(
                "Background",
                text_input("Inherit or #RRGGBB", &draft.background)
                    .size(13)
                    .on_input(Message::Background)
            ),
            labeled(
                "Terminal colors",
                checkbox(
                    draft
                        .style
                        .sgr_enabled
                        .unwrap_or_else(|| self.window_settings.terminal_appearance().sgr_enabled)
                )
                .label("Allow applications to set text colors")
                .on_toggle(Message::TerminalColors)
            ),
        ]
        .spacing(6)
        .into()
    }
}
