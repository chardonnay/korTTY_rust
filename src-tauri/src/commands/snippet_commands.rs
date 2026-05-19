use crate::model::snippet::Snippet;
use crate::persistence::xml_repository;
use crate::snippet_tools::{
    build_embedded_one_liner, build_plantuml_preview_source, check_snippet_tools, export_snippets,
    format_snippet_code, render_plantuml, SnippetExportRequest, SnippetExportResult,
    SnippetFormatRequest, SnippetFormatResult, SnippetGlobalVariable, SnippetOneLinerRequest,
    SnippetOneLinerResult, SnippetPlantUmlRenderRequest, SnippetPlantUmlRenderResult,
    SnippetToolAvailability,
};

const SNIPPET_GLOBAL_VARIABLES_FILE: &str = "snippet-variables.json";

#[tauri::command]
pub async fn get_snippets() -> Result<Vec<Snippet>, String> {
    let snippets: Vec<Snippet> = xml_repository::load_json("snippets.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    Ok(snippets)
}

#[tauri::command]
pub async fn save_snippet(snippet: Snippet) -> Result<(), String> {
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
    format_snippet_code(&request).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn export_snippet_scripts(
    request: SnippetExportRequest,
) -> Result<SnippetExportResult, String> {
    export_snippets(&request).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn render_snippet_plantuml(
    request: SnippetPlantUmlRenderRequest,
) -> Result<SnippetPlantUmlRenderResult, String> {
    render_plantuml(&request).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn build_snippet_plantuml_preview(snippet: Snippet) -> Result<String, String> {
    Ok(build_plantuml_preview_source(&snippet))
}

#[tauri::command]
pub async fn check_snippet_tools_available() -> Result<SnippetToolAvailability, String> {
    Ok(check_snippet_tools())
}
