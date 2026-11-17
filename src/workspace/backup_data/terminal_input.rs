//! Additive migration of Java terminal selection and paste settings.

use std::io;

use roxmltree::Node;
use serde::{Deserialize, Serialize};

use crate::connection_store::MAX_PASTE_LINE_DELAY_MS;
use crate::terminal_view::quick_select::{self, Preferences as QuickSelect};
use crate::workspace::paste_protection::{MAX_PASTE_KIB, WarningMode};

use super::{RawFields, java_flag, merge_unset_section, preference, preference_value};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Preferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_copy_on_select_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    paste_warning: Option<WarningMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    paste_large_warning_kib: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    paste_line_delay_ms: Option<u16>,
    quick_select: Selection,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    #[serde(skip_serializing_if = "Option::is_none")]
    alphabet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    patterns: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    java_patterns: Option<bool>,
}

fn patterns(root: Node<'_, '_>, omitted: &mut usize) -> Option<Vec<String>> {
    let mut lists = root.children().filter(|node| {
        node.has_tag_name("terminalQuickSelectPatterns") && node.tag_name().namespace().is_none()
    });
    let list = lists.next()?;
    if lists.next().is_some()
        || list.attributes().len() != 0
        || list
            .children()
            .filter_map(|node| node.text().filter(|_| node.is_text()))
            .any(|text| !text.trim().is_empty())
    {
        *omitted += 1;
        return None;
    }
    let mut patterns = Vec::new();
    let before = *omitted;
    for entry in list.children().filter(Node::is_element) {
        let mut contents = entry.children();
        let text = contents
            .next()
            .filter(Node::is_text)
            .and_then(|node| node.text());
        if !entry.has_tag_name("pattern")
            || entry.tag_name().namespace().is_some()
            || entry.attributes().len() != 0
            || contents.next().is_some()
            || text.is_none() && entry.has_children()
        {
            *omitted += 1;
            continue;
        }
        let Some(text) = text.filter(|text| !text.trim().is_empty()) else {
            continue;
        };
        if patterns.len() < quick_select::MAX_PATTERNS
            && QuickSelect::validate_pattern(text, true).is_ok()
        {
            patterns.push(text.to_owned());
        } else {
            *omitted += 1;
        }
    }
    (!patterns.is_empty() || before == *omitted).then_some(patterns)
}

impl Preferences {
    pub(super) fn read(root: Node<'_, '_>, omitted: &mut usize) -> Option<Self> {
        let alphabet = match preference_value(root, "terminalQuickSelectAlphabet", true) {
            Ok(Some(value)) if QuickSelect::validate_alphabet(value.trim()).is_ok() => {
                Some(value.trim().to_owned())
            }
            Ok(None) => None,
            _ => {
                *omitted += 1;
                None
            }
        };
        let patterns = patterns(root, omitted);
        let settings = Self {
            terminal_copy_on_select_enabled: preference(
                root,
                "terminalCopyOnSelectEnabled",
                omitted,
                java_flag,
            ),
            paste_warning: preference(root, "pasteWarningMode", omitted, |text| {
                match text.to_ascii_lowercase().as_str() {
                    "off" => Some(WarningMode::Off),
                    "unless-bracketed" => Some(WarningMode::UnlessBracketed),
                    "always" => Some(WarningMode::Always),
                    _ => None,
                }
            }),
            paste_large_warning_kib: preference(root, "pasteLargeWarningKiB", omitted, |text| {
                text.parse().ok().filter(|&value| value <= MAX_PASTE_KIB)
            }),
            paste_line_delay_ms: preference(root, "pasteLineDelayMs", omitted, |text| {
                text.parse()
                    .ok()
                    .filter(|&value| value <= MAX_PASTE_LINE_DELAY_MS)
            }),
            quick_select: Selection {
                alphabet,
                java_patterns: patterns
                    .as_ref()
                    .filter(|patterns| !patterns.is_empty())
                    .map(|_| true),
                patterns,
            },
        };
        (settings.count() != 0).then_some(settings)
    }

    pub(super) fn paste_warning_off(&self) -> bool {
        self.paste_warning == Some(WarningMode::Off)
    }

    pub(super) fn count(&self) -> usize {
        [
            self.terminal_copy_on_select_enabled.is_some(),
            self.paste_warning.is_some(),
            self.paste_large_warning_kib.is_some(),
            self.paste_line_delay_ms.is_some(),
            self.quick_select.alphabet.is_some(),
        ]
        .into_iter()
        .filter(|set| *set)
        .count()
            + self
                .quick_select
                .patterns
                .as_ref()
                .map_or(0, |patterns| patterns.len().max(1))
    }

    pub(super) fn merge(&self, fields: &mut RawFields) -> io::Result<bool> {
        if self
            .paste_large_warning_kib
            .is_some_and(|value| value > MAX_PASTE_KIB)
            || self
                .paste_line_delay_ms
                .is_some_and(|value| value > MAX_PASTE_LINE_DELAY_MS)
            || self
                .quick_select
                .patterns
                .as_ref()
                .is_some_and(|patterns| patterns.len() > quick_select::MAX_PATTERNS)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid Java terminal input settings",
            ));
        }
        let mut incoming: RawFields = serde_json::from_slice(&serde_json::to_vec(self)?)?;
        incoming.remove("quick_select");
        let before = fields.len();
        for (name, value) in incoming {
            fields.entry(name).or_insert(value);
        }
        let mut selection = self.quick_select.clone();
        let saved: RawFields = fields
            .get("quick_select")
            .map(|value| serde_json::from_str(value.get()))
            .transpose()?
            .unwrap_or_default();
        if saved.contains_key("patterns") {
            selection.java_patterns = None;
        }
        let selection_added = merge_unset_section(fields, "quick_select", &selection)?;
        Ok(selection_added || fields.len() != before)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{merge_java_window_settings, portable_java_preferences};

    #[test]
    fn java_terminal_input_preserves_pattern_order_and_omits_incompatible_syntax() {
        let source = br#"<globalSettings>
            <pasteLargeWarningKiB>100</pasteLargeWarningKiB>
            <pasteLineDelayMs>1001</pasteLineDelayMs>
            <terminalQuickSelectAlphabet>xy</terminalQuickSelectAlphabet>
            <terminalQuickSelectPatterns>
                <pattern>\d+-code</pattern>
                <pattern>(a)\12</pattern>
                <pattern>a++</pattern>
                <pattern>(?x) TKT - \d+ \# ID</pattern>
                <pattern>\X-ID</pattern>
                <pattern>\p{javaLowerCase}</pattern>
                <pattern>\p{javaUnknownProperty}</pattern>
                <pattern>value$</pattern>
                <pattern>last&lt;tag&gt;</pattern>
            </terminalQuickSelectPatterns>
        </globalSettings>"#;
        let incoming = portable_java_preferences(source, None).unwrap();
        assert_eq!((incoming.count, incoming.omitted), (9, 3));
        let incoming = incoming.contents.unwrap();
        let active = merge_java_window_settings(None, &incoming, &Default::default())
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&active).unwrap(),
            serde_json::json!({
                "quick_select":{"alphabet":"xy","java_patterns":true,"patterns":[r"\d+-code",r"(a)\12","a++",r"(?x) TKT - \d+ \# ID",r"\X-ID",r"\p{javaLowerCase}","value$","last<tag>"]}
            })
        );
        assert!(
            merge_java_window_settings(Some(&active), &incoming, &Default::default())
                .unwrap()
                .is_none()
        );
        let saved = serde_json::json!({"quick_select":{"patterns":[r"\w+"]}});
        let active = merge_java_window_settings(
            Some(&serde_json::to_vec(&saved).unwrap()),
            &incoming,
            &Default::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&active).unwrap(),
            serde_json::json!({"quick_select":{"alphabet":"xy","patterns":[r"\w+"]}})
        );
    }
}
