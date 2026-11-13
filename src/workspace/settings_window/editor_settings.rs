use iced::widget::{button, column, container, pick_list, row, text, text_input};
use iced::{Alignment, Element};

use crate::editor_window::caret::{Preferences, Shape};
use crate::terminal_theme::Rgb;

use super::{Desktop, Dialog, Message, Setting, highlighted};

#[derive(Clone, Debug)]
pub enum Edit {
    Shape(Shape),
    Color(String),
    ResetColor,
}

pub(super) fn update(dialog: &mut Dialog, edit: Edit) {
    let preferences = &mut dialog.draft.preferences.editor_caret;
    match edit {
        Edit::Shape(shape) => preferences.shape = shape,
        Edit::Color(mut value) => {
            value.truncate(value.floor_char_boundary(12));
            if let Some(color) = Rgb::parse(&value) {
                preferences.color = color;
                dialog.editor_cursor_color_input = None;
            } else {
                dialog.editor_cursor_color_input = Some(value);
            }
        }
        Edit::ResetColor => {
            preferences.color = Preferences::default().color;
            dialog.editor_cursor_color_input = None;
        }
    }
}

pub(super) fn view(dialog: &Dialog) -> Element<'_, Desktop> {
    let id = dialog.id;
    let edit = move |edit| Desktop::Settings(Message::EditorCaret(id, edit));
    let preferences = dialog.draft.preferences.editor_caret;
    let color = preferences.color.to_string();
    let input = dialog.editor_cursor_color_input.as_deref();
    column![
        text("File Editor Appearance").size(18),
        text("File editors use the terminal font and colors. Choose a cursor style and color for their text area.").size(13),
        highlighted(dialog, Setting::EditorCursorStyle, row![
            text(Setting::EditorCursorStyle.label()).width(120),
            pick_list(Shape::ALL, Some(preferences.shape), move |shape| edit(Edit::Shape(shape))).width(160),
        ].spacing(8).align_y(Alignment::Center)),
        highlighted(dialog, Setting::EditorCursorColor, column![
            row![
                text(Setting::EditorCursorColor.label()).width(120),
                dialog.reveal_input(
                    Setting::EditorCursorColor,
                    true,
                    text_input("#RRGGBB", input.unwrap_or(&color)).width(120).on_input(move |value| edit(Edit::Color(value))),
                ),
                container(text("")).width(20).height(20).style(move |_| container::Style::default().background(preferences.color.color())),
                button("Reset color").style(button::secondary).on_press_maybe(
                    (input.is_some() || preferences.color != Preferences::default().color).then_some(edit(Edit::ResetColor))
                ),
            ].spacing(8).align_y(Alignment::Center).wrap(),
            input.map(|_| text("Enter a color as #RRGGBB.").size(13).style(text::danger)),
        ].spacing(4)),
    ].spacing(10).into()
}
