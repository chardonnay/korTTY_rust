use std::fmt;

use kortty_terminal::{Color, CursorShape, Palette, Style};
use serde::{Deserialize, Serialize};

use super::{Rgb, Scheme};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BoldTextMode {
    BrightColor,
    BoldFontAndBrightColor,
    #[default]
    #[serde(other)]
    BoldFont,
}

impl BoldTextMode {
    pub(crate) const ALL: [Self; 3] = [
        Self::BoldFont,
        Self::BrightColor,
        Self::BoldFontAndBrightColor,
    ];

    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub(crate) fn uses_bold_font(self) -> bool {
        self != Self::BrightColor
    }

    pub(crate) fn foreground(self, style: Style) -> Color {
        if style.bold
            && self != Self::BoldFont
            && let Color::Indexed(index @ 0..=7) = style.foreground
        {
            Color::Indexed(index + 8)
        } else {
            style.foreground
        }
    }
}

impl fmt::Display for BoldTextMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BoldFont => "Bold font",
            Self::BrightColor => "Bright colors",
            Self::BoldFontAndBrightColor => "Bold font and bright colors",
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Colors {
    pub(crate) foreground: Option<Rgb>,
    pub(crate) background: Option<Rgb>,
    pub(crate) cursor: Option<Rgb>,
    pub(crate) cursor_blink: Option<bool>,
    pub(crate) sgr_enabled: bool,
    pub(crate) selection: Rgb,
    pub(crate) ansi: [Option<Rgb>; 16],
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            foreground: None,
            background: None,
            cursor: None,
            cursor_blink: None,
            sgr_enabled: true,
            selection: Self::DEFAULT_SELECTION,
            ansi: [None; 16],
        }
    }
}

impl Colors {
    const DEFAULT_SELECTION: Rgb = Rgb::from_hex(0x3399ff);

    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub(crate) fn appearance(self, scheme: Option<Scheme>) -> Appearance {
        let defaults = Scheme::default();
        let style = scheme.unwrap_or(defaults).cursor_style;
        let (cursor_shape, blinking) = style.behavior();
        let foreground = scheme.map(|theme| theme.foreground).or(self.foreground);
        let background = scheme.map(|theme| theme.background).or(self.background);
        let cursor = scheme.map(|theme| theme.cursor).or(self.cursor);
        let builtin = Palette::default();
        let palette = Palette::new(std::array::from_fn(|index| {
            self.ansi[index].map_or_else(|| builtin.rgb(index as u8), Rgb::bytes)
        }));
        let customized = palette != builtin || self.selection != Self::DEFAULT_SELECTION;
        Appearance {
            foreground: foreground.unwrap_or(defaults.foreground),
            background: background.unwrap_or(defaults.background),
            cursor: cursor.unwrap_or(defaults.cursor),
            cursor_shape,
            cursor_blink: self.cursor_blink.unwrap_or(blinking),
            sgr_enabled: self.sgr_enabled,
            bold_text_mode: BoldTextMode::default(),
            palette,
            selection: customized.then_some(self.selection),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Appearance {
    pub(crate) foreground: Rgb,
    pub(crate) background: Rgb,
    pub(crate) cursor: Rgb,
    pub(crate) cursor_shape: CursorShape,
    pub(crate) cursor_blink: bool,
    pub(crate) sgr_enabled: bool,
    pub(crate) bold_text_mode: BoldTextMode,
    pub(crate) palette: Palette,
    pub(crate) selection: Option<Rgb>,
}

impl Default for Appearance {
    fn default() -> Self {
        Self::from(None)
    }
}

impl From<Option<Scheme>> for Appearance {
    fn from(scheme: Option<Scheme>) -> Self {
        Colors::default().appearance(scheme)
    }
}
