use std::fmt;

use serde::{Deserialize, Serialize};

use super::ConflictChoice;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictDefault {
    Skip,
    Overwrite,
    #[default]
    #[serde(other)]
    Ask,
}

impl ConflictDefault {
    pub(crate) const ALL: [Self; 3] = [Self::Ask, Self::Skip, Self::Overwrite];

    pub(crate) fn from_id(id: &str) -> Option<Self> {
        match id.trim().to_ascii_lowercase().as_str() {
            "ask" => Some(Self::Ask),
            "skip" => Some(Self::Skip),
            "overwrite" => Some(Self::Overwrite),
            _ => None,
        }
    }

    pub(crate) fn most_restrictive(self, other: Self) -> Self {
        match (self, other) {
            (Self::Ask, _) | (_, Self::Ask) => Self::Ask,
            (Self::Skip, _) | (_, Self::Skip) => Self::Skip,
            _ => Self::Overwrite,
        }
    }

    pub(crate) fn is_default(&self) -> bool {
        *self == Self::Ask
    }

    pub(super) fn choice(self) -> Option<ConflictChoice> {
        match self {
            Self::Ask => None,
            Self::Skip => Some(ConflictChoice::Skip),
            Self::Overwrite => Some(ConflictChoice::Replace),
        }
    }
}

impl fmt::Display for ConflictDefault {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::Ask => "Ask",
            Self::Skip => "Skip existing entries",
            Self::Overwrite => "Replace regular files",
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    File,
    Folder,
    Link,
    TypeMismatch,
}

impl Kind {
    pub(super) const COUNT: usize = 4;
}
