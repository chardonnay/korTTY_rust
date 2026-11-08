use iced::widget::{button, checkbox, column, row, text, text_input};
use iced::{Alignment, Element};

use crate::connection_store::TabColor;

#[derive(Clone)]
pub(super) struct ColorDraft {
    enabled: bool,
    input: String,
}

impl ColorDraft {
    pub(super) fn new(saved: Option<TabColor>, inherited: Option<TabColor>) -> Self {
        Self {
            enabled: saved.is_some(),
            input: saved
                .or(inherited)
                .unwrap_or(TabColor::PRESETS[0].0)
                .to_string(),
        }
    }

    pub(super) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub(super) fn set_input(&mut self, input: String) -> bool {
        if !self.enabled {
            return false;
        }
        self.input = if input.len() <= 32 {
            input
        } else {
            String::new()
        };
        true
    }

    pub(super) fn value(&self) -> Result<Option<TabColor>, &'static str> {
        self.enabled
            .then(|| TabColor::parse(&self.input).ok_or("Enter a tab color as #RGB or #RRGGBB."))
            .transpose()
    }

    pub(super) fn view<'a, M: Clone + 'a>(
        &'a self,
        label: &'static str,
        enable: impl Fn(bool) -> M + 'a,
        input: impl Fn(String) -> M + 'a,
    ) -> Element<'a, M> {
        let presets =
            TabColor::PRESETS
                .into_iter()
                .fold(row![].spacing(4), |row, (color, label)| {
                    row.push(
                        button(
                            row![text("●").color(color.color()), text(label).size(12)].spacing(4),
                        )
                        .style(button::secondary)
                        .on_press_maybe(self.enabled.then(|| input(color.to_string()))),
                    )
                });
        column![
            checkbox(self.enabled).label(label).on_toggle(enable),
            row![
                text_input("#RRGGBB", &self.input)
                    .on_input_maybe(self.enabled.then_some(input))
                    .width(125),
                self.value()
                    .ok()
                    .flatten()
                    .map(|color| text("●").color(color.color())),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
            presets.wrap(),
        ]
        .spacing(6)
        .into()
    }
}
