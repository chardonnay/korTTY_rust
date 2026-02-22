use crate::model::gpg_key::GPGKey;
use crate::model::ssh_key::SSHKey;
use crate::persistence::xml_repository;

#[tauri::command]
pub async fn get_ssh_keys() -> Result<Vec<SSHKey>, String> {
    let keys: Vec<SSHKey> = xml_repository::load_json("ssh-keys.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    Ok(keys)
}

#[tauri::command]
pub async fn save_ssh_key(key: SSHKey) -> Result<(), String> {
    let mut keys: Vec<SSHKey> = xml_repository::load_json("ssh-keys.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    if let Some(pos) = keys.iter().position(|k| k.id == key.id) {
        keys[pos] = key;
    } else {
        keys.push(key);
    }

    xml_repository::save_json("ssh-keys.json", &keys).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_ssh_key(id: String) -> Result<(), String> {
    let mut keys: Vec<SSHKey> = xml_repository::load_json("ssh-keys.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    keys.retain(|k| k.id != id);
    xml_repository::save_json("ssh-keys.json", &keys).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_gpg_keys() -> Result<Vec<GPGKey>, String> {
    let keys: Vec<GPGKey> = xml_repository::load_json("gpg-keys.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    Ok(keys)
}

#[tauri::command]
pub async fn save_gpg_key(key: GPGKey) -> Result<(), String> {
    let mut keys: Vec<GPGKey> = xml_repository::load_json("gpg-keys.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    if let Some(pos) = keys.iter().position(|k| k.id == key.id) {
        keys[pos] = key;
    } else {
        keys.push(key);
    }

    xml_repository::save_json("gpg-keys.json", &keys).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_gpg_key(id: String) -> Result<(), String> {
    let mut keys: Vec<GPGKey> = xml_repository::load_json("gpg-keys.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    keys.retain(|k| k.id != id);
    xml_repository::save_json("gpg-keys.json", &keys).map_err(|e| e.to_string())
}
