//! Optional protection for a portable connection export, without a plaintext staging file.

use std::io::{self, Write};
use std::path::Path;

use kortty_vault::Secret;
use zeroize::Zeroizing;
use zip::write::FileOptions;
use zip::{AesMode, CompressionMethod, System, ZipWriter};

use crate::snippets::{
    archive::{ZipSettings, encrypt_saved_payload},
    recipients::Choice,
};

pub(crate) enum Protection<'a> {
    None,
    Password(&'a Secret),
    OpenPgp {
        profile: &'a Path,
        recipient: &'a Choice,
    },
}

pub(crate) fn protect(
    payload: Vec<u8>,
    extension: &'static str,
    protection: Protection<'_>,
) -> io::Result<Vec<u8>> {
    if extension.is_empty()
        || extension.len() > 16
        || !extension
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid connection export extension",
        ));
    }
    match protection {
        Protection::None => Ok(payload),
        Protection::Password(password) => zip_payload(
            &format!("connections.{extension}"),
            &Zeroizing::new(payload),
            Some(password),
        ),
        Protection::OpenPgp { profile, recipient } => {
            // The Java connection manager expects a ZIP after OpenPGP decryption.
            let archive = Zeroizing::new(zip_payload(
                &format!("connections.{extension}"),
                &Zeroizing::new(payload),
                None,
            )?);
            let mut encrypted = Vec::new();
            encrypt_saved_payload(
                profile,
                recipient,
                &archive,
                &mut encrypted,
                "connections.zip",
            )?;
            Ok(encrypted)
        }
    }
}

fn zip_payload(name: &str, payload: &[u8], password: Option<&Secret>) -> io::Result<Vec<u8>> {
    if password.is_some_and(|value| !(1..=ZipSettings::MAX_PASSWORD_BYTES).contains(&value.len())) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "A ZIP password must contain 1 to 1024 UTF-8 bytes",
        ));
    }

    // Streaming ZIP uses AE-2, which leaves the plaintext CRC out of the archive.
    let mut archive = ZipWriter::new_stream(Vec::new());
    let options = FileOptions::<()>::default()
        .system(System::Unix)
        .unix_permissions(0o600)
        .compression_method(CompressionMethod::Stored);
    let options = password.map_or(options, |password| {
        options.with_aes_encryption_bytes(AesMode::Aes256, password.as_bytes())
    });
    archive.start_file(name, options)?;
    archive.write_all(payload)?;
    Ok(archive.finish()?.into_inner())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Cursor, Read};
    use std::path::PathBuf;

    use pgp::composed::SignedPublicKey;
    use pgp::types::{KeyDetails, KeyVersion};
    use zip::ZipArchive;

    use super::*;
    use crate::snippets::{archive::test_openpgp_key, recipients};

    const INVENTORY: &[u8] = b"<connection host=\"example.invalid\"/>";

    #[test]
    fn connection_export_protection_preserves_plain_output_and_rejects_unsafe_members() {
        assert_eq!(
            protect(INVENTORY.to_vec(), "xml", Protection::None).unwrap(),
            INVENTORY
        );
        assert!(protect(INVENTORY.to_vec(), "../xml", Protection::None).is_err());
    }

    #[test]
    fn connection_export_protection_password_uses_a_single_encrypted_member() {
        let password = Secret::new("temporary export test password".into());
        let archive = protect(INVENTORY.to_vec(), "xml", Protection::Password(&password)).unwrap();
        assert!(
            !archive
                .windows(INVENTORY.len())
                .any(|part| part == INVENTORY)
        );
        let mut archive = ZipArchive::new(Cursor::new(archive)).unwrap();
        assert_eq!(archive.len(), 1);
        assert!(archive.by_index(0).is_err());
        // The short password verifier can collide. Reading to EOF also checks the HMAC.
        if let Ok(mut member) = archive.by_index_decrypt(0, b"wrong password") {
            assert!(io::copy(&mut member, &mut io::sink()).is_err());
        }

        let mut member = archive.by_index_decrypt(0, password.as_bytes()).unwrap();
        assert_eq!(member.name().unwrap(), "connections.xml");
        assert!(member.encrypted());
        assert_eq!(member.crc32(), 0);
        // WinZip AES extra field: AE-2, 256 bits, stored compression.
        let aes = [1, 0x99, 7, 0, 2, 0, b'A', b'E', 3, 0, 0];
        assert!(
            member
                .extra_data()
                .unwrap()
                .windows(aes.len())
                .any(|field| field == aes.as_slice())
        );
        let mut plaintext = Vec::new();
        member.read_to_end(&mut plaintext).unwrap();
        assert_eq!(plaintext, INVENTORY);
        assert!(
            protect(
                INVENTORY.to_vec(),
                "xml",
                Protection::Password(&Secret::default()),
            )
            .is_err()
        );
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).unwrap();
            let directory = std::env::temp_dir().join(format!(
                "kortty-connection-export-{:032x}",
                u128::from_le_bytes(nonce),
            ));
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&directory)
                    .unwrap();
            }
            #[cfg(not(unix))]
            fs::create_dir(&directory).unwrap();
            Self(directory)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn connection_export_protection_openpgp_pins_the_reviewed_recipient() {
        let profile = Scratch::new();
        let secret = test_openpgp_key(KeyVersion::V4, 20);
        let certificate = SignedPublicKey::from(secret.clone());
        let path = profile.0.join("recipient.asc");
        fs::write(
            &path,
            certificate.to_armored_bytes(Default::default()).unwrap(),
        )
        .unwrap();
        let catalog = profile.0.join(recipients::FILE_NAME);
        let mut choices = recipients::Action::Save(
            catalog.clone(),
            recipients::Change {
                previous: None,
                name: "Export test recipient".into(),
                fingerprint: format!("{:X}", certificate.fingerprint()),
                certificate: Some(path),
            },
        )
        .run()
        .unwrap();
        let recipient = choices.pop().unwrap();
        let protection = || Protection::OpenPgp {
            profile: &profile.0,
            recipient: &recipient,
        };
        let ciphertext = protect(INVENTORY.to_vec(), "xml", protection()).unwrap();
        // Java consumes the decrypted byte stream as ZIP. The outer literal
        // packet's optional filename is not part of the interchange format.
        let private_key = profile.0.join("private.asc");
        fs::write(
            &private_key,
            Zeroizing::new(secret.to_armored_bytes(Default::default()).unwrap()).as_slice(),
        )
        .unwrap();
        let password = Secret::default();
        let decrypt = |bytes: &[u8], limit| {
            crate::profile_backup::decrypt_openpgp_document(bytes, &private_key, &password, limit)
        };
        let decrypted = decrypt(&ciphertext, 1024).unwrap();
        assert!(decrypt(&ciphertext, decrypted.len() - 1).is_err());
        let mut tampered = ciphertext.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(decrypt(&tampered, 1024).is_err());

        let mut archive = ZipArchive::new(Cursor::new(decrypted.as_slice())).unwrap();
        assert_eq!(archive.len(), 1);
        let mut plaintext = Vec::new();
        archive
            .by_name("connections.xml")
            .unwrap()
            .read_to_end(&mut plaintext)
            .unwrap();
        assert_eq!(plaintext, INVENTORY);

        recipients::Action::Delete(catalog, recipient.clone())
            .run()
            .unwrap();
        assert!(protect(INVENTORY.to_vec(), "xml", protection()).is_err());
    }
}
