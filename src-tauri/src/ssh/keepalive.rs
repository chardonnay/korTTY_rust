use anyhow::Result;
use std::time::Duration;

pub struct KeepaliveManager {
    interval: Duration,
    enabled: bool,
}

impl KeepaliveManager {
    pub fn new(interval_secs: u32, enabled: bool) -> Self {
        Self {
            interval: Duration::from_secs(interval_secs as u64),
            enabled,
        }
    }

    pub async fn start(&self, _session_id: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        // Will be implemented in Phase 6
        Ok(())
    }
}
