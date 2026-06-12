pub mod color_filter;
pub mod jump;
pub mod keepalive;
pub mod session;
pub mod terminal_emulation;
pub mod tunnel;

use serde::Serialize;
use session::SSHSession;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

pub type SessionId = String;

/// Lock timeout for session operations to avoid deadlocks under rapid connect/disconnect.
pub const SESSION_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
/// Disconnect operation timeout.
pub const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub struct SSHManager {
    /// All session access must go through this mutex. No direct access to sessions elsewhere.
    sessions: Arc<Mutex<HashMap<SessionId, Arc<Mutex<SSHSession>>>>>,
}

impl SSHManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Inserts a session. Replaces any existing session for the same id (caller must ensure
    /// the previous session is disconnected, e.g. via take_session + background disconnect).
    pub async fn add_session(&self, id: SessionId, session: SSHSession) {
        let mut sessions = self.sessions.lock().await;
        sessions.insert(id, Arc::new(Mutex::new(session)));
    }

    /// Returns a clone of the session Arc if present. Lock is released before returning.
    pub async fn get_session(&self, id: &str) -> Option<Arc<Mutex<SSHSession>>> {
        let sessions = self.sessions.lock().await;
        sessions.get(id).cloned()
    }

    /// Removes the session from the map and returns it. Caller is responsible for disconnecting.
    /// Use this to ensure only one session per id and to disconnect before replacing.
    pub async fn remove_session(&self, id: &str) -> Option<Arc<Mutex<SSHSession>>> {
        let mut sessions = self.sessions.lock().await;
        sessions.remove(id)
    }

    /// Removes and returns existing session for this id, if any. Used by connect to replace
    /// or to ensure no duplicate id before adding. Same as remove_session; name clarifies intent.
    pub async fn take_session(&self, id: &str) -> Option<Arc<Mutex<SSHSession>>> {
        self.remove_session(id).await
    }

    pub async fn active_count(&self) -> usize {
        self.sessions.lock().await.len()
    }

    pub async fn active_names(&self) -> Vec<String> {
        self.sessions.lock().await.keys().cloned().collect()
    }
}

impl Default for SSHManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Best-known remote directories of a session for drag&drop and path hints.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDirectoryHints {
    pub current: Option<String>,
    pub home: Option<String>,
}

/// Returns the tracked current and home remote directories of a session.
/// Both values are absent until an absolute path was observed for them.
#[tauri::command]
pub async fn get_remote_directory_hints(
    state: tauri::State<'_, SSHManager>,
    session_id: String,
) -> Result<RemoteDirectoryHints, String> {
    let Some(session_arc) = state.get_session(&session_id).await else {
        return Ok(RemoteDirectoryHints::default());
    };
    let session = tokio::time::timeout(SESSION_LOCK_TIMEOUT, session_arc.lock())
        .await
        .map_err(|_| "Session busy or unavailable".to_string())?;
    let (current, home) = session.remote_directory_hints();
    Ok(RemoteDirectoryHints { current, home })
}
