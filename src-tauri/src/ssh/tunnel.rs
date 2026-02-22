use crate::model::tunnel::TunnelConfig;
use anyhow::Result;

pub struct TunnelManager {
    active_tunnels: Vec<ActiveTunnel>,
}

struct ActiveTunnel {
    config: TunnelConfig,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
}

impl TunnelManager {
    pub fn new() -> Self {
        Self {
            active_tunnels: Vec::new(),
        }
    }

    pub async fn start_tunnel(
        &mut self,
        _config: TunnelConfig,
        _handle: &russh::client::Handle<impl russh::client::Handler>,
    ) -> Result<()> {
        // Will be implemented in Phase 6
        Ok(())
    }

    pub async fn stop_tunnel(&mut self, _tunnel_id: &str) -> Result<()> {
        Ok(())
    }

    pub fn stop_all(&mut self) {
        self.active_tunnels.clear();
    }
}

impl Default for TunnelManager {
    fn default() -> Self {
        Self::new()
    }
}
