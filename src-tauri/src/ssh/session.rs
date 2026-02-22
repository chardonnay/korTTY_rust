use crate::model::connection::{AuthMethod, ConnectionSettings};
use anyhow::Result;
use async_trait::async_trait;
use russh::*;
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct SSHSession {
    pub connection_id: String,
    pub settings: ConnectionSettings,
    handle: Option<client::Handle<SSHHandler>>,
    channel: Option<Channel<client::Msg>>,
    output_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
}

struct SSHHandler {
    output_tx: mpsc::UnboundedSender<Vec<u8>>,
}

#[async_trait]
impl client::Handler for SSHHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh_keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }

    async fn data(
        &mut self,
        _channel: ChannelId,
        data: &[u8],
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let _ = self.output_tx.send(data.to_vec());
        Ok(())
    }
}

impl SSHSession {
    pub fn new(settings: ConnectionSettings) -> Self {
        Self {
            connection_id: settings.id.clone(),
            settings,
            handle: None,
            channel: None,
            output_tx: None,
        }
    }

    pub async fn connect(
        &mut self,
        output_tx: mpsc::UnboundedSender<Vec<u8>>,
    ) -> Result<()> {
        self.output_tx = Some(output_tx.clone());

        let config = Arc::new(client::Config {
            ..Default::default()
        });

        let handler = SSHHandler {
            output_tx: output_tx.clone(),
        };

        let addr = format!("{}:{}", self.settings.host, self.settings.port);
        let mut handle = client::connect(config, &addr, handler).await?;

        match &self.settings.auth_method {
            AuthMethod::Password => {
                let password = self.settings.password.clone().unwrap_or_default();
                let authenticated = handle
                    .authenticate_password(&self.settings.username, &password)
                    .await?;
                if !authenticated {
                    anyhow::bail!("Password authentication failed");
                }
            }
            AuthMethod::PrivateKey => {
                anyhow::bail!("Private key auth will be implemented in Phase 4");
            }
        }

        let channel = handle.channel_open_session().await?;

        let term = "xterm-256color";
        let cols = self.settings.columns as u32;
        let rows = self.settings.rows as u32;
        channel
            .request_pty(false, term, cols, rows, 0, 0, &[])
            .await?;
        channel.request_shell(false).await?;

        self.handle = Some(handle);
        self.channel = Some(channel);

        Ok(())
    }

    pub async fn send_data(&mut self, data: &[u8]) -> Result<()> {
        if let Some(channel) = &mut self.channel {
            channel.data(&data[..]).await?;
        }
        Ok(())
    }

    pub async fn resize(&mut self, cols: u32, rows: u32) -> Result<()> {
        if let Some(channel) = &mut self.channel {
            channel
                .window_change(cols, rows, 0, 0)
                .await?;
        }
        Ok(())
    }

    pub async fn disconnect(&mut self) -> Result<()> {
        if let Some(handle) = self.handle.take() {
            let _ = handle
                .disconnect(Disconnect::ByApplication, "User disconnected", "")
                .await;
        }
        self.channel = None;
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.handle.is_some()
    }
}
