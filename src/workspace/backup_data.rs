//! Existing settings parsers used to verify portable profile backups.

use std::collections::BTreeMap;
use std::io;

use roxmltree::{Document, Node, ParsingOptions};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use zeroize::Zeroizing;

use crate::diagram_window::appearance as diagram_appearance;
use crate::scheduler_window::DiagramColorMode;
use crate::session_journal::automation::Options as SwarmJournalOptions;
use crate::session_journal::markers;
use crate::snippet_window::analysis::{MAX_REPORT_FONT, MIN_REPORT_FONT};
use crate::terminal_view::background_image::AssetId;

use super::{
    appearance, chrome, coding_agent_dock,
    environment_access::Catalog,
    key_bindings::java::Overrides as JavaShortcuts,
    tab_titles::{
        MAX_MANUAL_SCALE, MAX_TERMINAL_SILENCE_SECONDS, MAX_WORKFLOW_FONT, MIN_MANUAL_SCALE,
        MIN_TERMINAL_SILENCE_SECONDS, MIN_WORKFLOW_FONT, REMOTE_SIDEBAR_MAX_WIDTH,
        REMOTE_SIDEBAR_MIN_WIDTH, WindowSettings,
    },
    terminal_agent::design::{DockPlacement as AgentDockPlacement, DockPreferences as AgentDock},
    terminal_notifications,
};

pub(crate) use chrome::java_toolbar;
pub(super) mod dialog_geometry;
mod editor;
mod hardening;
mod journal;
mod maintenance;
pub(crate) mod recording;
mod sftp;
mod snippet_languages;
mod startup;
pub(super) mod teamwork;
mod terminal_defaults;
mod terminal_input;

const DISPLAY_SECTION: &str = "java_display_preferences";
const JAVA_SETTINGS_BYTES: usize = 4 * 1024 * 1024;
const JAVA_SETTINGS_NODES: u32 = 65_536;
const JAVA_UI_FONT_SCALE: std::ops::RangeInclusive<u16> = 80..=160;
const TERMINAL_TRANSPARENCY_PERCENT: std::ops::RangeInclusive<u8> = 0..=100;
const TERMINAL_SILENCE_SECONDS: std::ops::RangeInclusive<u16> =
    MIN_TERMINAL_SILENCE_SECONDS..=MAX_TERMINAL_SILENCE_SECONDS;

type RawFields = BTreeMap<String, Box<RawValue>>;

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct JavaDisplayPreferences {
    #[serde(skip_serializing_if = "appearance::Preferences::is_default")]
    appearance: appearance::Preferences,
    #[serde(skip_serializing_if = "Option::is_none")]
    journal_live_log_height: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    journal_edit_split: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    journal_manager_geometry: Option<super::journal_access::dialog_geometry::Geometry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    journal_viewer_geometry: Option<super::journal_access::dialog_geometry::Geometry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    journal_screenshot_geometry: Option<super::journal_access::dialog_geometry::Geometry>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    java_dialog_geometry: dialog_geometry::Placements,
    #[serde(skip_serializing_if = "JavaLocalDock::is_empty")]
    java_local_dock: JavaLocalDock,
    #[serde(skip_serializing_if = "AgentDock::is_empty")]
    ai_agent_panel: AgentDock,
    #[serde(skip_serializing_if = "Option::is_none")]
    scheduler_journal_divider: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    maintenance: Option<maintenance::Preferences>,
    #[serde(skip_serializing_if = "Option::is_none")]
    java_teamwork: Option<teamwork::Proposals>,
    #[serde(skip_serializing_if = "Option::is_none")]
    java_swarm_journal: Option<SwarmJournalOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open_tool_windows_as_tabs: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    connection_color_border_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    split_pane_focus_ring_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tab_switch_most_recent_first: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skip_terminal_group_close_confirmation: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tab_title_from_shell_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    automatic_local_shell_integration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command_timestamps_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_link_detection_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_drag_drop: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    job_menu_status_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    journal_note_translation_language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet_translation_target_language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet_alternative_count: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scheduler_diagram_horizontal: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scheduler_diagram_color_mode: Option<DiagramColorMode>,
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
    ascii_art_preview_font_size: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet_analysis_report_font_size: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet_analysis_history_limit: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet_history_limit: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet_analysis_content_limit: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet_analysis_hardening_expanded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet_analysis_diagram_options_expanded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet_analysis_project_diagram_visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet_diagram_horizontal: Option<bool>,
    #[serde(skip_serializing_if = "diagram_appearance::Preferences::is_empty")]
    snippet_diagram_appearance: diagram_appearance::Preferences,
    #[serde(skip_serializing_if = "crate::snippet_window::structure::Preferences::is_default")]
    snippet_structure: crate::snippet_window::structure::Preferences,
    #[serde(
        skip_serializing_if = "crate::snippet_window::analysis::panel::Preferences::is_default"
    )]
    snippet_analysis_panel: crate::snippet_window::analysis::panel::Preferences,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet_apply_progress_dock: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet_hardening: Option<hardening::Preferences>,
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
    terminal_defaults: Option<terminal_defaults::Preferences>,
    #[serde(skip_serializing_if = "Option::is_none")]
    startup: Option<startup::Preferences>,
    #[serde(skip_serializing_if = "Option::is_none")]
    editor: Option<editor::Preferences>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    custom_snippet_code_languages: Vec<String>,
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum DockPosition {
    Hidden,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct JavaLocalDock {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) placement: Option<DockPosition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) width: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) show_hidden: Option<bool>,
}

impl JavaLocalDock {
    fn read(root: Node<'_, '_>, omitted: &mut usize) -> Self {
        Self {
            placement: preference(root, "fileBrowserPosition", omitted, DockPosition::parse),
            width: preference(root, "fileBrowserWidth", omitted, |value| {
                let width: f64 = value.parse().ok()?;
                width.is_finite().then(|| {
                    width
                        .clamp(
                            f64::from(super::local_dock::MIN_WIDTH),
                            f64::from(super::local_dock::MAX_WIDTH),
                        )
                        .round() as u16
                })
            }),
            show_hidden: preference(root, "fileBrowserShowHidden", omitted, java_flag),
        }
    }

    fn count(&self) -> usize {
        usize::from(self.placement.is_some())
            + usize::from(self.width.is_some())
            + usize::from(self.show_hidden.is_some())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.count() == 0
    }

    pub(super) fn validate(&self) -> io::Result<()> {
        if self.width.is_some_and(|width| {
            !(super::local_dock::MIN_WIDTH..=super::local_dock::MAX_WIDTH)
                .contains(&f32::from(width))
        }) {
            return Err(io::Error::other("Invalid Java local file browser width"));
        }
        Ok(())
    }
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
        if let Some(proposals) = &self.java_teamwork {
            proposals.validate()?;
        }
        if let Some(options) = &self.java_swarm_journal {
            validate_java_swarm_journal(options)?;
        }
        dialog_geometry::validate(&self.java_dialog_geometry)?;
        self.java_local_dock.validate()?;
        if !self.ai_agent_panel.valid() {
            return Err(io::Error::other("Invalid Java Agent panel width"));
        }
        if self
            .snippet_hardening
            .as_ref()
            .is_some_and(|settings| !settings.valid())
        {
            return Err(io::Error::other("Invalid Java snippet hardening defaults"));
        }
        if !crate::snippet_window::languages::valid(&self.custom_snippet_code_languages) {
            return Err(io::Error::other("Invalid Java snippet language catalogue"));
        }
        if self
            .workflow_script_font
            .is_some_and(|size| !(MIN_WORKFLOW_FONT..=MAX_WORKFLOW_FONT).contains(&size))
            || self.journal_edit_split.is_some_and(|split| {
                !super::journal_access::entry_editor::SPLIT_RANGE.contains(&split)
            })
            || self
                .terminal_transparency
                .is_some_and(|value| !TERMINAL_TRANSPARENCY_PERCENT.contains(&value))
            || self
                .terminal_silence_seconds
                .is_some_and(|seconds| !TERMINAL_SILENCE_SECONDS.contains(&seconds))
            || self
                .manual_scale
                .is_some_and(|scale| !(MIN_MANUAL_SCALE..=MAX_MANUAL_SCALE).contains(&scale))
            || self
                .ascii_art_preview_font_size
                .is_some_and(|size| !super::ascii_art::valid_preview_font(size))
            || self.snippet_alternative_count.is_some_and(|count| {
                !crate::snippet_window::ai_assist::ALTERNATIVE_COUNTS.contains(&count)
            })
            || self
                .snippet_analysis_report_font_size
                .is_some_and(|size| !(MIN_REPORT_FONT..=MAX_REPORT_FONT).contains(&size))
            || self.snippet_analysis_history_limit.is_some_and(|limit| {
                !crate::snippet_analysis::store::HISTORY_LIMITS.contains(&limit)
            })
            || self
                .snippet_history_limit
                .is_some_and(|limit| !crate::snippets::history::LIMITS.contains(&limit))
            || self
                .snippet_analysis_content_limit
                .is_some_and(|limit| !crate::snippet_analysis::model::valid_content_limit(limit))
            || self.snippet_structure.width.is_some_and(|width| {
                use crate::snippet_window::structure::{MAX_WIDTH, MIN_WIDTH};
                !(MIN_WIDTH..=MAX_WIDTH).contains(&width)
            })
            || self.snippet_analysis_panel.width.is_some_and(|width| {
                use crate::snippet_window::analysis::panel::{MAX_WIDTH, MIN_WIDTH};
                !(MIN_WIDTH..=MAX_WIDTH).contains(&width)
            })
            || self.remote_sidebar_width.is_some_and(|width| {
                !(REMOTE_SIDEBAR_MIN_WIDTH..=REMOTE_SIDEBAR_MAX_WIDTH).contains(&width)
            })
            || self
                .journal_note_translation_language
                .as_deref()
                .is_some_and(|language| {
                    !crate::session_journal::note_translation::valid_language(language)
                })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Java display preferences are outside their supported range",
            ));
        }
        let mut fields: RawFields = serde_json::from_slice(&serde_json::to_vec(self)?)?;
        fields.remove("appearance");
        fields.remove("coding_agent_dock");
        fields.remove("terminal_notifications");
        fields.remove("sftp");
        fields.remove("journal");
        fields.remove("terminal_input");
        fields.remove("terminal_defaults");
        fields.remove("startup");
        fields.remove("editor");
        fields.remove("snippet_diagram_appearance");
        fields.remove("snippet_structure");
        fields.remove("snippet_analysis_panel");
        fields.remove("snippet_hardening");
        fields.remove("custom_snippet_code_languages");
        fields.remove("logfile_presentation");
        fields.remove("key_bindings");
        fields.remove("maintenance");
        fields.remove("java_dialog_geometry");
        fields.remove("java_local_dock");
        fields.remove("ai_agent_panel");
        Ok(fields)
    }
}

pub(crate) struct JavaPreferences {
    pub(crate) teamwork_proposals: usize,
    pub(crate) swarm_journal_proposal: bool,
    pub(crate) java_update_opt_in: bool,
    pub(crate) java_log_directory: bool,
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
    let library = crate::terminal_theme::java::decode_backup(bytes)?;
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
    let journal_live_log_height = preference(
        root,
        "sessionJournalLiveTailHeightVh",
        &mut omitted,
        |value| {
            value
                .parse::<i32>()
                .ok()
                .map(super::journal_dock::clamp_live_log_height)
        },
    );
    let journal_edit_split = preference(
        root,
        "sessionJournalViewerEditDividerPosition",
        &mut omitted,
        |value| {
            let ratio: f64 = value.parse().ok()?;
            (0.0..=1.0)
                .contains(&ratio)
                .then(|| super::journal_access::entry_editor::split_parts(ratio))
        },
    );
    let maintenance = maintenance::Preferences::read(root, &mut omitted);
    let java_update_opt_in = maintenance
        .as_ref()
        .is_some_and(|value| value.enablement == Some(true));
    let java_log_directory = preference_text(root, "logDirectoryPath")
        .is_ok_and(|value| value.is_some_and(|path| !path.is_empty()));
    let icon_size = preference(
        root,
        "toolbarIconSize",
        &mut omitted,
        java_toolbar::icon_size,
    );
    let toolbar = java_toolbar::read(root, icon_size, &mut omitted);
    let manual_scale = java_guide_scale(root, &mut omitted);
    let ascii_art_preview_font_size =
        preference(root, "asciiArtPreviewFontSize", &mut omitted, |value| {
            super::ascii_art::clamp_preview_font(value.parse().ok()?)
        });
    let coding_agent_dock = JavaCodingDock::read(root, &mut omitted);
    let terminal_notifications = JavaTerminalNotifications::read(root, &mut omitted);
    let sftp = sftp::Preferences::read(root, &mut omitted);
    let journal = journal::Preferences::read(root, &mut omitted);
    let terminal_input = terminal_input::Preferences::read(root, &mut omitted);
    let terminal_defaults = terminal_defaults::Preferences::read(root, &mut omitted);
    let startup = startup::Preferences::read(root, &mut omitted);
    let editor = editor::Preferences::read(root, &mut omitted);
    let logfile_presentation = JavaLogfilePreferences::read(root, &mut omitted);
    let java_teamwork = teamwork::read(root, &mut omitted);
    let teamwork_proposals = java_teamwork.as_ref().map_or(0, teamwork::Proposals::count);
    let java_swarm_journal = match preference_node(root, "swarmSessionJournal") {
        Ok(None) => None,
        result => {
            let options = result
                .ok()
                .flatten()
                .and_then(crate::profile_backup::read_swarm_journal);
            omitted += usize::from(options.is_none());
            options
        }
    };
    let swarm_journal_proposal = java_swarm_journal.is_some();
    let paste_warning_off = terminal_input
        .as_ref()
        .is_some_and(terminal_input::Preferences::paste_warning_off);
    let sftp_overwrite = sftp.as_ref().is_some_and(sftp::Preferences::overwrites);
    let key_bindings = JavaShortcuts::read(root, &mut omitted);
    let journal_note_translation_language = preference(
        root,
        "sessionJournalNoteTranslationLanguage",
        &mut omitted,
        |language| {
            let language = language.trim();
            crate::session_journal::note_translation::valid_language(language)
                .then(|| language.to_owned())
        },
    );
    let snippet_translation_target_language = preference(
        root,
        "snippetTranslationTargetLanguage",
        &mut omitted,
        |value| crate::snippet_window::ai_assist::translation_language(value).map(str::to_owned),
    );
    let scheduler_diagram_color_mode = preference(
        root,
        "jobSchedulerDependencyDiagramColorMode",
        &mut omitted,
        |value| match value {
            "auto" => Some(DiagramColorMode::Auto),
            "light" => Some(DiagramColorMode::Light),
            "dark" => Some(DiagramColorMode::Dark),
            _ => None,
        },
    );
    let scheduler_journal_divider = preference(
        root,
        "jobSchedulerJournalDetailDividerPosition",
        &mut omitted,
        |value| {
            let ratio = value.parse::<f64>().ok()?;
            (ratio > 0.0 && ratio < 1.0)
                .then(|| crate::scheduler_window::journal_divider_parts(ratio))
                .flatten()
        },
    );
    let journal_manager_geometry =
        dialog_geometry::read_geometry(root, "sessionJournalManagerGeometry", &mut omitted);
    let journal_viewer_geometry =
        dialog_geometry::read_geometry(root, "sessionJournalViewerGeometry", &mut omitted);
    let journal_screenshot_geometry = dialog_geometry::read_geometry(
        root,
        "sessionJournalScreenshotEditorGeometry",
        &mut omitted,
    );
    let java_dialog_geometry = dialog_geometry::read(root, &mut omitted);
    let java_local_dock = JavaLocalDock::read(root, &mut omitted);
    let ai_agent_panel = AgentDock {
        placement: preference(
            root,
            "aiAgentPanelPlacement",
            &mut omitted,
            |value| match value.to_ascii_lowercase().as_str() {
                "bottom" => Some(AgentDockPlacement::Bottom),
                "left" => Some(AgentDockPlacement::Left),
                "right" => Some(AgentDockPlacement::Right),
                _ => None,
            },
        ),
        width: preference(root, "aiAgentPanelSideWidth", &mut omitted, |value| {
            let width: f64 = value.parse().ok()?;
            let range = AgentDock::WIDTH_RANGE;
            width.is_finite().then(|| {
                width
                    .clamp(f64::from(*range.start()), f64::from(*range.end()))
                    .round() as u16
            })
        }),
    };
    if !preference_value(root, "fileBrowserLastRoot", true)
        .is_ok_and(|path| path.is_none_or(|path| path.trim().is_empty()))
    {
        omitted += 1;
    }
    let appearance = appearance::Preferences {
        design: preference(root, "appDesign", &mut omitted, appearance::Design::from_id),
        animations: preference(root, "appDesignAnimationsEnabled", &mut omitted, java_flag),
    };
    let snippet_alternative_count = preference(
        root,
        "aiSnippetAlternativeSolutionCount",
        &mut omitted,
        |value| {
            let count = value.parse::<i32>().ok()?;
            let count = if count <= 0 {
                crate::snippet_window::ai_assist::DEFAULT_ALTERNATIVE_COUNT
            } else {
                count.min(i32::from(
                    *crate::snippet_window::ai_assist::ALTERNATIVE_COUNTS.end(),
                )) as u8
            };
            Some(count)
        },
    );
    let mut flag = |name| preference(root, name, &mut omitted, java_flag);
    let display = JavaDisplayPreferences {
        appearance,
        journal_live_log_height,
        journal_edit_split,
        journal_manager_geometry,
        journal_viewer_geometry,
        journal_screenshot_geometry,
        java_dialog_geometry,
        java_local_dock,
        ai_agent_panel,
        scheduler_journal_divider,
        maintenance,
        java_teamwork,
        java_swarm_journal,
        open_tool_windows_as_tabs: flag("openToolWindowsAsTabs"),
        connection_color_border_enabled: flag("connectionColorBorderEnabled"),
        split_pane_focus_ring_enabled: flag("splitPaneFocusRingEnabled"),
        tab_switch_most_recent_first: flag("tabSwitchMostRecentFirst"),
        skip_terminal_group_close_confirmation: flag(
            "closeActiveTerminalWindowsWithoutConfirmation",
        ),
        tab_title_from_shell_enabled: flag("tabTitleFromShellEnabled"),
        automatic_local_shell_integration: flag("shellIntegrationEnabled"),
        command_timestamps_enabled: flag("commandTimestampsEnabled"),
        terminal_link_detection_enabled: flag("terminalLinkDetectionEnabled"),
        terminal_drag_drop: flag("terminalDragDropEnabled"),
        job_menu_status_enabled: flag("jobSchedulerMenuStatusEnabled"),
        journal_note_translation_language,
        snippet_translation_target_language,
        snippet_alternative_count,
        scheduler_diagram_horizontal: flag("jobSchedulerDependencyDiagramHorizontal"),
        scheduler_diagram_color_mode,
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
        ascii_art_preview_font_size,
        snippet_analysis_report_font_size: preference(
            root,
            "codeAnalysisFontSize",
            &mut omitted,
            |text| {
                let size = text.parse::<i32>().ok()?;
                Some(size.clamp(i32::from(MIN_REPORT_FONT), i32::from(MAX_REPORT_FONT)) as u8)
            },
        ),
        snippet_analysis_history_limit: preference(
            root,
            "snippetAnalysisHistoryMaxSize",
            &mut omitted,
            |text| {
                use crate::snippet_analysis::store::{DEFAULT_HISTORY_LIMIT, HISTORY_LIMITS};

                let value = text.parse::<i32>().ok()?;
                if value <= 0 {
                    return Some(DEFAULT_HISTORY_LIMIT);
                }
                u8::try_from(value.min(i32::from(*HISTORY_LIMITS.end()))).ok()
            },
        ),
        snippet_history_limit: preference(root, "snippetHistoryMaxSize", &mut omitted, |text| {
            use crate::snippets::history::{DEFAULT_LIMIT, LIMITS};
            let value = text.parse::<i32>().ok()?;
            if value <= 0 {
                return Some(DEFAULT_LIMIT);
            }
            u8::try_from(value.min(i32::from(*LIMITS.end()))).ok()
        }),
        snippet_analysis_content_limit: preference(
            root,
            "snippetAnalysisMaxStoredContentBytes",
            &mut omitted,
            |text| {
                use crate::snippet_analysis::model::{
                    DEFAULT_CONTENT_BYTES, MAX_CONTENT_BYTES, MIN_CONTENT_BYTES,
                };

                match text.parse::<i64>().ok()? {
                    value if value < 0 => Some(DEFAULT_CONTENT_BYTES),
                    0 => Some(0),
                    value => usize::try_from(
                        value.clamp(MIN_CONTENT_BYTES as i64, MAX_CONTENT_BYTES as i64),
                    )
                    .ok(),
                }
            },
        ),
        snippet_analysis_hardening_expanded: preference(
            root,
            "codeAnalysisHardeningExpanded",
            &mut omitted,
            java_flag,
        ),
        snippet_analysis_diagram_options_expanded: preference(
            root,
            "codeAnalysisDiagramOptionsExpanded",
            &mut omitted,
            java_flag,
        ),
        snippet_analysis_project_diagram_visible: preference(
            root,
            "codeAnalysisProjectDiagramVisible",
            &mut omitted,
            java_flag,
        ),
        snippet_structure: crate::snippet_window::structure::Preferences {
            visible: preference(
                root,
                "snippetStructurePanelVisible",
                &mut omitted,
                java_flag,
            ),
            width: preference(root, "snippetStructurePanelWidth", &mut omitted, |text| {
                let width = text.parse::<f64>().ok()?;
                (width.is_finite() && width > 0.0).then(|| {
                    width.round().clamp(
                        f64::from(crate::snippet_window::structure::MIN_WIDTH),
                        800.0,
                    ) as u16
                })
            }),
        },
        snippet_analysis_panel: crate::snippet_window::analysis::panel::Preferences {
            visible: preference(root, "snippetAnalysisPanelVisible", &mut omitted, java_flag),
            width: preference(root, "snippetAnalysisPanelWidth", &mut omitted, |text| {
                use crate::snippet_window::analysis::panel::{MAX_WIDTH, MIN_WIDTH};
                let width = text.parse::<f64>().ok()?;
                (width.is_finite() && width > 0.0).then(|| {
                    width
                        .round()
                        .clamp(f64::from(MIN_WIDTH), f64::from(MAX_WIDTH))
                        as u16
                })
            }),
        },
        snippet_apply_progress_dock: preference(
            root,
            "snippetAiProgressDockVisible",
            &mut omitted,
            java_flag,
        ),
        snippet_diagram_horizontal: preference(
            root,
            "snippetDiagramLayout",
            &mut omitted,
            |value| match value {
                "horizontal" => Some(true),
                "vertical" => Some(false),
                _ => None,
            },
        ),
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
        snippet_diagram_appearance: diagram_appearance::Preferences {
            mode: preference(
                root,
                "snippetDiagramColorMode",
                &mut omitted,
                |value| match value {
                    "auto" => Some(diagram_appearance::Mode::Auto),
                    "light" => Some(diagram_appearance::Mode::Light),
                    "dark" => Some(diagram_appearance::Mode::Dark),
                    _ => None,
                },
            ),
            background: preference(
                root,
                "snippetDiagramBackgroundColor",
                &mut omitted,
                diagram_appearance::parse_rgb,
            ),
        },
        coding_agent_dock,
        terminal_notifications,
        sftp,
        journal,
        terminal_input,
        snippet_hardening: hardening::Preferences::read(root, &mut omitted),
        terminal_defaults,
        startup,
        editor,
        custom_snippet_code_languages: snippet_languages::read(root, &mut omitted),
        logfile_presentation,
        key_bindings,
    };
    let count = display.fields()?.len()
        + usize::from(display.appearance.design.is_some())
        + usize::from(display.appearance.animations.is_some())
        + usize::from(display.snippet_diagram_appearance.mode.is_some())
        + usize::from(display.snippet_diagram_appearance.background.is_some())
        + usize::from(display.snippet_structure.visible.is_some())
        + usize::from(display.snippet_structure.width.is_some())
        + usize::from(display.snippet_analysis_panel.visible.is_some())
        + usize::from(display.snippet_analysis_panel.width.is_some())
        + display
            .snippet_hardening
            .as_ref()
            .map_or(0, hardening::Preferences::count)
        + display.custom_snippet_code_languages.len()
        + display
            .terminal_defaults
            .as_ref()
            .map_or(0, terminal_defaults::Preferences::count)
        + display.java_dialog_geometry.len()
        + display.java_local_dock.count()
        + usize::from(display.ai_agent_panel.placement.is_some())
        + usize::from(display.ai_agent_panel.width.is_some())
        - usize::from(teamwork_proposals != 0)
        - usize::from(swarm_journal_proposal)
        + display
            .maintenance
            .as_ref()
            .map_or(0, maintenance::Preferences::count)
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
    let contents = if count == 0
        && teamwork_proposals == 0
        && !java_update_opt_in
        && !swarm_journal_proposal
    {
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
        teamwork_proposals,
        swarm_journal_proposal,
        java_update_opt_in,
        java_log_directory,
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

pub(super) fn validate_java_swarm_journal(options: &SwarmJournalOptions) -> io::Result<()> {
    if options.summary_profile.is_some()
        || options
            .unresolved_summary_profile
            .as_deref()
            .is_some_and(|id| crate::ai_manager::JavaProfileSource::parse_id(id) != Some(id))
    {
        return Err(io::Error::other(
            "Invalid Java Swarm journal profile reference",
        ));
    }
    options.validate()
}

pub(crate) fn associate_java_swarm_journal_profile<'a>(
    incoming: &[u8],
    sources: impl Iterator<Item = &'a crate::ai_manager::JavaProfileSource>,
) -> io::Result<Option<Vec<u8>>> {
    let mut fields = raw_fields(incoming)?;
    let Some(section) = fields.get_mut(DISPLAY_SECTION) else {
        return Ok(None);
    };
    let mut display: JavaDisplayPreferences = serde_json::from_str(section.get())?;
    let Some(options) = &mut display.java_swarm_journal else {
        return Ok(None);
    };
    let Some(id) = options.unresolved_summary_profile.as_deref() else {
        return Ok(None);
    };
    let mut matches = sources.filter(|source| source.id() == id);
    let Some(source) = matches.next().filter(|_| matches.next().is_none()) else {
        return Ok(None);
    };
    options.java_profile_source = Some(source.clone());
    validate_java_swarm_journal(options)?;
    *section = serde_json::value::to_raw_value(&display)?;
    Ok(Some(serde_json::to_vec(&fields)?))
}

fn java_guide_scale(root: Node<'_, '_>, omitted: &mut usize) -> Option<u16> {
    match preference_text(root, "guideFontScalePercent") {
        Ok(Some(text)) => {
            return match text.parse::<i32>() {
                Ok(scale) => Some(
                    scale.clamp(i32::from(MIN_MANUAL_SCALE), i32::from(MAX_MANUAL_SCALE)) as u16,
                ),
                Err(_) => {
                    *omitted += 1;
                    None
                }
            };
        }
        Err(()) => {
            *omitted += 1;
            return None;
        }
        Ok(None) => {}
    }
    // Older imports used the explicit manual UI scale as a fallback.
    match preference_text(root, "uiFontScaleAuto") {
        Ok(None) | Ok(Some("false" | "0")) => {
            preference(root, "uiFontScalePercent", omitted, |text| {
                text.parse()
                    .ok()
                    .filter(|scale| JAVA_UI_FONT_SCALE.contains(scale))
            })
        }
        Ok(Some("true" | "1")) => None,
        _ => {
            *omitted += 1;
            None
        }
    }
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
    let theme_report =
        crate::terminal_theme::java::merge_backup(&mut settings.themes, incoming_settings.themes)?;
    let themes_added = theme_report.added != 0 || theme_report.edited_built_in != 0;
    let has_local_base_colors = settings.colors.foreground.is_some()
        || settings.colors.background.is_some()
        || settings.colors.cursor.is_some();
    let themes_updated = terminal_defaults::merge_themes(
        &mut fields,
        &mut settings.themes,
        themes_added,
        display.terminal_defaults.as_ref(),
        has_local_base_colors,
    )?;
    let removed_marker = fields.remove(DISPLAY_SECTION).is_some();
    let before = fields.len();
    for (key, value) in display.fields()? {
        fields.entry(key).or_insert(value);
    }
    let added = before != fields.len();
    let appearance_added = !display.appearance.is_default()
        && merge_unset_section(&mut fields, "appearance", &display.appearance)?;
    let diagram_appearance_added = !display.snippet_diagram_appearance.is_empty()
        && merge_unset_section(
            &mut fields,
            "snippet_diagram_appearance",
            &display.snippet_diagram_appearance,
        )?;
    let snippet_hardening_added = display
        .snippet_hardening
        .as_ref()
        .map(|settings| merge_unset_section(&mut fields, "snippet_hardening", settings))
        .transpose()?
        .unwrap_or(false);
    let snippet_structure_added = !display.snippet_structure.is_default()
        && merge_unset_section(&mut fields, "snippet_structure", &display.snippet_structure)?;
    let analysis_panel_added = !display.snippet_analysis_panel.is_default()
        && merge_unset_section(
            &mut fields,
            "snippet_analysis_panel",
            &display.snippet_analysis_panel,
        )?;
    let dialog_geometry_added = !display.java_dialog_geometry.is_empty()
        && merge_unset_section(
            &mut fields,
            "java_dialog_geometry",
            &display.java_dialog_geometry,
        )?;
    let local_dock_added = !display.java_local_dock.is_empty()
        && merge_unset_section(&mut fields, "java_local_dock", &display.java_local_dock)?;
    let ai_agent_panel_added = !display.ai_agent_panel.is_empty()
        && merge_unset_section(&mut fields, "ai_agent_panel", &display.ai_agent_panel)?;
    let maintenance_added = display
        .maintenance
        .as_ref()
        .map(|settings| settings.merge(&mut fields))
        .transpose()?
        .unwrap_or(false);
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
    let terminal_defaults_added = display
        .terminal_defaults
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
    let languages_added =
        snippet_languages::merge(&mut fields, &display.custom_snippet_code_languages)?;
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
    if !themes_updated
        && !removed_marker
        && !added
        && !appearance_added
        && !diagram_appearance_added
        && !snippet_hardening_added
        && !snippet_structure_added
        && !analysis_panel_added
        && !dialog_geometry_added
        && !local_dock_added
        && !ai_agent_panel_added
        && !maintenance_added
        && !dock_added
        && !notifications_added
        && !sftp_added
        && !journal_added
        && !input_added
        && !terminal_defaults_added
        && !startup_added
        && !editor_added
        && !languages_added
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
    fn java_snippet_alternative_limit_preserves_local_choice() {
        for (value, expected) in [("0", 3), ("6", 6), ("11", 10)] {
            let xml = format!(
                "<globalSettings><aiSnippetAlternativeSolutionCount>{value}</aiSnippetAlternativeSolutionCount></globalSettings>"
            );
            let import = portable_java_preferences(xml.as_bytes(), None).unwrap();
            assert_eq!((import.count, import.omitted), (1, 0));
            let bytes = import.contents.unwrap();
            let restored = merge_java_window_settings(None, &bytes, &Default::default())
                .unwrap()
                .unwrap();
            assert_eq!(
                WindowSettings::decode(&restored)
                    .unwrap()
                    .snippet_alternative_count,
                Some(expected)
            );
            let local = br#"{"snippet_alternative_count":2}"#;
            assert!(
                merge_java_window_settings(Some(local), &bytes, &Default::default())
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn java_snippet_translation_language_keeps_native_choice() {
        let incoming = portable_java_preferences(
            "<globalSettings><snippetTranslationTargetLanguage> 日本語 </snippetTranslationTargetLanguage></globalSettings>".as_bytes(),
            None,
        ).unwrap();
        assert_eq!((incoming.count, incoming.omitted), (1, 0));
        let bytes = incoming.contents.unwrap();
        let restored = merge_java_window_settings(None, &bytes, &Default::default())
            .unwrap()
            .unwrap();
        assert_eq!(
            WindowSettings::decode(&restored)
                .unwrap()
                .snippet_translation_target_language
                .as_deref(),
            Some("日本語"),
        );
        let native = br#"{"snippet_translation_target_language":"French"}"#;
        assert!(
            merge_java_window_settings(Some(native), &bytes, &Default::default())
                .unwrap()
                .is_none()
        );
        let invalid = portable_java_preferences(
            br#"<globalSettings><snippetTranslationTargetLanguage>two&#10;lines</snippetTranslationTargetLanguage></globalSettings>"#,
            None,
        ).unwrap();
        assert_eq!((invalid.count, invalid.omitted), (0, 1));
    }

    #[test]
    fn java_ascii_art_zoom_preserves_fraction_and_native_setting() {
        fn import(value: &str) -> JavaPreferences {
            let xml = format!(
                "<globalSettings><asciiArtPreviewFontSize>{value}</asciiArtPreviewFontSize></globalSettings>"
            );
            portable_java_preferences(xml.as_bytes(), None).unwrap()
        }
        for (value, expected) in [("15.5", 15.5), ("2", 6.0), ("1e40", 40.0)] {
            let incoming = import(value);
            assert_eq!((incoming.count, incoming.omitted), (1, 0));
            let bytes = incoming.contents.unwrap();
            let restored = merge_java_window_settings(None, &bytes, &Default::default())
                .unwrap()
                .unwrap();
            assert_eq!(
                WindowSettings::decode(&restored)
                    .unwrap()
                    .ascii_art_preview_font_size,
                Some(expected)
            );
            for local in [&restored[..], br#"{"ascii_art_preview_font_size":19.0}"#] {
                assert!(
                    merge_java_window_settings(Some(local), &bytes, &Default::default())
                        .unwrap()
                        .is_none()
                );
            }
        }
        for value in ["NaN", "Infinity", "invalid"] {
            let incoming = import(value);
            assert_eq!((incoming.count, incoming.omitted), (0, 1));
        }
    }

    #[test]
    fn java_journal_translation_language_preserves_native_choice_and_rejects_invalid_text() {
        let incoming = portable_java_preferences(
            br#"<globalSettings><sessionJournalNoteTranslationLanguage> German </sessionJournalNoteTranslationLanguage></globalSettings>"#,
            None,
        ).unwrap();
        assert_eq!((incoming.count, incoming.omitted), (1, 0));
        let source = incoming.contents.unwrap();
        let restored = merge_java_window_settings(None, &source, &Default::default())
            .unwrap()
            .unwrap();
        assert_eq!(
            WindowSettings::decode(&restored)
                .unwrap()
                .journal_note_translation_language,
            "German"
        );
        let native = br#"{"journal_note_translation_language":"French"}"#;
        assert!(
            merge_java_window_settings(Some(native), &source, &Default::default())
                .unwrap()
                .is_none()
        );
        assert!(
            merge_java_window_settings(Some(&restored), &source, &Default::default())
                .unwrap()
                .is_none()
        );
        for language in ["", "line&#10;break"] {
            let xml = format!(
                "<globalSettings><sessionJournalNoteTranslationLanguage>{language}</sessionJournalNoteTranslationLanguage></globalSettings>"
            );
            let omitted = portable_java_preferences(xml.as_bytes(), None).unwrap();
            assert_eq!((omitted.count, omitted.omitted), (0, 1));
        }
    }

    #[test]
    fn java_live_log_height_import_clamps_and_preserves_native_preference() {
        let xml = br#"<globalSettings><sessionJournalLiveTailHeightVh>99</sessionJournalLiveTailHeightVh></globalSettings>"#;
        let import = portable_java_preferences(xml, None).unwrap();
        assert_eq!((import.count, import.omitted), (1, 0));
        let incoming = import.contents.unwrap();
        let restored = merge_java_window_settings(None, &incoming, &Default::default())
            .unwrap()
            .unwrap();
        assert_eq!(
            WindowSettings::decode(&restored)
                .unwrap()
                .journal_live_log_height,
            Some(85)
        );
        assert!(
            merge_java_window_settings(
                Some(br#"{"journal_live_log_height":30}"#),
                &incoming,
                &Default::default()
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn java_agent_dock_preserves_native_placement_and_imports_an_unset_width() {
        use super::super::terminal_agent::design::Slot;
        use crate::terminal_agent::design::{Arrangement, Layout};

        let imported = portable_java_preferences(
            br#"<globalSettings>
                <aiAgentPanelPlacement> LEFT </aiAgentPanelPlacement>
                <aiAgentPanelSideWidth>755</aiAgentPanelSideWidth>
            </globalSettings>"#,
            None,
        )
        .unwrap();
        assert_eq!((imported.count, imported.omitted), (2, 0));
        let incoming = imported.contents.unwrap();
        let restored = merge_java_window_settings(None, &incoming, &Default::default())
            .unwrap()
            .unwrap();
        let preferences = WindowSettings::decode(&restored).unwrap().ai_agent_panel;
        assert_eq!(preferences.placement, Some(AgentDockPlacement::Left));
        assert_eq!(preferences.width(), 600);
        assert_eq!(
            preferences.history_slot(Layout::default()),
            Some(Slot::Left)
        );
        let pip = Layout {
            arrangement: Arrangement::PictureInPicture,
            ..Layout::default()
        };
        assert_eq!(preferences.history_slot(pip), None);

        let native = br#"{"ai_agent_panel":{"placement":"design"}}"#;
        let restored = merge_java_window_settings(Some(native), &incoming, &Default::default())
            .unwrap()
            .unwrap();
        let preferences = WindowSettings::decode(&restored).unwrap().ai_agent_panel;
        assert_eq!(preferences.placement, Some(AgentDockPlacement::Design));
        assert_eq!(preferences.history_slot(Layout::default()), None);
        assert_eq!(preferences.width(), 600);
        assert!(
            merge_java_window_settings(Some(&restored), &incoming, &Default::default())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn java_journal_dialog_geometry_restores_as_separate_native_fallbacks() {
        let import = portable_java_preferences(
            br#"<globalSettings>
                <sessionJournalManagerGeometry>
                    <x>-920.6</x><y>95.2</y><width>1010</width><height>740</height>
                </sessionJournalManagerGeometry>
                <sessionJournalViewerGeometry>
                    <width>500</width><height>8000</height>
                </sessionJournalViewerGeometry>
                <sessionJournalScreenshotEditorGeometry>
                    <x>40</x><y>80</y><width>1100</width><height>730</height>
                    <maximized>true</maximized><fullScreen>true</fullScreen>
                </sessionJournalScreenshotEditorGeometry>
            </globalSettings>"#,
            None,
        )
        .unwrap();
        assert_eq!((import.count, import.omitted), (3, 0));
        let incoming = import.contents.unwrap();
        let restored = merge_java_window_settings(None, &incoming, &Default::default())
            .unwrap()
            .unwrap();
        let screenshot = WindowSettings::decode(&restored)
            .unwrap()
            .journal_screenshot_geometry
            .unwrap()
            .placement(iced::Size::new(620.0, 470.0));
        assert_eq!((screenshot.width, screenshot.height), (1100.0, 730.0));
        assert_eq!(
            screenshot.position,
            crate::profile::SavedPosition::new(40.0, 80.0)
        );
        let native =
            br#"{"journal_screenshot_geometry":{"width":900,"height":650,"position":null}}"#;
        let merged = merge_java_window_settings(Some(native), &incoming, &Default::default())
            .unwrap()
            .unwrap();
        let settings = WindowSettings::decode(&merged).unwrap();
        let minimum = iced::Size::new(620.0, 400.0);
        let manager = settings
            .journal_manager_geometry
            .unwrap()
            .placement(minimum);
        assert_eq!((manager.width, manager.height), (1010.0, 740.0));
        let position = manager.position.unwrap();
        assert_eq!(
            (position.x, position.y, position.scale_factor),
            (-921.0, 95.0, None)
        );
        let viewer = settings.journal_viewer_geometry.unwrap().placement(minimum);
        assert_eq!(
            (viewer.width, viewer.height, viewer.position),
            (620.0, 4096.0, None)
        );
        let screenshot = settings
            .journal_screenshot_geometry
            .unwrap()
            .placement(minimum);
        assert_eq!(
            (screenshot.width, screenshot.height, screenshot.position),
            (900.0, 650.0, None)
        );
        assert!(
            merge_java_window_settings(Some(&merged), &incoming, &Default::default())
                .unwrap()
                .is_none()
        );

        let invalid = portable_java_preferences(
            br#"<globalSettings>
                <sessionJournalManagerGeometry>
                    <width>NaN</width><height>600</height>
                </sessionJournalManagerGeometry>
                <sessionJournalViewerGeometry>
                    <x>50</x><x>60</x><y>70</y><width>900</width><height>800</height>
                </sessionJournalViewerGeometry>
            </globalSettings>"#,
            None,
        )
        .unwrap();
        assert_eq!((invalid.count, invalid.omitted), (0, 2));
        assert!(invalid.contents.is_none());
    }

    #[test]
    fn java_live_journal_dock_and_viewer_split_migrate_independently() {
        use super::super::{Workspace, journal_dock::Placement};

        let read = |body: &str| {
            portable_java_preferences(
                format!("<globalSettings>{body}</globalSettings>").as_bytes(),
                None,
            )
            .unwrap()
        };
        let import = read(
            "<journalLivePanelPlacement> Left </journalLivePanelPlacement>
             <journalLivePanelWidth>510.7</journalLivePanelWidth>
             <sessionJournalViewerEditDividerPosition>0.6897</sessionJournalViewerEditDividerPosition>",
        );
        assert_eq!((import.count, import.omitted), (3, 0));
        let incoming = import.contents.unwrap();
        let restored = merge_java_window_settings(None, &incoming, &Default::default())
            .unwrap()
            .unwrap();
        let mut workspace = Workspace {
            window_settings: WindowSettings::decode(&restored).unwrap(),
            ..Workspace::default()
        };
        assert_eq!(workspace.window_settings.journal_dock.width, 511);
        assert_eq!(workspace.window_settings.journal_edit_split, Some(6897));
        let (owner, _) = workspace.open_window(false);
        assert_eq!(workspace.journal_dock_placement(owner), Placement::Left);

        let local = br#"{"journal_dock":{"width":410},"journal_edit_split":6200}"#;
        let restored = merge_java_window_settings(Some(local), &incoming, &Default::default())
            .unwrap()
            .unwrap();
        let settings = WindowSettings::decode(&restored).unwrap();
        assert_eq!(settings.journal_dock.placement, Placement::Left);
        assert_eq!(settings.journal_dock.width, 410);
        assert_eq!(settings.journal_edit_split, Some(6200));
        let resaved = serde_json::to_vec(&settings).unwrap();
        assert!(
            merge_java_window_settings(Some(&resaved), &incoming, &Default::default())
                .unwrap()
                .is_none()
        );

        let partial = read(
            "<journalLivePanelPlacement>diagonal</journalLivePanelPlacement>
             <journalLivePanelWidth>2500</journalLivePanelWidth>
             <sessionJournalViewerEditDividerPosition>0.95</sessionJournalViewerEditDividerPosition>",
        );
        assert_eq!((partial.count, partial.omitted), (2, 1));
        let restored =
            merge_java_window_settings(None, &partial.contents.unwrap(), &Default::default())
                .unwrap()
                .unwrap();
        let settings = WindowSettings::decode(&restored).unwrap();
        assert_eq!(settings.journal_dock.placement, Placement::Hidden);
        assert_eq!(settings.journal_dock.width, 1600);
        assert_eq!(settings.journal_edit_split, Some(7400));

        let invalid = read(
            "<journalLivePanelWidth>NaN</journalLivePanelWidth>
             <sessionJournalViewerEditDividerPosition>0.5</sessionJournalViewerEditDividerPosition>
             <sessionJournalViewerEditDividerPosition>0.7</sessionJournalViewerEditDividerPosition>",
        );
        assert_eq!((invalid.count, invalid.omitted), (0, 2));
        assert!(invalid.contents.is_none());
    }

    #[test]
    fn java_scheduler_journal_divider_import_reopens_the_layout_and_preserves_native_choice() {
        let read = |value| {
            portable_java_preferences(
            format!("<globalSettings><jobSchedulerJournalDetailDividerPosition>{value}</jobSchedulerJournalDetailDividerPosition></globalSettings>").as_bytes(),
            None,
        ).unwrap()
        };
        let incoming = read("0.832").contents.unwrap();
        let restored = merge_java_window_settings(None, &incoming, &Default::default())
            .unwrap()
            .unwrap();
        let settings = WindowSettings::decode(&restored).unwrap();
        assert_eq!(settings.scheduler_journal_divider, Some(832));

        let profile = crate::snippets::tests::Profile::new();
        let scheduler =
            crate::scheduler_window::SchedulerWindow::open(profile.0.join("scheduler.json"))
                .unwrap();
        let mut view = scheduler.new_view();
        view.apply_journal_divider(settings.scheduler_journal_divider);
        assert_eq!(view.journal_divider(), Some(832));

        let native = br#"{"scheduler_journal_divider":300}"#;
        assert!(
            merge_java_window_settings(Some(native), &incoming, &Default::default())
                .unwrap()
                .is_none()
        );
        assert_eq!(read("NaN").omitted, 1);
        assert_eq!(read("1").omitted, 1);
        let incoming = read("0.98").contents.unwrap();
        let restored = merge_java_window_settings(None, &incoming, &Default::default())
            .unwrap()
            .unwrap();
        assert_eq!(
            WindowSettings::decode(&restored)
                .unwrap()
                .scheduler_journal_divider,
            Some(900)
        );
    }

    #[test]
    fn java_application_appearance_import_preserves_explicit_native_defaults() {
        let read = |design, animations| {
            let xml = format!(
                "<globalSettings><appDesign>{design}</appDesign><appDesignAnimationsEnabled>{animations}</appDesignAnimationsEnabled></globalSettings>"
            );
            portable_java_preferences(xml.as_bytes(), None).unwrap()
        };
        let imported = read("ATLANTAFX_CUPERTINO_LIGHT", "false");
        assert_eq!((imported.count, imported.omitted), (2, 0));
        let incoming = imported.contents.unwrap();
        let restore = |local: Option<&[u8]>| {
            let merged = merge_java_window_settings(local, &incoming, &Default::default())
                .unwrap()
                .unwrap();
            WindowSettings::decode(&merged).unwrap()
        };
        let fresh = restore(None);
        assert_eq!(
            fresh.appearance.design(),
            appearance::Design::AtlantaFxCupertinoLight
        );
        assert!(!fresh.appearance.animations());

        let local = WindowSettings::decode(br#"{"appearance":{"design":"normal"}}"#).unwrap();
        let serialized = serde_json::to_vec(&local).unwrap();
        let restored = restore(Some(&serialized));
        assert_eq!(restored.appearance.design(), appearance::Design::Normal);
        assert!(!restored.appearance.animations());
        let restored = restore(Some(br#"{"appearance":{"animations":true}}"#));
        assert_eq!(
            restored.appearance.design(),
            appearance::Design::AtlantaFxCupertinoLight
        );
        assert!(restored.appearance.animations());
        let saved = serde_json::to_vec(&restored).unwrap();
        assert!(
            merge_java_window_settings(Some(&saved), &incoming, &Default::default())
                .unwrap()
                .is_none()
        );

        let invalid = read("unknown", "maybe");
        assert_eq!((invalid.count, invalid.omitted), (0, 2));
    }

    #[test]
    fn java_scheduler_diagram_preferences_fill_only_unset_fields() {
        let xml = br#"<globalSettings>
            <jobSchedulerDependencyDiagramHorizontal>true</jobSchedulerDependencyDiagramHorizontal>
            <jobSchedulerDependencyDiagramColorMode>auto</jobSchedulerDependencyDiagramColorMode>
        </globalSettings>"#;
        let incoming = portable_java_preferences(xml, None).unwrap();
        assert_eq!((incoming.count, incoming.omitted), (2, 0));
        let incoming = incoming.contents.unwrap();
        let local = br#"{"scheduler_diagram_horizontal":false}"#;
        let restored = merge_java_window_settings(Some(local), &incoming, &Default::default())
            .unwrap()
            .unwrap();
        let settings = WindowSettings::decode(&restored).unwrap();
        assert_eq!(settings.scheduler_diagram_horizontal, Some(false));
        assert_eq!(
            settings.scheduler_diagram_color_mode,
            Some(DiagramColorMode::Auto)
        );
        assert!(
            merge_java_window_settings(Some(&restored), &incoming, &Default::default())
                .unwrap()
                .is_none()
        );
        let invalid = portable_java_preferences(
            br#"<globalSettings><jobSchedulerDependencyDiagramColorMode>sepia</jobSchedulerDependencyDiagramColorMode></globalSettings>"#,
            None,
        )
        .unwrap();
        assert_eq!((invalid.count, invalid.omitted), (0, 1));
    }

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
    fn java_manual_scale_fallback_requires_explicit_percent_and_disabled_auto() {
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
    fn java_analysis_presentation_clamps_font_and_merges_each_unset_field() {
        let xml = br#"<globalSettings>
            <codeAnalysisFontSize>99</codeAnalysisFontSize>
            <codeAnalysisHardeningExpanded>true</codeAnalysisHardeningExpanded>
        </globalSettings>"#;
        let imported = portable_java_preferences(xml, None).unwrap();
        assert_eq!((imported.count, imported.omitted), (2, 0));
        let restored = merge_java_window_settings(
            Some(br#"{"snippet_analysis_hardening_expanded":false}"#),
            &imported.contents.unwrap(),
            &Default::default(),
        )
        .unwrap()
        .unwrap();
        let settings = WindowSettings::decode(&restored).unwrap();
        assert_eq!(
            settings.snippet_analysis_report_font_size,
            Some(MAX_REPORT_FONT)
        );
        assert_eq!(settings.snippet_analysis_hardening_expanded, Some(false));
    }

    #[test]
    fn java_guide_zoom_takes_precedence_and_preserves_a_native_choice() {
        for (guide, expected, omitted) in [
            ("210", Some(210), 0),
            ("-10", Some(70), 0),
            ("1000", Some(250), 0),
            ("invalid", None, 1),
            (
                "200</guideFontScalePercent><guideFontScalePercent>190",
                None,
                1,
            ),
        ] {
            for automatic in ["true", "false"] {
                let xml = format!(
                    "<globalSettings>
                        <uiFontScaleAuto>{automatic}</uiFontScaleAuto>
                        <uiFontScalePercent>132</uiFontScalePercent>
                        <guideFontScalePercent>{guide}</guideFontScalePercent>
                    </globalSettings>"
                );
                let import = portable_java_preferences(xml.as_bytes(), None).unwrap();
                assert_eq!(import.omitted, omitted);
                assert_eq!(import.count, usize::from(expected.is_some()));
                if let Some(bytes) = import.contents {
                    let restored = merge_java_window_settings(None, &bytes, &Default::default())
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        Some(WindowSettings::decode(&restored).unwrap().manual_scale),
                        expected
                    );
                    assert!(
                        merge_java_window_settings(
                            Some(br#"{"manual_scale":177}"#),
                            &bytes,
                            &Default::default()
                        )
                        .unwrap()
                        .is_none()
                    );
                } else {
                    assert!(expected.is_none());
                }
            }
        }
    }

    #[test]
    fn java_backup_restores_edited_built_in_themes_at_their_original_ids() {
        use crate::connection_store::ThemeOverride;
        use crate::terminal_theme::{Id, Preset, Rgb};

        let (catalogue, report) = portable_java_themes(
            br##"<themes agentPaletteVersion="2">
                <theme id="dark"><name>Dark Mode</name><fontFamily>Monaco</fontFamily>
                    <fontSize>15</fontSize><backgroundColor>#1E1E1E</backgroundColor>
                    <builtIn>true</builtIn>
                </theme>
                <theme id="nord"><name>Edited Nord</name><fontFamily>Monaco</fontFamily>
                    <fontSize>21</fontSize><backgroundColor>#101234</backgroundColor>
                    <agentPanelAccentColor>#123456</agentPanelAccentColor>
                    <backgroundImagePath>/legacy/desktop.png</backgroundImagePath>
                    <builtIn>true</builtIn>
                </theme>
            </themes>"##,
        )
        .unwrap();
        assert_eq!(
            (report.added, report.built_in, report.edited_built_in),
            (0, 1, 1)
        );
        assert_eq!(report.limited, 1);
        let bytes = merge_java_window_settings(None, &catalogue, &Default::default())
            .unwrap()
            .unwrap();
        let saved = WindowSettings::decode(&bytes).unwrap();
        let id = Id::BuiltIn(Preset::Nord);
        let theme = saved.themes.find_java("nord").unwrap();
        assert_eq!(theme.id, id);
        assert_eq!(theme.name, "Edited Nord");
        assert_eq!(theme.scheme.font_size, 21);
        assert_eq!(theme.scheme.background, Rgb::from_hex(0x101234));
        assert_eq!(
            ThemeOverride::BuiltIn(Preset::Nord).scheme(&saved.themes),
            Some(theme.scheme)
        );
        assert_eq!(
            saved.themes.agent_panel_colors(id).unwrap()[4],
            Rgb::parse("#123456")
        );
        assert_eq!(
            saved.themes.java_background_path(id),
            Some("/legacy/desktop.png")
        );
        assert_eq!(saved.themes.background_asset(id), None);
        assert!(
            merge_java_window_settings(Some(&bytes), &catalogue, &Default::default())
                .unwrap()
                .is_none()
        );

        let mut local = WindowSettings::default();
        let stock_scheme = local.themes.find(id).unwrap().scheme;
        local
            .themes
            .store(Some(id), "Native Nord", stock_scheme)
            .unwrap();
        let native = serde_json::to_vec(&local).unwrap();
        assert!(
            merge_java_window_settings(Some(&native), &catalogue, &Default::default())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn java_global_theme_uses_the_restored_id_and_base_cursor() {
        use crate::terminal_theme::{Id, Preset, Rgb, Scheme};

        let (catalogue, _) = portable_java_themes(
            br##"<themes><theme id="java-work">
                <name>Work</name><foregroundColor>#123456</foregroundColor>
                <fontSize>22</fontSize><cursorStyle>BLINK_BLOCK</cursorStyle>
            </theme></themes>"##,
        )
        .unwrap();
        let mut local = WindowSettings::default();
        let native = local
            .themes
            .store(None, "Local", Scheme::default())
            .unwrap();
        local.themes.select(None);
        // A catalogue written without a display preference leaves selection and fonts unset.
        let mut local_fields: serde_json::Value = serde_json::to_value(&local).unwrap();
        let local_themes = local_fields["themes"].as_object_mut().unwrap();
        local_themes.remove("selected");
        local_themes.remove("apply_font");
        let local_fields = serde_json::to_vec(&local_fields).unwrap();
        let with_catalogue =
            merge_java_window_settings(Some(&local_fields), &catalogue, &Default::default())
                .unwrap()
                .unwrap();
        let import = portable_java_preferences(
            br#"<globalSettings><applyThemeFonts>true</applyThemeFonts>
                <defaultTerminalSettings><themeId>java-work</themeId>
                    <fontSize>13</fontSize><cursorStyle>STEADY_BAR</cursorStyle>
                </defaultTerminalSettings></globalSettings>"#,
            Some(&catalogue),
        )
        .unwrap();
        let incoming = import.contents.unwrap();
        let direct =
            merge_java_window_settings(Some(&local_fields), &incoming, &Default::default())
                .unwrap()
                .unwrap();
        let staged =
            merge_java_window_settings(Some(&with_catalogue), &incoming, &Default::default())
                .unwrap()
                .unwrap();
        assert_eq!(direct, staged);
        let restored = WindowSettings::decode(&direct).unwrap();
        let theme = restored.themes.find_java("java-work").unwrap();
        assert_ne!(theme.id, native);
        assert_eq!(restored.themes.selected(), Some(theme.id));
        assert!(restored.themes.apply_font);
        assert_eq!(restored.editor_appearance().size, 22);
        let appearance = restored.terminal_appearance();
        assert_eq!(appearance.foreground, Rgb::from_hex(0x123456));
        assert_eq!(appearance.cursor_shape, kortty_terminal::CursorShape::Bar);
        assert!(!appearance.cursor_blink);
        assert!(
            merge_java_window_settings(Some(&direct), &incoming, &Default::default())
                .unwrap()
                .is_none()
        );

        let builtin = portable_java_preferences(
            br#"<globalSettings><defaultTerminalSettings>
                <themeId>nord</themeId></defaultTerminalSettings></globalSettings>"#,
            None,
        )
        .unwrap()
        .contents
        .unwrap();
        let bytes = merge_java_window_settings(None, &builtin, &Default::default())
            .unwrap()
            .unwrap();
        let restored = WindowSettings::decode(&bytes).unwrap();
        assert_eq!(restored.themes.selected(), Some(Id::BuiltIn(Preset::Nord)));
        assert!(!restored.themes.apply_font);
    }

    #[test]
    fn java_global_theme_keeps_native_preferences_and_ignores_unresolved_ids() {
        use crate::terminal_theme::Rgb;

        for (native, selected) in [
            (
                br#"{"themes":{"selected":null,"apply_font":false}}"#.as_slice(),
                true,
            ),
            (br#"{"colors":{"foreground":[1,2,3]}}"#.as_slice(), false),
        ] {
            let imported = portable_java_preferences(
                br#"<globalSettings><applyThemeFonts>true</applyThemeFonts>
                    <defaultTerminalSettings><themeId>nord</themeId></defaultTerminalSettings>
                </globalSettings>"#,
                None,
            )
            .unwrap()
            .contents
            .unwrap();
            let merged =
                merge_java_window_settings(Some(native), &imported, &Default::default()).unwrap();
            assert_eq!(merged.is_none(), selected);
            let saved = WindowSettings::decode(merged.as_deref().unwrap_or(native)).unwrap();
            assert_eq!(saved.themes.selected(), None);
            assert_eq!(saved.themes.apply_font, !selected);
            if !selected {
                assert_eq!(
                    saved.terminal_appearance().foreground,
                    Rgb::from_hex(0x010203)
                );
            }
        }
        let unresolved = portable_java_preferences(
            br#"<globalSettings><defaultTerminalSettings>
                <themeId>NORD</themeId></defaultTerminalSettings></globalSettings>"#,
            None,
        )
        .unwrap()
        .contents
        .unwrap();
        assert!(
            merge_java_window_settings(None, &unresolved, &Default::default())
                .unwrap()
                .is_none()
        );
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
    #[test]
    fn java_swarm_journal_restores_an_inactive_proposal_with_exact_profile_provenance() {
        let read = |body: &str| {
            portable_java_preferences(
                format!("<globalSettings>{body}</globalSettings>").as_bytes(),
                None,
            )
            .unwrap()
        };
        let xml = "<swarmSessionJournal>
            <enabled>true</enabled><captureInput>true</captureInput>
            <aiMode>ON_FAILURE</aiMode><aiProfileId>legacy-profile</aiProfileId>
            <keepMode>ONLY_ON_FAILURE</keepMode>
            <retentionMode>FIXED_DATE</retentionMode><expiryDate>2027-06-28</expiryDate>
            <maxJournals>12</maxJournals><maxStorageMb>128</maxStorageMb>
            <dedupEnabled>false</dedupEnabled>
        </swarmSessionJournal>";
        let import = read(xml);
        assert!(import.swarm_journal_proposal);
        assert_eq!((import.count, import.omitted), (0, 0));
        let incoming = import.contents.unwrap();
        let fingerprint = [42_u8; 32];
        let source: crate::ai_manager::JavaProfileSource =
            serde_json::from_value(serde_json::json!({
                "id": "legacy-profile",
                "fingerprint": fingerprint,
            }))
            .unwrap();
        assert!(
            associate_java_swarm_journal_profile(&incoming, [&source, &source].into_iter())
                .unwrap()
                .is_none()
        );
        let incoming = associate_java_swarm_journal_profile(&incoming, std::iter::once(&source))
            .unwrap()
            .unwrap();
        let restored = merge_java_window_settings(None, &incoming, &Default::default())
            .unwrap()
            .unwrap();
        let options = WindowSettings::decode(&restored)
            .unwrap()
            .java_swarm_journal
            .unwrap();
        assert!(options.enabled && options.capture_input);
        assert!(!options.deduplicate);
        assert_eq!(
            options.closing,
            crate::session_journal::automation::Closing::Failures
        );
        assert_eq!(
            options.keep,
            crate::session_journal::automation::Keep::Failures
        );
        assert_eq!(
            options.retention,
            crate::session_journal::automation::Retention::Date {
                date: "2027-06-28".into()
            }
        );
        assert_eq!((options.max_runs, options.max_storage_mib), (12, 128));
        assert_eq!(
            options.unresolved_summary_profile.as_deref(),
            Some("legacy-profile")
        );
        assert_eq!(options.java_profile_source.as_ref(), Some(&source));
        assert!(options.summary_profile.is_none());
        for proposal in [&incoming, &read("<swarmSessionJournal/>").contents.unwrap()] {
            assert!(
                merge_java_window_settings(Some(&restored), proposal, &Default::default())
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(read(&format!("{xml}{xml}")).omitted, 1);
        assert_eq!(read(&xml.replace("128", "99999999")).omitted, 1);
    }
}
