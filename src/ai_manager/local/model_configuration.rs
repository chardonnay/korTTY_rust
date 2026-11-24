//! Shared window frame and numeric controls for local model configuration.

use std::ops::RangeInclusive;

use iced::advanced::graphics::text::Paragraph;
use iced::advanced::text::{self as layout_text, Paragraph as _};
use iced::widget::{Id, column, container, row, scrollable, space, text, text_input};
use iced::{Alignment, Element, Font, Length, Pixels, Size};

use crate::workspace::dialog_design::{header, number_input};

use super::Message;

pub(super) const GGUF_MINIMUM_PREFERRED_WIDTH: f32 = 460.0;
const LABEL_WIDTH: f32 = 134.0;
const COLUMN_GAP: f32 = 10.0;
const FORM_PADDING: f32 = 8.0;
const SCROLLBAR_SPACE: f32 = 16.0;
const FORM_FONT_SIZE: f32 = 13.0;

pub(super) fn gguf_preferred_width(path: &str) -> f32 {
    let paragraph = Paragraph::with_text(layout_text::Text {
        content: path,
        bounds: Size::INFINITE,
        size: Pixels(FORM_FONT_SIZE),
        line_height: layout_text::LineHeight::default(),
        font: Font::DEFAULT,
        align_x: layout_text::Alignment::Default,
        align_y: iced::alignment::Vertical::Top,
        shaping: layout_text::Shaping::Advanced,
        wrapping: layout_text::Wrapping::None,
    });
    // Include enough space for the label, window padding and vertical scrollbar.
    let frame = LABEL_WIDTH + COLUMN_GAP + 2.0 * FORM_PADDING + SCROLLBAR_SPACE;
    (paragraph.min_width() + frame)
        .ceil()
        .max(GGUF_MINIMUM_PREFERRED_WIDTH)
}

pub(super) fn gguf_field<'a>(
    label: &'static str,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    row![
        text(label).size(FORM_FONT_SIZE).width(LABEL_WIDTH),
        control.into()
    ]
    .align_y(Alignment::Center)
    .spacing(COLUMN_GAP)
    .into()
}

pub(super) fn gguf_path<'a>(path: String) -> Element<'a, Message> {
    text(path)
        .size(FORM_FONT_SIZE)
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
    column![
        container(text("Model runtime and automatic unload settings").size(15))
            .padding([12, 12])
            .width(Length::Fill)
            .style(header),
        container(scrollable(fields).id(viewport).height(Length::Fill))
            .padding(FORM_PADDING)
            .width(Length::Fill)
            .height(Length::Fill),
        status.map(
            |status| container(text(status).size(13).wrapping(text::Wrapping::WordOrGlyph))
                .padding([0, 12])
        ),
        container(row![space::horizontal().width(Length::Fill), save, cancel].spacing(10))
            .padding([8, 12]),
    ]
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
    number_input(input, value, bounds, step, on_step, 128.0)
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
            let mut field = gguf_field("GGUF file", gguf_path(path.into()));
            let mut tree = Tree::new(field.as_widget());
            let max = Size::new(width - 2.0 * FORM_PADDING - SCROLLBAR_SPACE, f32::INFINITY);
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

    #[test]
    fn compact_number_arrows_fit_flush_against_the_input() {
        let renderer = Renderer::new(Font::DEFAULT, Pixels(14.0));
        let mut number = number(text_input("", "4096"), "4096", 512..=2_097_152, 512, None);
        let mut tree = Tree::new(number.as_widget());
        let layout = number.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(288.0, f32::INFINITY)),
        );
        let input = layout.children()[0].bounds();
        let arrows = layout.children()[1].bounds();

        assert_eq!(arrows.x, input.x + input.width);
        assert_eq!((arrows.y, arrows.height), (input.y, input.height));
    }
}
