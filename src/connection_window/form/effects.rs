use std::fmt;

use iced::widget::{column, pick_list, row, slider, text, text_input};
use iced::{Alignment, Element, Length};

use crate::terminal_effects::{Effect, Selection};

use super::{Edit, Message};

pub(super) struct EffectDraft {
    pub(super) effect: Option<Effect>,
    speed: u8,
    speed_input: String,
}

impl Default for EffectDraft {
    fn default() -> Self {
        Self::new(None)
    }
}

impl EffectDraft {
    pub(super) fn new(selection: Option<Selection>) -> Self {
        let speed = selection.map_or(1, Selection::speed);
        Self {
            effect: selection.map(|selection| selection.effect),
            speed,
            speed_input: speed.to_string(),
        }
    }

    pub(super) fn set_speed(&mut self, value: String) {
        if value.len() <= 2 && value.bytes().all(|byte| byte.is_ascii_digit()) {
            if let Some(speed) = parse_speed(&value) {
                self.speed = speed;
            }
            self.speed_input = value;
        }
    }

    pub(super) fn selection(&self) -> Option<Selection> {
        self.effect.map(|effect| Selection {
            effect,
            speed: self.speed,
        })
    }

    pub(super) fn view(&self, available: &[Effect]) -> Element<'_, Message> {
        let choices: Vec<_> = std::iter::once(EffectChoice(None))
            .chain(available.iter().map(|&effect| EffectChoice(Some(effect))))
            .collect();
        let effect = pick_list(choices, Some(EffectChoice(self.effect)), |choice| {
            Message::Edit(Edit::TerminalEffect(choice.0))
        })
        .width(Length::Fill);
        let mut section = column![text("Terminal Effect").size(16), effect].spacing(6);

        if let Some(effect) = self.effect {
            if available.contains(&effect) {
                let speed = self.speed;
                section = section.push(text(format!("Animation Speed · {speed}.00x")).size(14));
                section = section.push(
                    row![
                        slider(1..=10, speed.min(10), |speed: u8| {
                            Message::Edit(Edit::TerminalEffectSpeed(speed.to_string()))
                        }),
                        text_input("1–99", &self.speed_input)
                            .on_input(|value| Message::Edit(Edit::TerminalEffectSpeed(value)))
                            .on_submit(Message::Edit(Edit::TerminalEffectSpeed(speed.to_string())))
                            .width(56),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                );
                if parse_speed(&self.speed_input).is_none() {
                    section = section.push(text("Enter a speed from 1 to 99.").size(13));
                }
            } else {
                section = section.push(text("This saved effect is disabled.").size(13));
            }
        }
        section.into()
    }
}

fn parse_speed(value: &str) -> Option<u8> {
    value.parse().ok().filter(|speed| (1..=99).contains(speed))
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct EffectChoice(Option<Effect>);

impl fmt::Display for EffectChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0.map_or("None", Effect::name))
    }
}
