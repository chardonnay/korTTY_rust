use std::ops::RangeInclusive;

use iced::widget::{checkbox, column, row, rule, space, text, text_input};
use iced::{Alignment, Element};

use crate::connection_store::WindowGeometry;
use crate::workspace::dialog_design::{keyboard_table, number_input};

const LABEL_WIDTH: f32 = 128.0;

#[derive(Clone, Debug)]
pub enum Edit {
    Custom(bool),
    Value(Field, String),
    Maximized(bool),
}

#[derive(Clone, Copy, Debug)]
pub enum Field {
    Width,
    Height,
    X,
    Y,
}

impl Field {
    fn label(self) -> &'static str {
        match self {
            Self::Width => "Width",
            Self::Height => "Height",
            Self::X => "X position",
            Self::Y => "Y position",
        }
    }

    fn bounds(self) -> RangeInclusive<u16> {
        match self {
            Self::Width => WindowGeometry::WIDTH,
            Self::Height => WindowGeometry::HEIGHT,
            Self::X => WindowGeometry::X,
            Self::Y => WindowGeometry::Y,
        }
    }
}

pub(super) struct Draft {
    custom: bool,
    fields: [String; 4],
    maximized: bool,
}

impl Draft {
    pub(super) fn new(saved: Option<WindowGeometry>) -> Self {
        let value = saved.unwrap_or_default();
        Self {
            custom: saved.is_some(),
            fields: [value.width, value.height, value.x, value.y].map(|value| value.to_string()),
            maximized: value.maximized,
        }
    }

    pub(super) fn update(&mut self, edit: Edit) -> bool {
        match edit {
            Edit::Custom(custom) if custom != self.custom => self.custom = custom,
            Edit::Value(field, value)
                if self.custom
                    && value.len() <= 4
                    && value.bytes().all(|byte| byte.is_ascii_digit())
                    && self.fields[field as usize] != value =>
            {
                self.fields[field as usize] = value;
            }
            Edit::Maximized(value) if self.custom && self.maximized != value => {
                self.maximized = value;
            }
            _ => return false,
        }
        true
    }

    pub(super) fn value(&self) -> Result<Option<WindowGeometry>, String> {
        if !self.custom {
            return Ok(None);
        }
        let field = |field: Field| {
            let bounds = field.bounds();
            self.fields[field as usize]
                .parse::<u16>()
                .ok()
                .filter(|value| bounds.contains(value))
                .ok_or_else(|| {
                    format!(
                        "{} must be between {} and {}.",
                        field.label(),
                        bounds.start(),
                        bounds.end()
                    )
                })
        };
        Ok(Some(WindowGeometry {
            width: field(Field::Width)?,
            height: field(Field::Height)?,
            x: field(Field::X)?,
            y: field(Field::Y)?,
            maximized: self.maximized,
        }))
    }

    pub(super) fn view(&self) -> Element<'_, Edit> {
        let custom = checkbox(self.custom)
            .label("Use specific window geometry for this connection")
            .on_toggle(Edit::Custom);
        let maximize = self.custom.then_some(Edit::Maximized(!self.maximized));
        let maximized = checkbox(self.maximized)
            .label("Open window maximized")
            .on_toggle_maybe(self.custom.then_some(Edit::Maximized));
        column![
            keyboard_table::action(custom.into(), Some(Edit::Custom(!self.custom)), false),
            text(
                "These settings apply when the connection opens in a separate terminal window.\nGlobal window settings apply when this option is off."
            )
            .size(13)
            .style(text::secondary),
            rule::horizontal(1),
            row![
                text("Window size").width(LABEL_WIDTH),
                self.stepper(Field::Width, "Width", ""),
                self.stepper(Field::Height, "Height", "px"),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .wrap(),
            row![
                text("Window position").width(LABEL_WIDTH),
                self.stepper(Field::X, "X", ""),
                self.stepper(Field::Y, "Y", "px"),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .wrap(),
            row![
                space::horizontal().width(LABEL_WIDTH),
                keyboard_table::action(maximized.into(), maximize, false),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .wrap(),
        ]
        .spacing(12)
        .into()
    }

    fn stepper(&self, field: Field, label: &'static str, unit: &'static str) -> Element<'_, Edit> {
        let value = &self.fields[field as usize];
        let bounds = field.bounds();
        let edit = self
            .custom
            .then_some(move |value| Edit::Value(field, value));
        row![
            text(label).size(13),
            number_input(
                text_input(field.label(), value).on_input_maybe(edit),
                value,
                i64::from(*bounds.start())..=i64::from(*bounds.end()),
                1,
                edit,
                74.0,
            ),
            (!unit.is_empty()).then(|| text(unit).size(13)),
        ]
        .spacing(5)
        .align_y(Alignment::Center)
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_steps_geometry_within_bounds_and_preserves_the_disabled_draft() {
        use iced::advanced::widget::operation::focusable;
        use iced::keyboard::{Key, key::Named};
        use iced::{Font, Renderer, Size};
        use iced_runtime::{Action, UserInterface, task, user_interface::Cache};

        use crate::settings_search::test_input;

        let mut draft = Draft::new(Some(WindowGeometry {
            width: 4000,
            height: 300,
            maximized: true,
            ..WindowGeometry::default()
        }));
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        {
            let mut interface = UserInterface::build(
                draft.view(),
                Size::new(720.0, 300.0),
                Cache::default(),
                &mut renderer,
            );
            for _ in 0..2 {
                test_input::apply(
                    &mut interface,
                    &renderer,
                    task::effect(Action::widget(focusable::focus_next())),
                );
            }
            let press = |interface: &mut _, renderer: &mut _, key| {
                test_input::press(interface, renderer, Key::Named(key), None)
            };
            assert!(press(&mut interface, &mut renderer, Named::ArrowUp).is_empty());
            assert!(matches!(
                press(&mut interface, &mut renderer, Named::ArrowDown).as_slice(),
                [Edit::Value(Field::Width, width)] if width == "3999"
            ));
            test_input::apply(
                &mut interface,
                &renderer,
                task::effect(Action::widget(focusable::focus_next())),
            );
            assert!(press(&mut interface, &mut renderer, Named::ArrowDown).is_empty());
        }
        assert!(draft.update(Edit::Custom(false)));
        assert!(!draft.update(Edit::Value(Field::Width, "500".into())));
        assert!(!draft.update(Edit::Maximized(false)));
        assert_eq!(draft.value().unwrap(), None);
        assert!(draft.update(Edit::Custom(true)));
        let geometry = draft.value().unwrap().unwrap();
        assert_eq!(geometry.width, 4000);
        assert!(geometry.maximized);
    }
}
