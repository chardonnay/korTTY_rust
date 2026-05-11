use serde::{Deserialize, Serialize};

pub const TERMINAL_EFFECT_SPEED_MINIMUM: f64 = 1.0;
pub const TERMINAL_EFFECT_SPEED_DEFAULT: f64 = 1.0;
pub const TERMINAL_EFFECT_SPEED_SLIDER_MAXIMUM: f64 = 10.0;
pub const TERMINAL_EFFECT_SPEED_MAXIMUM: f64 = 99.0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TerminalEffectPluginSource {
    Bundled,
    Imported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalEffectPluginManifest {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub entry: String,
    pub css: Option<String>,
    #[serde(default)]
    pub assets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalEffectPluginEntry {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub entry_path: String,
    pub css_path: Option<String>,
    pub package_path: String,
    pub source: TerminalEffectPluginSource,
    pub enabled: bool,
    pub bundled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalEffectPluginBundle {
    pub manifest: TerminalEffectPluginManifest,
    pub entry_js: String,
    pub css: Option<String>,
}

pub fn normalize_terminal_effect_speed(speed: f64) -> f64 {
    if !speed.is_finite() || speed <= 0.0 {
        return TERMINAL_EFFECT_SPEED_DEFAULT;
    }
    speed.clamp(TERMINAL_EFFECT_SPEED_MINIMUM, TERMINAL_EFFECT_SPEED_MAXIMUM)
}

impl TerminalEffectPluginManifest {
    pub fn bundled_mother() -> Self {
        Self {
            id: "mother".into(),
            name: "MU/TH/UR 6000".into(),
            version: Some("1.0.0".into()),
            description: Some(
                "ALIEN-style green CRT terminal appearance with scanlines, noise, paced visible output, immediate control sequences, high-volume bypass, and output line flash."
                    .into(),
            ),
            entry: "mother.js".into(),
            css: Some("mother.css".into()),
            assets: Vec::new(),
        }
    }
}
