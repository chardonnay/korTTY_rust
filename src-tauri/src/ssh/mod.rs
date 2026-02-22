pub mod session;
pub mod tunnel;
pub mod jump;
pub mod keepalive;

use session::SSHSession;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

pub type SessionId = String;

pub struct SSHManager {
    sessions: Arc<Mutex<HashMap<SessionId, Arc<Mutex<SSHSession>>>>>,
}

impl SSHManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn add_session(&self, id: SessionId, session: SSHSession) {
        let mut sessions = self.sessions.lock().await;
        sessions.insert(id, Arc::new(Mutex::new(session)));
    }

    pub async fn get_session(&self, id: &str) -> Option<Arc<Mutex<SSHSession>>> {
        let sessions = self.sessions.lock().await;
        sessions.get(id).cloned()
    }

    pub async fn remove_session(&self, id: &str) -> Option<Arc<Mutex<SSHSession>>> {
        let mut sessions = self.sessions.lock().await;
        sessions.remove(id)
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
