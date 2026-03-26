use crate::ai;
use crate::model::ai::{AiExecutionResult, AiProfile, AiRequestPayload, SavedAiChat};
use crate::model::settings::GlobalSettings;
use crate::persistence::xml_repository;
use std::collections::HashMap;
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
    profiles.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));
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
    xml_repository::save_json(AI_PROFILES_FILE, &profiles).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn test_ai_profile(profile: AiProfile) -> Result<bool, String> {
    let mut profile = profile;
    ai::normalize_profile(&mut profile);
    Ok(ai::test_connection(&profile).await)
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
    let Some(profile) = profiles
        .iter_mut()
        .find(|profile| profile.id == request.profile_id)
    else {
        return Err("AI profile not found".into());
    };

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
