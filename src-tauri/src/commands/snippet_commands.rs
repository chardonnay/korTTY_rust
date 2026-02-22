use crate::model::snippet::Snippet;
use crate::persistence::xml_repository;

#[tauri::command]
pub async fn get_snippets() -> Result<Vec<Snippet>, String> {
    let snippets: Vec<Snippet> = xml_repository::load_json("snippets.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    Ok(snippets)
}

#[tauri::command]
pub async fn save_snippet(snippet: Snippet) -> Result<(), String> {
    let mut snippets: Vec<Snippet> = xml_repository::load_json("snippets.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    if let Some(pos) = snippets.iter().position(|s| s.id == snippet.id) {
        snippets[pos] = snippet;
    } else {
        snippets.push(snippet);
    }

    xml_repository::save_json("snippets.json", &snippets).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_snippet(id: String) -> Result<(), String> {
    let mut snippets: Vec<Snippet> = xml_repository::load_json("snippets.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    snippets.retain(|s| s.id != id);
    xml_repository::save_json("snippets.json", &snippets).map_err(|e| e.to_string())
}
