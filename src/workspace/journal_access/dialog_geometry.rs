//! Dialog placement fallbacks stored in the general window settings.

use iced::Size;
use serde::{Deserialize, Serialize};

use crate::profile::{SavedPosition, WindowPlacement};

const MAX_SIZE: u16 = 4096;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(in crate::workspace) struct Geometry {
    width: u16,
    height: u16,
    position: Option<[i32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scale_milli: Option<u16>,
}

impl Geometry {
    pub(in crate::workspace) fn valid_imported_bounds(self) -> bool {
        (100..=MAX_SIZE).contains(&self.width)
            && (100..=MAX_SIZE).contains(&self.height)
            && self.scale_milli.is_none()
            && self
                .position
                .is_none_or(|[x, y]| SavedPosition::new(x as f32, y as f32).is_some())
    }

    pub(in crate::workspace) fn from_java(
        width: f64,
        height: f64,
        position: Option<[i32; 2]>,
    ) -> Option<Self> {
        let dimension = |value: f64| {
            (value.is_finite() && value >= 100.0)
                .then(|| value.min(f64::from(MAX_SIZE)).round() as u16)
        };
        Some(Self {
            width: dimension(width)?,
            height: dimension(height)?,
            position,
            scale_milli: None,
        })
    }

    pub(in crate::workspace) fn placement(self, minimum: Size) -> WindowPlacement {
        let position = self.position.and_then(|[x, y]| {
            let mut saved = SavedPosition::new(x as f32, y as f32)?;
            saved.scale_factor = self
                .scale_milli
                .filter(|scale| (250..=8000).contains(scale))
                .map(|scale| f32::from(scale) / 1000.0);
            Some(saved)
        });
        WindowPlacement {
            width: f32::from(self.width).clamp(minimum.width, f32::from(MAX_SIZE)),
            height: f32::from(self.height).clamp(minimum.height, f32::from(MAX_SIZE)),
            position,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_dialog_geometry_converts_legacy_positions_and_optional_scale() {
        let minimum = Size::new(500.0, 400.0);
        let unscaled: Geometry =
            serde_json::from_str(r#"{"width":900,"height":700,"position":[-600,85]}"#).unwrap();
        let placement = unscaled.placement(minimum);
        assert_eq!((placement.width, placement.height), (900.0, 700.0));
        let saved = placement.position.unwrap();
        assert_eq!((saved.x, saved.y, saved.scale_factor), (-600.0, 85.0, None));

        let scaled: Geometry = serde_json::from_str(
            r#"{"width":5200,"height":200,"position":[-600,85],"scale_milli":1500}"#,
        )
        .unwrap();
        let placement = scaled.placement(minimum);
        assert_eq!((placement.width, placement.height), (4096.0, 400.0));
        assert_eq!(placement.position.unwrap().scale_factor, Some(1.5));
    }
}
