//! Persisted presets for the native agent activity window and pane status.

use std::fmt;

use serde::{Deserialize, Serialize};

mod custom;
pub(crate) mod inline;
pub(crate) use custom::{
    Approval, Arrangement, Catalog, Choice, History, Layout, MAX_NAME_BYTES, Status,
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    Classic,
    Floating,
    #[default]
    #[serde(other)]
    Standard,
}

impl Preset {
    pub(crate) const ALL: [Self; 3] = [Self::Standard, Self::Classic, Self::Floating];

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Classic => "classic",
            Self::Floating => "floating",
        }
    }

    pub(crate) fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.id() == id)
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Standard => "Activity window and a status strip in the source terminal.",
            Self::Classic => "Activity window with its own controls and transcript.",
            Self::Floating => "Compact always-on-top activity window and a terminal status strip.",
        }
    }

    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl fmt::Display for Preset {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Standard => "Standard",
            Self::Classic => "Classic",
            Self::Floating => "Floating",
        })
    }
}

pub(crate) fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_design_override_and_unknown_preset_fallback() {
        let catalog = Catalog::default();
        assert_eq!(
            catalog.resolve(None, Preset::Classic),
            Preset::Classic.layout()
        );
        assert_eq!(
            catalog.resolve(Some("standard"), Preset::Classic),
            Preset::Standard.layout()
        );
        assert_eq!(
            catalog.resolve(Some("unknown"), Preset::Floating),
            Preset::Floating.layout()
        );
        for preset in Preset::ALL {
            assert_eq!(serde_json::to_value(preset).unwrap(), preset.id());
            assert_eq!(Preset::from_id(preset.id()), Some(preset));
        }
        assert_eq!(
            serde_json::from_str::<Preset>(r#""future-design""#).unwrap(),
            Preset::Standard
        );
    }
}
