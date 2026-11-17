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

    fn limits(self) -> RangeInclusive<i32> {
        match self {
            Self::Width => {
                i32::from(*FixedGeometry::WIDTH.start())..=i32::from(*FixedGeometry::WIDTH.end())
            }
            Self::Height => {
                i32::from(*FixedGeometry::HEIGHT.start())..=i32::from(*FixedGeometry::HEIGHT.end())
            }
            Self::X | Self::Y => FixedGeometry::POSITION,
        }
    }

    fn value(self, geometry: &FixedGeometry) -> i32 {
        match self {
            Self::Width => geometry.width.into(),
            Self::Height => geometry.height.into(),
            Self::X => geometry.x,
            Self::Y => geometry.y,
        }
    }

    fn set(self, geometry: &mut FixedGeometry, value: i32) {
        match self {
            Self::Width => geometry.width = value as u16,
            Self::Height => geometry.height = value as u16,
            Self::X => geometry.x = value,
            Self::Y => geometry.y = value,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Edit {
    MasterPassword(bool),
    RememberGeometry(bool),
    RememberDashboard(bool),
    Fixed(bool),
    Maximize(bool),
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
                if let Ok(value) = input.parse::<i32>()
                    && field.limits().contains(&value)
                {
                    field.set(&mut preferences.fixed, value);
                    self.invalid[field.index()] = None;
                } else {
                    input.truncate(input.floor_char_boundary(8));
                    self.invalid[field.index()] = Some(input);
                }
            }
            Edit::Maximize(enabled) if preferences.use_fixed_geometry => {
                preferences.fixed.maximized = enabled;
            }
            Edit::Value(..) | Edit::Maximize(_) => {}
        }
    }

    pub(super) fn view(
        &self,
        preferences: StartupPreferences,
        target: Option<&Id>,
    ) -> Element<'_, Edit> {
        let enabled = preferences.use_fixed_geometry;
        let field = |field: Field| {
            self.field(
                field,
                field.value(&preferences.fixed),
                enabled,
                target.filter(|_| enabled && matches!(field, Field::Width)),
            )
        };
        column![
            text("Fixed Window Geometry").size(16),
            checkbox(enabled).label(Setting::FixedGeometry.label()).on_toggle(Edit::Fixed),
            row![field(Field::Width), field(Field::Height)].spacing(16),
            row![field(Field::X), field(Field::Y)].spacing(16),
            checkbox(preferences.fixed.maximized)
                .label("Start maximized")
                .on_toggle_maybe(enabled.then_some(Edit::Maximize)),
            text("Applies when opening a new window. A restored project or session uses its saved layout.").size(13),
            cfg!(target_os = "linux").then(|| text("On Wayland, the compositor chooses the window position.").size(13)),
        ].spacing(8).into()
    }

    fn field(
        &self,
        field: Field,
        value: i32,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_geometry_editor_accepts_signed_positions_and_optional_maximization() {
        let mut editor = Editor::default();
        let mut preferences = StartupPreferences::default();
        editor.update(&mut preferences, Edit::Maximize(true));
        assert!(!preferences.fixed.maximized);
        editor.update(&mut preferences, Edit::Fixed(true));
        editor.update(&mut preferences, Edit::Value(Field::X, "-1240".into()));
        editor.update(&mut preferences, Edit::Value(Field::Height, "-1".into()));
        assert!(!editor.valid());
        assert_eq!(preferences.fixed.height, FixedGeometry::default().height);
        editor.update(&mut preferences, Edit::Value(Field::Height, "900".into()));
        editor.update(&mut preferences, Edit::Maximize(true));
        assert!(editor.valid());
        assert_eq!(preferences.fixed.x, -1240);
        assert_eq!(preferences.fixed.height, 900);
        assert!(preferences.fixed.maximized);
    }
}
