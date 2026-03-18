use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credential {
    pub id: String,
    pub name: String,
    pub username: String,
    pub encrypted_password: Option<String>,
    #[serde(default = "super::environment::default_environment_id")]
    pub environment: String,
    pub server_pattern: Option<String>,
}

impl Default for Credential {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            username: String::new(),
            encrypted_password: None,
            environment: super::environment::default_environment_id(),
            server_pattern: None,
        }
    }
}
