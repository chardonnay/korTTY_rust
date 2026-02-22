use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::Result;
use rand::RngCore;

const NONCE_LENGTH: usize = 12;
const KEY_LENGTH: usize = 32;

pub struct EncryptionService;

impl EncryptionService {
    pub fn derive_key(master_password: &str, salt: &[u8]) -> Vec<u8> {
        let mut key = vec![0u8; KEY_LENGTH];
        pbkdf2::pbkdf2_hmac::<sha2::Sha256>(master_password.as_bytes(), salt, 310_000, &mut key);
        key
    }

    pub fn encrypt(plaintext: &[u8], key: &[u8]) -> Result<Vec<u8>> {
        let cipher =
            Aes256Gcm::new_from_slice(key).map_err(|e| anyhow::anyhow!("Invalid key: {}", e))?;

        let mut nonce_bytes = [0u8; NONCE_LENGTH];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| anyhow::anyhow!("Encryption failed: {}", e))?;

        let mut result = nonce_bytes.to_vec();
        result.extend_from_slice(&ciphertext);
        Ok(result)
    }

    pub fn decrypt(data: &[u8], key: &[u8]) -> Result<Vec<u8>> {
        if data.len() < NONCE_LENGTH {
            anyhow::bail!("Data too short for decryption");
        }

        let cipher =
            Aes256Gcm::new_from_slice(key).map_err(|e| anyhow::anyhow!("Invalid key: {}", e))?;

        let nonce = Nonce::from_slice(&data[..NONCE_LENGTH]);
        let ciphertext = &data[NONCE_LENGTH..];

        let plaintext = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow::anyhow!("Decryption failed: {}", e))?;

        Ok(plaintext)
    }
}
