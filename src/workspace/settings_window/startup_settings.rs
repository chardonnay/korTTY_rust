use std::ops::RangeInclusive;

use iced::widget::{Id, button, checkbox, column, row, text, text_input};
use iced::{Alignment, Element};

use super::super::window_lifecycle::{FixedGeometry, StartupPreferences};
use super::Setting;

#[derive(Clone, Copy, Debug)]
pub enum Field {
    Width,
    Height,
    X,
    Y,
}

impl Field {
    fn index(self) -> usize {
        match self {
            Self::Width => 0,
            Self::Height => 1,
            Self::X => 2,
            Self::Y => 3,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Width => "Width",
            Self::Height => "Height",
            Self::X => "X",
            Self::Y => "Y",
        }
    }

    fn limits(self) -> RangeInclusive<u16> {
        match self {
            Self::Width => FixedGeometry::WIDTH,
            Self::Height => FixedGeometry::HEIGHT,
            Self::X => FixedGeometry::X,
            Self::Y => FixedGeometry::Y,
        }
    }

    fn value(self, geometry: &mut FixedGeometry) -> &mut u16 {
        match self {
            Self::Width => &mut geometry.width,
            Self::Height => &mut geometry.height,
            Self::X => &mut geometry.x,
            Self::Y => &mut geometry.y,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Edit {
    MasterPassword(bool),
    RememberGeometry(bool),
    RememberDashboard(bool),
    Fixed(bool),
    Value(Field, String),
}

#[derive(Default)]
pub(super) struct Editor {
    invalid: [Option<String>; 4],
}

impl Editor {
    pub(super) fn valid(&self) -> bool {
        self.invalid.iter().all(Option::is_none)
    }

    pub(super) fn update(&mut self, preferences: &mut StartupPreferences, edit: Edit) {
        match edit {
            Edit::MasterPassword(enabled) => preferences.require_master_password = enabled,
            Edit::RememberGeometry(enabled) => preferences.remember_geometry = enabled,
            Edit::RememberDashboard(enabled) => preferences.remember_dashboard = enabled,
            Edit::Fixed(enabled) => {
                preferences.use_fixed_geometry = enabled;
                if !enabled {
                    self.invalid.fill(None);
                }
            }
            Edit::Value(field, mut input) if preferences.use_fixed_geometry => {
                if let Ok(value) = input.parse::<u16>()
                    && field.limits().contains(&value)
                {
                    *field.value(&mut preferences.fixed) = value;
                    self.invalid[field.index()] = None;
                } else {
                    input.truncate(input.floor_char_boundary(8));
                    self.invalid[field.index()] = Some(input);
                }
            }
            Edit::Value(..) => {}
        }
    }

    pub(super) fn view(
        &self,
        mut preferences: StartupPreferences,
        target: Option<&Id>,
    ) -> Element<'_, Edit> {
        let enabled = preferences.use_fixed_geometry;
        let mut field = |field: Field| {
            self.field(
                field,
                *field.value(&mut preferences.fixed),
                enabled,
                target.filter(|_| enabled && matches!(field, Field::Width)),
            )
        };
        column![
            text("Fixed Window Geometry").size(16),
            checkbox(enabled).label(Setting::FixedGeometry.label()).on_toggle(Edit::Fixed),
            row![field(Field::Width), field(Field::Height)].spacing(16),
            row![field(Field::X), field(Field::Y)].spacing(16),
            text("Applies when opening a new window. A restored project or session uses its saved layout.").size(13),
            cfg!(target_os = "linux").then(|| text("On Wayland, the compositor chooses the window position.").size(13)),
        ].spacing(8).into()
    }

    fn field(
        &self,
        field: Field,
        value: u16,
        enabled: bool,
        target: Option<&Id>,
    ) -> Element<'_, Edit> {
        let range = field.limits();
        let placeholder = format!("{}–{}", range.start(), range.end());
        let formatted = value.to_string();
        let invalid = self.invalid[field.index()].as_deref();
        column![
            row![
                text(field.label()).width(46),
                crate::settings_search::mark_input(
                    text_input(&placeholder, invalid.unwrap_or(&formatted))
                        .width(86)
                        .on_input_maybe(enabled.then_some(move |value| Edit::Value(field, value))),
                    target,
                ),
                button("−").style(button::secondary).on_press_maybe(
                    (enabled && value > *range.start())
                        .then(|| Edit::Value(field, (value - 1).to_string())),
                ),
                button("+").style(button::secondary).on_press_maybe(
                    (enabled && value < *range.end())
                        .then(|| Edit::Value(field, (value + 1).to_string())),
                ),
            ]
            .spacing(5)
            .align_y(Alignment::Center),
            invalid.map(|_| text(format!("Enter {placeholder}."))
                .size(13)
                .style(text::danger)),
        ]
        .spacing(3)
        .into()
    }
}
