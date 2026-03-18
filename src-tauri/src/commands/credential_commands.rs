use crate::model::credential::Credential;
use crate::model::environment::{
    built_in_environments, is_built_in_environment, EnvironmentDefinition,
};
use crate::persistence::xml_repository;

#[tauri::command]
pub async fn get_credentials() -> Result<Vec<Credential>, String> {
    let creds: Vec<Credential> = xml_repository::load_json("credentials.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    Ok(creds)
}

#[tauri::command]
pub async fn save_credential(credential: Credential) -> Result<(), String> {
    let mut creds: Vec<Credential> = xml_repository::load_json("credentials.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    if let Some(pos) = creds.iter().position(|c| c.id == credential.id) {
        creds[pos] = credential;
    } else {
        creds.push(credential);
    }

    xml_repository::save_json("credentials.json", &creds).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_credential(id: String) -> Result<(), String> {
    let mut creds: Vec<Credential> = xml_repository::load_json("credentials.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    creds.retain(|c| c.id != id);
    xml_repository::save_json("credentials.json", &creds).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_environments() -> Result<Vec<EnvironmentDefinition>, String> {
    let mut environments = built_in_environments();
    let mut custom: Vec<EnvironmentDefinition> = xml_repository::load_json("environments.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    custom.retain(|environment| !is_built_in_environment(&environment.id));
    custom.sort_by(|left, right| left.display_name.cmp(&right.display_name));
    environments.extend(custom);
    Ok(environments)
}

#[tauri::command]
pub async fn save_environment(
    environment: EnvironmentDefinition,
) -> Result<EnvironmentDefinition, String> {
    if is_built_in_environment(&environment.id) {
        return Err("Built-in environments cannot be modified".into());
    }

    let display_name = environment.display_name.trim();
    if display_name.is_empty() {
        return Err("Environment name cannot be empty".into());
    }

    let mut environments: Vec<EnvironmentDefinition> =
        xml_repository::load_json("environments.json")
            .map_err(|e| e.to_string())?
            .unwrap_or_default();

    let saved = EnvironmentDefinition::new_custom(
        if environment.id.trim().is_empty() {
            format!("custom-{}", uuid::Uuid::new_v4().simple())
        } else {
            environment.id
        },
        display_name.to_string(),
    );

    if let Some(position) = environments.iter().position(|item| item.id == saved.id) {
        environments[position] = saved.clone();
    } else {
        environments.push(saved.clone());
    }

    xml_repository::save_json("environments.json", &environments).map_err(|e| e.to_string())?;
    Ok(saved)
}

#[tauri::command]
pub async fn delete_environment(id: String) -> Result<(), String> {
    if is_built_in_environment(&id) {
        return Err("Built-in environments cannot be deleted".into());
    }

    let credentials: Vec<Credential> = xml_repository::load_json("credentials.json")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    if credentials
        .iter()
        .any(|credential| credential.environment == id)
    {
        return Err("Environment is still used by saved credentials".into());
    }

    let mut environments: Vec<EnvironmentDefinition> =
        xml_repository::load_json("environments.json")
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
    environments.retain(|environment| environment.id != id);
    xml_repository::save_json("environments.json", &environments).map_err(|e| e.to_string())
}
