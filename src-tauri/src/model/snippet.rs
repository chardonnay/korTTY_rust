use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    pub id: String,
    pub name: String,
    pub content: String,
    pub category: Option<String>,
    pub favorite: bool,
    pub variables: Vec<SnippetVariable>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetVariable {
    pub name: String,
    pub default_value: String,
    pub description: Option<String>,
}

impl Default for Snippet {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            content: String::new(),
            category: None,
            favorite: false,
            variables: Vec::new(),
        }
    }
}
