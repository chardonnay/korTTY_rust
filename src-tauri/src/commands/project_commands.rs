use crate::model::project::Project;
use crate::persistence::xml_repository;

const RECENT_PROJECTS_FILE: &str = "recent-projects.json";

#[tauri::command]
pub async fn save_project(project: Project, path: String) -> Result<Project, String> {
    let mut project = project;
    let now = chrono::Utc::now().to_rfc3339();
    if project.name.trim().is_empty() {
        project.name = "Untitled".into();
    } else {
        project.name = project.name.trim().to_string();
    }
    project.file_path = Some(path.clone());
    if project.created_at.is_none() {
        project.created_at = Some(now.clone());
    }
    project.last_modified = Some(now);

    let json = serde_json::to_string_pretty(&project).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    update_recent_projects(&path).map_err(|e| e.to_string())?;
    Ok(project)
}

#[tauri::command]
pub async fn load_project(path: String) -> Result<Project, String> {
    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut project: Project = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    project.file_path = Some(path.clone());
    update_recent_projects(&path).map_err(|e| e.to_string())?;
    Ok(project)
}

#[tauri::command]
pub async fn peek_project(path: String) -> Result<Project, String> {
    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut project: Project = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    project.file_path = Some(path);
    Ok(project)
}

#[tauri::command]
pub async fn get_recent_projects() -> Result<Vec<String>, String> {
    let projects: Vec<String> = xml_repository::load_json(RECENT_PROJECTS_FILE)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let existing: Vec<String> = projects
        .iter()
        .filter(|path| std::path::Path::new(path).exists())
        .cloned()
        .collect();
    if existing.len() < projects.len() {
        xml_repository::save_json(RECENT_PROJECTS_FILE, &existing).map_err(|e| e.to_string())?;
    }
    Ok(existing)
}

fn update_recent_projects(path: &str) -> anyhow::Result<()> {
    let mut projects: Vec<String> =
        xml_repository::load_json(RECENT_PROJECTS_FILE)?.unwrap_or_default();
    projects.retain(|entry| entry != path);
    projects.insert(0, path.to_string());
    if projects.len() > 10 {
        projects.truncate(10);
    }
    xml_repository::save_json(RECENT_PROJECTS_FILE, &projects)
}
