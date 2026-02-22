use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuthMethod {
    Password,
    PrivateKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSettings {
    pub id: String,
    pub name: String,
    pub group: Option<String>,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: AuthMethod,
    pub password: Option<String>,
    pub credential_id: Option<String>,
    pub ssh_key_id: Option<String>,
    pub font_family: String,
    pub font_size: f32,
    pub columns: u16,
    pub rows: u16,
    pub scrollback_lines: u32,
    pub foreground_color: String,
    pub background_color: String,
    pub cursor_color: String,
    pub cursor_style: CursorStyle,
    pub ansi_colors: Vec<String>,
    pub ssh_keepalive_enabled: bool,
    pub ssh_keepalive_interval: u32,
    pub connection_timeout: u32,
    pub retry_count: u32,
    pub terminal_logging: bool,
    pub command_timestamps: bool,
    pub theme_id: Option<String>,
    pub jump_server: Option<JumpServerConfig>,
    pub tunnels: Vec<super::tunnel::TunnelConfig>,
    pub tab_group: Option<String>,
    pub usage_count: u64,
    pub last_used: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CursorStyle {
    Block,
    Underline,
    Bar,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JumpServerConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: AuthMethod,
    pub password: Option<String>,
    pub ssh_key_id: Option<String>,
    pub auto_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionGroup {
    pub name: String,
    pub connections: Vec<String>,
}

impl Default for ConnectionSettings {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            group: None,
            host: String::new(),
            port: 22,
            username: String::new(),
            auth_method: AuthMethod::Password,
            password: None,
            credential_id: None,
            ssh_key_id: None,
            font_family: "JetBrains Mono".into(),
            font_size: 14.0,
            columns: 80,
            rows: 24,
            scrollback_lines: 10000,
            foreground_color: "#cdd6f4".into(),
            background_color: "#11111b".into(),
            cursor_color: "#89b4fa".into(),
            cursor_style: CursorStyle::Block,
            ansi_colors: default_ansi_colors(),
            ssh_keepalive_enabled: true,
            ssh_keepalive_interval: 60,
            connection_timeout: 15,
            retry_count: 4,
            terminal_logging: false,
            command_timestamps: false,
            theme_id: None,
            jump_server: None,
            tunnels: Vec::new(),
            tab_group: None,
            usage_count: 0,
            last_used: None,
        }
    }
}

fn default_ansi_colors() -> Vec<String> {
    vec![
        "#45475a".into(), // Black
        "#f38ba8".into(), // Red
        "#a6e3a1".into(), // Green
        "#f9e2af".into(), // Yellow
        "#89b4fa".into(), // Blue
        "#f5c2e7".into(), // Magenta
        "#94e2d5".into(), // Cyan
        "#bac2de".into(), // White
        "#585b70".into(), // Bright Black
        "#f38ba8".into(), // Bright Red
        "#a6e3a1".into(), // Bright Green
        "#f9e2af".into(), // Bright Yellow
        "#89b4fa".into(), // Bright Blue
        "#f5c2e7".into(), // Bright Magenta
        "#94e2d5".into(), // Bright Cyan
        "#a6adc8".into(), // Bright White
    ]
}
