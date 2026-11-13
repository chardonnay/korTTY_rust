//! Display settings from the primary connection of a Java terminal tab.

use std::io;

use iced::Font;
use kortty_terminal::Palette;
use roxmltree::Node;
use serde::{Deserialize, Serialize};

use crate::connection_store::{JAVA_ANSI_FIELDS, ThemeOverride};
use crate::profile::{MAX_TERMINAL_FONT, MIN_TERMINAL_FONT, invalid};
use crate::ssh_dialog::unsafe_text;
use crate::terminal_theme::{
    Appearance, BoldTextMode, CursorStyle, Id, MONOSPACED, Rgb, Scheme, Themes, installed_font,
};

use super::{boolean, child, number, text_value, value};

const DEFAULT_FONT: u8 = 14;
const JAVA_CUSTOM_ANSI_DEFAULTS: [u32; 16] = [
    0x000000, 0xCD0000, 0x00CD00, 0xCDCD00, 0x0000EE, 0xCD00CD, 0x00CDCD, 0xE5E5E5, 0x7F7F7F,
    0xFF0000, 0x00FF00, 0xFFFF00, 0x5C5CFF, 0xFF00FF, 0x00FFFF, 0xFFFFFF,
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectTerminalAppearance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) font_zoom: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) style: Option<Style>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Style {
    theme: Option<ThemeOverride>,
    font_family: String,
    font_size: u8,
    foreground: Rgb,
    background: Rgb,
    cursor: Rgb,
    cursor_style: CursorStyle,
    selection: Option<Rgb>,
    ansi: Option<[Rgb; 16]>,
    bold: BoldTextMode,
    sgr_enabled: bool,
}

impl ProjectTerminalAppearance {
    pub(crate) fn theme_id(&self, themes: &Themes) -> Option<Id> {
        match &self.style {
            Some(style) => style
                .theme
                .as_ref()?
                .resolved_theme(themes)
                .map(|theme| theme.id),
            None => themes.selected(),
        }
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        if self.font_zoom.is_some_and(|size| !valid_font(size))
            || self.style.as_ref().is_some_and(|style| {
                !valid_font(style.font_size)
                    || style.font_family.is_empty()
                    || style.font_family.len() > 160
                    || unsafe_text(&style.font_family)
                    || style.theme.as_ref().is_some_and(|theme| !theme.valid())
            })
        {
            return Err(invalid("invalid Java project terminal appearance"));
        }
        Ok(())
    }

    pub(crate) fn apply(&self, mut base: Appearance, themes: &Themes) -> Appearance {
        let Some(style) = &self.style else {
            return base;
        };
        let theme = style.scheme(themes);
        base.foreground = theme.map_or(style.foreground, |theme| theme.foreground);
        base.background = theme.map_or(style.background, |theme| theme.background);
        base.cursor = theme.map_or(style.cursor, |theme| theme.cursor);
        base.cursor_shape = style.cursor_style.behavior().0;
        base.selection = style.selection;
        base.palette = style.ansi.map_or_else(Palette::default, |colors| {
            Palette::new(colors.map(Rgb::bytes))
        });
        base.bold_text_mode = style.bold;
        base.sgr_enabled = style.sgr_enabled;
        base
    }

    pub(crate) fn own_font(&self, themes: &Themes, effect: bool) -> Option<(u8, Font)> {
        let style = self.style.as_ref()?;
        let theme = style.scheme(themes).filter(|_| themes.apply_font);
        let size = theme.map_or(style.font_size, |theme| theme.font_size);
        let face = theme.filter(|_| !effect).map_or_else(
            || installed_font(&style.font_family).unwrap_or(Font::MONOSPACE),
            |theme| theme.family.font(),
        );
        Some((size, face))
    }
}

impl Style {
    fn scheme(&self, themes: &Themes) -> Option<Scheme> {
        self.theme.as_ref()?.scheme(themes)
    }
}

pub(super) fn catalogue_style(connection: Node<'_, '_>) -> io::Result<Option<Style>> {
    let Some(settings) = child(connection, "settings")? else {
        return Ok(None);
    };
    if boolean(settings, "useGlobalSettings")? != Some(false) {
        return Ok(None);
    }
    let color = |field, fallback| -> io::Result<Rgb> {
        Ok(value(settings, field)?
            .and_then(Rgb::parse)
            .unwrap_or_else(|| Rgb::from_hex(fallback)))
    };
    let customized = boolean(settings, "ansiPaletteCustomized")? == Some(true);
    let mut colors = JAVA_CUSTOM_ANSI_DEFAULTS.map(Rgb::from_hex);
    if customized {
        for (field, fallback) in JAVA_ANSI_FIELDS.into_iter().zip(&mut colors) {
            *fallback = value(settings, field)?
                .and_then(Rgb::parse)
                .unwrap_or(*fallback);
        }
    }
    let style = Style {
        theme: text_value(settings, "themeId")?
            .map(str::to_owned)
            .and_then(ThemeOverride::from_java),
        font_family: value(settings, "fontFamily")?
            .filter(|name| !name.is_empty())
            .unwrap_or(MONOSPACED)
            .to_owned(),
        font_size: number::<i32>(settings, "fontSize")?.map_or(DEFAULT_FONT, clamp_font),
        foreground: color("foregroundColor", 0xFFFFFF)?,
        background: color("backgroundColor", 0x1E1E1E)?,
        cursor: color("cursorColor", 0xFFFFFF)?,
        cursor_style: enum_value(settings, "cursorStyle")?,
        selection: if customized {
            Some(color("selectionColor", 0x3399FF)?)
        } else {
            None
        },
        ansi: customized.then_some(colors),
        bold: enum_value(settings, "boldTextMode")?,
        sgr_enabled: boolean(settings, "terminalColorsEnabled")?.unwrap_or(true),
    };
    let appearance = ProjectTerminalAppearance {
        font_zoom: None,
        style: Some(style),
    };
    appearance.validate()?;
    Ok(appearance.style)
}

pub(super) fn clamp_font(size: i32) -> u8 {
    size.clamp(i32::from(MIN_TERMINAL_FONT), i32::from(MAX_TERMINAL_FONT)) as u8
}

fn valid_font(size: u8) -> bool {
    (MIN_TERMINAL_FONT..=MAX_TERMINAL_FONT).contains(&size)
}

fn enum_value<T: serde::de::DeserializeOwned + Default>(
    node: Node<'_, '_>,
    field: &str,
) -> io::Result<T> {
    Ok(value(node, field)?
        .and_then(|value| {
            serde_json::from_value(value.to_ascii_lowercase().replace('_', "-").into()).ok()
        })
        .unwrap_or_default())
}
