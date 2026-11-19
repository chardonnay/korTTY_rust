use std::io;

use kortty_terminal::{Palette, ScrollbackConfig, TextEncoding};
use serde::{Deserialize, Serialize};

use crate::ai_manager::ProfileRef;
use crate::profile::{InitialTerminalSize, MAX_TERMINAL_FONT, MIN_TERMINAL_FONT};
use crate::ssh_dialog::unsafe_text;
use crate::terminal_theme::{
    Appearance, BoldTextMode, CursorStyle, Id, Preset, Rgb, Scheme, Theme, Themes, java,
};

pub(crate) const MIN_TERMINAL_COLUMNS: u16 = 40;
pub(crate) const MAX_TERMINAL_COLUMNS: u16 = 500;
pub(crate) const MIN_TERMINAL_ROWS: u16 = 10;
pub(crate) const MAX_TERMINAL_ROWS: u16 = 200;
pub(crate) const MAX_PASTE_LINE_DELAY_MS: u16 = 1000;
pub(crate) const MAX_ASSIGNED_AI_SKILLS: usize = 32;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AiAssignments {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) profile: Option<ProfileRef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) skills: Vec<u64>,
}

impl AiAssignments {
    pub(crate) fn from_terminal(terminal: &TerminalOverrides) -> Self {
        Self {
            profile: terminal.ai_profile.clone(),
            skills: terminal.ai_skills.clone(),
        }
    }

    pub(crate) fn write_terminal(&self, terminal: &mut TerminalOverrides) {
        terminal.ai_profile.clone_from(&self.profile);
        terminal.ai_skills.clone_from(&self.skills);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.profile.is_none() && self.skills.is_empty()
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        if self
            .profile
            .as_ref()
            .is_some_and(|profile| !profile.is_nonzero())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid AI profile reference.",
            ));
        }
        validate_ai_skills(&self.skills)
    }
}

fn validate_ai_skills(skills: &[u64]) -> io::Result<()> {
    if skills.len() > MAX_ASSIGNED_AI_SKILLS
        || skills.contains(&0)
        || skills
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != skills.len()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid assigned AI skills.",
        ));
    }
    Ok(())
}

/// Built-in IDs retain their original string representation in saved profiles.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub(crate) enum ThemeOverride {
    BuiltIn(Preset),
    Custom { custom: u64 },
    Java { java: String },
}

impl ThemeOverride {
    pub(crate) fn from_java(id: String) -> Option<Self> {
        if let Some(preset) = Preset::from_java_id(&id) {
            return Some(Self::BuiltIn(preset));
        }
        if java::valid_id(&id) {
            Some(Self::Java { java: id })
        } else {
            Preset::from_legacy_java_id(&id).map(Self::BuiltIn)
        }
    }

    pub(crate) fn scheme(&self, themes: &Themes) -> Option<Scheme> {
        self.resolved_theme(themes).map(|theme| theme.scheme)
    }

    pub(crate) fn resolved_theme<'a>(&self, themes: &'a Themes) -> Option<Theme<'a>> {
        match self {
            Self::BuiltIn(preset) => themes.find(Id::BuiltIn(*preset)),
            Self::Custom { custom } => themes.find(Id::Custom(*custom)),
            Self::Java { java } => themes.find_java(java).or_else(|| {
                // An exact imported definition takes precedence over the
                // forgiving built-in aliases recognized by old Rust imports.
                themes.find(Id::BuiltIn(Preset::from_legacy_java_id(java)?))
            }),
        }
    }

    pub(crate) fn valid(&self) -> bool {
        match self {
            Self::BuiltIn(_) => true,
            Self::Custom { custom } => *custom != 0,
            Self::Java { java } => java::valid_id(java),
        }
    }
}

impl From<Id> for ThemeOverride {
    fn from(value: Id) -> Self {
        match value {
            Id::BuiltIn(preset) => Self::BuiltIn(preset),
            Id::Custom(custom) => Self::Custom { custom },
        }
    }
}

/// Unset fields inherit the active global settings. Initial dimensions apply at launch.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct TerminalOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) theme: Option<ThemeOverride>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) font_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) font_size: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) foreground: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) background: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cursor: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cursor_style: Option<CursorStyle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) selection: Option<Rgb>,
    #[serde(skip_serializing_if = "no_ansi_colors")]
    pub(crate) ansi: [Option<Rgb>; 16],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bold: Option<BoldTextMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sgr_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) encoding: Option<TextEncoding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) terminal_columns: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) terminal_rows: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scrollback_lines: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scrollback_memory_mib: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scrollback_compression: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) confirm_close: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) paste_line_delay_ms: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) strict_terminal_mode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ai_profile: Option<ProfileRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) java_ai_profile: Option<crate::ai_manager::JavaAiProfileHint>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) java_ai_profile_dismissed: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) ai_agent_disabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ai_agent_design: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) ai_skills: Vec<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) highlight_set: Option<String>,
    #[serde(skip_serializing_if = "crate::session_journal::ConnectionConfig::is_default")]
    pub(crate) journal: crate::session_journal::ConnectionConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct TerminalAppearance {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) theme: Option<ThemeOverride>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) font_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) font_size: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) foreground: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) background: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sgr_enabled: Option<bool>,
}

impl TerminalAppearance {
    pub(crate) fn from_terminal(other: &TerminalOverrides) -> Self {
        Self {
            theme: other.theme.clone(),
            font_family: other.font_family.clone(),
            font_size: other.font_size,
            foreground: other.foreground,
            background: other.background,
            sgr_enabled: other.sgr_enabled,
        }
    }

    pub(crate) fn write_terminal(&self, terminal: &mut TerminalOverrides) {
        terminal.theme.clone_from(&self.theme);
        terminal.font_family.clone_from(&self.font_family);
        terminal.font_size = self.font_size;
        terminal.foreground = self.foreground;
        terminal.background = self.background;
        terminal.sgr_enabled = self.sgr_enabled;
    }

    pub(crate) fn display_overrides(&self, saved: Option<&TerminalOverrides>) -> TerminalOverrides {
        let mut result = TerminalOverrides::default();
        if let Some(saved) = saved {
            result.cursor = saved.cursor;
            result.cursor_style = saved.cursor_style;
            result.selection = saved.selection;
            result.ansi = saved.ansi;
            result.bold = saved.bold;
        }
        self.write_terminal(&mut result);
        result
    }

    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        self.display_overrides(None).validate()
    }
}

fn no_ansi_colors(colors: &[Option<Rgb>; 16]) -> bool {
    colors.iter().all(Option::is_none)
}

impl TerminalOverrides {
    pub(crate) fn dismiss_java_ai_profile(&mut self) {
        self.java_ai_profile_dismissed |= self.java_ai_profile.take().is_some();
    }

    pub(super) fn merge_java_ai_profile(&mut self, incoming: &Self) -> bool {
        let Some(hint) = &incoming.java_ai_profile else {
            return false;
        };
        if self.ai_profile.is_some() || self.java_ai_profile_dismissed {
            return false;
        }
        if self.java_ai_profile.as_ref().is_some_and(|current| {
            current.id() != hint.id() || current.source().is_some() || hint.source().is_none()
        }) {
            return false;
        }
        self.java_ai_profile = Some(hint.clone());
        true
    }

    pub(super) fn remap_java_highlight(
        &mut self,
        ids: &std::collections::BTreeMap<String, String>,
    ) {
        self.highlight_set = self
            .highlight_set
            .as_deref()
            .and_then(|id| crate::terminal_highlight::resolve_java_backup_set(ids, id));
    }

    pub(crate) fn scrollback(&self, defaults: ScrollbackConfig) -> ScrollbackConfig {
        ScrollbackConfig {
            lines: self.scrollback_lines.unwrap_or(defaults.lines),
            memory_mib: self.scrollback_memory_mib.unwrap_or(defaults.memory_mib),
            compression: self.scrollback_compression.unwrap_or(defaults.compression),
        }
        .bounded()
    }

    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        if self
            .java_ai_profile
            .as_ref()
            .is_some_and(|hint| hint.source().is_some_and(|source| source.id() != hint.id()))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid imported AI profile provenance",
            ));
        }
        self.journal.validate()?;
        self.initial_size().validate()?;
        validate_paste_line_delay(self.paste_line_delay_ms)?;
        if self.theme.as_ref().is_some_and(|theme| !theme.valid()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid terminal theme ID.",
            ));
        }
        if self.scrollback_lines.is_some_and(|lines| {
            !(ScrollbackConfig::MIN_LINES..=ScrollbackConfig::MAX_LINES).contains(&lines)
        }) || self.scrollback_memory_mib.is_some_and(|memory| {
            !(ScrollbackConfig::MIN_MEMORY_MIB..=ScrollbackConfig::MAX_MEMORY_MIB).contains(&memory)
        }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Scrollback requires 100 to 100000 lines and 1 to 50 MiB.",
            ));
        }
        if self
            .ai_agent_design
            .as_deref()
            .is_some_and(|id| !crate::terminal_agent::design::valid_id(id))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid AI agent design ID.",
            ));
        }
        validate_ai_skills(&self.ai_skills)?;
        if self
            .highlight_set
            .as_deref()
            .is_some_and(|id| !crate::terminal_highlight::valid_id(id))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid highlight set ID.",
            ));
        }
        if self
            .font_size
            .is_some_and(|size| !(MIN_TERMINAL_FONT..=MAX_TERMINAL_FONT).contains(&size))
            || self
                .font_family
                .as_deref()
                .is_some_and(|name| name.is_empty() || name.len() > 160 || unsafe_text(name))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid terminal font override.",
            ));
        }
        Ok(())
    }

    pub(crate) fn initial_size(&self) -> InitialTerminalSize {
        InitialTerminalSize {
            columns: self.terminal_columns,
            rows: self.terminal_rows,
        }
    }

    pub(crate) fn scheme(&self, themes: &Themes) -> Option<Scheme> {
        self.theme.as_ref()?.scheme(themes)
    }

    pub(crate) fn appearance(&self, mut base: Appearance, themes: &Themes) -> Appearance {
        if let Some(scheme) = self.scheme(themes) {
            base.foreground = scheme.foreground;
            base.background = scheme.background;
            base.cursor = scheme.cursor;
            (base.cursor_shape, base.cursor_blink) = scheme.cursor_style.behavior();
        }
        if let Some(style) = self.cursor_style {
            (base.cursor_shape, base.cursor_blink) = style.behavior();
        }
        base.foreground = self.foreground.unwrap_or(base.foreground);
        base.background = self.background.unwrap_or(base.background);
        base.cursor = self.cursor.unwrap_or(base.cursor);
        base.selection = self.selection.or(base.selection);
        base.bold_text_mode = self.bold.unwrap_or(base.bold_text_mode);
        base.sgr_enabled = self.sgr_enabled.unwrap_or(base.sgr_enabled);
        if !no_ansi_colors(&self.ansi) {
            base.palette = Palette::new(std::array::from_fn(|index| {
                self.ansi[index].map_or_else(|| base.palette.rgb(index as u8), Rgb::bytes)
            }));
        }
        base
    }
}

pub(crate) fn validate_paste_line_delay(delay: Option<u16>) -> io::Result<()> {
    if delay.is_some_and(|delay| delay > MAX_PASTE_LINE_DELAY_MS) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Enter a paste line delay from 0 to 1000 ms.",
        ));
    }
    Ok(())
}

pub(crate) const ANSI_NAMES: [&str; 16] = [
    "Black",
    "Red",
    "Green",
    "Yellow",
    "Blue",
    "Magenta",
    "Cyan",
    "White",
    "Bright black",
    "Bright red",
    "Bright green",
    "Bright yellow",
    "Bright blue",
    "Bright magenta",
    "Bright cyan",
    "Bright white",
];

pub(crate) const JAVA_ANSI_FIELDS: [&str; 16] = [
    "ansiBlack",
    "ansiRed",
    "ansiGreen",
    "ansiYellow",
    "ansiBlue",
    "ansiMagenta",
    "ansiCyan",
    "ansiWhite",
    "ansiBrightBlack",
    "ansiBrightRed",
    "ansiBrightGreen",
    "ansiBrightYellow",
    "ansiBrightBlue",
    "ansiBrightMagenta",
    "ansiBrightCyan",
    "ansiBrightWhite",
];

#[cfg(test)]
#[test]
fn terminal_theme_references_preserve_legacy_strings_and_validate_new_ids() {
    use serde_json::json;

    for (value, expected) in [
        (json!("nord"), ThemeOverride::BuiltIn(Preset::Nord)),
        (json!({"custom":7}), ThemeOverride::Custom { custom: 7 }),
        (
            json!({"java":"Unmapped-Theme"}),
            ThemeOverride::Java {
                java: "Unmapped-Theme".into(),
            },
        ),
    ] {
        let terminal: TerminalOverrides = serde_json::from_value(json!({"theme":value})).unwrap();
        terminal.validate().unwrap();
        assert_eq!(terminal.theme, Some(expected));
        assert_eq!(serde_json::to_value(&terminal).unwrap()["theme"], value);
    }
    assert!(
        serde_json::from_value::<TerminalOverrides>(json!({
            "theme":{"custom":7,"java":"ambiguous"}
        }))
        .is_err()
    );
    for value in [json!({"custom":0}), json!({"java":"\u{202e}"})] {
        let terminal: TerminalOverrides = serde_json::from_value(json!({"theme":value})).unwrap();
        assert!(terminal.validate().is_err());
    }
}
