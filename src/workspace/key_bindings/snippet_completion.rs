//! Portable chord for the snippet editor's completion list.

use std::fmt;

use iced::keyboard::{Key, Modifiers};
use serde::{Deserialize, Serialize};

use super::{Chord, MainKey, java};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "Chord")]
pub(crate) struct SnippetCompletionChord(Chord);

impl Default for SnippetCompletionChord {
    fn default() -> Self {
        Self(Chord {
            key: MainKey::Tab,
            shortcut: false,
            control: false,
            shift: true,
            alt: false,
        })
    }
}

impl SnippetCompletionChord {
    pub(in crate::workspace) fn from_java(value: &str) -> Option<Self> {
        Self::try_from(java::parse_completion_chord(value)?).ok()
    }

    pub(in crate::workspace) fn capture(key: &Key, modifiers: Modifiers) -> Option<Self> {
        Self::try_from(Chord::capture(key, modifiers)?).ok()
    }

    pub(crate) fn matches(self, key: &Key, modifiers: Modifiers) -> bool {
        Self::capture(key, modifiers) == Some(self)
    }
}

impl TryFrom<Chord> for SnippetCompletionChord {
    type Error = &'static str;

    fn try_from(chord: Chord) -> Result<Self, Self::Error> {
        if chord.control
            || matches!(chord.key, MainKey::Function(0 | 13..=u8::MAX))
            || matches!(chord.key, MainKey::Character(ch) if !MainKey::supported_character(ch))
        {
            return Err("This key cannot be used for a snippet completion shortcut.");
        }
        Ok(Self(chord))
    }
}

impl fmt::Display for SnippetCompletionChord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_shortcuts_use_the_platform_primary_modifier() {
        let imported = SnippetCompletionChord::from_java("alt - ctrl - k").unwrap();
        assert!(imported.matches(
            &Key::Character("k".into()),
            Modifiers::COMMAND | Modifiers::ALT
        ));
        assert!(!imported.matches(&Key::Character("k".into()), Modifiers::ALT));
        assert!(SnippetCompletionChord::from_java("Ctrl+Cmd+K").is_none());
        assert!(SnippetCompletionChord::from_java("Ctrl+InvalidKey").is_none());
        assert_eq!(
            SnippetCompletionChord::from_java("Shift+Tab"),
            Some(SnippetCompletionChord::default())
        );
    }
}
