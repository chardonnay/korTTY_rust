use serde::{Deserialize, Serialize};

use crate::editor_window::{Appearance, caret};
use crate::terminal_theme::{Rgb, installed_font};

pub(crate) const MAX_FONT_SIZE: u8 = 72;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Preferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) font_family: Option<String>,
    pub(crate) font_size: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) foreground: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) background: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cursor_style: Option<caret::Shape>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cursor_color: Option<Rgb>,
}

impl Preferences {
    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub(crate) fn normalize(&mut self) {
        self.font_size = self.font_size.min(MAX_FONT_SIZE);
        self.font_family = self
            .font_family
            .take()
            .filter(|name| !name.trim().is_empty() && !name.chars().any(char::is_control));
    }

    pub(crate) fn resolve(&self, inherited: Appearance) -> Appearance {
        Appearance {
            font: self
                .font_family
                .as_deref()
                .and_then(installed_font)
                .unwrap_or(inherited.font),
            size: if self.font_size == 0 {
                inherited.size
            } else {
                self.font_size.min(MAX_FONT_SIZE)
            },
            foreground: self.foreground.unwrap_or(inherited.foreground),
            background: self.background.unwrap_or(inherited.background),
        }
    }

    pub(crate) fn resolve_caret(&self, inherited: caret::Preferences) -> caret::Preferences {
        caret::Preferences {
            shape: self.cursor_style.unwrap_or(inherited.shape),
            color: self.cursor_color.unwrap_or(inherited.color),
        }
    }
}
