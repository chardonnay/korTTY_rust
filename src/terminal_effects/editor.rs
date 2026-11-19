use std::fmt;

use iced::widget::{column, pick_list, row, slider, text, text_input};
use iced::{Alignment, Element, Length};

use crate::terminal_effects::{Effect, Selection, Speed};

pub(crate) struct EffectDraft {
    pub(crate) effect: Option<Effect>,
    speed: Speed,
    speed_input: String,
}

impl Default for EffectDraft {
    fn default() -> Self {
        Self::new(None)
    }
}

impl EffectDraft {
    pub(crate) fn new(selection: Option<Selection>) -> Self {
        Self::with_default(selection, Speed::DEFAULT)
    }

    pub(crate) fn with_default(selection: Option<Selection>, default: Speed) -> Self {
        let speed = selection.map_or(default, |selection| selection.speed);
        Self {
            effect: selection.map(|selection| selection.effect),
            speed,
            speed_input: speed.to_string(),
        }
    }

    pub(crate) fn set_speed(&mut self, value: String) {
        if value.len() <= 24
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
            && value.bytes().filter(|&byte| byte == b'.').count() <= 1
        {
            if let Some(speed) = parse_speed(&value) {
                self.speed = speed;
            }
            self.speed_input = value;
        }
    }

    pub(crate) fn selection(&self) -> Option<Selection> {
        self.effect.map(|effect| Selection {
            effect,
            speed: self.speed,
        })
    }

    pub(crate) fn view<M: Clone + 'static>(
        &self,
        available: &[Effect],
        select: fn(Option<Effect>) -> M,
        set_speed: fn(String) -> M,
    ) -> Element<'_, M> {
        let choices: Vec<_> = std::iter::once(EffectChoice(None))
            .chain(available.iter().map(|&effect| EffectChoice(Some(effect))))
            .collect();
        let effect = pick_list(choices, Some(EffectChoice(self.effect)), move |choice| {
            select(choice.0)
        })
        .width(Length::Fill);
        let mut section = column![effect].spacing(6);

        if let Some(effect) = self.effect {
            if available.contains(&effect) {
                let speed = self.speed;
                section = section.push(text(format!("Animation Speed · {speed:.2}x")).size(14));
                section = section.push(
                    row![
                        slider(
                            1.0..=10.0,
                            speed.multiplier().min(10.0),
                            move |speed: f64| { set_speed(speed.to_string()) }
                        ),
                        text_input("1–99", &self.speed_input)
                            .on_input(set_speed)
                            .on_submit(set_speed(speed.to_string()))
                            .width(84),
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

fn parse_speed(value: &str) -> Option<Speed> {
    Speed::try_from(value.parse::<f64>().ok()?).ok()
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct EffectChoice(Option<Effect>);

impl fmt::Display for EffectChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0.map_or("None", Effect::name))
    }
}
