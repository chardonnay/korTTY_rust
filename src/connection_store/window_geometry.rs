use std::io;
use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::profile::{SavedPosition, WindowPlacement};

/// Private desktop preference for a standalone window launched from a saved SSH entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WindowGeometry {
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) x: u16,
    pub(crate) y: u16,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) maximized: bool,
}

impl Default for WindowGeometry {
    fn default() -> Self {
        Self {
            width: 1200,
            height: 800,
            x: 100,
            y: 100,
            maximized: false,
        }
    }
}

impl WindowGeometry {
    pub(crate) const WIDTH: RangeInclusive<u16> = 400..=4000;
    pub(crate) const HEIGHT: RangeInclusive<u16> = 300..=3000;
    pub(crate) const X: RangeInclusive<u16> = 0..=5000;
    pub(crate) const Y: RangeInclusive<u16> = 0..=3000;

    pub(super) fn validate(self) -> io::Result<()> {
        if Self::WIDTH.contains(&self.width)
            && Self::HEIGHT.contains(&self.height)
            && Self::X.contains(&self.x)
            && Self::Y.contains(&self.y)
        {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid saved window geometry.",
        ))
    }

    pub(crate) fn placement(self) -> WindowPlacement {
        WindowPlacement {
            width: f32::from(self.width),
            height: f32::from(self.height),
            position: SavedPosition::new(f32::from(self.x), f32::from(self.y)),
        }
    }
}
