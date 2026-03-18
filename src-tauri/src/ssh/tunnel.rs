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
        config: TunnelConfig,
        _handle: &russh::client::Handle<impl russh::client::Handler>,
    ) -> Result<()> {
        let (shutdown_tx, _shutdown_rx) = tokio::sync::oneshot::channel();
        self.active_tunnels.push(ActiveTunnel {
            config,
            shutdown_tx,
        });
        Ok(())
    }

    pub async fn stop_tunnel(&mut self, tunnel_id: &str) -> Result<()> {
        if let Some(index) = self
            .active_tunnels
            .iter()
            .position(|tunnel| tunnel.config.id == tunnel_id)
        {
            let active_tunnel = self.active_tunnels.swap_remove(index);
            let _ = active_tunnel.shutdown_tx.send(());
        }
        Ok(())
    }

    pub fn stop_all(&mut self) {
        for active_tunnel in self.active_tunnels.drain(..) {
            let _ = active_tunnel.shutdown_tx.send(());
        }
    }
}

impl Default for TunnelManager {
    fn default() -> Self {
        Self::new()
    }
}
