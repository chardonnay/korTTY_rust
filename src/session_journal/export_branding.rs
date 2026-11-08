//! Profile choices shared by the session journal export formats.

use serde::{Deserialize, Serialize};

use crate::terminal_theme::Rgb;

use super::is_display_character;

pub(crate) const REPOSITORY_URL: &str = "https://github.com/chardonnay/kortty";
pub(crate) const DEFAULT_WATERMARK: &str = "kortty — Developed by Daniel Mengel";
pub(crate) const DEFAULT_FOOTER: &str = "Created with kortty — Developed by Daniel Mengel";
const DEFAULT_COLOR: Rgb = Rgb::from_bytes([0x6b, 0x72, 0x80]);
const MAX_TEXT_BYTES: usize = 512;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct Preferences {
    pub watermark_enabled: bool,
    pub watermark_text: String,
    pub watermark_color: Rgb,
    pub footer_enabled: bool,
    pub footer_text: String,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            watermark_enabled: false,
            watermark_text: String::new(),
            watermark_color: DEFAULT_COLOR,
            footer_enabled: true,
            footer_text: String::new(),
        }
    }
}

impl Preferences {
    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub(crate) fn resolve(&self) -> PdfBranding {
        PdfBranding {
            watermark: self.watermark_enabled.then(|| {
                let cleaned = brand_input(&self.watermark_text);
                let text = cleaned.trim();
                let repository_link = text.is_empty() || text == DEFAULT_WATERMARK;
                Watermark {
                    brand: BrandText {
                        text: if text.is_empty() {
                            DEFAULT_WATERMARK.into()
                        } else {
                            text.into()
                        },
                        repository_link,
                    },
                    color: self.watermark_color.bytes(),
                }
            }),
            footer: self.footer_enabled.then(|| {
                let cleaned = brand_input(&self.footer_text);
                let text = cleaned.trim();
                let repository_link = text.is_empty();
                BrandText {
                    text: if repository_link {
                        format!("{DEFAULT_FOOTER}  ·  {REPOSITORY_URL}")
                    } else {
                        text.into()
                    },
                    repository_link,
                }
            }),
        }
    }
}

pub(crate) fn brand_input(value: &str) -> String {
    value[..value.floor_char_boundary(MAX_TEXT_BYTES)]
        .chars()
        .filter(|&character| is_display_character(character) && !character.is_control())
        .collect()
}

#[derive(Clone, Debug)]
pub(crate) struct PdfBranding {
    pub footer: Option<BrandText>,
    pub watermark: Option<Watermark>,
}

impl Default for PdfBranding {
    fn default() -> Self {
        Preferences::default().resolve()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct BrandText {
    pub text: String,
    pub repository_link: bool,
}

impl BrandText {
    pub(crate) fn footer_label(&self) -> &str {
        if self.repository_link {
            DEFAULT_FOOTER
        } else {
            &self.text
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Watermark {
    pub brand: BrandText,
    pub color: [u8; 3],
}
