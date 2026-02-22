use serde::{Deserialize, Serialize};

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
    pub store_window_geometry: bool,
    pub store_dashboard_state: bool,
    pub backup_encryption_type: BackupEncryptionType,
    pub backup_credential_id: Option<String>,
    pub backup_gpg_key_id: Option<String>,
    pub max_backups: u32,
    pub translation_provider: Option<TranslationProvider>,
    pub translation_api_key: Option<String>,
    pub translation_api_url: Option<String>,
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
            store_window_geometry: true,
            store_dashboard_state: true,
            backup_encryption_type: BackupEncryptionType::Password,
            backup_credential_id: None,
            backup_gpg_key_id: None,
            max_backups: 10,
            translation_provider: None,
            translation_api_key: None,
            translation_api_url: None,
        }
    }
}
