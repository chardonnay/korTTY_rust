//! Portable defaults from Java's global ConnectionSettings bean.

use std::io;

use kortty_ssh::KeepAlive;
use kortty_terminal::{ScrollbackConfig, TextEncoding};
use roxmltree::Node;
use serde::{Deserialize, Serialize};

use crate::connection_store::{
    JAVA_ANSI_FIELDS, MAX_TERMINAL_COLUMNS, MAX_TERMINAL_ROWS, MIN_TERMINAL_COLUMNS,
    MIN_TERMINAL_ROWS,
};
use crate::profile::{MAX_TERMINAL_FONT, MIN_TERMINAL_FONT};
use crate::terminal_theme::{BoldTextMode, CursorStyle, Rgb, Themes, java};
use crate::workspace::ssh_access::DefaultEncoding;

use super::{RawFields, java_flag, preference, preference_node, preference_value};

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Preferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_font: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_font_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_columns: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_rows: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scrollback_lines: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scrollback_memory_mib: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scrollback_compression_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ssh_keepalive_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ssh_keepalive_interval: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_bold_text_mode: Option<BoldTextMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command_timestamps_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    confirm_busy_terminal_close: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ssh_default_encoding: Option<DefaultEncoding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    java_ssh_encoding_hint: Option<TextEncoding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    theme_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    apply_theme_fonts: Option<bool>,
    colors: Colors,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Colors {
    #[serde(skip_serializing_if = "Option::is_none")]
    foreground: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    background: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor_style: Option<CursorStyle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor_blink: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sgr_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selection: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ansi: Option<[Option<Rgb>; 16]>,
}

fn cursor_style(value: &str) -> Option<CursorStyle> {
    let name = value.to_ascii_lowercase().replace('_', "-");
    serde_json::from_value(name.into()).ok()
}

fn bold_mode(value: &str) -> Option<BoldTextMode> {
    match value.to_ascii_uppercase().as_str() {
        "BOLD_FONT" => Some(BoldTextMode::BoldFont),
        "BRIGHT_COLOR" => Some(BoldTextMode::BrightColor),
        "BOLD_FONT_AND_BRIGHT_COLOR" => Some(BoldTextMode::BoldFontAndBrightColor),
        _ => None,
    }
}

fn bounded<T: std::str::FromStr + Ord>(
    value: &str,
    limits: std::ops::RangeInclusive<T>,
) -> Option<T> {
    value.parse().ok().filter(|value| limits.contains(value))
}

fn valid_font_family(name: &str) -> bool {
    !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
}

impl Preferences {
    pub(super) fn read(root: Node<'_, '_>, omitted: &mut usize) -> Option<Self> {
        let mut settings = match preference_node(root, "defaultTerminalSettings") {
            Ok(Some(node)) => Self::read_terminal(node, omitted),
            Ok(None) => Self::default(),
            Err(()) => {
                *omitted += 1;
                Self::default()
            }
        };
        settings.apply_theme_fonts = preference(root, "applyThemeFonts", omitted, java_flag);
        settings.colors.cursor_blink = match preference_node(root, "terminalCursorBlink") {
            Ok(None) => settings.colors.cursor_style.map(|style| style.behavior().1),
            _ => preference(root, "terminalCursorBlink", omitted, java_flag),
        };
        if preference(root, "terminalEncodingConfirmed", omitted, java_flag) == Some(true) {
            settings.ssh_default_encoding = settings
                .java_ssh_encoding_hint
                .take()
                .map(DefaultEncoding::Text);
        } else if settings.java_ssh_encoding_hint == Some(TextEncoding::Utf8) {
            settings.java_ssh_encoding_hint = None;
        }
        (settings.count() != 0).then_some(settings)
    }

    fn read_terminal(node: Node<'_, '_>, omitted: &mut usize) -> Self {
        let customized =
            preference(node, "ansiPaletteCustomized", omitted, java_flag) == Some(true);
        let ansi = customized
            .then(|| JAVA_ANSI_FIELDS.map(|name| preference(node, name, omitted, Rgb::parse)));
        Self {
            theme_id: match preference_value(node, "themeId", true) {
                Ok(Some(id)) if java::valid_id(id) => Some(id.into()),
                Ok(None | Some("")) => None,
                _ => {
                    *omitted += 1;
                    None
                }
            },
            terminal_font: preference(node, "fontSize", omitted, |value| {
                bounded(value, MIN_TERMINAL_FONT..=MAX_TERMINAL_FONT)
            }),
            terminal_font_family: preference(node, "fontFamily", omitted, |value| {
                valid_font_family(value).then(|| value.to_owned())
            }),
            terminal_columns: preference(node, "terminalColumns", omitted, |value| {
                bounded(value, MIN_TERMINAL_COLUMNS..=MAX_TERMINAL_COLUMNS)
            }),
            terminal_rows: preference(node, "terminalRows", omitted, |value| {
                bounded(value, MIN_TERMINAL_ROWS..=MAX_TERMINAL_ROWS)
            }),
            scrollback_lines: preference(node, "scrollbackLines", omitted, |value| {
                bounded(
                    value,
                    ScrollbackConfig::MIN_LINES..=ScrollbackConfig::MAX_LINES,
                )
            }),
            scrollback_memory_mib: preference(node, "scrollbackMemoryMb", omitted, |value| {
                let value: i32 = value.parse().ok()?;
                Some(if value <= 0 {
                    ScrollbackConfig::DEFAULT_MEMORY_MIB
                } else {
                    value.min(i32::from(ScrollbackConfig::MAX_MEMORY_MIB)) as u8
                })
            }),
            scrollback_compression_enabled: preference(
                node,
                "scrollbackCompression",
                omitted,
                java_flag,
            ),
            ssh_keepalive_enabled: preference(node, "sshKeepAliveEnabled", omitted, java_flag),
            ssh_keepalive_interval: preference(node, "sshKeepAliveInterval", omitted, |value| {
                bounded(value, KeepAlive::MIN_SECONDS..=KeepAlive::MAX_SECONDS)
            }),
            terminal_bold_text_mode: preference(node, "boldTextMode", omitted, bold_mode),
            command_timestamps_enabled: preference(
                node,
                "commandTimestampsEnabled",
                omitted,
                java_flag,
            ),
            confirm_busy_terminal_close: preference(
                node,
                "closeWithoutConfirmation",
                omitted,
                |value| java_flag(value).map(|without| !without),
            ),
            ssh_default_encoding: None,
            java_ssh_encoding_hint: preference(node, "encoding", omitted, TextEncoding::from_name),
            apply_theme_fonts: None,
            colors: Colors {
                foreground: preference(node, "foregroundColor", omitted, Rgb::parse),
                background: preference(node, "backgroundColor", omitted, Rgb::parse),
                cursor: preference(node, "cursorColor", omitted, Rgb::parse),
                cursor_style: preference(node, "cursorStyle", omitted, cursor_style),
                sgr_enabled: preference(node, "terminalColorsEnabled", omitted, java_flag),
                selection: customized
                    .then(|| preference(node, "selectionColor", omitted, Rgb::parse))
                    .flatten(),
                ansi: ansi.filter(|values| values.iter().any(Option::is_some)),
                cursor_blink: None,
            },
        }
    }

    pub(super) fn count(&self) -> usize {
        [
            self.terminal_font.is_some(),
            self.terminal_font_family.is_some(),
            self.terminal_columns.is_some(),
            self.terminal_rows.is_some(),
            self.scrollback_lines.is_some(),
            self.scrollback_memory_mib.is_some(),
            self.scrollback_compression_enabled.is_some(),
            self.ssh_keepalive_enabled.is_some(),
            self.ssh_keepalive_interval.is_some(),
            self.terminal_bold_text_mode.is_some(),
            self.command_timestamps_enabled.is_some(),
            self.confirm_busy_terminal_close.is_some(),
            self.ssh_default_encoding.is_some(),
            self.java_ssh_encoding_hint.is_some(),
            self.theme_id.is_some(),
            self.apply_theme_fonts.is_some(),
            self.colors.foreground.is_some(),
            self.colors.background.is_some(),
            self.colors.cursor.is_some(),
            self.colors.cursor_style.is_some(),
            self.colors.cursor_blink.is_some(),
            self.colors.sgr_enabled.is_some(),
            self.colors.selection.is_some(),
            self.colors.ansi.is_some(),
        ]
        .into_iter()
        .filter(|set| *set)
        .count()
    }

    pub(super) fn merge(&self, fields: &mut RawFields) -> io::Result<bool> {
        if self
            .terminal_font
            .is_some_and(|font| !(MIN_TERMINAL_FONT..=MAX_TERMINAL_FONT).contains(&font))
            || self
                .terminal_font_family
                .as_deref()
                .is_some_and(|name| !valid_font_family(name))
        {
            return Err(io::Error::other("Invalid Java default terminal font"));
        }
        let mut incoming: RawFields = serde_json::from_slice(&serde_json::to_vec(self)?)?;
        incoming.remove("colors");
        incoming.remove("theme_id");
        incoming.remove("apply_theme_fonts");
        let before = fields.len();
        for (key, value) in incoming {
            fields.entry(key).or_insert(value);
        }
        let colors = self.colors.merge(fields)?;
        Ok(colors || fields.len() != before)
    }
}

pub(super) fn merge_themes(
    fields: &mut RawFields,
    themes: &mut Themes,
    library_added: bool,
    imported: Option<&Preferences>,
    has_local_base_colors: bool,
) -> io::Result<bool> {
    let local: RawFields = fields
        .get("themes")
        .map(|value| serde_json::from_str(value.get()))
        .transpose()?
        .unwrap_or_default();
    let mut selected = local.contains_key("selected");
    let mut apply_font = local.contains_key("apply_font");
    let mut changed = library_added;
    if let Some(imported) = imported {
        if let Some(id) = &imported.theme_id {
            if !java::valid_id(id) {
                return Err(io::Error::other("Invalid Java default terminal theme ID"));
            }
            if !selected
                && !has_local_base_colors
                && let Some(theme) = themes.find_java(id)
            {
                themes.select_java_default(theme.id);
                selected = true;
                changed = true;
            }
        }
        if !apply_font && let Some(value) = imported.apply_theme_fonts {
            themes.apply_font = value;
            apply_font = true;
            changed = true;
        }
    }
    if changed {
        let mut merged: RawFields = serde_json::from_slice(&serde_json::to_vec(themes)?)?;
        if !selected {
            merged.remove("selected");
        }
        if !apply_font {
            merged.remove("apply_font");
        }
        fields.insert("themes".into(), serde_json::value::to_raw_value(&merged)?);
    }
    Ok(changed)
}

impl Colors {
    fn merge(&self, fields: &mut RawFields) -> io::Result<bool> {
        let mut incoming: RawFields = serde_json::from_slice(&serde_json::to_vec(self)?)?;
        if incoming.is_empty() {
            return Ok(false);
        }
        let mut local: RawFields = fields
            .get("colors")
            .map(|value| serde_json::from_str(value.get()))
            .transpose()?
            .unwrap_or_default();
        let mut changed = false;
        if let Some(ansi) = incoming.remove("ansi") {
            let imported: [Option<Rgb>; 16] = serde_json::from_str(ansi.get())?;
            let existing: [Option<Rgb>; 16] = local
                .get("ansi")
                .map(|value| serde_json::from_str(value.get()))
                .transpose()?
                .unwrap_or_default();
            let merged =
                std::array::from_fn::<_, 16, _>(|index| existing[index].or(imported[index]));
            if merged != existing {
                local.insert("ansi".into(), serde_json::value::to_raw_value(&merged)?);
                changed = true;
            }
        }
        for (key, value) in incoming {
            if local
                .get(&key)
                .is_none_or(|current| current.get() == "null")
            {
                local.insert(key, value);
                changed = true;
            }
        }
        if changed {
            fields.insert("colors".into(), serde_json::value::to_raw_value(&local)?);
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{WindowSettings, merge_java_window_settings, portable_java_preferences};
    use super::*;

    #[test]
    fn java_window_close_confirmation_restores_independently_of_terminal_defaults() {
        let incoming = portable_java_preferences(
            br#"<globalSettings>
            <closeActiveTerminalWindowsWithoutConfirmation>true</closeActiveTerminalWindowsWithoutConfirmation>
            <defaultTerminalSettings><closeWithoutConfirmation>false</closeWithoutConfirmation></defaultTerminalSettings>
            </globalSettings>"#,
            None,
        )
        .unwrap();
        assert_eq!((incoming.count, incoming.omitted), (2, 0));
        let incoming = incoming.contents.unwrap();
        let restore = |local: Option<&[u8]>| {
            let merged = merge_java_window_settings(local, &incoming, &Default::default())
                .unwrap()
                .unwrap();
            WindowSettings::decode(&merged).unwrap()
        };
        let restored = restore(None);
        assert_eq!(restored.skip_terminal_group_close_confirmation, Some(true));
        assert!(restored.confirm_busy_terminal_close);
        let local = restore(Some(br#"{"skip_terminal_group_close_confirmation":false}"#));
        assert_eq!(local.skip_terminal_group_close_confirmation, Some(false));
        assert!(local.confirm_busy_terminal_close);
    }

    #[test]
    fn java_skip_confirmation_imports_an_unset_native_busy_close_preference() {
        let incoming = portable_java_preferences(
            br#"<globalSettings><defaultTerminalSettings>
            <closeWithoutConfirmation>true</closeWithoutConfirmation>
            </defaultTerminalSettings></globalSettings>"#,
            None,
        )
        .unwrap();
        assert_eq!((incoming.count, incoming.omitted), (1, 0));
        let incoming = incoming.contents.unwrap();
        let merged = merge_java_window_settings(None, &incoming, &Default::default())
            .unwrap()
            .unwrap();
        assert!(
            !WindowSettings::decode(&merged)
                .unwrap()
                .confirm_busy_terminal_close
        );
        let native = br#"{"confirm_busy_terminal_close":true}"#;
        assert!(
            merge_java_window_settings(Some(native), &incoming, &Default::default())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn java_ssh_encoding_is_active_only_after_confirmation_and_respects_native_choice() {
        let read = |confirmation: &str, encoding: &str| {
            portable_java_preferences(
                format!("<globalSettings>{confirmation}<defaultTerminalSettings><encoding>{encoding}</encoding></defaultTerminalSettings></globalSettings>").as_bytes(),
                None,
            ).unwrap()
        };
        let restore = |incoming: &[u8], local: Option<&[u8]>| {
            let restored = merge_java_window_settings(local, incoming, &Default::default())
                .unwrap()
                .unwrap();
            WindowSettings::decode(&restored).unwrap()
        };
        let pending = read("", "latin9");
        assert_eq!((pending.count, pending.omitted), (1, 0));
        let settings = restore(&pending.contents.unwrap(), None);
        assert_eq!(settings.java_ssh_encoding_hint, Some(TextEncoding::Latin9));
        assert_eq!(settings.ssh_default_encoding, None);

        let confirmed = "<terminalEncodingConfirmed>true</terminalEncodingConfirmed>";
        let import = read(confirmed, "cp1252").contents.unwrap();
        let settings = restore(&import, None);
        assert_eq!(
            settings.ssh_default_encoding,
            Some(DefaultEncoding::Text(TextEncoding::Windows1252))
        );
        assert_eq!(settings.java_ssh_encoding_hint, None);
        let native = br#"{"ssh_default_encoding":"emulation"}"#;
        assert!(
            merge_java_window_settings(Some(native), &import, &Default::default())
                .unwrap()
                .is_none()
        );
        let invalid = read(confirmed, "UTF-16");
        assert_eq!((invalid.count, invalid.omitted), (0, 1));
        assert!(invalid.contents.is_none());
    }

    #[test]
    fn java_defaults_fill_native_terminal_fields_with_explicit_cursor_blink_and_palette() {
        let xml = br#"<globalSettings>
            <terminalCursorBlink>false</terminalCursorBlink>
            <commandTimestampsEnabled>false</commandTimestampsEnabled>
            <defaultTerminalSettings>
                <fontSize>19</fontSize><fontFamily>Monospaced</fontFamily>
                <terminalColumns>112</terminalColumns><terminalRows>37</terminalRows>
                <scrollbackLines>28000</scrollbackLines><scrollbackMemoryMb>28</scrollbackMemoryMb>
                <scrollbackCompression>false</scrollbackCompression>
                <sshKeepAliveEnabled>false</sshKeepAliveEnabled><sshKeepAliveInterval>90</sshKeepAliveInterval>
                <boldTextMode>BRIGHT_COLOR</boldTextMode>
                <cursorStyle>BLINK_VERTICAL_BAR</cursorStyle><foregroundColor>#ABCDEF</foregroundColor>
                <cursorColor>#123456</cursorColor><terminalColorsEnabled>false</terminalColorsEnabled>
                <ansiPaletteCustomized>true</ansiPaletteCustomized><ansiRed>#990000</ansiRed>
                <ansiGreen>#007711</ansiGreen><selectionColor>#202020</selectionColor>
                <commandTimestampsEnabled>true</commandTimestampsEnabled>
            </defaultTerminalSettings>
        </globalSettings>"#;
        let incoming = portable_java_preferences(xml, None).unwrap();
        assert_eq!(incoming.omitted, 0);
        let incoming = incoming.contents.unwrap();
        let native = br#"{"terminal_font":17,"colors":{
            "foreground":[18,52,86], "cursor_blink":null,
            "ansi":[null,[119,68,17],null,null,null,null,null,null,null,null,null,null,null,null,null,null]
        }}"#;
        let restored = merge_java_window_settings(Some(native), &incoming, &Default::default())
            .unwrap()
            .unwrap();
        let settings = WindowSettings::decode(&restored).unwrap();
        assert_eq!(settings.terminal_font, 17);
        assert_eq!(
            (settings.terminal_columns, settings.terminal_rows),
            (Some(112), Some(37))
        );
        assert_eq!(
            settings.scrollback_config(),
            ScrollbackConfig {
                lines: 28000,
                memory_mib: 28,
                compression: false
            }
        );
        assert_eq!(settings.ssh_keepalive(), KeepAlive::new(false, 90).unwrap());
        assert!(!settings.command_timestamps_enabled);
        assert_eq!(settings.terminal_bold_text_mode, BoldTextMode::BrightColor);
        assert_eq!(settings.colors.foreground, Rgb::parse("#123456"));
        assert_eq!(settings.colors.ansi[1], Rgb::parse("#774411"));
        assert_eq!(settings.colors.ansi[2], Rgb::parse("#007711"));
        assert_eq!(settings.colors.selection, Rgb::parse("#202020").unwrap());
        let appearance = settings.terminal_appearance();
        assert_eq!(appearance.cursor_shape, kortty_terminal::CursorShape::Bar);
        assert!(!appearance.cursor_blink && !appearance.sgr_enabled);
        assert!(
            merge_java_window_settings(Some(&restored), &incoming, &Default::default())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn older_java_defaults_use_cursor_style_and_ignore_unconfirmed_palette() {
        let imported = portable_java_preferences(
            br#"<globalSettings><defaultTerminalSettings>
                <cursorStyle>STEADY_UNDERLINE</cursorStyle>
                <ansiRed>#CC3322</ansiRed><selectionColor>#202020</selectionColor>
                <terminalRows>0</terminalRows><sshKeepAliveInterval>none</sshKeepAliveInterval>
                <scrollbackMemoryMb>0</scrollbackMemoryMb>
            </defaultTerminalSettings></globalSettings>"#,
            None,
        )
        .unwrap();
        assert_eq!((imported.count, imported.omitted), (3, 2));
        let restored =
            merge_java_window_settings(None, &imported.contents.unwrap(), &Default::default())
                .unwrap()
                .unwrap();
        let settings = WindowSettings::decode(&restored).unwrap();
        assert_eq!(
            settings.colors.cursor_style,
            Some(CursorStyle::SteadyUnderline)
        );
        assert_eq!(settings.colors.cursor_blink, Some(false));
        assert_eq!(settings.colors.ansi, [None; 16]);
        assert_eq!(
            settings.scrollback_memory_mib,
            ScrollbackConfig::DEFAULT_MEMORY_MIB
        );
    }
}
