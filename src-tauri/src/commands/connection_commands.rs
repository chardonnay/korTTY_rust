use crate::model::connection::{ConnectionGroup, ConnectionSettings};
use crate::persistence::xml_repository;

#[tauri::command]
pub async fn get_connections() -> Result<Vec<ConnectionSettings>, String> {
    let connections: Vec<ConnectionSettings> =
        xml_repository::load_json("connections.json")
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
    Ok(connections)
}

#[tauri::command]
pub async fn save_connection(connection: ConnectionSettings) -> Result<(), String> {
    let mut connections: Vec<ConnectionSettings> =
        xml_repository::load_json("connections.json")
            .map_err(|e| e.to_string())?
            .unwrap_or_default();

    if let Some(pos) = connections.iter().position(|c| c.id == connection.id) {
        connections[pos] = connection;
    } else {
        connections.push(connection);
    }

    xml_repository::save_json("connections.json", &connections).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_connection(id: String) -> Result<(), String> {
    let mut connections: Vec<ConnectionSettings> =
        xml_repository::load_json("connections.json")
            .map_err(|e| e.to_string())?
            .unwrap_or_default();

    connections.retain(|c| c.id != id);
    xml_repository::save_json("connections.json", &connections).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_connection_groups() -> Result<Vec<ConnectionGroup>, String> {
    let connections: Vec<ConnectionSettings> =
        xml_repository::load_json("connections.json")
            .map_err(|e| e.to_string())?
            .unwrap_or_default();

    let mut groups = std::collections::HashMap::<String, Vec<String>>::new();
    for conn in &connections {
        if let Some(group) = &conn.group {
            groups
                .entry(group.clone())
                .or_default()
                .push(conn.id.clone());
        }
    }

    Ok(groups
        .into_iter()
        .map(|(name, connections)| ConnectionGroup { name, connections })
        .collect())
}
