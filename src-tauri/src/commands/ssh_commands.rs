use crate::model::connection::ConnectionSettings;
use crate::ssh::SSHManager;
use crate::ssh::session::SSHSession;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::mpsc;

#[tauri::command]
pub async fn ssh_connect(
    app: AppHandle,
    state: State<'_, SSHManager>,
    session_id: String,
    settings: ConnectionSettings,
) -> Result<(), String> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();

    let mut session = SSHSession::new(settings);
    session.connect(tx).await.map_err(|e| e.to_string())?;

    state.add_session(session_id.clone(), session).await;

    let app_clone = app.clone();
    let sid = session_id.clone();
    tokio::spawn(async move {
        while let Some(data) = rx.recv().await {
            let _ = app_clone.emit(&format!("terminal-output-{}", sid), data);
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn ssh_disconnect(
    state: State<'_, SSHManager>,
    session_id: String,
) -> Result<(), String> {
    if let Some(session_arc) = state.remove_session(&session_id).await {
        let mut session = session_arc.lock().await;
        session.disconnect().await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn ssh_send_input(
    state: State<'_, SSHManager>,
    session_id: String,
    data: Vec<u8>,
) -> Result<(), String> {
    if let Some(session_arc) = state.get_session(&session_id).await {
        let mut session = session_arc.lock().await;
        session.send_data(&data).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn ssh_resize(
    state: State<'_, SSHManager>,
    session_id: String,
    cols: u32,
    rows: u32,
) -> Result<(), String> {
    if let Some(session_arc) = state.get_session(&session_id).await {
        let mut session = session_arc.lock().await;
        session.resize(cols, rows).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}
