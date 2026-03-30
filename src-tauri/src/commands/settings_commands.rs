use crate::model::settings::GlobalSettings;
use crate::persistence::xml_repository;
use tauri::{AppHandle, Emitter};

#[tauri::command]
pub async fn get_settings() -> Result<GlobalSettings, String> {
    let settings: GlobalSettings = xml_repository::load_json("global-settings.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    Ok(settings)
}

#[tauri::command]
pub async fn save_settings(app: AppHandle, settings: GlobalSettings) -> Result<(), String> {
    xml_repository::save_json("global-settings.json", &settings).map_err(|e| e.to_string())?;
    let _ = app.emit("kortty-settings-updated", &settings);
    Ok(())
}
