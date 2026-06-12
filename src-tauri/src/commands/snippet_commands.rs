use crate::code_formatter::{self, FormatterInfo};
use crate::model::settings::GlobalSettings;
use crate::model::snippet::Snippet;
use crate::persistence::xml_repository;
use crate::snippet_tools::{
    build_embedded_one_liner, build_plantuml_preview_source, check_snippet_tools, export_snippets,
    format_snippet_code, render_plantuml, render_plantuml_svg_string, SnippetExportRequest,
    SnippetExportResult, SnippetFormatRequest, SnippetFormatResult, SnippetGlobalVariable,
    SnippetOneLinerRequest, SnippetOneLinerResult, SnippetPlantUmlRenderRequest,
    SnippetPlantUmlRenderResult, SnippetPlantUmlSvgRequest, SnippetPlantUmlSvgResult,
    SnippetToolAvailability,
};
use std::collections::HashMap;

const SNIPPET_GLOBAL_VARIABLES_FILE: &str = "snippet-variables.json";

#[tauri::command]
pub async fn get_snippets() -> Result<Vec<Snippet>, String> {
    let snippets: Vec<Snippet> = xml_repository::load_json("snippets.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    Ok(snippets)
}

/// Trims the snippet history to the configured maximum, removing the oldest
/// entries first. Mirrors SnippetEditDialog.trimHistoryToLimit in the Java app.
fn trim_snippet_history(snippet: &mut Snippet, max_size: usize) {
    let max_size = max_size.max(1);
    if snippet.history.len() > max_size {
        let excess = snippet.history.len() - max_size;
        snippet.history.drain(0..excess);
    }
}

fn snippet_history_max_size() -> usize {
    let mut settings: GlobalSettings = xml_repository::load_json("global-settings.json")
        .ok()
        .flatten()
        .unwrap_or_default();
    settings.normalize();
    settings.snippet_history_max_size as usize
}

#[tauri::command]
pub async fn save_snippet(mut snippet: Snippet) -> Result<(), String> {
    trim_snippet_history(&mut snippet, snippet_history_max_size());

    let mut snippets: Vec<Snippet> = xml_repository::load_json("snippets.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    if let Some(pos) = snippets.iter().position(|s| s.id == snippet.id) {
        snippets[pos] = snippet;
    } else {
        snippets.push(snippet);
    }

    xml_repository::save_json("snippets.json", &snippets).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_snippet(id: String) -> Result<(), String> {
    let mut snippets: Vec<Snippet> = xml_repository::load_json("snippets.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    snippets.retain(|s| s.id != id);
    xml_repository::save_json("snippets.json", &snippets).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_snippet_global_variables() -> Result<Vec<SnippetGlobalVariable>, String> {
    let mut variables: Vec<SnippetGlobalVariable> =
        xml_repository::load_json(SNIPPET_GLOBAL_VARIABLES_FILE)
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
    variables.sort_by_key(|variable| variable.name.to_lowercase());
    Ok(variables)
}

#[tauri::command]
pub async fn save_snippet_global_variables(
    variables: Vec<SnippetGlobalVariable>,
) -> Result<(), String> {
    let mut normalized = Vec::<SnippetGlobalVariable>::new();
    for variable in variables {
        let name = variable.name.trim();
        if name.is_empty() {
            continue;
        }
        if normalized
            .iter()
            .any(|existing| existing.name.eq_ignore_ascii_case(name))
        {
            continue;
        }
        normalized.push(SnippetGlobalVariable {
            name: name.to_string(),
            value: variable.value,
            description: variable
                .description
                .map(|description| description.trim().to_string())
                .filter(|description| !description.is_empty()),
        });
    }
    normalized.sort_by_key(|variable| variable.name.to_lowercase());
    xml_repository::save_json(SNIPPET_GLOBAL_VARIABLES_FILE, &normalized)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn build_snippet_one_liner(
    request: SnippetOneLinerRequest,
) -> Result<SnippetOneLinerResult, String> {
    build_embedded_one_liner(&request).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn format_snippet(request: SnippetFormatRequest) -> Result<SnippetFormatResult, String> {
    format_snippet_code(&request)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn get_snippet_formatter_info(language: String) -> Result<Option<FormatterInfo>, String> {
    Ok(code_formatter::get_formatter_info(&language))
}

#[tauri::command]
pub async fn get_formatter_manifest() -> Result<HashMap<String, String>, String> {
    Ok(code_formatter::formatter_manifest())
}

#[tauri::command]
pub async fn export_snippet_scripts(
    request: SnippetExportRequest,
) -> Result<SnippetExportResult, String> {
    export_snippets(&request).map_err(|error| error.to_string())
}

/// Renders PlantUML to a file. Runs on a blocking thread because PlantUML
/// rendering shells out (and may launch a JVM), which can take seconds and must
/// not stall the async runtime.
#[tauri::command]
pub async fn render_snippet_plantuml(
    request: SnippetPlantUmlRenderRequest,
) -> Result<SnippetPlantUmlRenderResult, String> {
    tokio::task::spawn_blocking(move || render_plantuml(&request))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

/// WP2.10: renders PlantUML to an in-memory SVG string for the snippet
/// diagram dialog. Runs on a blocking thread because PlantUML rendering
/// shells out and can take seconds.
#[tauri::command]
pub async fn render_snippet_plantuml_svg(
    source: String,
    background_color: Option<String>,
) -> Result<SnippetPlantUmlSvgResult, String> {
    let request = SnippetPlantUmlSvgRequest {
        source,
        background_color,
        plantuml_jar_path: None,
    };
    tokio::task::spawn_blocking(move || render_plantuml_svg_string(&request))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn build_snippet_plantuml_preview(snippet: Snippet) -> Result<String, String> {
    Ok(build_plantuml_preview_source(&snippet))
}

#[tauri::command]
pub async fn check_snippet_tools_available() -> Result<SnippetToolAvailability, String> {
    Ok(check_snippet_tools())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::snippet::SnippetHistoryEntry;

    fn snippet_with_history(count: usize) -> Snippet {
        Snippet {
            history: (0..count)
                .map(|index| SnippetHistoryEntry {
                    content: format!("entry-{index}"),
                    timestamp: index as i64,
                })
                .collect(),
            ..Snippet::default()
        }
    }

    #[test]
    fn trim_snippet_history_removes_oldest_entries_first() {
        let mut snippet = snippet_with_history(5);
        trim_snippet_history(&mut snippet, 3);
        assert_eq!(snippet.history.len(), 3);
        assert_eq!(snippet.history[0].content, "entry-2");
        assert_eq!(snippet.history[2].content, "entry-4");
    }

    #[test]
    fn trim_snippet_history_keeps_short_history_untouched() {
        let mut snippet = snippet_with_history(2);
        trim_snippet_history(&mut snippet, 30);
        assert_eq!(snippet.history.len(), 2);
        assert_eq!(snippet.history[0].content, "entry-0");
    }

    #[test]
    fn trim_snippet_history_enforces_minimum_of_one_entry() {
        let mut snippet = snippet_with_history(4);
        trim_snippet_history(&mut snippet, 0);
        assert_eq!(snippet.history.len(), 1);
        assert_eq!(snippet.history[0].content, "entry-3");
    }
}
