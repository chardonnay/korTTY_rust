use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    pub id: String,
    pub name: String,
    pub content: String,
    pub category: Option<String>,
    pub description: Option<String>,
    pub language: Option<String>,
    pub favorite: bool,
    #[serde(default)]
    pub diagrams: Vec<SnippetDiagram>,
    #[serde(default)]
    pub editor_profile_id: Option<String>,
    pub variables: Vec<SnippetVariable>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetDiagram {
    pub id: String,
    pub name: String,
    pub diagram_type: SnippetDiagramType,
    pub source: String,
    pub rendered_path: Option<String>,
    pub content_hash: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum SnippetDiagramType {
    #[default]
    PlantUml,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetEditorProfile {
    pub id: String,
    pub name: String,
    pub language: Option<String>,
    #[serde(default)]
    pub formatter_command: Option<String>,
    #[serde(default)]
    pub formatter_args: Vec<String>,
    #[serde(default = "default_tab_size")]
    pub tab_size: u8,
    #[serde(default)]
    pub insert_spaces: bool,
}

impl Default for SnippetEditorProfile {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            language: None,
            formatter_command: None,
            formatter_args: Vec::new(),
            tab_size: default_tab_size(),
            insert_spaces: true,
        }
    }
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
            description: None,
            language: Some("bash".to_string()),
            favorite: false,
            diagrams: Vec::new(),
            editor_profile_id: None,
            variables: Vec::new(),
        }
    }
}

fn default_tab_size() -> u8 {
    2
}
