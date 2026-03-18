use crate::ai;
use crate::model::ai::{AiExecutionResult, AiProfile, AiRequestPayload, SavedAiChat};
use crate::persistence::xml_repository;

const AI_PROFILES_FILE: &str = "ai-profiles.json";
const AI_CHATS_FILE: &str = "ai-chats.json";

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
pub async fn execute_ai_action(request: AiRequestPayload) -> Result<AiExecutionResult, String> {
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

    let mut result = ai::execute_request(profile, &request)
        .await
        .map_err(|error| error.to_string())?;

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
