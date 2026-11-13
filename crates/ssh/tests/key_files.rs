use std::io;

use kortty_ssh::{ConnectError, validate_private_key};
use ssh_encoding::Decode;
use ssh_key::private::{EcdsaPrivateKey, Ed25519Keypair};
use ssh_key::{Cipher, Kdf, LineEnding, PrivateKey};

#[test]
fn offline_validation_accepts_pkcs8_and_encrypted_openssh_keys() {
    // Public Ed25519 PKCS#8 test vector from RFC 8410, section 10.3.
    let pem = concat!(
        "-----BEGIN PRIVATE KEY-----\n",
        "MC4CAQAwBQYDK2VwBCIEINTuctv5E1hK1bbY8fdp+K06/nwoy/HU++CXqI9EdVhC\n",
        "-----END PRIVATE KEY-----\n",
    );
    validate_private_key(pem.as_bytes(), None).unwrap();

    let key = PrivateKey::new(Ed25519Keypair::from_seed(&[3; 32]).into(), "fixture").unwrap();
    let passphrase = "fixture passphrase";
    let encrypted = key
        .encrypt_with(
            Cipher::Aes256Ctr,
            Kdf::Bcrypt {
                salt: vec![2; 16],
                rounds: 1,
            },
            4,
            passphrase,
        )
        .unwrap()
        .to_openssh(LineEnding::LF)
        .unwrap();
    validate_private_key(encrypted.as_bytes(), Some(passphrase)).unwrap();
    for passphrase in [None, Some(""), Some("wrong passphrase")] {
        assert!(matches!(
            validate_private_key(encrypted.as_bytes(), passphrase),
            Err(ConnectError::Io(error)) if error.kind() == io::ErrorKind::PermissionDenied
        ));
    }
}

#[test]
fn offline_validation_rejects_invalid_keys_and_excessive_resource_inputs() {
    let oversized = vec![0; 1024 * 1024 + 1];
    let long_passphrase = "x".repeat(1025);
    for (bytes, passphrase) in [
        (&b""[..], None),
        (&b"unrecognized private key data"[..], None),
        (oversized.as_slice(), None),
        (&b"small key"[..], Some(long_passphrase.as_str())),
    ] {
        assert!(matches!(
            validate_private_key(bytes, passphrase),
            Err(ConnectError::Io(error)) if error.kind() == io::ErrorKind::InvalidInput
        ));
    }
}

#[test]
fn openssh_ecdsa_scalar_accepts_short_positive_and_previous_fixed_width_encodings() {
    for (mut wire, expected) in [
        (&[0, 0, 0, 1, 0x7f][..], [0, 0x7f]),
        (&[0, 0, 0, 2, 0, 0x80][..], [0, 0x80]),
        (&[0, 0, 0, 2, 0, 0x7f][..], [0, 0x7f]),
        (&[0, 0, 0, 3, 0, 0x80, 1][..], [0x80, 1]),
    ] {
        let scalar = EcdsaPrivateKey::<2>::decode(&mut wire).unwrap();
        assert!(wire.is_empty());
        assert_eq!(scalar.as_slice(), expected);
    }
}

#[test]
fn openssh_ecdsa_scalar_rejects_negative_empty_and_oversized_encodings() {
    for mut wire in [
        &[0, 0, 0, 1, 0x80][..],
        &[0, 0, 0, 2, 0x80, 1][..],
        &[0, 0, 0, 0][..],
        &[0, 0, 0, 4, 0, 1, 2, 3][..],
    ] {
        assert!(EcdsaPrivateKey::<2>::decode(&mut wire).is_err());
    }
}
