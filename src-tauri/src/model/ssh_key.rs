use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SSHKeyType {
    RSA,
    DSA,
    ECDSA,
    Ed25519,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SSHKey {
    pub id: String,
    pub name: String,
    pub path: String,
    pub key_type: SSHKeyType,
    pub encrypted_passphrase: Option<String>,
    pub copied_to_user_dir: bool,
}

impl Default for SSHKey {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            path: String::new(),
            key_type: SSHKeyType::Ed25519,
            encrypted_passphrase: None,
            copied_to_user_dir: false,
        }
    }
}
