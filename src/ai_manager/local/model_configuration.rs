//! Shared window frame and numeric controls for local model configuration.

use std::ops::RangeInclusive;

use iced::widget::{Id, column, container, row, scrollable, space, text, text_input};
use iced::{Alignment, Element, Length};

use crate::workspace::dialog_design::number_step;

use super::Message;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Gguf,
    Mlx,
}

pub(super) fn window<'a>(
    fields: Element<'a, Message>,
    viewport: Id,
    status: Option<&'a str>,
    save: Element<'a, Message>,
    cancel: Element<'a, Message>,
) -> Element<'a, Message> {
    container(
        column![
            text("Model runtime and automatic unload settings").size(16),
            scrollable(fields).id(viewport).height(Length::Fill),
            status.map(|status| text(status).size(13).wrapping(text::Wrapping::WordOrGlyph)),
            row![space::horizontal().width(Length::Fill), save, cancel,].spacing(8),
        ]
        .spacing(10),
    )
    .padding(14)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

pub(super) fn number<'a>(
    input: text_input::TextInput<'a, Message>,
    value: &str,
    bounds: RangeInclusive<i64>,
    step: i64,
    on_step: Option<fn(String) -> Message>,
) -> Element<'a, Message> {
    let adjustment = |delta: i64| {
        let update = on_step?;
        let value: i64 = value.trim().parse().ok()?;
        if !bounds.contains(&value) {
            return None;
        }
        let next = (value + delta).clamp(*bounds.start(), *bounds.end());
        (next != value).then(|| update(next.to_string()))
    };
    row![
        input.width(106),
        column![
            number_step("▴", adjustment(step)),
            number_step("▾", adjustment(-step)),
        ]
    ]
    .spacing(3)
    .align_y(Alignment::Center)
    .into()
}
