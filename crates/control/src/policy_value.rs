//! The Java installation policy's versioned AES-GCM value envelope.
//!
//! The key comes from a public application constant. Installation policy file
//! permissions are the security boundary for credentials stored in this format.

use std::io;

use aes_gcm::{Aes256Gcm, KeyInit, Nonce, Tag, aead::AeadInPlace};
use base64::{
    Engine as _, alphabet,
    engine::{
        DecodePaddingMode,
        general_purpose::{GeneralPurpose, GeneralPurposeConfig, STANDARD},
    },
};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

pub const PREFIX: &str = "kortty-enc:v1:";
const KEY_SEED: &[u8] = b"de.kortty.policy.value-cipher.v1";
const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;
const MAX_ENVELOPE_BYTES: usize = 1024 * 1024;
pub const MAX_PLAINTEXT_BYTES: usize =
    (MAX_ENVELOPE_BYTES - PREFIX.len()) / 4 * 3 - NONCE_BYTES - TAG_BYTES;
const DECODER: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

fn cipher() -> Aes256Gcm {
    Aes256Gcm::new(&Sha256::digest(KEY_SEED))
}

pub fn encrypt(plaintext: &str) -> io::Result<String> {
    let mut nonce = [0; NONCE_BYTES];
    getrandom::fill(&mut nonce)
        .map_err(|_| io::Error::other("Could not generate a policy nonce"))?;
    encrypt_with_nonce(plaintext, nonce)
}

fn encrypt_with_nonce(plaintext: &str, nonce: [u8; NONCE_BYTES]) -> io::Result<String> {
    if plaintext.len() > MAX_PLAINTEXT_BYTES {
        return Err(invalid("The policy value exceeds the size limit"));
    }
    let mut combined = Zeroizing::new(Vec::with_capacity(
        NONCE_BYTES + plaintext.len() + TAG_BYTES,
    ));
    combined.extend_from_slice(&nonce);
    combined.extend_from_slice(plaintext.as_bytes());
    let tag = cipher()
        .encrypt_in_place_detached(Nonce::from_slice(&nonce), b"", &mut combined[NONCE_BYTES..])
        .map_err(|_| io::Error::other("Could not encrypt the policy value"))?;
    combined.extend_from_slice(&tag);
    Ok(format!("{PREFIX}{}", STANDARD.encode(combined.as_slice())))
}

pub fn validate_envelope(envelope: &str) -> io::Result<()> {
    decode(envelope).map(drop)
}

pub fn decrypt(envelope: &str) -> io::Result<Zeroizing<String>> {
    let mut combined = Zeroizing::new(decode(envelope)?);
    let (nonce, encrypted) = combined.split_at_mut(NONCE_BYTES);
    let (body, tag) = encrypted.split_at_mut(encrypted.len() - TAG_BYTES);
    cipher()
        .decrypt_in_place_detached(Nonce::from_slice(nonce), b"", body, Tag::from_slice(tag))
        .map_err(|_| invalid("Could not authenticate the encrypted policy value"))?;
    let text = std::str::from_utf8(body)
        .map_err(|_| invalid("The encrypted policy value is not UTF-8"))?;
    Ok(Zeroizing::new(text.to_owned()))
}

fn decode(envelope: &str) -> io::Result<Vec<u8>> {
    if envelope.len() > MAX_ENVELOPE_BYTES {
        return Err(invalid("The encrypted policy value exceeds the size limit"));
    }
    let payload = envelope
        .strip_prefix(PREFIX)
        .ok_or_else(|| invalid("Expected a kortty-enc:v1 policy value"))?;
    let combined = DECODER
        .decode(payload)
        .map_err(|_| invalid("Invalid Base64 in the encrypted policy value"))?;
    if combined.len() < NONCE_BYTES + TAG_BYTES {
        return Err(invalid("The encrypted policy value is truncated"));
    }
    Ok(combined)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Generated with the pinned Java cipher parameters and nonce 00 through 0b.
    const JAVA_VALUE: &str = "policy-fixture-\u{20ac}-\u{1f512}!";
    const JAVA_ENVELOPE: &str =
        "kortty-enc:v1:AAECAwQFBgcICQoLs1QizL57Klx37cNHHXMMH3mnMVNB/nHKqChWmqK0Ft+p/Ggigm44PQ==";

    #[test]
    fn policy_value_matches_java_in_both_directions() {
        let nonce = std::array::from_fn(|index| index as u8);
        assert_eq!(
            encrypt_with_nonce(JAVA_VALUE, nonce).unwrap(),
            JAVA_ENVELOPE
        );
        assert_eq!(&*decrypt(JAVA_ENVELOPE).unwrap(), JAVA_VALUE);
        assert_eq!(
            &*decrypt(JAVA_ENVELOPE.trim_end_matches('=')).unwrap(),
            JAVA_VALUE
        );
    }

    #[test]
    fn policy_value_refuses_invalid_and_unauthenticated_envelopes() {
        for invalid in [
            "plaintext",
            "kortty-enc:v2:AAAA",
            "kortty-enc:v1:!!!!",
            "kortty-enc:v1:AAAA",
        ] {
            assert!(validate_envelope(invalid).is_err());
            assert!(decrypt(invalid).is_err());
        }

        let mut bytes = decode(JAVA_ENVELOPE).unwrap();
        let tag = bytes.last_mut().unwrap();
        *tag ^= 1;
        let tampered = format!("{PREFIX}{}", STANDARD.encode(bytes));
        assert!(validate_envelope(&tampered).is_ok());
        assert!(decrypt(&tampered).is_err());
        assert!(encrypt(&"a".repeat(MAX_PLAINTEXT_BYTES + 1)).is_err());
    }
}
