use anyhow::{anyhow, Result};
use std::sync::{Mutex, MutexGuard};

pub struct Vault {
    master_key: Mutex<Option<Vec<u8>>>,
}

impl Vault {
    pub fn new() -> Self {
        Self {
            master_key: Mutex::new(None),
        }
    }

    pub fn unlock(&self, key: Vec<u8>) -> Result<()> {
        let mut master_key = self.lock_master_key()?;
        *master_key = Some(key);
        Ok(())
    }

    pub fn lock(&self) -> Result<()> {
        let mut master_key = self.lock_master_key()?;
        *master_key = None;
        Ok(())
    }

    pub fn is_unlocked(&self) -> Result<bool> {
        Ok(self.lock_master_key()?.is_some())
    }

    pub fn encrypt_value(&self, plaintext: &str) -> Result<String> {
        let key = self.current_key()?;
        let encrypted = super::encryption::EncryptionService::encrypt(plaintext.as_bytes(), &key)?;
        Ok(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &encrypted,
        ))
    }

    pub fn decrypt_value(&self, encrypted_b64: &str) -> Result<String> {
        let key = self.current_key()?;
        let data =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encrypted_b64)?;
        let plaintext = super::encryption::EncryptionService::decrypt(&data, &key)?;
        Ok(String::from_utf8(plaintext)?)
    }

    fn current_key(&self) -> Result<Vec<u8>> {
        self.lock_master_key()?
            .clone()
            .ok_or_else(|| anyhow!("Vault is locked"))
    }

    fn lock_master_key(&self) -> Result<MutexGuard<'_, Option<Vec<u8>>>> {
        self.master_key
            .lock()
            .map_err(|_| anyhow!("Vault state is poisoned"))
    }
}

impl Default for Vault {
    fn default() -> Self {
        Self::new()
    }
}
