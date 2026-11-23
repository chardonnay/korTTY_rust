//! Shared window frame and numeric controls for local model configuration.

use std::ops::RangeInclusive;

use iced::advanced::graphics::text::Paragraph;
use iced::advanced::text::{self as layout_text, Paragraph as _};
use iced::widget::{Id, column, container, row, scrollable, space, text, text_input};
use iced::{Alignment, Element, Font, Length, Pixels, Size};

use crate::workspace::dialog_design::number_step;

use super::Message;

pub(super) const GGUF_MINIMUM_PREFERRED_WIDTH: f32 = 668.0;
const PADDING: f32 = 14.0;
const PATH_FONT_SIZE: f32 = 13.0;

pub(super) fn gguf_preferred_width(path: &str) -> f32 {
    let paragraph = Paragraph::with_text(layout_text::Text {
        content: path,
        bounds: Size::INFINITE,
        size: Pixels(PATH_FONT_SIZE),
        line_height: layout_text::LineHeight::default(),
        font: Font::DEFAULT,
        align_x: layout_text::Alignment::Default,
        align_y: iced::alignment::Vertical::Top,
        shaping: layout_text::Shaping::Advanced,
        wrapping: layout_text::Wrapping::None,
    });
    // Include enough space for the label, window padding and vertical scrollbar.
    let frame = super::FORM_LABEL_WIDTH + super::FORM_COLUMN_GAP + 2.0 * PADDING + 16.0;
    (paragraph.min_width() + frame)
        .ceil()
        .max(GGUF_MINIMUM_PREFERRED_WIDTH)
}

pub(super) fn gguf_path<'a>(path: String) -> Element<'a, Message> {
    text(path)
        .size(PATH_FONT_SIZE)
        .shaping(text::Shaping::Advanced)
        .wrapping(text::Wrapping::WordOrGlyph)
        .width(Length::Fill)
        .into()
}

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
    .padding(PADDING)
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

#[cfg(test)]
mod tests {
    use iced::Renderer;
    use iced::advanced::{layout, widget::Tree};

    use super::*;

    #[test]
    fn gguf_preferred_width_fits_long_paths_and_the_path_wraps_in_a_smaller_window() {
        let short = r"C:\Models\test.gguf";
        let long = format!(r"C:\Models\{}test.gguf", "registered models\\".repeat(12));
        assert_eq!(gguf_preferred_width(short), GGUF_MINIMUM_PREFERRED_WIDTH);
        let preferred = gguf_preferred_width(&long);
        assert!(preferred > GGUF_MINIMUM_PREFERRED_WIDTH);

        let renderer = Renderer::new(Font::DEFAULT, Pixels(14.0));
        let path_height = |path: &str, width: f32| {
            let mut field = super::super::field("GGUF file", gguf_path(path.into()));
            let mut tree = Tree::new(field.as_widget());
            let max = Size::new(width - 2.0 * PADDING - 16.0, f32::INFINITY);
            let node = field.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, max),
            );
            node.children()[1].bounds().height
        };
        let single_line = path_height(short, preferred);
        assert_eq!(path_height(&long, preferred), single_line);
        assert!(path_height(&long, 460.0) > single_line);
    }
}
