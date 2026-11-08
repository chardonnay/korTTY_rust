use std::sync::LazyLock;

use iced::widget::{Button, button, row, svg, text};
use iced::{Alignment, Theme};

use super::Message;

#[derive(Clone, Copy)]
pub(super) enum Icon {
    Browse,
    Refresh,
    Play,
    Rename,
    Delete,
    Export,
    Save,
}

// Filled glyphs from the Java video manager.
static ICONS: LazyLock<[svg::Handle; 7]> = LazyLock::new(|| {
    [
        "M10 4l2 2h8c.55 0 1 .45 1 1v11c0 .55-.45 1-1 1H4c-.55 0-1-.45-1-1V5c0-.55.45-1 1-1h6z",
        "M17.65 6.35C16.2 4.9 14.21 4 12 4c-4.42 0-8 3.58-8 8s3.58 8 8 8c3.73 0 6.84-2.55 7.73-6h-2.08c-.82 2.33-3.04 4-5.65 4-3.31 0-6-2.69-6-6s2.69-6 6-6c1.66 0 3.14.69 4.22 1.78L13 11h8V3z",
        "M8 5v14l11-7z",
        "M3 17.25V21h3.75L17.81 9.94l-3.75-3.75L3 17.25zM20.71 7.04c.39-.39.39-1.02 0-1.41l-2.34-2.34c-.39-.39-1.02-.39-1.41 0l-1.83 1.83 3.75 3.75z",
        "M6 19c0 1.1.9 2 2 2h8c1.1 0 2-.9 2-2V7H6v12zM8 9h8v10H8V9zm7.5-5l-1-1h-5l-1 1H5v2h14V4z",
        "M14 3h7v7h-2V6.41l-9.29 9.3-1.42-1.42 9.3-9.29H14V3zM5 5h7v2H7v10h10v-5h2v7H5z",
        "M17 3H5c-1.11 0-2 .9-2 2v14c0 1.1.89 2 2 2h14c1.1 0 2-.9 2-2V7l-4-4zM12 19c-1.66 0-3-1.34-3-3s1.34-3 3-3 3 1.34 3 3-1.34 3-3 3zM15 9H5V5h10z",
    ]
    .map(|path| {
        svg::Handle::from_memory(
            format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="{path}"/></svg>"#)
                .into_bytes(),
        )
    })
});

impl Icon {
    pub(super) fn button(
        self,
        label: &'static str,
        action: Option<Message>,
    ) -> Button<'static, Message> {
        let primary = matches!(self, Self::Save);
        let enabled = action.is_some();
        let icon = svg::Svg::new(ICONS[self as usize].clone())
            .width(15)
            .height(15)
            .style(move |theme: &Theme, _| {
                let palette = theme.extended_palette();
                let mut color = if primary {
                    palette.primary.base.text
                } else {
                    palette.secondary.base.text
                };
                if !enabled {
                    color.a *= 0.5;
                }
                svg::Style { color: Some(color) }
            });
        button(
            row![icon, text(label)]
                .spacing(6)
                .align_y(Alignment::Center),
        )
        .style(if primary {
            button::primary
        } else {
            button::secondary
        })
        .on_press_maybe(action)
    }
}
