use serde::{Deserialize, Serialize};

use super::ai::AiSkill;

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
        }
    }
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
}
