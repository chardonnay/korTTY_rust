use crate::model::connection::ConnectionSettings;
use crate::ssh::session::SSHSession;
use crate::ssh::{SSHManager, DISCONNECT_TIMEOUT, SESSION_LOCK_TIMEOUT};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::mpsc;
use tokio::time::timeout;

/// Disconnects a session with timeouts to avoid deadlock. Intended for background cleanup.
async fn disconnect_session_with_timeout(
    session_arc: std::sync::Arc<tokio::sync::Mutex<SSHSession>>,
) {
    let Ok(mut guard) = timeout(SESSION_LOCK_TIMEOUT, session_arc.lock()).await else {
        return;
    };
    let _ = timeout(DISCONNECT_TIMEOUT, guard.disconnect()).await;
}

#[tauri::command]
pub async fn ssh_connect(
    app: AppHandle,
    state: State<'_, SSHManager>,
    session_id: String,
    settings: ConnectionSettings,
) -> Result<(), String> {
    // Remove any existing session for this id first to avoid duplicate ids and races.
    if let Some(old) = state.take_session(&session_id).await {
        tokio::spawn(disconnect_session_with_timeout(old));
    }

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
    let Some(session_arc) = state.remove_session(&session_id).await else {
        return Ok(());
    };
    let lock_result = timeout(SESSION_LOCK_TIMEOUT, session_arc.lock()).await;
    match lock_result {
        Ok(mut guard) => {
            let _ = timeout(DISCONNECT_TIMEOUT, guard.disconnect()).await;
        }
        Err(_) => {
            tokio::spawn(disconnect_session_with_timeout(std::sync::Arc::clone(
                &session_arc,
            )));
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn ssh_send_input(
    state: State<'_, SSHManager>,
    session_id: String,
    data: Vec<u8>,
) -> Result<(), String> {
    let Some(session_arc) = state.get_session(&session_id).await else {
        return Ok(());
    };
    let mut session = timeout(SESSION_LOCK_TIMEOUT, session_arc.lock())
        .await
        .map_err(|_| "Session busy or unavailable".to_string())?;
    session.send_data(&data).await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn ssh_resize(
    state: State<'_, SSHManager>,
    session_id: String,
    cols: u32,
    rows: u32,
) -> Result<(), String> {
    let Some(session_arc) = state.get_session(&session_id).await else {
        return Ok(());
    };
    let mut session = timeout(SESSION_LOCK_TIMEOUT, session_arc.lock())
        .await
        .map_err(|_| "Session busy or unavailable".to_string())?;
    session
        .resize(cols, rows)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
