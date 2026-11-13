use iced::widget::{
    button, canvas, checkbox, column, container, responsive, row, rule, scrollable, space, stack,
    text,
};
use iced::{
    Alignment, Element, Font, Length, Pixels, Point, Rectangle, Renderer, Size, Theme, mouse,
    window,
};

use crate::terminal_effects::{Effect, Preferences, Selection, Speed};

pub(super) const TITLE: &str = "Terminal Effects";
const ACTIVE_WIDTH: f32 = 76.0;
const NAME_WIDTH: f32 = 144.0;
const PREVIEW_WIDTH: f32 = 360.0;
const PREVIEW_HEIGHT: f32 = 220.0;

#[derive(Clone, Debug)]
pub(crate) enum Message {
    Open(window::Id),
    Window(window::Id, Action),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Action {
    Opened,
    Close,
    Select(Effect),
    SetEnabled(bool),
    SetEffectEnabled(Effect, bool),
}

pub(super) struct Manager {
    pub(super) owner: window::Id,
    effects: Vec<Effect>,
    selected: Option<Effect>,
    error: Option<String>,
}

impl Manager {
    pub(super) fn new(owner: window::Id) -> Self {
        let mut effects = Effect::ALL.to_vec();
        effects.sort_unstable_by_key(|effect| effect.name());
        Self {
            owner,
            effects,
            selected: None,
            error: None,
        }
    }

    pub(super) fn select(&mut self, effect: Effect) {
        self.selected = Some(effect);
    }

    pub(super) fn set_error(&mut self, error: Option<String>) {
        self.error = error;
    }

    pub(super) fn is_previewing(&self, preferences: &Preferences) -> bool {
        self.selected_preview(preferences).is_some()
    }

    pub(super) fn view<'a>(
        &'a self,
        preferences: &'a Preferences,
        clock: f32,
    ) -> Element<'a, Action> {
        responsive(move |size| self.sized_view(preferences, clock, size)).into()
    }

    fn sized_view<'a>(
        &'a self,
        preferences: &'a Preferences,
        clock: f32,
        size: Size,
    ) -> Element<'a, Action> {
        let compact = size.width < 780.0;
        let active_width = if compact { 54.0 } else { ACTIVE_WIDTH };
        let name_width = if compact {
            Length::Fill
        } else {
            Length::Fixed(NAME_WIDTH)
        };
        let preview_width = if compact {
            (size.width * 0.49).clamp(124.0, PREVIEW_WIDTH)
        } else {
            PREVIEW_WIDTH
        };
        let preview_height = (size.height - 100.0).clamp(82.0, PREVIEW_HEIGHT);
        let rows = column(self.effects.iter().map(|&effect| {
            self.effect_row(preferences, effect, active_width, name_width, !compact)
        }))
        .spacing(2);
        let mut heading = row![
            container(text("Active").size(13)).center_x(active_width),
            text("Name").size(13).width(name_width),
        ];
        if !compact {
            heading = heading.push(text("Description").size(13));
        }
        let table = column![
            heading,
            rule::horizontal(1),
            scrollable(rows).height(Length::Fill),
        ]
        .width(Length::Fill)
        .spacing(6);

        let preview: Element<'_, Action> = match self.selected_preview(preferences) {
            Some(effect) => stack![
                canvas(Preview { effect, clock })
                    .width(preview_width)
                    .height(preview_height),
                Selection {
                    effect,
                    speed: Speed::DEFAULT
                }
                .overlay(clock),
            ]
            .clip(true)
            .into(),
            None => container(text("Select an effect to see its preview.").size(14))
                .center_x(preview_width)
                .center_y(preview_height)
                .style(container::bordered_box)
                .into(),
        };
        let mut body = column![
            checkbox(preferences.enabled)
                .label("Enable terminal effects")
                .on_toggle(Action::SetEnabled),
            row![table, column![text("Preview").size(14), preview].spacing(6)]
                .height(Length::Fill)
                .spacing(12),
        ]
        .spacing(10)
        .padding(12)
        .height(Length::Fill);
        if let Some(error) = &self.error {
            body = body.push(text(error.as_str()).size(13));
        }
        body.push(row![
            space::horizontal(),
            button("Close").on_press(Action::Close)
        ])
        .into()
    }

    fn selected_preview(&self, preferences: &Preferences) -> Option<Effect> {
        // Individual activation controls live panes. A disabled effect can still be previewed.
        self.selected.filter(|_| preferences.enabled)
    }

    fn effect_row(
        &self,
        preferences: &Preferences,
        effect: Effect,
        active_width: f32,
        name_width: Length,
        show_description: bool,
    ) -> Element<'_, Action> {
        let active = checkbox(preferences.effect_enabled(effect))
            .size(16)
            .on_toggle_maybe(
                preferences
                    .enabled
                    .then_some(move |enabled| Action::SetEffectEnabled(effect, enabled)),
            );
        let selected = preferences.enabled && self.selected == Some(effect);
        let mut label = row![text(effect.name()).size(14).width(name_width)];
        if show_description {
            label = label.push(text(effect.description()).size(13).width(Length::Fill));
        }
        let select = button(label.align_y(Alignment::Center))
            .padding([7, 4])
            .width(Length::Fill)
            .style(if selected {
                button::primary
            } else {
                button::text
            })
            .on_press_maybe(preferences.enabled.then_some(Action::Select(effect)));
        row![container(active).center_x(active_width), select]
            .align_y(Alignment::Center)
            .into()
    }
}

struct Preview {
    effect: Effect,
    clock: f32,
}

impl canvas::Program<Action> for Preview {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let (background, foreground) = self.effect.colors();
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), background);
        let prompt = if self.clock.rem_euclid(1.1) < 0.55 {
            "$ ▉"
        } else {
            "$"
        };
        for (index, line) in [
            "kortty terminal preview",
            "$ echo 'Native terminal effects'",
            "Native terminal effects",
            prompt,
        ]
        .into_iter()
        .enumerate()
        {
            frame.fill_text(canvas::Text {
                content: line.to_owned(),
                position: Point::new(16.0, 21.0 + index as f32 * 23.0),
                font: Font::MONOSPACE,
                size: Pixels(14.0),
                color: foreground,
                ..canvas::Text::default()
            });
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_requires_selection_and_global_activation() {
        let mut manager = Manager::new(window::Id::unique());
        let mut preferences = Preferences::default();
        assert!(!manager.is_previewing(&preferences));

        let effect = Effect::ALL[0];
        manager.select(effect);
        assert!(manager.is_previewing(&preferences));
        preferences.set_effect_enabled(effect, false);
        assert!(!preferences.allows(effect));
        assert!(manager.is_previewing(&preferences));
        preferences.enabled = false;
        assert!(!manager.is_previewing(&preferences));
        preferences.enabled = true;
        assert!(manager.is_previewing(&preferences));
    }
}
