//! Local token counts for reviewed request forecasts.

use std::fmt;

use serde::{Deserialize, Serialize};
use tiktoken_rs::{
    cl100k_base_singleton, o200k_base_singleton, p50k_base_singleton, r50k_base_singleton,
};

const CHAT_MESSAGE_OVERHEAD: u64 = 12;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Tokenizer {
    #[default]
    Estimate,
    Cl100kBase,
    O200kBase,
    P50kBase,
    R50kBase,
}

impl Tokenizer {
    pub(crate) const ALL: [Self; 5] = [
        Self::Estimate,
        Self::Cl100kBase,
        Self::O200kBase,
        Self::P50kBase,
        Self::R50kBase,
    ];

    pub(crate) fn is_default(&self) -> bool {
        *self == Self::Estimate
    }

    pub(crate) fn count_messages<'a>(self, parts: impl IntoIterator<Item = &'a str>) -> u64 {
        parts.into_iter().fold(CHAT_MESSAGE_OVERHEAD, |sum, part| {
            sum.saturating_add(self.count_text(part))
        })
    }

    pub(crate) fn count_text(self, text: &str) -> u64 {
        match self {
            Self::Estimate => estimate_text(text),
            Self::Cl100kBase => cl100k_base_singleton().count_ordinary(text) as u64,
            Self::O200kBase => o200k_base_singleton().count_ordinary(text) as u64,
            Self::P50kBase => p50k_base_singleton().count_ordinary(text) as u64,
            Self::R50kBase => r50k_base_singleton().count_ordinary(text) as u64,
        }
    }
}

impl fmt::Display for Tokenizer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Estimate => "Estimate",
            Self::Cl100kBase => "CL100K Base",
            Self::O200kBase => "O200K Base",
            Self::P50kBase => "P50K Base",
            Self::R50kBase => "R50K Base",
        })
    }
}

fn estimate_text(text: &str) -> u64 {
    let words = text.split_whitespace().count() as u64;
    if words == 0 {
        return 0;
    }
    let by_character = (text.chars().count() as u64).div_ceil(4);
    let by_word = words.saturating_mul(135).div_ceil(100);
    by_character.max(by_word)
}

#[cfg(test)]
mod tests {
    use super::Tokenizer;

    #[test]
    fn encodings_count_unicode_as_literal_input() {
        let phrase = ["お誕生日おめでとう"];
        assert_eq!(Tokenizer::Cl100kBase.count_messages(phrase), 21);
        assert_eq!(Tokenizer::O200kBase.count_messages(phrase), 20);
        assert_eq!(Tokenizer::P50kBase.count_messages(phrase), 26);
        assert_eq!(Tokenizer::R50kBase.count_messages(phrase), 26);
        assert!(Tokenizer::O200kBase.count_messages(["<|endoftext|>"]) > 13);
    }
}
