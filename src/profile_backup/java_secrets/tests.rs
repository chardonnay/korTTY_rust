use super::*;

const MASTER_PROPERTIES: &[u8] = include_bytes!("fixtures/master.properties");
const ENVELOPES: &str = include_str!("fixtures/envelopes.txt");
const TEMPORARY_KEY: &str = concat!(
    "-----BEGIN PRIVATE KEY-----\n",
    "MC4CAQAwBQYDK2VwBCIEIJ1hsZ3v/VpguoRK9JLsLMREScVpezJpGXA7rAMcrn9g\n",
    "-----END PRIVATE KEY-----\n"
);

fn source_password() -> Secret {
    Secret::new("päss🔑例".into())
}

fn java_envelopes() -> [&'static str; 3] {
    let mut lines = ENVELOPES.lines();
    let entries = std::array::from_fn(|_| lines.next().unwrap());
    assert!(lines.next().is_none());
    entries
}

#[test]
fn decrypts_unicode_and_temporary_ssh_material_from_java_kortty() {
    let password = source_password();
    let secrets = JavaSecrets::open(Some(MASTER_PROPERTIES), &password).unwrap();
    let [credential, empty, temporary_key] = java_envelopes();
    assert_eq!(
        secrets.decrypt_field(credential).unwrap().as_str(),
        "café – 新🔒"
    );
    assert!(secrets.decrypt_field(empty).unwrap().is_empty());
    assert_eq!(
        secrets
            .decrypt_temporary_key(temporary_key)
            .unwrap()
            .as_str(),
        TEMPORARY_KEY
    );
}

#[test]
fn requires_the_source_master_password_when_the_verifier_is_present() {
    let destination_password = Secret::new("a different vault password".into());
    let error = JavaSecrets::open(Some(MASTER_PROPERTIES), &destination_password)
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

#[test]
fn older_backups_can_decrypt_individually_salted_fields_without_master_key() {
    let password = source_password();
    let secrets = JavaSecrets::open(None, &password).unwrap();
    let [credential, _, temporary_key] = java_envelopes();
    assert_eq!(
        secrets.decrypt_field(credential).unwrap().as_str(),
        "café – 新🔒"
    );
    assert_eq!(
        secrets
            .decrypt_temporary_key(temporary_key)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn refuses_modified_gcm_tags_and_plaintext_key_fallback() {
    let password = source_password();
    let secrets = JavaSecrets::open(Some(MASTER_PROPERTIES), &password).unwrap();
    let [credential, _, temporary_key] = java_envelopes();
    let (salt, ciphertext) = credential.split_once(':').unwrap();
    let mut envelope = JAVA_BASE64.decode(ciphertext).unwrap();
    *envelope.last_mut().unwrap() ^= 1;
    let altered_credential = format!("{salt}:{}", JAVA_BASE64.encode(envelope));
    assert_eq!(
        secrets
            .decrypt_field(&altered_credential)
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );

    let (_, ciphertext) = temporary_key.split_once(':').unwrap();
    let mut envelope = JAVA_BASE64.decode(ciphertext).unwrap();
    *envelope.last_mut().unwrap() ^= 1;
    let altered_key = format!("enc:{}", JAVA_BASE64.encode(envelope));
    assert_eq!(
        secrets
            .decrypt_temporary_key(&altered_key)
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        secrets
            .decrypt_temporary_key(TEMPORARY_KEY)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn rejects_ambiguous_and_out_of_bounds_input_before_deriving_keys() {
    let password = source_password();
    for invalid in [
        b"salt=missing-hash".as_slice(),
        b"hash=invalid\nhash=duplicate\nsalt=invalid",
        &vec![b'a'; MAX_MASTER_FILE_BYTES + 1],
    ] {
        let error = JavaSecrets::open(Some(invalid), &password).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    let secrets = JavaSecrets::open(None, &password).unwrap();
    let [credential, _, _] = java_envelopes();
    let (salt, _) = credential.split_once(':').unwrap();
    for invalid in [
        "missing-colon".into(),
        format!("{salt}:short"),
        format!("{salt}:{}", "A".repeat(MAX_FIELD_BYTES * 2)),
    ] {
        assert_eq!(
            secrets.decrypt_field(&invalid).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
