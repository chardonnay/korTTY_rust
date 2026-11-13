use std::ops::RangeInclusive;

use iced::Element;
use iced::widget::{checkbox, column, row, text, text_input};

use crate::connection_store::WindowGeometry;

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
    const ALL: [Self; 4] = [Self::Width, Self::Height, Self::X, Self::Y];

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
        let fields = Field::ALL.into_iter().map(|field| {
            column![
                text(field.label()).size(13),
                text_input(field.label(), &self.fields[field as usize])
                    .width(100)
                    .on_input(move |value| Edit::Value(field, value)),
            ]
            .spacing(4)
            .into()
        });
        column![
            checkbox(self.custom)
                .label("Use custom geometry in a new window")
                .on_toggle(Edit::Custom),
            self.custom.then(|| row(fields).spacing(8).wrap()),
            self.custom.then(|| checkbox(self.maximized)
                .label("Open maximized")
                .on_toggle(Edit::Maximized)),
        ]
        .spacing(7)
        .into()
    }
}
