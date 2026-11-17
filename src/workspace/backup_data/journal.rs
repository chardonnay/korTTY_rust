//! Additive migration of Java journal display, capture format, and export preferences.

use std::io;

use roxmltree::Node;
use serde::{Deserialize, Serialize};

use crate::session_journal::{LogFormat, export_branding::brand_input};
use crate::terminal_theme::Rgb;
use crate::workspace::journal_appearance::{MAX_SCALE, MIN_SCALE, PageTheme, SchemeId};

use super::{RawFields, java_flag, merge_unset_section, preference, preference_value};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Preferences {
    appearance: Appearance,
    capture: Capture,
    branding: Branding,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Appearance {
    #[serde(skip_serializing_if = "Option::is_none")]
    scheme: Option<SchemeId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    theme: Option<PageTheme>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scale: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ui_font: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mono_font: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Capture {
    #[serde(skip_serializing_if = "Option::is_none")]
    clickable_links: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    log_format: Option<LogFormat>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Branding {
    #[serde(skip_serializing_if = "Option::is_none")]
    watermark_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    watermark_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    watermark_color: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    footer_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    footer_text: Option<String>,
}

fn read_text(
    root: Node<'_, '_>,
    name: &str,
    omitted: &mut usize,
    valid: impl FnOnce(&str) -> bool,
) -> Option<String> {
    match preference_value(root, name, true).map(|value| value.map(str::trim)) {
        Ok(Some(value)) if valid(value) => Some(value.to_owned()),
        Ok(None) => None,
        _ => {
            *omitted += 1;
            None
        }
    }
}

fn valid_font(value: &str) -> bool {
    value.len() <= 256 && !value.chars().any(char::is_control)
}

fn valid_brand(value: &str) -> bool {
    brand_input(value) == value
}

impl Preferences {
    pub(super) fn read(root: Node<'_, '_>, omitted: &mut usize) -> Option<Self> {
        let settings =
            Self {
                appearance: Appearance {
                    scheme: preference(root, "sessionJournalPageSchemeId", omitted, |text| {
                        match text {
                            "auto" => Some(SchemeId::Auto),
                            "theme" => Some(SchemeId::Theme),
                            "paper" => Some(SchemeId::Paper),
                            "midnight" => Some(SchemeId::Midnight),
                            "ocean" => Some(SchemeId::Ocean),
                            "forest" => Some(SchemeId::Forest),
                            "retrowave" => Some(SchemeId::Retrowave),
                            "high-contrast" => Some(SchemeId::HighContrast),
                            _ => None,
                        }
                    }),
                    theme: preference(root, "sessionJournalPageTheme", omitted, |text| match text
                        .to_ascii_lowercase()
                        .as_str()
                    {
                        "auto" => Some(PageTheme::Auto),
                        "light" => Some(PageTheme::Light),
                        "dark" => Some(PageTheme::Dark),
                        _ => None,
                    }),
                    scale: preference(root, "sessionJournalFontScalePercent", omitted, |text| {
                        text.parse()
                            .ok()
                            .filter(|scale| (MIN_SCALE..=MAX_SCALE).contains(scale))
                    }),
                    ui_font: read_text(root, "sessionJournalPageUiFont", omitted, valid_font),
                    mono_font: read_text(root, "sessionJournalPageMonoFont", omitted, valid_font),
                },
                capture: Capture {
                    clickable_links: preference(
                        root,
                        "sessionJournalClickableLinks",
                        omitted,
                        java_flag,
                    ),
                    log_format: preference(
                        root,
                        "sessionJournalLogFormat",
                        omitted,
                        LogFormat::parse_policy,
                    ),
                },
                branding: Branding {
                    watermark_enabled: preference(root, "pdfWatermarkEnabled", omitted, java_flag),
                    watermark_text: read_text(root, "pdfWatermarkText", omitted, valid_brand),
                    watermark_color: preference(root, "pdfWatermarkColor", omitted, Rgb::parse),
                    footer_enabled: preference(root, "exportFooterEnabled", omitted, java_flag),
                    footer_text: read_text(root, "exportFooterText", omitted, valid_brand),
                },
            };
        (settings.count() != 0).then_some(settings)
    }

    pub(super) fn count(&self) -> usize {
        [
            self.appearance.scheme.is_some(),
            self.appearance.theme.is_some(),
            self.appearance.scale.is_some(),
            self.appearance.ui_font.is_some(),
            self.appearance.mono_font.is_some(),
            self.capture.clickable_links.is_some(),
            self.capture.log_format.is_some(),
            self.branding.watermark_enabled.is_some(),
            self.branding.watermark_text.is_some(),
            self.branding.watermark_color.is_some(),
            self.branding.footer_enabled.is_some(),
            self.branding.footer_text.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count()
    }

    pub(super) fn merge(&self, fields: &mut RawFields) -> io::Result<bool> {
        if self
            .appearance
            .scale
            .is_some_and(|scale| !(MIN_SCALE..=MAX_SCALE).contains(&scale))
            || [
                self.appearance.ui_font.as_deref(),
                self.appearance.mono_font.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|value| !valid_font(value))
            || [
                self.branding.watermark_text.as_deref(),
                self.branding.footer_text.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|value| !valid_brand(value))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid Java journal preferences",
            ));
        }
        let appearance = merge_unset_section(fields, "journal_appearance", &self.appearance)?;
        let capture = merge_unset_section(fields, "session_journal", &self.capture)?;
        let branding = merge_unset_section(fields, "export_branding", &self.branding)?;
        Ok(appearance || capture || branding)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{merge_java_window_settings, portable_java_preferences};

    #[test]
    fn java_journal_preferences_reject_invalid_values_and_keep_explicit_defaults() {
        let source = br#"<globalSettings>
            <sessionJournalPageSchemeId>unavailable</sessionJournalPageSchemeId>
            <sessionJournalPageTheme>AUTO</sessionJournalPageTheme>
            <sessionJournalFontScalePercent>251</sessionJournalFontScalePercent>
            <sessionJournalPageUiFont/>
            <sessionJournalLogFormat>unknown</sessionJournalLogFormat>
            <pdfWatermarkColor>#FFF</pdfWatermarkColor>
            <exportFooterText/>
        </globalSettings>"#;
        let incoming = portable_java_preferences(source, None).unwrap();
        assert_eq!((incoming.count, incoming.omitted), (3, 4));
        let active =
            merge_java_window_settings(None, &incoming.contents.unwrap(), &Default::default())
                .unwrap()
                .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&active).unwrap(),
            serde_json::json!({
                "journal_appearance": {"theme":"auto", "ui_font":""},
                "export_branding": {"footer_text":""}
            })
        );
    }
}
