use std::ops::RangeInclusive;
use std::sync::LazyLock;

use iced::advanced::text::{self as layout_text, Paragraph as _};
use iced::theme::Base as _;
use iced::{
    Alignment, Color, Element, Font, Length, Pixels, Size, Theme, border,
    widget::{button, column, container, row, rule, stack, text, text_input},
};
use iced_tiny_skia::font_scale::Paragraph;

pub(crate) use super::dialog_placement::escape::on_key_press;
pub(crate) mod keyboard_table;

pub(crate) const TAB_LABEL_SIZE: f32 = 13.0;

pub(crate) fn tab_label<'a>(
    label: impl iced::widget::text::IntoFragment<'a>,
) -> iced::widget::Text<'a> {
    text(label)
        .size(TAB_LABEL_SIZE)
        .shaping(layout_text::Shaping::Advanced)
        .wrapping(layout_text::Wrapping::None)
}

pub(crate) fn tab_label_width(label: &str) -> f32 {
    // Measure labels inside the same font-scale scope as their actual widgets.
    Paragraph::with_text(layout_text::Text {
        content: label,
        bounds: Size::INFINITE,
        size: Pixels(TAB_LABEL_SIZE),
        line_height: layout_text::LineHeight::default(),
        font: Font::DEFAULT,
        align_x: layout_text::Alignment::Default,
        align_y: iced::alignment::Vertical::Top,
        shaping: layout_text::Shaping::Advanced,
        wrapping: layout_text::Wrapping::None,
    })
    .min_width()
}

const NAME: &str = "Normal Dialog";
const PAGE: Color = Color::from_rgb8(30, 30, 30);

static NORMAL: LazyLock<Theme> = LazyLock::new(|| {
    Theme::custom(
        NAME.to_owned(),
        iced::theme::Palette {
            background: Color::from_rgb8(44, 44, 44),
            text: Color::from_rgb8(230, 230, 230),
            primary: Color::from_rgb8(0, 102, 204),
            ..Theme::Dark.palette()
        },
    )
});

pub(super) fn normal_theme() -> Theme {
    NORMAL.clone()
}

pub(super) fn page_color(theme: &Theme) -> Color {
    if theme.name() == NAME {
        PAGE
    } else {
        theme.palette().background
    }
}

pub(crate) fn page(theme: &Theme) -> container::Style {
    container::Style::default().background(page_color(theme))
}

pub(crate) fn sidebar(theme: &Theme) -> container::Style {
    let background = if theme.name() == NAME {
        Color::from_rgb8(37, 37, 37)
    } else {
        theme.extended_palette().background.weakest.color
    };
    container::Style::default().background(background)
}

pub(crate) fn header(theme: &Theme) -> container::Style {
    if theme.name() == NAME {
        container::Style::default().background(Color::from_rgb8(60, 60, 60))
    } else {
        container::secondary(theme)
    }
}

pub(crate) fn table_row(theme: &Theme, selected: bool, alternate: bool) -> container::Style {
    if theme.name() == NAME {
        let gray = match (selected, alternate) {
            (true, _) => 55,
            (false, true) => 40,
            (false, false) => 37,
        };
        container::Style::default().background(Color::from_rgb8(gray, gray, gray))
    } else if selected {
        container::secondary(theme)
    } else {
        sidebar(theme)
    }
}

pub(crate) fn form_input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let mut style = text_input::default(theme, status);
    if theme.name() == NAME {
        style.background = PAGE.into();
        if !matches!(status, text_input::Status::Focused { .. }) {
            style.border.color = Color::from_rgb8(72, 72, 72);
        }
    }
    style
}

pub(crate) fn tab(theme: &Theme, status: button::Status, selected: bool) -> button::Style {
    let mut style = button::text(theme, status);
    if selected {
        style.background = Some(page_color(theme).into());
        style.border.radius = 0.0.into();
    }
    style
}

pub(crate) fn primary_tab<'a, Message: Clone + 'a>(
    title: &'a str,
    selected: bool,
    action: Message,
) -> Element<'a, Message> {
    let label = text(title).size(13).font(Font {
        weight: if selected {
            iced::font::Weight::Bold
        } else {
            iced::font::Weight::Normal
        },
        ..Font::DEFAULT
    });
    let item = button(label)
        .padding([9, 15])
        .style(move |theme, status| tab(theme, status, selected))
        .on_press(action);
    if !selected {
        return item.into();
    }
    let line = rule::horizontal(3).style(|theme: &Theme| rule::Style {
        color: theme.palette().primary,
        ..rule::default(theme)
    });
    stack![item, container(line).align_bottom(Length::Fill)].into()
}

pub(crate) fn secondary_button(theme: &Theme, status: button::Status) -> button::Style {
    let mut style = button::secondary(theme, status);
    if theme.name() == NAME {
        let gray = match status {
            button::Status::Active => 62,
            button::Status::Hovered => 77,
            button::Status::Pressed => 53,
            button::Status::Disabled => 53,
        };
        style.background = Some(Color::from_rgb8(gray, gray, gray).into());
        style.text_color = if status == button::Status::Disabled {
            Color::from_rgb8(122, 122, 122)
        } else {
            theme.palette().text
        };
    }
    style
}

pub(crate) fn secondary_action<'a, Message: Clone + 'a>(
    label: &'a str,
) -> button::Button<'a, Message> {
    button(text(label).size(13))
        .padding([6, 10])
        .style(secondary_button)
}

pub(crate) fn number_step<'a, Message: Clone + 'a>(
    symbol: &'a str,
    action: Option<Message>,
) -> button::Button<'a, Message> {
    button(
        text(symbol)
            .size(10)
            .line_height(text::LineHeight::Absolute(13.0.into())),
    )
    .padding([0, 6])
    .width(24)
    .style(secondary_button)
    .on_press_maybe(action)
}

pub(crate) fn number_input<'a, Message: Clone + 'a>(
    input: text_input::TextInput<'a, Message>,
    value: &str,
    bounds: RangeInclusive<i64>,
    step: i64,
    on_step: Option<fn(String) -> Message>,
    input_width: f32,
) -> Element<'a, Message> {
    let adjustment = |delta: i64| {
        let update = on_step?;
        let value: i64 = value.trim().parse().ok()?;
        if !bounds.contains(&value) {
            return None;
        }
        let next = value
            .saturating_add(delta)
            .clamp(*bounds.start(), *bounds.end());
        (next != value).then(|| update(next.to_string()))
    };
    let input = input
        .size(13)
        .line_height(text::LineHeight::Absolute(16.0.into()))
        .padding([7, 5])
        .width(input_width)
        .style(|theme, status| {
            let mut style = form_input(theme, status);
            style.border.radius = style.border.radius.right(0);
            style
        });
    let increment = number_step("▴", adjustment(step))
        .padding([1, 6])
        .width(26)
        .style(|theme, status| {
            let mut style = secondary_button(theme, status);
            style.border.radius = border::top_right(style.border.radius.top_right);
            style
        });
    let decrement = number_step("▾", adjustment(-step))
        .padding([1, 6])
        .width(26)
        .style(|theme, status| {
            let mut style = secondary_button(theme, status);
            style.border.radius = border::bottom_right(style.border.radius.bottom_right);
            style
        });
    row![input, column![increment, decrement]]
        .align_y(Alignment::Center)
        .into()
}

pub(crate) fn segment(theme: &Theme, status: button::Status, selected: bool) -> button::Style {
    let mut style = secondary_button(theme, status);
    if theme.name() == NAME && status != button::Status::Disabled {
        let gray = match (selected, status) {
            (_, button::Status::Pressed) => 192,
            (_, button::Status::Hovered) => 238,
            (true, _) => 222,
            (false, _) => 202,
        };
        style.background = Some(Color::from_rgb8(gray, gray, gray).into());
        style.text_color = Color::from_rgb8(29, 29, 29);
    }
    if selected {
        style.border.width = 1.0;
        style.border.color = theme.palette().primary;
    }
    style
}
