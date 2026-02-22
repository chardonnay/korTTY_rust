use crate::model::credential::Credential;
use crate::persistence::xml_repository;

#[tauri::command]
pub async fn get_credentials() -> Result<Vec<Credential>, String> {
    let creds: Vec<Credential> = xml_repository::load_json("credentials.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    Ok(creds)
}

#[tauri::command]
pub async fn save_credential(credential: Credential) -> Result<(), String> {
    let mut creds: Vec<Credential> = xml_repository::load_json("credentials.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    if let Some(pos) = creds.iter().position(|c| c.id == credential.id) {
        creds[pos] = credential;
    } else {
        creds.push(credential);
    }

    xml_repository::save_json("credentials.json", &creds).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_credential(id: String) -> Result<(), String> {
    let mut creds: Vec<Credential> = xml_repository::load_json("credentials.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    creds.retain(|c| c.id != id);
    xml_repository::save_json("credentials.json", &creds).map_err(|e| e.to_string())
}
