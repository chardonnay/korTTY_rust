use std::fs;
use std::path::PathBuf;

use pgp::composed::{MessageBuilder, SignedPublicKey};
use pgp::crypto::sym::SymmetricKeyAlgorithm;
use pgp::ser::Serialize as _;
use pgp::types::{CompressionAlgorithm, KeyDetails, KeyVersion};
use rand::{SeedableRng, rngs::StdRng};

use super::*;
use crate::profile_backup::{BackupFormat, detect_format};
use crate::snippets::{archive::test_openpgp_key, recipients};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!(
            "kortty-backup-pgp-{:032x}",
            u128::from_le_bytes(nonce)
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        }
        #[cfg(not(unix))]
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn backup(&self, snapshot: &Snapshot, recipient: &str) -> PathBuf {
        let path = self.0.join("backup.gpg");
        write_to(
            &mut File::create(&path).unwrap(),
            snapshot,
            &self.0,
            recipient,
        )
        .unwrap();
        path
    }

    fn key(&self, key: &SignedSecretKey) -> PathBuf {
        let path = self.0.join("private.asc");
        fs::write(
            &path,
            Zeroizing::new(key.to_armored_bytes(Default::default()).unwrap()).as_slice(),
        )
        .unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(in crate::profile_backup) fn enroll(
    root: &Path,
    version: KeyVersion,
    seed: u8,
) -> (String, SignedSecretKey) {
    let secret = test_openpgp_key(version, seed);
    let public = SignedPublicKey::from(secret.clone());
    let fingerprint = format!("{:X}", public.fingerprint());
    let public_path = root.join("recipient.asc");
    fs::write(
        &public_path,
        public.to_armored_bytes(Default::default()).unwrap(),
    )
    .unwrap();
    recipients::Action::Save(
        root.join(recipients::FILE_NAME),
        recipients::Change {
            previous: None,
            name: "Local backup recipient fixture".into(),
            fingerprint: fingerprint.clone(),
            certificate: Some(public_path),
        },
    )
    .run()
    .unwrap();
    (fingerprint, secret)
}

fn snapshot() -> Snapshot {
    Snapshot::from([(
        "ssh/known_hosts".into(),
        Zeroizing::new(b"fixture marker".repeat(60_000)),
    )])
}

fn read(source: &Path, private_key: &Path, password: &Secret) -> io::Result<Snapshot> {
    match read_profile(source, private_key, password)? {
        ProfileBackup::Native(snapshot) => Ok(snapshot),
        ProfileBackup::Java(_) => panic!("expected a native OpenPGP backup"),
    }
}

fn decrypt(
    ciphertext: &[u8],
    key: &SignedSecretKey,
    password: &Secret,
    max_payload: usize,
) -> io::Result<Zeroizing<Vec<u8>>> {
    decrypt_literal(ciphertext, key, password, max_payload, false).map(|literal| literal.bytes)
}

#[test]
fn round_trip_uses_protected_v4_and_v6_keys_and_checks_full_authentication() {
    for version in [KeyVersion::V4, KeyVersion::V6] {
        let fixture = Fixture::new();
        let (recipient, mut key) = enroll(&fixture.0, version, 7);
        let password = Secret::new("local fixture passphrase".into());
        key.secret_subkeys[0]
            .key
            .set_password(
                StdRng::from_seed([10; 32]),
                &Password::from(password.as_str()),
            )
            .unwrap();
        let key_path = fixture.key(&key);
        let snapshot = snapshot();
        let path = fixture.backup(&snapshot, &recipient);
        let mut file = File::open(&path).unwrap();
        validate_ciphertext(&mut file).unwrap();
        assert_eq!(file.stream_position().unwrap(), 0);
        let armor = fs::read_to_string(&path).unwrap();
        assert!(armor.contains(COMMENT));
        assert!(!armor.contains("fixture marker"));
        assert_eq!(read(&path, &key_path, &password).unwrap(), snapshot);
        assert!(
            read(
                &path,
                &key_path,
                &Secret::new("wrong fixture passphrase".into())
            )
            .is_err()
        );

        let binary = fixture.0.join("binary.backup");
        let mut bytes = Vec::new();
        with_binary(File::open(&path).unwrap(), Armor::Message, true, |input| {
            input.read_to_end(&mut bytes)?;
            Ok(())
        })
        .unwrap();
        fs::write(&binary, &bytes).unwrap();
        assert_eq!(detect_format(&binary).unwrap(), BackupFormat::OpenPgp);
        assert_eq!(read(&binary, &key_path, &password).unwrap(), snapshot);
        *bytes.last_mut().unwrap() ^= 0x01;
        fs::write(&binary, &bytes).unwrap();
        assert!(read(&binary, &key_path, &password).is_err());
        fs::write(&binary, &bytes[..bytes.len() - 1]).unwrap();
        assert!(read(&binary, &key_path, &password).is_err());
    }
}

#[test]
fn markerless_import_works_but_rotation_requires_marker_exact_shape_and_eof() {
    let fixture = Fixture::new();
    let (recipient, key) = enroll(&fixture.0, KeyVersion::V4, 8);
    let key_path = fixture.key(&key);
    let expected = snapshot();
    let written = fixture.backup(&expected, &recipient);
    let path = fixture.0.join("renamed.zip");
    fs::rename(written, &path).unwrap();
    assert_eq!(detect_format(&path).unwrap(), BackupFormat::OpenPgp);
    let native = fs::read_to_string(&path).unwrap();
    let password = Secret::default();
    let markerless = native.replace(&format!("Comment: {COMMENT}\n"), "");
    fs::write(&path, &markerless).unwrap();
    assert_eq!(detect_format(&path).unwrap(), BackupFormat::OpenPgp);
    assert_eq!(read(&path, &key_path, &password).unwrap(), expected);
    let mut file = File::open(&path).unwrap();
    assert!(validate_ciphertext(&mut file).is_err());
    assert_eq!(file.stream_position().unwrap(), 0);
    drop(file);
    for invalid in [
        format!("{native}trailing"),
        native.replace("-----END PGP MESSAGE-----", ""),
    ] {
        fs::write(&path, invalid).unwrap();
        assert!(read(&path, &key_path, &password).is_err());
        assert!(validate_ciphertext(&mut File::open(&path).unwrap()).is_err());
    }
    fs::write(&path, native).unwrap();
    let foreign_key = fixture.key(&test_openpgp_key(KeyVersion::V4, 9));
    assert!(read(&path, &foreign_key, &password).is_err());
}

#[test]
fn compressed_and_non_profile_literal_zip_are_refused() {
    let fixture = Fixture::new();
    let key = test_openpgp_key(KeyVersion::V4, 11);
    let public = SignedPublicKey::from(key.clone());
    let key_path = fixture.key(&key);
    let target = fixture.0.join("not-profile.pgp");
    let mut rng = StdRng::from_seed([12; 32]);
    let zip = write_zip(&archive::encode_snapshot(&snapshot()).unwrap()).unwrap();
    for (compression, bytes) in [
        (CompressionAlgorithm::ZLIB, zip.as_slice()),
        (CompressionAlgorithm::Uncompressed, b"not a ZIP".as_slice()),
    ] {
        let mut message = MessageBuilder::from_reader(LITERAL, bytes)
            .seipd_v1(&mut rng, SymmetricKeyAlgorithm::AES256);
        message.compression(compression);
        message
            .encrypt_to_key(&mut rng, &public.public_subkeys[0].key)
            .unwrap();
        message
            .to_writer(&mut rng, File::create(&target).unwrap())
            .unwrap();
        assert!(read(&target, &key_path, &Secret::default()).is_err());
    }
    assert!(read_zip(&[zip.as_slice(), b"suffix"].concat()).is_err());
    assert!(
        write_to(
            &mut File::create(&target).unwrap(),
            &snapshot(),
            &fixture.0,
            &format!("{:X}", public.fingerprint())
        )
        .is_err()
    );
}

#[test]
fn java_gnupg_backup_uses_the_additive_preview_and_rejects_tampering() {
    const DEFAULT: &[u8] = include_bytes!("fixtures/java-default.zip.gpg");
    const COMPRESSED: &[u8] = include_bytes!("fixtures/java-compressed.zip.gpg");
    const PRIVATE: &[u8] = include_bytes!("fixtures/java-fixture-secret.asc");

    let fixture = Fixture::new();
    let path = fixture.0.join("renamed-java-backup.dat");
    let key_path = fixture.0.join("java-fixture-secret.asc");
    fs::write(&key_path, PRIVATE).unwrap();
    let password = Secret::new("fixture-only Java GnuPG".into());
    for (ciphertext, compressed) in [(DEFAULT, false), (COMPRESSED, true)] {
        fs::write(&path, ciphertext).unwrap();
        assert_eq!(detect_format(&path).unwrap(), BackupFormat::OpenPgp);
        let preview = crate::profile_backup::inspect_openpgp(&path, &key_path, &password).unwrap();
        let (format, entries, omissions) = preview.java_review().unwrap();
        assert_eq!(format, "Integrity-checked Java OpenPGP backup");
        assert_eq!(
            entries,
            ["Connection profiles (1)", "Project layout (GnuPG Project)"]
        );
        assert!(
            omissions
                .iter()
                .any(|entry| entry.starts_with("credentials.xml:"))
        );
        assert!(
            omissions
                .iter()
                .any(|entry| entry.starts_with("memory/empty.md:"))
        );
        let literal = decrypt_literal(
            ciphertext,
            &read_secret_key(&key_path).unwrap(),
            &password,
            4096,
            true,
        )
        .unwrap();
        assert_eq!(literal.compressed, compressed);
        assert!(
            decrypt_literal(
                ciphertext,
                &read_secret_key(&key_path).unwrap(),
                &password,
                128,
                true
            )
            .is_err()
        );
        assert!(
            crate::profile_backup::inspect_openpgp(&path, &key_path, &Secret::new("wrong".into()))
                .is_err()
        );

        let mut tampered = ciphertext.to_vec();
        *tampered.last_mut().unwrap() ^= 1;
        fs::write(&path, tampered).unwrap();
        assert!(crate::profile_backup::inspect_openpgp(&path, &key_path, &password).is_err());
    }
}

#[test]
fn compressed_literal_rejects_nested_packets_and_trailing_data() {
    let content = b"bounded document";
    let mut literal = vec![0xcb, (6 + content.len()) as u8, b'b', 0, 0, 0, 0, 0];
    literal.extend_from_slice(content);
    assert_eq!(
        read_literal_packet(literal.as_slice(), content.len())
            .unwrap()
            .as_slice(),
        content
    );
    assert!(read_literal_packet(literal.as_slice(), content.len() - 1).is_err());
    let nested = [literal.as_slice(), literal.as_slice()].concat();
    assert!(read_literal_packet(nested.as_slice(), nested.len()).is_err());
    let mut trailing = literal;
    trailing.push(0);
    assert!(read_literal_packet(trailing.as_slice(), trailing.len()).is_err());
}

#[test]
fn v1_authentication_failure_precedes_literal_validation() {
    let key = test_openpgp_key(KeyVersion::V4, 17);
    let public = SignedPublicKey::from(key.clone());
    // More than one decryption chunk makes the literal header available before
    // the MDC in the upstream streaming mode this regression guards against.
    let content = vec![b'x'; 128 * 1024];
    let mut rng = StdRng::from_seed([18; 32]);
    let mut message = MessageBuilder::from_reader(LITERAL, content.as_slice())
        .seipd_v1(&mut rng, SymmetricKeyAlgorithm::AES256);
    message.data_mode(DataMode::Utf8).unwrap();
    message
        .encrypt_to_key(&mut rng, &public.public_subkeys[0].key)
        .unwrap();
    let mut bytes = Vec::new();
    message.to_writer(&mut rng, &mut bytes).unwrap();
    validate_packets(&mut bytes.as_slice()).unwrap();
    let password = Secret::default();
    assert_eq!(
        decrypt(&bytes, &key, &password, MAX_ZIP)
            .unwrap_err()
            .to_string(),
        "unexpected OpenPGP literal data mode"
    );
    *bytes.last_mut().unwrap() ^= 1;
    validate_packets(&mut bytes.as_slice()).unwrap();
    assert_eq!(
        decrypt(&bytes, &key, &password, MAX_ZIP)
            .unwrap_err()
            .to_string(),
        "could not unlock or decrypt the OpenPGP document"
    );
}

#[test]
fn multiple_binary_secret_keys_are_refused() {
    let fixture = Fixture::new();
    let key = test_openpgp_key(KeyVersion::V4, 12);
    let mut key_bytes = Zeroizing::new(key.to_bytes().unwrap());
    key_bytes.extend_from_slice(&Zeroizing::new(key.to_bytes().unwrap()));
    let path = fixture.0.join("keyring.pgp");
    fs::write(&path, key_bytes.as_slice()).unwrap();
    assert!(read_secret_key(&path).is_err());
}

#[test]
fn private_key_argon2_work_is_bounded_before_unlock() {
    use pgp::crypto::aead::AeadAlgorithm;
    use pgp::types::EncryptedSecretParams;

    let encrypted = |t, p, m_enc| {
        SecretParams::Encrypted(EncryptedSecretParams::new(
            vec![1].into(),
            S2kParams::Aead {
                sym_alg: SymmetricKeyAlgorithm::AES256,
                aead_mode: AeadAlgorithm::Ocb,
                s2k: StringToKey::Argon2 {
                    salt: [0; 16],
                    t,
                    p,
                    m_enc,
                },
                nonce: vec![0; 15].into(),
            },
        ))
    };
    assert!(validate_s2k(&encrypted(3, 4, 16)).is_ok());
    for (t, p, m_enc) in [(5, 4, 16), (3, 5, 16), (3, 4, 17)] {
        assert!(validate_s2k(&encrypted(t, p, m_enc)).is_err());
    }
}

#[cfg(unix)]
#[test]
#[ignore = "requires installed GnuPG solely as an interoperability verifier"]
fn gnupg_decrypts_the_native_v4_backup() {
    use std::os::unix::fs::DirBuilderExt as _;
    use std::process::Command;

    struct Agent(PathBuf);
    impl Drop for Agent {
        fn drop(&mut self) {
            let _ = Command::new("gpgconf")
                .args(["--homedir"])
                .arg(&self.0)
                .args(["--kill", "gpg-agent"])
                .output();
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    let fixture = Fixture::new();
    let (recipient, key) = enroll(&fixture.0, KeyVersion::V4, 13);
    let key_path = fixture.key(&key);
    let expected = snapshot();
    let backup = fixture.backup(&expected, &recipient);
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).unwrap();
    // A short absolute path fits the Unix domain socket limit on macOS.
    let home = PathBuf::from(format!(
        "/tmp/kortty-profile-gpg-{:032x}",
        u128::from_ne_bytes(nonce)
    ));
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    let _agent = Agent(home.clone());
    let gpg = |action: &str, path: &Path| {
        let output = Command::new("gpg")
            .args(["--no-options", "--homedir"])
            .arg(&home)
            .args([
                "--batch",
                "--yes",
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                "",
                action,
            ])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Zeroizing::new(output.stdout)
    };
    gpg("--import", &key_path);
    let zip = gpg("--decrypt", &backup);
    assert_eq!(read_zip(&zip).unwrap(), expected);
}
