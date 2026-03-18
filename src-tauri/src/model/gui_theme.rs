use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiTheme {
    pub id: String,
    pub name: String,
    pub bg: String,
    pub surface: String,
    pub panel: String,
    pub border: String,
    pub text: String,
    pub text_dim: String,
    pub accent: String,
    pub accent_hover: String,
    pub success: String,
    pub warning: String,
    pub error: String,
    pub terminal: String,
}

impl Default for GuiTheme {
    fn default() -> Self {
        Self {
            id: "builtin-catppuccin-mocha".into(),
            name: "Catppuccin Mocha".into(),
            bg: "#1e1e2e".into(),
            surface: "#252536".into(),
            panel: "#2a2a3c".into(),
            border: "#3a3a4c".into(),
            text: "#cdd6f4".into(),
            text_dim: "#6c7086".into(),
            accent: "#89b4fa".into(),
            accent_hover: "#74a8fc".into(),
            success: "#a6e3a1".into(),
            warning: "#f9e2af".into(),
            error: "#f38ba8".into(),
            terminal: "#11111b".into(),
        }
    }
}
