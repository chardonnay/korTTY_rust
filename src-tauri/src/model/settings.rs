use serde::{Deserialize, Serialize};

use super::ai::AiSkill;
use super::snippet::SnippetEditorProfile;
use super::terminal_recording::{TerminalRecordingFormat, TerminalRecordingScope};

/// Stable app-design IDs, mirroring the Java AppDesign enum.
pub const APP_DESIGN_IDS: [&str; 5] = [
    "normal",
    "matrix-terminal",
    "holographic-interface",
    "klingon-tactical",
    "elegant-dark",
];

pub const DEFAULT_LOG_RETENTION_DAYS: u32 = 7;
pub const MAX_LOG_RETENTION_DAYS: u32 = 3650;
pub const DEFAULT_UPDATE_CHECK_INTERVAL_DAYS: u32 = 1;
pub const MIN_UPDATE_CHECK_INTERVAL_DAYS: u32 = 1;
pub const MAX_UPDATE_CHECK_INTERVAL_DAYS: u32 = 30;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackupEncryptionType {
    Password,
    GPG,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TranslationProvider {
    Google,
    DeepL,
    LibreTranslate,
    Microsoft,
    Yandex,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TerminalAgentExecutionTarget {
    TerminalWindow,
    ChatWindow,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TerminalAgentPanelDock {
    Bottom,
    Left,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TeamworkSourceType {
    Git,
    SharedFile,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LocalFileBrowserDock {
    Left,
    Right,
    Bottom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamworkSourceConfig {
    pub id: String,
    pub source_type: TeamworkSourceType,
    pub location: String,
    pub check_interval_minutes: u32,
    pub read_only: bool,
    pub enabled: bool,
}

impl Default for TeamworkSourceConfig {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            source_type: TeamworkSourceType::Git,
            location: String::new(),
            check_interval_minutes: 15,
            read_only: false,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalSettings {
    pub language: String,
    pub auto_detect_language: bool,
    pub default_font_family: String,
    pub default_font_size: f32,
    pub default_columns: u16,
    pub default_rows: u16,
    pub default_scrollback_lines: u32,
    pub default_ssh_keepalive_enabled: bool,
    pub default_ssh_keepalive_interval: u32,
    pub default_connection_timeout: u32,
    pub default_retry_count: u32,
    #[serde(default = "default_true")]
    pub show_menu_bar: bool,
    pub store_window_geometry: bool,
    pub store_dashboard_state: bool,
    pub backup_encryption_type: BackupEncryptionType,
    pub backup_credential_id: Option<String>,
    pub backup_gpg_key_id: Option<String>,
    pub max_backups: u32,
    pub translation_provider: Option<TranslationProvider>,
    pub translation_api_key: Option<String>,
    pub translation_api_url: Option<String>,
    pub teamwork_sources: Vec<TeamworkSourceConfig>,
    pub teamwork_default_check_interval_minutes: u32,
    pub teamwork_default_credential_id: Option<String>,
    pub teamwork_default_ssh_key_id: Option<String>,
    pub teamwork_default_username: Option<String>,
    pub teamwork_use_temporary_key: bool,
    #[serde(default)]
    pub default_command_timestamps_enabled: bool,
    #[serde(default = "default_true")]
    pub default_prompt_hook_enabled: bool,
    #[serde(default)]
    pub terminal_agent_show_debug_messages: bool,
    #[serde(default)]
    pub terminal_agent_show_runtime_messages: bool,
    #[serde(default = "default_true")]
    pub terminal_agent_show_run_dialog: bool,
    #[serde(default = "default_terminal_agent_command_name")]
    pub terminal_agent_command_name: String,
    #[serde(default)]
    pub terminal_agent_command_name_case_insensitive: bool,
    #[serde(default = "default_terminal_agent_execution_target")]
    pub terminal_agent_execution_target: TerminalAgentExecutionTarget,
    #[serde(default)]
    pub terminal_agent_remember_panel_layout: bool,
    #[serde(default = "default_terminal_agent_panel_dock")]
    pub terminal_agent_panel_dock: TerminalAgentPanelDock,
    #[serde(default)]
    pub terminal_agent_panel_height: Option<f64>,
    #[serde(default)]
    pub terminal_agent_panel_side_width: Option<f64>,
    #[serde(default)]
    pub terminal_agent_panel_font_size: Option<f64>,
    #[serde(default)]
    pub default_ai_profile_id: Option<String>,
    #[serde(default)]
    pub ai_tavily_api_key: Option<String>,
    #[serde(default)]
    pub ai_bright_data_api_token: Option<String>,
    #[serde(default)]
    pub ai_brave_search_api_key: Option<String>,
    #[serde(default)]
    pub ai_searxng_url: Option<String>,
    #[serde(default = "default_tavily_mcp_server_label")]
    pub ai_tavily_mcp_server_label: String,
    #[serde(default = "default_bright_data_mcp_server_label")]
    pub ai_bright_data_mcp_server_label: String,
    #[serde(default)]
    pub ai_brave_search_mcp_plugin_id: Option<String>,
    #[serde(default)]
    pub ai_searxng_mcp_plugin_id: Option<String>,
    #[serde(default)]
    pub ai_lm_studio_toolpack_mcp_plugin_id: Option<String>,
    #[serde(default)]
    pub ai_skills: Vec<AiSkill>,
    #[serde(default = "default_job_scheduler_retention_days")]
    pub job_scheduler_journal_retention_days: u32,
    #[serde(default)]
    pub job_scheduler_show_menu_bar_status: bool,
    #[serde(default)]
    pub job_scheduler_rsync_path: Option<String>,
    #[serde(default)]
    pub default_terminal_effect_plugin_id: Option<String>,
    #[serde(default = "default_terminal_effect_animation_speed")]
    pub default_terminal_effect_animation_speed: f64,
    #[serde(default)]
    pub last_quick_connect_terminal_effect_plugin_id: Option<String>,
    #[serde(default)]
    pub last_quick_connect_terminal_effect_animation_speed: Option<f64>,
    #[serde(default)]
    pub terminal_recording_enabled: bool,
    #[serde(default = "default_true")]
    pub terminal_recording_idle_auto_pause: bool,
    #[serde(default)]
    pub terminal_recording_directory: Option<String>,
    #[serde(default = "default_local_file_browser_dock")]
    pub local_file_browser_dock: LocalFileBrowserDock,
    #[serde(default)]
    pub local_file_browser_visible: bool,
    #[serde(default = "default_true")]
    pub update_checks_enabled: bool,
    #[serde(default = "default_update_check_interval_days")]
    pub update_check_interval_days: u32,
    #[serde(default)]
    pub last_successful_update_check_millis: i64,
    #[serde(default)]
    pub ignored_update_version: Option<String>,
    #[serde(default)]
    pub snoozed_update_version: Option<String>,
    #[serde(default)]
    pub update_snoozed_until_local_date: Option<String>,
    #[serde(default)]
    pub last_automatic_update_prompt_version: Option<String>,
    #[serde(default)]
    pub last_automatic_update_prompt_local_date: Option<String>,
    #[serde(default)]
    pub log_directory_path: Option<String>,
    #[serde(default = "default_log_retention_days")]
    pub log_retention_days: u32,
    #[serde(default = "default_app_design")]
    pub app_design: String,
    #[serde(default)]
    pub hide_terminal_scrollbars_in_fullscreen: bool,
    #[serde(default = "default_true")]
    pub terminal_agent_execution_enabled: bool,
    #[serde(default)]
    pub terminal_agent_confirm_mutating_command_sets: bool,
    #[serde(default)]
    pub terminal_recording_format: TerminalRecordingFormat,
    #[serde(default = "default_terminal_recording_scope")]
    pub terminal_recording_default_scope: TerminalRecordingScope,
    #[serde(default = "default_terminal_recording_idle_pause_seconds")]
    pub terminal_recording_idle_pause_seconds: u32,
    #[serde(default)]
    pub terminal_recording_ffmpeg_path: Option<String>,
    #[serde(default)]
    pub terminal_recording_capture_colors_enabled: bool,
    #[serde(default = "default_snippet_diagram_background_color")]
    pub snippet_diagram_background_color: Option<String>,
    #[serde(default)]
    pub selected_snippet_editor_profile_id: Option<String>,
    #[serde(default)]
    pub snippet_editor_profiles: Vec<SnippetEditorProfile>,
    #[serde(default)]
    pub snippet_manager_preview_divider_position: Option<f64>,
    #[serde(default = "default_snippet_history_max_size")]
    pub snippet_history_max_size: u32,
    #[serde(default)]
    pub snippet_font_family: Option<String>,
    #[serde(default)]
    pub snippet_font_size: Option<u32>,
    #[serde(default)]
    pub snippet_foreground_color: Option<String>,
    #[serde(default)]
    pub snippet_background_color: Option<String>,
    #[serde(default)]
    pub snippet_cursor_color: Option<String>,
    #[serde(default)]
    pub snippet_cursor_style: Option<String>,
    #[serde(default)]
    pub snippet_word_wrap: bool,
    #[serde(default)]
    pub snippet_line_numbers: bool,
    #[serde(default)]
    pub ai_snippet_editor_additional_instructions_enabled: bool,
    #[serde(default = "default_ai_snippet_alternative_solution_count")]
    pub ai_snippet_alternative_solution_count: u32,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            language: "en".into(),
            auto_detect_language: true,
            default_font_family: "JetBrains Mono".into(),
            default_font_size: 14.0,
            default_columns: 80,
            default_rows: 24,
            default_scrollback_lines: 10000,
            default_ssh_keepalive_enabled: true,
            default_ssh_keepalive_interval: 60,
            default_connection_timeout: 15,
            default_retry_count: 4,
            show_menu_bar: true,
            store_window_geometry: true,
            store_dashboard_state: true,
            backup_encryption_type: BackupEncryptionType::Password,
            backup_credential_id: None,
            backup_gpg_key_id: None,
            max_backups: 10,
            translation_provider: None,
            translation_api_key: None,
            translation_api_url: None,
            teamwork_sources: Vec::new(),
            teamwork_default_check_interval_minutes: 15,
            teamwork_default_credential_id: None,
            teamwork_default_ssh_key_id: None,
            teamwork_default_username: None,
            teamwork_use_temporary_key: false,
            default_command_timestamps_enabled: false,
            default_prompt_hook_enabled: true,
            terminal_agent_show_debug_messages: false,
            terminal_agent_show_runtime_messages: false,
            terminal_agent_show_run_dialog: true,
            terminal_agent_command_name: default_terminal_agent_command_name(),
            terminal_agent_command_name_case_insensitive: false,
            terminal_agent_execution_target: default_terminal_agent_execution_target(),
            terminal_agent_remember_panel_layout: false,
            terminal_agent_panel_dock: default_terminal_agent_panel_dock(),
            terminal_agent_panel_height: None,
            terminal_agent_panel_side_width: None,
            terminal_agent_panel_font_size: None,
            default_ai_profile_id: None,
            ai_tavily_api_key: None,
            ai_bright_data_api_token: None,
            ai_brave_search_api_key: None,
            ai_searxng_url: None,
            ai_tavily_mcp_server_label: default_tavily_mcp_server_label(),
            ai_bright_data_mcp_server_label: default_bright_data_mcp_server_label(),
            ai_brave_search_mcp_plugin_id: None,
            ai_searxng_mcp_plugin_id: None,
            ai_lm_studio_toolpack_mcp_plugin_id: None,
            ai_skills: Vec::new(),
            job_scheduler_journal_retention_days: default_job_scheduler_retention_days(),
            job_scheduler_show_menu_bar_status: false,
            job_scheduler_rsync_path: None,
            default_terminal_effect_plugin_id: None,
            default_terminal_effect_animation_speed: default_terminal_effect_animation_speed(),
            last_quick_connect_terminal_effect_plugin_id: None,
            last_quick_connect_terminal_effect_animation_speed: None,
            terminal_recording_enabled: false,
            terminal_recording_idle_auto_pause: true,
            terminal_recording_directory: None,
            local_file_browser_dock: default_local_file_browser_dock(),
            local_file_browser_visible: false,
            update_checks_enabled: true,
            update_check_interval_days: default_update_check_interval_days(),
            last_successful_update_check_millis: 0,
            ignored_update_version: None,
            snoozed_update_version: None,
            update_snoozed_until_local_date: None,
            last_automatic_update_prompt_version: None,
            last_automatic_update_prompt_local_date: None,
            log_directory_path: None,
            log_retention_days: default_log_retention_days(),
            app_design: default_app_design(),
            hide_terminal_scrollbars_in_fullscreen: false,
            terminal_agent_execution_enabled: true,
            terminal_agent_confirm_mutating_command_sets: false,
            terminal_recording_format: TerminalRecordingFormat::KorttyReplay,
            terminal_recording_default_scope: default_terminal_recording_scope(),
            terminal_recording_idle_pause_seconds: default_terminal_recording_idle_pause_seconds(),
            terminal_recording_ffmpeg_path: None,
            terminal_recording_capture_colors_enabled: false,
            snippet_diagram_background_color: default_snippet_diagram_background_color(),
            selected_snippet_editor_profile_id: None,
            snippet_editor_profiles: Vec::new(),
            snippet_manager_preview_divider_position: None,
            snippet_history_max_size: default_snippet_history_max_size(),
            snippet_font_family: None,
            snippet_font_size: None,
            snippet_foreground_color: None,
            snippet_background_color: None,
            snippet_cursor_color: None,
            snippet_cursor_style: None,
            snippet_word_wrap: false,
            snippet_line_numbers: false,
            ai_snippet_editor_additional_instructions_enabled: false,
            ai_snippet_alternative_solution_count: default_ai_snippet_alternative_solution_count(),
        }
    }
}

impl GlobalSettings {
    /// Clamps and normalizes field values, mirroring the Java setter clamping.
    /// Applied whenever settings are loaded or saved.
    pub fn normalize(&mut self) {
        self.update_check_interval_days = self.update_check_interval_days.clamp(
            MIN_UPDATE_CHECK_INTERVAL_DAYS,
            MAX_UPDATE_CHECK_INTERVAL_DAYS,
        );
        self.last_successful_update_check_millis = self.last_successful_update_check_millis.max(0);
        normalize_optional_string(&mut self.ignored_update_version);
        normalize_optional_string(&mut self.snoozed_update_version);
        normalize_optional_string(&mut self.update_snoozed_until_local_date);
        normalize_optional_string(&mut self.last_automatic_update_prompt_version);
        normalize_optional_string(&mut self.last_automatic_update_prompt_local_date);
        normalize_optional_string(&mut self.log_directory_path);
        self.log_retention_days = self.log_retention_days.min(MAX_LOG_RETENTION_DAYS);
        self.app_design = normalize_app_design(&self.app_design);
        normalize_optional_string(&mut self.terminal_recording_directory);
        self.terminal_recording_idle_pause_seconds =
            self.terminal_recording_idle_pause_seconds.clamp(1, 3600);
        normalize_optional_string(&mut self.terminal_recording_ffmpeg_path);
        normalize_optional_string(&mut self.snippet_diagram_background_color);
        if self.snippet_diagram_background_color.is_none() {
            self.snippet_diagram_background_color = default_snippet_diagram_background_color();
        }
        normalize_optional_string(&mut self.selected_snippet_editor_profile_id);
        self.snippet_manager_preview_divider_position = self
            .snippet_manager_preview_divider_position
            .filter(|value| value.is_finite())
            .map(|value| value.clamp(0.35, 0.9));
        self.snippet_history_max_size = if self.snippet_history_max_size == 0 {
            default_snippet_history_max_size()
        } else {
            self.snippet_history_max_size.min(99)
        };
        self.ai_snippet_alternative_solution_count =
            if self.ai_snippet_alternative_solution_count == 0 {
                default_ai_snippet_alternative_solution_count()
            } else {
                self.ai_snippet_alternative_solution_count.min(10)
            };
    }
}

fn normalize_optional_string(value: &mut Option<String>) {
    *value = value
        .take()
        .map(|raw| raw.trim().to_string())
        .filter(|trimmed| !trimmed.is_empty());
}

fn normalize_app_design(raw: &str) -> String {
    // Accepts stable IDs and Java enum constant names, case-insensitively.
    let candidate = raw.trim().to_lowercase().replace('_', "-");
    APP_DESIGN_IDS
        .iter()
        .find(|id| **id == candidate)
        .map(|id| id.to_string())
        .unwrap_or_else(default_app_design)
}

fn default_true() -> bool {
    true
}

fn default_terminal_agent_command_name() -> String {
    "agent".into()
}

fn default_terminal_agent_execution_target() -> TerminalAgentExecutionTarget {
    TerminalAgentExecutionTarget::TerminalWindow
}

fn default_terminal_agent_panel_dock() -> TerminalAgentPanelDock {
    TerminalAgentPanelDock::Bottom
}

fn default_tavily_mcp_server_label() -> String {
    "tavily".into()
}

fn default_bright_data_mcp_server_label() -> String {
    "bright-data".into()
}

fn default_job_scheduler_retention_days() -> u32 {
    14
}

fn default_terminal_effect_animation_speed() -> f64 {
    1.0
}

fn default_local_file_browser_dock() -> LocalFileBrowserDock {
    LocalFileBrowserDock::Left
}

fn default_update_check_interval_days() -> u32 {
    DEFAULT_UPDATE_CHECK_INTERVAL_DAYS
}

fn default_log_retention_days() -> u32 {
    DEFAULT_LOG_RETENTION_DAYS
}

fn default_app_design() -> String {
    "normal".into()
}

fn default_terminal_recording_scope() -> TerminalRecordingScope {
    TerminalRecordingScope::ActiveSplit
}

fn default_terminal_recording_idle_pause_seconds() -> u32 {
    20
}

fn default_snippet_diagram_background_color() -> Option<String> {
    Some("#FFFFFF".into())
}

fn default_snippet_history_max_size() -> u32 {
    30
}

fn default_ai_snippet_alternative_solution_count() -> u32 {
    3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_agent_panel_layout_defaults_for_legacy_settings() {
        let mut value = serde_json::to_value(GlobalSettings::default()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("terminalAgentPanelDock");
        object.remove("terminalAgentPanelSideWidth");

        let settings: GlobalSettings = serde_json::from_value(value).unwrap();

        assert_eq!(
            settings.terminal_agent_panel_dock,
            TerminalAgentPanelDock::Bottom
        );
        assert_eq!(settings.terminal_agent_panel_side_width, None);
    }

    #[test]
    fn terminal_agent_panel_layout_serializes_lowercase_dock() {
        let settings = GlobalSettings {
            terminal_agent_panel_dock: TerminalAgentPanelDock::Right,
            terminal_agent_panel_side_width: Some(420.0),
            ..Default::default()
        };

        let value = serde_json::to_value(&settings).unwrap();
        assert_eq!(value["terminalAgentPanelDock"], "right");
        assert_eq!(value["terminalAgentPanelSideWidth"], 420.0);

        let restored: GlobalSettings = serde_json::from_value(value).unwrap();
        assert_eq!(
            restored.terminal_agent_panel_dock,
            TerminalAgentPanelDock::Right
        );
        assert_eq!(restored.terminal_agent_panel_side_width, Some(420.0));
    }

    #[test]
    fn legacy_settings_get_new_field_defaults() {
        let settings: GlobalSettings = serde_json::from_str("{\"language\":\"en\",\"autoDetectLanguage\":true,\"defaultFontFamily\":\"JetBrains Mono\",\"defaultFontSize\":14.0,\"defaultColumns\":80,\"defaultRows\":24,\"defaultScrollbackLines\":10000,\"defaultSshKeepaliveEnabled\":true,\"defaultSshKeepaliveInterval\":60,\"defaultConnectionTimeout\":15,\"defaultRetryCount\":4,\"storeWindowGeometry\":true,\"storeDashboardState\":true,\"backupEncryptionType\":\"Password\",\"backupCredentialId\":null,\"backupGpgKeyId\":null,\"maxBackups\":10,\"translationProvider\":null,\"translationApiKey\":null,\"translationApiUrl\":null,\"teamworkSources\":[],\"teamworkDefaultCheckIntervalMinutes\":15,\"teamworkDefaultCredentialId\":null,\"teamworkDefaultSshKeyId\":null,\"teamworkDefaultUsername\":null,\"teamworkUseTemporaryKey\":false}").unwrap();

        assert!(settings.update_checks_enabled);
        assert_eq!(settings.update_check_interval_days, 1);
        assert_eq!(settings.last_successful_update_check_millis, 0);
        assert_eq!(settings.log_retention_days, 7);
        assert_eq!(settings.app_design, "normal");
        assert!(settings.terminal_agent_execution_enabled);
        assert!(!settings.terminal_agent_confirm_mutating_command_sets);
        assert_eq!(
            settings.terminal_recording_format,
            TerminalRecordingFormat::KorttyReplay
        );
        assert_eq!(
            settings.terminal_recording_default_scope,
            TerminalRecordingScope::ActiveSplit
        );
        assert_eq!(settings.terminal_recording_idle_pause_seconds, 20);
        assert_eq!(
            settings.snippet_diagram_background_color.as_deref(),
            Some("#FFFFFF")
        );
        assert_eq!(settings.snippet_history_max_size, 30);
        assert_eq!(settings.ai_snippet_alternative_solution_count, 3);
    }

    #[test]
    fn normalize_clamps_and_trims_values() {
        let mut settings = GlobalSettings {
            update_check_interval_days: 90,
            last_successful_update_check_millis: -5,
            ignored_update_version: Some("  ".into()),
            snoozed_update_version: Some(" 2.3.0 ".into()),
            log_directory_path: Some("".into()),
            log_retention_days: 9999,
            app_design: "MATRIX_TERMINAL".into(),
            terminal_recording_idle_pause_seconds: 0,
            terminal_recording_ffmpeg_path: Some("  /usr/bin/ffmpeg ".into()),
            snippet_diagram_background_color: Some("  ".into()),
            snippet_manager_preview_divider_position: Some(0.05),
            snippet_history_max_size: 0,
            ai_snippet_alternative_solution_count: 25,
            ..Default::default()
        };

        settings.normalize();

        assert_eq!(settings.update_check_interval_days, 30);
        assert_eq!(settings.last_successful_update_check_millis, 0);
        assert_eq!(settings.ignored_update_version, None);
        assert_eq!(settings.snoozed_update_version.as_deref(), Some("2.3.0"));
        assert_eq!(settings.log_directory_path, None);
        assert_eq!(settings.log_retention_days, 3650);
        assert_eq!(settings.app_design, "matrix-terminal");
        assert_eq!(settings.terminal_recording_idle_pause_seconds, 1);
        assert_eq!(
            settings.terminal_recording_ffmpeg_path.as_deref(),
            Some("/usr/bin/ffmpeg")
        );
        assert_eq!(
            settings.snippet_diagram_background_color.as_deref(),
            Some("#FFFFFF")
        );
        assert_eq!(
            settings.snippet_manager_preview_divider_position,
            Some(0.35)
        );
        assert_eq!(settings.snippet_history_max_size, 30);
        assert_eq!(settings.ai_snippet_alternative_solution_count, 10);
    }

    #[test]
    fn normalize_falls_back_to_normal_design_for_unknown_ids() {
        let mut settings = GlobalSettings {
            app_design: "does-not-exist".into(),
            ..Default::default()
        };
        settings.normalize();
        assert_eq!(settings.app_design, "normal");
    }
}
