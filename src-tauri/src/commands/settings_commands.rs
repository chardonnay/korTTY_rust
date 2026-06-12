use crate::model::settings::GlobalSettings;
use crate::persistence::xml_repository;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;
use tauri::{AppHandle, Emitter};

const SETTINGS_FILE: &str = "global-settings.json";

/// Modification time of the settings file at the moment it was last loaded
/// or saved, so external changes can be detected
/// (port of Java GlobalSettingsManager.reloadIfChanged()).
static LOADED_SETTINGS_MODIFIED: Mutex<Option<SystemTime>> = Mutex::new(None);

fn record_loaded_modified(modified: Option<SystemTime>) {
    if let Ok(mut guard) = LOADED_SETTINGS_MODIFIED.lock() {
        *guard = modified;
    }
}

fn loaded_modified() -> Option<SystemTime> {
    LOADED_SETTINGS_MODIFIED
        .lock()
        .ok()
        .and_then(|guard| *guard)
}

fn settings_file_path() -> Result<PathBuf, String> {
    Ok(xml_repository::config_dir()
        .map_err(|e| e.to_string())?
        .join(SETTINGS_FILE))
}

fn settings_file_modified() -> Option<SystemTime> {
    settings_file_path()
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|metadata| metadata.modified().ok())
}

fn load_settings_from_disk() -> Result<GlobalSettings, String> {
    let mut settings: GlobalSettings = xml_repository::load_json(SETTINGS_FILE)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    settings.normalize();
    Ok(settings)
}

#[tauri::command]
pub async fn get_settings() -> Result<GlobalSettings, String> {
    let modified = settings_file_modified();
    let settings = load_settings_from_disk()?;
    record_loaded_modified(modified);
    Ok(settings)
}

#[tauri::command]
pub async fn save_settings(app: AppHandle, mut settings: GlobalSettings) -> Result<(), String> {
    settings.normalize();
    xml_repository::save_json(SETTINGS_FILE, &settings).map_err(|e| e.to_string())?;
    record_loaded_modified(settings_file_modified());
    let _ = app.emit("kortty-settings-updated", &settings);
    Ok(())
}

/// Reloads the settings when the settings file was modified on disk since the
/// last load/save (e.g. by another KorTTY window or an external tool).
/// Returns `None` when nothing changed.
#[tauri::command]
pub async fn reload_settings_if_changed() -> Result<Option<GlobalSettings>, String> {
    let current_modified = settings_file_modified();
    if current_modified == loaded_modified() {
        return Ok(None);
    }
    let settings = load_settings_from_disk()?;
    record_loaded_modified(current_modified);
    Ok(Some(settings))
}

/// Returns the default log directory (`<config-dir>/logs`) so the settings
/// dialog can display it as a placeholder.
#[tauri::command]
pub async fn get_default_log_directory() -> Result<String, String> {
    let config_dir = xml_repository::config_dir().map_err(|e| e.to_string())?;
    Ok(crate::logging::default_log_directory(&config_dir)
        .to_string_lossy()
        .into_owned())
}
