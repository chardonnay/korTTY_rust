use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TunnelType {
    Local,
    Remote,
    Dynamic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelConfig {
    pub id: String,
    pub tunnel_type: TunnelType,
    pub local_host: String,
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    pub description: Option<String>,
    pub enabled: bool,
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            tunnel_type: TunnelType::Local,
            local_host: "localhost".into(),
            local_port: 0,
            remote_host: String::new(),
            remote_port: 0,
            description: None,
            enabled: true,
        }
    }
}
