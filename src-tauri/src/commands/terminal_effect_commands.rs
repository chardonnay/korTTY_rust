use crate::model::terminal_effect::{TerminalEffectPluginBundle, TerminalEffectPluginEntry};
use crate::terminal_effects;
use std::path::Path;

#[tauri::command]
pub async fn list_terminal_effect_plugins() -> Result<Vec<TerminalEffectPluginEntry>, String> {
    terminal_effects::list_plugins().map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn load_terminal_effect_plugin(
    plugin_id: String,
) -> Result<TerminalEffectPluginBundle, String> {
    terminal_effects::load_plugin_bundle(&plugin_id).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_terminal_effect_plugin_enabled(
    plugin_id: String,
    enabled: bool,
) -> Result<(), String> {
    terminal_effects::set_plugin_enabled(&plugin_id, enabled).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn import_terminal_effect_plugin(
    source_path: String,
) -> Result<TerminalEffectPluginEntry, String> {
    terminal_effects::import_plugin_package(Path::new(&source_path))
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn export_terminal_effect_plugin(
    plugin_id: String,
    target_path: String,
) -> Result<(), String> {
    terminal_effects::export_plugin_package(&plugin_id, Path::new(&target_path))
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn normalize_terminal_effect_speed(speed: f64) -> Result<f64, String> {
    Ok(terminal_effects::normalize_speed(speed))
}
