use std::fmt;

use iced::widget::{Id, column, pick_list, row, slider, text, text_input};
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

    pub(crate) fn connection_view<M: Clone + 'static>(
        &self,
        available: &[Effect],
        picker_id: Id,
        select: fn(Option<Effect>) -> M,
        set_speed: fn(String) -> M,
    ) -> Element<'_, M> {
        let enabled = self
            .effect
            .is_some_and(|effect| available.contains(&effect));
        let choices: Vec<_> = std::iter::once(EffectChoice(None))
            .chain(available.iter().map(|&effect| EffectChoice(Some(effect))))
            .collect();
        let picker = pick_list(choices, Some(EffectChoice(self.effect)), move |choice| {
            select(choice.0)
        })
        .text_size(13)
        .width(220)
        .keyboard_navigation(picker_id);
        let speed = self.speed;
        let scale = slider(
            1.0..=10.0,
            speed.multiplier().min(10.0),
            move |value: f64| set_speed(value.to_string()),
        )
        .width(180)
        .disabled(!enabled)
        .style(move |theme: &iced::Theme, status| {
            let mut style = slider::default(theme, status);
            if !enabled {
                let muted = theme.extended_palette().background.strong.color;
                style.rail.backgrounds = (muted.into(), muted.into());
                style.handle.background = muted.into();
                style.handle.border_color = muted;
            }
            style
        });
        let controls = column![
            text(format!("Animation Speed: {speed:.2}x"))
                .size(12)
                .style(text::secondary),
            row![
                scale,
                text_input("1–99", &self.speed_input)
                    .size(13)
                    .on_input_maybe(enabled.then_some(set_speed))
                    .on_submit_maybe(enabled.then(|| set_speed(speed.to_string())))
                    .width(68)
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            (enabled && parse_speed(&self.speed_input).is_none())
                .then(|| text("Enter a speed from 1 to 99.").size(12)),
        ]
        .spacing(5);
        column![
            row![text("Terminal Effect").size(13).width(100), picker]
                .spacing(8)
                .align_y(Alignment::Center),
            row![text("Animation Speed").size(13).width(100), controls]
                .spacing(8)
                .align_y(Alignment::Center),
            self.effect
                .filter(|effect| !available.contains(effect))
                .map(|_| text("This saved effect is disabled.").size(12)),
        ]
        .spacing(10)
        .into()
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

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::clipboard;
    use iced::{Event, Font, Point, Renderer, Size, mouse};
    use iced_runtime::{UserInterface, user_interface::Cache};

    #[test]
    fn disabled_speed_slider_ignores_input_and_enabled_slider_changes_value() {
        for disabled in [true, false] {
            let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
            let control: slider::Slider<'_, f64, f64, iced::Theme> =
                slider(1.0..=10.0, 1.0, |value: f64| value)
                    .width(180)
                    .disabled(disabled);
            let mut interface: UserInterface<'_, f64, iced::Theme, Renderer> = UserInterface::build(
                control,
                Size::new(180.0, 30.0),
                Cache::default(),
                &mut renderer,
            );
            let mut messages = Vec::new();
            interface.update(
                &[
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                ],
                mouse::Cursor::Available(Point::new(160.0, 10.0)),
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            if disabled {
                assert!(messages.is_empty());
            } else {
                assert!(messages.iter().any(|value| *value > 1.0));
            }
        }
    }

    #[test]
    fn connection_effect_picker_can_be_opened_and_selected_with_the_keyboard() {
        use crate::settings_search::test_input;
        use iced::advanced::widget::operation::focusable;
        use iced::keyboard::{Key, key::Named};
        use iced_runtime::{Action, task};
        #[derive(Clone, Debug)]
        enum Event {
            Select(Option<Effect>),
            Speed,
        }
        let draft = EffectDraft::default();
        let id = Id::unique();
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut interface = UserInterface::build(
            draft.connection_view(&Effect::ALL, id.clone(), Event::Select, |_| Event::Speed),
            Size::new(450.0, 180.0),
            Cache::default(),
            &mut renderer,
        );
        test_input::apply(
            &mut interface,
            &renderer,
            task::effect(Action::widget(focusable::focus(id))),
        );
        assert!(
            test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(Named::Space),
                None
            )
            .is_empty()
        );
        assert!(
            test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(Named::ArrowDown),
                None
            )
            .is_empty()
        );
        let messages = test_input::press(
            &mut interface,
            &mut renderer,
            Key::Named(Named::Enter),
            None,
        );
        assert!(matches!(
            messages.as_slice(),
            [Event::Select(Some(Effect::AmberCrt90))]
        ));
    }
}
