use std::ops::RangeInclusive;

use iced::widget::{Id, checkbox, column, container, row, text, text_input};
use iced::{Alignment, Element};

use super::super::dialog_design::{self, keyboard_table};
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
    fn setting(self) -> Setting {
        match self {
            Self::Width => Setting::FixedWidth,
            Self::Height => Setting::FixedHeight,
            Self::X => Setting::FixedX,
            Self::Y => Setting::FixedY,
        }
    }

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
            Self::X => "X Position",
            Self::Y => "Y Position",
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
    pub(super) fn first_problem(&self) -> Option<Setting> {
        [Field::Width, Field::Height, Field::X, Field::Y]
            .into_iter()
            .find(|field| self.invalid[field.index()].is_some())
            .map(Field::setting)
    }

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

    pub(super) fn view<'a>(
        &'a self,
        preferences: StartupPreferences,
        target: Option<(Setting, &Id)>,
        highlight: &Id,
    ) -> Element<'a, Edit> {
        let enabled = preferences.use_fixed_geometry;
        let control_id = |setting, editable| {
            target
                .filter(|(selected, _)| editable && *selected == setting)
                .map_or_else(
                    || Id::new(super::window_page::control_name(setting)),
                    |(_, id)| id.clone(),
                )
        };
        let mark = |setting, content: Element<'a, Edit>| -> Element<'a, Edit> {
            let width = if matches!(
                setting,
                Setting::FixedWidth | Setting::FixedHeight | Setting::FixedX | Setting::FixedY
            ) {
                iced::Length::Shrink
            } else {
                iced::Length::Fill
            };
            let row = container(content).width(width).padding(4);
            if target.is_some_and(|(selected, _)| selected == setting) {
                row.id(highlight.clone()).style(container::secondary).into()
            } else {
                row.into()
            }
        };
        let field = |field: Field| {
            let id = target
                .filter(|(selected, _)| enabled && *selected == field.setting())
                .map(|(_, id)| id);
            mark(
                field.setting(),
                self.field(field, field.value(&preferences.fixed), enabled, id),
            )
        };
        let fixed = keyboard_table::action_with_id(
            checkbox(enabled)
                .label(Setting::FixedGeometry.label())
                .size(18)
                .text_size(13)
                .spacing(8)
                .on_toggle(Edit::Fixed)
                .into(),
            Some(Edit::Fixed(!enabled)),
            false,
            Some(control_id(Setting::FixedGeometry, true)),
        );
        let maximized = keyboard_table::action_with_id(
            checkbox(preferences.fixed.maximized)
                .label(Setting::FixedMaximized.label())
                .size(18)
                .text_size(13)
                .spacing(8)
                .on_toggle_maybe(enabled.then_some(Edit::Maximize))
                .into(),
            enabled.then_some(Edit::Maximize(!preferences.fixed.maximized)),
            false,
            Some(control_id(Setting::FixedMaximized, enabled)),
        );
        column![
            text("Fixed Window Geometry").size(14).font(dialog_design::ui_font(iced::font::Weight::Bold)),
            mark(Setting::FixedGeometry, fixed),
            row![text("Size:").size(13).width(128), field(Field::Width), field(Field::Height)]
                .spacing(16).align_y(Alignment::Center).wrap(),
            row![text("Position:").size(13).width(128), field(Field::X), field(Field::Y)]
                .spacing(16).align_y(Alignment::Center).wrap(),
            mark(Setting::FixedMaximized, maximized),
            text("Applies when opening a new window. A restored project or session uses its saved layout.").size(12).style(text::secondary),
            cfg!(target_os = "linux").then(|| text("On Wayland, the compositor chooses the window position.").size(12)),
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
        let value = invalid.unwrap_or(&formatted);
        let edit = enabled.then_some(move |value| Edit::Value(field, value));
        let input = crate::settings_search::mark_input(
            text_input(&placeholder, value).on_input_maybe(edit),
            target,
        );
        let input = dialog_design::number_input(
            input,
            value,
            i64::from(*range.start())..=i64::from(*range.end()),
            1,
            edit,
            76.0,
        );
        column![
            row![text(field.label()).size(13).width(68), input]
                .spacing(5)
                .align_y(Alignment::Center),
            invalid.map(|_| text(format!("Enter {placeholder}."))
                .size(12)
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
    #[test]
    fn geometry_fields_share_rows_when_wide_and_wrap_inside_a_narrow_view() {
        use iced::advanced::{
            Layout, layout,
            widget::{Operation, Tree},
        };
        use iced::{Font, Rectangle, Renderer, Size};
        struct Bounds {
            id: Id,
            found: Option<Rectangle>,
        }
        impl Operation for Bounds {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
                if id == Some(&self.id) {
                    self.found = Some(bounds);
                }
            }
        }
        let editor = Editor::default();
        let preferences = StartupPreferences::default();
        let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let measure = |width, setting| {
            let highlight = Id::unique();
            let input = Id::unique();
            let mut view = editor.view(preferences, Some((setting, &input)), &highlight);
            let mut tree = Tree::new(view.as_widget());
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(width, 700.0)),
            );
            let mut bounds = Bounds {
                id: highlight,
                found: None,
            };
            view.as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut bounds);
            bounds.found.unwrap()
        };
        for width in [620.0, 430.0] {
            let fields = [
                Setting::FixedWidth,
                Setting::FixedHeight,
                Setting::FixedX,
                Setting::FixedY,
            ]
            .map(|setting| measure(width, setting));
            assert!(
                fields
                    .iter()
                    .all(|bounds| bounds.x >= 0.0 && bounds.x + bounds.width <= width)
            );
            if width > 600.0 {
                assert_eq!(fields[0].y, fields[1].y);
                assert_eq!(fields[2].y, fields[3].y);
                assert!(fields[0].y < fields[2].y);
            } else {
                assert!(fields[0].y < fields[1].y);
                assert!(fields[2].y < fields[3].y);
            }
        }
    }
}
