//! Additive migration of the active Java snippet appearance and editor caret.

use std::io;

use roxmltree::Node;
use serde::{Deserialize, Serialize};

use crate::editor_window::caret::Shape;
use crate::snippet_window::appearance::MAX_FONT_SIZE;
use crate::terminal_theme::Rgb;

use super::{RawFields, merge_unset_section, preference, preference_node, preference_text};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Preferences {
    caret: Caret,
    fonts: Fonts,
    palette: Palette,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Caret {
    #[serde(skip_serializing_if = "Option::is_none")]
    shape: Option<Shape>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<Rgb>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Fonts {
    #[serde(skip_serializing_if = "Option::is_none")]
    font_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    font_size: Option<u8>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Palette {
    #[serde(skip_serializing_if = "Option::is_none")]
    foreground: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    background: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor_style: Option<Shape>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor_color: Option<Rgb>,
}

fn shape(value: &str) -> Option<Shape> {
    match value.to_ascii_uppercase().as_str() {
        "BLOCK" => Some(Shape::Block),
        "LINE" => Some(Shape::Line),
        "UNDERSCORE" => Some(Shape::Underscore),
        _ => None,
    }
}

fn valid_font(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

impl Palette {
    fn new(foreground: u32, background: u32, cursor_style: Shape, cursor_color: u32) -> Self {
        Self {
            foreground: Some(Rgb::from_hex(foreground)),
            background: Some(Rgb::from_hex(background)),
            cursor_style: Some(cursor_style),
            cursor_color: Some(Rgb::from_hex(cursor_color)),
        }
    }

    fn builtin(id: &str) -> Option<Self> {
        use Shape::{Block, Line};
        // The background, text and cursor values from Java's SnippetEditorProfileSupport.
        let (foreground, background, style, cursor) = match id {
            "preset-intellij-light" => (0x080808, 0xffffff, Line, 0x000000),
            "preset-darcula" => (0xa9b7c6, 0x2b2b2b, Block, 0xbbbbbb),
            "preset-high-contrast" => (0xffffff, 0x000000, Block, 0xffff00),
            "preset-new-ui-dark" => (0xced0d6, 0x1e1f22, Line, 0xced0d6),
            "preset-warm-light" => (0x1f2328, 0xfafaf7, Line, 0x1f2328),
            "preset-blue-light" => (0x1b1f2a, 0xf4f8ff, Line, 0x1b1f2a),
            "preset-graphite-dark" => (0xd4d4d4, 0x252526, Block, 0xd4d4d4),
            "preset-night-owl" => (0xd6deeb, 0x011627, Block, 0x80cbc4),
            "preset-solarized-light" => (0x586e75, 0xfdf6e3, Line, 0x586e75),
            "preset-solarized-dark" => (0x839496, 0x002b36, Block, 0x93a1a1),
            _ => return None,
        };
        Some(Self::new(foreground, background, style, cursor))
    }

    fn selected(root: Node<'_, '_>, id: &str, omitted: &mut usize) -> Result<Option<Self>, ()> {
        if let Some(profiles) = preference_node(root, "snippetEditorProfiles")? {
            let mut matches = profiles.children().filter(|node| {
                node.has_tag_name("profile")
                    && node.tag_name().namespace().is_none()
                    && preference_text(*node, "id") == Ok(Some(id))
                    && matches!(
                        preference_text(*node, "builtIn"),
                        Ok(None | Some("false" | "0"))
                    )
            });
            if let Some(profile) = matches.next() {
                if matches.next().is_some() {
                    return Err(());
                }
                // Java normalizes missing or invalid custom fields to Darcula and a block caret.
                return Ok(Some(Self {
                    foreground: preference(profile, "foregroundColor", omitted, Rgb::parse)
                        .or(Some(Rgb::from_hex(0xa9b7c6))),
                    background: preference(profile, "backgroundColor", omitted, Rgb::parse)
                        .or(Some(Rgb::from_hex(0x2b2b2b))),
                    cursor_style: preference(profile, "cursorStyle", omitted, shape)
                        .or(Some(Shape::Block)),
                    cursor_color: preference(profile, "cursorColor", omitted, Rgb::parse)
                        .or(Some(Rgb::from_hex(0xbbbbbb))),
                }));
            }
        }
        Ok(Self::builtin(id))
    }

    fn read(root: Node<'_, '_>, omitted: &mut usize) -> Self {
        match preference_text(root, "selectedSnippetEditorProfileId") {
            Ok(Some(id)) if !id.is_empty() => match Self::selected(root, id, omitted) {
                Ok(Some(palette)) => return palette,
                Ok(None) => *omitted += 1,
                Err(()) => {
                    *omitted += 1;
                    return Self::default();
                }
            },
            Err(()) => {
                *omitted += 1;
                return Self::default();
            }
            _ => {}
        }
        Self {
            foreground: preference(root, "snippetForegroundColor", omitted, Rgb::parse),
            background: preference(root, "snippetBackgroundColor", omitted, Rgb::parse),
            cursor_style: preference(root, "snippetCursorStyle", omitted, shape),
            cursor_color: preference(root, "snippetCursorColor", omitted, Rgb::parse),
        }
    }
}

impl Preferences {
    pub(super) fn read(root: Node<'_, '_>, omitted: &mut usize) -> Option<Self> {
        let settings = Self {
            caret: Caret {
                shape: preference(root, "editorCursorStyle", omitted, shape),
                color: preference(root, "editorCursorColor", omitted, Rgb::parse),
            },
            fonts: Fonts {
                font_family: preference(root, "snippetFontFamily", omitted, |text| {
                    valid_font(text).then(|| text.to_owned())
                }),
                font_size: preference(root, "snippetFontSize", omitted, |text| {
                    text.parse().ok().filter(|&size| size <= MAX_FONT_SIZE)
                }),
            },
            palette: Palette::read(root, omitted),
        };
        (settings.count() != 0).then_some(settings)
    }

    pub(super) fn count(&self) -> usize {
        [
            self.caret.shape.is_some(),
            self.caret.color.is_some(),
            self.fonts.font_family.is_some(),
            self.fonts.font_size.is_some(),
            self.palette.foreground.is_some(),
            self.palette.background.is_some(),
            self.palette.cursor_style.is_some(),
            self.palette.cursor_color.is_some(),
        ]
        .into_iter()
        .filter(|set| *set)
        .count()
    }

    pub(super) fn merge(&self, fields: &mut RawFields) -> io::Result<bool> {
        if self
            .fonts
            .font_size
            .is_some_and(|size| size > MAX_FONT_SIZE)
            || self
                .fonts
                .font_family
                .as_deref()
                .is_some_and(|value| !valid_font(value))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid Java snippet editor font settings",
            ));
        }
        let caret = merge_unset_section(fields, "editor_caret", &self.caret)?;
        let fonts = merge_unset_section(fields, "snippet_appearance", &self.fonts)?;
        let palette = merge_unset_section(fields, "snippet_appearance", &self.palette)?;
        Ok(caret || fonts || palette)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{merge_java_window_settings, portable_java_preferences};

    #[test]
    fn java_editor_preferences_resolve_builtins_and_reject_ambiguous_custom_ids() {
        let source = |profiles: &str| {
            format!("<globalSettings>
            <selectedSnippetEditorProfileId>preset-night-owl</selectedSnippetEditorProfileId>
            <snippetBackgroundColor>#ffffff</snippetBackgroundColor><snippetFontSize>0</snippetFontSize>
            {profiles}</globalSettings>")
        };
        for (profiles, expected, omitted) in [
            (
                "",
                serde_json::json!({"font_size":0,"foreground":[214,222,235],"background":[1,22,39],
                "cursor_style":"block","cursor_color":[128,203,196]}),
                0,
            ),
            (
                "<snippetEditorProfiles><profile><id>preset-night-owl</id></profile><profile><id>preset-night-owl</id></profile></snippetEditorProfiles>",
                serde_json::json!({"font_size":0}),
                1,
            ),
        ] {
            let parsed = portable_java_preferences(source(profiles).as_bytes(), None).unwrap();
            assert_eq!(parsed.omitted, omitted);
            let installed =
                merge_java_window_settings(None, &parsed.contents.unwrap(), &Default::default())
                    .unwrap()
                    .unwrap();
            let installed: serde_json::Value = serde_json::from_slice(&installed).unwrap();
            assert_eq!(installed["snippet_appearance"], expected);
        }
    }
}
