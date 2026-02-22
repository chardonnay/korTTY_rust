use crate::model::project::Project;

#[tauri::command]
pub async fn save_project(project: Project, path: String) -> Result<(), String> {
    let json = serde_json::to_string_pretty(&project).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn load_project(path: String) -> Result<Project, String> {
    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    serde_json::from_str(&content).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_recent_projects() -> Result<Vec<String>, String> {
    // Will be implemented in Phase 9
    Ok(Vec::new())
}
