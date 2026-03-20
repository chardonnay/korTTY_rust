use crate::security::{
    encryption::EncryptionService, master_password::MasterPassword, vault::Vault,
};
use serde::Serialize;
use tauri::State;

const MIN_MASTER_PASSWORD_LENGTH: usize = 8;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterPasswordStatus {
    pub has_password: bool,
    pub unlocked: bool,
}

#[tauri::command]
pub async fn get_master_password_status(
    vault: State<'_, Vault>,
) -> Result<MasterPasswordStatus, String> {
    let has_password = MasterPassword::load_hash()
        .map_err(|e| e.to_string())?
        .is_some();
    let unlocked = vault.is_unlocked().map_err(|e| e.to_string())?;

    Ok(MasterPasswordStatus {
        has_password,
        unlocked,
    })
}

#[tauri::command]
pub async fn set_master_password(password: String, vault: State<'_, Vault>) -> Result<(), String> {
    validate_new_master_password(&password)?;

    if MasterPassword::load_hash()
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err("A master password is already configured.".into());
    }

    let salt = MasterPassword::generate_salt();
    let hash = MasterPassword::hash_password(&password, &salt);
    MasterPassword::store_hash(&hash, &salt).map_err(|e| e.to_string())?;

    let key = EncryptionService::derive_key(&password, &salt);
    vault.unlock(key).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn unlock_master_password(
    password: String,
    vault: State<'_, Vault>,
) -> Result<(), String> {
    if password.is_empty() {
        return Err("Please enter your master password.".into());
    }

    let Some((salt, expected_hash)) = MasterPassword::load_hash().map_err(|e| e.to_string())?
    else {
        return Err("No master password is configured yet.".into());
    };

    if !MasterPassword::verify_password(&password, &salt, &expected_hash) {
        return Err("Incorrect master password.".into());
    }

    let key = EncryptionService::derive_key(&password, &salt);
    vault.unlock(key).map_err(|e| e.to_string())
}

fn validate_new_master_password(password: &str) -> Result<(), String> {
    if password.is_empty() {
        return Err("Please enter a master password.".into());
    }

    if password.chars().count() < MIN_MASTER_PASSWORD_LENGTH {
        return Err(format!(
            "Master password must be at least {MIN_MASTER_PASSWORD_LENGTH} characters long."
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_new_master_password;

    #[test]
    fn rejects_short_master_passwords() {
        let result = validate_new_master_password("short");

        assert_eq!(
            result.expect_err("short password should be rejected"),
            "Master password must be at least 8 characters long."
        );
    }

    #[test]
    fn accepts_reasonable_master_passwords() {
        assert!(validate_new_master_password("LongEnough123").is_ok());
    }
}
