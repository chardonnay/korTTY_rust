//! User-owned combinations of native terminal Agent presentation modules.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use super::Preset;

const MAX_DESIGNS: usize = 32;
pub(crate) const MAX_NAME_BYTES: usize = 100;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum History {
    StepCards,
    #[serde(rename = "d02")]
    Timeline,
    #[serde(rename = "d03")]
    InlineBlocks,
    #[serde(rename = "d06")]
    PlanExecution,
    #[serde(rename = "d07")]
    CardStack,
    #[serde(rename = "d09")]
    Tmux,
    #[serde(rename = "d13")]
    Conversation,
    #[serde(rename = "d16")]
    LogTable,
    #[serde(rename = "d17")]
    Focus,
    #[default]
    #[serde(other)]
    Transcript,
}

impl History {
    pub(crate) const ALL: [Self; 10] = [
        Self::Transcript,
        Self::StepCards,
        Self::Timeline,
        Self::InlineBlocks,
        Self::PlanExecution,
        Self::CardStack,
        Self::Tmux,
        Self::Conversation,
        Self::LogTable,
        Self::Focus,
    ];

    pub(crate) fn inline_style(self) -> Option<super::inline::Style> {
        match self {
            Self::InlineBlocks => Some(super::inline::Style::Inline),
            Self::Focus => Some(super::inline::Style::Focus),
            _ => None,
        }
    }
}

impl fmt::Display for History {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Transcript => "Transcript",
            Self::StepCards => "Step cards",
            Self::Timeline => "Side timeline",
            Self::InlineBlocks => "In-terminal blocks",
            Self::PlanExecution => "Plan and execution",
            Self::CardStack => "Card stack",
            Self::Tmux => "tmux pane",
            Self::Conversation => "Conversation",
            Self::LogTable => "Log table",
            Self::Focus => "Terminal focus mode",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    Hidden,
    Pill,
    #[serde(rename = "d08")]
    Metro,
    #[serde(rename = "d01")]
    Powerline,
    #[serde(rename = "d11")]
    Minimap,
    #[serde(rename = "d12")]
    TopBar,
    #[serde(rename = "d18")]
    Neon,
    #[serde(rename = "d04")]
    Hud,
    #[serde(rename = "d05")]
    Spotlight,
    #[serde(rename = "d10")]
    Toasts,
    #[serde(rename = "d15")]
    Orbit,
    #[default]
    #[serde(other)]
    Steps,
}

impl Status {
    pub(crate) const ALL: [Self; 12] = [
        Self::Steps,
        Self::Pill,
        Self::Hidden,
        Self::Metro,
        Self::Powerline,
        Self::Minimap,
        Self::TopBar,
        Self::Neon,
        Self::Hud,
        Self::Spotlight,
        Self::Toasts,
        Self::Orbit,
    ];
}

impl fmt::Display for Status {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Steps => "Terminal step strip",
            Self::Pill => "Terminal status pill",
            Self::Hidden => "No terminal status",
            Self::Metro => "Metro line",
            Self::Powerline => "Powerline status",
            Self::Minimap => "Minimap rail",
            Self::TopBar => "Top bar with drawer",
            Self::Neon => "Neon ticker",
            Self::Hud => "Floating HUD",
            Self::Spotlight => "Spotlight and pill",
            Self::Toasts => "Toasts",
            Self::Orbit => "Orbit",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Approval {
    AboveActivity,
    #[serde(rename = "d14")]
    Cursor,
    #[default]
    #[serde(other)]
    BelowActivity,
}

impl Approval {
    pub(crate) const ALL: [Self; 3] = [Self::BelowActivity, Self::AboveActivity, Self::Cursor];
}

impl fmt::Display for Approval {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AboveActivity => "Above activity",
            Self::BelowActivity => "Below activity",
            Self::Cursor => "At the terminal cursor",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Arrangement {
    Floating,
    #[serde(rename = "d19")]
    PictureInPicture,
    #[serde(rename = "d20")]
    Adaptive,
    #[default]
    #[serde(other)]
    Normal,
}

impl Arrangement {
    pub(crate) const ALL: [Self; 4] = [
        Self::Normal,
        Self::Floating,
        Self::PictureInPicture,
        Self::Adaptive,
    ];
}

impl fmt::Display for Arrangement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Normal => "Activity window",
            Self::Floating => "Compact always-on-top window",
            Self::PictureInPicture => "Picture in picture",
            Self::Adaptive => "Adaptive terminal dock",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct Layout {
    pub(crate) history: History,
    pub(crate) status: Status,
    pub(crate) approval: Approval,
    pub(crate) arrangement: Arrangement,
}

impl Preset {
    pub(crate) fn layout(self) -> Layout {
        match self {
            Self::Standard => Layout::default(),
            Self::Classic => Layout {
                status: Status::Hidden,
                ..Layout::default()
            },
            Self::Floating => Layout {
                arrangement: Arrangement::Floating,
                ..Layout::default()
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Combination {
    id: String,
    name: String,
    layout: Layout,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Choice {
    pub(crate) id: String,
    pub(crate) name: String,
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct Catalog {
    last_id: u64,
    default_id: Option<String>,
    designs: Vec<Combination>,
}

impl<'de> Deserialize<'de> for Catalog {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct Stored {
            last_id: u64,
            default_id: Option<String>,
            designs: Vec<Combination>,
        }
        let stored = Stored::deserialize(deserializer)?;
        let mut catalog = Self {
            last_id: stored.last_id,
            ..Self::default()
        };
        for mut design in stored.designs {
            let Some(number) = design
                .id
                .strip_prefix("custom-")
                .and_then(|suffix| suffix.parse::<u64>().ok())
                .filter(|&number| number != 0 && design.id == format!("custom-{number}"))
            else {
                continue;
            };
            catalog.last_id = catalog.last_id.max(number);
            design.name = design.name.trim().to_owned();
            if catalog.designs.len() < MAX_DESIGNS
                && catalog.available_name(&design.name, None)
                && catalog.find(&design.id).is_none()
            {
                catalog.designs.push(design);
            }
        }
        if let Some(id) = stored.default_id {
            catalog.set_custom_default(&id);
        }
        Ok(catalog)
    }
}

impl Catalog {
    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }

    fn find(&self, id: &str) -> Option<&Combination> {
        self.designs.iter().find(|design| design.id == id)
    }

    pub(crate) fn layout(&self, id: &str) -> Option<Layout> {
        Preset::from_id(id)
            .map(Preset::layout)
            .or_else(|| self.find(id).map(|design| design.layout))
    }

    pub(crate) fn resolve(&self, connection: Option<&str>, global: Preset) -> Layout {
        connection
            .and_then(|id| self.layout(id))
            .or_else(|| self.default_id.as_deref().and_then(|id| self.layout(id)))
            .unwrap_or_else(|| global.layout())
    }

    pub(crate) fn default_id(&self, builtin: Preset) -> &str {
        self.default_id.as_deref().unwrap_or_else(|| builtin.id())
    }

    pub(crate) fn set_custom_default(&mut self, id: &str) -> bool {
        if self.find(id).is_none() {
            return false;
        }
        self.default_id = Some(id.to_owned());
        true
    }

    pub(crate) fn clear_custom_default(&mut self) {
        self.default_id = None;
    }

    pub(crate) fn choices(&self) -> Vec<Choice> {
        Preset::ALL
            .into_iter()
            .map(|preset| Choice {
                id: preset.id().to_owned(),
                name: preset.to_string(),
            })
            .chain(self.designs.iter().map(|design| Choice {
                id: design.id.clone(),
                name: design.name.clone(),
            }))
            .collect()
    }

    pub(crate) fn connection_choices(&self, global: Preset) -> Vec<Choice> {
        let mut choices = self.choices();
        let default = self.default_id(global);
        let name = choices
            .iter()
            .find(|choice| choice.id == default)
            .map_or("Standard", |choice| &choice.name);
        choices.insert(
            0,
            Choice {
                id: String::new(),
                name: format!("Use default ({name})"),
            },
        );
        choices
    }

    pub(crate) fn can_duplicate(&self) -> bool {
        self.designs.len() < MAX_DESIGNS && self.last_id != u64::MAX
    }

    pub(crate) fn duplicate(&mut self, id: &str) -> Option<Choice> {
        let layout = self.layout(id)?;
        if !self.can_duplicate() {
            return None;
        }
        let mut number = self.last_id;
        let (id, name) = loop {
            number = number.checked_add(1)?;
            let name = format!("Custom design {number}");
            if self.available_name(&name, None) {
                break (format!("custom-{number}"), name);
            }
        };
        self.last_id = number;
        self.designs.push(Combination {
            id: id.clone(),
            name: name.clone(),
            layout,
        });
        Some(Choice { id, name })
    }

    pub(crate) fn rename(&mut self, id: &str, name: &str) -> bool {
        let name = name.trim();
        if !self.available_name(name, Some(id)) {
            return false;
        }
        let Some(design) = self.designs.iter_mut().find(|design| design.id == id) else {
            return false;
        };
        design.name = name.to_owned();
        true
    }

    pub(crate) fn set_layout(&mut self, id: &str, layout: Layout) {
        if let Some(design) = self.designs.iter_mut().find(|design| design.id == id) {
            design.layout = layout;
        }
    }

    pub(crate) fn remove(&mut self, id: &str) {
        self.designs.retain(|design| design.id != id);
        if self.default_id.as_deref() == Some(id) {
            self.clear_custom_default();
        }
    }

    fn available_name(&self, name: &str, except: Option<&str>) -> bool {
        if name.is_empty()
            || name.len() > MAX_NAME_BYTES
            || name.chars().any(|character| {
                character.is_control() || !crate::session_journal::is_display_character(character)
            })
        {
            return false;
        }
        let name = name.to_lowercase();
        !Preset::ALL.into_iter().any(|preset| preset.id() == name)
            && !self.designs.iter().any(|design| {
                Some(design.id.as_str()) != except && design.name.to_lowercase() == name
            })
    }
}

#[cfg(test)]
mod tests;
