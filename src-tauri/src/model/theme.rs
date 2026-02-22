use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub foreground_color: String,
    pub background_color: String,
    pub cursor_color: String,
    pub selection_color: String,
    pub font_family: String,
    pub font_size: f32,
    pub ansi_colors: Vec<String>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Default Dark".into(),
            foreground_color: "#cdd6f4".into(),
            background_color: "#11111b".into(),
            cursor_color: "#89b4fa".into(),
            selection_color: "#45475a".into(),
            font_family: "JetBrains Mono".into(),
            font_size: 14.0,
            ansi_colors: vec![
                "#45475a".into(),
                "#f38ba8".into(),
                "#a6e3a1".into(),
                "#f9e2af".into(),
                "#89b4fa".into(),
                "#f5c2e7".into(),
                "#94e2d5".into(),
                "#bac2de".into(),
                "#585b70".into(),
                "#f38ba8".into(),
                "#a6e3a1".into(),
                "#f9e2af".into(),
                "#89b4fa".into(),
                "#f5c2e7".into(),
                "#94e2d5".into(),
                "#a6adc8".into(),
            ],
        }
    }
}
