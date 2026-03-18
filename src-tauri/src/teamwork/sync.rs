use anyhow::{anyhow, Result};
use quick_xml::de::from_str;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::model::connection::{AuthMethod, ConnectionSettings, ConnectionSource, CursorStyle};
use crate::model::settings::{GlobalSettings, TeamworkSourceConfig, TeamworkSourceType};
use crate::persistence::xml_repository;

const CACHE_FILE: &str = "teamwork-cache.json";
const RECYCLE_BIN_FILE: &str = "teamwork-recycle-bin.json";
const TEAMWORK_REPOS_DIR: &str = "teamwork-repos";
const CONNECTIONS_FILENAME: &str = "kortty-teamwork-connections.xml";
const CONNECTIONS_FILENAME_LEGACY: &str = "connections.xml";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedTeamworkSource {
    pub source_id: String,
    pub last_checked_millis: i64,
    pub version_token: String,
    pub connections: Vec<ConnectionSettings>,
}

#[derive(Debug, Deserialize)]
struct XmlConnectionsWrapper {
    #[serde(rename = "connection", default)]
    connections: Vec<XmlConnection>,
}

#[derive(Debug, Deserialize)]
struct XmlConnection {
    #[serde(rename = "@id")]
    id: Option<String>,
    name: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    username: Option<String>,
    #[serde(rename = "authMethod")]
    auth_method: Option<String>,
    group: Option<String>,
    #[serde(rename = "credentialId")]
    credential_id: Option<String>,
    #[serde(rename = "sshKeyId")]
    ssh_key_id: Option<String>,
    #[serde(rename = "teamworkRole")]
    teamwork_role: Option<String>,
    settings: Option<XmlConnectionSettings>,
}

#[derive(Debug, Deserialize)]
struct XmlConnectionSettings {
    #[serde(rename = "fontFamily")]
    font_family: Option<String>,
    #[serde(rename = "fontSize")]
    font_size: Option<f32>,
    #[serde(rename = "foregroundColor")]
    foreground_color: Option<String>,
    #[serde(rename = "backgroundColor")]
    background_color: Option<String>,
    #[serde(rename = "cursorColor")]
    cursor_color: Option<String>,
    #[serde(rename = "themeId")]
    theme_id: Option<String>,
    #[serde(rename = "terminalColumns")]
    columns: Option<u16>,
    #[serde(rename = "terminalRows")]
    rows: Option<u16>,
    #[serde(rename = "scrollbackLines")]
    scrollback_lines: Option<u32>,
    #[serde(rename = "commandTimestampsEnabled")]
    command_timestamps: Option<bool>,
    #[serde(rename = "sshKeepAliveEnabled")]
    ssh_keepalive_enabled: Option<bool>,
    #[serde(rename = "sshKeepAliveInterval")]
    ssh_keepalive_interval: Option<u32>,
    #[serde(rename = "ansiBlack")]
    ansi_black: Option<String>,
    #[serde(rename = "ansiRed")]
    ansi_red: Option<String>,
    #[serde(rename = "ansiGreen")]
    ansi_green: Option<String>,
    #[serde(rename = "ansiYellow")]
    ansi_yellow: Option<String>,
    #[serde(rename = "ansiBlue")]
    ansi_blue: Option<String>,
    #[serde(rename = "ansiMagenta")]
    ansi_magenta: Option<String>,
    #[serde(rename = "ansiCyan")]
    ansi_cyan: Option<String>,
    #[serde(rename = "ansiWhite")]
    ansi_white: Option<String>,
    #[serde(rename = "ansiBrightBlack")]
    ansi_bright_black: Option<String>,
    #[serde(rename = "ansiBrightRed")]
    ansi_bright_red: Option<String>,
    #[serde(rename = "ansiBrightGreen")]
    ansi_bright_green: Option<String>,
    #[serde(rename = "ansiBrightYellow")]
    ansi_bright_yellow: Option<String>,
    #[serde(rename = "ansiBrightBlue")]
    ansi_bright_blue: Option<String>,
    #[serde(rename = "ansiBrightMagenta")]
    ansi_bright_magenta: Option<String>,
    #[serde(rename = "ansiBrightCyan")]
    ansi_bright_cyan: Option<String>,
    #[serde(rename = "ansiBrightWhite")]
    ansi_bright_white: Option<String>,
}

pub struct SyncService;

impl SyncService {
    pub async fn sync_now(settings: &GlobalSettings) -> Result<Vec<CachedTeamworkSource>> {
        let previous = Self::load_cache().unwrap_or_default();
        let mut next_cache: Vec<CachedTeamworkSource> = Vec::new();

        for source in settings.teamwork_sources.iter().filter(|s| s.enabled) {
            let loaded = match source.source_type {
                TeamworkSourceType::Git => Self::load_from_git(source, settings),
                TeamworkSourceType::SharedFile => Self::load_from_shared_file(source, settings),
            };

            match loaded {
                Ok(cached) => next_cache.push(cached),
                Err(_) => {
                    if let Some(prev) = previous.iter().find(|c| c.source_id == source.id) {
                        next_cache.push(prev.clone());
                    }
                }
            }
        }

        Self::save_cache(&next_cache)?;
        Ok(next_cache)
    }

    pub fn load_cache() -> Result<Vec<CachedTeamworkSource>> {
        let cache: Vec<CachedTeamworkSource> =
            xml_repository::load_json(CACHE_FILE)?.unwrap_or_default();
        Ok(cache)
    }

    pub fn get_deleted_ids() -> Result<HashSet<String>> {
        let list: Vec<String> = xml_repository::load_json(RECYCLE_BIN_FILE)?.unwrap_or_default();
        Ok(list.into_iter().collect())
    }

    pub fn mark_deleted(connection_id: &str) -> Result<()> {
        let mut list: Vec<String> =
            xml_repository::load_json(RECYCLE_BIN_FILE)?.unwrap_or_default();
        if !list.iter().any(|id| id == connection_id) {
            list.push(connection_id.to_string());
            xml_repository::save_json(RECYCLE_BIN_FILE, &list)?;
        }
        Ok(())
    }

    pub fn restore_deleted(connection_id: &str) -> Result<()> {
        let mut list: Vec<String> =
            xml_repository::load_json(RECYCLE_BIN_FILE)?.unwrap_or_default();
        list.retain(|id| id != connection_id);
        xml_repository::save_json(RECYCLE_BIN_FILE, &list)?;
        Ok(())
    }

    pub fn all_teamwork_connections() -> Result<Vec<ConnectionSettings>> {
        let deleted = Self::get_deleted_ids()?;
        let mut all = Vec::new();
        for source in Self::load_cache()? {
            for conn in source.connections {
                if !deleted.contains(&conn.id) {
                    all.push(conn);
                }
            }
        }
        Ok(all)
    }

    pub fn deleted_teamwork_connections() -> Result<Vec<ConnectionSettings>> {
        let deleted = Self::get_deleted_ids()?;
        let mut all = Vec::new();
        for source in Self::load_cache()? {
            for conn in source.connections {
                if deleted.contains(&conn.id) {
                    all.push(conn);
                }
            }
        }
        Ok(all)
    }

    fn load_from_git(
        source: &TeamworkSourceConfig,
        settings: &GlobalSettings,
    ) -> Result<CachedTeamworkSource> {
        if source.location.trim().is_empty() {
            return Err(anyhow!("Empty Git location"));
        }
        let repo_path = Self::repo_path_for(&source.id)?;
        Self::ensure_cloned_or_pulled(&source.location, &repo_path)?;

        let connections_file = Self::find_connections_file(&repo_path)?;
        let xml = fs::read_to_string(connections_file)?;
        let mut connections = Self::parse_connections_xml(&xml, source, settings)?;
        let version = Self::git_current_revision(&repo_path).unwrap_or_default();
        for c in &mut connections {
            c.teamwork_version_token = Some(version.clone());
        }

        Ok(CachedTeamworkSource {
            source_id: source.id.clone(),
            last_checked_millis: chrono::Utc::now().timestamp_millis(),
            version_token: version,
            connections,
        })
    }

    fn load_from_shared_file(
        source: &TeamworkSourceConfig,
        settings: &GlobalSettings,
    ) -> Result<CachedTeamworkSource> {
        let path = PathBuf::from(source.location.trim());
        if !path.exists() {
            return Err(anyhow!("Shared file does not exist"));
        }

        let xml = fs::read_to_string(&path)?;
        let mut connections = Self::parse_connections_xml(&xml, source, settings)?;
        let version = fs::metadata(&path)?
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis().to_string())
            .unwrap_or_default();
        for c in &mut connections {
            c.teamwork_version_token = Some(version.clone());
        }

        Ok(CachedTeamworkSource {
            source_id: source.id.clone(),
            last_checked_millis: chrono::Utc::now().timestamp_millis(),
            version_token: version,
            connections,
        })
    }

    fn parse_connections_xml(
        xml: &str,
        source: &TeamworkSourceConfig,
        settings: &GlobalSettings,
    ) -> Result<Vec<ConnectionSettings>> {
        let wrapper: XmlConnectionsWrapper = from_str(xml)?;
        let mut out = Vec::new();
        for raw in wrapper.connections {
            let XmlConnection {
                id,
                name,
                host,
                port,
                username,
                auth_method,
                group,
                credential_id,
                ssh_key_id,
                teamwork_role,
                settings: xml_settings,
            } = raw;

            let host = host.unwrap_or_default();
            let username = username
                .or_else(|| settings.teamwork_default_username.clone())
                .unwrap_or_default();
            if host.trim().is_empty() || username.trim().is_empty() {
                continue;
            }

            let mut conn = ConnectionSettings {
                id: id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name: name.unwrap_or_else(|| format!("{username}@{host}")),
                group,
                host,
                port: port.unwrap_or(22),
                username,
                auth_method: match auth_method.as_deref() {
                    Some("PRIVATE_KEY") | Some("PrivateKey") => AuthMethod::PrivateKey,
                    _ => AuthMethod::Password,
                },
                password: None,
                credential_id: credential_id
                    .or_else(|| settings.teamwork_default_credential_id.clone()),
                ssh_key_id: ssh_key_id.or_else(|| settings.teamwork_default_ssh_key_id.clone()),
                connection_source: Some(ConnectionSource::Teamwork),
                teamwork_source_id: Some(source.id.clone()),
                teamwork_role,
                ..ConnectionSettings::default()
            };

            if let Some(s) = xml_settings {
                if let Some(v) = s.font_family {
                    conn.font_family = v;
                }
                if let Some(v) = s.font_size {
                    conn.font_size = v;
                }
                if let Some(v) = s.foreground_color {
                    conn.foreground_color = v;
                }
                if let Some(v) = s.background_color {
                    conn.background_color = v;
                }
                if let Some(v) = s.cursor_color {
                    conn.cursor_color = v;
                }
                if let Some(v) = s.columns {
                    conn.columns = v;
                }
                if let Some(v) = s.rows {
                    conn.rows = v;
                }
                if let Some(v) = s.scrollback_lines {
                    conn.scrollback_lines = v;
                }
                if let Some(v) = s.command_timestamps {
                    conn.command_timestamps = v;
                }
                if let Some(v) = s.ssh_keepalive_enabled {
                    conn.ssh_keepalive_enabled = v;
                }
                if let Some(v) = s.ssh_keepalive_interval {
                    conn.ssh_keepalive_interval = v;
                }
                if let Some(theme_id) = s.theme_id {
                    conn.theme_id = Some(theme_id);
                }

                conn.cursor_style = CursorStyle::Block;
                conn.ansi_colors = vec![
                    s.ansi_black.unwrap_or_else(|| "#000000".to_string()),
                    s.ansi_red.unwrap_or_else(|| "#CD0000".to_string()),
                    s.ansi_green.unwrap_or_else(|| "#00CD00".to_string()),
                    s.ansi_yellow.unwrap_or_else(|| "#CDCD00".to_string()),
                    s.ansi_blue.unwrap_or_else(|| "#0000EE".to_string()),
                    s.ansi_magenta.unwrap_or_else(|| "#CD00CD".to_string()),
                    s.ansi_cyan.unwrap_or_else(|| "#00CDCD".to_string()),
                    s.ansi_white.unwrap_or_else(|| "#E5E5E5".to_string()),
                    s.ansi_bright_black.unwrap_or_else(|| "#7F7F7F".to_string()),
                    s.ansi_bright_red.unwrap_or_else(|| "#FF0000".to_string()),
                    s.ansi_bright_green.unwrap_or_else(|| "#00FF00".to_string()),
                    s.ansi_bright_yellow
                        .unwrap_or_else(|| "#FFFF00".to_string()),
                    s.ansi_bright_blue.unwrap_or_else(|| "#5C5CFF".to_string()),
                    s.ansi_bright_magenta
                        .unwrap_or_else(|| "#FF00FF".to_string()),
                    s.ansi_bright_cyan.unwrap_or_else(|| "#00FFFF".to_string()),
                    s.ansi_bright_white.unwrap_or_else(|| "#FFFFFF".to_string()),
                ];
            }
            out.push(conn);
        }
        Ok(out)
    }

    fn save_cache(cache: &[CachedTeamworkSource]) -> Result<()> {
        xml_repository::save_json(CACHE_FILE, cache)?;
        Ok(())
    }

    fn find_connections_file(repo_path: &Path) -> Result<PathBuf> {
        let primary = repo_path.join(CONNECTIONS_FILENAME);
        if primary.is_file() {
            return Ok(primary);
        }
        let legacy = repo_path.join(CONNECTIONS_FILENAME_LEGACY);
        if legacy.is_file() {
            return Ok(legacy);
        }
        Err(anyhow!("No teamwork connections xml found"))
    }

    fn repo_path_for(source_id: &str) -> Result<PathBuf> {
        let base = xml_repository::config_dir()?.join(TEAMWORK_REPOS_DIR);
        fs::create_dir_all(&base)?;
        let safe: String = source_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        Ok(base.join(safe))
    }

    fn ensure_cloned_or_pulled(git_url: &str, repo_path: &Path) -> Result<()> {
        if !repo_path.exists() {
            let parent = repo_path
                .parent()
                .ok_or_else(|| anyhow!("Invalid repo path"))?;
            let status = Command::new("git")
                .arg("clone")
                .arg("--depth")
                .arg("1")
                .arg(git_url)
                .arg(repo_path)
                .current_dir(parent)
                .status()?;
            if !status.success() {
                return Err(anyhow!("git clone failed"));
            }
        } else {
            let status = Command::new("git")
                .arg("pull")
                .arg("--rebase")
                .current_dir(repo_path)
                .status()?;
            if !status.success() {
                return Err(anyhow!("git pull failed"));
            }
        }
        Ok(())
    }

    fn git_current_revision(repo_path: &Path) -> Result<String> {
        let out = Command::new("git")
            .arg("rev-parse")
            .arg("HEAD")
            .current_dir(repo_path)
            .output()?;
        if !out.status.success() {
            return Ok(String::new());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}
