//! Persistent placement of an inline snippet analysis.

use serde::{Deserialize, Serialize};

use crate::horizontal_divider;

pub(crate) const MIN_WIDTH: u16 = 360;
pub(crate) const MAX_WIDTH: u16 = 1600;
const DEFAULT_WIDTH: u16 = 520;
const MIN_EDITOR_WIDTH: f32 = 260.0;

pub(crate) fn remaining_space(available: f32, width: f32) -> f32 {
    available - MIN_EDITOR_WIDTH - horizontal_divider::WIDTH - width
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct Preferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) width: Option<u16>,
}

impl Preferences {
    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub(crate) fn visible(self) -> bool {
        self.visible.unwrap_or_default()
    }

    pub(crate) fn fitted_width(self, available: f32) -> Option<f32> {
        let maximum = available - MIN_EDITOR_WIDTH - horizontal_divider::WIDTH;
        (maximum >= f32::from(MIN_WIDTH)).then(|| {
            f32::from(
                self.width
                    .unwrap_or(DEFAULT_WIDTH)
                    .clamp(MIN_WIDTH, MAX_WIDTH),
            )
            .min(maximum)
        })
    }

    pub(crate) fn resized_width(self, width: f32, available: f32) -> Option<u16> {
        if !width.is_finite() || !available.is_finite() {
            return None;
        }
        let current = self.fitted_width(available)?.round();
        let maximum =
            (available - MIN_EDITOR_WIDTH - horizontal_divider::WIDTH).min(f32::from(MAX_WIDTH));
        let resized = width.clamp(f32::from(MIN_WIDTH), maximum).round();
        (resized != current).then_some(resized as u16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_narrow_editor_falls_back_without_replacing_the_requested_width() {
        let panel = Preferences {
            visible: Some(true),
            width: Some(750),
        };
        assert_eq!(panel.fitted_width(590.0), None);
        assert_eq!(panel.fitted_width(700.0), Some(432.0));
        assert_eq!(panel.resized_width(490.0, 700.0), None);
        assert_eq!(panel.fitted_width(1200.0), Some(750.0));
        assert_eq!(panel.resized_width(397.0, 700.0), Some(397));
    }
}
