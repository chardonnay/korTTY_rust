#[tauri::command]
pub fn generate_banner(text: String, font: String) -> Result<String, String> {
    crate::figlet::generate_banner(&text, &font).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_font_list() -> Vec<String> {
    crate::figlet::get_available_fonts()
}
