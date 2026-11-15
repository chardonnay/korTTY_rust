//! Existing settings parsers used to verify portable profile backups.

use std::collections::BTreeMap;
use std::io;

use roxmltree::{Document, Node, ParsingOptions};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use zeroize::Zeroizing;

use crate::terminal_view::background_image::AssetId;

use super::{
    chrome,
    environment_access::Catalog,
    tab_titles::{
        MAX_WORKFLOW_FONT, MIN_WORKFLOW_FONT, REMOTE_SIDEBAR_MAX_WIDTH, REMOTE_SIDEBAR_MIN_WIDTH,
        WindowSettings,
    },
};

const DISPLAY_SECTION: &str = "java_display_preferences";
const JAVA_SETTINGS_BYTES: usize = 4 * 1024 * 1024;
const JAVA_SETTINGS_NODES: u32 = 65_536;
const JAVA_MANUAL_SCALE: std::ops::RangeInclusive<u16> = 80..=160;

type RawFields = BTreeMap<String, Box<RawValue>>;

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct JavaDisplayPreferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    open_tool_windows_as_tabs: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    connection_color_border_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    split_pane_focus_ring_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tab_switch_most_recent_first: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    show_terminal_scrollbar: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hide_terminal_scrollbars_in_fullscreen: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_script_font: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    manual_scale: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_sidebar_width: Option<u16>,
}

impl JavaDisplayPreferences {
    fn fields(&self) -> io::Result<RawFields> {
        if self
            .workflow_script_font
            .is_some_and(|size| !(MIN_WORKFLOW_FONT..=MAX_WORKFLOW_FONT).contains(&size))
            || self
                .manual_scale
                .is_some_and(|scale| !JAVA_MANUAL_SCALE.contains(&scale))
            || self.remote_sidebar_width.is_some_and(|width| {
                !(REMOTE_SIDEBAR_MIN_WIDTH..=REMOTE_SIDEBAR_MAX_WIDTH).contains(&width)
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Java display preferences are outside their supported range",
            ));
        }
        Ok(serde_json::from_slice(&serde_json::to_vec(self)?)?)
    }
}

pub(crate) struct JavaDisplayImport {
    pub(crate) contents: Option<Vec<u8>>,
    pub(crate) count: usize,
    pub(crate) omitted: usize,
}

pub(crate) fn validate(name: &str, bytes: &[u8]) -> io::Result<()> {
    match name {
        "window-settings.json" => WindowSettings::validate_backup(bytes),
        "toolbar.json" => chrome::validate_toolbar_backup(bytes),
        "environments.json" => Catalog::validate_backup(bytes),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported portable settings file",
        )),
    }
}

pub(crate) fn background_ids(bytes: &[u8]) -> io::Result<Vec<AssetId>> {
    WindowSettings::backup_background_ids(bytes)
}

pub(crate) fn portable_java_themes(
    bytes: &[u8],
) -> io::Result<(Vec<u8>, crate::terminal_theme::java::Import)> {
    let mut settings = WindowSettings::default();
    let library = crate::terminal_theme::java::decode(bytes)?;
    let report = crate::terminal_theme::java::merge(&mut settings.themes, library)?;
    let bytes = serde_json::to_vec(&serde_json::json!({"themes": settings.themes}))?;
    WindowSettings::validate_backup(&bytes)?;
    Ok((bytes, report))
}

pub(crate) fn portable_java_display_preferences(
    bytes: &[u8],
    themes: Option<&[u8]>,
) -> io::Result<JavaDisplayImport> {
    if bytes.len() > JAVA_SETTINGS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Java display settings exceed their size limit",
        ));
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
    let document = Document::parse_with_options(
        text,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: JAVA_SETTINGS_NODES,
            entity_resolver: None,
        },
    )
    .map_err(io::Error::other)?;
    let root = document.root_element();
    if root.tag_name().namespace().is_some() || !root.has_tag_name("globalSettings") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Expected Java global settings",
        ));
    }
    let mut omitted = 0;
    let manual_scale = match preference_text(root, "uiFontScaleAuto") {
        Ok(None) | Ok(Some("false" | "0")) => {
            preference(root, "uiFontScalePercent", &mut omitted, |text| {
                text.parse()
                    .ok()
                    .filter(|scale| JAVA_MANUAL_SCALE.contains(scale))
            })
        }
        Ok(Some("true" | "1")) => None,
        _ => {
            omitted += 1;
            None
        }
    };
    let mut flag = |name| {
        preference(root, name, &mut omitted, |text| match text {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        })
    };
    let display = JavaDisplayPreferences {
        open_tool_windows_as_tabs: flag("openToolWindowsAsTabs"),
        connection_color_border_enabled: flag("connectionColorBorderEnabled"),
        split_pane_focus_ring_enabled: flag("splitPaneFocusRingEnabled"),
        tab_switch_most_recent_first: flag("tabSwitchMostRecentFirst"),
        show_terminal_scrollbar: flag("showTerminalScrollbar"),
        hide_terminal_scrollbars_in_fullscreen: flag("hideTerminalScrollbarsInFullscreen"),
        workflow_script_font: preference(root, "workflowScriptFontSize", &mut omitted, |text| {
            text.parse()
                .ok()
                .filter(|size| (MIN_WORKFLOW_FONT..=MAX_WORKFLOW_FONT).contains(size))
        }),
        manual_scale,
        remote_sidebar_width: preference(
            root,
            "terminalRemoteSidebarWidth",
            &mut omitted,
            |text| {
                text.parse::<f64>()
                    .ok()
                    .filter(|width| {
                        (f64::from(REMOTE_SIDEBAR_MIN_WIDTH)..=f64::from(REMOTE_SIDEBAR_MAX_WIDTH))
                            .contains(width)
                    })
                    .map(|width| width.round() as u16)
            },
        ),
    };
    let count = display.fields()?.len();
    let contents = if count == 0 {
        None
    } else {
        let mut fields: RawFields = themes
            .map(serde_json::from_slice)
            .transpose()?
            .unwrap_or_default();
        // Only this typed section can add display settings. Older pending theme snapshots
        // serialized a full set of Rust defaults around their themes.
        fields.insert(
            DISPLAY_SECTION.into(),
            serde_json::value::to_raw_value(&display)?,
        );
        let contents = serde_json::to_vec(&fields)?;
        WindowSettings::validate_backup(&contents)?;
        Some(contents)
    };
    Ok(JavaDisplayImport {
        contents,
        count,
        omitted,
    })
}

fn preference_text<'a>(root: Node<'a, '_>, name: &str) -> Result<Option<&'a str>, ()> {
    let mut elements = root
        .children()
        .filter(|node| node.has_tag_name(name) && node.tag_name().namespace().is_none());
    let Some(element) = elements.next() else {
        return Ok(None);
    };
    let mut contents = element.children();
    if elements.next().is_some() || element.attributes().next().is_some() {
        return Err(());
    }
    let text = contents
        .next()
        .filter(|node| node.is_text())
        .and_then(|node| node.text())
        .ok_or(())?;
    if contents.next().is_some() {
        return Err(());
    }
    Ok(Some(text.trim()))
}

fn preference<T>(
    root: Node<'_, '_>,
    name: &str,
    omitted: &mut usize,
    parse: impl FnOnce(&str) -> Option<T>,
) -> Option<T> {
    let parsed = match preference_text(root, name) {
        Ok(None) => return None,
        Ok(Some(text)) => parse(text),
        Err(()) => None,
    };
    if parsed.is_none() {
        *omitted += 1;
    }
    parsed
}

pub(crate) fn merge_java_window_settings(
    local: Option<&[u8]>,
    incoming: &[u8],
) -> io::Result<Option<Vec<u8>>> {
    let mut settings = local
        .map(WindowSettings::decode)
        .transpose()?
        .unwrap_or_default();
    let incoming_settings = WindowSettings::decode(incoming)?;
    let incoming_fields = raw_fields(incoming)?;
    let display: JavaDisplayPreferences = incoming_fields
        .get(DISPLAY_SECTION)
        .map(|value| serde_json::from_str(value.get()))
        .transpose()?
        .unwrap_or_default();
    let mut fields = match local {
        Some(local) if is_object(local) => serde_json::from_slice(local)?,
        Some(local)
            if serde_json::from_slice::<Vec<&RawValue>>(local)
                .is_ok_and(|legacy| !legacy.is_empty()) =>
        {
            serde_json::from_slice(&serde_json::to_vec(&settings)?)?
        }
        _ => RawFields::new(),
    };
    let themes_added =
        crate::terminal_theme::java::merge_backup(&mut settings.themes, incoming_settings.themes)?
            .added
            != 0;
    if themes_added {
        fields.insert(
            "themes".into(),
            serde_json::value::to_raw_value(&settings.themes)?,
        );
    }
    let removed_marker = fields.remove(DISPLAY_SECTION).is_some();
    let before = fields.len();
    for (key, value) in display.fields()? {
        fields.entry(key).or_insert(value);
    }
    if !themes_added && !removed_marker && before == fields.len() {
        return Ok(None);
    }
    let result = serde_json::to_vec(&fields)?;
    WindowSettings::validate_backup(&result)?;
    Ok(Some(result))
}

fn raw_fields(bytes: &[u8]) -> io::Result<RawFields> {
    if is_object(bytes) {
        Ok(serde_json::from_slice(bytes)?)
    } else {
        Ok(RawFields::new())
    }
}

fn is_object(bytes: &[u8]) -> bool {
    bytes.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'{')
}

pub(crate) fn keep_legacy_job_journal_retention(
    bytes: &[u8],
) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
    let mut settings = WindowSettings::decode(bytes)?;
    if settings.job_scheduler_journal_retention_days == 0 {
        return Ok(None);
    }
    let replacement = if bytes.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'{') {
        // Keep fields from other versions when projecting this one preference.
        let mut fields: BTreeMap<String, &RawValue> = serde_json::from_slice(bytes)?;
        fields.insert(
            "job_scheduler_journal_retention_days".into(),
            serde_json::from_str("0")?,
        );
        serde_json::to_vec(&fields)?
    } else {
        settings.job_scheduler_journal_retention_days = 0;
        serde_json::to_vec(&settings)?
    };
    let replacement = Zeroizing::new(replacement);
    WindowSettings::validate_backup(&replacement)?;
    Ok(Some(replacement))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_manual_scale_requires_explicit_percent_and_disabled_auto() {
        for (auto, expected, omitted) in [
            ("", Some(131), 0),
            ("<uiFontScaleAuto>false</uiFontScaleAuto>", Some(131), 0),
            ("<uiFontScaleAuto>true</uiFontScaleAuto>", None, 0),
            ("<uiFontScaleAuto>maybe</uiFontScaleAuto>", None, 1),
            (
                "<uiFontScaleAuto>false</uiFontScaleAuto><uiFontScaleAuto>false</uiFontScaleAuto>",
                None,
                1,
            ),
        ] {
            let xml = format!(
                "<globalSettings>{auto}<uiFontScalePercent>131</uiFontScalePercent></globalSettings>"
            );
            let import = portable_java_display_preferences(xml.as_bytes(), None).unwrap();
            assert_eq!(import.omitted, omitted);
            let active = import
                .contents
                .map(|bytes| merge_java_window_settings(None, &bytes).unwrap().unwrap());
            let fields: RawFields = active
                .as_deref()
                .map(serde_json::from_slice)
                .transpose()
                .unwrap()
                .unwrap_or_default();
            assert_eq!(
                fields
                    .get("manual_scale")
                    .map(|value| serde_json::from_str::<u16>(value.get()).unwrap()),
                expected
            );
            assert!(!fields.contains_key(DISPLAY_SECTION));
        }
    }

    #[test]
    fn previous_java_theme_snapshot_defaults_do_not_migrate_as_display_choices() {
        let incoming = serde_json::to_vec(&WindowSettings::default()).unwrap();
        assert!(
            merge_java_window_settings(None, &incoming)
                .unwrap()
                .is_none()
        );
        let xml = br#"<globalSettings><showTerminalScrollbar>false</showTerminalScrollbar><terminalRemoteSidebarWidth>1200.5</terminalRemoteSidebarWidth><workflowScriptFontSize>49</workflowScriptFontSize><uiFontScalePercent>161</uiFontScalePercent><openToolWindowsAsTabs>true</openToolWindowsAsTabs><openToolWindowsAsTabs>false</openToolWindowsAsTabs></globalSettings>"#;
        let incoming = portable_java_display_preferences(xml, None).unwrap();
        assert_eq!((incoming.count, incoming.omitted), (1, 4));
        let active = merge_java_window_settings(Some(b"[]"), &incoming.contents.unwrap())
            .unwrap()
            .unwrap();
        let fields: serde_json::Value = serde_json::from_slice(&active).unwrap();
        assert_eq!(
            fields,
            serde_json::json!({"show_terminal_scrollbar": false})
        );
    }
}
