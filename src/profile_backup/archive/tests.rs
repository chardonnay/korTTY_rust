use std::fs;
use std::path::PathBuf;

use super::*;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!(
            "kortty-profile-backup-test-{:032x}",
            u128::from_le_bytes(nonce)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn archive(&self) -> PathBuf {
        self.0.join("profile.zip")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn password() -> Secret {
    Secret::new("fixture passphrase × 123".into())
}

fn snapshot() -> Snapshot {
    Snapshot::from([
        (
            "ssh/known_hosts".into(),
            Zeroizing::new(b"fixture host-key data with a recognizable marker".to_vec()),
        ),
        ("vault/vault.bin".into(), Zeroizing::new(vec![0, 255, 17])),
        ("window-settings.json".into(), Zeroizing::new(Vec::new())),
    ])
}

#[test]
fn archive_roundtrip_uses_one_encrypted_entry_and_fresh_salts() {
    let scratch = Scratch::new();
    let target = scratch.archive();
    let expected = snapshot();
    write(&target, &expected, &password()).unwrap();
    let bytes = fs::read(&target).unwrap();
    let (size, first_salt) = validate_zip(&bytes).unwrap();
    assert_eq!(size, payload_size(&expected).unwrap());
    for secret in [b"ssh/known_hosts".as_slice(), b"fixture host-key data"] {
        assert!(!bytes.windows(secret.len()).any(|part| part == secret));
    }
    assert_eq!(read(&target, &password()).unwrap(), expected);

    let second = scratch.0.join("renamed.zip.gpg");
    write(&second, &expected, &password()).unwrap();
    assert_eq!(
        super::super::detect_format(&second).unwrap(),
        super::super::preferences::EncryptionMode::Password
    );
    let second_bytes = fs::read(second).unwrap();
    let (_, second_salt) = validate_zip(&second_bytes).unwrap();
    assert_ne!(first_salt, second_salt);
    assert_ne!(
        &bytes[ZIP_LOCAL_PREFIX..ZIP_LOCAL_PREFIX + 16],
        &second_bytes[ZIP_LOCAL_PREFIX..ZIP_LOCAL_PREFIX + 16]
    );
}

#[test]
fn wrong_password_modified_salt_or_last_mac_byte_is_refused() {
    let scratch = Scratch::new();
    let target = scratch.archive();
    write(&target, &snapshot(), &password()).unwrap();
    assert!(read(&target, &Secret::new("different fixture passphrase".into())).is_err());
    let bytes = fs::read(&target).unwrap();
    let (size, _) = validate_zip(&bytes).unwrap();
    for offset in [bytes.len() - 1, ZIP_LOCAL_PREFIX + AES_OVERHEAD + size - 1] {
        let mut altered = bytes.clone();
        altered[offset] ^= 1;
        assert!(validate_zip(&altered).is_ok());
        fs::write(&target, altered).unwrap();
        assert!(read(&target, &password()).is_err());
    }
}

#[test]
fn rejects_plaintext_changed_headers_trailing_and_truncated_archives() {
    let scratch = Scratch::new();
    let target = scratch.archive();
    write(&target, &snapshot(), &password()).unwrap();
    let original = fs::read(&target).unwrap();
    let (size, _) = validate_zip(&original).unwrap();
    let descriptor = ZIP_LOCAL_PREFIX + AES_OVERHEAD + size;
    let central = descriptor + DESCRIPTOR_SIZE;
    let end = central + ZIP_CENTRAL_SIZE;
    for offset in [
        6,
        LOCAL_SIZE,
        ZIP_LOCAL_PREFIX - 3,
        descriptor + 8,
        central + 42,
        end + 10,
    ] {
        let mut altered = original.clone();
        altered[offset] ^= 1;
        assert!(validate_zip(&altered).is_err());
    }
    assert!(validate_zip(&original[..original.len() - 1]).is_err());
    let mut trailing = original.clone();
    trailing.push(0);
    assert!(validate_zip(&trailing).is_err());
    let mut prefixed = vec![0];
    prefixed.extend_from_slice(&original);
    assert!(validate_zip(&prefixed).is_err());

    let mut clear = ZipWriter::new_stream(Vec::new());
    clear
        .start_file(ZIP_NAME, SimpleFileOptions::default())
        .unwrap();
    write_payload(&mut clear, &snapshot()).unwrap();
    let mut clear = clear.finish().unwrap();
    let bytes = clear.get_mut();
    fs::write(&target, bytes).unwrap();
    assert!(read(&target, &password()).is_err());
    assert!(super::super::detect_format(&target).is_err());
}

#[test]
fn pending_payload_rejects_duplicate_paths_aliases_prefixes_and_invalid_lengths() {
    let encoded = encode_snapshot(&snapshot()).unwrap();
    assert_eq!(decode_snapshot(&encoded).unwrap(), snapshot());
    for paths in [
        vec!["same", "same"],
        vec!["File", "file"],
        vec!["a/é", "a/e\u{301}"],
        vec!["directory", "directory/file"],
        vec!["../escape"],
        vec!["a/./b"],
        vec!["a\\b"],
        vec!["a//b"],
        vec!["C:/escape"],
        vec!["/absolute"],
        vec!["a/COM1.txt"],
        vec!["a/file."],
    ] {
        let mut raw = PAYLOAD_MAGIC.to_vec();
        raw.extend_from_slice(&(paths.len() as u32).to_le_bytes());
        for path in paths {
            raw.extend_from_slice(&(path.len() as u16).to_le_bytes());
            raw.extend_from_slice(&0_u32.to_le_bytes());
            raw.extend_from_slice(path.as_bytes());
        }
        assert!(decode_snapshot(&raw).is_err());
    }
    let mut count = encoded.to_vec();
    count[PAYLOAD_MAGIC.len()..PAYLOAD_HEADER_SIZE].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decode_snapshot(&count).is_err());
    let mut bad_length = encoded.to_vec();
    let file_size = PAYLOAD_HEADER_SIZE + 2;
    bad_length[file_size..file_size + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decode_snapshot(&bad_length).is_err());
    assert!(decode_snapshot(&encoded[..encoded.len() - 1]).is_err());
}

#[test]
fn invalid_snapshot_and_existing_destination_are_never_published_over() {
    let scratch = Scratch::new();
    let target = scratch.archive();
    let invalid = Snapshot::from([("a/../b".into(), Zeroizing::new(vec![1]))]);
    assert!(write(&target, &invalid, &password()).is_err());
    assert!(!target.exists());
    fs::write(&target, b"preexisting archive").unwrap();
    assert_eq!(
        write(&target, &snapshot(), &password()).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read(&target).unwrap(), b"preexisting archive");
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 1);
}
