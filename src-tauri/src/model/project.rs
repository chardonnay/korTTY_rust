use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub name: String,
    pub file_path: Option<String>,
    pub connection_ids: Vec<String>,
    pub window_geometry: Option<WindowGeometry>,
    pub dashboard_open: bool,
    pub split_pane_state: Option<SplitPaneState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowGeometry {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub maximized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitPaneState {
    pub orientation: SplitOrientation,
    pub ratios: Vec<f64>,
    pub session_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SplitOrientation {
    Horizontal,
    Vertical,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            name: "Untitled".into(),
            file_path: None,
            connection_ids: Vec::new(),
            window_geometry: None,
            dashboard_open: false,
            split_pane_state: None,
        }
    }
}
