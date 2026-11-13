use std::fmt;

use iced::widget::{button, column, container, pick_list, row, text, text_input};
use iced::{Alignment, Element};

use crate::editor_window::caret::Shape;
use crate::snippet_window::appearance::{MAX_FONT_SIZE, Preferences};
use crate::terminal_theme::{Rgb, installed_families, installed_font};

use super::{Desktop, Dialog, Message, Setting, highlighted};

#[derive(Clone, Copy, Debug)]
pub enum Color {
    Foreground,
    Background,
    Cursor,
}

impl Color {
    fn setting(self) -> Setting {
        match self {
            Self::Foreground => Setting::SnippetForeground,
            Self::Background => Setting::SnippetBackground,
            Self::Cursor => Setting::SnippetCursorColor,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Edit {
    Family(Option<String>),
    Size(String),
    CursorStyle(Option<Shape>),
    Color(Color, String),
}

#[derive(Default)]
pub(super) struct Editor {
    size: Option<String>,
    foreground: Option<String>,
    background: Option<String>,
    cursor: Option<String>,
}

impl Editor {
    pub(super) fn update(&mut self, preferences: &mut Preferences, edit: Edit) {
        match edit {
            Edit::Family(family) => {
                if family
                    .as_deref()
                    .is_none_or(|name| installed_font(name).is_some())
                {
                    preferences.font_family = family;
                }
            }
            Edit::Size(mut value) => {
                value.truncate(value.floor_char_boundary(3));
                let size = if value.is_empty() {
                    Some(0)
                } else {
                    value.parse::<u8>().ok()
                };
                if let Some(size) = size.filter(|&size| size <= MAX_FONT_SIZE) {
                    preferences.font_size = size;
                    self.size = None;
                } else {
                    self.size = Some(value);
                }
            }
            Edit::CursorStyle(style) => preferences.cursor_style = style,
            Edit::Color(channel, mut value) => {
                value.truncate(value.floor_char_boundary(12));
                let (color, input) = match channel {
                    Color::Foreground => (&mut preferences.foreground, &mut self.foreground),
                    Color::Background => (&mut preferences.background, &mut self.background),
                    Color::Cursor => (&mut preferences.cursor_color, &mut self.cursor),
                };
                if value.trim().is_empty() {
                    *color = None;
                    *input = None;
                } else if let Some(value) = Rgb::parse(&value) {
                    *color = Some(value);
                    *input = None;
                } else {
                    *input = Some(value);
                }
            }
        }
    }

    pub(super) fn first_problem(&self) -> Option<Setting> {
        [
            (Setting::SnippetFontSize, &self.size),
            (Setting::SnippetForeground, &self.foreground),
            (Setting::SnippetBackground, &self.background),
            (Setting::SnippetCursorColor, &self.cursor),
        ]
        .into_iter()
        .find_map(|(setting, value)| value.is_some().then_some(setting))
    }
}

#[derive(Clone, PartialEq)]
struct Family(Option<String>);

impl fmt::Display for Family {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0.as_deref().unwrap_or("Inherit terminal font"))
    }
}

#[derive(Clone, Copy, PartialEq)]
struct CursorStyle(Option<Shape>);

impl fmt::Display for CursorStyle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(shape) => fmt::Display::fmt(&shape, formatter),
            None => formatter.write_str("Inherit"),
        }
    }
}

pub(super) fn view(dialog: &Dialog) -> Element<'_, Desktop> {
    let id = dialog.id;
    let preferences = &dialog.draft.preferences.snippet_appearance;
    let editor = &dialog.snippet_appearance;
    let edit = move |edit| Desktop::Settings(Message::SnippetAppearance(id, edit));
    let families = std::iter::once(Family(None))
        .chain(
            installed_families()
                .iter()
                .cloned()
                .map(|name| Family(Some(name))),
        )
        .collect::<Vec<_>>();
    let cursor_styles = std::iter::once(CursorStyle(None))
        .chain(Shape::ALL.map(|shape| CursorStyle(Some(shape))))
        .collect::<Vec<_>>();
    let size = preferences.font_size.to_string();
    let color_row = |channel: Color, color: Option<Rgb>, input: Option<&str>| {
        let setting = channel.setting();
        let color = color.map(|color| color.to_string());
        let current = input.or(color.as_deref()).unwrap_or_default();
        highlighted(
            dialog,
            setting,
            column![
                row![
                    text(setting.label()).width(150),
                    dialog.reveal_input(
                        setting,
                        true,
                        text_input("Inherit", current)
                            .width(120)
                            .on_input(move |value| edit(Edit::Color(channel, value))),
                    ),
                    button("Inherit").style(button::secondary).on_press_maybe(
                        (color.is_some() || input.is_some())
                            .then(|| edit(Edit::Color(channel, String::new())))
                    ),
                ]
                .align_y(Alignment::Center)
                .spacing(8),
                input.map(|_| text("Enter a color as #RRGGBB.")
                    .size(13)
                    .style(text::danger)),
            ]
            .spacing(4),
        )
    };
    let appearance = dialog.draft.preferences.snippet_editor_appearance();
    column![
        text("Snippet Editor Appearance").size(18),
        text("These settings apply to snippet editors and previews. Inherited fonts and text colors follow the terminal. Inherited cursor settings follow the file editor.").size(13),
        highlighted(dialog, Setting::SnippetFontFamily, row![
            text(Setting::SnippetFontFamily.label()).width(150),
            pick_list(families, Some(Family(preferences.font_family.clone())), move |family| edit(Edit::Family(family.0)))
                .width(240),
        ].align_y(Alignment::Center).spacing(8)),
        highlighted(dialog, Setting::SnippetFontSize, column![
            row![
                text(Setting::SnippetFontSize.label()).width(150),
                dialog.reveal_input(
                    Setting::SnippetFontSize,
                    true,
                    text_input("0–72", editor.size.as_deref().unwrap_or(&size)).width(80).on_input(move |value| edit(Edit::Size(value))),
                ),
                text("0 inherits").size(13),
            ].align_y(Alignment::Center).spacing(8),
            editor.size.as_ref().map(|_| text("Enter a font size from 0 to 72.").size(13).style(text::danger)),
        ].spacing(4)),
        color_row(Color::Foreground, preferences.foreground, editor.foreground.as_deref()),
        color_row(Color::Background, preferences.background, editor.background.as_deref()),
        highlighted(dialog, Setting::SnippetCursorStyle, row![
            text(Setting::SnippetCursorStyle.label()).width(150),
            pick_list(cursor_styles, Some(CursorStyle(preferences.cursor_style)), move |style| edit(Edit::CursorStyle(style.0)))
                .width(160),
        ].align_y(Alignment::Center).spacing(8)),
        color_row(Color::Cursor, preferences.cursor_color, editor.cursor.as_deref()),
        text("Preview").size(16),
        container(text("AaBbCcDdEe 0123456789")
            .font(appearance.font)
            .size(f32::from(appearance.size))
            .color(appearance.foreground.color()))
            .padding(12)
            .style(move |_| container::Style::default().background(appearance.background.color())),
    ].spacing(10).into()
}
