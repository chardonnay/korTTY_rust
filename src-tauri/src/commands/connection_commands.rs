use crate::model::connection::{ConnectionGroup, ConnectionSettings, ConnectionSource};
use crate::persistence::xml_repository;
use crate::teamwork::sync::SyncService;

#[tauri::command]
pub async fn get_connections() -> Result<Vec<ConnectionSettings>, String> {
    let mut connections: Vec<ConnectionSettings> = xml_repository::load_json("connections.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    for conn in &mut connections {
        if conn.connection_source.is_none() {
            conn.connection_source = Some(ConnectionSource::Local);
        }
    }

    if let Ok(teamwork) = SyncService::all_teamwork_connections() {
        connections.extend(teamwork);
    }

    Ok(connections)
}

#[tauri::command]
pub async fn save_connection(connection: ConnectionSettings) -> Result<(), String> {
    let mut connections: Vec<ConnectionSettings> = xml_repository::load_json("connections.json")
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
    if let Ok(teamwork) = SyncService::all_teamwork_connections() {
        if teamwork.iter().any(|c| c.id == id) {
            SyncService::mark_deleted(&id).map_err(|e| e.to_string())?;
            return Ok(());
        }
    }

    let mut connections: Vec<ConnectionSettings> = xml_repository::load_json("connections.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    connections.retain(|c| c.id != id);
    xml_repository::save_json("connections.json", &connections).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_connection_groups() -> Result<Vec<ConnectionGroup>, String> {
    let connections = get_connections().await?;

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
