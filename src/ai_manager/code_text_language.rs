//! Language selection for prose inside generated or rewritten scripts.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum CodeTextLanguage {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "en")]
    English,
    #[serde(rename = "de")]
    German,
    #[serde(rename = "it")]
    Italian,
    #[serde(rename = "es")]
    Spanish,
    #[serde(rename = "pt")]
    Portuguese,
    #[serde(rename = "fr")]
    French,
    #[serde(rename = "hr")]
    Croatian,
    #[serde(rename = "nl")]
    Dutch,
}

impl CodeTextLanguage {
    pub(crate) const ALL: [Self; 9] = [
        Self::Auto,
        Self::English,
        Self::German,
        Self::Italian,
        Self::Spanish,
        Self::Portuguese,
        Self::French,
        Self::Croatian,
        Self::Dutch,
    ];

    pub(crate) fn explicit_code(self) -> Option<&'static str> {
        Some(match self {
            Self::Auto => return None,
            Self::English => "en",
            Self::German => "de",
            Self::Italian => "it",
            Self::Spanish => "es",
            Self::Portuguese => "pt",
            Self::French => "fr",
            Self::Croatian => "hr",
            Self::Dutch => "nl",
        })
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Auto => "Keep script language",
            Self::English => "English",
            Self::German => "German",
            Self::Italian => "Italian",
            Self::Spanish => "Spanish",
            Self::Portuguese => "Portuguese",
            Self::French => "French",
            Self::Croatian => "Croatian",
            Self::Dutch => "Dutch",
        }
    }
}

impl fmt::Display for CodeTextLanguage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.explicit_code() {
            Some(code) => write!(formatter, "{} ({code})", self.name()),
            None => formatter.write_str(self.name()),
        }
    }
}
