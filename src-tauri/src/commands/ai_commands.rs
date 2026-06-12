use crate::ai;
use crate::ai::cli_registry::{self, AiCliArgumentPreset, AiCliProviderDescriptor};
use crate::ai::{profile_selection, reasoning_discovery};
use crate::ai_skills;
use crate::model::ai::{
    AiExecutionResult, AiProfile, AiReasoningEffort, AiRequestPayload, AiSkill, SavedAiChat,
};
use crate::model::settings::GlobalSettings;
use crate::persistence::xml_repository;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use tokio::sync::oneshot;

const AI_PROFILES_FILE: &str = "ai-profiles.json";
const AI_CHATS_FILE: &str = "ai-chats.json";
const GLOBAL_SETTINGS_FILE: &str = "global-settings.json";

pub struct AiRequestCancelStore(pub Mutex<HashMap<String, oneshot::Sender<()>>>);

fn normalize_language_code(value: &str) -> String {
    value
        .trim()
        .split(['-', '_'])
        .next()
        .unwrap_or("en")
        .trim()
        .to_lowercase()
}

fn apply_response_language_default(request: &mut AiRequestPayload) -> Result<(), String> {
    if let Some(language_code) = request
        .response_language_code
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        request.response_language_code = Some(normalize_language_code(language_code));
        return Ok(());
    }

    let settings: GlobalSettings = xml_repository::load_json(GLOBAL_SETTINGS_FILE)
        .map_err(|error| error.to_string())?
        .unwrap_or_default();

    if !settings.auto_detect_language && !settings.language.trim().is_empty() {
        request.response_language_code = Some(normalize_language_code(&settings.language));
    }

    Ok(())
}

fn register_cancel_handle(
    store: &AiRequestCancelStore,
    request_id: &str,
) -> Result<oneshot::Receiver<()>, String> {
    let (cancel_tx, cancel_rx) = oneshot::channel();
    store
        .0
        .lock()
        .map_err(|error| error.to_string())?
        .insert(request_id.to_string(), cancel_tx);
    Ok(cancel_rx)
}

fn remove_cancel_handle(store: &AiRequestCancelStore, request_id: &str) {
    if let Ok(mut guard) = store.0.lock() {
        guard.remove(request_id);
    }
}

#[tauri::command]
pub async fn get_ai_profiles() -> Result<Vec<AiProfile>, String> {
    let mut profiles: Vec<AiProfile> = xml_repository::load_json(AI_PROFILES_FILE)
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    for profile in &mut profiles {
        ai::normalize_profile(profile);
        let _ = ai::refresh_usage(profile);
    }
    profiles.sort_by_key(|profile| profile.name.to_lowercase());
    Ok(profiles)
}

#[tauri::command]
pub async fn save_ai_profile(profile: AiProfile) -> Result<AiProfile, String> {
    let mut profiles: Vec<AiProfile> = xml_repository::load_json(AI_PROFILES_FILE)
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    let mut profile = profile;
    ai::normalize_profile(&mut profile);
    let _ = ai::refresh_usage(&mut profile);

    if let Some(position) = profiles.iter().position(|item| item.id == profile.id) {
        profiles[position] = profile.clone();
    } else {
        profiles.push(profile.clone());
    }

    xml_repository::save_json(AI_PROFILES_FILE, &profiles).map_err(|error| error.to_string())?;
    Ok(profile)
}

#[tauri::command]
pub async fn delete_ai_profile(id: String) -> Result<(), String> {
    let mut profiles: Vec<AiProfile> = xml_repository::load_json(AI_PROFILES_FILE)
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    profiles.retain(|profile| profile.id != id);
    xml_repository::save_json(AI_PROFILES_FILE, &profiles).map_err(|error| error.to_string())?;

    // Keep the configured default profile id valid after deletions.
    let mut settings: GlobalSettings = xml_repository::load_json(GLOBAL_SETTINGS_FILE)
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    let normalized = profile_selection::normalize_default_profile_id(
        settings.default_ai_profile_id.as_deref(),
        &profiles,
    );
    if normalized != settings.default_ai_profile_id {
        settings.default_ai_profile_id = normalized;
        xml_repository::save_json(GLOBAL_SETTINGS_FILE, &settings)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn test_ai_profile(profile: AiProfile) -> Result<bool, String> {
    let mut profile = profile;
    ai::normalize_profile(&mut profile);
    Ok(ai::test_connection(&profile).await)
}

#[tauri::command]
pub async fn list_lm_studio_models(
    api_url: String,
    api_key: Option<String>,
) -> Result<Vec<String>, String> {
    ai::list_local_lm_models(&api_url, api_key.as_deref())
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn execute_ai_action(
    app: AppHandle,
    mut request: AiRequestPayload,
    request_id: String,
) -> Result<AiExecutionResult, String> {
    apply_response_language_default(&mut request)?;
    let request_id = request_id.trim().to_string();
    if request_id.is_empty() {
        return Err("Request id is required".into());
    }

    let mut profiles: Vec<AiProfile> = xml_repository::load_json(AI_PROFILES_FILE)
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    // Resolve the profile: the fixed profile of the originating connection wins
    // when it is available, then the explicitly requested profile (id/name
    // lookup), then the configured default profile (Port of the Java
    // `resolveAiProfileForConnection` + `AiProfileSelectionSupport`).
    let selected_profile_id = {
        let requested = request.profile_id.trim();
        let connection_profile = request
            .connection_ai_profile_id
            .as_deref()
            .and_then(|profile_id| profile_selection::find_by_id(&profiles, profile_id));
        let selected = if let Some(connection_profile) = connection_profile {
            Some(connection_profile)
        } else if requested.is_empty() {
            let settings: GlobalSettings = xml_repository::load_json(GLOBAL_SETTINGS_FILE)
                .map_err(|error| error.to_string())?
                .unwrap_or_default();
            profile_selection::default_profile(&profiles, settings.default_ai_profile_id.as_deref())
        } else {
            profile_selection::find_by_lookup(&profiles, requested)
        };
        selected.map(|profile| profile.id.clone())
    };
    let Some(selected_profile_id) = selected_profile_id else {
        return Err("AI profile not found".into());
    };
    let Some(profile) = profiles
        .iter_mut()
        .find(|profile| profile.id == selected_profile_id)
    else {
        return Err("AI profile not found".into());
    };
    request.profile_id = selected_profile_id;

    ai::normalize_profile(profile);
    let selection_limit = profile.max_selection_chars.max(1);
    if request.selected_text.chars().count() > selection_limit {
        return Err(format!(
            "Selected text exceeds the profile limit of {selection_limit} characters"
        ));
    }

    let cancel_store = app
        .try_state::<AiRequestCancelStore>()
        .ok_or_else(|| "AI request cancel store not initialized".to_string())?;
    let cancel_rx = register_cancel_handle(&cancel_store, &request_id)?;
    let execution = ai::execute_request_with_cancel(profile, &request, cancel_rx).await;
    remove_cancel_handle(&cancel_store, &request_id);
    let mut result = execution.map_err(|error| error.to_string())?;

    if let Some(usage) = result.usage.as_ref() {
        result.usage_snapshot = Some(ai::record_usage(profile, usage));
        xml_repository::save_json(AI_PROFILES_FILE, &profiles)
            .map_err(|error| error.to_string())?;
    } else {
        let snapshot = ai::refresh_usage(profile);
        result.usage_snapshot = Some(snapshot);
        xml_repository::save_json(AI_PROFILES_FILE, &profiles)
            .map_err(|error| error.to_string())?;
    }

    Ok(result)
}

#[tauri::command]
pub async fn cancel_ai_request(app: AppHandle, request_id: String) -> Result<(), String> {
    let request_id = request_id.trim();
    if request_id.is_empty() {
        return Ok(());
    }

    let Some(store) = app.try_state::<AiRequestCancelStore>() else {
        return Ok(());
    };

    let sender = store
        .0
        .lock()
        .map_err(|error| error.to_string())?
        .remove(request_id);

    if let Some(cancel_tx) = sender {
        let _ = cancel_tx.send(());
    }
    Ok(())
}

#[tauri::command]
pub async fn get_ai_chats() -> Result<Vec<SavedAiChat>, String> {
    let mut chats: Vec<SavedAiChat> = xml_repository::load_json(AI_CHATS_FILE)
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    chats.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
    });
    Ok(chats)
}

#[tauri::command]
pub async fn save_ai_chat(chat: SavedAiChat) -> Result<SavedAiChat, String> {
    let mut chats: Vec<SavedAiChat> = xml_repository::load_json(AI_CHATS_FILE)
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    let mut chat = chat;
    let now = chrono::Utc::now().timestamp_millis();
    if chat.id.trim().is_empty() {
        chat.id = uuid::Uuid::new_v4().to_string();
    }
    if chat.created_at <= 0 {
        chat.created_at = now;
    }
    chat.updated_at = now;
    if chat.title.trim().is_empty() {
        chat.title = "AI Chat".into();
    } else {
        chat.title = chat.title.trim().to_string();
    }

    if let Some(position) = chats.iter().position(|item| item.id == chat.id) {
        chats[position] = chat.clone();
    } else {
        chats.push(chat.clone());
    }

    xml_repository::save_json(AI_CHATS_FILE, &chats).map_err(|error| error.to_string())?;
    Ok(chat)
}

#[tauri::command]
pub async fn delete_ai_chat(id: String) -> Result<(), String> {
    let mut chats: Vec<SavedAiChat> = xml_repository::load_json(AI_CHATS_FILE)
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    chats.retain(|chat| chat.id != id);
    xml_repository::save_json(AI_CHATS_FILE, &chats).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn get_ai_skills() -> Result<Vec<AiSkill>, String> {
    let mut settings: GlobalSettings = xml_repository::load_json(GLOBAL_SETTINGS_FILE)
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    for skill in &mut settings.ai_skills {
        ai_skills::normalize_skill(skill);
    }
    settings
        .ai_skills
        .sort_by_key(|skill| skill.name.to_lowercase());
    Ok(settings.ai_skills)
}

#[tauri::command]
pub async fn save_ai_skills(skills: Vec<AiSkill>) -> Result<Vec<AiSkill>, String> {
    let mut settings: GlobalSettings = xml_repository::load_json(GLOBAL_SETTINGS_FILE)
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    let mut normalized = skills;
    for skill in &mut normalized {
        ai_skills::normalize_skill(skill);
    }
    settings.ai_skills = normalized.clone();
    xml_repository::save_json(GLOBAL_SETTINGS_FILE, &settings)
        .map_err(|error| error.to_string())?;
    Ok(normalized)
}

#[tauri::command]
pub async fn import_ai_skill_markdown(path: String) -> Result<AiSkill, String> {
    let path_ref = Path::new(&path);
    let text = fs::read_to_string(path_ref).map_err(|error| error.to_string())?;
    ai_skills::import_skill_from_markdown(Some(path_ref), &text).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn export_ai_skill_markdown(path: String, skill: AiSkill) -> Result<(), String> {
    let markdown = ai_skills::export_skill_to_markdown(&skill);
    fs::write(path, markdown).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn list_ai_cli_providers() -> Result<Vec<AiCliProviderDescriptor>, String> {
    Ok(cli_registry::providers().to_vec())
}

#[tauri::command]
pub async fn resolve_ai_cli_executable(
    provider_id: Option<String>,
    executable_path: Option<String>,
) -> Result<Option<String>, String> {
    if let Some(path) = executable_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(cli_registry::find_executable(path));
    }
    Ok(cli_registry::find_provider_executable(
        provider_id.as_deref().unwrap_or(""),
    ))
}

#[tauri::command]
pub async fn get_ai_cli_argument_preset(
    provider_id: String,
) -> Result<Option<AiCliArgumentPreset>, String> {
    Ok(cli_registry::default_argument_preset(&provider_id))
}

#[tauri::command]
pub async fn discover_ai_reasoning_efforts(
    profile: AiProfile,
) -> Result<Vec<AiReasoningEffort>, String> {
    reasoning_discovery::discover(&profile).await
}
