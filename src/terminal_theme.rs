//! Terminal palettes and font/cursor choices from the pinned Java reference.

use std::fmt;
use std::io;
use std::sync::OnceLock;

use iced::{Color, Font};
use kortty_terminal::CursorShape;
use serde::{Deserialize, Serialize};

use crate::profile::{MAX_TERMINAL_FONT, MIN_TERMINAL_FONT};
use crate::terminal_view::background_image::AssetId;

mod colors;
mod fonts;
pub(crate) mod java;
pub(crate) mod panel_colors;

pub(crate) use colors::{Appearance, BoldTextMode, Colors};
pub(crate) use fonts::{MONOSPACED, cell_width_ratio, installed_families, installed_font};

const MAX_NAME: usize = 72;
const MAX_CUSTOM: usize = 24;
pub(crate) type AgentPanelColors = [Option<Rgb>; 6];

fn no_agent_panel_colors(colors: &AgentPanelColors) -> bool {
    colors.iter().all(Option::is_none)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Rgb([u8; 3]);

impl Rgb {
    pub(crate) const fn from_bytes(bytes: [u8; 3]) -> Self {
        Self(bytes)
    }

    pub(crate) const fn from_hex(hex: u32) -> Self {
        Self([(hex >> 16) as u8, (hex >> 8) as u8, hex as u8])
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        let hex = value.trim().strip_prefix('#')?;
        if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(hex, 16).ok().map(Self::from_hex)
    }

    pub(crate) fn color(self) -> Color {
        let [red, green, blue] = self.0;
        Color::from_rgb8(red, green, blue)
    }

    pub(crate) const fn bytes(self) -> [u8; 3] {
        self.0
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [red, green, blue] = self.0;
        write!(formatter, "#{red:02X}{green:02X}{blue:02X}")
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum FontFamily {
    #[default]
    Monospaced,
    Monaco,
    JetBrainsMono,
    CourierNew,
    Consolas,
    DejaVuSansMono,
}

impl FontFamily {
    pub(crate) const ALL: [Self; 6] = [
        Self::Monaco,
        Self::Monospaced,
        Self::CourierNew,
        Self::Consolas,
        Self::DejaVuSansMono,
        Self::JetBrainsMono,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Monospaced => MONOSPACED,
            Self::Monaco => "Monaco",
            Self::JetBrainsMono => "JetBrains Mono",
            Self::CourierNew => "Courier New",
            Self::Consolas => "Consolas",
            Self::DejaVuSansMono => "DejaVu Sans Mono",
        }
    }

    pub(crate) fn font(self) -> Font {
        if self == Self::Monospaced {
            return Font::MONOSPACE;
        }
        static AVAILABLE: OnceLock<[bool; FontFamily::ALL.len()]> = OnceLock::new();
        let available = AVAILABLE.get_or_init(|| {
            let mut system = iced::advanced::graphics::text::font_system()
                .write()
                .unwrap_or_else(|error| error.into_inner());
            let database = system.raw().db();
            Self::ALL.map(|family| {
                database.faces().any(|face| {
                    face.monospaced
                        && face
                            .families
                            .iter()
                            .any(|(name, _)| name.eq_ignore_ascii_case(family.label()))
                })
            })
        });
        if Self::ALL
            .iter()
            .zip(available)
            .any(|(&family, &available)| family == self && available)
        {
            Font::with_name(self.label())
        } else {
            Font::MONOSPACE
        }
    }
}

impl fmt::Display for FontFamily {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CursorStyle {
    #[default]
    BlinkBlock,
    SteadyBlock,
    BlinkUnderline,
    SteadyUnderline,
    #[serde(alias = "blink-vertical-bar")]
    BlinkBar,
    #[serde(alias = "steady-vertical-bar")]
    SteadyBar,
}

impl CursorStyle {
    pub(crate) const ALL: [Self; 6] = [
        Self::BlinkBlock,
        Self::SteadyBlock,
        Self::BlinkUnderline,
        Self::SteadyUnderline,
        Self::BlinkBar,
        Self::SteadyBar,
    ];

    pub(crate) fn behavior(self) -> (CursorShape, bool) {
        match self {
            Self::BlinkBlock => (CursorShape::Block, true),
            Self::SteadyBlock => (CursorShape::Block, false),
            Self::BlinkUnderline => (CursorShape::Underline, true),
            Self::SteadyUnderline => (CursorShape::Underline, false),
            Self::BlinkBar => (CursorShape::Bar, true),
            Self::SteadyBar => (CursorShape::Bar, false),
        }
    }
}

impl fmt::Display for CursorStyle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BlinkBlock => "Blinking block",
            Self::SteadyBlock => "Steady block",
            Self::BlinkUnderline => "Blinking underline",
            Self::SteadyUnderline => "Steady underline",
            Self::BlinkBar => "Blinking vertical bar",
            Self::SteadyBar => "Steady vertical bar",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Scheme {
    pub(crate) foreground: Rgb,
    pub(crate) background: Rgb,
    pub(crate) cursor: Rgb,
    pub(crate) cursor_style: CursorStyle,
    pub(crate) family: FontFamily,
    pub(crate) font_size: u8,
}

impl Scheme {
    const fn preset(
        foreground: u32,
        background: u32,
        cursor: u32,
        family: FontFamily,
        font_size: u8,
    ) -> Self {
        Self {
            foreground: Rgb::from_hex(foreground),
            background: Rgb::from_hex(background),
            cursor: Rgb::from_hex(cursor),
            cursor_style: CursorStyle::BlinkBlock,
            family,
            font_size,
        }
    }
}

impl Default for Scheme {
    fn default() -> Self {
        Self::preset(0xFFFFFF, 0x1E1E1E, 0xFFFFFF, FontFamily::Monospaced, 14)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Preset {
    Default,
    Dark,
    Darcula,
    IntellijLight,
    FleetDark,
    FleetLight,
    OneDark,
    Monokai,
    SolarizedDark,
    SolarizedLight,
    Nord,
    GithubDark,
    GithubLight,
    HighContrastDark,
}

impl Preset {
    pub(crate) const ALL: [Self; 14] = [
        Self::Default,
        Self::Dark,
        Self::Darcula,
        Self::IntellijLight,
        Self::FleetDark,
        Self::FleetLight,
        Self::OneDark,
        Self::Monokai,
        Self::SolarizedDark,
        Self::SolarizedLight,
        Self::Nord,
        Self::GithubDark,
        Self::GithubLight,
        Self::HighContrastDark,
    ];

    pub(crate) fn java_id(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Dark => "dark",
            Self::Darcula => "darcula",
            Self::IntellijLight => "intellij-light",
            Self::FleetDark => "fleet-dark",
            Self::FleetLight => "fleet-light",
            Self::OneDark => "one-dark",
            Self::Monokai => "monokai",
            Self::SolarizedDark => "solarized-dark",
            Self::SolarizedLight => "solarized-light",
            Self::Nord => "nord",
            Self::GithubDark => "github-dark",
            Self::GithubLight => "github-light",
            Self::HighContrastDark => "high-contrast-dark",
        }
    }

    pub(crate) fn from_java_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.java_id() == id)
    }

    pub(crate) fn from_legacy_java_id(id: &str) -> Option<Self> {
        let normalized = id.trim().bytes().map(|byte| {
            if byte == b'_' {
                b'-'
            } else {
                byte.to_ascii_lowercase()
            }
        });
        Self::ALL
            .into_iter()
            .find(|preset| normalized.clone().eq(preset.java_id().bytes()))
    }

    // ThemeManager.java in Java reference 1ebbe724ee84, MIT, © 2026 Daniel Mengel.
    fn definition(self) -> (&'static str, Scheme) {
        use FontFamily::{JetBrainsMono, Monaco};
        let (name, fg, bg, cursor, family, size) = match self {
            Self::Default => ("Default", 0xFFFFFF, 0x000000, 0xFFFFFF, Monaco, 15),
            Self::Dark => ("Dark Mode", 0xFFFFFF, 0x1E1E1E, 0xFFFFFF, Monaco, 15),
            Self::Darcula => (
                "IntelliJ Darcula",
                0xA9B7C6,
                0x2B2B2B,
                0xA9B7C6,
                JetBrainsMono,
                14,
            ),
            Self::IntellijLight => (
                "IntelliJ Light",
                0x1E1E1E,
                0xFFFFFF,
                0x1E1E1E,
                JetBrainsMono,
                14,
            ),
            Self::FleetDark => (
                "Fleet Dark",
                0xCAD3F5,
                0x1E1F22,
                0xA6DA95,
                JetBrainsMono,
                14,
            ),
            Self::FleetLight => (
                "Fleet Light",
                0x2C2D30,
                0xF7F8FA,
                0x005FCC,
                JetBrainsMono,
                14,
            ),
            Self::OneDark => ("One Dark", 0xABB2BF, 0x282C34, 0x61AFEF, JetBrainsMono, 14),
            Self::Monokai => ("Monokai", 0xF8F8F2, 0x272822, 0xA6E22E, JetBrainsMono, 14),
            Self::SolarizedDark => (
                "Solarized Dark",
                0x93A1A1,
                0x002B36,
                0x93A1A1,
                JetBrainsMono,
                14,
            ),
            Self::SolarizedLight => (
                "Solarized Light",
                0x586E75,
                0xFDF6E3,
                0x586E75,
                JetBrainsMono,
                14,
            ),
            Self::Nord => ("Nord", 0xD8DEE9, 0x2E3440, 0x88C0D0, JetBrainsMono, 14),
            Self::GithubDark => (
                "GitHub Dark",
                0xC9D1D9,
                0x0D1117,
                0x79C0FF,
                JetBrainsMono,
                14,
            ),
            Self::GithubLight => (
                "GitHub Light",
                0x24292F,
                0xFFFFFF,
                0x0969DA,
                JetBrainsMono,
                14,
            ),
            Self::HighContrastDark => (
                "High Contrast Dark",
                0xFFFFFF,
                0x000000,
                0x00FFFF,
                JetBrainsMono,
                14,
            ),
        };
        (name, Scheme::preset(fg, bg, cursor, family, size))
    }
}

impl fmt::Display for Preset {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.definition().0)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Id {
    BuiltIn(Preset),
    Custom(u64),
}

impl Id {
    pub(crate) fn built_in(self) -> bool {
        matches!(self, Self::BuiltIn(_))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredTheme {
    id: Id,
    name: String,
    scheme: Scheme,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    java: Option<java::Exchange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    background_asset: Option<AssetId>,
    #[serde(default, skip_serializing_if = "no_agent_panel_colors")]
    agent_panel_colors: AgentPanelColors,
}

#[derive(Clone, Copy)]
pub(crate) struct Theme<'a> {
    pub(crate) id: Id,
    pub(crate) name: &'a str,
    pub(crate) scheme: Scheme,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Themes {
    selected: Option<Id>,
    pub(crate) apply_font: bool,
    edited: Vec<StoredTheme>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_custom_id: Option<u64>,
}

impl Themes {
    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub(crate) fn selected(&self) -> Option<Id> {
        self.selected
    }

    pub(crate) fn appearance(&self) -> Option<Scheme> {
        self.selected
            .and_then(|id| self.find(id))
            .map(|theme| theme.scheme)
    }

    pub(crate) fn select(&mut self, id: Option<Id>) {
        if id.is_none_or(|id| self.find(id).is_some()) {
            self.selected = id;
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = Theme<'_>> {
        Preset::ALL
            .into_iter()
            .filter_map(|preset| self.find(Id::BuiltIn(preset)))
            .chain(
                self.edited
                    .iter()
                    .filter(|theme| !theme.id.built_in())
                    .map(StoredTheme::borrowed),
            )
    }

    pub(crate) fn find(&self, id: Id) -> Option<Theme<'_>> {
        if let Some(theme) = self.edited.iter().find(|theme| theme.id == id) {
            return Some(theme.borrowed());
        }
        let Id::BuiltIn(preset) = id else {
            return None;
        };
        let (name, scheme) = preset.definition();
        Some(Theme { id, name, scheme })
    }

    pub(crate) fn find_java(&self, id: &str) -> Option<Theme<'_>> {
        if let Some(preset) = Preset::from_java_id(id) {
            return self.find(Id::BuiltIn(preset));
        }
        self.edited
            .iter()
            .find_map(|theme| (theme.java.as_ref()?.id == id).then(|| theme.borrowed()))
    }

    pub(crate) fn java_id(&self, id: Id) -> Option<&str> {
        match id {
            Id::BuiltIn(preset) => Some(preset.java_id()),
            Id::Custom(_) => self
                .edited
                .iter()
                .find(|theme| theme.id == id)?
                .java
                .as_ref()
                .map(|entry| entry.id.as_str()),
        }
    }

    pub(crate) fn java_background_path(&self, id: Id) -> Option<&str> {
        self.edited
            .iter()
            .find(|theme| theme.id == id)?
            .java
            .as_ref()?
            .background_image_path
            .as_deref()
    }

    pub(crate) fn background_asset(&self, id: Id) -> Option<AssetId> {
        self.edited
            .iter()
            .find(|theme| theme.id == id)?
            .background_asset
    }

    pub(crate) fn background_assets(&self) -> impl Iterator<Item = AssetId> + '_ {
        self.edited
            .iter()
            .filter_map(|theme| theme.background_asset)
    }

    pub(crate) fn set_background_asset(&mut self, id: Id, asset: Option<AssetId>) -> bool {
        let Some(theme) = self.edited.iter_mut().find(|theme| theme.id == id) else {
            return false;
        };
        theme.background_asset = asset;
        true
    }

    pub(crate) fn agent_panel_colors(&self, id: Id) -> Option<AgentPanelColors> {
        let theme = self.edited.iter().find(|theme| theme.id == id)?;
        let colors = theme
            .java
            .as_ref()
            .map_or(theme.agent_panel_colors, |exchange| exchange.panel_colors);
        (!no_agent_panel_colors(&colors)).then_some(colors)
    }

    pub(crate) fn set_agent_panel_colors(&mut self, id: Id, colors: AgentPanelColors) {
        if let Some(theme) = self.edited.iter_mut().find(|theme| theme.id == id) {
            if let Some(exchange) = &mut theme.java {
                exchange.panel_colors = colors;
            } else {
                theme.agent_panel_colors = colors;
            }
        }
    }

    /// Assigns persistent exchange IDs to themes created by earlier releases.
    pub(crate) fn prepare_java_export(&mut self) -> io::Result<()> {
        let mut draft = self.clone();
        for index in 0..draft.edited.len() {
            if !draft.edited[index].id.built_in() && draft.edited[index].java.is_none() {
                let mut exchange = draft.new_java_id()?;
                exchange.panel_colors = std::mem::take(&mut draft.edited[index].agent_panel_colors);
                draft.edited[index].java = Some(exchange);
            }
        }
        *self = draft;
        Ok(())
    }

    fn new_java_id(&self) -> io::Result<java::Exchange> {
        let exchange = java::Exchange::generate()?;
        if self.find_java(&exchange.id).is_some() {
            return Err(io::Error::other(
                "Generated theme exchange ID already exists.",
            ));
        }
        Ok(exchange)
    }

    pub(crate) fn can_add(&self) -> bool {
        self.edited
            .iter()
            .filter(|theme| !theme.id.built_in())
            .count()
            < MAX_CUSTOM
    }

    pub(crate) fn store(
        &mut self,
        id: Option<Id>,
        name: &str,
        scheme: Scheme,
    ) -> Result<Id, &'static str> {
        let name = name.trim();
        if !valid_name(name) {
            return Err("Enter a theme name of 1–72 bytes without control characters.");
        }
        if !(MIN_TERMINAL_FONT..=MAX_TERMINAL_FONT).contains(&scheme.font_size) {
            return Err("Enter a font size between 8 and 72.");
        }
        let (id, java) = match id {
            Some(id) if self.find(id).is_some() => (id, None),
            Some(_) => return Err("This theme is no longer available."),
            None if !self.can_add() => return Err("The custom theme limit has been reached."),
            None => {
                let exchange = self
                    .new_java_id()
                    .map_err(|_| "Could not allocate a theme exchange ID.")?;
                (self.allocate_custom_id()?, Some(exchange))
            }
        };
        if let Some(existing) = self.edited.iter_mut().find(|item| item.id == id) {
            if existing.scheme.family != scheme.family
                && let Some(java) = &mut existing.java
            {
                java.font_family = None;
            }
            existing.name = name.into();
            existing.scheme = scheme;
        } else {
            self.edited.push(StoredTheme {
                id,
                name: name.into(),
                scheme,
                java,
                background_asset: None,
                agent_panel_colors: Default::default(),
            });
        }
        self.selected = Some(id);
        Ok(id)
    }

    pub(crate) fn remove(&mut self, id: Id) {
        if id.built_in() || self.find(id).is_none() {
            return;
        }
        // A saved connection may still reference a deleted custom theme.
        // Capture the high-water mark before removing a legacy catalogue entry.
        if self.last_custom_id.is_none() {
            self.last_custom_id = Some(self.highest_custom_id());
        }
        self.edited.retain(|theme| theme.id != id);
        if self.selected == Some(id) {
            self.selected = Some(Id::BuiltIn(Preset::Default));
        }
    }

    pub(crate) fn valid(&self) -> bool {
        let mut ids = std::collections::BTreeSet::new();
        let mut java_ids = std::collections::BTreeSet::new();
        self.edited.len() <= Preset::ALL.len() + MAX_CUSTOM
            && self
                .edited
                .iter()
                .filter(|theme| !theme.id.built_in())
                .count()
                <= MAX_CUSTOM
            && self.edited.iter().all(|theme| {
                ids.insert(theme.id)
                    && theme.id != Id::Custom(0)
                    && valid_name(&theme.name)
                    && (MIN_TERMINAL_FONT..=MAX_TERMINAL_FONT).contains(&theme.scheme.font_size)
                    && theme.java.as_ref().is_none_or(|java| {
                        !theme.id.built_in() && java.valid() && java_ids.insert(java.id.as_str())
                    })
            })
            && self.selected.is_none_or(|id| self.find(id).is_some())
            && self
                .last_custom_id
                .is_none_or(|id| id != 0 && id >= self.highest_custom_id())
    }

    fn allocate_custom_id(&mut self) -> Result<Id, &'static str> {
        if !self.can_add() {
            return Err("The custom theme limit has been reached.");
        }
        let id = self
            .last_custom_id
            .unwrap_or_else(|| self.highest_custom_id())
            .checked_add(1)
            .ok_or("The theme ID limit has been reached.")?;
        self.last_custom_id = Some(id);
        Ok(Id::Custom(id))
    }

    fn highest_custom_id(&self) -> u64 {
        self.edited
            .iter()
            .filter_map(|theme| match theme.id {
                Id::Custom(id) => Some(id),
                Id::BuiltIn(_) => None,
            })
            .max()
            .unwrap_or(0)
    }

    pub(crate) fn font_size(&self, scheme: Option<Scheme>, zoomed: u8, base: u8) -> u8 {
        let Some(scheme) = scheme.filter(|_| self.apply_font) else {
            return zoomed;
        };
        (i16::from(scheme.font_size) + i16::from(zoomed) - i16::from(base))
            .clamp(i16::from(MIN_TERMINAL_FONT), i16::from(MAX_TERMINAL_FONT)) as u8
    }
}

impl StoredTheme {
    fn borrowed(&self) -> Theme<'_> {
        Theme {
            id: self.id,
            name: &self.name,
            scheme: self.scheme,
        }
    }
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= MAX_NAME && !name.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_theme_order_and_rgb_values_are_exact() {
        let expected = [
            ("Default", 0xFFFFFF, 0x000000, 0xFFFFFF),
            ("Dark Mode", 0xFFFFFF, 0x1E1E1E, 0xFFFFFF),
            ("IntelliJ Darcula", 0xA9B7C6, 0x2B2B2B, 0xA9B7C6),
            ("IntelliJ Light", 0x1E1E1E, 0xFFFFFF, 0x1E1E1E),
            ("Fleet Dark", 0xCAD3F5, 0x1E1F22, 0xA6DA95),
            ("Fleet Light", 0x2C2D30, 0xF7F8FA, 0x005FCC),
            ("One Dark", 0xABB2BF, 0x282C34, 0x61AFEF),
            ("Monokai", 0xF8F8F2, 0x272822, 0xA6E22E),
            ("Solarized Dark", 0x93A1A1, 0x002B36, 0x93A1A1),
            ("Solarized Light", 0x586E75, 0xFDF6E3, 0x586E75),
            ("Nord", 0xD8DEE9, 0x2E3440, 0x88C0D0),
            ("GitHub Dark", 0xC9D1D9, 0x0D1117, 0x79C0FF),
            ("GitHub Light", 0x24292F, 0xFFFFFF, 0x0969DA),
            ("High Contrast Dark", 0xFFFFFF, 0x000000, 0x00FFFF),
        ];
        let themes = Themes::default();
        assert!(themes.appearance().is_none());
        assert_eq!(themes.iter().count(), expected.len());
        for (index, (theme, (name, foreground, background, cursor))) in
            themes.iter().zip(expected).enumerate()
        {
            assert_eq!(theme.name, name);
            assert_eq!(theme.scheme.foreground, Rgb::from_hex(foreground));
            assert_eq!(theme.scheme.background, Rgb::from_hex(background));
            assert_eq!(theme.scheme.cursor, Rgb::from_hex(cursor));
            assert_eq!(theme.scheme.cursor_style, CursorStyle::BlinkBlock);
            let (family, size) = if index < 2 {
                (FontFamily::Monaco, 15)
            } else {
                (FontFamily::JetBrainsMono, 14)
            };
            assert_eq!(
                (theme.scheme.family, theme.scheme.font_size),
                (family, size)
            );
        }
    }

    #[test]
    fn persisted_overrides_preserve_builtin_identity_and_custom_deletion_falls_back() {
        let mut themes = Themes::default();
        let builtin = Id::BuiltIn(Preset::Nord);
        let scheme = Scheme {
            font_size: 21,
            cursor_style: CursorStyle::SteadyBar,
            ..themes.find(builtin).unwrap().scheme
        };
        assert_eq!(
            themes.store(Some(builtin), "Nord revised", scheme).unwrap(),
            builtin
        );
        themes.remove(builtin);
        assert_eq!(themes.find(builtin).unwrap().name, "Nord revised");
        assert!(themes.store(None, "bad\0name", scheme).is_err());
        let custom = themes.store(None, "Copy", scheme).unwrap();
        assert_eq!(themes.font_size(Some(scheme), 16, 14), 16);
        themes.apply_font = true;
        assert_eq!(themes.font_size(Some(scheme), 16, 14), 23);
        let bytes = serde_json::to_vec(&themes).unwrap();
        let mut saved: Themes = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(saved, themes);
        assert!(saved.valid());
        saved.remove(custom);
        assert_eq!(saved.selected(), Some(Id::BuiltIn(Preset::Default)));
        assert_eq!(Rgb::parse("#00aF40").unwrap().to_string(), "#00AF40");
        assert!(Rgb::parse("#ABC").is_none());
    }

    #[test]
    fn legacy_custom_theme_ids_are_never_reused_after_deletion_and_restart() {
        let mut themes = Themes::default();
        let scheme = Scheme::default();
        let original = themes.store(None, "Original", scheme).unwrap();
        let mut legacy = serde_json::to_value(&themes).unwrap();
        legacy.as_object_mut().unwrap().remove("last_custom_id");
        let mut themes: Themes = serde_json::from_value(legacy).unwrap();
        assert!(themes.valid());
        themes.remove(original);
        let stored = serde_json::to_vec(&themes).unwrap();
        let mut reopened: Themes = serde_json::from_slice(&stored).unwrap();
        assert!(reopened.valid());
        let next = reopened.store(None, "New theme", scheme).unwrap();
        assert!(next > original);
        assert!(reopened.find(original).is_none());
    }
}
