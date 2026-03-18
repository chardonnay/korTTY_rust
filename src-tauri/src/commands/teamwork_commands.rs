use crate::commands::settings_commands::get_settings;
use crate::model::connection::ConnectionSettings;
use crate::teamwork::sync::SyncService;

#[tauri::command]
pub async fn sync_teamwork_now() -> Result<usize, String> {
    let settings = get_settings().await?;
    let cache = SyncService::sync_now(&settings)
        .await
        .map_err(|e| e.to_string())?;
    Ok(cache.iter().map(|s| s.connections.len()).sum())
}

#[tauri::command]
pub async fn restore_teamwork_connection(connection_id: String) -> Result<(), String> {
    SyncService::restore_deleted(&connection_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_teamwork_connections() -> Result<Vec<ConnectionSettings>, String> {
    SyncService::all_teamwork_connections().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_deleted_teamwork_connections() -> Result<Vec<ConnectionSettings>, String> {
    SyncService::deleted_teamwork_connections().map_err(|e| e.to_string())
}
