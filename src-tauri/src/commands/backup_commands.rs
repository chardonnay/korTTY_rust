#[tauri::command]
pub async fn create_backup(
    destination: String,
    password: Option<String>,
) -> Result<String, String> {
    crate::backup::manager::BackupManager::create_backup(&destination, password.as_deref())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn import_backup(file_path: String, password: Option<String>) -> Result<(), String> {
    crate::backup::manager::BackupManager::import_backup(&file_path, password.as_deref())
        .map_err(|e| e.to_string())
}
