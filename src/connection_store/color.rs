use std::fmt;

use iced::Color;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) struct TabColor(u8, u8, u8);

impl TabColor {
    pub(crate) const PRESETS: [(Self, &str); 7] = [
        (Self(0xD3, 0x2F, 0x2F), "Red"),
        (Self(0xF5, 0x7C, 0x00), "Orange"),
        (Self(0xFB, 0xC0, 0x2D), "Yellow"),
        (Self(0x38, 0x8E, 0x3C), "Green"),
        (Self(0x19, 0x76, 0xD2), "Blue"),
        (Self(0x7B, 0x1F, 0xA2), "Purple"),
        (Self(0x61, 0x61, 0x61), "Gray"),
    ];

    pub(crate) fn parse(value: &str) -> Option<Self> {
        fn digit(byte: u8) -> Option<u8> {
            match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                b'A'..=b'F' => Some(byte - b'A' + 10),
                _ => None,
            }
        }

        match value.trim().strip_prefix('#')?.as_bytes() {
            [r, g, b] => Some(Self(digit(*r)? * 17, digit(*g)? * 17, digit(*b)? * 17)),
            [r1, r2, g1, g2, b1, b2] => Some(Self(
                digit(*r1)? * 16 + digit(*r2)?,
                digit(*g1)? * 16 + digit(*g2)?,
                digit(*b1)? * 16 + digit(*b2)?,
            )),
            _ => None,
        }
    }

    pub(crate) fn color(self) -> Color {
        Color::from_rgb8(self.0, self.1, self.2)
    }
}

impl TryFrom<String> for TabColor {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value).ok_or("invalid tab color")
    }
}

impl From<TabColor> for String {
    fn from(value: TabColor) -> Self {
        value.to_string()
    }
}

impl fmt::Display for TabColor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "#{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }
}
