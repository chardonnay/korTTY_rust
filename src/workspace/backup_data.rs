//! Existing settings parsers used to verify portable profile backups.

use std::collections::BTreeMap;
use std::io;

use roxmltree::{Document, Node, ParsingOptions};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use zeroize::Zeroizing;

use crate::session_journal::markers;
use crate::terminal_view::background_image::AssetId;

use super::{
    chrome, coding_agent_dock,
    environment_access::Catalog,
    key_bindings::java::Overrides as JavaShortcuts,
    tab_titles::{
        MAX_TERMINAL_SILENCE_SECONDS, MAX_WORKFLOW_FONT, MIN_TERMINAL_SILENCE_SECONDS,
        MIN_WORKFLOW_FONT, REMOTE_SIDEBAR_MAX_WIDTH, REMOTE_SIDEBAR_MIN_WIDTH, WindowSettings,
    },
    terminal_notifications,
};

pub(crate) use chrome::java_toolbar;
mod editor;
mod journal;
pub(crate) mod recording;
mod sftp;
mod startup;
mod terminal_input;

const DISPLAY_SECTION: &str = "java_display_preferences";
const JAVA_SETTINGS_BYTES: usize = 4 * 1024 * 1024;
const JAVA_SETTINGS_NODES: u32 = 65_536;
const JAVA_MANUAL_SCALE: std::ops::RangeInclusive<u16> = 80..=160;
const TERMINAL_TRANSPARENCY_PERCENT: std::ops::RangeInclusive<u8> = 0..=100;
const TERMINAL_SILENCE_SECONDS: std::ops::RangeInclusive<u16> =
    MIN_TERMINAL_SILENCE_SECONDS..=MAX_TERMINAL_SILENCE_SECONDS;

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
    tab_title_from_shell_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command_timestamps_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_link_detection_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    job_menu_status_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet_analysis_auto_diagram: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet_analysis_auto_modularization: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet_analysis_export_include_code: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_transparency: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_silence_seconds: Option<u16>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_sidebar_position: Option<DockPosition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    coding_agent_detection_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    coding_agent_app_badge_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    coding_agent_notifications_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    coding_agent_dock_automatic: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    coding_agent_dock: Option<JavaCodingDock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_notifications: Option<JavaTerminalNotifications>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sftp: Option<sftp::Preferences>,
    #[serde(skip_serializing_if = "Option::is_none")]
    journal: Option<journal::Preferences>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_input: Option<terminal_input::Preferences>,
    #[serde(skip_serializing_if = "Option::is_none")]
    startup: Option<startup::Preferences>,
    #[serde(skip_serializing_if = "Option::is_none")]
    editor: Option<editor::Preferences>,
    #[serde(skip_serializing_if = "Option::is_none")]
    logfile_presentation: Option<JavaLogfilePreferences>,
    #[serde(skip_serializing_if = "Option::is_none")]
    key_bindings: Option<JavaShortcuts>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JavaLogfilePreferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    links: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    keywords: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    keyword_set: Option<String>,
}

impl JavaLogfilePreferences {
    fn read(root: Node<'_, '_>, omitted: &mut usize) -> Option<Self> {
        let settings = Self {
            links: preference(root, "logfileViewerHighlightLinks", omitted, java_flag),
            keywords: preference(root, "logfileViewerHighlightKeywords", omitted, java_flag),
            keyword_set: preference(root, "logfileViewerKeywordSetId", omitted, |id| {
                crate::terminal_highlight::valid_id(id).then(|| id.to_owned())
            }),
        };
        (settings.count() != 0).then_some(settings)
    }

    fn count(&self) -> usize {
        usize::from(self.links.is_some())
            + usize::from(self.keywords.is_some())
            + usize::from(self.keyword_set.is_some())
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum DockPosition {
    Hidden,
    Left,
    Right,
}

impl DockPosition {
    fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "hidden" => Some(Self::Hidden),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            _ => None,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JavaCodingDock {
    #[serde(skip_serializing_if = "Option::is_none")]
    placement: Option<DockPosition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    side: Option<DockPosition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<u16>,
}

impl JavaCodingDock {
    fn width_range() -> std::ops::RangeInclusive<u16> {
        coding_agent_dock::DEFAULT_WIDTH..=coding_agent_dock::MAX_WIDTH
    }

    fn read(root: Node<'_, '_>, omitted: &mut usize) -> Option<Self> {
        let placement = preference(
            root,
            "codingAgentPanelPlacement",
            omitted,
            DockPosition::parse,
        );
        let side = match preference_text(root, "codingAgentPanelSide") {
            Ok(None) => placement.filter(|value| !matches!(value, DockPosition::Hidden)),
            _ => preference(root, "codingAgentPanelSide", omitted, |text| {
                DockPosition::parse(text).filter(|value| !matches!(value, DockPosition::Hidden))
            }),
        };
        let width = preference(root, "codingAgentPanelWidth", omitted, |text| {
            let value: f64 = text.parse().ok()?;
            let range = Self::width_range();
            (f64::from(*range.start())..=f64::from(*range.end()))
                .contains(&value)
                .then(|| value.round() as u16)
        });
        let settings = Self {
            placement,
            side,
            width,
        };
        (settings.count() != 0).then_some(settings)
    }

    fn count(&self) -> usize {
        [
            self.placement.is_some(),
            self.side.is_some(),
            self.width.is_some(),
        ]
        .into_iter()
        .filter(|set| *set)
        .count()
    }

    fn merge(&self, fields: &mut RawFields) -> io::Result<bool> {
        if matches!(self.side, Some(DockPosition::Hidden))
            || self
                .width
                .is_some_and(|width| !Self::width_range().contains(&width))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid Java Coding Agents panel settings",
            ));
        }
        merge_unset_section(fields, "coding_agent_dock", self)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JavaTerminalNotifications {
    #[serde(skip_serializing_if = "Option::is_none")]
    bell: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command_finished: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command_seconds: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    programs: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_runs: Option<bool>,
}

impl JavaTerminalNotifications {
    fn read(root: Node<'_, '_>, omitted: &mut usize) -> Option<Self> {
        let mut flag = |name| preference(root, name, omitted, java_flag);
        let notifications = Self {
            bell: flag("terminalBellNotificationsEnabled"),
            command_finished: flag("commandFinishedNotificationsEnabled"),
            programs: flag("remoteTerminalNotificationsEnabled"),
            ai_runs: flag("aiRunToastsEnabled"),
            command_seconds: preference(
                root,
                "commandFinishedNotificationSeconds",
                omitted,
                |text| {
                    text.parse().ok().filter(|seconds| {
                        terminal_notifications::Preferences::COMMAND_SECONDS.contains(seconds)
                    })
                },
            ),
        };
        (notifications.count() != 0).then_some(notifications)
    }

    fn count(&self) -> usize {
        [
            self.bell.is_some(),
            self.command_finished.is_some(),
            self.command_seconds.is_some(),
            self.programs.is_some(),
            self.ai_runs.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count()
    }

    fn merge(&self, fields: &mut RawFields) -> io::Result<bool> {
        if self.command_seconds.is_some_and(|seconds| {
            !terminal_notifications::Preferences::COMMAND_SECONDS.contains(&seconds)
        }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid Java command notification duration",
            ));
        }
        merge_unset_section(fields, "terminal_notifications", self)
    }
}

fn merge_unset_section(
    fields: &mut RawFields,
    name: &str,
    section: &impl Serialize,
) -> io::Result<bool> {
    let mut local: RawFields = fields
        .get(name)
        .map(|value| serde_json::from_str(value.get()))
        .transpose()?
        .unwrap_or_default();
    let before = local.len();
    let incoming: RawFields = serde_json::from_slice(&serde_json::to_vec(section)?)?;
    for (key, value) in incoming {
        local.entry(key).or_insert(value);
    }
    if local.len() == before {
        return Ok(false);
    }
    fields.insert(name.into(), serde_json::value::to_raw_value(&local)?);
    Ok(true)
}

impl JavaDisplayPreferences {
    fn fields(&self) -> io::Result<RawFields> {
        if self
            .workflow_script_font
            .is_some_and(|size| !(MIN_WORKFLOW_FONT..=MAX_WORKFLOW_FONT).contains(&size))
            || self
                .terminal_transparency
                .is_some_and(|value| !TERMINAL_TRANSPARENCY_PERCENT.contains(&value))
            || self
                .terminal_silence_seconds
                .is_some_and(|seconds| !TERMINAL_SILENCE_SECONDS.contains(&seconds))
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
        let mut fields: RawFields = serde_json::from_slice(&serde_json::to_vec(self)?)?;
        fields.remove("coding_agent_dock");
        fields.remove("terminal_notifications");
        fields.remove("sftp");
        fields.remove("journal");
        fields.remove("terminal_input");
        fields.remove("startup");
        fields.remove("editor");
        fields.remove("logfile_presentation");
        fields.remove("key_bindings");
        Ok(fields)
    }
}

pub(crate) struct JavaPreferences {
    pub(crate) contents: Option<Vec<u8>>,
    pub(crate) count: usize,
    pub(crate) omitted: usize,
    pub(crate) recording: recording::Import,
    pub(crate) markers: markers::java::Import,
    pub(crate) highlights: io::Result<crate::terminal_highlight::JavaBackupImport>,
    pub(crate) toolbar: Option<java_toolbar::Source>,
    pub(crate) sftp_overwrite: bool,
    pub(crate) paste_warning_off: bool,
    pub(crate) analysis_export_includes_code: bool,
}

pub(crate) fn validate(name: &str, bytes: &[u8]) -> io::Result<()> {
    match name {
        "window-settings.json" => WindowSettings::validate_backup(bytes),
        java_toolbar::FILE => chrome::validate_toolbar_backup(bytes),
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

pub(crate) fn portable_java_preferences(
    bytes: &[u8],
    themes: Option<&[u8]>,
) -> io::Result<JavaPreferences> {
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
    let recording = recording::read(root)?;
    let markers = markers::java::read(root)?;
    let mut omitted = 0;
    let icon_size = preference(
        root,
        "toolbarIconSize",
        &mut omitted,
        java_toolbar::icon_size,
    );
    let toolbar = java_toolbar::read(root, icon_size, &mut omitted);
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
    let coding_agent_dock = JavaCodingDock::read(root, &mut omitted);
    let terminal_notifications = JavaTerminalNotifications::read(root, &mut omitted);
    let sftp = sftp::Preferences::read(root, &mut omitted);
    let journal = journal::Preferences::read(root, &mut omitted);
    let terminal_input = terminal_input::Preferences::read(root, &mut omitted);
    let startup = startup::Preferences::read(root, &mut omitted);
    let editor = editor::Preferences::read(root, &mut omitted);
    let logfile_presentation = JavaLogfilePreferences::read(root, &mut omitted);
    let paste_warning_off = terminal_input
        .as_ref()
        .is_some_and(terminal_input::Preferences::paste_warning_off);
    let sftp_overwrite = sftp.as_ref().is_some_and(sftp::Preferences::overwrites);
    let key_bindings = JavaShortcuts::read(root, &mut omitted);
    let mut flag = |name| preference(root, name, &mut omitted, java_flag);
    let display = JavaDisplayPreferences {
        open_tool_windows_as_tabs: flag("openToolWindowsAsTabs"),
        connection_color_border_enabled: flag("connectionColorBorderEnabled"),
        split_pane_focus_ring_enabled: flag("splitPaneFocusRingEnabled"),
        tab_switch_most_recent_first: flag("tabSwitchMostRecentFirst"),
        tab_title_from_shell_enabled: flag("tabTitleFromShellEnabled"),
        command_timestamps_enabled: flag("commandTimestampsEnabled"),
        terminal_link_detection_enabled: flag("terminalLinkDetectionEnabled"),
        job_menu_status_enabled: flag("jobSchedulerMenuStatusEnabled"),
        snippet_analysis_auto_diagram: flag("codeAnalysisDiagramAutoGenerate"),
        snippet_analysis_auto_modularization: flag("codeAnalysisProposeModularization"),
        snippet_analysis_export_include_code: flag("snippetAnalysisExportIncludeCode"),
        show_terminal_scrollbar: flag("showTerminalScrollbar"),
        hide_terminal_scrollbars_in_fullscreen: flag("hideTerminalScrollbarsInFullscreen"),
        coding_agent_detection_enabled: flag("codingAgentDetectionEnabled"),
        coding_agent_app_badge_enabled: flag("codingAgentAppBadgeEnabled"),
        coding_agent_notifications_enabled: flag("codingAgentNotificationsEnabled"),
        coding_agent_dock_automatic: flag("codingAgentPanelAutoVisibility"),
        terminal_transparency: preference(
            root,
            "terminalBackgroundTransparency",
            &mut omitted,
            |text| {
                text.parse()
                    .ok()
                    .filter(|value| TERMINAL_TRANSPARENCY_PERCENT.contains(value))
            },
        ),
        terminal_silence_seconds: preference(
            root,
            "terminalSilenceSeconds",
            &mut omitted,
            |text| {
                text.parse()
                    .ok()
                    .filter(|seconds| TERMINAL_SILENCE_SECONDS.contains(seconds))
            },
        ),
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
        remote_sidebar_position: preference(
            root,
            "terminalRemoteSidebarPosition",
            &mut omitted,
            DockPosition::parse,
        ),
        coding_agent_dock,
        terminal_notifications,
        sftp,
        journal,
        terminal_input,
        startup,
        editor,
        logfile_presentation,
        key_bindings,
    };
    let count = display.fields()?.len()
        + display
            .coding_agent_dock
            .as_ref()
            .map_or(0, JavaCodingDock::count)
        + display
            .terminal_notifications
            .as_ref()
            .map_or(0, JavaTerminalNotifications::count)
        + display.sftp.as_ref().map_or(0, sftp::Preferences::count)
        + display
            .journal
            .as_ref()
            .map_or(0, journal::Preferences::count)
        + display
            .terminal_input
            .as_ref()
            .map_or(0, terminal_input::Preferences::count)
        + display
            .startup
            .as_ref()
            .map_or(0, startup::Preferences::count)
        + display
            .editor
            .as_ref()
            .map_or(0, editor::Preferences::count)
        + display
            .logfile_presentation
            .as_ref()
            .map_or(0, JavaLogfilePreferences::count)
        + display.key_bindings.as_ref().map_or(0, JavaShortcuts::len);
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
    Ok(JavaPreferences {
        contents,
        count,
        omitted,
        recording,
        markers,
        highlights: crate::terminal_highlight::read_java_backup(root),
        toolbar,
        sftp_overwrite,
        paste_warning_off,
        analysis_export_includes_code: display.snippet_analysis_export_include_code == Some(true),
    })
}

fn java_flag(text: &str) -> Option<bool> {
    match text {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

fn preference_text<'a>(root: Node<'a, '_>, name: &str) -> Result<Option<&'a str>, ()> {
    preference_value(root, name, false).map(|text| text.map(str::trim))
}

fn preference_value<'a>(
    root: Node<'a, '_>,
    name: &str,
    allow_empty: bool,
) -> Result<Option<&'a str>, ()> {
    let Some(element) = preference_node(root, name)? else {
        return Ok(None);
    };
    let mut contents = element.children();
    let Some(value) = contents.next() else {
        return if allow_empty { Ok(Some("")) } else { Err(()) };
    };
    let text = value
        .is_text()
        .then_some(value)
        .and_then(|node| node.text())
        .ok_or(())?;
    if contents.next().is_some() {
        return Err(());
    }
    Ok(Some(text))
}

fn preference_node<'a, 'input>(
    root: Node<'a, 'input>,
    name: &str,
) -> Result<Option<Node<'a, 'input>>, ()> {
    let mut elements = root
        .children()
        .filter(|node| node.has_tag_name(name) && node.tag_name().namespace().is_none());
    let Some(element) = elements.next() else {
        return Ok(None);
    };
    if elements.next().is_some() || element.attributes().next().is_some() {
        return Err(());
    }
    Ok(Some(element))
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
    highlight_set_ids: &BTreeMap<String, String>,
) -> io::Result<Option<Vec<u8>>> {
    let mut settings = local
        .map(WindowSettings::decode)
        .transpose()?
        .unwrap_or_default();
    let incoming_settings = WindowSettings::decode(incoming)?;
    let incoming_fields = raw_fields(incoming)?;
    let mut display: JavaDisplayPreferences = incoming_fields
        .get(DISPLAY_SECTION)
        .map(|value| serde_json::from_str(value.get()))
        .transpose()?
        .unwrap_or_default();
    if let Some(settings) = &mut display.logfile_presentation
        && let Some(id) = settings.keyword_set.take()
    {
        settings.keyword_set = if crate::terminal_highlight::builtins()
            .iter()
            .any(|set| set.id == id)
        {
            Some(id)
        } else {
            highlight_set_ids.get(&id).cloned()
        };
    }
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
    let added = before != fields.len();
    let dock_added = display
        .coding_agent_dock
        .as_ref()
        .map(|dock| dock.merge(&mut fields))
        .transpose()?
        .unwrap_or(false);
    let notifications_added = display
        .terminal_notifications
        .as_ref()
        .map(|notifications| notifications.merge(&mut fields))
        .transpose()?
        .unwrap_or(false);
    let sftp_added = display
        .sftp
        .as_ref()
        .map(|sftp| sftp.merge(&mut fields))
        .transpose()?
        .unwrap_or(false);
    let journal_added = display
        .journal
        .as_ref()
        .map(|journal| journal.merge(&mut fields))
        .transpose()?
        .unwrap_or(false);
    let input_added = display
        .terminal_input
        .as_ref()
        .map(|settings| settings.merge(&mut fields))
        .transpose()?
        .unwrap_or(false);
    let startup_added = display
        .startup
        .as_ref()
        .map(|settings| settings.merge(&mut fields))
        .transpose()?
        .unwrap_or(false);
    let editor_added = display
        .editor
        .as_ref()
        .map(|settings| settings.merge(&mut fields))
        .transpose()?
        .unwrap_or(false);
    let logfile_added = display
        .logfile_presentation
        .as_ref()
        .map(|settings| merge_unset_section(&mut fields, "logfile_presentation", settings))
        .transpose()?
        .unwrap_or(false);
    let shortcuts_added = display
        .key_bindings
        .as_ref()
        .is_some_and(|shortcuts| shortcuts.merge(&mut settings.key_bindings));
    if shortcuts_added {
        fields.insert(
            "key_bindings".into(),
            serde_json::value::to_raw_value(&settings.key_bindings)?,
        );
    }
    if !themes_added
        && !removed_marker
        && !added
        && !dock_added
        && !notifications_added
        && !sftp_added
        && !journal_added
        && !input_added
        && !startup_added
        && !editor_added
        && !logfile_added
        && !shortcuts_added
    {
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
    fn java_recording_preferences_follow_replay_capture_and_exclude_encoder_paths() {
        let xml = br#"<globalSettings>
            <terminalRecordingEnabled>1</terminalRecordingEnabled>
            <terminalRecordingFormat>WEBM</terminalRecordingFormat>
            <terminalRecordingDefaultScope>WHOLE_TAB</terminalRecordingDefaultScope>
            <terminalRecordingIdlePauseSeconds>4000</terminalRecordingIdlePauseSeconds>
            <terminalRecordingFfmpegPath>C:\fixture\ffmpeg</terminalRecordingFfmpegPath>
        </globalSettings>"#;
        let recording = portable_java_preferences(xml, None).unwrap().recording;
        assert_eq!(
            (recording.count, recording.omitted, recording.paths),
            (3, 0, 1)
        );
        let converted = recording.contents.unwrap();
        let bytes = recording::merge(None, &converted).unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::json!({
                "enabled_after_restart": true, "scope": "whole_tab", "idle_seconds": 3600
            })
        );
        assert!(recording::validate(br#"{"ffmpeg":"/fixture/ffmpeg"}"#).is_err());
        assert!(recording::validate(br#"{"idle_seconds":3601}"#).is_err());
    }

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
            let import = portable_java_preferences(xml.as_bytes(), None).unwrap();
            assert_eq!(import.omitted, omitted);
            let active = import.contents.map(|bytes| {
                merge_java_window_settings(None, &bytes, &Default::default())
                    .unwrap()
                    .unwrap()
            });
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
            merge_java_window_settings(None, &incoming, &Default::default())
                .unwrap()
                .is_none()
        );
        let xml = br#"<globalSettings>
            <showTerminalScrollbar>false</showTerminalScrollbar>
            <terminalRemoteSidebarWidth>1200.5</terminalRemoteSidebarWidth>
            <workflowScriptFontSize>49</workflowScriptFontSize>
            <uiFontScalePercent>161</uiFontScalePercent>
            <terminalBackgroundTransparency>101</terminalBackgroundTransparency>
            <terminalSilenceSeconds>4</terminalSilenceSeconds>
            <commandFinishedNotificationSeconds>3601</commandFinishedNotificationSeconds>
            <openToolWindowsAsTabs>true</openToolWindowsAsTabs>
            <openToolWindowsAsTabs>false</openToolWindowsAsTabs>
        </globalSettings>"#;
        let incoming = portable_java_preferences(xml, None).unwrap();
        assert_eq!((incoming.count, incoming.omitted), (1, 7));
        let active = merge_java_window_settings(
            Some(b"[]"),
            &incoming.contents.unwrap(),
            &Default::default(),
        )
        .unwrap()
        .unwrap();
        let fields: serde_json::Value = serde_json::from_slice(&active).unwrap();
        assert_eq!(
            fields,
            serde_json::json!({"show_terminal_scrollbar": false})
        );
    }
}
