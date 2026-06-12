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
    #[serde(default)]
    pub history: Vec<SnippetHistoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetHistoryEntry {
    pub content: String,
    pub timestamp: i64,
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
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub custom_instructions: Option<String>,
    #[serde(default)]
    pub code_references: Vec<SnippetCodeReference>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetCodeReference {
    pub label: String,
    pub start_line: u32,
    pub end_line: u32,
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
    pub built_in: bool,
    #[serde(default)]
    pub formatter_command: Option<String>,
    #[serde(default)]
    pub formatter_args: Vec<String>,
    #[serde(default = "default_tab_size")]
    pub tab_size: u8,
    #[serde(default = "default_insert_spaces")]
    pub insert_spaces: bool,
    #[serde(default)]
    pub foreground_color: Option<String>,
    #[serde(default)]
    pub background_color: Option<String>,
    /// LINE, UNDERSCORE or BLOCK.
    #[serde(default)]
    pub cursor_style: Option<String>,
    #[serde(default)]
    pub cursor_color: Option<String>,
    #[serde(default)]
    pub comment_color: Option<String>,
    #[serde(default)]
    pub string_color: Option<String>,
    #[serde(default)]
    pub number_color: Option<String>,
    #[serde(default)]
    pub boolean_color: Option<String>,
    #[serde(default)]
    pub key_color: Option<String>,
    #[serde(default)]
    pub keyword_color: Option<String>,
    #[serde(default)]
    pub section_color: Option<String>,
    #[serde(default)]
    pub variable_color: Option<String>,
    #[serde(default)]
    pub brace_color: Option<String>,
}

impl Default for SnippetEditorProfile {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            language: None,
            built_in: false,
            formatter_command: None,
            formatter_args: Vec::new(),
            tab_size: default_tab_size(),
            insert_spaces: default_insert_spaces(),
            foreground_color: None,
            background_color: None,
            cursor_style: None,
            cursor_color: None,
            comment_color: None,
            string_color: None,
            number_color: None,
            boolean_color: None,
            key_color: None,
            keyword_color: None,
            section_color: None,
            variable_color: None,
            brace_color: None,
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
            history: Vec::new(),
        }
    }
}

impl Default for SnippetDiagram {
    fn default() -> Self {
        let now = chrono::Utc::now().timestamp_millis();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            diagram_type: SnippetDiagramType::PlantUml,
            source: String::new(),
            rendered_path: None,
            content_hash: None,
            title: None,
            custom_instructions: None,
            code_references: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

fn default_tab_size() -> u8 {
    2
}

fn default_insert_spaces() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_profile_defaults_insert_spaces_to_true_for_legacy_payloads() {
        // Old payloads written before insertSpaces existed must keep spaces on.
        let profile: SnippetEditorProfile =
            serde_json::from_str(r#"{"id":"p1","name":"Legacy"}"#).unwrap();
        assert!(profile.insert_spaces);
        assert_eq!(profile.tab_size, default_tab_size());
    }

    #[test]
    fn editor_profile_honors_explicit_insert_spaces_false() {
        let profile: SnippetEditorProfile =
            serde_json::from_str(r#"{"id":"p1","name":"Tabs","insertSpaces":false}"#).unwrap();
        assert!(!profile.insert_spaces);
    }
}
