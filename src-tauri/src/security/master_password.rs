use anyhow::Result;
use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;

const PBKDF2_ITERATIONS: u32 = 310_000;
const SALT_LENGTH: usize = 32;
const HASH_LENGTH: usize = 32;

pub struct MasterPassword;

impl MasterPassword {
    pub fn hash_password(password: &str, salt: &[u8]) -> Vec<u8> {
        let mut hash = vec![0u8; HASH_LENGTH];
        pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, PBKDF2_ITERATIONS, &mut hash);
        hash
    }

    pub fn generate_salt() -> Vec<u8> {
        use rand::RngCore;
        let mut salt = vec![0u8; SALT_LENGTH];
        rand::thread_rng().fill_bytes(&mut salt);
        salt
    }

    pub fn verify_password(password: &str, salt: &[u8], expected_hash: &[u8]) -> bool {
        let hash = Self::hash_password(password, salt);
        hash == expected_hash
    }

    pub fn store_hash(hash: &[u8], salt: &[u8]) -> Result<()> {
        let config_dir = crate::persistence::xml_repository::config_dir()?;
        let path = config_dir.join("master-password-hash");
        let mut data = salt.to_vec();
        data.extend_from_slice(hash);
        std::fs::write(path, base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data))?;
        Ok(())
    }

    pub fn load_hash() -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        let config_dir = crate::persistence::xml_repository::config_dir()?;
        let path = config_dir.join("master-password-hash");
        if !path.exists() {
            return Ok(None);
        }
        let encoded = std::fs::read_to_string(path)?;
        let data = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded.trim())?;
        if data.len() < SALT_LENGTH + HASH_LENGTH {
            anyhow::bail!("Invalid master password hash file");
        }
        let salt = data[..SALT_LENGTH].to_vec();
        let hash = data[SALT_LENGTH..SALT_LENGTH + HASH_LENGTH].to_vec();
        Ok(Some((salt, hash)))
    }
}
