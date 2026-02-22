use anyhow::Result;
use std::sync::Mutex;

pub struct Vault {
    master_key: Mutex<Option<Vec<u8>>>,
}

impl Vault {
    pub fn new() -> Self {
        Self {
            master_key: Mutex::new(None),
        }
    }

    pub fn unlock(&self, key: Vec<u8>) {
        let mut mk = self.master_key.lock().unwrap();
        *mk = Some(key);
    }

    pub fn lock(&self) {
        let mut mk = self.master_key.lock().unwrap();
        *mk = None;
    }

    pub fn is_unlocked(&self) -> bool {
        self.master_key.lock().unwrap().is_some()
    }

    pub fn encrypt_value(&self, plaintext: &str) -> Result<String> {
        let key = self
            .master_key
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Vault is locked"))?;
        let encrypted = super::encryption::EncryptionService::encrypt(plaintext.as_bytes(), &key)?;
        Ok(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &encrypted,
        ))
    }

    pub fn decrypt_value(&self, encrypted_b64: &str) -> Result<String> {
        let key = self
            .master_key
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Vault is locked"))?;
        let data =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encrypted_b64)?;
        let plaintext = super::encryption::EncryptionService::decrypt(&data, &key)?;
        Ok(String::from_utf8(plaintext)?)
    }
}

impl Default for Vault {
    fn default() -> Self {
        Self::new()
    }
}
