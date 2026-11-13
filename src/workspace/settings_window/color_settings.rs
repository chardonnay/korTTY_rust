use std::fmt;

use iced::widget::{checkbox, column, container, pick_list, row, rule, space, text, text_input};
use iced::{Alignment, Element, Length};
use kortty_terminal::Palette;

use crate::terminal_theme::{Id, Rgb, Scheme};

use super::WindowSettings;

const ANSI_NAMES: [&str; 8] = [
    "Black", "Red", "Green", "Yellow", "Blue", "Magenta", "Cyan", "White",
];

#[derive(Clone, Copy, Debug)]
pub enum Channel {
    Foreground,
    Background,
    Cursor,
    Selection,
    Ansi(u8),
}

impl Channel {
    fn index(self) -> usize {
        match self {
            Self::Foreground => 0,
            Self::Background => 1,
            Self::Cursor => 2,
            Self::Selection => 3,
            Self::Ansi(index) => 4 + usize::from(index),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Edit {
    Profile(Option<Id>),
    Rgb(Channel, String),
    CursorBlink(bool),
    EnableSgr(bool),
}

#[derive(Default)]
pub(super) struct Editor {
    fields: [Option<String>; 20],
}

#[derive(Clone, PartialEq)]
struct Profile {
    id: Option<Id>,
    name: String,
}

impl fmt::Display for Profile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)
    }
}

impl Editor {
    pub(super) fn valid(&self) -> bool {
        self.fields
            .iter()
            .flatten()
            .all(|value| Rgb::parse(value).is_some())
    }

    pub(super) fn profile_changed(&mut self) {
        self.fields[..3].fill(None);
    }

    pub(super) fn update(&mut self, preferences: &mut WindowSettings, message: Edit) {
        match message {
            Edit::Profile(id) if id != preferences.themes.selected() => {
                let blink = preferences.terminal_appearance().cursor_blink;
                preferences.themes.select(id);
                if preferences.themes.selected() != id {
                    return;
                }
                preferences.colors.cursor_blink = Some(blink);
                if preferences.themes.apply_font
                    && let Some(scheme) = preferences.themes.appearance()
                {
                    preferences.terminal_font = scheme.font_size;
                    preferences.set_terminal_font_family(scheme.family.label().into());
                }
                self.profile_changed();
            }
            Edit::CursorBlink(enabled) => preferences.colors.cursor_blink = Some(enabled),
            Edit::EnableSgr(enabled) => {
                preferences.colors.sgr_enabled = enabled;
                if !enabled {
                    self.fields[4..].fill(None);
                }
            }
            Edit::Rgb(channel, mut value) => {
                value.truncate(value.floor_char_boundary(12));
                let colors = &mut preferences.colors;
                let customized = preferences.themes.selected().is_none();
                let defaults = Scheme::default();
                let (target, builtin) = match channel {
                    Channel::Foreground if customized => {
                        (&mut colors.foreground, defaults.foreground)
                    }
                    Channel::Background if customized => {
                        (&mut colors.background, defaults.background)
                    }
                    Channel::Cursor if customized => (&mut colors.cursor, defaults.cursor),
                    Channel::Ansi(index) if index < 16 && colors.sgr_enabled => (
                        &mut colors.ansi[usize::from(index)],
                        Rgb::from_bytes(Palette::default().rgb(index)),
                    ),
                    Channel::Selection => {
                        if let Some(color) = Rgb::parse(&value) {
                            colors.selection = color;
                        }
                        self.fields[channel.index()] = Some(value);
                        return;
                    }
                    _ => return,
                };
                if let Some(color) = Rgb::parse(&value) {
                    *target = (color != builtin).then_some(color);
                }
                self.fields[channel.index()] = Some(value);
            }
            Edit::Profile(_) => {}
        }
    }

    pub(super) fn view(
        &self,
        preferences: &WindowSettings,
        target: Option<&iced::widget::Id>,
    ) -> Element<'_, Edit> {
        let appearance = preferences.terminal_appearance();
        let colors = &preferences.colors;
        let selected = preferences.themes.selected();
        let profiles = std::iter::once(Profile {
            id: None,
            name: "Custom (no profile)".into(),
        })
        .chain(preferences.themes.iter().map(|theme| Profile {
            id: Some(theme.id),
            name: theme.name.into(),
        }))
        .collect::<Vec<_>>();
        let profile = profiles
            .iter()
            .find(|profile| profile.id == selected)
            .cloned();
        let color_row = |label, channel, color, editable| {
            let focus = target.filter(|_| {
                matches!(
                    (selected, channel),
                    (None, Channel::Foreground) | (Some(_), Channel::Selection)
                )
            });
            row![
                text(label).width(132),
                self.field(channel, color, editable, focus).width(112),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
        };
        let palette_row = |bright| {
            let offset = if bright { 8 } else { 0 };
            let fields = ANSI_NAMES.iter().enumerate().map(|(index, name)| {
                let index = index as u8 + offset;
                column![
                    text(*name).size(12),
                    self.field(
                        Channel::Ansi(index),
                        Rgb::from_bytes(appearance.palette.rgb(index)),
                        colors.sgr_enabled,
                        None,
                    ),
                ]
                .width(Length::Fill)
                .spacing(3)
                .into()
            });
            row![
                text(if bright { "Bright" } else { "Normal" }).width(48),
                row(fields).width(Length::Fill).spacing(5),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
        };
        column![
            row![
                text("Color Profile").width(132),
                pick_list(profiles, profile, |profile| Edit::Profile(profile.id)).width(260)
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            color_row(
                "Foreground",
                Channel::Foreground,
                appearance.foreground,
                selected.is_none()
            ),
            color_row(
                "Background",
                Channel::Background,
                appearance.background,
                selected.is_none()
            ),
            color_row(
                "Cursor",
                Channel::Cursor,
                appearance.cursor,
                selected.is_none()
            ),
            selected.map(|_| text(
                "The selected profile supplies the foreground, background and cursor colors."
            )
            .size(12)),
            checkbox(appearance.cursor_blink)
                .label("Cursor blinks")
                .on_toggle(Edit::CursorBlink),
            color_row("Selection", Channel::Selection, colors.selection, true),
            checkbox(colors.sgr_enabled)
                .label("Enable terminal colors")
                .on_toggle(Edit::EnableSgr),
            text("Accept ANSI and TrueColor commands that change text and background colors.")
                .size(12),
            rule::horizontal(1),
            text("ANSI Colors").size(16),
            palette_row(false),
            palette_row(true),
        ]
        .spacing(10)
        .into()
    }

    fn field(
        &self,
        channel: Channel,
        color: Rgb,
        editable: bool,
        target: Option<&iced::widget::Id>,
    ) -> iced::widget::Column<'_, Edit> {
        let raw = self.fields[channel.index()]
            .clone()
            .unwrap_or_else(|| color.to_string());
        let swatch = container(space::horizontal())
            .width(Length::Fill)
            .height(8)
            .style(move |_| container::Style::default().background(color.color()));
        column![
            swatch,
            crate::settings_search::mark_input(
                text_input("#RRGGBB", &raw)
                    .size(12)
                    .padding(4)
                    .on_input_maybe(editable.then_some(move |value| Edit::Rgb(channel, value))),
                target,
            ),
        ]
        .width(Length::Fill)
        .spacing(3)
    }
}
