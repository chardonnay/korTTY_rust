use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GPGKey {
    pub id: String,
    pub key_id: String,
    pub name: String,
    pub email: String,
}

impl Default for GPGKey {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            key_id: String::new(),
            name: String::new(),
            email: String::new(),
        }
    }
}
