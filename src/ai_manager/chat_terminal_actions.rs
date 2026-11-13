//! User preference for terminal actions on AI chat code blocks.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChatTerminalActions {
    Off,
    InsertOnly,
    #[default]
    InsertAndRun,
}

impl ChatTerminalActions {
    pub(super) const ALL: [Self; 3] = [Self::Off, Self::InsertOnly, Self::InsertAndRun];

    pub(crate) fn allows_insert(self) -> bool {
        self != Self::Off
    }

    pub(crate) fn allows_run(self) -> bool {
        self == Self::InsertAndRun
    }
}

impl<'de> Deserialize<'de> for ChatTerminalActions {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = Option::<String>::deserialize(deserializer)?.unwrap_or_default();
        value.make_ascii_lowercase();
        Ok(match value.trim() {
            "off" => Self::Off,
            "insert_only" => Self::InsertOnly,
            "insert_and_run" => Self::InsertAndRun,
            _ => Self::default(),
        })
    }
}

impl fmt::Display for ChatTerminalActions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Off => "Off (copy and save only)",
            Self::InsertOnly => "Insert only",
            Self::InsertAndRun => "Insert and Run",
        })
    }
}
